//! Lazy compute Photoshop primitives. Pixels are straight-alpha, unclamped RGBA32F.
//! Kernel and color-model defaults are documented in docs/photoshop-op-math.md.
//! Large Gaussian kernels use an antialiased, endpoint-aligned reduction pyramid,
//! a separable 4-sigma kernel, and bilinear reconstruction. Small kernels are exact.
//! Motion uses hierarchical line quadrature; radial zoom, ripple, Smart Sharpen,
//! vibrance, colorize and Highlights Flatten are calibrated approximations.
//! Depth conversions use explicit sRGB.
//! Operation-specialized pipelines are compiled once per device; dropping images recycles their textures.
//! Image clones share immutable storage; only the last alias returns its texture.
//! `trim_idle` releases oldest pooled textures, while `memory_stats` reports
//! texture payload and a resettable texture-plus-explicit-buffer high-water mark.
//! Opaque upload staging, the 16-byte dummy and driver overhead are excluded from the high-water mark.
//! `download_rows` reuses at most 256 rows of staging; `download_row_range` supports
//! pull-based encoders. Gray u16 transfers preserve CPU mask quantization exactly.
//! Source masks reproduce CPU luminosity, alpha and four-neighbor outline math.
//! Long-span motion fuses up to four Hillis-Steele scan passes, retaining each
//! f32 addition's original operands and order; flags skip unused final stages.
//! Set `MOYA_PROFILE_OPS` before the first operation to profile each dispatch's
//! CPU setup, submission, and GPU completion (not GPU-only time). Completion is
//! deferred until the next run, record, or flush so owned store rounding can fuse;
//! elapsed time includes the intervening CPU gap. Flush the final operation to emit it.
//! Stderr records: `MOYA_PROFILE_OPS op=<u32> w=<u32> h=<u32> ms=<decimal>`.
//! `Gpu::lane` adds a second command lane (own encoder and texture pool, shared device and
//! pipelines); `Gpu::set_chunking` makes a lane run row-tiled, duty-limited and cancellable
//! (bit-identical to untiled). Cross-lane visibility requires the producer to flush.
pub use wgpu;
pub mod reference;
mod masks;
pub use masks::MaskSourceKind;
mod transfers;
mod region;
pub use region::{Filter, Region, ViewMap};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::{AtomicBool, Ordering}, Arc, OnceLock, Weak},
    thread::ThreadId,
    time::{Duration, Instant},
};
use wgpu::util::DeviceExt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Eight,
    Sixteen,
    Float,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Screen,
    LinearDodge,
    ColorDodge,
    Multiply,
    Lighten,
    Overlay,
    SoftLight,
    Divide,
    Color,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialQuality {
    Draft,
    Good,
    Best,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub pos: f32,
    pub rgb: [f32; 3],
}
/// Logical RGBA32F texture payload; excludes staging buffers and driver overhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemoryStats {
    pub live_bytes: u64,
    pub idle_bytes: u64,
    pub live_count: u64,
    pub idle_count: u64,
    /// High-water live texture + explicit buffer bytes; excludes opaque driver/upload staging.
    pub peak_live_bytes: u64,
}
/// Idle RGBA32F textures retain at most 64 times the largest requested texture,
/// with a 512 MiB floor and 2 GiB ceiling; live images are never evicted.
struct TexturePool {
    by_size: HashMap<(u32, u32), VecDeque<(u64, wgpu::Texture)>>,
    bytes: u64,
    budget: u64,
    clock: u64,
    live_bytes: u64,
    live_count: u64,
    buffer_bytes: u64,
    peak_live_bytes: u64,
    /// Lane pools recycle only on the thread that allocates from them (see `Gpu::lane`).
    thread_bound: bool,
    owner: Option<ThreadId>,
}
impl TexturePool {
    fn new(budget: u64) -> Self {
        Self {
            by_size: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
            live_bytes: 0,
            live_count: 0,
            buffer_bytes: 0,
            peak_live_bytes: 0,
            thread_bound: false,
            owner: None,
        }
    }
    fn grow_budget(&mut self, w: u32, h: u32) {
        let working_set = u64::from(w) * u64::from(h) * 16 * 64;
        self.budget = self.budget.max(working_set.clamp(512 * 1024 * 1024, 2 * 1024 * 1024 * 1024));
    }
    fn insert(&mut self, texture: wgpu::Texture) {
        let size = (texture.width(), texture.height());
        let bytes = u64::from(size.0) * u64::from(size.1) * 16;
        if bytes > self.budget {
            return;
        }
        self.trim(self.budget - bytes);
        self.clock += 1;
        self.by_size
            .entry(size)
            .or_default()
            .push_back((self.clock, texture));
        self.bytes += bytes;
    }
    fn take(&mut self, size: (u32, u32)) -> Option<wgpu::Texture> {
        let textures = self.by_size.get_mut(&size)?;
        let (_, texture) = textures.pop_back()?;
        self.bytes -= u64::from(size.0) * u64::from(size.1) * 16;
        if textures.is_empty() {
            self.by_size.remove(&size);
        }
        Some(texture)
    }
    fn trim(&mut self, keep_bytes: u64) {
        while self.bytes > keep_bytes {
            // ponytail: scan size buckets on eviction; index ages if this becomes hot.
            let oldest = *self.by_size.iter()
                .min_by_key(|(_, textures)| textures.front().unwrap().0).unwrap().0;
            let textures = self.by_size.get_mut(&oldest).unwrap();
            textures.pop_front();
            self.bytes -= u64::from(oldest.0) * u64::from(oldest.1) * 16;
            if textures.is_empty() { self.by_size.remove(&oldest); }
        }
    }
}
type Pool = Mutex<TexturePool>;
#[derive(Clone)]
struct TrackedBuffer(Arc<BufferLease>);
struct BufferLease {
    buffer: wgpu::Buffer,
    pool: Weak<Pool>,
}
impl std::ops::Deref for TrackedBuffer {
    type Target = wgpu::Buffer;
    fn deref(&self) -> &Self::Target { &self.0.buffer }
}
impl Drop for BufferLease {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            pool.lock().buffer_bytes -= self.buffer.size();
        }
    }
}
struct Producer {
    uniform: TrackedBuffer,
    generation: u64,
    epoch: u64,
    rounded: bool,
}
struct Profile {
    start: Instant,
    op: u32,
    w: u32,
    h: u32,
}
#[derive(Default)]
struct Pending {
    encoder: Option<wgpu::CommandEncoder>,
    generation: u64,
    epoch: u64,
    profile: Option<Profile>,
    buffers: Vec<TrackedBuffer>,
    transient_textures: Vec<(wgpu::Texture, u64)>,
}
pub struct Image {
    texture: wgpu::Texture,
    pool: Weak<Pool>,
    producer: Option<Producer>,
    lease: Arc<ImageLease>,
}
impl Image {
    pub fn width(&self) -> u32 {
        self.texture.width()
    }
    pub fn height(&self) -> u32 {
        self.texture.height()
    }
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
    /// Stable identity shared by aliases of the same live texture.
    pub fn storage_id(&self) -> usize { Arc::as_ptr(&self.lease) as usize }
}
struct ImageLease {
    texture: wgpu::Texture,
    pool: Weak<Pool>,
}
impl Drop for ImageLease {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            let mut pool = pool.lock();
            pool.live_bytes -= u64::from(self.texture.width()) * u64::from(self.texture.height()) * 16;
            pool.live_count -= 1;
            if !pool.thread_bound || pool.owner == Some(std::thread::current().id()) {
                pool.insert(self.texture.clone());
            }
        }
    }
}
impl Clone for Image {
    fn clone(&self) -> Self {
        Self { texture: self.texture.clone(), pool: self.pool.clone(), producer: None, lease: self.lease.clone() }
    }
}
/// Reuses byte-identical Gaussian reductions of one immutable source.
///
/// Retained RGBA32F reductions are capped at 96 MiB; full-resolution blur
/// results are streamed to the caller, never cached. Drop after the glow stack.
pub struct BlurSource<'a> {
    gpu: &'a Gpu,
    image: &'a Image,
    reductions: BlurReductions,
}
impl<'a> BlurSource<'a> {
    pub fn new(gpu: &'a Gpu, image: &'a Image) -> Self {
        Self {
            gpu,
            image,
            reductions: BlurReductions {
                budget: 96 * 1024 * 1024,
                ..Default::default()
            },
        }
    }
    pub fn blur(&mut self, radius: f32) -> Image {
        self.gpu.gaussian_blur_reusing(self.image, radius, &mut self.reductions)
    }
}
struct BlurReduction {
    parent: usize,
    image: Image,
}
#[derive(Default)]
struct BlurReductions {
    nodes: Vec<BlurReduction>,
    bytes: u64,
    budget: u64,
}
/// Throttled, cancellable execution for a [`Gpu::lane`]; see [`Gpu::set_chunking`].
#[derive(Clone)]
pub struct Chunking {
    /// Upper bound on output pixels per tile and per batch of small dispatches.
    pub max_pixels: u64,
    /// Target GPU time per submission; tiles and batches shrink to fit once per-op cost is measured.
    pub target: Duration,
    /// Maximum share of wall time this lane keeps the GPU busy (0..=1].
    pub duty: f32,
    /// Set to stop recording new work; the caller must discard the result.
    pub cancel: Arc<AtomicBool>,
}
struct Chunker {
    cfg: Chunking,
    /// Measured nanoseconds per output pixel by operation; 0 = unmeasured.
    rate: [f64; 36],
    pixels: u64,
    estimate_ns: f64,
    /// Bit set of the operations in the batch so far.
    ops: u64,
}
impl Chunker {
    /// Measured cost per output pixel; until measured, a deliberately pessimistic guess so a
    /// first tile errs small (observed range: 0.3 to 3.6 ns per pixel).
    fn rate_of(&self, op: u32) -> f64 {
        match self.rate[op as usize] {
            measured if measured > 0. => measured,
            _ => 2.,
        }
    }
}
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pool: Arc<Pool>,
    pending: Mutex<Pending>,
    chunker: Mutex<Option<Chunker>>,
    pipelines: [wgpu::ComputePipeline; 36],
    display: wgpu::ComputePipeline,
    gray_upload: OnceLock<wgpu::ComputePipeline>,
    mask_source: OnceLock<wgpu::ComputePipeline>,
    layout: wgpu::BindGroupLayout,
    display_layout: wgpu::BindGroupLayout,
    dummy: wgpu::Texture,
}
fn depth(d: Depth) -> f32 {
    match d {
        Depth::Eight => 255.,
        Depth::Sixteen => 32768.,
        Depth::Float => 0.,
    }
}
/// Device limits Moya needs: the adapter's full texture and buffer sizes, so full-resolution
/// photos (often > wgpu's default 8192 px) fit in one texture and can be read back.
pub fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let supported = adapter.limits();
    wgpu::Limits {
        max_texture_dimension_2d: supported.max_texture_dimension_2d,
        max_buffer_size: supported.max_buffer_size,
        ..wgpu::Limits::default()
    }
}

impl Gpu {
    pub fn new_blocking() -> anyhow::Result<Self> {
        pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    ..Default::default()
                })
                .await?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("Moya compute"),
                    required_limits: device_limits(&adapter),
                    ..Default::default()
                })
                .await?;
            Ok(Self::from_device(device, queue))
        })
    }
    /// Largest width/height an `Image` can have on this device.
    pub fn max_image_dimension(&self) -> u32 {
        self.device().limits().max_texture_dimension_2d
    }
    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        fn make_layout(device: &wgpu::Device, display: bool) -> wgpu::BindGroupLayout {
            let mut entries = Vec::new();
            for binding in 0..if display { 1 } else { 3 } {
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                });
            }
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: if display { 1 } else { 3 },
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: if display {
                        wgpu::TextureFormat::Rgba8Unorm
                    } else {
                        wgpu::TextureFormat::Rgba32Float
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            });
            if !display {
                for (binding, ty) in [
                    (4, wgpu::BufferBindingType::Uniform),
                    (5, wgpu::BufferBindingType::Storage { read_only: true }),
                ] {
                    entries.push(wgpu::BindGroupLayoutEntry {
                        binding,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    });
                }
            }
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &entries,
            })
        }
        fn make_pipeline(
            device: &wgpu::Device,
            layout: &wgpu::PipelineLayout,
            module: &wgpu::ShaderModule,
            constants: &[(&str, f64)],
        ) -> wgpu::ComputePipeline {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(layout),
                module,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants,
                    ..Default::default()
                },
                cache: None,
            })
        }
        let layout = make_layout(&device, false);
        let display_layout = make_layout(&device, true);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Moya kernels"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ops.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = std::array::from_fn(|op| {
            make_pipeline(&device, &pipeline_layout, &module, &[("operation", op as f64)])
        });
        let display_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Moya kernels"),
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&display_layout)],
                immediate_size: 0,
            });
        let display = make_pipeline(&device, &display_pipeline_layout, &display_module, &[]);
        let dummy = Self::texture_new(&device, 1, 1);
        Self {
            device,
            queue,
            pool: Arc::new(Mutex::new(TexturePool::new(512 * 1024 * 1024))),
            pending: Mutex::new(Pending::default()),
            chunker: Mutex::new(None),
            pipelines,
            display,
            gray_upload: OnceLock::new(),
            mask_source: OnceLock::new(),
            layout,
            display_layout,
            dummy,
        }
    }
    /// A second command lane on the same device and queue: shared compiled pipelines,
    /// layouts and queue, but its own pending encoder and its own texture pool.
    ///
    /// Work recorded on one lane is visible to another only once the producing lane has
    /// flushed (queue submission order). The pool is private and recycles only on the thread
    /// that allocates from it, so a texture dropped by another lane's thread (whose unsubmitted
    /// commands may still read it) is freed rather than handed to this lane for overwriting.
    pub fn lane(&self) -> Gpu {
        let mut pool = TexturePool::new(512 * 1024 * 1024);
        pool.thread_bound = true;
        Gpu {
            device: self.device.clone(),
            queue: self.queue.clone(),
            pool: Arc::new(Mutex::new(pool)),
            pending: Mutex::new(Pending::default()),
            chunker: Mutex::new(None),
            pipelines: self.pipelines.clone(),
            display: self.display.clone(),
            gray_upload: self.gray_upload.clone(),
            mask_source: self.mask_source.clone(),
            layout: self.layout.clone(),
            display_layout: self.display_layout.clone(),
            dummy: self.dummy.clone(),
        }
    }
    /// Enable (or with `None`, disable) chunked execution on this lane. Dispatches above
    /// `max_pixels` run as row tiles (bit-identical to one dispatch: every output pixel is
    /// independent); after each chunk the lane submits, waits for the GPU and sleeps so its
    /// busy share stays at most `duty`. Learned per-operation costs survive replacement.
    pub fn set_chunking(&self, chunking: Option<Chunking>) {
        let mut chunker = self.chunker.lock();
        let rate = chunker.as_ref().map_or([0.; 36], |c| c.rate);
        *chunker = chunking.map(|cfg| Chunker { cfg, rate, pixels: 0, estimate_ns: 0., ops: 0 });
    }
    /// True once the active chunking's cancel flag is set; recording is then a no-op.
    pub fn cancelled(&self) -> bool {
        self.chunker.lock().as_ref().is_some_and(|c| c.cfg.cancel.load(Ordering::Relaxed))
    }
    /// Drop unsubmitted work (after cancellation); the lane stays usable.
    pub fn discard_pending(&self) {
        let mut pending = self.pending.lock();
        pending.profile = None;
        pending.encoder = None;
        pending.epoch += 1;
        pending.buffers.clear();
        let transient: u64 = pending.transient_textures.drain(..).map(|(_, bytes)| bytes).sum();
        self.pool.lock().buffer_bytes -= transient;
        if let Some(chunker) = self.chunker.lock().as_mut() { chunker.pixels = 0; chunker.estimate_ns = 0.; chunker.ops = 0; }
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    /// Release idle textures oldest-first without evicting live images or changing the pool budget.
    pub fn trim_idle(&self, keep_bytes: u64) {
        self.pool.lock().trim(keep_bytes);
    }
    pub fn memory_stats(&self) -> GpuMemoryStats {
        let pool = self.pool.lock();
        GpuMemoryStats {
            live_bytes: pool.live_bytes, live_count: pool.live_count,
            idle_bytes: pool.bytes,
            idle_count: pool.by_size.values().map(|v| v.len() as u64).sum(),
            peak_live_bytes: pool.peak_live_bytes,
        }
    }
    /// Reset the high-water mark to the currently live payload, not zero.
    pub fn reset_peak(&self) {
        let mut pool = self.pool.lock();
        pool.peak_live_bytes = pool.live_bytes + pool.buffer_bytes;
    }
    fn track_buffer(&self, buffer: wgpu::Buffer) -> TrackedBuffer {
        let mut pool = self.pool.lock();
        pool.buffer_bytes += buffer.size();
        pool.peak_live_bytes = pool.peak_live_bytes.max(pool.live_bytes + pool.buffer_bytes);
        TrackedBuffer(Arc::new(BufferLease { buffer, pool: Arc::downgrade(&self.pool) }))
    }
    pub fn flush(&self) {
        let mut pending = self.pending.lock();
        self.complete_profile(&mut pending);
        self.submit_pending(&mut pending);
    }
    fn submit_pending(&self, pending: &mut Pending) -> Option<wgpu::SubmissionIndex> {
        let encoder = pending.encoder.take()?;
        let index = self.queue.submit([encoder.finish()]);
        pending.epoch += 1;
        let buffers = std::mem::take(&mut pending.buffers);
        let textures = std::mem::take(&mut pending.transient_textures);
        let pool = Arc::downgrade(&self.pool);
        self.queue.on_submitted_work_done(move || {
            if let Some(pool) = pool.upgrade() {
                pool.lock().buffer_bytes -= textures.iter().map(|(_, bytes)| bytes).sum::<u64>();
            }
            drop(textures);
        });
        self.queue.on_submitted_work_done(move || drop(buffers));
        Some(index)
    }
    fn complete_profile(&self, pending: &mut Pending) {
        if let Some(Profile { start, op, w, h }) = pending.profile.take() {
            self.submit_pending(pending);
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("GPU profiling dispatch completion");
            eprintln!(
                "MOYA_PROFILE_OPS op={op} w={w} h={h} ms={:.6}",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
    fn texture_new(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        assert!(w > 0 && h > 0, "images must be nonempty");
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Moya float image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }
    fn image(&self, w: u32, h: u32) -> Image {
        let texture = {
            let mut pool = self.pool.lock();
            if pool.thread_bound { pool.owner.get_or_insert_with(|| std::thread::current().id()); }
            pool.grow_budget(w, h);
            pool.live_bytes += u64::from(w) * u64::from(h) * 16;
            pool.live_count += 1;
            pool.peak_live_bytes = pool.peak_live_bytes.max(pool.live_bytes + pool.buffer_bytes);
            pool.take((w, h))
        }.unwrap_or_else(|| Self::texture_new(&self.device, w, h));
        Image {
            lease: Arc::new(ImageLease { texture: texture.clone(), pool: Arc::downgrade(&self.pool) }),
            texture,
            pool: Arc::downgrade(&self.pool),
            producer: None,
        }
    }
    fn record(
        &self,
        profile: Option<Profile>,
        f: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> (u64, u64) {
        self.record_buffers(profile, &[], f)
    }
    fn record_buffers(&self, profile: Option<Profile>, buffers: &[TrackedBuffer], f: impl FnOnce(&mut wgpu::CommandEncoder)) -> (u64, u64) {
        self.record_resources(profile, buffers, &[], f)
    }
    fn record_resources(&self, profile: Option<Profile>, buffers: &[TrackedBuffer], textures: &[(wgpu::Texture, u64)], f: impl FnOnce(&mut wgpu::CommandEncoder)) -> (u64, u64) {
        let mut pending = self.pending.lock();
        self.complete_profile(&mut pending);
        let encoder = pending.encoder.get_or_insert_with(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Moya lazy encoder"),
                })
        });
        f(encoder);
        pending.generation += 1;
        pending.profile = profile;
        pending.buffers.extend_from_slice(buffers);
        pending.transient_textures.extend_from_slice(textures);
        (pending.generation, pending.epoch)
    }
    /// Upload straight-alpha RGBA32F using staging bands bounded by the device buffer limit.
    /// Dimensions may reach `device.limits().max_texture_dimension_2d`; total image
    /// bytes may exceed `max_buffer_size` (each padded row must still fit).
    pub fn upload_rgba32f(&self, w: u32, h: u32, rgba: &[f32]) -> Image {
        self.upload_rgba32f_capped(w, h, rgba, self.device.limits().max_buffer_size)
    }
    fn upload_rgba32f_capped(&self, w: u32, h: u32, rgba: &[f32], cap: u64) -> Image {
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        self.flush();
        let img = self.image(w, h);
        self.upload_bands(img.texture.as_image_copy(), img.texture.size(), 16, rgba, cap);
        img
    }
    fn transfer_rows(w: u32, bytes_per_pixel: u32, cap: u64) -> u32 {
        let pitch = (u64::from(w) * u64::from(bytes_per_pixel)).div_ceil(256) * 256;
        assert!(pitch > 0 && pitch <= cap, "one transfer row exceeds buffer limit");
        (cap / pitch).min(u64::from(u32::MAX)) as u32
    }
    fn upload_bands(&self, target: wgpu::TexelCopyTextureInfo<'_>, size: wgpu::Extent3d, bytes_per_pixel: u32, data: &[f32], cap: u64) {
        let rows = Self::transfer_rows(size.width, bytes_per_pixel, cap.min(self.device.limits().max_buffer_size));
        let row_floats = size.width as usize * bytes_per_pixel as usize / 4;
        for y in (0..size.height).step_by(rows as usize) {
            let height = rows.min(size.height - y);
            let mut target = target.clone();
            target.origin.y += y;
            self.queue.write_texture(
                target,
                bytemuck::cast_slice(&data[y as usize * row_floats..(y + height) as usize * row_floats]),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * bytes_per_pixel),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { height, ..size },
            );
            // Submit each staging allocation before allocating the next band.
            self.queue.submit([]);
        }
    }
    /// Upload one unclamped float per pixel; expand to (gray, gray, gray, 1) on the GPU.
    pub fn upload_gray32f(&self, w: u32, h: u32, gray: &[f32]) -> Image {
        self.upload_gray32f_capped(w, h, gray, self.device.limits().max_buffer_size)
    }
    fn upload_gray32f_capped(&self, w: u32, h: u32, gray: &[f32], cap: u64) -> Image {
        assert!(w > 0 && h > 0, "images must be nonempty");
        assert_eq!(gray.len(), w as usize * h as usize);
        self.flush();
        let source = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Moya gray upload"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let source_bytes = u64::from(w) * u64::from(h) * 4;
        {
            let mut pool = self.pool.lock();
            pool.buffer_bytes += source_bytes;
            pool.peak_live_bytes = pool.peak_live_bytes.max(pool.live_bytes + pool.buffer_bytes);
        }
        self.upload_bands(source.as_image_copy(), source.size(), 4, gray, cap);
        let image = self.image(w, h);
        let pipeline = self.gray_upload.get_or_init(|| {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Moya gray expansion"),
                source: wgpu::ShaderSource::Wgsl(include_str!("gray_upload.wgsl").into()),
            });
            let bindings = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2, multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba32Float,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None, bind_group_layouts: &[Some(&bindings)], immediate_size: 0,
            });
            self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Moya gray expansion"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        let source_view = source.create_view(&Default::default());
        let target_view = image.texture.create_view(&Default::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&source_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target_view) },
            ],
        });
        self.record_resources(None, &[], &[(source.clone(), source_bytes)], |encoder| {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        });
        image
    }
    /// Overwrite a nonempty region with tightly packed, unclamped RGBA32F pixels.
    /// Pending operations are submitted first so existing consumers see the old pixels.
    pub fn write_region(&self, img: &Image, x: u32, y: u32, w: u32, h: u32, rgba: &[f32]) {
        self.write_region_capped(img, x, y, w, h, rgba, self.device.limits().max_buffer_size);
    }
    fn write_region_capped(&self, img: &Image, x: u32, y: u32, w: u32, h: u32, rgba: &[f32], cap: u64) {
        assert!(w > 0 && h > 0, "regions must be nonempty");
        assert!(x <= img.width() && w <= img.width() - x);
        assert!(y <= img.height() && h <= img.height() - y);
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        self.flush();
        let mut target = img.texture.as_image_copy();
        target.origin = wgpu::Origin3d { x, y, z: 0 };
        self.upload_bands(target, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, 16, rgba, cap);
    }
    /// Read RGBA32F in padded-row bands bounded by the device buffer limit.
    /// Supports texture-limit dimensions and images larger than `max_buffer_size`,
    /// provided one padded RGBA32F row fits in that buffer limit.
    pub fn download_rgba32f(&self, img: &Image) -> Vec<f32> {
        self.download_rgba32f_capped(img, self.device.limits().max_buffer_size)
    }
    fn download_rgba32f_capped(&self, img: &Image, cap: u64) -> Vec<f32> {
        let rows = Self::transfer_rows(img.width(), 16, cap.min(self.device.limits().max_buffer_size)).min(256);
        let mut result = Vec::with_capacity(img.width() as usize * img.height() as usize * 4);
        self.download_rows(img, rows, |_, _, rgba| {
            result.extend_from_slice(rgba);
            Ok(())
        }).expect("GPU readback failed");
        result
    }
    pub fn write_display_rgba8(&self, img: &Image, target: &wgpu::Texture) {
        assert_eq!(
            (img.width(), img.height()),
            (target.width(), target.height())
        );
        let a = img.texture.create_view(&Default::default());
        let b = target.create_view(&Default::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.display_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&a),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&b),
                },
            ],
        });
        self.record(None, |e| {
            let mut pass = e.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.display);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(img.width().div_ceil(16), img.height().div_ceil(16), 1);
        });
    }
    fn run(
        &self,
        op: u32,
        inputs: &[&Image],
        w: u32,
        h: u32,
        args: &[f32],
        stops: &[[f32; 4]],
    ) -> Image {
        let mut out = self.image(w, h);
        self.run_into(op, inputs, &mut out, args, stops);
        out
    }
    /// Relative per-pixel cost of one dispatch of `op` (kernel taps / samples / source area per
    /// output pixel), so one measured rate per operation holds across radii, sample counts and scales.
    fn op_units(op: u32, args: &[f32], source_px: u64, out_px: u64) -> f64 {
        let arg = |i: usize| f64::from(args.get(i).copied().unwrap_or(0.));
        match op {
            3 => (source_px as f64 / out_px as f64).max(1.),
            6 if arg(0) > 0. => 4.,
            7 => 2. * arg(0) + 1.,
            8 => arg(3).max(4.),
            9 => arg(3).max(1.),
            _ => 1.,
        }
    }
    /// Rows of the next tile of a dispatch with `rows_left` rows to go: all of them unless this
    /// lane is chunked. A chunk never exceeds its pixel/time budget: work that does not fit what
    /// is left of the current batch flushes it first. Tiles are multiples of 16 rows so
    /// neighbours never overlap. `None` if cancelled.
    fn next_tile_rows(&self, op: u32, units: f64, w: u32, rows_left: u32) -> Option<u32> {
        loop {
            let mut guard = self.chunker.lock();
            let Some(c) = guard.as_mut() else { return Some(rows_left) };
            if c.cfg.cancel.load(Ordering::Relaxed) { return None; }
            let (rate, target) = (c.rate_of(op) * units, c.cfg.target.as_nanos() as f64);
            let room = ((target - c.estimate_ns) / rate).min((c.cfg.max_pixels - c.pixels.min(c.cfg.max_pixels)) as f64);
            let fresh = (target / rate).min(c.cfg.max_pixels as f64);
            let whole = f64::from(w) * f64::from(rows_left);
            if whole <= room { return Some(rows_left); }
            if c.pixels > 0 && (room < 16. * f64::from(w) || whole <= fresh) {
                drop(guard);
                if self.flush_chunk() { return None; }
                continue;
            }
            let pixels = if c.pixels > 0 { room } else { fresh };
            return Some((((pixels / f64::from(w)) as u32 / 16 * 16).max(16)).min(rows_left));
        }
    }
    /// Count a recorded dispatch against the current chunk, flushing it once its budget is spent.
    /// Returns true if cancelled.
    fn chunk_account(&self, op: u32, units: f64, pixels: u64) -> bool {
        let mut guard = self.chunker.lock();
        let Some(c) = guard.as_mut() else { return false };
        if c.cfg.cancel.load(Ordering::Relaxed) { return true; }
        c.estimate_ns += pixels as f64 * units * c.rate_of(op);
        c.pixels += pixels;
        c.ops |= 1 << op;
        if c.pixels < c.cfg.max_pixels && c.estimate_ns < c.cfg.target.as_nanos() as f64 { return false; }
        drop(guard);
        self.flush_chunk()
    }
    /// Submit the current chunk, wait for the GPU, correct the cost estimates of the operations in
    /// it by the measured/estimated ratio, and sleep so this lane's GPU busy share stays at most
    /// `duty`. Returns true if cancelled.
    fn flush_chunk(&self) -> bool {
        let mut guard = self.chunker.lock();
        let Some(c) = guard.as_mut() else { return false };
        c.pixels = 0;
        let (ops, estimate) = (std::mem::take(&mut c.ops), std::mem::take(&mut c.estimate_ns));
        let duty = c.cfg.duty.clamp(0.05, 1.);
        drop(guard);
        let started = Instant::now();
        let index = {
            let mut pending = self.pending.lock();
            self.complete_profile(&mut pending);
            self.submit_pending(&mut pending)
        };
        let Some(submission_index) = index else { return self.cancelled() };
        self.device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission_index), timeout: None })
            .expect("GPU chunk completion");
        let busy = started.elapsed();
        let mut guard = self.chunker.lock();
        let Some(c) = guard.as_mut() else { return false };
        if estimate > 0. {
            let ratio = busy.as_nanos() as f64 / estimate;
            for op in (0..64u32).filter(|op| ops >> op & 1 == 1) {
                // Underestimates are corrected at once (a stall is costly), overestimates gradually.
                c.rate[op as usize] = c.rate_of(op) * if ratio > 1. { ratio } else { 0.5 + 0.5 * ratio };
            }
        }
        let cancelled = c.cfg.cancel.load(Ordering::Relaxed);
        drop(guard);
        if !cancelled && duty < 1. {
            std::thread::sleep(busy.mul_f32((1. - duty) / duty).min(Duration::from_millis(100)));
        }
        cancelled
    }
    fn run_into(&self, op: u32, inputs: &[&Image], out: &mut Image, args: &[f32], stops: &[[f32; 4]]) {
        let (w, h) = (out.width(), out.height());
        out.producer = None;
        if self.cancelled() { return; }
        static PROFILE_OPS: OnceLock<bool> = OnceLock::new();
        let mut start = (*PROFILE_OPS.get_or_init(|| std::env::var_os("MOYA_PROFILE_OPS").is_some()))
            .then(|| {
                self.flush();
                self.device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .expect("GPU profiling pre-dispatch completion");
                Instant::now()
            });
        let mut params = [0f32; 32];
        params[4..4 + args.len()].copy_from_slice(args);
        let storage = self.track_buffer(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(if stops.is_empty() { &[[0.; 4]] } else { stops }),
                usage: wgpu::BufferUsages::STORAGE,
            }));
        let views: Vec<_> = (0..3)
            .map(|i| {
                inputs
                    .get(i)
                    .map_or(&self.dummy, |x| &x.texture)
                    .create_view(&Default::default())
            })
            .collect();
        let dst = out.texture.create_view(&Default::default());
        let source_px = inputs.first().map_or(0, |i| u64::from(i.width()) * u64::from(i.height()));
        let units = Self::op_units(op, args, source_px, u64::from(w) * u64::from(h));
        let mut y0 = 0;
        while y0 < h {
            let Some(rows) = self.next_tile_rows(op, units, w, h - y0) else { return };
            // head.y: tile origin (see ops.wgsl).
            params[1] = y0 as f32;
            let uniform = self.track_buffer(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&params),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                }));
            let mut entries: Vec<_> = views
                .iter()
                .enumerate()
                .map(|(i, v)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: wgpu::BindingResource::TextureView(v),
                })
                .collect();
            entries.extend([
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&dst),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: storage.as_entire_binding(),
                },
            ]);
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &entries,
            });
            let (generation, epoch) = self.record_buffers(start.take().map(|start| Profile { start, op, w, h }), &[uniform.clone(), storage.clone()], |e| {
                let mut pass = e.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipelines[op as usize]);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(w.div_ceil(16), rows.div_ceil(16), 1);
            });
            y0 += rows;
            // A chunked lane never fuses a later store round into a (possibly multi-tile) producer.
            if y0 == h && self.chunker.lock().is_none() {
                out.producer = Some(Producer { uniform, generation, epoch, rounded: false });
            }
            if self.chunk_account(op, units, u64::from(w) * u64::from(rows)) { return; }
        }
    }
    fn unary(&self, op: u32, img: &Image, args: &[f32]) -> Image {
        self.run(op, &[img], img.width(), img.height(), args, &[])
    }
    pub fn copy(&self, img: &Image) -> Image {
        self.unary(0, img, &[])
    }
    pub fn fill(&self, w: u32, h: u32, rgba: [f32; 4]) -> Image {
        self.run(1, &[], w, h, &rgba, &[])
    }
    pub fn resize_bilinear(&self, img: &Image, w: u32, h: u32) -> Image {
        assert!(w > 0 && h > 0, "resize target dimensions must be nonzero");
        self.run(2, &[img], w, h, &[], &[])
    }
    pub fn resize_bicubic(&self, img: &Image, w: u32, h: u32) -> Image {
        assert!(w > 0 && h > 0, "resize target dimensions must be nonzero");
        // Antialias before reduction; a bounded half-size pyramid avoids huge reconstruction footprints.
        if w < img.width().div_ceil(2) || h < img.height().div_ceil(2) {
            let half = self.resize_bicubic(
                img,
                (img.width().div_ceil(2)).max(w),
                (img.height().div_ceil(2)).max(h),
            );
            return self.resize_bicubic(&half, w, h);
        }
        self.run(3, &[img], w, h, &[], &[])
    }
    pub fn scale_about(&self, img: &Image, c: [f32; 2], scale: f32) -> Image {
        assert!(scale > 0. && scale.is_finite());
        self.unary(4, img, &[c[0], c[1], scale])
    }
    /// Photoshop `fillMode: repeat` extends boundary pixels; it does not wrap.
    pub fn offset_repeat(&self, img: &Image, dx: i32, dy: i32) -> Image {
        self.unary(5, img, &[dx as f32, dy as f32])
    }
    pub fn gaussian_blur(&self, img: &Image, radius: f32) -> Image {
        self.gaussian_blur_reusing(img, radius, &mut BlurReductions::default())
    }
    fn gaussian_blur_reusing(&self, img: &Image, radius: f32, reductions: &mut BlurReductions) -> Image {
        // PS uses a finite box spline, not exp(-x*x/(2*radius*radius)).
        let sigma = radius.max(0.);
        if sigma < 0.001 {
            return self.copy(img);
        }
        let kernel = reference::gaussian_kernel(f64::from(sigma));
        let convolve = |input: &Image, scale: f32, dx: f32, dy: f32| {
            let support = ((kernel.len() - 1) as f32 * scale).ceil() as usize;
            let mut weights = vec![[0.; 4]; support + 1];
            weights[0][0] = kernel[0] as f32;
            for (k, &weight) in kernel.iter().enumerate().skip(1) {
                let q = k as f32 * scale;
                let lo = q.floor() as usize;
                let t = q - lo as f32;
                weights[lo][0] += weight as f32 * (1. - t) * if lo == 0 { 2. } else { 1. };
                if t > 0. { weights[lo + 1][0] += weight as f32 * t; }
            }
            self.run(7, &[input], input.width(), input.height(),
                &[support as f32, dx, dy], &weights)
        };
        if sigma > 8. {
            // Endpoint-aligned grid preserves the exact clamp boundary for very large kernels.
            let w = (((img.width() - 1) as f32 * 8. / sigma).ceil() as u32 + 1)
                .max(2)
                .min(img.width());
            let h = (((img.height() - 1) as f32 * 8. / sigma).ceil() as u32 + 1)
                .max(2)
                .min(img.height());
            // A node's parent is part of its key: equal output dimensions alone
            // do not imply equal pixels when a terminal reduction was clamped.
            let mut parent = 0;
            let mut uncached = None;
            loop {
                let input = uncached.as_ref().unwrap_or_else(|| {
                    if parent == 0 { img } else { &reductions.nodes[parent - 1].image }
                });
                let nw = input.width().div_ceil(2).max(w);
                let nh = input.height().div_ceil(2).max(h);
                let cached = if uncached.is_none() {
                    reductions.nodes.iter().position(|node| {
                        node.parent == parent && node.image.width() == nw && node.image.height() == nh
                    })
                } else { None };
                if let Some(index) = cached {
                    parent = index + 1;
                } else {
                    let next = self.run(6, &[input], nw, nh, &[1.], &[]);
                    let bytes = u64::from(nw) * u64::from(nh) * 16;
                    if uncached.is_none() && reductions.bytes + bytes <= reductions.budget {
                        reductions.nodes.push(BlurReduction { parent, image: next });
                        reductions.bytes += bytes;
                        parent = reductions.nodes.len();
                    } else {
                        uncached = Some(next);
                    }
                }
                if nw == w && nh == h { break; }
            }
            let small = uncached.as_ref().unwrap_or_else(|| &reductions.nodes[parent - 1].image);
            // CALIBRATE: bounded pyramid resampling, Float corpus max .0001773;
            // full PS spline weights preserve shape without radius-dependent GPU work.
            let sx = if img.width() > 1 {
                (w - 1) as f32 / (img.width() - 1) as f32
            } else { 1. };
            let sy = if img.height() > 1 {
                (h - 1) as f32 / (img.height() - 1) as f32
            } else { 1. };
            let x = convolve(small, sx, 1., 0.);
            let y = convolve(&x, sy, 0., 1.);
            return self.run(6, &[&y], img.width(), img.height(), &[], &[]);
        }
        let x = convolve(img, 1., 1., 0.);
        convolve(&x, 1., 0., 1.)
    }
    pub fn motion_blur(&self, img: &Image, angle: f32, distance: f32, d: Depth) -> Image {
        if distance < 0. {
            return self.copy(img);
        }
        // PS accepts descriptor distance zero and applies the minimum kernel.
        let distance = distance.max(1.);
        // PS 27.11 rounds projected endpoints by truncating after adding .5,
        // including negative projections; distance one is not an identity.
        let radians = angle as f64 * std::f64::consts::PI / 180.;
        let dx = (distance as f64 * radians.cos() + 0.5).trunc() as f32;
        let dy = (distance as f64 * radians.sin() + 0.5).trunc() as f32;
        let span = dx.abs().max(dy.abs());
        let vertical = dy.abs() > dx.abs();
        let slope = if span == 0. { 0. } else if vertical { dx / dy } else { dy / dx };
        let minor = if vertical { img.width() } else { img.height() };
        let limit = self.device.limits().max_texture_dimension_2d;
        if span <= 256. && !(d != Depth::Float && slope != 0. && minor + 4 > limit) {
            let vertical = dy.abs() > dx.abs();
            let slope = if span == 0. { 0. } else if vertical { dx / dy } else { dy / dx };
            let vertical = if vertical { 1. } else { 0. };
            if d != Depth::Float && slope != 0. {
                // Two guard pixels preserve clamped source samples beyond the
                // canvas before the inverse shear, rather than clamping its result.
                let (w, h) = if vertical > 0. { (img.width() + 4, img.height()) } else { (img.width(), img.height() + 4) };
                let sheared = self.run(8, &[img], w, h, &[slope, vertical, span, 1., 2., 0., depth(d)], &[]);
                return self.run(8, &[&sheared], img.width(), img.height(), &[slope, vertical, span, span + 1., 1., 1., depth(d)], &[]);
            }
            return self.unary(8, img, &[slope, vertical, span, span + 1., 1.]);
        }
        // Shear once, scan along the major axis, then undo the shear. Fusing
        // four scan passes preserves their addition tree with fewer dispatches.
        let vertical = dy.abs() > dx.abs();
        let slope = if vertical { dx / dy } else { dy / dx };
        let (major, minor) = if vertical {
            (img.height(), img.width())
        } else {
            (img.width(), img.height())
        };
        let first = ((0.5 * slope * 128.).floor() / 128.).floor() as i32;
        let last = ((((major - 1) as f32 + 0.5) * slope * 128.).floor() / 128.).floor() as i32;
        let origin = first.min(last) - 2;
        let extent = minor + (last - first).unsigned_abs() + 4;
        let axis = if vertical { 1. } else { 0. };
        let grid = depth(d);
        // Each strip retains the full major axis, preserving the prefix addition
        // tree exactly. Four overlapping rows cover every inverse cubic tap.
        // Cap each strip texture (RGBA32F, major x rows) at 256 MiB: GPUs with less memory
        // headroom (e.g. virtualized Metal) silently produce zeros for ~1 GiB strips. Strip
        // height doesn't change results, only how many strips run.
        let budget_rows = ((256u64 << 20) / (major as u64 * 16)).clamp(8, u32::MAX as u64) as u32;
        let strip_limit = limit.min(4096).min(budget_rows);
        let core = strip_limit - 4;
        let mut output = self.image(img.width(), img.height());
        let mut start = 0;
        while start < extent - 4 {
            let count = core.min(extent - 4 - start);
            let strip_origin = origin + start as i32;
            let strip_extent = count + 4;
            let (w, h) = if vertical { (strip_extent, major) } else { (major, strip_extent) };
            let mut prefix = self.run(32, &[img], w, h, &[slope, axis, strip_origin as f32, grid], &[]);
            let mut offset = 1;
            while offset < major {
                let second = if 2 * offset < major { 1. } else { 0. };
                let third = if 4 * offset < major { 1. } else { 0. };
                let fourth = if 8 * offset < major { 1. } else { 0. };
                prefix = self.run(33, &[&prefix], w, h, &[axis, offset as f32, second, third, fourth], &[]);
                offset *= 16;
            }
            self.run_into(34, &[&prefix], &mut output,
                &[slope, axis, strip_origin as f32, grid, span, count as f32], &[]);
            start += count;
        }
        output
    }
    /// Zoom about PS's normalized center: 1/128 bins, then canvas scaling minus one pixel.
    pub fn radial_zoom_blur(
        &self,
        img: &Image,
        center: [f32; 2],
        amount: f32,
        q: RadialQuality,
    ) -> Image {
        self.unary(
            9,
            img,
            &[
                (center[0].clamp(0., 1.) * 128.).floor() * (img.width() as f32 / 128.) - 1.,
                (center[1].clamp(0., 1.) * 128.).floor() * (img.height() as f32 / 128.) - 1.,
                amount.clamp(0., 100.) / 100.,
                match q {
                    RadialQuality::Draft => 16.,
                    RadialQuality::Good => 64.,
                    RadialQuality::Best => 256.,
                },
            ],
        )
    }
    pub fn ripple(&self, img: &Image, amount: f32, large: bool, seed: u32) -> Image {
        self.unary(
            10,
            img,
            &[amount, if large { 99. } else { 8. }, f32::from_bits(seed)],
        )
    }
    /// Lens-blur Smart Sharpen with shadow amount 100, width 52, and radius 1.
    pub fn smart_sharpen(&self, img: &Image, amount: f32, radius: f32, d: Depth) -> Image {
        if amount <= 0. || radius < 0.1 {
            return self.copy(img);
        }
        // Measured 7x7 radial responses at sharp=1,5,10 on a 256px portrait.
        // ponytail: compact empirical lens model; wider radii need new oracle captures.
        let kernels = if d == Depth::Float {
            [
                [-0.3015825,-0.0978492,0.0288385,0.0378094,0.0020423,-0.0198018,0.0021775,-0.0039862,0.0012980],
                [-0.8670229,-0.2398351,0.1181897,0.1163666,-0.0157029,-0.0501276,0.0177393,-0.0314773,0.0234408],
                [-0.4748680,-0.3081494,-0.2803166,-0.0885655,0.1130579,0.0218994,0.0630843,0.0158327,-0.0227915],
            ]
        } else {
            [
                [-0.1372466,-0.0552101,0.0182827,0.0137107,0.0005059,-0.0070785,0.0007837,-0.0000786,-0.0025447],
                [-0.4844988,-0.1273667,0.0803136,0.0619988,-0.0320292,-0.0197693,0.0140476,-0.0122040,0.0024012],
                [-0.2392689,-0.1990907,-0.1480073,-0.0623867,0.0795732,0.0291026,0.0337072,0.0124384,-0.0246952],
            ]
        };
        let radii = [256. / 1300., 5. * 256. / 1300., 10. * 256. / 1300.];
        let (lo, hi, t) = if radius < radii[0] {
            (0, 0, 0.)
        } else if radius < radii[1] {
            (0, 1, (radius-radii[0])/(radii[1]-radii[0]))
        } else {
            (1, 2, ((radius-radii[1])/(radii[2]-radii[1])).min(1.))
        };
        let strength = amount.clamp(0., 500.) / 500.;
        let mut args = [0.; 10];
        for i in 0..9 {
            args[i] = (kernels[lo][i]*(1.-t)+kernels[hi][i]*t)*strength;
        }
        args[9] = if d == Depth::Float { 0. } else { 1. };
        self.unary(11, img, &args)
    }
    pub fn add_noise(&self, img: &Image, amount: f32, mono: bool, seed: u32, d: Depth) -> Image {
        self.unary(
            12,
            img,
            &[
                amount / 100. * match d { Depth::Eight => 0.93145, Depth::Sixteen => 1., Depth::Float => 2. },
                if mono { 1. } else { 0. },
                f32::from_bits(seed),
            ],
        )
    }
    /// Photoshop Levels uses clipped encoded channels at 8/16-bit and signed,
    /// unclipped power-law channels in Float documents.
    pub fn levels(&self, img: &Image, b: f32, w: f32, g: f32, ob: f32, ow: f32, d: Depth) -> Image {
        assert!(w > b && g > 0.);
        let g = g.clamp(0.01, 9.99);
        let [q, a, toe_b, slope] = reference::levels_toe(g);
        self.unary(13, img, &[b / 255., w / 255., g, ob / 255., ow / 255., depth(d), q, a, toe_b, slope])
    }
    /// Photoshop Exposure: linear-light stops, then offset, then signed reciprocal gamma.
    /// Integer depths use the measured power-563/256 transfer and inverse lookup interpolation.
    pub fn exposure(&self, img: &Image, e: f32, o: f32, g: f32, d: Depth) -> Image {
        assert!(g > 0.);
        // Below gamma 0.01, mantissa powers exceed Float storage.
        // Preserve native power semantics there rather than allocate an unbounded LUT.
        if d != Depth::Float || g == 1. || g < 0.01 {
            return self.unary(14, img, &[e, o, g, depth(d)]);
        }
        // Range reduction keeps the LUT independent of image range, including HDR.
        // Hi/lo entries retain the precision needed at large Float output values.
        let exponent = f64::from(1.0f32 / g);
        let steps = ((exponent * (exponent - 1.)).abs().sqrt() * 1024.)
            .ceil().max(1024.) as usize;
        let steps = steps.next_power_of_two();
        let mut table = Vec::with_capacity(steps + 1 + 277);
        for i in 0..=steps {
            let value = (1.0 + i as f64 / steps as f64).powf(exponent);
            let hi = value as f32;
            table.push([hi, (value - f64::from(hi)) as f32, 0., 0.]);
        }
        for i in -149..=127 {
            let power = i as f64 * exponent;
            let shift = power.floor();
            let value = (power - shift).exp2();
            let hi = value as f32;
            table.push([hi, (value - f64::from(hi)) as f32, shift as f32, 0.]);
        }
        self.run(14, &[img], img.width(), img.height(), &[e, o, g, depth(d), steps as f32], &table)
    }
    pub fn hue_saturation(&self, img: &Image, h: f32, s: f32, l: f32, colorize: bool, d: Depth) -> Image {
        self.unary(
            15,
            img,
            &[h / 360., s / 100., l / 100., if colorize { 1. } else { 0. }, depth(d), (h * 4.25 + 0.5).floor()],
        )
    }
    /// Encoded-RGB Vibrance; PS 27.11 rejects this operation in native Float.
    /// HDR callers must perform the source workflow's explicit 16-bit conversion.
    /// Uses the measured 1024-entry sRGB ICC curve and linear-HSV red protection.
    pub fn vibrance(&self, img: &Image, v: f32, s: f32, d: Depth) -> Image {
        // Photoshop rounds signed half-integers towards +infinity.
        self.unary(16, img, &[(v + 0.5).floor() / 100., (s + 0.5).floor() / 100., depth(d)])
    }
    pub fn desaturate(&self, img: &Image, d: Depth) -> Image {
        self.unary(17, img, &[depth(d)])
    }
    pub fn black_and_white_default(&self, img: &Image) -> Image {
        self.unary(18, img, &[])
    }
    pub fn luminance(&self, img: &Image, d: Depth) -> Image {
        self.unary(19, img, &[depth(d)])
    }
    /// Perceptual Gradient Map, midpoint 50 and smoothness 100 (4096).
    /// Photoshop rejects this operation in native 32-bit documents.
    pub fn gradient_map(&self, img: &Image, stops: &[GradientStop], d: Depth) -> Image {
        assert!(!stops.is_empty());
        let mut stops: Vec<_> = stops
            .iter()
            .map(|s| {
                let [r, g, b] = s.rgb.map(|v| {
                    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
                });
                let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
                let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
                let t = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
                [
                    (s.pos * 4096.).round() / 4096.,
                    0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * t,
                    1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * t,
                    0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * t,
                ]
            })
            .collect();
        stops.sort_by(|a, b| a[0].total_cmp(&b[0]));
        self.run(
            20,
            &[img],
            img.width(),
            img.height(),
            &[stops.len() as f32, depth(d)],
            &stops,
        )
    }
    pub fn tonemap_exp(&self, img: &Image, e: f32, g: f32) -> Image {
        assert!(g > 0.);
        self.unary(21, img, &[e, g])
    }
    pub fn unmult(&self, img: &Image) -> Image {
        self.unary(22, img, &[])
    }
    pub fn quantize(&self, img: &Image, d: Depth) -> Image {
        self.unary(23, img, &[depth(d)])
    }
    /// Quantize owned storage, folding the first rounding into an unsubmitted
    /// producer only while no later command could have consumed its original output.
    pub fn quantize_owned(&self, mut img: Image, d: Depth) -> Image {
        if d == Depth::Float {
            return img;
        }
        {
            let pending = self.pending.lock();
            if img.pool.as_ptr() == Arc::as_ptr(&self.pool) && Arc::strong_count(&img.lease) == 1 {
                if let Some(producer) = &mut img.producer {
                    if producer.generation == pending.generation
                        && producer.epoch == pending.epoch
                        && pending.encoder.is_some()
                        && !producer.rounded
                    {
                        self.queue.write_buffer(&producer.uniform, 12, bytemuck::bytes_of(&depth(d)));
                        producer.rounded = true;
                        return img;
                    }
                }
            }
        }
        self.quantize(&img, d)
    }
    pub fn highlights_flatten(&self, img: &Image, d: Depth) -> Image {
        let guide = self.unary(24, img, &[depth(d), 1.]);
        let local = self.gaussian_blur(&guide, 64.);
        let long_edge = img.width().max(img.height());
        let reduced = (long_edge > 512).then(|| {
            let scale = 512. / long_edge as f32;
            let width = (img.width() as f32 * scale).round().max(1.) as u32;
            let height = (img.height() as f32 * scale).round().max(1.) as u32;
            // Decode each bilinear tap before reduction, without allocating a
            // full-resolution linear-light intermediate.
            if d == Depth::Float { self.resize_bilinear(img, width, height) }
            else { self.run(24, &[img], width, height, &[depth(d), 7.], &[]) }
        });
        let small = reduced.as_ref().unwrap_or(img);
        let moments = self.unary(24, small, &[if reduced.is_some() { 0. } else { depth(d) }, 2.]);
        let radius = (0.003 * small.width().max(small.height()) as f32).round().max(1.);
        let box_mean = |input: &Image| {
            let horizontal = self.unary(24, input, &[depth(d), 3., radius, 1., 0.]);
            self.unary(24, &horizontal, &[depth(d), 3., radius, 0., 1.])
        };
        let mean = box_mean(&moments);
        let coefficients = self.run(
            24,
            &[&moments, &mean],
            small.width(),
            small.height(),
            &[depth(d), 4.],
            &[],
        );
        let adaptive = box_mean(&coefficients);
        let packed = self.run(
            24,
            &[&local, &adaptive],
            img.width(),
            img.height(),
            &[depth(d), 5.],
            &[],
        );
        let mut key = None;
        let (mut key_width, mut key_height) = (small.width(), small.height());
        while key_width > 1 || key_height > 1 {
            key_width = key_width.div_ceil(2);
            key_height = key_height.div_ceil(2);
            key = Some(self.resize_bilinear(
                key.as_ref().unwrap_or(&moments),
                key_width,
                key_height,
            ));
        }
        self.run(
            24,
            &[img, &packed, key.as_ref().unwrap_or(&moments)],
            img.width(),
            img.height(),
            &[depth(d), 6.],
            &[],
        )
    }
    /// Photoshop layer composition: ordinary Fill × Opacity rounds to byte coverage;
    /// Linear/Color Dodge Fill scales source RGB instead. Byte layers round source
    /// Fill and blend RGB before opacity. Quantize the returned image at storage boundaries.
    /// Screen and Color Dodge at Float are mathematical extensions, unavailable in PS.
    pub fn blend(
        &self,
        base: &Image,
        top: &Image,
        mode: BlendMode,
        opacity: f32,
        fill: f32,
        d: Depth,
    ) -> Image {
        self.binary(
            25,
            base,
            top,
            &[mode as u32 as f32, opacity, fill, depth(d)],
        )
    }
    pub fn apply_image(
        &self,
        target: &Image,
        src: &Image,
        mode: BlendMode,
        opacity: f32,
        invert: bool,
        d: Depth,
    ) -> Image {
        self.binary(
            26,
            target,
            src,
            &[
                mode as u32 as f32,
                opacity,
                if invert { 1. } else { 0. },
                depth(d),
            ],
        )
    }
    pub fn extract_channel(&self, img: &Image, channel: u32) -> Image {
        assert!(channel < 3);
        self.unary(27, img, &[channel as f32])
    }
    pub fn apply_to_channel(
        &self,
        target: &Image,
        src: &Image,
        channel: u32,
        mode: BlendMode,
        opacity: f32,
    ) -> Image {
        assert!(channel < 3);
        self.binary(
            28,
            target,
            src,
            &[channel as f32, mode as u32 as f32, opacity],
        )
    }
    pub fn mask_alpha(&self, img: &Image, mask: &Image) -> Image {
        self.binary(29, img, mask, &[])
    }
    pub fn mix_by_mask(&self, a: &Image, b: &Image, mask: &Image) -> Image {
        assert_eq!((a.width(), a.height()), (mask.width(), mask.height()));
        self.run(30, &[a, b, mask], a.width(), a.height(), &[], &[])
    }
    /// sRGB depth conversion with Photoshop's 32768-step SDR input grid.
    /// HDR downconversion uses exposure-zero/gamma-one clipping, not adaptive toning.
    /// Eight-bit output is undithered (captured PS differs by up to one code).
    pub fn convert_depth(&self, img: &Image, from: Depth, to: Depth) -> Image {
        self.unary(31, img, &[depth(from), depth(to)])
    }
    fn binary(&self, op: u32, a: &Image, b: &Image, args: &[f32]) -> Image {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        self.run(op, &[a, b], a.width(), a.height(), args, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::{Depth, Gpu};

    #[test]
    fn texture_pool_bounds_idle_bytes_and_reuses_newest() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        let mut pool = super::TexturePool::new(128);
        let check = |pool: &super::TexturePool| {
            assert!(pool.bytes <= pool.budget);
            assert_eq!(
                pool.bytes,
                pool.by_size
                    .iter()
                    .map(|(&(w, h), textures)| {
                        assert!(!textures.is_empty());
                        u64::from(w) * u64::from(h) * 16 * textures.len() as u64
                    })
                    .sum::<u64>()
            );
        };
        let oldest = Gpu::texture_new(gpu.device(), 1, 1);
        let other_size = Gpu::texture_new(gpu.device(), 2, 1);
        let newest = Gpu::texture_new(gpu.device(), 1, 1);
        for texture in [&oldest, &other_size, &newest] {
            pool.insert(texture.clone());
            check(&pool);
        }
        assert_eq!(pool.take((1, 1)).unwrap(), newest);
        check(&pool);
        pool.insert(newest.clone());
        check(&pool);
        pool.insert(Gpu::texture_new(gpu.device(), 4, 1));
        check(&pool);
        assert_eq!(pool.bytes, 128);
        pool.insert(Gpu::texture_new(gpu.device(), 3, 1));
        check(&pool);
        assert_eq!(pool.bytes, 128);
        assert!(pool.take((2, 1)).is_none());
        check(&pool);
        assert_eq!(pool.take((1, 1)).unwrap(), newest);
        check(&pool);
        assert!(!pool.by_size.contains_key(&(1, 1)));
        for _ in 0..32 {
            pool.insert(newest.clone());
            check(&pool);
            assert_eq!(pool.take((1, 1)).unwrap(), newest);
            check(&pool);
        }
        let before = pool.bytes;
        pool.insert(Gpu::texture_new(gpu.device(), 9, 1));
        check(&pool);
        assert_eq!(pool.bytes, before);
        assert!(!pool.by_size.contains_key(&(9, 1)));
        assert!(pool.take((4, 1)).is_some());
        check(&pool);
        assert!(pool.take((3, 1)).is_some());
        check(&pool);
        assert_eq!(pool.bytes, 0);
        assert!(pool.by_size.is_empty());
        pool.grow_budget(1, 1);
        assert_eq!(pool.budget, 512 * 1024 * 1024);
        pool.grow_budget(1086, 1448);
        assert_eq!(pool.budget, 1086 * 1448 * 16 * 64);
        pool.grow_budget(4000, 4000);
        assert_eq!(pool.budget, 2 * 1024 * 1024 * 1024);
        pool.grow_budget(1, 1);
        assert_eq!(pool.budget, 2 * 1024 * 1024 * 1024);
        check(&pool);
    }

    #[test]
    fn transfers_band_at_injected_buffer_limits() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        // Odd widths exercise readback padding; seven rows leave a partial final band.
        for w in [3u32, 19] {
            let h = 7;
            let pixels: Vec<f32> = (0..w * h * 4)
                .map(|i| (i as f32 - 71.) / 16.)
                .collect();
            let pitch = u64::from((w * 16).div_ceil(256) * 256);
            for rows in [1, 2, 3] {
                let cap = pitch * rows + 17;
                assert_eq!(Gpu::transfer_rows(w, 16, cap), rows as u32);
                let image = gpu.upload_rgba32f_capped(w, h, &pixels, cap);
                assert_eq!(gpu.download_rgba32f_capped(&image, cap), pixels);
                // Also exercise a pending consumer before the banded readback.
                let copied = gpu.copy(&image);
                assert_eq!(gpu.download_rgba32f_capped(&copied, cap), pixels);
            }
            let gray: Vec<f32> = (0..w * h).map(|i| (i as f32 - 13.) / 8.).collect();
            let expected: Vec<f32> = gray.iter().flat_map(|&v| [v, v, v, 1.]).collect();
            for cap in [256, 529] {
                let image = gpu.upload_gray32f_capped(w, h, &gray, cap);
                assert_eq!(gpu.download_rgba32f_capped(&image, pitch * 2), expected);
            }
        }
    }

    #[test]
    fn region_upload_bands_preserve_offsets_and_pending_consumers() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        let original = [-2., 3., 0.5, 1.];
        for cap in [256, 529] {
            let image = gpu.fill(5, 9, original);
            let before = gpu.copy(&image);
            let patch: Vec<f32> = (0..3 * 7 * 4).map(|i| (i as f32 - 37.) / 8.).collect();
            gpu.write_region_capped(&image, 1, 1, 3, 7, &patch, cap);
            assert_eq!(gpu.download_rgba32f(&before), original.repeat(45));
            let mut expected = original.repeat(45);
            for y in 0..7 {
                expected[((y + 1) * 5 + 1) * 4..((y + 1) * 5 + 4) * 4]
                    .copy_from_slice(&patch[y * 12..(y + 1) * 12]);
            }
            assert_eq!(gpu.download_rgba32f(&image), expected);
        }
    }

    #[test]
    fn write_region_preserves_float_pixels_and_pending_consumers() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        let original = [-2., 3., 0.5, 1.];
        let image = gpu.fill(3, 2, original);
        let before = gpu.copy(&image);
        let patch = [
            4., -3., 0.25, 0.5, 5., -4., 0.75, 1.,
            6., -5., 1.25, 0.25, 7., -6., 1.5, 0.,
        ];
        gpu.write_region(&image, 1, 0, 2, 2, &patch);
        assert_eq!(gpu.download_rgba32f(&before), original.repeat(6));
        let mut expected = original.repeat(6);
        expected[4..12].copy_from_slice(&patch[..8]);
        expected[16..24].copy_from_slice(&patch[8..]);
        assert_eq!(gpu.download_rgba32f(&image), expected);
    }

    #[test]
    fn owned_store_round_matches_separate_and_guards_consumers_bitwise() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        let pixels = [
            -2., 1.5, 0.5 / 255. - 1e-8, -0.25,
            0.00195, 0.50001, 0.99999, 1.25,
            0., 1., 0.5 / 32768., 0.1234567,
            0.5 / 255. + 1e-8, -0.001, 3.25, 0.5,
        ];
        let source = gpu.upload_rgba32f(4, 1, &pixels);
        let bits = |img: &super::Image| {
            gpu.download_rgba32f(img).into_iter().map(f32::to_bits).collect::<Vec<_>>()
        };
        for d in [Depth::Eight, Depth::Sixteen] {
            for transform in [false, true] {
                let producer = if transform {
                    gpu.exposure(&source, 0.75, -0.125, 1., Depth::Float)
                } else {
                    gpu.copy(&source)
                };
                let generation = producer.producer.as_ref().unwrap().generation;
                let fused = gpu.quantize_owned(producer, d);
                assert_eq!(fused.producer.as_ref().unwrap().generation, generation);
                assert!(fused.producer.as_ref().unwrap().rounded);
                let producer = if transform {
                    gpu.exposure(&source, 0.75, -0.125, 1., Depth::Float)
                } else {
                    gpu.copy(&source)
                };
                let separate = gpu.quantize(&producer, d);
                assert_eq!(bits(&fused), bits(&separate), "{d:?} transform={transform}");
            }

            let producer = gpu.copy(&source);
            let generation = producer.producer.as_ref().unwrap().generation;
            let consumer = gpu.copy(&producer);
            let rounded = gpu.quantize_owned(producer, d);
            assert_ne!(rounded.producer.as_ref().unwrap().generation, generation);
            assert!(!rounded.producer.as_ref().unwrap().rounded);
            assert_eq!(bits(&consumer), pixels.map(f32::to_bits));
            assert_eq!(bits(&rounded), bits(&gpu.quantize(&source, d)));

            let producer = gpu.copy(&source);
            let generation = producer.producer.as_ref().unwrap().generation;
            gpu.flush();
            let rounded = gpu.quantize_owned(producer, d);
            assert_ne!(rounded.producer.as_ref().unwrap().generation, generation);
            assert!(!rounded.producer.as_ref().unwrap().rounded);
            assert_eq!(bits(&rounded), bits(&gpu.quantize(&source, d)));
        }

        for (first, second) in [
            (Depth::Eight, Depth::Sixteen),
            (Depth::Sixteen, Depth::Eight),
        ] {
            let producer = gpu.copy(&source);
            let once = gpu.quantize_owned(producer, first);
            let generation = once.producer.as_ref().unwrap().generation;
            let twice = gpu.quantize_owned(once, second);
            assert_ne!(twice.producer.as_ref().unwrap().generation, generation);
            let separate = gpu.quantize(&gpu.quantize(&source, first), second);
            let actual = bits(&twice);
            assert_eq!(actual, bits(&separate), "{first:?} then {second:?}");
            assert_ne!(actual, bits(&gpu.quantize(&source, second)));
        }

        let producer = gpu.copy(&source);
        let generation = producer.producer.as_ref().unwrap().generation;
        let unchanged = gpu.quantize_owned(producer, Depth::Float);
        assert_eq!(unchanged.producer.as_ref().unwrap().generation, generation);
        assert!(!unchanged.producer.as_ref().unwrap().rounded);
        assert_eq!(bits(&unchanged), pixels.map(f32::to_bits));
    }

    #[test]
    fn fused_scan_matches_separate_passes_bitwise() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        // Cancellation and mixed magnitudes expose changes to addition order.
        let values = [1e20, 1., -1e20, 0.125, -3.25, 1e-10, 7.1];
        for vertical in [false, true] {
            for major in [2, 3, 4, 5, 8, 9, 16, 17, 33] {
                let (w, h) = if vertical { (3, major) } else { (major, 3) };
                let pixels: Vec<f32> = (0..w * h * 4)
                    .map(|i| values[i as usize % values.len()])
                    .collect();
                let image = gpu.upload_rgba32f(w, h, &pixels);
                let axis = if vertical { 1. } else { 0. };
                for stages in 1..=4 {
                    let mut fused = gpu.copy(&image);
                    let mut separate = gpu.copy(&image);
                    let mut offset = 1;
                    while offset < major {
                        let second = stages >= 2 && 2 * offset < major;
                        let third = stages >= 3 && 4 * offset < major;
                        let fourth = stages >= 4 && 8 * offset < major;
                        fused = gpu.run(33, &[&fused], w, h, &[
                            axis, offset as f32,
                            if second { 1. } else { 0. },
                            if third { 1. } else { 0. },
                            if fourth { 1. } else { 0. },
                        ], &[]);
                        for stage in 0..stages {
                            let step = offset << stage;
                            if step < major {
                                separate = gpu.run(33, &[&separate], w, h, &[axis, step as f32, 0., 0., 0.], &[]);
                            }
                        }
                        let actual = gpu.download_rgba32f(&fused);
                        let expected = gpu.download_rgba32f(&separate);
                        for (i, (a, b)) in actual.iter().zip(&expected).enumerate() {
                            assert_eq!(a.to_bits(), b.to_bits(),
                                "vertical={vertical} major={major} stages={stages} offset={offset} channel={i}");
                        }
                        offset <<= stages;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod motion_tests;
#[cfg(test)]
mod chunk_tests;

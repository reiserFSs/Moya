#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod bridge;
mod canvas;
mod data;
mod history;
mod mask;
mod panels;
mod workers;
mod layers;
mod projects;
mod refine;
mod bridge_session;
mod platform_paths;
mod update;
mod theme;
mod thumbs;
#[cfg(target_os = "macos")]
mod macos_open;
#[cfg(target_os = "macos")]
mod macos_color;

use eframe::{
    egui,
    egui_wgpu,
};
use moya_core::*;
use moya_gpu::{Depth, Gpu, Image, wgpu};
use moya_io::{Picture, SampleDepth};
use moya_pipeline::{Document, LayerInputs, RenderRequest, ViewFlags};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

fn main() -> eframe::Result {
    #[cfg(target_os = "macos")]
    let (open_handler, open_documents) = macos_open::install();
    let path = std::env::args_os().skip(1)
        .find(|arg| {
            let arg = arg.to_string_lossy();
            !arg.starts_with("--") && !arg.starts_with("-psn_")
        })
        .map(PathBuf::from);
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default()
        .with_surface_config(egui_wgpu::SurfaceConfig::LOW_LATENCY);
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
            label: Some("Moya device"),
            required_limits: moya_gpu::device_limits(adapter),
            ..Default::default()
        });
    }
    eframe::run_native(
        "Moya",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 850.0])
                .with_app_id("Moya")
                .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/window-icon-256.png"))
                    .expect("embedded Moya window icon")),
            wgpu_options,
            persist_window: true,
            ..Default::default()
        },
        Box::new(move |cc| {
            #[cfg(target_os = "macos")]
            open_handler.set_context(cc.egui_ctx.clone());
            let app = App::new(cc, path);
            #[cfg(target_os = "macos")]
            let app = {
                let mut app = app;
                app.open_documents = Some(open_documents);
                app.open_handler = Some(open_handler);
                app
            };
            Ok(Box::new(app))
        }),
    )
}

struct Preview {
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
}

fn preview_request(view: ViewFlags, selected: u64, solo_layer: Option<u64>) -> RenderRequest {
    RenderRequest { mode: RenderMode::Draft, view, selected: Some(selected), solo_layer,
        want_layer_glows: false, want_xray: view.xray,
        want_composite: true, want_glow_composite: false }
}
struct App {
    gpu: Arc<Gpu>,
    render_state: egui_wgpu::RenderState,
    layers: Vec<layers::Layer>,
    selected: u64,
    solo_layer: Option<u64>,
    project: projects::State,
    /// Last window title sent: egui requests a repaint for every viewport command,
    /// so resending it each frame kept the app redrawing nonstop.
    window_title: String,
    hover_params: Option<Params>,
    last_render_ms: Option<f32>,
    last_rendered: std::time::Instant,
    idle_trimmed: bool,
    busy: bool,
    history: history::History,
    last_folder: Option<PathBuf>,
    renderer: Option<Document>,
    refiner: refine::Refiner,
    picture: Option<Picture>,
    source_preview: Option<Image>,
    path: Option<PathBuf>,
    preview: Option<Preview>,
    auto_update: bool,
    xray: bool,
    solo: bool,
    dirty: bool,
    force: bool,
    status: String,
    updates: update::Updates,
    panels: panels::PanelState,
    canvas: canvas::CanvasState,
    thumbs: thumbs::Thumbs,
    logo: egui::TextureHandle,
    set_center: bool,
    glow_only: bool,
    export_depth: usize,
    egui_ctx: egui::Context,
    loader: workers::Loader,
    loading: Option<(u64, PathBuf, Option<u64>)>,
    saving: Option<mpsc::Receiver<anyhow::Result<PathBuf>>>,
    bridge: Option<bridge::Bridge>,
    session: Option<bridge::Edit>,
    session_ready: bool,
    send_pending: bool,
    sending: Option<(workers::SendBatch, Option<serde_json::Value>)>,
    bridge_test_auto_send: bool,
    #[cfg(target_os = "macos")]
    open_documents: Option<mpsc::Receiver<PathBuf>>,
    #[cfg(target_os = "macos")]
    open_handler: Option<macos_open::OpenDocuments>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        theme::install(&cc.egui_ctx);
        let render_state = cc.wgpu_render_state.clone().expect("wgpu backend required");
        let gpu = Arc::new(Gpu::from_device(
            render_state.device.clone(),
            render_state.queue.clone(),
        ));
        let logo = image::load_from_memory(include_bytes!("../assets/header-logo-128.png"))
            .expect("embedded Moya logo").into_rgba8();
        let logo = cc.egui_ctx.load_texture("Moya logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [logo.width() as usize, logo.height() as usize], logo.as_raw()),
            egui::TextureOptions::LINEAR);
        let loaded = data::load_presets();
        let status = loaded
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        let mut params: Params = cc.storage
            .and_then(|s| s.get_string("moya.params"))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        params.sanitize();
        let layers = layers::new_document(params);
        let selected = layers[0].settings.id;
        let last_folder = cc.storage
            .and_then(|s| s.get_string("moya.last_folder"))
            .map(PathBuf::from);
        let refiner = refine::Refiner::new(gpu.clone(), cc.egui_ctx.clone());
        let mut app = Self {
            gpu,
            render_state,
            history: history::History::new(&layers, selected),
            layers,
            selected,
            solo_layer: None,
            project: projects::State::default(),
            window_title: String::new(),
            last_folder,
            hover_params: None,
            last_render_ms: None,
            last_rendered: std::time::Instant::now(),
            idle_trimmed: false,
            busy: false,
            renderer: None,
            refiner,
            picture: None,
            source_preview: None,
            path: None,
            preview: None,
            auto_update: true,
            xray: false,
            solo: false,
            dirty: false,
            force: false,
            status,
            updates: update::Updates::new(cc.storage, cc.egui_ctx.clone()),
            panels: panels::PanelState::new(loaded),
            canvas: canvas::CanvasState::default(),
            thumbs: thumbs::Thumbs::default(),
            logo,
            set_center: false,
            glow_only: false,
            export_depth: 1,
            egui_ctx: cc.egui_ctx.clone(),
            loader: workers::Loader::new(cc.egui_ctx.clone()),
            loading: None,
            saving: None,
            bridge: None,
            session: None,
            session_ready: false,
            send_pending: false,
            sending: None,
            bridge_test_auto_send: std::env::args().any(|arg| arg == "--bridge-test-auto-send"),
            #[cfg(target_os = "macos")]
            open_documents: None,
            #[cfg(target_os = "macos")]
            open_handler: None,
        };
        match bridge::Bridge::start(cc.egui_ctx.clone()) {
            Ok(bridge) => app.bridge = Some(bridge),
            Err(error) => app.status = format!("Photoshop bridge unavailable: {error:#}"),
        }
        if let Some(path) = path {
            app.open(&path);
        }
        app
    }
    /// Decodes on a worker thread so large files don't freeze the window.
    fn open_dialog(&mut self) {
        self.status.clear();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Images and Moya projects", &["moya", "png", "jpg", "jpeg", "tif", "tiff", "exr", "psd", "psb"]);
        if let Some(folder) = &self.last_folder {
            dialog = dialog.set_directory(folder);
        }
        if let Some(path) = dialog.pick_file() { self.open(&path); }
    }

    fn selected_index(&self) -> usize {
        self.layers.iter().position(|layer| layer.settings.id == self.selected).unwrap_or(0)
    }
    fn restore_history(&mut self, redo: bool) {
        for layer in &mut self.layers { layer.mask.end_stroke(); }
        self.history.observe(&mut self.layers, self.selected, false);
        let restored = if redo {
            self.history.redo(&mut self.layers, &mut self.selected)
        } else {
            self.history.undo(&mut self.layers, &mut self.selected)
        };
        if restored {
            self.hover_params = None;
            self.dirty = true;
            self.force = true;
            self.status.clear();
        }
    }
    fn open(&mut self, path: &Path) {
        self.guard_discard(projects::Pending::Open(path.to_owned()));
    }
    fn open_now(&mut self, path: &Path) {
        self.cancel_session();
        if projects::is_project(path) { self.open_project(path); return; }
        self.request_load(path, false);
    }
    fn request_load(&mut self, path: &Path, mask: bool) {
        let generation = self.loader.request(path.to_owned());
        self.loading = Some((generation, path.to_owned(), mask.then_some(self.selected)));
        self.busy = true;
        self.status = format!("Loading {}…", path.file_name().unwrap_or_default().to_string_lossy());
    }
    fn poll_loading(&mut self) {
        let Some((generation, path, result)) = self.loader.poll() else { return; };
        let Some((current, _, mask)) = self.loading.as_ref() else { return; };
        if generation != *current { return; }
        let mask = *mask;
        self.loading = None;
        self.busy = false;
        if let Some(id) = mask {
            match result {
                Ok(pic) => {
                    if let Some(layer) = self.layers.iter_mut().find(|layer| layer.settings.id == id) {
                        layer.mask.load_image(&pic);
                    }
                    self.status.clear();
                }
                Err(e) => self.status = format!("Mask load failed: {e:#}"),
            }
            return;
        }
        let result = result.and_then(|pic| {
            if let Some(session) = &self.session {
                let depth = match pic.depth { SampleDepth::U8 => 8, SampleDepth::U16 => 16, SampleDepth::F32 => 32 };
                anyhow::ensure!(
                    pic.width == session.document.width && pic.height == session.document.height && depth == session.document.depth,
                    "Source TIFF dimensions/depth do not match the Photoshop document"
                );
            }
            Ok(pic)
        });
        match result {
            Ok(pic) => {
                let opened = self.finish_open(&path, pic);
                if let Some(session) = self.session.as_ref() {
                    if opened {
                        self.session_ready = true;
                        self.bridge_reply(serde_json::json!({"type":"opened","id":session.id}));
                        if self.bridge_test_auto_send { self.queue_send(); }
                    } else {
                        self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
                        self.session = None;
                    }
                }
            }
            Err(e) => {
                self.status = format!("Open failed: {e:#}");
                if let Some(session) = self.session.take() {
                    self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
                }
            }
        }
    }
    fn finish_open(&mut self, path: &Path, mut pic: Picture) -> bool {
        let limit = self.gpu.max_image_dimension();
        if pic.width > limit || pic.height > limit {
            self.status = format!(
                "Open failed: {}×{} exceeds this GPU's {limit} px texture limit",
                pic.width, pic.height
            );
            return false;
        }
        let params = self.layers[self.selected_index()].settings.params.clone();
        self.layers = layers::new_document(params);
        self.selected = self.layers[0].settings.id;
        self.solo_layer = None;
        let depth = match pic.depth {
            SampleDepth::U8 => Depth::Eight,
            SampleDepth::U16 => Depth::Sixteen,
            SampleDepth::F32 => Depth::Float,
        };
        let image = self.gpu.upload_rgba32f(pic.width, pic.height, &pic.rgba);
        pic.rgba = Vec::new();
        let (w, h) = moya_core::DocGeometry { orig_w: pic.width, orig_h: pic.height }.working_size(RenderMode::Draft);
        let draft = self.gpu.resize_bicubic(&image, w, h);
        self.source_preview = Some(self.gpu.convert_depth(&draft, depth, Depth::Eight));
        self.refiner.cancel();
        self.renderer = Some(Document::new(self.gpu.clone(), image, depth));
        for layer in &mut self.layers {
            layer.settings.params.hdr32 = depth == Depth::Float;
            if depth == Depth::Float { layer.settings.params.gamma = 1.0; }
            layer.mask.reset(pic.width, pic.height);
        }
        self.picture = Some(pic);
        self.path = Some(path.to_owned());
        self.last_folder = path.parent().map(Path::to_owned);
        self.hover_params = None;
        self.last_render_ms = None;
        self.history.clear(&self.layers, self.selected);
        self.dirty = true;
        self.force = true;
        self.canvas.reset_source();
        self.reset_project();
        self.status = "Image ready".into();
        true
    }
    fn flags(&self) -> ViewFlags {
        ViewFlags {
            xray: self.xray,
            solo: self.solo,
            host_is_float: self
                .picture
                .as_ref()
                .is_some_and(|p| p.depth == SampleDepth::F32),
        }
    }
    fn display(&mut self, image: &Image) {
        let size = [image.width(), image.height()];
        if self.preview.as_ref().is_none_or(|p| p.size != size) {
            if let Some(p) = self.preview.take() {
                self.render_state.renderer.write().free_texture(&p.id);
            }
            let texture = self.gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("Moya GPU preview"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let id = self.render_state.renderer.write().register_native_texture(
                self.gpu.device(),
                &view,
                wgpu::FilterMode::Linear,
            );
            self.preview = Some(Preview { texture, id, size });
        }
        self.gpu
            .write_display_rgba8(image, &self.preview.as_ref().unwrap().texture);
        self.gpu.flush();
    }
    fn render(&mut self) {
        if self.renderer.is_none() {
            self.dirty = false;
            self.force = false;
            return;
        }
        let started = std::time::Instant::now();
        self.busy = true;
        self.render_preview();
        self.last_render_ms = Some(started.elapsed().as_secs_f32() * 1000.0);
        self.last_rendered = std::time::Instant::now();
        self.idle_trimmed = false;
        self.busy = self.loading.is_some();
    }
    fn render_preview(&mut self) {
        let flags = self.flags();
        let index = self.selected_index();
        let original = self.hover_params.as_ref().map(|params| {
            std::mem::replace(&mut self.layers[index].settings.params, params.clone())
        });
        let inputs: Vec<_> = self.layers.iter().map(|layer| LayerInputs {
            settings: &layer.settings, mask: layer.mask.gpu_image(), mask_generation: layer.mask.generation(),
        }).collect();
        let mut request = preview_request(flags, self.selected, self.solo_layer);
        if self.glow_only {
            request.want_composite = false;
            request.want_glow_composite = true;
        }
        let output = self.renderer.as_mut().unwrap().render(&inputs, &request);
        if let Some(params) = original { self.layers[index].settings.params = params; }
        self.display(output.xray.as_ref().or(output.composite.as_ref()).or(output.glow_composite.as_ref()).expect("requested preview"));
        self.canvas.invalidate();
        self.dirty = false;
        self.force = false;
    }
    fn export(&mut self) {
        if self.saving.is_some() { return; }
        self.status.clear();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PNG / TIFF / OpenEXR", &["png", "tif", "tiff", "exr"])
            .set_file_name("Moya.png");
        if let Some(folder) = &self.last_folder {
            dialog = dialog.set_directory(folder);
        }
        let Some(path) = dialog.save_file() else { return; };
        let depth = if path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("exr"))
        {
            SampleDepth::F32
        } else {
            [SampleDepth::U8, SampleDepth::U16, SampleDepth::F32][self.export_depth]
        };
        let pic = match self.render_hq(self.glow_only, false, depth) {
            Ok(pic) => pic,
            Err(e) => { self.status = format!("Save failed: {e:#}"); return; }
        };
        let (tx, rx) = mpsc::channel();
        self.saving = Some(rx);
        self.status = "Saving HQ…".into();
        self.busy = true;
        let ctx = self.egui_ctx.clone();
        let gpu = self.gpu.clone();
        std::thread::spawn(move || {
            let result = pic.save(&gpu, &path).map(|()| path);
            drop(pic);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn render_hq(&mut self, glow: bool, opaque: bool, depth: SampleDepth) -> anyhow::Result<workers::ExportImage> {
        let flags = ViewFlags { xray: false, solo: false, host_is_float: self.flags().host_is_float };
        let inputs: Vec<_> = self.layers.iter().map(|layer| LayerInputs {
            settings: &layer.settings, mask: layer.mask.gpu_image(), mask_generation: layer.mask.generation(),
        }).collect();
        let renderer = self.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("Open an image first"))?;
        let output = renderer.render(&inputs, &RenderRequest { mode: RenderMode::Hq, view: flags,
            selected: Some(self.selected), solo_layer: None, want_layer_glows: false, want_xray: false,
            want_composite: !glow, want_glow_composite: glow });
        let image = if glow { output.glow_composite } else { output.composite }.expect("requested HQ output");
        let from = if !glow && flags.host_is_float { Depth::Float } else { Depth::Eight };
        let to = match depth { SampleDepth::U8 => Depth::Eight, SampleDepth::U16 => Depth::Sixteen, SampleDepth::F32 => Depth::Float };
        let image = if from != to { self.gpu.convert_depth(&image, from, to) } else { image };
        let pic = workers::ExportImage {
            image, depth, opaque,
            icc_profile: self.picture.as_ref().and_then(|p| p.icc_profile.clone()),
        };
        self.dirty = true;
        self.force = true;
        Ok(pic)
    }

    fn trim_idle(&self) {
        let budget = self.source_preview.as_ref()
            .map_or(0, |image| image.width() as u64 * image.height() as u64 * 16 * 3);
        self.gpu.trim_idle(budget);
    }


    fn sync_mask(&mut self, ctx: &egui::Context) {
        self.history.observe(&mut self.layers, self.selected, ctx.input(|i| i.pointer.any_down()));
        for layer in &mut self.layers {
            // Rays, glare and CA make each mid-stroke preview slow enough to stall the brush:
            // for those layers the glow updates when the brush rests and on release.
            let p = &layer.settings.params;
            let live = !(p.rays_on || p.glare_on || p.ca_on);
            if !matches!(layer.mask.sync(ctx, &self.gpu, live), mask::MaskSync::Unchanged) {
                self.dirty = true;
                self.force = true;
            }
        }
    }
}
impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.updates.save(storage);
        let params = &self.layers[self.selected_index()].settings.params;
        if let Ok(json) = serde_json::to_string(params) {
            storage.set_string("moya.params", json);
        }
        if let Some(folder) = &self.last_folder {
            storage.set_string("moya.last_folder", folder.to_string_lossy().into_owned());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Color-manage the window like other Mac apps (see macos_color).
        #[cfg(target_os = "macos")]
        macos_color::tag_srgb(frame);
        #[cfg(not(target_os = "macos"))]
        let _ = frame;
        #[cfg(target_os = "macos")]
        while let Some(path) = self.open_documents.as_ref().and_then(|receiver| receiver.try_recv().ok()) {
            self.open(&path);
        }
        self.poll_bridge();
        self.poll_loading();
        if let Some(rx) = &self.saving {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("image writer stopped"))),
            };
            if let Some(result) = result {
                self.saving = None;
                self.trim_idle();
                self.status = match result {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => format!("Save failed: {e:#}"),
                };
            }
        }
        let before: Vec<_> = self.layers.iter().map(|layer| layer.settings.clone()).collect();
        let old_hover = self.hover_params.clone();
        let old_view = (self.xray, self.solo, self.solo_layer, self.selected, self.glow_only, self.export_depth);
        let ctx = ui.ctx().clone();
        self.handle_project_close(&ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
            self.queue_send();
        }
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
                self.open_dialog();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S)) {
                self.save_project(true);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                self.save_project(false);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::E)) {
                self.export();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)) {
                self.restore_history(true);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)) {
                self.restore_history(false);
            }
        }
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_owned())) {
            self.open(&path);
        }
        self.busy = self.send_pending || self.sending.is_some() || self.saving.is_some() || self.loading.is_some()
            || (self.renderer.is_some() && self.dirty && (self.auto_update || self.force));
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(32.0, 32.0)));
                ui.heading("MOYA");
                if ui.button("Open…").on_hover_text("Open image or .moya project (Cmd/Ctrl+O)").clicked() {
                    self.open_dialog();
                }
                self.project_controls(ui);
                ui.separator();
                let undo = ui.add_enabled(self.history.can_undo(), egui::Button::new(" ").min_size(egui::vec2(30.0, 26.0)));
                paint_toolbar_icon(ui, &undo, 0);
                if undo.on_hover_text("Undo (Cmd/Ctrl+Z)").clicked() {
                    self.restore_history(false);
                }
                let redo = ui.add_enabled(self.history.can_redo(), egui::Button::new(" ").min_size(egui::vec2(30.0, 26.0)));
                paint_toolbar_icon(ui, &redo, 1);
                if redo.on_hover_text("Redo (Cmd/Ctrl+Shift+Z)").clicked() {
                    self.restore_history(true);
                }
                let title = self.document_title();
                let right_width = if self.session.is_some() { 390.0 } else { 170.0 };
                ui.allocate_ui_with_layout(egui::vec2((ui.available_width() - right_width).max(0.0), 32.0),
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| { ui.weak(title); });
                if let Some(session) = &self.session {
                    let send_hint = format!("Send HQ result to {} (document {}; Cmd+Enter)", session.document.name, session.document.id);
                    if ui.add_enabled(self.session_ready && self.sending.is_none() && !self.send_pending,
                        egui::Button::new(egui::RichText::new("Send to Photoshop").color(theme::ACCENT))
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT)))
                        .on_hover_text(send_hint).clicked() {
                        self.queue_send();
                    }
                    if ui.small_button("Cancel").on_hover_text("Cancel Photoshop session").clicked() {
                        self.cancel_session();
                        self.status = "Photoshop session cancelled".into();
                    }
                    if self.send_pending || self.sending.is_some() { ui.spinner(); }
                }
                let export = ui.add(egui::Button::new(egui::RichText::new("Export  ▾").strong().color(ui.visuals().panel_fill))
                    .fill(theme::ACCENT).min_size(egui::vec2(86.0, 24.0)))
                    .on_hover_text("Export HQ image (Cmd/Ctrl+E)");
                egui::Popup::menu(&export).show(|ui| {
                    ui.small(egui::RichText::new("BIT DEPTH").color(ui.visuals().weak_text_color()));
                    theme::segmented(ui, &mut self.export_depth, &[(0, "8 bit"), (1, "16 bit"), (2, "32 bit")]);
                    ui.add_space(4.0);
                    ui.small(egui::RichText::new("OUTPUT").color(ui.visuals().weak_text_color()));
                    theme::segmented(ui, &mut self.glow_only, &[(false, "Composite"), (true, "Glow only")]);
                    ui.separator();
                    if ui.add_enabled(self.renderer.is_some() && self.saving.is_none(), egui::Button::new("Export…"))
                        .on_hover_text("Export HQ image (Cmd/Ctrl+E)").clicked() {
                        self.export();
                        ui.close();
                    }
                });
                let settings = ui.menu_button("   ", |ui| {
                    ui.checkbox(&mut self.updates.enabled, "Check for updates")
                        .on_hover_text("On launch, contact GitHub at most once per day. Changes apply to the next launch.");
                    if ui.button("Reset selected layer").clicked() {
                        self.status.clear();
                        self.hover_params = None;
                        let index = self.selected_index();
                        self.layers[index].settings.params = Params::default();
                        self.xray = false;
                        self.solo = false;
                        self.auto_update = true;
                        self.layers[index].mask.enabled = false;
                        self.layers[index].mask.tool = mask::Tool::None;
                        ui.close();
                    }
                }).response.on_hover_text("Settings");
                paint_toolbar_icon(ui, &settings, 2);
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.busy { ui.spinner(); }
                if let (Some(path), Some(pic)) = (&self.path, &self.picture) {
                    ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                    ui.separator();
                    let depth = match pic.depth {
                        SampleDepth::U8 => "8 bit",
                        SampleDepth::U16 => "16 bit",
                        SampleDepth::F32 => "32 bit float",
                    };
                    ui.label(format!("{}×{} · {depth}", pic.width, pic.height));
                }
                if let Some(ms) = self.last_render_ms {
                    ui.separator();
                    ui.label(format!("{ms:.1} ms"));
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.updates.note(ui);
                    if !self.auto_update && self.dirty && ui.button("Update").clicked() {
                        self.force = true;
                        self.dirty = true;
                    }
                    theme::switch(ui, &mut self.auto_update).on_hover_text("Render draft preview whenever parameters change");
                    ui.label("Live");
                });
            });
        });
        let collapsed_id = egui::Id::unique("moya.layers_collapsed");
        let collapsed = ctx.data_mut(|data| *data.get_persisted_mut_or_default::<bool>(collapsed_id));
        let toggle = if collapsed {
            egui::Panel::right("layers_strip").exact_size(52.0).resizable(false)
                .show(ui, |ui| self.layer_strip(ui)).inner
        } else {
            egui::Panel::right("layers").default_size(240.0).size_range(200.0..=360.0)
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| self.layer_controls(ui)).inner
        };
        if toggle { ctx.data_mut(|data| data.insert_persisted(collapsed_id, !collapsed)); }
        egui::Panel::left("controls")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(14, 8)))
            .default_size(350.0)
            .size_range(310.0..=500.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        self.confirm_dialogs(&ctx);
        self.unsaved_dialog(&ctx);
        for layer in &mut self.layers { layer.settings.sanitize(); }
        if old_hover != self.hover_params {
            self.dirty = true;
            self.canvas.invalidate();
            self.force = true;
        }
        if !self.layers.iter().map(|layer| &layer.settings).eq(before.iter()) {
            self.dirty = true;
            self.canvas.invalidate();
            self.status.clear();
        }
        // The canvas view bar toggles X-Ray and Hide source, so compare view flags after it runs.
        egui::CentralPanel::default().show(ui, |ui| self.canvas_ui(ui));
        if old_view != (self.xray, self.solo, self.solo_layer, self.selected, self.glow_only, self.export_depth) {
            self.dirty = true;
            self.canvas.invalidate();
            self.force = true;
        }
        self.history.observe(&mut self.layers, self.selected, ctx.input(|i| i.pointer.any_down()));
        self.sync_mask(&ctx);
        // Thumbnails follow finished glows; skip while dragging so strokes and slider drags
        // never pay for them (they catch up on release).
        if !ctx.input(|i| i.pointer.any_down()) {
            let ids: Vec<u64> = self.layers.iter().map(|layer| layer.settings.id).collect();
            self.thumbs.sync(&self.gpu, &self.render_state, self.renderer.as_ref(), &ids);
        }
        let title = self.project_title();
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
        if self.dirty && (self.auto_update || self.force) {
            self.render();
            ctx.request_repaint();
        }
        if self.dirty && self.auto_update {
            ui.ctx().request_repaint();
        }
        if !self.busy && !self.idle_trimmed {
            let elapsed = self.last_rendered.elapsed();
            if elapsed >= std::time::Duration::from_secs(5) {
                self.trim_idle();
                self.idle_trimmed = true;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs(5) - elapsed);
            }
        }
    }
}

fn paint_toolbar_icon(ui: &egui::Ui, response: &egui::Response, kind: u8) {
    let center = response.rect.center();
    let color = if response.enabled() { ui.visuals().text_color() } else { ui.visuals().weak_text_color().gamma_multiply(0.45) };
    let stroke = egui::Stroke::new(1.5, color);
    if kind == 2 {
        ui.painter().circle_stroke(center, 5.0, stroke);
        ui.painter().circle_stroke(center, 1.5, stroke);
        for i in 0..8 {
            let direction = egui::Vec2::angled(i as f32 * std::f32::consts::TAU / 8.0);
            ui.painter().line_segment([center + direction * 5.0, center + direction * 8.0], stroke);
        }
    } else {
        let direction = if kind == 0 { 1.0 } else { -1.0 };
        let point = |x: f32, y: f32| center + egui::vec2(x * direction, y);
        ui.painter().line_segment([point(-7.0, -3.0), point(2.0, -3.0)], stroke);
        ui.painter().line_segment([point(2.0, -3.0), point(6.0, 1.0)], stroke);
        ui.painter().line_segment([point(6.0, 1.0), point(6.0, 6.0)], stroke);
        ui.painter().line_segment([point(-7.0, -3.0), point(-2.0, -8.0)], stroke);
        ui.painter().line_segment([point(-7.0, -3.0), point(-2.0, 2.0)], stroke);
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn hidden_selected_layer_keeps_composite_fallback_in_xray() {
        let gpu = Arc::new(Gpu::new_blocking().expect("GPU is mandatory for preview tests"));
        let image = gpu.upload_rgba32f(1, 1, &[0.25, 0.5, 0.75, 1.0]);
        let mut document = Document::new(gpu, image, Depth::Eight);
        let mut layers = layers::new_document(Params::default());
        layers[0].settings.visible = false;
        let inputs = [LayerInputs { settings: &layers[0].settings, mask: None, mask_generation: 0 }];
        let request = preview_request(ViewFlags { xray: true, solo: false, host_is_float: false }, layers[0].settings.id, None);
        let output = document.render(&inputs, &request);
        assert!(output.xray.is_none());
        assert!(output.composite.is_some());
    }
}

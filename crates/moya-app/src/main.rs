mod bridge;
mod canvas;
mod data;
mod history;
mod mask;
mod panels;
mod workers;

use eframe::{
    egui,
    egui_wgpu,
};
use moya_core::*;
use moya_gpu::{Depth, Gpu, Image, wgpu};
use moya_io::{Picture, SampleDepth};
use moya_pipeline::{Renderer, ViewFlags};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

fn main() -> eframe::Result {
    let path = std::env::args_os().skip(1)
        .find(|arg| !arg.to_string_lossy().starts_with("--"))
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
        Box::new(move |cc| Ok(Box::new(App::new(cc, path)))),
    )
}

struct Preview {
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
}
struct App {
    gpu: Arc<Gpu>,
    render_state: egui_wgpu::RenderState,
    params: Params,
    hover_params: Option<Params>,
    last_render_ms: Option<f32>,
    busy: bool,
    history: history::History,
    last_folder: Option<PathBuf>,
    renderer: Option<Renderer>,
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
    panels: panels::PanelState,
    canvas: canvas::CanvasState,
    mask: mask::Mask,
    logo: egui::TextureHandle,
    set_center: bool,
    glow_only: bool,
    export_depth: usize,
    egui_ctx: egui::Context,
    loader: workers::Loader,
    loading: Option<(u64, PathBuf, bool)>,
    saving: Option<mpsc::Receiver<anyhow::Result<PathBuf>>>,
    bridge: Option<bridge::Bridge>,
    session: Option<bridge::Edit>,
    session_ready: bool,
    send_pending: bool,
    sending: Option<(workers::SendWriter, Option<serde_json::Value>)>,
    bridge_test_auto_send: bool,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
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
        let last_folder = cc.storage
            .and_then(|s| s.get_string("moya.last_folder"))
            .map(PathBuf::from);
        let mut app = Self {
            gpu,
            render_state,
            history: history::History::new(&params),
            params,
            last_folder,
            hover_params: None,
            last_render_ms: None,
            busy: false,
            renderer: None,
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
            panels: panels::PanelState::new(loaded),
            canvas: canvas::CanvasState::default(),
            mask: mask::Mask::default(),
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
            .add_filter("Images", &["png", "jpg", "jpeg", "tif", "tiff", "exr", "psd", "psb"]);
        if let Some(folder) = &self.last_folder {
            dialog = dialog.set_directory(folder);
        }
        if let Some(path) = dialog.pick_file() { self.open(&path); }
    }

    fn restore_history(&mut self, redo: bool) {
        self.mask.end_stroke();
        self.record_mask_history();
        let restored = if redo { self.history.redo() } else { self.history.undo() };
        if let Some(edit) = restored {
            match edit {
                history::Edit::Params(params) => self.params = params,
                history::Edit::Mask => { if redo { self.mask.redo(); } else { self.mask.undo(); } }
            }
            self.hover_params = None;
            self.dirty = true;
            self.force = true;
            self.status.clear();
        }
    }
    fn open(&mut self, path: &Path) {
        self.cancel_session();
        self.request_load(path, false);
    }
    fn request_load(&mut self, path: &Path, mask: bool) {
        let generation = self.loader.request(path.to_owned());
        self.loading = Some((generation, path.to_owned(), mask));
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
        if mask {
            match result {
                Ok(pic) => { self.mask.load_image(&pic); self.status.clear(); }
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
                let pending_params = self.session.as_ref().and_then(|s| s.params.clone());
                let old_params = self.params.clone();
                if let Some(mut params) = pending_params { params.sanitize(); self.params = params; }
                let opened = self.finish_open(&path, pic);
                if !opened { self.params = old_params; }
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
    fn finish_open(&mut self, path: &Path, pic: Picture) -> bool {
        let limit = self.gpu.max_image_dimension();
        if pic.width > limit || pic.height > limit {
            self.status = format!(
                "Open failed: {}×{} exceeds this GPU's {limit} px texture limit",
                pic.width, pic.height
            );
            return false;
        }
        let depth = match pic.depth {
            SampleDepth::U8 => Depth::Eight,
            SampleDepth::U16 => Depth::Sixteen,
            SampleDepth::F32 => Depth::Float,
        };
        let image = self.gpu.upload_rgba32f(pic.width, pic.height, &pic.rgba);
        let (w, h) = moya_core::DocGeometry { orig_w: pic.width, orig_h: pic.height }.working_size(RenderMode::Draft);
        let draft = self.gpu.resize_bicubic(&image, w, h);
        self.source_preview = Some(self.gpu.convert_depth(&draft, depth, Depth::Eight));
        self.renderer = Some(Renderer::new(self.gpu.clone(), image, depth));
        self.params.hdr32 = depth == Depth::Float;
        if depth == Depth::Float { self.params.gamma = 1.0; }
        self.mask.reset(pic.width, pic.height);
        self.picture = Some(pic);
        self.path = Some(path.to_owned());
        self.last_folder = path.parent().map(Path::to_owned);
        self.hover_params = None;
        self.last_render_ms = None;
        self.history.clear(&self.params);
        self.dirty = true;
        self.force = true;
        self.canvas.reset_source();
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
        self.busy = self.loading.is_some();
    }
    fn render_preview(&mut self) {
        let params = self.hover_params.as_ref().unwrap_or(&self.params).clone();
        let flags = self.flags();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let composite = renderer.render_preview(&params, RenderMode::Draft, &flags);
        self.display(&composite);
        self.dirty = false;
        self.force = false;
    }
    fn save(&mut self) {
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
        let mut params = self.params.clone();
        if self.glow_only { params.unmult = true; }
        let pic = match self.render_hq(&params, self.glow_only, false, depth) {
            Ok(pic) => pic,
            Err(e) => { self.status = format!("Save failed: {e:#}"); return; }
        };
        let (tx, rx) = mpsc::channel();
        self.saving = Some(rx);
        self.status = "Saving HQ…".into();
        self.busy = true;
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = moya_io::save(&path, &pic, depth).map(|()| path);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn render_hq(&mut self, params: &Params, glow: bool, opaque: bool, depth: SampleDepth) -> anyhow::Result<Picture> {
        let flags = ViewFlags { xray: false, solo: false, host_is_float: self.flags().host_is_float };
        let renderer = self.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("Open an image first"))?;
        let output = renderer.render(params, RenderMode::Hq, &flags);
        let image = if glow { &output.glow } else { &output.composite };
        let from = if !glow && flags.host_is_float { Depth::Float } else { Depth::Eight };
        let to = match depth { SampleDepth::U8 => Depth::Eight, SampleDepth::U16 => Depth::Sixteen, SampleDepth::F32 => Depth::Float };
        let converted = (from != to).then(|| self.gpu.convert_depth(image, from, to));
        let image = converted.as_ref().unwrap_or(image);
        let mut rgba = self.gpu.download_rgba32f(image);
        if opaque {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[0] *= pixel[3];
                pixel[1] *= pixel[3];
                pixel[2] *= pixel[3];
                pixel[3] = 1.0;
            }
        }
        let pic = Picture {
            width: image.width(), height: image.height(), depth, rgba,
            icc_profile: self.picture.as_ref().and_then(|p| p.icc_profile.clone()),
        };
        self.dirty = true;
        self.force = true;
        Ok(pic)
    }

    fn bridge_reply(&self, value: serde_json::Value) {
        if let Some(bridge) = &self.bridge { bridge.send(value); }
    }

    fn cancel_session(&mut self) {
        if let Some(session) = self.session.take() {
            self.bridge_reply(serde_json::json!({"type":"cancelled","id":session.id}));
        }
        self.session_ready = false;
        self.send_pending = false;
        self.sending = None;
        self.loader.invalidate();
        self.loading = None;
    }

    fn queue_send(&mut self) {
        if self.session_ready && self.sending.is_none() && !self.send_pending {
            self.send_pending = true;
            self.status = "Rendering HQ for Photoshop…".into();
            self.egui_ctx.request_repaint();
        }
    }

    fn send_to_photoshop(&mut self) {
        self.send_pending = false;
        let Some(session) = self.session.clone() else { return; };
        let depth = match session.document.depth {
            8 => SampleDepth::U8, 16 => SampleDepth::U16, _ => SampleDepth::F32,
        };
        let glow = session.return_mode == bridge::ReturnMode::Glow;
        let mut params = self.params.clone();
        // Bridge glow is premultiplied RGB over black, never an unmult export.
        if glow { params.unmult = false; }
        let pic = match self.render_hq(&params, glow, glow, depth) {
            Ok(pic) => pic,
            Err(e) => {
                self.status = format!("Send failed: {e:#}");
                self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
                return;
            }
        };
        let stem = session.source.file_stem().unwrap_or_default().to_string_lossy();
        let path = session.source.with_file_name(format!("{stem}.moya.tif"));
        let message = serde_json::json!({
            "type":"result", "id":session.id, "path":path, "depth":session.document.depth,
            "returnMode":session.return_mode,
            "blend":if !glow { "normal" } else if session.document.depth == 32 { "linearDodge" } else { "screen" },
            "layerName":"Moya Glow", "params":params
        });
        self.sending = Some((workers::SendWriter::new(self.egui_ctx.clone(), path, pic, depth), Some(message)));
        self.status = "Sending to Photoshop…".into();
    }

    fn poll_bridge(&mut self) {
        // Queueing a send paints its busy state before the synchronous GPU render.
        if self.send_pending { self.send_to_photoshop(); }
        loop {
            let event = self.bridge.as_ref().and_then(|bridge| bridge.events.try_recv().ok());
            let Some(event) = event else { break; };
            match event {
                bridge::Event::Edit(edit) => {
                    self.open(&edit.source);
                    self.session = Some(edit);
                    self.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                bridge::Event::Placed(id) => {
                    if self.session.as_ref().is_some_and(|session| session.id == id) {
                        if let Some((writer, _)) = self.sending.take() { writer.keep_output(); }
                        self.status = "Placed in Photoshop".into();
                    }
                }
                bridge::Event::Error { id, message } => {
                    if self.session.as_ref().is_some_and(|session| session.id == id) {
                        self.status = format!("Photoshop: {message}");
                    }
                }
                bridge::Event::Disconnected => {
                    self.cancel_session();
                    self.status = "Photoshop bridge disconnected".into();
                }
            }
        }
        let result = self.sending.as_mut().and_then(|(writer, _)| writer.poll());
        if let Some(result) = result {
            match result {
                Ok(()) => {
                    if let Some(message) = self.sending.as_mut().and_then(|(_, message)| message.take()) {
                        self.bridge_reply(message);
                    }
                    self.status = "Waiting for Photoshop…".into();
                }
                Err(e) => {
                    self.sending = None;
                    self.status = format!("Send failed: {e:#}");
                    if let Some(session) = &self.session {
                        self.bridge_reply(serde_json::json!({"type":"error","id":session.id,"message":self.status}));
                    }
                }
            }
        }
    }
    fn record_mask_history(&mut self) {
        let (steps, evicted) = self.mask.take_history_steps();
        for _ in 0..steps { self.history.mask_step(); }
        self.history.drop_oldest_mask(evicted);
    }

    fn sync_mask(&mut self, ctx: &egui::Context) {
        self.record_mask_history();
        if !matches!(self.mask.sync(ctx, &self.gpu), mask::MaskSync::Unchanged) {
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_mask(self.mask.gpu_image().map(|image| self.gpu.copy(image)));
            }
            self.dirty = true;
            self.force = true;
        }
    }
}
impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(json) = serde_json::to_string(&self.params) {
            storage.set_string("moya.params", json);
        }
        if let Some(folder) = &self.last_folder {
            storage.set_string("moya.last_folder", folder.to_string_lossy().into_owned());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
                self.status = match result {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => format!("Save failed: {e:#}"),
                };
            }
        }
        let before = self.params.clone();
        let old_hover = self.hover_params.clone();
        let old_view = (self.xray, self.solo);
        let ctx = ui.ctx().clone();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
            self.queue_send();
        }
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
                self.open_dialog();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                self.save();
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
                if ui.button("Open…").clicked() {
                    self.open_dialog();
                }
                if ui
                    .add_enabled(self.renderer.is_some() && self.saving.is_none(), egui::Button::new("Save HQ…"))
                    .clicked()
                {
                    self.save();
                }
                if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                    .on_hover_text("Undo edit (Cmd/Ctrl+Z)").clicked() {
                    self.restore_history(false);
                }
                if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                    .on_hover_text("Redo edit (Cmd/Ctrl+Shift+Z)").clicked() {
                    self.restore_history(true);
                }
                ui.checkbox(&mut self.glow_only, "Glow only (unmult)")
                    .on_hover_text("Export transparent glow instead of the source composite");
                egui::ComboBox::from_id_salt("save depth")
                    .selected_text(["8 bit", "16 bit", "32 bit"][self.export_depth])
                    .show_ui(ui, |ui| {
                        for (i, label) in ["8 bit", "16 bit", "32 bit"].iter().enumerate() {
                            ui.selectable_value(&mut self.export_depth, i, *label);
                        }
                    });
                if ui.button("Reset").clicked() {
                    self.status.clear();
                    self.hover_params = None;
                    self.params = Params::default();
                    self.xray = false;
                    self.solo = false;
                    self.auto_update = true;
                    self.mask.enabled = false;
                    self.mask.tool = mask::Tool::None;
                }
            });
            if let Some(session) = &self.session {
                let name = session.document.name.clone();
                let document_id = session.document.id;
                ui.horizontal(|ui| {
                    ui.label(format!("Photoshop: {name}"))
                        .on_hover_text(format!("Photoshop document {document_id}"));
                    if ui.add_enabled(self.session_ready && self.sending.is_none() && !self.send_pending,
                        egui::Button::new("Send to Photoshop").fill(ui.visuals().selection.bg_fill))
                        .on_hover_text("Send HQ result (Cmd+Enter)").clicked() {
                        self.queue_send();
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel_session();
                        self.status = "Photoshop session cancelled".into();
                    }
                    if self.send_pending || self.sending.is_some() { ui.spinner(); }
                });
            }
        });
        egui::Panel::bottom("helpers").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.auto_update, "Auto-update")
                    .on_hover_text("Render draft preview whenever parameters change");
                ui.checkbox(&mut self.xray, "X-Ray")
                    .on_hover_text("Inspect thresholded light input");
                ui.checkbox(&mut self.solo, "Solo")
                    .on_hover_text("Show only the glow layer");
                ui.checkbox(&mut self.params.unmult, "Unmult")
                    .on_hover_text("Unpremultiply glow over a transparent background");
                ui.checkbox(&mut self.params.hdr32, "32 Bits")
                    .on_hover_text("Keep unclamped floating-point glow processing");
                let mut tonemap = self.params.tonemap;
                if ui
                    .checkbox(&mut tonemap, "Tonemap")
                    .on_hover_text("Compress HDR glow using exponential tonemapping")
                    .changed()
                {
                    self.params.set_tonemap(tonemap);
                }
                if ui.button("Update").clicked() {
                    self.force = true;
                    self.dirty = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
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
            });
        });
        egui::Panel::left("controls")
            .default_size(350.0)
            .size_range(310.0..=500.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        self.params.sanitize();
        if old_hover != self.hover_params {
            self.dirty = true;
            self.force = true;
        }
        if self.params != before {
            self.dirty = true;
            self.status.clear();
        }
        if old_view != (self.xray, self.solo) {
            self.dirty = true;
            self.force = true;
        }
        egui::CentralPanel::default().show(ui, |ui| self.canvas_ui(ui));
        self.history.observe(&self.params, ctx.input(|i| i.pointer.any_down()));
        if !self.history.can_redo() { self.mask.clear_redo(); }
        self.sync_mask(&ctx);
        if self.dirty && (self.auto_update || self.force) {
            self.render();
            ctx.request_repaint();
        }
        if self.dirty && self.auto_update {
            ui.ctx().request_repaint();
        }
    }
}

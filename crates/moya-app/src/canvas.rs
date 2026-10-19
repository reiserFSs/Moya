use crate::{App, Preview, mask::{BrushSettings, MaskView, Tool}};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use moya_gpu::wgpu;
use moya_gpu::{Depth, Filter, Image, ViewMap};
use moya_pipeline::{LayerInputs, RefineState, RegionQuality, ViewRegion};
use std::time::{Duration, Instant};

struct Viewport {
    result: Preview,
    source: Option<Preview>,
    quality: RegionQuality,
}

pub(super) struct CanvasState {
    pub(super) zoom: f32,
    pan: Vec2,
    fit: bool,
    split: bool,
    divider: f32,
    source: Option<Preview>,
    source_dirty: bool,
    stroke_active: bool,
    stroke_layer: Option<u64>,
    viewport: Option<Viewport>,
    viewport_key: Option<[u32; 13]>,
    last_interaction: Instant,
    refine: RefineState,
    magnified: bool,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            split: false,
            divider: 0.5,
            source: None,
            source_dirty: true,
            stroke_active: false,
            stroke_layer: None,
            viewport: None,
            viewport_key: None,
            last_interaction: Instant::now(),
            refine: RefineState::Idle,
            magnified: false,
        }
    }
}

impl CanvasState {
    pub(super) fn reset_source(&mut self) {
        self.fit = true;
        self.pan = Vec2::ZERO;
        self.source_dirty = true;
        self.stroke_active = false;
        self.stroke_layer = None;
        self.invalidate();
    }

    pub(super) fn invalidate(&mut self) {
        self.viewport_key = None;
        self.last_interaction = Instant::now();
    }

    fn zoom_at(&mut self, factor: f32, cursor: Pos2, center: Pos2) {
        let old = self.zoom;
        self.zoom = (old * factor).clamp(0.01, 32.0);
        let offset = cursor - center;
        self.pan = (self.pan - offset) * (self.zoom / old) + offset;
        self.fit = false;
    }
}

impl App {
    fn viewport_texture(&self, image: &Image, label: &str, old: Option<Preview>) -> Preview {
        let size = [image.width(), image.height()];
        if let Some(old) = old {
            if old.size == size {
                self.gpu.write_display_rgba8(image, &old.texture);
                return old;
            }
            self.render_state.renderer.write().free_texture(&old.id);
        }
        let texture = self.gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let id = self.render_state.renderer.write().register_native_texture(
            self.gpu.device(), &view, wgpu::FilterMode::Nearest,
        );
        self.gpu.write_display_rgba8(image, &texture);
        Preview { texture, id, size }
    }

    fn clear_viewport(&mut self) {
        if let Some(viewport) = self.canvas.viewport.take() {
            let mut renderer = self.render_state.renderer.write();
            renderer.free_texture(&viewport.result.id);
            if let Some(source) = viewport.source { renderer.free_texture(&source.id); }
        }
        self.canvas.viewport_key = None;
    }

    /// Only the visible original-pixel bounds are composited; the full source stays on the GPU.
    fn canvas_viewport(&mut self, ui: &egui::Ui, rect: Rect, area: Rect, show_source: bool) -> Option<Rect> {
        let source = self.source_preview.as_ref()?;
        let pic = self.picture.as_ref()?;
        let width = pic.width;
        let height = pic.height;
        let host_depth = pic.depth;
        let magnified = self.canvas.zoom > source.width() as f32 / width as f32;
        if !magnified {
            self.refiner.cancel();
            if self.canvas.magnified {
                self.clear_viewport();
                if let Some(document) = self.renderer.as_mut() { document.drop_refine(); }
                self.trim_idle();
            }
            self.canvas.magnified = false;
            self.canvas.refine = RefineState::Idle;
            return None;
        }
        if !self.canvas.magnified {
            self.canvas.last_interaction = Instant::now();
        }
        self.canvas.magnified = true;
        let clipped = rect.intersect(area);
        if !clipped.is_positive() { return None; }
        let region = visible_region(rect, clipped, width, height);
        let native = self.canvas.zoom >= 1.0;
        let mut key = [region.x, region.y, region.w, region.h, 0, 0,
            self.canvas.zoom.to_bits(), self.export_depth as u32, self.glow_only as u32, 0,
            rect.min.x.to_bits(), rect.min.y.to_bits(), show_source as u32];
        if self.canvas.viewport_key.is_some_and(|previous| previous[..4] != key[..4]
            || previous[6..9] != key[6..9] || previous[10..] != key[10..])
            || self.dirty || ui.input(|i| i.pointer.any_down()) {
            self.canvas.last_interaction = Instant::now();
        }
        let flags = self.flags();
        let mut request = crate::preview_request(flags, self.selected, self.solo_layer);
        if self.glow_only {
            request.want_composite = false;
            request.want_glow_composite = true;
        }
        let hover = self.hover_params.as_ref().map(|params| {
            let mut settings = self.layers[self.selected_index()].settings.clone();
            settings.params = params.clone();
            settings
        });
        let inputs: Vec<_> = self.layers.iter().map(|layer| LayerInputs {
            settings: hover.as_ref().filter(|settings| settings.id == layer.settings.id).unwrap_or(&layer.settings),
            mask: layer.mask.gpu_image(), mask_generation: layer.mask.generation(),
        }).collect();
        let document = self.renderer.as_mut()?;
        if let Some(glow) = self.refiner.poll() {
            document.accept_refine(glow, &inputs, &flags);
        }
        if self.refiner.ticket().is_some_and(|ticket| !document.refine_current(ticket, &inputs, &flags)) {
            self.refiner.cancel();
        }
        self.canvas.refine = document.refine_state(&inputs, &flags);
        let elapsed = self.canvas.last_interaction.elapsed();
        if !self.dirty && !self.busy && self.hover_params.is_none() && elapsed >= Duration::from_millis(500)
            && !matches!(self.canvas.refine, RefineState::Ready) {
            if !self.refiner.busy() && !self.refiner.stopped() {
                if let Some(job) = document.refine_job(&inputs, &flags) { self.refiner.start(job); }
            }
        } else if elapsed < Duration::from_millis(500) {
            ui.ctx().request_repaint_after(Duration::from_millis(500) - elapsed);
        }
        key[9] = matches!(self.canvas.refine, RefineState::Ready) as u32;
        let exact = native && key[9] != 0 && !flags.xray;
        let out_w = if exact { region.w } else { (region.w as f32 * self.canvas.zoom).ceil() as u32 }.max(1);
        let out_h = if exact { region.h } else { (region.h as f32 * self.canvas.zoom).ceil() as u32 }.max(1);
        key[4] = out_w;
        key[5] = out_h;
        if self.canvas.viewport_key != Some(key) {
            let depth = [Depth::Eight, Depth::Sixteen, Depth::Float][self.export_depth];
            let (image, quality) = document.render_region(&inputs, &request, region, out_w, out_h, depth);
            let display = if matches!(quality, RegionQuality::Exact) && depth != Depth::Eight {
                self.gpu.convert_depth(&image, depth, Depth::Eight)
            } else { image };
            let original = show_source.then(|| {
                let original = self.gpu.sample_view(document.source(), ViewMap {
                    origin_x: region.x as f64, origin_y: region.y as f64,
                    src_per_dst: region.w as f64 / out_w as f64,
                }, out_w, out_h, if exact { Filter::Nearest } else { Filter::Bilinear });
                let from = match host_depth {
                    crate::SampleDepth::F32 => Depth::Float,
                    crate::SampleDepth::U16 => Depth::Sixteen,
                    _ => Depth::Eight,
                };
                if from != Depth::Eight {
                    self.gpu.convert_depth(&original, from, Depth::Eight)
                } else { original }
            });
            let (old_result, old_source) = self.canvas.viewport.take()
                .map(|viewport| (Some(viewport.result), viewport.source)).unwrap_or_default();
            let result = self.viewport_texture(&display, "Moya viewport result", old_result);
            let source = match original {
                Some(image) => Some(self.viewport_texture(&image, "Moya viewport original", old_source)),
                None => old_source,
            };
            self.gpu.flush();
            self.canvas.viewport = Some(Viewport {
                result, source, quality: if exact { quality } else { RegionQuality::Preview },
            });
            self.canvas.viewport_key = Some(key);
        }
        Some(Rect::from_min_size(
            rect.min + egui::vec2(region.x as f32 / width as f32 * rect.width(),
                region.y as f32 / height as f32 * rect.height()),
            egui::vec2(region.w as f32 / width as f32 * rect.width(),
                region.h as f32 / height as f32 * rect.height()),
        ))
    }

    fn canvas_source(&mut self) {
        if !self.canvas.source_dirty {
            return;
        }
        let Some(image) = self.source_preview.as_ref() else { return };
        let size = [image.width(), image.height()];
        if self.canvas.source.as_ref().is_none_or(|p| p.size != size) {
            if let Some(old) = self.canvas.source.take() {
                self.render_state.renderer.write().free_texture(&old.id);
            }
            let texture = self.gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("Moya original preview"),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let id = self.render_state.renderer.write().register_native_texture(
                self.gpu.device(), &view, wgpu::FilterMode::Linear,
            );
            self.canvas.source = Some(Preview { texture, id, size });
        }
        self.gpu.write_display_rgba8(image, &self.canvas.source.as_ref().unwrap().texture);
        self.gpu.flush();
        self.canvas.source_dirty = false;
    }

    pub(super) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let index = self.selected_index();
        if self.canvas.stroke_layer.is_some_and(|id| id != self.selected) {
            if let Some(layer) = self.layers.iter_mut().find(|layer| Some(layer.settings.id) == self.canvas.stroke_layer) {
                layer.mask.end_stroke();
            }
            self.canvas.stroke_active = false;
            self.canvas.stroke_layer = None;
        }
        let (area, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(area);
        painter.rect_filled(area, 0.0, Color32::from_gray(22));
        if self.preview.is_none() {
            let hovered = ui.input(|i| !i.raw.hovered_files.is_empty());
            let zone = area.shrink(36.0);
            painter.rect_filled(zone, 12.0, Color32::from_gray(if hovered { 42 } else { 26 }));
            painter.rect_stroke(zone, 12.0, egui::Stroke::new(1.5,
                if hovered { Color32::from_rgb(105, 220, 140) } else { Color32::from_gray(65) }),
                egui::StrokeKind::Inside);
            let logo = Rect::from_center_size(area.center() - egui::vec2(0.0, 66.0), egui::vec2(96.0, 96.0));
            painter.image(self.logo.id(), logo,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::from_white_alpha(45));
            painter.text(area.center() + egui::vec2(0.0, 16.0), egui::Align2::CENTER_CENTER,
                if cfg!(target_os = "macos") { "Drop an image or press Cmd+O" }
                else { "Drop an image or press Ctrl+O" },
                egui::FontId::proportional(22.0), Color32::LIGHT_GRAY);
            return;
        }
        self.canvas_source();
        let pic = self.picture.as_ref().unwrap();
        let pixels = Vec2::new(pic.width as f32, pic.height as f32);
        let native = pixels / ui.ctx().pixels_per_point();
        let fit = (area.width() / native.x).min(area.height() / native.y);
        let keyboard = !ui.ctx().egui_wants_keyboard_input();
        if keyboard {
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) { self.layers[index].mask.tool = Tool::None; }
            if ui.input(|i| !i.modifiers.command && !i.modifiers.ctrl) {
                if ui.input(|i| i.key_pressed(egui::Key::X)) && self.layers[index].mask.tool != Tool::None {
                    self.layers[index].mask.tool = if self.layers[index].mask.tool == Tool::Brush { Tool::Eraser } else { Tool::Brush };
                }
                if self.layers[index].mask.tool != Tool::None {
                    for (key, shifted, increase) in [
                        (egui::Key::OpenBracket, egui::Key::OpenCurlyBracket, false),
                        (egui::Key::CloseBracket, egui::Key::CloseCurlyBracket, true),
                    ] {
                        if ui.input(|i| i.key_pressed(key) || i.key_pressed(shifted)) {
                            adjust_brush(&mut self.layers[index].mask.brush, increase, ui.input(|i| i.modifiers.shift));
                        }
                    }
                }
            }
        }
        let painting = self.layers[index].mask.tool != Tool::None;
        let original = keyboard && ui.input(|i| i.key_down(egui::Key::Space));
        if keyboard && ui.input(|i| i.key_pressed(egui::Key::F))
            || response.double_clicked() && !painting && !self.set_center
        {
            self.canvas.fit = true;
            self.canvas.pan = Vec2::ZERO;
        }
        if keyboard && ui.input(|i| i.key_pressed(egui::Key::Num1)) {
            self.canvas.fit = false;
            self.canvas.zoom = 1.0;
            self.canvas.pan = Vec2::ZERO;
        }
        if self.canvas.fit { self.canvas.zoom = fit; }
        if response.hovered() {
            let (scroll, zoom, source, modifiers, cursor) = ui.input(|i|
                (i.smooth_scroll_delta, i.zoom_delta(), i.scroll_source(), i.modifiers, i.pointer.hover_pos()));
            if let Some(cursor) = cursor {
                let trackpad = source == Some(egui::MouseWheelSource::Trackpad);
                let factor = if zoom != 1.0 { zoom }
                    else if !trackpad { (scroll.y * 0.002).exp() } else { 1.0 };
                if factor != 1.0 { self.canvas.zoom_at(factor, cursor, area.center()); }
                if trackpad && !modifiers.command && !modifiers.ctrl {
                    self.canvas.pan += scroll;
                }
            }
        }
        let mut rect = Rect::from_center_size(area.center() + self.canvas.pan, native * self.canvas.zoom);
        let divider_x = rect.left() + rect.width() * self.canvas.divider;
        let divider_rect = Rect::from_min_max(
            Pos2::new(divider_x - 7.0, rect.top().max(area.top())),
            Pos2::new(divider_x + 7.0, rect.bottom().min(area.bottom())),
        ).intersect(area);
        let divider = (self.canvas.split && !painting).then(|| ui.interact(divider_rect,
            ui.scope_id().with("split divider"), egui::Sense::drag()));
        if let Some(handle) = &divider {
            handle.clone().on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if handle.dragged() {
                if let Some(pos) = handle.interact_pointer_pos() {
                    self.canvas.divider = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                }
            }
        }
        let center = rect.min + rect.size() * Vec2::from(self.layers[index].settings.params.rays_center);
        let rays = (!painting && (self.layers[index].settings.params.rays_on || self.set_center)).then(|| ui.interact(
            Rect::from_center_size(center, Vec2::splat(26.0)).intersect(area),
            ui.scope_id().with("rays center"), egui::Sense::drag()));
        if let Some(handle) = &rays {
            handle.clone().on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag the rays center");
            if handle.dragged() {
                if let Some(pos) = handle.interact_pointer_pos() {
                    self.layers[index].settings.params.rays_center = normalized_position(rect, pos);
                    self.dirty = true;
                }
            }
        }
        let dragging_handle = divider.as_ref().is_some_and(|r| r.dragged() || r.is_pointer_button_down_on())
            || rays.as_ref().is_some_and(|r| r.dragged() || r.is_pointer_button_down_on());
        if !dragging_handle && (response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Primary) && !painting && !self.set_center)
        {
            self.canvas.pan += response.drag_delta();
            rect = Rect::from_center_size(area.center() + self.canvas.pan, native * self.canvas.zoom);
        }
        let clipped = rect.intersect(area);
        let cell = 16.0;
        for y in 0..(clipped.height() / cell).ceil() as usize {
            for x in 0..(clipped.width() / cell).ceil() as usize {
                painter.rect_filled(Rect::from_min_size(clipped.min + Vec2::new(x as f32 * cell, y as f32 * cell),
                    Vec2::splat(cell)).intersect(clipped), 0.0,
                    Color32::from_gray(if (x + y) % 2 == 0 { 50 } else { 65 }));
            }
        }
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        let viewport_rect = self.canvas_viewport(ui, rect, area, original || self.canvas.split);
        let (result_id, source_id, image_rect) = if let Some((viewport, draw_rect)) =
            self.canvas.viewport.as_ref().zip(viewport_rect) {
            (viewport.result.id, viewport.source.as_ref().map_or(viewport.result.id, |source| source.id), draw_rect)
        } else {
            (self.preview.as_ref().unwrap().id,
                self.canvas.source.as_ref().map(|p| p.id).unwrap_or(self.preview.as_ref().unwrap().id), rect)
        };
        painter.image(if original { source_id } else { result_id }, image_rect, uv, Color32::WHITE);
        if self.canvas.split && !original {
            let x = rect.left() + rect.width() * self.canvas.divider;
            painter.with_clip_rect(area.intersect(Rect::from_min_max(area.min, Pos2::new(x, area.bottom()))))
                .image(source_id, image_rect, uv, Color32::WHITE);
            painter.line_segment([Pos2::new(x, clipped.top()), Pos2::new(x, clipped.bottom())],
                egui::Stroke::new(2.0, Color32::WHITE));
            painter.circle_filled(Pos2::new(x, clipped.center().y), 5.0, Color32::WHITE);
            for (label, pos, align) in [("Original", clipped.left_top() + Vec2::splat(12.0), egui::Align2::LEFT_TOP),
                ("Result", clipped.right_top() + Vec2::new(-12.0, 12.0), egui::Align2::RIGHT_TOP)] {
                painter.text(pos, align, label, egui::FontId::proportional(13.0), Color32::WHITE);
            }
        }
        if (painting || self.panels.mask_tab_open()) && self.layers[index].mask.view != MaskView::Off && !original {
            if let Some(texture) = self.layers[index].mask.overlay_texture() { painter.image(texture.id(), rect, uv, Color32::WHITE); }
        }
        let position = response.interact_pointer_pos().filter(|p| rect.contains(*p));
        if !painting && self.set_center && response.clicked() {
            if let Some(pos) = position {
                self.layers[index].settings.params.rays_center = normalized_position(rect, pos);
                self.set_center = false;
                self.dirty = true;
            }
        }
        if !painting && (self.layers[index].settings.params.rays_on || self.set_center) {
            let p = rect.min + rect.size() * Vec2::from(self.layers[index].settings.params.rays_center);
            let stroke = egui::Stroke::new(1.5, Color32::YELLOW);
            painter.circle_stroke(p, 7.0, stroke);
            for axis in [Vec2::new(12.0, 0.0), Vec2::new(0.0, 12.0)] {
                painter.line_segment([p - axis, p + axis], stroke);
            }
        }
        let primary_down = ui.input(|i| i.pointer.primary_down());
        let erase = self.layers[index].mask.tool == Tool::Eraser || ui.input(|i| i.modifiers.alt);
        if painting && !dragging_handle {
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let start = !self.canvas.stroke_active && response.is_pointer_button_down_on()
                && primary_down && pointer.is_some_and(|p| rect.contains(p));
            if start || self.canvas.stroke_active {
                self.layers[index].mask.begin_stroke(erase);
                self.canvas.stroke_active = true;
                self.canvas.stroke_layer = Some(self.selected);
                if let Some(pos) = pointer {
                    self.layers[index].mask.stroke_to([
                        (pos.x - rect.left()) / rect.width() * pixels.x,
                        (pos.y - rect.top()) / rect.height() * pixels.y,
                    ]);
                }
            } else if !self.canvas.stroke_active && response.clicked() {
                if let Some(pos) = position {
                    self.layers[index].mask.begin_stroke(erase);
                    let uv = normalized_position(rect, pos);
                    self.layers[index].mask.stroke_to([uv[0] * pixels.x, uv[1] * pixels.y]);
                    self.layers[index].mask.end_stroke();
                }
            }
        }
        if self.canvas.stroke_active && (!primary_down || !painting) {
            self.layers[index].mask.end_stroke();
            self.canvas.stroke_active = false;
            self.canvas.stroke_layer = None;
        }
        if painting {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                let radius = self.layers[index].mask.brush.size * rect.width() / pixels.x;
                let color = if erase { Color32::from_rgb(100, 220, 255) } else { Color32::from_rgb(255, 100, 215) };
                painter.circle_stroke(pos, radius + 1.0, egui::Stroke::new(2.0, Color32::from_black_alpha(190)));
                painter.circle_stroke(pos, radius, egui::Stroke::new(1.0, color));
                painter.circle_stroke(pos, radius * self.layers[index].mask.brush.hardness,
                    egui::Stroke::new(1.0, color.gamma_multiply(0.45)));
                painter.circle_filled(pos, 1.5, color);
            }
        }
        let overlay = Rect::from_min_size(area.left_bottom() + Vec2::new(12.0, -44.0), Vec2::new(460.0, 32.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(overlay), |ui| {
            egui::Frame::new().fill(Color32::from_black_alpha(220)).corner_radius(5.0).inner_margin(5.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Fit").on_hover_text("Fit image (F)").clicked() {
                        self.canvas.fit = true;
                        self.canvas.pan = Vec2::ZERO;
                    }
                    if ui.button("100%").on_hover_text("Original pixel size (1)").clicked() {
                        self.canvas.fit = false;
                        self.canvas.zoom = 1.0;
                        self.canvas.pan = Vec2::ZERO;
                    }
                    ui.label(format!("{:.0}%", self.canvas.zoom * 100.0));
                    // (label, tooltip, colour): grey Preview, amber Refining, green Exact.
                    let preview = Color32::from_gray(150);
                    let (label, explanation, color) = if self.canvas.magnified {
                        let refining = match self.canvas.refine {
                            RefineState::Pending { done, total } => Some((done, total)),
                            RefineState::Idle if self.refiner.busy() =>
                                Some((0, self.layers.iter().filter(|layer| layer.settings.visible).count())),
                            _ => None,
                        }.filter(|_| self.canvas.last_interaction.elapsed() >= Duration::from_millis(500));
                        match refining {
                            Some((done, total)) =>
                                (format!("Refining {done}/{total}"), "Computing export-quality glows in the background, one layer at a time. Editing cancels the layer in progress.", Color32::from_rgb(232, 166, 74)),
                            _ if self.canvas.viewport.as_ref().is_some_and(|v| matches!(v.quality, RegionQuality::Exact)) =>
                                ("Exact".into(), "Native image pixels, export-quality glows and the selected export depth: identical to the export. Nearest-neighbour display.", Color32::from_rgb(92, 196, 137)),
                            _ => ("Preview".into(), "Full-resolution source with Draft glows. After a short pause, glows refine to export quality; 100% or above shows exact export pixels.", preview),
                        }
                    } else {
                        ("Preview".into(), "Draft preview at fit scale. Zoom in for full-resolution source detail and automatic glow refinement.", preview)
                    };
                    let text = egui::RichText::new(label).size(11.0).color(color);
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.7))).corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(7, 1))
                        .show(ui, |ui| { ui.label(text); })
                        .response.on_hover_text(explanation);
                    ui.toggle_value(&mut self.canvas.split, "Split").on_hover_text("Compare original and result; hold Space for original");
                });
            });
        });
    }
}

fn visible_region(rect: Rect, clipped: Rect, width: u32, height: u32) -> ViewRegion {
    let x = (((clipped.left() - rect.left()) / rect.width() * width as f32).floor() as u32).min(width - 1);
    let y = (((clipped.top() - rect.top()) / rect.height() * height as f32).floor() as u32).min(height - 1);
    let right = (((clipped.right() - rect.left()) / rect.width() * width as f32).ceil() as u32).clamp(x + 1, width);
    let bottom = (((clipped.bottom() - rect.top()) / rect.height() * height as f32).ceil() as u32).clamp(y + 1, height);
    ViewRegion { x, y, w: right - x, h: bottom - y }
}

fn adjust_brush(brush: &mut BrushSettings, increase: bool, hardness: bool) {
    if hardness {
        brush.hardness = (brush.hardness + if increase { 0.1 } else { -0.1 }).clamp(0.0, 1.0);
    } else {
        brush.size = (brush.size * if increase { 1.25 } else { 0.8 }).clamp(1.0, 500.0);
    }
}

fn normalized_position(rect: Rect, pos: Pos2) -> [f32; 2] {
    [((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
     ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_bounds_cover_partial_pixels_and_clamp_image_edges() {
        let rect = Rect::from_min_size(egui::pos2(-10.5, -20.25), egui::vec2(100.0, 50.0));
        let clipped = rect.intersect(Rect::from_min_max(Pos2::ZERO, egui::pos2(70.0, 60.0)));
        let region = visible_region(rect, clipped, 1000, 500);
        assert_eq!((region.x, region.y, region.w, region.h), (105, 202, 700, 298));
        let full = visible_region(rect, rect, 1000, 500);
        assert_eq!((full.x, full.y, full.w, full.h), (0, 0, 1000, 500));
    }


    #[test]
    fn brush_shortcuts_scale_size_and_clamp_hardness() {
        let mut brush = BrushSettings { size: 40.0, hardness: 0.95, opacity: 0.4 };
        adjust_brush(&mut brush, true, false);
        assert_eq!(brush.size, 50.0);
        adjust_brush(&mut brush, false, false);
        assert_eq!(brush.size, 40.0);
        adjust_brush(&mut brush, true, true);
        assert_eq!(brush.hardness, 1.0);
        brush.hardness = 0.05;
        adjust_brush(&mut brush, false, true);
        assert_eq!(brush.hardness, 0.0);
        brush.size = 1.0;
        adjust_brush(&mut brush, false, false);
        assert_eq!(brush.size, 1.0);
        brush.size = 500.0;
        adjust_brush(&mut brush, true, false);
        assert_eq!(brush.size, 500.0);
        assert_eq!(brush.opacity, 0.4);
    }

    #[test]
    fn zoom_keeps_cursor_pixel_stationary_and_center_clamps() {
        let mut canvas = CanvasState::default();
        canvas.pan = Vec2::new(15.0, -20.0);
        let center = Pos2::new(300.0, 200.0);
        let cursor = Pos2::new(400.0, 350.0);
        let before = (cursor - center - canvas.pan) / canvas.zoom;
        canvas.zoom_at(2.0, cursor, center);
        assert!(((cursor - center - canvas.pan) / canvas.zoom - before).length() < 0.001);
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(100.0, 50.0));
        assert_eq!(normalized_position(rect, Pos2::new(60.0, 45.0)), [0.5, 0.5]);
        assert_eq!(normalized_position(rect, Pos2::new(-1.0, 90.0)), [0.0, 1.0]);
    }
}

use crate::{App, Preview, mask::{BrushSettings, MaskView, Tool}};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use moya_gpu::wgpu;

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
        let source_id = self.canvas.source.as_ref().map(|p| p.id).unwrap_or(self.preview.as_ref().unwrap().id);
        painter.image(if original { source_id } else { self.preview.as_ref().unwrap().id }, rect, uv, Color32::WHITE);
        if self.canvas.split && !original {
            let x = rect.left() + rect.width() * self.canvas.divider;
            painter.with_clip_rect(area.intersect(Rect::from_min_max(area.min, Pos2::new(x, area.bottom()))))
                .image(source_id, rect, uv, Color32::WHITE);
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
        let overlay = Rect::from_min_size(area.left_bottom() + Vec2::new(12.0, -44.0), Vec2::new(300.0, 32.0));
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
                    ui.toggle_value(&mut self.canvas.split, "Split").on_hover_text("Compare original and result; hold Space for original");
                });
            });
        });
    }
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

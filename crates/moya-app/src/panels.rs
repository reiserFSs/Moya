use crate::{App, data, layers::Layer, mask::{Combine, MaskKind, MaskView, Tool}};
use eframe::egui::{self, Color32};
use moya_core::{presets::{self, Preset}, *};

pub(crate) struct PanelState {
    tab: usize,
    presets: Vec<Preset>,
    preset_name: String,
    rename: Option<(usize, String)>,
    delete: Option<usize>,
    layer_rename: Option<(u64, String)>,
    /// Layer id being dragged in the Layers list.
    layer_drag: Option<u64>,
    spectrum_mix: std::collections::HashMap<u64, f32>,
    preset_load_warning: Option<String>,
    preset_backup_pending: bool,
    /// Pending in-app confirmation (rendered as an egui modal; never blocks the event loop).
    confirm: Option<Confirm>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm { DeleteLayer(u64), ClearMask(u64) }

impl PanelState {
    pub(crate) fn new(loaded: anyhow::Result<Vec<Preset>>) -> Self {
        let (presets, preset_load_warning) = match loaded {
            Ok(presets) => (presets, None),
            Err(error) => (Vec::new(), Some(format!(
                "Presets could not be loaded: {error:#}. Unloaded presets are not included in new saves."
            ))),
        };
        let preset_backup_pending = preset_load_warning.is_some();
        Self { tab: 0, presets, preset_name: "My Preset".into(), rename: None, delete: None, layer_rename: None, layer_drag: None, spectrum_mix: Default::default(), preset_load_warning, preset_backup_pending, confirm: None }
    }

    pub(crate) fn mask_tab_open(&self) -> bool { self.tab == 3 }
}

impl App {
    fn delete_layer(&mut self, id: u64) {
        self.history.remove_layer(&mut self.layers, &mut self.selected, id);
        if self.solo_layer == Some(id) { self.solo_layer = None; }
        self.panels.layer_rename = None;
    }

    /// Confirmation dialogs for destructive (but undoable) actions. Enter confirms, Esc cancels.
    pub(crate) fn confirm_dialogs(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.panels.confirm else { return; };
        let (title, text, action) = match confirm {
            Confirm::DeleteLayer(id) => {
                let name = self.layers.iter().find(|l| l.settings.id == id).map(|l| l.settings.name.clone()).unwrap_or_default();
                ("Delete layer?", format!("Delete \"{name}\" and its mask? You can undo this with Cmd+Z."), "Delete")
            }
            Confirm::ClearMask(_) => ("Clear mask?", "Clear the entire mask? You can undo this with Cmd+Z.".to_owned(), "Clear"),
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::unique("moya.confirm")).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(text);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(action).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) { decision = Some(true); }
                if ui.button("Cancel").clicked() { decision = Some(false); }
            });
        });
        if modal.should_close() && decision.is_none() { decision = Some(false); }
        let Some(confirmed) = decision else { return; };
        self.panels.confirm = None;
        if !confirmed { return; }
        match confirm {
            Confirm::DeleteLayer(id) => { if self.layers.iter().any(|l| l.settings.id == id) && self.layers.len() > 1 { self.delete_layer(id); } }
            Confirm::ClearMask(id) => { if let Some(layer) = self.layers.iter_mut().find(|l| l.settings.id == id) { layer.mask.clear(); } }
        }
    }

    /// Narrow strip shown when the Layers panel is collapsed: expand button + one swatch per layer.
    pub(crate) fn layer_strip(&mut self, ui: &mut egui::Ui) -> bool {
        let mut expand = false;
        ui.vertical_centered(|ui| {
            if ui.small_button("‹").on_hover_text("Show the Layers panel").clicked() { expand = true; }
            ui.add_space(4.0);
            for index in (0..self.layers.len()).rev() {
                let id = self.layers[index].settings.id;
                let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                let painter = ui.painter();
                let visible = self.layers[index].settings.visible;
                painter.rect_filled(rect, 4.0, if self.selected == id { ui.visuals().selection.bg_fill } else { ui.visuals().faint_bg_color });
                if !paint_thumb(ui, rect.shrink(3.0), self.thumbs.get(id), visible) {
                    // No glow rendered yet: stack position (top = highest).
                    let color = if visible { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                    painter.text(rect.center(), egui::Align2::CENTER_CENTER, (index + 1).to_string(), egui::FontId::proportional(13.0), color);
                }
                if response.on_hover_text(&self.layers[index].settings.name).clicked() { self.selected = id; }
                ui.add_space(2.0);
            }
        });
        expand
    }

    /// Right-side Layers panel. Returns true when the user asks to collapse it.
    pub(crate) fn layer_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut collapse = false;
        let index = self.selected_index();
        ui.horizontal(|ui| {
            ui.small(egui::RichText::new("LAYERS").color(ui.visuals().weak_text_color()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Frameless icon buttons, as in the mockup's panel header.
                let widgets = &mut ui.visuals_mut().widgets;
                widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                widgets.inactive.bg_stroke = egui::Stroke::NONE;
                ui.spacing_mut().item_spacing.x = 2.0;
                let icon = egui::vec2(22.0, 22.0);
                if ui.add(egui::Button::new("›").min_size(icon)).on_hover_text("Collapse the Layers panel").clicked() { collapse = true; }
                if ui.add_enabled(self.layers.len() > 1, egui::Button::new("×").min_size(icon)).on_hover_text("Delete selected layer; edited layers ask for confirmation").clicked() {
                    let layer = &self.layers[index];
                    let id = layer.settings.id;
                    if !layer.mask.is_empty() || layer.settings.params != Params::default() {
                        // In-app confirmation: a native modal alert would block the event loop.
                        self.panels.confirm = Some(Confirm::DeleteLayer(id));
                    } else {
                        self.delete_layer(id);
                    }
                }
                let duplicate = ui.add(egui::Button::new("").min_size(icon)).on_hover_text("Duplicate this layer, including its mask");
                let square = egui::Rect::from_center_size(duplicate.rect.center(), egui::vec2(9.0, 9.0));
                let stroke = egui::Stroke::new(1.0, ui.visuals().text_color());
                ui.painter().rect_stroke(square.translate(egui::vec2(-2.0, -2.0)), 1.0, stroke, egui::StrokeKind::Inside);
                ui.painter().rect_stroke(square.translate(egui::vec2(2.0, 2.0)), 1.0, stroke, egui::StrokeKind::Inside);
                if duplicate.clicked() {
                    let id = self.history.next_id();
                    let layer = self.layers[index].duplicate(id);
                    self.layers.insert(index + 1, layer);
                    self.selected = id;
                }
                if ui.add(egui::Button::new(egui::RichText::new("+").size(16.0)).min_size(icon)).on_hover_text("Add a new effect layer above the selected layer").clicked() {
                    let id = self.history.next_id();
                    let mut layer = Layer::new(id, format!("Layer {id}"));
                    if let Some(picture) = &self.picture {
                        layer.settings.params.hdr32 = picture.depth == moya_io::SampleDepth::F32;
                        if layer.settings.params.hdr32 { layer.settings.params.gamma = 1.0; }
                        layer.mask.reset(picture.width, picture.height);
                    }
                    self.layers.insert(index + 1, layer);
                    self.selected = id;
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();
        {
            // Reserve the selected layer's blend, opacity and input controls below the list.
            let list_height = (ui.available_height() - 126.0).max(72.0);
            let mut rows: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.layers.len());
            egui::ScrollArea::vertical().id_salt("layer_list").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in (0..self.layers.len()).rev() {
                    let id = self.layers[index].settings.id;
                    ui.push_id(id, |ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), egui::Sense::hover());
                        rows.push((index, rect));
                        if self.selected == id {
                            ui.painter().rect_filled(rect.shrink(1.0), 6.0, crate::theme::ACCENT.gamma_multiply(0.12));
                            ui.painter().rect_stroke(rect.shrink(1.0), 6.0, egui::Stroke::new(1.0, crate::theme::ACCENT), egui::StrokeKind::Inside);
                        }
                        let visible = self.layers[index].settings.visible;
                        let text_color = if visible { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                        let eye_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 28.0, rect.top() + 12.0), egui::vec2(24.0, 24.0));
                        let eye = ui.interact(eye_rect, ui.make_persistent_id("visible"), egui::Sense::click());
                        let color = if visible { text_color } else { ui.visuals().weak_text_color() };
                        let painter = ui.painter();
                        painter.add(egui::Shape::ellipse_stroke(eye_rect.center(), egui::vec2(8.0, 5.0), egui::Stroke::new(1.2, color)));
                        painter.circle_filled(eye_rect.center(), 2.5, color);
                        if !visible { painter.line_segment([eye_rect.left_bottom(), eye_rect.right_top()], egui::Stroke::new(1.2, color)); }
                        if eye.on_hover_text("Show or hide this layer").clicked() { self.layers[index].settings.visible = !visible; }
                        let content_rect = egui::Rect::from_min_max(rect.min, egui::pos2(eye_rect.left() - 3.0, rect.bottom()));
                        let thumb_rect = egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 7.0), egui::vec2(32.0, 32.0));
                        ui.painter().rect_filled(thumb_rect, 4.0, Color32::from_gray(if visible { 40 } else { 30 }));
                        paint_thumb(ui, thumb_rect, self.thumbs.get(id), visible);
                        let name_rect = egui::Rect::from_min_max(rect.min + egui::vec2(48.0, 6.0), egui::pos2(content_rect.right(), rect.top() + 24.0));
                        let hint_rect = egui::Rect::from_min_max(egui::pos2(name_rect.left(), rect.top() + 24.0), egui::pos2(content_rect.right(), rect.bottom() - 6.0));
                        let blend = match self.layers[index].settings.blend {
                            LayerBlend::Auto if self.picture.as_ref().is_some_and(|picture| picture.depth == moya_io::SampleDepth::F32) => "Linear Dodge",
                            LayerBlend::Auto | LayerBlend::Screen => "Screen",
                            LayerBlend::LinearDodge => "Linear Dodge",
                            LayerBlend::Lighten => "Lighten",
                            LayerBlend::Normal => "Normal",
                        };
                        let hint = format!("{blend} · {:.0}%{}", self.layers[index].settings.opacity * 100.0,
                            if !self.layers[index].mask.is_empty() { " · mask" } else { "" });
                        paint_left(ui, hint_rect, &hint, 10.5, ui.visuals().weak_text_color());
                        if let Some((_, name)) = self.panels.layer_rename.as_mut().filter(|(renaming, _)| *renaming == id) {
                            let edit_rect = name_rect.shrink2(egui::vec2(0.0, 2.0));
                            let edit = ui.put(edit_rect, egui::TextEdit::singleline(name).id(ui.make_persistent_id("layer_name")).desired_width(edit_rect.width()));
                            let commit = edit.lost_focus() || ui.input(|input| input.key_pressed(egui::Key::Enter));
                            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                                self.panels.layer_rename = None;
                            } else if commit {
                                if !name.trim().is_empty() { self.layers[index].settings.name = name.trim().into(); }
                                self.panels.layer_rename = None;
                            }
                        } else {
                            let row = ui.interact(content_rect, ui.make_persistent_id("layer_select"), egui::Sense::click_and_drag())
                                .on_hover_text(format!("{}\n{hint}\nSelect layer; double-click to rename", self.layers[index].settings.name));
                            paint_left(ui, name_rect, &self.layers[index].settings.name, 13.0, text_color);
                            if row.clicked() { self.selected = id; }
                            if row.drag_started() { self.panels.layer_drag = Some(id); self.selected = id; }
                            row.context_menu(|ui| {
                                if ui.selectable_label(self.solo_layer == Some(id), "Solo").clicked() {
                                    self.solo_layer = if self.solo_layer == Some(id) { None } else { Some(id) };
                                    ui.close();
                                }
                                if ui.button("Rename").clicked() {
                                    self.panels.layer_rename = Some((id, self.layers[index].settings.name.clone()));
                                    ui.close();
                                }
                                if ui.add_enabled(index + 1 < self.layers.len(), egui::Button::new("Up")).clicked() {
                                    reorder_layer(&mut self.layers, index, index + 1);
                                    ui.close();
                                }
                                if ui.add_enabled(index > 0, egui::Button::new("Down")).clicked() {
                                    reorder_layer(&mut self.layers, index, index - 1);
                                    ui.close();
                                }
                            });
                            if row.double_clicked() {
                                self.selected = id;
                                self.panels.layer_rename = Some((id, self.layers[index].settings.name.clone()));
                                ui.memory_mut(|memory| memory.request_focus(ui.make_persistent_id("layer_name")));
                            }
                        }
                    });
                }
            });
            // Drag-to-reorder: the dragged layer takes the slot of the row under the pointer.
            if let Some(dragged) = self.panels.layer_drag {
                let pointer = ui.input(|input| input.pointer.interact_pos());
                let target = pointer.filter(|p| ui.clip_rect().contains(*p)).and_then(|p| {
                    rows.iter().find(|(_, r)| r.contains(p)).copied()
                });
                if let Some((_, rect)) = target {
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.5, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                }
                if ui.input(|input| input.pointer.any_released() || !input.pointer.any_down()) {
                    if let (Some((to, _)), Some(from)) = (target, self.layers.iter().position(|layer| layer.settings.id == dragged)) {
                        reorder_layer(&mut self.layers, from, to);
                    }
                    self.panels.layer_drag = None;
                }
            }
            let index = self.selected_index();
            choice(ui, "Layer blend", &mut self.layers[index].settings.blend, &[
                (LayerBlend::Auto, "Auto"), (LayerBlend::Screen, "Screen"),
                (LayerBlend::LinearDodge, "Linear Dodge"), (LayerBlend::Lighten, "Lighten"),
                (LayerBlend::Normal, "Normal"),
            ]);
            slider_row(ui, &mut self.layers[index].settings.opacity, &SliderSpec { slot: 200, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Layer glow opacity; does not change its saved effect settings" });
            ui.horizontal(|ui| {
                ui.label("Input");
                crate::theme::segmented(ui, &mut self.layers[index].settings.input, &[
                    (LayerInput::Original, "Original"), (LayerInput::Below, "Below"),
                ]);
            });
            ui.weak("Below extracts light from the composite below this layer.");
        }
        collapse
    }

    pub(crate) fn controls(&mut self, ui: &mut egui::Ui) {
        let index = self.selected_index();
        self.hover_params = None;
        if let Some(warning) = &self.panels.preset_load_warning {
            ui.colored_label(Color32::YELLOW, warning);
            if self.panels.preset_backup_pending {
                ui.weak("Before saving, the existing file will be preserved as userPresets.json.bak-<timestamp>.");
            }
        }
        crate::theme::segmented(ui, &mut self.panels.tab, &[(0, "Main"), (1, "Glare"), (2, "FX"), (3, "Mask"), (4, "Presets")]);
        ui.add_space(2.0);
        match self.panels.tab {
            0 => {
                if crate::theme::section(ui, "Input", None, "") {
                    self.sliders(ui, &[0, 1]);
                    choice(ui, "Transition", &mut self.layers[index].settings.params.transition, &[(Transition::Soft, "Soft"), (Transition::Precise, "Precise")]);
                    let old = self.layers[index].settings.params.highlights;
                    choice(ui, "Highlights", &mut self.layers[index].settings.params.highlights, &[(Highlights::Preserve, "Preserve"), (Highlights::Flatten, "Flatten")]);
                    let highlights = self.layers[index].settings.params.highlights;
                    if old != highlights { self.layers[index].settings.params.set_highlights(highlights); }
                }
                if crate::theme::section(ui, "Glow", None, "") { self.sliders(ui, &[2, 3, 4, 46, 5, 6, 32, 36]); }
                let mut on = self.layers[index].settings.params.colorize;
                let open = crate::theme::section(ui, "Color", None, "");
                if open {
                    if crate::theme::switch_row(ui, "Colorize", &mut on, "Tint the glow with one hue").changed() { self.layers[index].settings.params.set_colorize(on); }
                    if self.layers[index].settings.params.colorize { glow_color_field(ui, &mut self.layers[index].settings.params); }
                    self.sliders(ui, &[11, 12]);
                }
                if crate::theme::section(ui, "Processing", None, "Options that change how the glow is computed") {
                    let params = &mut self.layers[index].settings.params;
                    crate::theme::switch_row(ui, "32-bit processing", &mut params.hdr32, "Keep unclamped floating-point glow processing");
                    let mut tonemap = params.tonemap;
                    if crate::theme::switch_row(ui, "Tonemap", &mut tonemap, "Compress HDR glow using exponential tonemapping").changed() {
                        params.set_tonemap(tonemap);
                    }
                    crate::theme::switch_row(ui, "Unmult", &mut params.unmult, "Unpremultiply the selected layer's glow over transparency");
                }
            }
            1 => {
                if section(ui, "Glare", &mut self.layers[index].settings.params.glare_on, "Add diffraction streaks around bright light") {
                    crate::theme::switch_row(ui, "Protect core", &mut self.layers[index].settings.params.protect_core, "");
                    choice(ui, "Rays", &mut self.layers[index].settings.params.glare_rays, &[(GlareRays::Two, "2"), (GlareRays::Four, "4"), (GlareRays::Six, "6")]);
                    choice(ui, "Blend", &mut self.layers[index].settings.params.glare_blend, &[(GlareBlend::ColorDodge, "Color Dodge"), (GlareBlend::LinearDodge, "Linear Dodge")]);
                    self.sliders(ui, &[7, 8, 9, 10, 31, 45]);
                }
                let mut spectrum = self.layers[index].settings.params.spectrum_mix > 0.0;
                let was_on = spectrum;
                let open = section(ui, "Spectrum", &mut spectrum, "Mix a colored spectrum into glare");
                if was_on != spectrum {
                    if spectrum { self.layers[index].settings.params.spectrum_mix = self.panels.spectrum_mix.get(&self.selected).copied().unwrap_or(50.0); }
                    else {
                        self.panels.spectrum_mix.insert(self.selected, self.layers[index].settings.params.spectrum_mix);
                        self.layers[index].settings.params.spectrum_mix = 0.0;
                    }
                }
                if open {
                    choice(ui, "Blend", &mut self.layers[index].settings.params.spectrum_blend, &[(SpectrumBlend::Overlay, "Overlay"), (SpectrumBlend::SoftLight, "Soft Light"), (SpectrumBlend::Multiply, "Multiply")]);
                    self.sliders(ui, &[13, 14, 15, 16]);
                }
            }
            2 => {
                if section(ui, "Chromatic Aberration", &mut self.layers[index].settings.params.ca_on, "Separate the color channels") {
                    crate::theme::switch_row(ui, "Mix over glow", &mut self.layers[index].settings.params.ca_mix, "");
                    choice(ui, "Type", &mut self.layers[index].settings.params.ca_type, &[(CaType::Offset, "Offset"), (CaType::Center, "Center")]);
                    choice(ui, "Channels", &mut self.layers[index].settings.params.ca_channels, &[(CaChannels::Rgb, "RGB"), (CaChannels::Bgr, "BGR"), (CaChannels::MagentaGreen, "Magenta / Green"), (CaChannels::YellowBlue, "Yellow / Blue"), (CaChannels::RedCyan, "Red / Cyan")]);
                    self.sliders(ui, &[17, 28, 29, 35]);
                }
                if section(ui, "Moya Rays", &mut self.layers[index].settings.params.rays_on, "Add rays radiating from a chosen origin") {
                    ui.toggle_value(&mut self.set_center, "Click preview to set center").on_hover_text("Click a point on the image to place the rays origin");
                    self.sliders(ui, &[38, 39, 40, 41, 42]);
                }
            }
            3 => self.mask_controls(ui),
            _ => self.preset_controls(ui),
        }
    }

    fn sliders(&mut self, ui: &mut egui::Ui, slots: &[usize]) {
        let index = self.selected_index();
        let specs = self.layers[index].settings.params.slider_specs();
        for slot in slots {
            let Some(spec) = specs.iter().find(|spec| spec.slot == *slot) else { continue; };
            slider_row(ui, slider_value(&mut self.layers[index].settings.params, *slot), spec);
        }
    }

    fn mask_controls(&mut self, ui: &mut egui::Ui) {
        let index = self.selected_index();
        if crate::theme::section(ui, "Mask", Some(&mut self.layers[index].mask.enabled),
            "Enable the mask: white pixels mark where light is taken from") {
            ui.horizontal(|ui| {
                let (tile, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                ui.painter().rect_filled(tile, 4.0, Color32::from_gray(40));
                if let Some(texture) = self.layers[index].mask.thumbnail() {
                    let [w, h] = texture.size();
                    let scale = (tile.width() / w as f32).min(tile.height() / h as f32);
                    let rect = egui::Rect::from_center_size(tile.center(), egui::vec2(w as f32 * scale, h as f32 * scale));
                    ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                ui.vertical(|ui| {
                    ui.add_space(5.0);
                    ui.label(if self.layers[index].mask.is_empty() { "Empty".into() }
                        else { format!("{:.1}% coverage", self.layers[index].mask.coverage() * 100.0) })
                        .on_hover_text("Average mask coverage, including partially selected pixels");
                    ui.weak("White takes light, black blocks it");
                });
            });
            if crate::theme::section(ui, "Paint", None, "Left-drag on the image to paint; Alt temporarily erases") {
                let tool = self.layers[index].mask.tool;
                crate::theme::segmented(ui, &mut self.layers[index].mask.tool, &[
                    (Tool::None, "None"), (Tool::Brush, "Brush"), (Tool::Eraser, "Eraser"), (Tool::Gradient, "Gradient"),
                ]).on_hover_text("None navigates (Esc) · Brush paints light (X swaps to Eraser) · Gradient drags from white to black");
                if self.layers[index].mask.tool != tool && self.layers[index].mask.tool != Tool::None {
                    self.layers[index].mask.enabled = true;
                }
                match self.layers[index].mask.tool {
                    Tool::Brush | Tool::Eraser => {
                        slider_row(ui, &mut self.layers[index].mask.brush.size, &SliderSpec { slot: 100, label: "Size", min: 1.0, max: 500.0, default: 30.0, step: 1.0, decimals: 0, tooltip: "Brush radius in image pixels; [ / ] changes size" });
                        slider_row(ui, &mut self.layers[index].mask.brush.hardness, &SliderSpec { slot: 101, label: "Hardness", min: 0.0, max: 1.0, default: 0.75, step: 0.01, decimals: 2, tooltip: "Solid inner radius; Shift+[ / ] changes hardness by 10%" });
                        slider_row(ui, &mut self.layers[index].mask.brush.opacity, &SliderSpec { slot: 102, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Maximum coverage per stroke; overlapping dabs do not build up" });
                    }
                    Tool::Gradient => {
                        labeled(ui, "Shape", |ui| {
                            crate::theme::segmented(ui, &mut self.layers[index].mask.gradient, &[
                                (crate::mask::GradientKind::Linear, "Linear"), (crate::mask::GradientKind::Radial, "Radial"),
                            ]);
                        });
                        ui.weak("Drag on the image; uses the Combine mode below.");
                    }
                    Tool::None => { ui.weak("Pick Brush, Eraser or Gradient to paint on the image."); }
                }
            }
            if crate::theme::section(ui, "View", None, "How the mask is shown on the image; never changes the mask") {
                labeled(ui, "Show", |ui| {
                    crate::theme::segmented(ui, &mut self.layers[index].mask.view, &[
                        (MaskView::Overlay, "Overlay"), (MaskView::MaskOnly, "Mask"), (MaskView::Off, "Off"),
                    ]).on_hover_text("Overlay: magenta over the image · Mask: grayscale mask only · Off: hidden");
                });
                if self.layers[index].mask.view == MaskView::Overlay {
                    slider_row(ui, &mut self.layers[index].mask.overlay_opacity, &SliderSpec { slot: 103, label: "Overlay", min: 0.0, max: 1.0, default: 0.4, step: 0.01, decimals: 2, tooltip: "Magenta preview opacity; does not change the mask" });
                }
            }
            if crate::theme::section(ui, "Build from image", None, "Generate a mask from the source image or load an existing mask") {
                choice(ui, "Combine", &mut self.layers[index].mask.combine, &[
                    (Combine::Replace, "Replace"),
                    (Combine::Add, "Add"),
                    (Combine::Subtract, "Subtract"),
                    (Combine::Intersect, "Intersect"),
                ]);
                slider_row(ui, &mut self.layers[index].mask.lum_range[0], &SliderSpec { slot: 104, label: "Lum. low", min: 0.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, tooltip: "Luminosity at or below this value is excluded; ramps linearly up to the high value" });
                self.layers[index].mask.lum_range[0] = self.layers[index].mask.lum_range[0].min(self.layers[index].mask.lum_range[1]);
                slider_row(ui, &mut self.layers[index].mask.lum_range[1], &SliderSpec { slot: 105, label: "Lum. high", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Luminosity above this value is fully selected" });
                self.layers[index].mask.lum_range[1] = self.layers[index].mask.lum_range[1].max(self.layers[index].mask.lum_range[0]);
                ui.add_enabled_ui(self.picture.is_some(), |ui| {
                    let built = button_grid(ui, &[
                        ("Luminosity", "Build from image luminance using the low/high range"),
                        ("Transparency", "Build from the image alpha channel"),
                        ("Outlines", "Build from image edges"),
                        ("Load image…", "Load an image as a mask, resampled to the current image size"),
                    ], 2);
                    match built {
                        Some(3) => if let Some(path) = rfd::FileDialog::new().pick_file() { self.request_load(&path, true); },
                        Some(choice) => if let Some(renderer) = &self.renderer {
                            let kind = [MaskKind::Luminosity, MaskKind::Transparency, MaskKind::Outlines][choice];
                            self.layers[index].mask.build(&self.gpu, renderer.source(), kind);
                        },
                        None => {}
                    }
                });
            }
            if crate::theme::section(ui, "Refine", None, "Feather, grow, shrink, invert or clear the mask; every operation can be undone") {
                slider_row(ui, &mut self.layers[index].mask.feather, &SliderSpec { slot: 106, label: "Feather px", min: 0.0, max: 100.0, default: 5.0, step: 0.5, decimals: 1, tooltip: "Blur radius in image pixels; Apply commits the refinement" });
                slider_row(ui, &mut self.layers[index].mask.grow, &SliderSpec { slot: 107, label: "Grow/Shrink", min: -100.0, max: 100.0, default: 5.0, step: 1.0, decimals: 0, tooltip: "Image pixels: positive expands the selection, negative contracts it" });
                match button_grid(ui, &[
                    ("Apply feather", "Soften the mask edges using the feather radius"),
                    ("Apply grow", "Expand or contract the mask by the chosen pixel radius"),
                ], 2) {
                    Some(0) => self.layers[index].mask.apply_feather(&self.gpu),
                    Some(_) => self.layers[index].mask.apply_grow(&self.gpu),
                    None => {}
                }
                match button_grid(ui, &[
                    ("Invert", "Swap selected and excluded pixels"),
                    ("Clear", "Clear the entire mask; asks for confirmation when non-empty"),
                    ("Save…", "Save the grayscale mask as a PNG image"),
                ], 3) {
                    Some(0) => self.layers[index].mask.invert(),
                    Some(1) => if self.layers[index].mask.is_empty() { self.layers[index].mask.clear(); }
                        else { self.panels.confirm = Some(Confirm::ClearMask(self.layers[index].settings.id)); },
                    Some(_) => if let Some(path) = rfd::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("mask.png").save_file() {
                        match self.layers[index].mask.save_png(&path) {
                            Ok(()) => self.status = format!("Saved mask: {}", path.display()),
                            Err(e) => self.status = format!("Mask save failed: {e:#}"),
                        }
                    },
                    None => {}
                }
            }
        }
    }

    fn persist_presets(&mut self) {
        if let Err(e) = data::save_presets(&self.panels.presets, &mut self.panels.preset_backup_pending) {
            self.status = format!("Preset save failed: {e:#}");
        }
    }

    fn preset_controls(&mut self, ui: &mut egui::Ui) {
        let index = self.selected_index();
        ui.label("Save current settings");
        ui.add(egui::TextEdit::singleline(&mut self.panels.preset_name).hint_text("Preset name").desired_width(f32::INFINITY));
        if ui.button("Save current preset").clicked() {
            if self.panels.preset_name.trim().is_empty() { self.status = "Preset name cannot be empty".into(); }
            else {
                self.panels.presets.push(Preset { name: self.panels.preset_name.trim().into(), params: self.layers[index].settings.params.clone() });
                self.persist_presets();
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Import…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Moya / Oniric presets", &["json"]).pick_file() {
                    let result = std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|s| Ok(presets::parse_presets(&s)?));
                    match result {
                        Ok(p) => { self.panels.presets.extend(p); self.persist_presets(); }
                        Err(e) => self.status = format!("Preset import failed: {e:#}"),
                    }
                }
            }
            if ui.button("Export…").clicked() {
                if let Some(path) = rfd::FileDialog::new().set_file_name("Moya_Presets.json").save_file() {
                    if let Err(e) = std::fs::write(path, presets::serialize_presets(&self.panels.presets)) { self.status = format!("Preset export failed: {e}"); }
                }
            }
        });
        ui.separator();
        ui.label("Hover to preview · Click to apply");
        ui.weak("Right-click a preset to manage it");
        if self.panels.presets.is_empty() { ui.weak("No presets yet. Save your current settings above."); }
        let mut persist = false;
        let mut duplicate = None;
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt("preset_list").show(ui, |ui| {
            for i in 0..self.panels.presets.len() {
                ui.push_id(i, |ui| {
                    let row = ui.scope(|ui| {
                        if let Some((index, name)) = self.panels.rename.as_mut().filter(|(index, _)| *index == i) {
                            let edit = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                            let save = ui.button("Save name").clicked() || (edit.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
                            if save && !name.trim().is_empty() {
                                self.panels.presets[*index].name = name.trim().into();
                                self.panels.rename = None;
                                persist = true;
                            } else if ui.button("Cancel rename").clicked() { self.panels.rename = None; }
                        } else {
                            let preset = &self.panels.presets[i];
                            let response = ui
                                .with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                    ui.add(egui::Button::selectable(self.layers[index].settings.params == preset.params, &preset.name).min_size(egui::vec2(0.0, 24.0)))
                                })
                                .inner;
                            if response.clicked() { apply_preset(&mut self.layers, index, preset); }
                            response.context_menu(|ui| {
                                if ui.button("Rename").clicked() {
                                    self.panels.rename = Some((i, preset.name.clone()));
                                    ui.close();
                                }
                                if ui.button("Duplicate").clicked() {
                                    duplicate = Some(i);
                                    ui.close();
                                }
                                if ui.button("Delete…").clicked() {
                                    self.panels.delete = Some(i);
                                    ui.close();
                                }
                            });
                        }
                        if self.panels.delete == Some(i) {
                            ui.label("Delete this preset?");
                            ui.horizontal(|ui| {
                                if ui.button("Confirm delete").clicked() { remove = Some(i); }
                                if ui.button("Keep preset").clicked() { self.panels.delete = None; }
                            });
                        }
                    });
                    if row.response.contains_pointer() && self.panels.rename.is_none() && self.panels.delete.is_none() {
                        self.hover_params = Some(self.panels.presets[i].params.clone());
                    }
                });
            }
        });
        if let Some(i) = duplicate {
            let preset = &self.panels.presets[i];
            self.panels.presets.push(Preset { name: format!("{} copy", preset.name), params: preset.params.clone() });
            persist = true;
        }
        if let Some(i) = remove {
            self.panels.presets.remove(i);
            self.panels.delete = None;
            self.panels.rename = None;
            self.hover_params = None;
            persist = true;
        }
        if persist { self.persist_presets(); }
    }
}

/// Photoshop Colorize uses HSL, not HSV: the tint swatch has lightness 50%.
fn tint_rgb(hue: f32, saturation: f32) -> [f32; 3] {
    let h = hue.rem_euclid(360.0) / 60.0;
    let c = saturation.clamp(0.0, 100.0) / 100.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let rgb = match h as u32 {
        0 => [c, x, 0.0], 1 => [x, c, 0.0], 2 => [0.0, c, x],
        3 => [0.0, x, c], 4 => [x, 0.0, c], _ => [c, 0.0, x],
    };
    rgb.map(|v| v + (1.0 - c) * 0.5)
}

fn tint_color(hue: f32, saturation: f32) -> Color32 {
    let [r, g, b] = tint_rgb(hue, saturation).map(|v| (v * 255.0).round() as u8);
    Color32::from_rgb(r, g, b)
}

/// Hue (x) / saturation (y) field shown inline while Colorize is on; kept in sync with the sliders.
fn glow_color_field(ui: &mut egui::Ui, params: &mut Params) {
    let size = egui::vec2(ui.available_width().max(1.0), 96.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let response = response.on_hover_text("Click or drag to pick the tint: hue left to right, saturation bottom to top");
    let mut mesh = egui::Mesh::default();
    for y in 0..=10 {
        for x in 0..=36 {
            let u = x as f32 / 36.0;
            let v = y as f32 / 10.0;
            mesh.colored_vertex(egui::pos2(rect.left() + u * rect.width(), rect.top() + v * rect.height()),
                tint_color(u * 360.0, (1.0 - v) * 100.0));
            if x < 36 && y < 10 {
                let i = y * 37 + x;
                mesh.add_triangle(i, i + 1, i + 37);
                mesh.add_triangle(i + 1, i + 38, i + 37);
            }
        }
    }
    ui.painter().add(mesh);
    if (response.clicked() || response.dragged()) && let Some(pos) = response.interact_pointer_pos() {
        params.hue = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * 360.0;
        params.saturation = (1.0 - (pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0) * 100.0;
    }
    let marker = egui::pos2(
        rect.left() + params.hue.clamp(0.0, 360.0) / 360.0 * rect.width(),
        rect.bottom() - params.saturation.clamp(0.0, 100.0) / 100.0 * rect.height());
    let painter = ui.painter().with_clip_rect(rect);
    painter.circle_stroke(marker, 5.0, egui::Stroke::new(3.0, Color32::BLACK));
    painter.circle_stroke(marker, 5.0, egui::Stroke::new(1.0, Color32::WHITE));
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    #[test]
    fn hsl_tints_handle_primaries_gray_and_wrap() {
        assert_eq!(tint_rgb(0.0, 100.0), [1.0, 0.0, 0.0]);
        assert_eq!(tint_rgb(360.0, 100.0), tint_rgb(0.0, 100.0));
        assert_eq!(tint_rgb(-60.0, 100.0), tint_rgb(300.0, 100.0));
        assert_eq!(tint_rgb(120.0, 0.0), [0.5; 3]);
        assert_eq!(tint_rgb(210.0, 100.0), [0.0, 0.5, 1.0]);
    }
}

fn apply_preset(layers: &mut [Layer], index: usize, preset: &Preset) {
    layers[index].settings.params = preset.params.clone();
}

fn section(ui: &mut egui::Ui, title: &str, on: &mut bool, tooltip: &str) -> bool {
    crate::theme::section(ui, title, Some(on), tooltip) && *on
}

fn short_label(label: &str) -> &str {
    match label {
        "Glare length" => "Length",
        "Glare opacity" => "Opacity",
        "Glare rotation" => "Rotation",
        "Glare detail" => "Detail",
        "Glare sharpness" => "Sharpness",
        "Glare gamma" => "Gamma",
        "Spectrum mix" => "Mix",
        "Spectrum frequency" => "Frequency",
        "Spectrum blur" | "CA blur" => "Blur",
        "Spectrum saturation" => "Saturation",
        "CA distance" => "Distance",
        "CA direction" => "Direction",
        "CA intensity" => "Intensity",
        "Rays center X" => "Center X",
        "Rays center Y" => "Center Y",
        "Rays spacing" => "Spacing",
        "Rays samples" => "Samples",
        "Rays falloff" => "Falloff",
        "Glow iterations" => "Iterations",
        _ => label,
    }
}

fn slider_row(ui: &mut egui::Ui, value: &mut f32, spec: &SliderSpec) {
    let precision = 10.0_f64.powi(i32::from(spec.decimals));
    let step = (f64::from(spec.step) * precision).round() / precision;
    ui.push_id(spec.slot, |ui| {
        ui.scope(|ui| {
            ui.visuals_mut().selection.bg_fill = crate::theme::ACCENT;
            let changed = (*value - spec.default).abs() > 0.00001;
            let response = ui.horizontal(|ui| {
                let label = short_label(spec.label);
                ui.allocate_ui_with_layout(egui::vec2(94.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(94.0);
                    ui.label(label);
                });
                ui.spacing_mut().item_spacing.x = 4.0;
                let width = (ui.available_width() - 46.0 - ui.spacing().item_spacing.x * 2.0).max(24.0);
                ui.scope(|ui| {
                    ui.spacing_mut().slider_width = width;
                    // egui's slider handles input only; the rail, fill and knob are painted below.
                    let rail = ui.visuals().widgets.inactive.bg_fill;
                    let visuals = ui.visuals_mut();
                    visuals.selection.bg_fill = Color32::TRANSPARENT;
                    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                        widget.bg_fill = Color32::TRANSPARENT;
                        widget.bg_stroke = egui::Stroke::NONE;
                        widget.fg_stroke = egui::Stroke::NONE;
                    }
                    // Preserve saved values until an edit; egui otherwise snaps them on every draw.
                    let track = ui.add(egui::Slider::new(value, spec.min..=spec.max).show_value(false)
                        .clamping(egui::SliderClamping::Edits).step_by(step));
                    // Same knob position mapping as egui (range inset by its handle radius).
                    let inset = track.rect.height() / 2.5;
                    let t = ((*value - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0);
                    let x = egui::lerp(track.rect.left() + inset..=track.rect.right() - inset, t);
                    let y = track.rect.center().y;
                    let painter = ui.painter();
                    painter.line_segment([egui::pos2(track.rect.left() + 2.0, y), egui::pos2(track.rect.right() - 2.0, y)], egui::Stroke::new(4.0, rail));
                    painter.line_segment([egui::pos2(track.rect.left() + 2.0, y), egui::pos2(x, y)], egui::Stroke::new(4.0, crate::theme::ACCENT));
                    let knob = if track.hovered() || track.dragged() { Color32::WHITE } else { Color32::from_gray(225) };
                    painter.circle_filled(egui::pos2(x, y), 6.0, knob);
                    if track.secondary_clicked() || track.clicked_by(egui::PointerButton::Middle) { *value = spec.default; }
                });
                // Frameless value text; a frame appears on hover to show it is editable.
                let widgets = &mut ui.visuals_mut().widgets;
                widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                widgets.inactive.bg_stroke = egui::Stroke::NONE;
                if changed { ui.visuals_mut().override_text_color = Some(crate::theme::ACCENT); }
                let number = ui.add_sized([46.0, 22.0], egui::DragValue::new(value).range(spec.min..=spec.max).clamp_existing_to_range(false).speed(step).fixed_decimals(spec.decimals as usize).update_while_editing(false));
                if number.secondary_clicked() || number.clicked_by(egui::PointerButton::Middle) { *value = spec.default; }
                number.on_hover_text("Click to type an exact value. Right/middle-click to reset.");
            }).response;
            if response.secondary_clicked() || response.clicked_by(egui::PointerButton::Middle) { *value = spec.default; }
            response.on_hover_ui(|ui| {
                ui.label(spec.tooltip);
                ui.label(format!("Right/middle-click to reset to {}", spec.default));
            });
        });
    });
}

fn choice<T: Copy + PartialEq>(ui: &mut egui::Ui, label: &str, value: &mut T, options: &[(T, &str)]) {
    let selected = options.iter().find(|(v, _)| v == value).map_or("", |(_, s)| *s);
    ui.horizontal(|ui| {
        ui.label(label);
        // Fit the dropdown into the space left after the label so narrow panels never overlap it.
        let width = (ui.available_width() - ui.spacing().item_spacing.x).clamp(60.0, 145.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(label).selected_text(selected).width(width).show_ui(ui, |ui| {
                for &(v, name) in options { ui.selectable_value(value, v, name); }
            });
        });
    });
}

fn slider_value(p: &mut Params, slot: usize) -> &mut f32 {
    match slot {
        0 => &mut p.threshold, 1 => &mut p.dr_compression, 2 => &mut p.main_opacity,
        3 => &mut p.core_opacity, 4 => &mut p.radius, 5 => &mut p.exposure,
        6 => &mut p.gamma, 7 => &mut p.glare_length, 8 => &mut p.glare_opacity,
        9 => &mut p.glare_rotation, 10 => &mut p.glare_detail, 11 => &mut p.hue,
        12 => &mut p.saturation, 13 => &mut p.spectrum_mix, 14 => &mut p.spectrum_frequency,
        15 => &mut p.spectrum_blur, 16 => &mut p.spectrum_saturation, 17 => &mut p.ca_distance,
        28 => &mut p.ca_blur, 29 => &mut p.ca_direction, 31 => &mut p.glare_sharpness,
        32 => &mut p.aspect_ratio, 35 => &mut p.ca_intensity, 36 => &mut p.aspect_rotation,
        38 => &mut p.rays_center[0], 39 => &mut p.rays_center[1], 40 => &mut p.rays_spacing,
        41 => &mut p.rays_samples, 42 => &mut p.rays_falloff, 45 => &mut p.glare_gamma,
        46 => &mut p.glow_iterations, _ => unreachable!("non-slider slot"),
    }
}

/// `to` is the destination row's final stack index, not an insertion gap.
fn reorder_layer(layers: &mut Vec<Layer>, from: usize, to: usize) {
    if from != to {
        let layer = layers.remove(from);
        layers.insert(to, layer);
    }
}

/// A row with the same 94-px label column as slider rows, then `add` (segmented controls).
fn labeled(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(94.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(94.0);
            ui.label(label);
        });
        add(ui);
    });
}

/// Equal-width action buttons, `columns` per row. Returns the index of a clicked button.
fn button_grid(ui: &mut egui::Ui, buttons: &[(&str, &str)], columns: usize) -> Option<usize> {
    let gap = ui.spacing().item_spacing.x;
    let width = (ui.available_width() - gap * (columns - 1) as f32) / columns as f32;
    let mut clicked = None;
    for (row, chunk) in buttons.chunks(columns).enumerate() {
        ui.horizontal(|ui| {
            for (column, (label, tip)) in chunk.iter().enumerate() {
                if ui.add(egui::Button::new(*label).min_size(egui::vec2(width, 24.0))).on_hover_text(*tip).clicked() {
                    clicked = Some(row * columns + column);
                }
            }
        });
    }
    clicked
}

/// Single-line, left-aligned text clipped to `rect` (row labels; `ui.put` would center them).
fn paint_left(ui: &egui::Ui, rect: egui::Rect, text: &str, size: f32, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(size), color);
    let top = rect.center().y - galley.size().y / 2.0;
    ui.painter().with_clip_rect(rect).galley(egui::pos2(rect.left(), top), galley, color);
}

/// Aspect-fits a layer's glow thumbnail into `tile` on black (the glow's own background).
/// Returns false when no thumbnail exists yet.
fn paint_thumb(ui: &egui::Ui, tile: egui::Rect, thumb: Option<(egui::TextureId, [u32; 2])>, visible: bool) -> bool {
    let Some((texture, [w, h])) = thumb else { return false };
    let scale = (tile.width() / w as f32).min(tile.height() / h as f32);
    let rect = egui::Rect::from_center_size(tile.center(), egui::vec2(w as f32 * scale, h as f32 * scale));
    ui.painter().rect_filled(rect, 2.0, Color32::BLACK);
    let tint = if visible { Color32::WHITE } else { Color32::from_gray(80) };
    ui.painter().image(texture, rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layer_drop_uses_destination_row_and_one_undo_step() {
        for (from, to, expected) in [
            (0, 2, [2, 3, 1]), (2, 0, [3, 1, 2]),
            (1, 2, [1, 3, 2]), (1, 0, [2, 1, 3]), (1, 1, [1, 2, 3]),
        ] {
            let mut layers = vec![Layer::new(1, "One"), Layer::new(2, "Two"), Layer::new(3, "Three")];
            let mut selected = layers[from].settings.id;
            let mut history = crate::history::History::new(&layers, selected);
            history.observe(&mut layers, selected, true);
            reorder_layer(&mut layers, from, to);
            history.observe(&mut layers, selected, false);
            assert_eq!(layers.iter().map(|layer| layer.settings.id).collect::<Vec<_>>(), expected);
            assert_eq!(history.undo(&mut layers, &mut selected), from != to);
            assert_eq!(layers.iter().map(|layer| layer.settings.id).collect::<Vec<_>>(), [1, 2, 3]);
            assert!(!history.can_undo());
        }
    }


    #[test]
    fn idle_slider_rows_preserve_saved_values() {
        let ctx = egui::Context::default();
        let mut params = Params::default();
        params.exposure = 1.75;
        let original = params.clone();
        let mut opacity = 0.837;
        let opacity_spec = SliderSpec { slot: 200, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "" };
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                for spec in params.slider_specs() {
                    slider_row(ui, slider_value(&mut params, spec.slot), &spec);
                }
                slider_row(ui, &mut opacity, &opacity_spec);
            });
            assert_eq!(params, original);
            assert_eq!(opacity, 0.837);
        }
    }

    #[test]
    fn preset_changes_only_selected_layer_parameters() {
        let mut layers = vec![Layer::new(1, "First"), Layer::new(2, "Second")];
        let original = layers[0].settings.clone();
        let mut params = Params::default();
        params.radius = 77.0;
        let preset = Preset { name: "Selected only".into(), params };
        apply_preset(&mut layers, 1, &preset);
        assert_eq!(layers[0].settings, original);
        assert_eq!(layers[1].settings.params, preset.params);
        assert_eq!(layers[1].settings.id, 2);
        assert_eq!(layers[1].settings.name, "Second");
    }
    #[test]
    fn every_slider_maps_to_its_parameter() {
        let mut params = Params::default();
        for spec in params.slider_specs() {
            *slider_value(&mut params, spec.slot) = spec.max;
            let slots = params.to_slots();
            assert_eq!(slots[spec.slot] as f32, spec.max);
        }
        assert_eq!(short_label("Glare length"), "Length");
    }
}

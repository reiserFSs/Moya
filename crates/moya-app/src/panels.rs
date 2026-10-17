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
}

impl PanelState {
    pub(crate) fn new(loaded: anyhow::Result<Vec<Preset>>) -> Self {
        let (presets, preset_load_warning) = match loaded {
            Ok(presets) => (presets, None),
            Err(error) => (Vec::new(), Some(format!(
                "Presets could not be loaded: {error:#}. Unloaded presets are not included in new saves."
            ))),
        };
        let preset_backup_pending = preset_load_warning.is_some();
        Self { tab: 0, presets, preset_name: "My Preset".into(), rename: None, delete: None, layer_rename: None, layer_drag: None, spectrum_mix: Default::default(), preset_load_warning, preset_backup_pending }
    }

    pub(crate) fn mask_tab_open(&self) -> bool { self.tab == 3 }
}

impl App {
    /// Narrow strip shown when the Layers panel is collapsed: expand button + one swatch per layer.
    pub(crate) fn layer_strip(&mut self, ui: &mut egui::Ui) -> bool {
        let mut expand = false;
        ui.vertical_centered(|ui| {
            if ui.small_button("<<").on_hover_text("Show the Layers panel").clicked() { expand = true; }
            ui.add_space(4.0);
            for index in (0..self.layers.len()).rev() {
                let id = self.layers[index].settings.id;
                let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
                let painter = ui.painter();
                let visible = self.layers[index].settings.visible;
                painter.rect_filled(rect, 4.0, if self.selected == id { ui.visuals().selection.bg_fill } else { ui.visuals().faint_bg_color });
                let inner = rect.shrink(4.0);
                match self.layers[index].mask.thumbnail().filter(|_| !self.layers[index].mask.is_empty()) {
                    Some(texture) => {
                        let tint = if visible && self.layers[index].mask.enabled { Color32::WHITE } else { Color32::from_gray(90) };
                        painter.image(texture.id(), inner, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
                    }
                    None => {
                        // Stack position (top = highest), since default names all share the same initial.
                        let label = (index + 1).to_string();
                        let color = if visible { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                        painter.text(inner.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(13.0), color);
                    }
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
        ui.horizontal(|ui| {
            ui.strong("Layers");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(">>").on_hover_text("Collapse the Layers panel").clicked() { collapse = true; }
            });
        });
        ui.separator();
        {
            // Leave room for the action rows and the selected layer's blend/opacity/input below the list.
            let list_height = (ui.available_height() - 150.0).max(72.0);
            let mut rows: Vec<(usize, egui::Rect)> = Vec::with_capacity(self.layers.len());
            egui::ScrollArea::vertical().id_salt("layer_list").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in (0..self.layers.len()).rev() {
                    let id = self.layers[index].settings.id;
                    ui.push_id(id, |ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
                        rows.push((index, rect));
                        if self.selected == id {
                            ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                        }
                        let visible = self.layers[index].settings.visible;
                        let text_color = if visible { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                        let eye_rect = egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 2.0), egui::vec2(20.0, 20.0));
                        ui.put(eye_rect, egui::Checkbox::without_text(&mut self.layers[index].settings.visible))
                            .on_hover_text("Show or hide this layer");
                        let solo_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 38.0, rect.top() + 2.0), egui::vec2(36.0, 20.0));
                        let mut solo = self.solo_layer == Some(id);
                        if ui.put(solo_rect, egui::Button::selectable(solo, egui::RichText::new("Solo").size(10.0)))
                            .on_hover_text("Solo this layer; click again to restore the stack").clicked() {
                            solo = !solo;
                            self.solo_layer = solo.then_some(id);
                        }
                        let content_rect = egui::Rect::from_min_max(egui::pos2(eye_rect.right() + 4.0, rect.top()), egui::pos2(solo_rect.left() - 4.0, rect.bottom()));
                        let hint_width = (content_rect.width() * 0.48).min(112.0);
                        let hint_rect = egui::Rect::from_min_max(egui::pos2(content_rect.right() - hint_width, rect.top()), content_rect.max);
                        let mut name_rect = egui::Rect::from_min_max(content_rect.min, egui::pos2(hint_rect.left() - 4.0, rect.bottom()));
                        if !self.layers[index].mask.is_empty() {
                            if let Some(texture) = self.layers[index].mask.thumbnail() {
                                let mask_rect = egui::Rect::from_min_size(name_rect.min + egui::vec2(0.0, 4.0), egui::vec2(16.0, 16.0));
                                let tint = if visible && self.layers[index].mask.enabled { Color32::WHITE } else { Color32::from_gray(90) };
                                ui.painter().image(texture.id(), mask_rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
                                name_rect.min.x += 20.0;
                            }
                        }
                        let blend = match self.layers[index].settings.blend {
                            LayerBlend::Auto if self.picture.as_ref().is_some_and(|picture| picture.depth == moya_io::SampleDepth::F32) => "Linear Dodge",
                            LayerBlend::Auto | LayerBlend::Screen => "Screen",
                            LayerBlend::LinearDodge => "Linear Dodge",
                            LayerBlend::Lighten => "Lighten",
                            LayerBlend::Normal => "Normal",
                        };
                        let hint = format!("{blend} · {:.0}%{}", self.layers[index].settings.opacity * 100.0,
                            if self.layers[index].settings.input == LayerInput::Below { " · Below" } else { "" });
                        ui.put(hint_rect, egui::Label::new(egui::RichText::new(&hint).size(10.0).color(ui.visuals().weak_text_color()))
                            .halign(egui::Align::Max).truncate().selectable(false)).on_hover_text(&hint);
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
                            ui.put(name_rect, egui::Label::new(egui::RichText::new(&self.layers[index].settings.name).color(text_color))
                                .halign(egui::Align::Min).truncate().selectable(false));
                            if row.clicked() { self.selected = id; }
                            if row.drag_started() { self.panels.layer_drag = Some(id); self.selected = id; }
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
                let target = pointer.and_then(|p| {
                    rows.iter().find(|(_, r)| p.y >= r.top() && p.y < r.bottom())
                        .or_else(|| rows.iter().min_by(|a, b| (a.1.center().y - p.y).abs().total_cmp(&(b.1.center().y - p.y).abs())))
                        .copied()
                });
                if let Some((_, rect)) = target {
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.5, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                }
                if ui.input(|input| input.pointer.any_released() || !input.pointer.any_down()) {
                    if let (Some((to, _)), Some(from)) = (target, self.layers.iter().position(|layer| layer.settings.id == dragged)) {
                        if from != to {
                            let layer = self.layers.remove(from);
                            self.layers.insert(to, layer);
                        }
                    }
                    self.panels.layer_drag = None;
                }
            }
            let index = self.selected_index();
            ui.horizontal_wrapped(|ui| {
                if ui.button("Add").on_hover_text("Add a new effect layer above the selected layer").clicked() {
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
                if ui.button("Duplicate").on_hover_text("Duplicate this layer, including its mask").clicked() {
                    let id = self.history.next_id();
                    let layer = self.layers[index].duplicate(id);
                    self.layers.insert(index + 1, layer);
                    self.selected = id;
                }
                if ui.add_enabled(self.layers.len() > 1, egui::Button::new("Delete")).on_hover_text("Delete selected layer; edited layers ask for confirmation").clicked() {
                    let layer = &self.layers[index];
                    let confirm = !layer.mask.is_empty() || layer.settings.params != Params::default();
                    if !confirm || rfd::MessageDialog::new().set_title("Delete layer?")
                        .set_description("Delete this layer and its mask? You can undo this change.")
                        .set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
                    {
                        let id = layer.settings.id;
                        self.history.remove_layer(&mut self.layers, &mut self.selected, id);
                        if self.solo_layer == Some(id) { self.solo_layer = None; }
                        self.panels.layer_rename = None;
                    }
                }
            });
            let index = self.selected_index();
            ui.horizontal(|ui| {
                if ui.add_enabled(index + 1 < self.layers.len(), egui::Button::new("Up")).on_hover_text("Move selected layer up in the stack").clicked() {
                    self.layers.swap(index, index + 1);
                }
                if ui.add_enabled(index > 0, egui::Button::new("Down")).on_hover_text("Move selected layer down in the stack").clicked() {
                    self.layers.swap(index, index - 1);
                }
            });
            let index = self.selected_index();
            choice(ui, "Layer blend", &mut self.layers[index].settings.blend, &[
                (LayerBlend::Auto, "Auto"), (LayerBlend::Screen, "Screen"),
                (LayerBlend::LinearDodge, "Linear Dodge"), (LayerBlend::Lighten, "Lighten"),
                (LayerBlend::Normal, "Normal"),
            ]);
            slider_row(ui, &mut self.layers[index].settings.opacity, &SliderSpec { slot: 200, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Layer glow opacity; does not change its saved effect settings" });
            choice(ui, "Input", &mut self.layers[index].settings.input, &[
                (LayerInput::Original, "Original"), (LayerInput::Below, "Below"),
            ]);
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
        ui.horizontal(|ui| {
            for (i, label) in ["Main", "Glare", "FX", "Mask", "Presets"].iter().enumerate() {
                ui.selectable_value(&mut self.panels.tab, i, *label);
            }
        });
        ui.separator();
        match self.panels.tab {
            0 => {
                egui::CollapsingHeader::new("Input").default_open(true).show(ui, |ui| {
                    self.sliders(ui, &[0, 1]);
                    choice(ui, "Transition", &mut self.layers[index].settings.params.transition, &[(Transition::Soft, "Soft"), (Transition::Precise, "Precise")]);
                    let old = self.layers[index].settings.params.highlights;
                    choice(ui, "Highlights", &mut self.layers[index].settings.params.highlights, &[(Highlights::Preserve, "Preserve"), (Highlights::Flatten, "Flatten")]);
                    let highlights = self.layers[index].settings.params.highlights;
                    if old != highlights { self.layers[index].settings.params.set_highlights(highlights); }
                });
                egui::CollapsingHeader::new("Glow").default_open(true).show(ui, |ui| { self.sliders(ui, &[2, 3, 4, 46, 5, 6, 32, 36]); });
                egui::CollapsingHeader::new("Color").default_open(true).show(ui, |ui| {
                    let mut on = self.layers[index].settings.params.colorize;
                    if ui.checkbox(&mut on, "Colorize").on_hover_text("Tint the glow with one hue").changed() { self.layers[index].settings.params.set_colorize(on); }
                    self.sliders(ui, &[11, 12]);
                });
            }
            1 => {
                if section(ui, "Glare", &mut self.layers[index].settings.params.glare_on, "Add diffraction streaks around bright light") {
                    ui.checkbox(&mut self.layers[index].settings.params.protect_core, "Protect Core");
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
                    ui.checkbox(&mut self.layers[index].settings.params.ca_mix, "Mix over glow");
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
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(), ui.make_persistent_id("mask_section"), true,
        ).show_header(ui, |ui| {
            ui.checkbox(&mut self.layers[index].mask.enabled, egui::RichText::new("Mask").strong())
                .on_hover_text("Enable the mask: white pixels mark where light is taken from");
        }).body(|ui| {
            ui.horizontal(|ui| {
                if let Some(texture) = self.layers[index].mask.thumbnail() {
                    ui.add(egui::Image::new(texture).fit_to_exact_size(egui::vec2(56.0, 56.0)))
                        .on_hover_text("Mask thumbnail: white includes light, black excludes it");
                }
                ui.label(if self.layers[index].mask.is_empty() { "Empty".into() }
                    else { format!("{:.1}% coverage", self.layers[index].mask.coverage() * 100.0) })
                    .on_hover_text("Average mask coverage, including partially selected pixels");
            });
            ui.horizontal(|ui| {
                ui.label("Tools").on_hover_text("Left-drag to paint; Alt temporarily erases");
                for (tool, label, tip) in [
                    (Tool::None, "None", "Navigate without painting (Esc)"),
                    (Tool::Brush, "Brush", "Paint light into the mask; X swaps to Eraser"),
                    (Tool::Eraser, "Eraser", "Remove light from the mask; X swaps to Brush"),
                ] {
                    if ui.selectable_value(&mut self.layers[index].mask.tool, tool, label).on_hover_text(tip).changed()
                        && tool != Tool::None { self.layers[index].mask.enabled = true; }
                }
            });
            slider_row(ui, &mut self.layers[index].mask.brush.size, &SliderSpec { slot: 100, label: "Size", min: 1.0, max: 500.0, default: 30.0, step: 1.0, decimals: 0, tooltip: "Brush radius in image pixels; [ / ] changes size" });
            slider_row(ui, &mut self.layers[index].mask.brush.hardness, &SliderSpec { slot: 101, label: "Hardness", min: 0.0, max: 1.0, default: 0.75, step: 0.01, decimals: 2, tooltip: "Solid inner radius; Shift+[ / ] changes hardness by 10%" });
            slider_row(ui, &mut self.layers[index].mask.brush.opacity, &SliderSpec { slot: 102, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Maximum coverage per stroke; overlapping dabs do not build up" });
            ui.horizontal(|ui| {
                ui.label("View").on_hover_text("Choose how to inspect the mask on the image");
                for (view, label, tip) in [
                    (MaskView::Overlay, "Overlay", "Show the mask as a magenta overlay"),
                    (MaskView::MaskOnly, "Mask", "Show only the grayscale mask"),
                    (MaskView::Off, "Off", "Hide the mask preview"),
                ] { ui.selectable_value(&mut self.layers[index].mask.view, view, label).on_hover_text(tip); }
            });
            slider_row(ui, &mut self.layers[index].mask.overlay_opacity, &SliderSpec { slot: 103, label: "Overlay", min: 0.0, max: 1.0, default: 0.4, step: 0.01, decimals: 2, tooltip: "Magenta preview opacity; does not change the mask" });
            egui::CollapsingHeader::new("Build from image").default_open(true).show(ui, |ui| {
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
                    ui.horizontal_wrapped(|ui| {
                        for (name, kind, tip) in [
                            ("Luminosity", MaskKind::Luminosity, "Build from image luminance using the low/high range"),
                            ("Transparency", MaskKind::Transparency, "Build from the image alpha channel"),
                            ("Outlines", MaskKind::Outlines, "Build from image edges"),
                        ] {
                            if ui.button(name).on_hover_text(tip).clicked() {
                                if let Some(renderer) = &self.renderer {
                                    self.layers[index].mask.build(&self.gpu, renderer.source(), kind);
                                }
                            }
                        }
                        if ui.button("Load image…").on_hover_text("Load an image as a mask, resampled to the current image size").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                self.request_load(&path, true);
                            }
                        }
                    });
                });
            }).header_response.on_hover_text("Generate a mask from the source image or load an existing mask");
            egui::CollapsingHeader::new("Refine").default_open(true).show(ui, |ui| {
                slider_row(ui, &mut self.layers[index].mask.feather, &SliderSpec { slot: 106, label: "Feather px", min: 0.0, max: 100.0, default: 5.0, step: 0.5, decimals: 1, tooltip: "Blur radius in image pixels; Apply commits the refinement" });
                if ui.button("Apply feather").on_hover_text("Soften the mask edges using the feather radius").clicked() { self.layers[index].mask.apply_feather(&self.gpu); }
                slider_row(ui, &mut self.layers[index].mask.grow, &SliderSpec { slot: 107, label: "Grow/Shrink", min: -100.0, max: 100.0, default: 5.0, step: 1.0, decimals: 0, tooltip: "Image pixels: positive expands the selection, negative contracts it" });
                if ui.button("Apply grow/shrink").on_hover_text("Expand or contract the mask by the chosen pixel radius").clicked() { self.layers[index].mask.apply_grow(&self.gpu); }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Invert").on_hover_text("Swap selected and excluded pixels").clicked() { self.layers[index].mask.invert(); }
                    if ui.button("Clear").on_hover_text("Clear the entire mask; asks for confirmation when non-empty").clicked() {
                        if self.layers[index].mask.is_empty() || rfd::MessageDialog::new().set_title("Clear mask?")
                            .set_description("Clear the entire mask? You can undo this change.")
                            .set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
                        { self.layers[index].mask.clear(); }
                    }
                    if ui.button("Save mask…").on_hover_text("Save the grayscale mask as a PNG image").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("mask.png").save_file() {
                            match self.layers[index].mask.save_png(&path) {
                                Ok(()) => self.status = format!("Saved mask: {}", path.display()),
                                Err(e) => self.status = format!("Mask save failed: {e:#}"),
                            }
                        }
                    }
                });
            }).header_response.on_hover_text("Feather, grow, shrink, invert or clear the mask; every operation can be undone");
        });
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

fn apply_preset(layers: &mut [Layer], index: usize, preset: &Preset) {
    layers[index].settings.params = preset.params.clone();
}

fn section(ui: &mut egui::Ui, title: &str, on: &mut bool, tooltip: &str) -> bool {
    ui.separator();
    ui.checkbox(on, egui::RichText::new(title).strong()).on_hover_text(tooltip);
    *on
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
            if (*value - spec.default).abs() > 0.00001 { ui.visuals_mut().override_text_color = Some(Color32::from_rgb(105, 220, 140)); }
            let response = ui.horizontal(|ui| {
                let label = short_label(spec.label);
                ui.allocate_ui_with_layout(egui::vec2(94.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(94.0);
                    ui.label(label);
                });
                let width = (ui.available_width() - 62.0 - ui.spacing().item_spacing.x).max(24.0);
                ui.scope(|ui| {
                    ui.spacing_mut().slider_width = width;
                    // Preserve saved values until an edit; egui otherwise snaps them on every draw.
                    let track = ui.add(egui::Slider::new(value, spec.min..=spec.max).show_value(false)
                        .clamping(egui::SliderClamping::Edits).step_by(step));
                    if track.secondary_clicked() || track.clicked_by(egui::PointerButton::Middle) { *value = spec.default; }
                });
                let number = ui.add_sized([62.0, 20.0], egui::DragValue::new(value).range(spec.min..=spec.max).clamp_existing_to_range(false).speed(step).fixed_decimals(spec.decimals as usize).update_while_editing(false));
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt(label).selected_text(selected).width(145.0).show_ui(ui, |ui| {
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

#[cfg(test)]
mod tests {
    use super::*;

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

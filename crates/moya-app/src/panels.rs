use crate::{App, data, mask::{Combine, MaskKind, MaskView, Tool}};
use eframe::egui::{self, Color32};
use moya_core::{presets::{self, Preset}, *};

pub(crate) struct PanelState {
    tab: usize,
    presets: Vec<Preset>,
    preset_name: String,
    rename: Option<(usize, String)>,
    delete: Option<usize>,
    spectrum_mix: f32,
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
        Self { tab: 0, presets, preset_name: "My Preset".into(), rename: None, delete: None, spectrum_mix: 50.0, preset_load_warning, preset_backup_pending }
    }

    pub(crate) fn mask_tab_open(&self) -> bool { self.tab == 3 }
}

impl App {
    pub(crate) fn controls(&mut self, ui: &mut egui::Ui) {
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
                    choice(ui, "Transition", &mut self.params.transition, &[(Transition::Soft, "Soft"), (Transition::Precise, "Precise")]);
                    let old = self.params.highlights;
                    choice(ui, "Highlights", &mut self.params.highlights, &[(Highlights::Preserve, "Preserve"), (Highlights::Flatten, "Flatten")]);
                    if old != self.params.highlights { self.params.set_highlights(self.params.highlights); }
                });
                egui::CollapsingHeader::new("Glow").default_open(true).show(ui, |ui| { self.sliders(ui, &[2, 3, 4, 46, 5, 6, 32, 36]); });
                egui::CollapsingHeader::new("Color").default_open(true).show(ui, |ui| {
                    let mut on = self.params.colorize;
                    if ui.checkbox(&mut on, "Colorize").on_hover_text("Tint the glow with one hue").changed() { self.params.set_colorize(on); }
                    self.sliders(ui, &[11, 12]);
                });
            }
            1 => {
                if section(ui, "Glare", &mut self.params.glare_on, "Add diffraction streaks around bright light") {
                    ui.checkbox(&mut self.params.protect_core, "Protect Core");
                    choice(ui, "Rays", &mut self.params.glare_rays, &[(GlareRays::Two, "2"), (GlareRays::Four, "4"), (GlareRays::Six, "6")]);
                    choice(ui, "Blend", &mut self.params.glare_blend, &[(GlareBlend::ColorDodge, "Color Dodge"), (GlareBlend::LinearDodge, "Linear Dodge")]);
                    self.sliders(ui, &[7, 8, 9, 10, 31, 45]);
                }
                let mut spectrum = self.params.spectrum_mix > 0.0;
                let was_on = spectrum;
                let open = section(ui, "Spectrum", &mut spectrum, "Mix a colored spectrum into glare");
                if was_on != spectrum {
                    if spectrum { self.params.spectrum_mix = self.panels.spectrum_mix; }
                    else { self.panels.spectrum_mix = self.params.spectrum_mix; self.params.spectrum_mix = 0.0; }
                }
                if open {
                    choice(ui, "Blend", &mut self.params.spectrum_blend, &[(SpectrumBlend::Overlay, "Overlay"), (SpectrumBlend::SoftLight, "Soft Light"), (SpectrumBlend::Multiply, "Multiply")]);
                    self.sliders(ui, &[13, 14, 15, 16]);
                }
            }
            2 => {
                if section(ui, "Chromatic Aberration", &mut self.params.ca_on, "Separate the color channels") {
                    ui.checkbox(&mut self.params.ca_mix, "Mix over glow");
                    choice(ui, "Type", &mut self.params.ca_type, &[(CaType::Offset, "Offset"), (CaType::Center, "Center")]);
                    choice(ui, "Channels", &mut self.params.ca_channels, &[(CaChannels::Rgb, "RGB"), (CaChannels::Bgr, "BGR"), (CaChannels::MagentaGreen, "Magenta / Green"), (CaChannels::YellowBlue, "Yellow / Blue"), (CaChannels::RedCyan, "Red / Cyan")]);
                    self.sliders(ui, &[17, 28, 29, 35]);
                }
                if section(ui, "Moya Rays", &mut self.params.rays_on, "Add rays radiating from a chosen origin") {
                    ui.toggle_value(&mut self.set_center, "Click preview to set center").on_hover_text("Click a point on the image to place the rays origin");
                    self.sliders(ui, &[38, 39, 40, 41, 42]);
                }
            }
            3 => self.mask_controls(ui),
            _ => self.preset_controls(ui),
        }
    }

    fn sliders(&mut self, ui: &mut egui::Ui, slots: &[usize]) {
        let specs = self.params.slider_specs();
        for slot in slots {
            let Some(spec) = specs.iter().find(|spec| spec.slot == *slot) else { continue; };
            slider_row(ui, slider_value(&mut self.params, *slot), spec);
        }
    }

    fn mask_controls(&mut self, ui: &mut egui::Ui) {
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(), ui.make_persistent_id("mask_section"), true,
        ).show_header(ui, |ui| {
            ui.checkbox(&mut self.mask.enabled, egui::RichText::new("Mask").strong())
                .on_hover_text("Enable the mask: white pixels mark where light is taken from");
        }).body(|ui| {
            ui.horizontal(|ui| {
                if let Some(texture) = self.mask.thumbnail() {
                    ui.add(egui::Image::new(texture).fit_to_exact_size(egui::vec2(56.0, 56.0)))
                        .on_hover_text("Mask thumbnail: white includes light, black excludes it");
                }
                ui.label(if self.mask.is_empty() { "Empty".into() }
                    else { format!("{:.1}% coverage", self.mask.coverage() * 100.0) })
                    .on_hover_text("Average mask coverage, including partially selected pixels");
            });
            ui.horizontal(|ui| {
                ui.label("Tools").on_hover_text("Left-drag to paint; Alt temporarily erases");
                for (tool, label, tip) in [
                    (Tool::None, "None", "Navigate without painting (Esc)"),
                    (Tool::Brush, "Brush", "Paint light into the mask; X swaps to Eraser"),
                    (Tool::Eraser, "Eraser", "Remove light from the mask; X swaps to Brush"),
                ] {
                    if ui.selectable_value(&mut self.mask.tool, tool, label).on_hover_text(tip).changed()
                        && tool != Tool::None { self.mask.enabled = true; }
                }
            });
            slider_row(ui, &mut self.mask.brush.size, &SliderSpec { slot: 100, label: "Size", min: 1.0, max: 500.0, default: 30.0, step: 1.0, decimals: 0, tooltip: "Brush radius in image pixels; [ / ] changes size" });
            slider_row(ui, &mut self.mask.brush.hardness, &SliderSpec { slot: 101, label: "Hardness", min: 0.0, max: 1.0, default: 0.75, step: 0.01, decimals: 2, tooltip: "Solid inner radius; Shift+[ / ] changes hardness by 10%" });
            slider_row(ui, &mut self.mask.brush.opacity, &SliderSpec { slot: 102, label: "Opacity", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Maximum coverage per stroke; overlapping dabs do not build up" });
            ui.horizontal(|ui| {
                ui.label("View").on_hover_text("Choose how to inspect the mask on the image");
                for (view, label, tip) in [
                    (MaskView::Overlay, "Overlay", "Show the mask as a magenta overlay"),
                    (MaskView::MaskOnly, "Mask", "Show only the grayscale mask"),
                    (MaskView::Off, "Off", "Hide the mask preview"),
                ] { ui.selectable_value(&mut self.mask.view, view, label).on_hover_text(tip); }
            });
            slider_row(ui, &mut self.mask.overlay_opacity, &SliderSpec { slot: 103, label: "Overlay", min: 0.0, max: 1.0, default: 0.4, step: 0.01, decimals: 2, tooltip: "Magenta preview opacity; does not change the mask" });
            egui::CollapsingHeader::new("Build from image").default_open(true).show(ui, |ui| {
                choice(ui, "Combine", &mut self.mask.combine, &[
                    (Combine::Replace, "Replace"),
                    (Combine::Add, "Add"),
                    (Combine::Subtract, "Subtract"),
                    (Combine::Intersect, "Intersect"),
                ]);
                slider_row(ui, &mut self.mask.lum_range[0], &SliderSpec { slot: 104, label: "Lum. low", min: 0.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, tooltip: "Luminosity at or below this value is excluded; ramps linearly up to the high value" });
                self.mask.lum_range[0] = self.mask.lum_range[0].min(self.mask.lum_range[1]);
                slider_row(ui, &mut self.mask.lum_range[1], &SliderSpec { slot: 105, label: "Lum. high", min: 0.0, max: 1.0, default: 1.0, step: 0.01, decimals: 2, tooltip: "Luminosity above this value is fully selected" });
                self.mask.lum_range[1] = self.mask.lum_range[1].max(self.mask.lum_range[0]);
                ui.add_enabled_ui(self.picture.is_some(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (name, kind, tip) in [
                            ("Luminosity", MaskKind::Luminosity, "Build from image luminance using the low/high range"),
                            ("Transparency", MaskKind::Transparency, "Build from the image alpha channel"),
                            ("Outlines", MaskKind::Outlines, "Build from image edges"),
                        ] {
                            if ui.button(name).on_hover_text(tip).clicked() {
                                if let Some(pic) = &self.picture { self.mask.build(pic, kind); }
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
                slider_row(ui, &mut self.mask.feather, &SliderSpec { slot: 106, label: "Feather px", min: 0.0, max: 100.0, default: 5.0, step: 0.5, decimals: 1, tooltip: "Blur radius in image pixels; Apply commits the refinement" });
                if ui.button("Apply feather").on_hover_text("Soften the mask edges using the feather radius").clicked() { self.mask.apply_feather(&self.gpu); }
                slider_row(ui, &mut self.mask.grow, &SliderSpec { slot: 107, label: "Grow/Shrink", min: -100.0, max: 100.0, default: 5.0, step: 1.0, decimals: 0, tooltip: "Image pixels: positive expands the selection, negative contracts it" });
                if ui.button("Apply grow/shrink").on_hover_text("Expand or contract the mask by the chosen pixel radius").clicked() { self.mask.apply_grow(&self.gpu); }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Invert").on_hover_text("Swap selected and excluded pixels").clicked() { self.mask.invert(); }
                    if ui.button("Clear").on_hover_text("Clear the entire mask; asks for confirmation when non-empty").clicked() {
                        if self.mask.is_empty() || rfd::MessageDialog::new().set_title("Clear mask?")
                            .set_description("Clear the entire mask? You can undo this change.")
                            .set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
                        { self.mask.clear(); }
                    }
                    if ui.button("Save mask…").on_hover_text("Save the grayscale mask as a PNG image").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("mask.png").save_file() {
                            match self.mask.save_png(&path) {
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
        ui.label("Save current settings");
        ui.add(egui::TextEdit::singleline(&mut self.panels.preset_name).hint_text("Preset name").desired_width(f32::INFINITY));
        if ui.button("Save current preset").clicked() {
            if self.panels.preset_name.trim().is_empty() { self.status = "Preset name cannot be empty".into(); }
            else {
                self.panels.presets.push(Preset { name: self.panels.preset_name.trim().into(), params: self.params.clone() });
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
                                    ui.add(egui::Button::selectable(self.params == preset.params, &preset.name).min_size(egui::vec2(0.0, 24.0)))
                                })
                                .inner;
                            if response.clicked() { self.params = preset.params.clone(); }
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
                    let track = ui.add(egui::Slider::new(value, spec.min..=spec.max).show_value(false).step_by(spec.step as f64));
                    if track.secondary_clicked() || track.clicked_by(egui::PointerButton::Middle) { *value = spec.default; }
                });
                let number = ui.add_sized([62.0, 20.0], egui::DragValue::new(value).range(spec.min..=spec.max).speed(spec.step).fixed_decimals(spec.decimals as usize).update_while_editing(false));
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

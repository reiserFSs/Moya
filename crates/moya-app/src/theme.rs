use eframe::egui::{self, Color32, Response, Stroke};

// Warm orange sampled from the bundled Moya ring logo.
pub const ACCENT: Color32 = Color32::from_rgb(255, 181, 115);

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("Inter".into(), egui::FontData::from_static(include_bytes!("../assets/Inter.ttf")).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "Inter".into());
    fonts.font_data.insert("Noto Symbols".into(), egui::FontData::from_static(include_bytes!("../assets/NotoSansSymbols2-Regular.ttf")).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().push("Noto Symbols".into());
    ctx.set_fonts(fonts);
    let mut style = egui::Style::default();
    for (kind, size) in [(egui::TextStyle::Body, 13.0), (egui::TextStyle::Button, 13.0), (egui::TextStyle::Small, 10.5), (egui::TextStyle::Heading, 16.0), (egui::TextStyle::Monospace, 12.0)] {
        let family = if kind == egui::TextStyle::Monospace { egui::FontFamily::Monospace } else { egui::FontFamily::Proportional };
        style.text_styles.insert(kind, egui::FontId::new(size, family));
    }
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 22.0;
    style.spacing.slider_rail_height = 4.0;
    style.visuals = egui::Visuals::dark();
    style.visuals.handle_shape = egui::style::HandleShape::Circle;
    let visuals = &mut style.visuals;
    visuals.panel_fill = Color32::from_rgb(25, 26, 29);
    visuals.window_fill = Color32::from_rgb(30, 31, 35);
    visuals.extreme_bg_color = Color32::from_rgb(18, 19, 22);
    visuals.faint_bg_color = Color32::from_rgb(36, 37, 42);
    visuals.weak_text_color = Some(Color32::from_rgb(149, 150, 157));
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.22);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.slider_trailing_fill = true;
    visuals.disabled_alpha = 0.35;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(47, 48, 54);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(35, 36, 41);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(65, 66, 73));
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(65, 57, 51);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::from_rgb(25, 26, 29));
    for widget in [&mut visuals.widgets.noninteractive, &mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active, &mut visuals.widgets.open] {
        widget.corner_radius = egui::CornerRadius::same(6);
    }
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

pub fn switch(ui: &mut egui::Ui, value: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::click());
    if response.clicked() { *value = !*value; response.mark_changed(); }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *value, ""));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let fill = if *value { ACCENT } else { ui.visuals().widgets.inactive.bg_fill };
        ui.painter().rect_filled(rect, 8.0, fill);
        let x = if *value { rect.right() - 8.0 } else { rect.left() + 8.0 };
        let knob = if *value { ui.visuals().panel_fill } else { visuals.fg_stroke.color };
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), 5.0, knob);
    }
    response
}

pub fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> Response {
    let result = egui::Frame::new().stroke(ui.visuals().widgets.inactive.bg_stroke).corner_radius(7).inner_margin(2).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut changed = false;
            for &(item, label) in options {
                if ui.selectable_label(*value == item, label).clicked() && *value != item { *value = item; changed = true; }
            }
            changed
        }).inner
    });
    let mut response = result.response;
    if result.inner { response.mark_changed(); }
    response
}

/// Section header shared by every panel: hairline, chevron, small muted capitals and, for
/// sections with an on/off parameter, a switch on the right. Returns whether the section is
/// expanded; callers decide whether a disabled section still shows its body.
pub fn section(ui: &mut egui::Ui, title: &str, enabled: Option<&mut bool>, tooltip: &str) -> bool {
    ui.add_space(4.0);
    let line = ui.available_rect_before_wrap();
    ui.painter().hline(line.x_range(), line.top(), Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
    ui.add_space(4.0);
    let id = ui.make_persistent_id(("section", title));
    let mut open = ui.data_mut(|data| data.get_temp::<bool>(id).unwrap_or(true));
    ui.horizontal(|ui| {
        let switch_width = if enabled.is_some() { 28.0 + ui.spacing().item_spacing.x } else { 0.0 };
        let (rect, response) = ui.allocate_exact_size(egui::vec2((ui.available_width() - switch_width).max(20.0), 18.0), egui::Sense::click());
        let color = ui.visuals().weak_text_color();
        let c = egui::pos2(rect.left() + 5.0, rect.center().y);
        let points = if open { vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)] } else { vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(-2.0, 4.0), c + egui::vec2(3.0, 0.0)] };
        ui.painter().add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        let galley = ui.painter().layout_no_wrap(title.to_uppercase(), egui::FontId::proportional(10.5), color);
        ui.painter().galley(egui::pos2(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0), galley, color);
        let response = if tooltip.is_empty() { response } else { response.on_hover_text(tooltip) };
        if response.clicked() { open = !open; }
        if let Some(enabled) = enabled { switch(ui, enabled).on_hover_text(tooltip); }
    });
    ui.data_mut(|data| data.insert_temp(id, open));
    open
}

/// "Label ......... [switch]" row, the mockup's layout for on/off options.
pub fn switch_row(ui: &mut egui::Ui, label: &str, value: &mut bool, tooltip: &str) -> Response {
    ui.horizontal(|ui| {
        let label = ui.label(label);
        let response = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| switch(ui, value)).inner;
        if tooltip.is_empty() { response } else { label.on_hover_text(tooltip); response.on_hover_text(tooltip) }
    }).inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_font_has_toolbar_symbols() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                // Undo, redo, gear, and duplicate are painted shapes, not font glyphs.
                let missing: String = "◉▾".chars().filter(|&symbol| !fonts.has_glyph(&egui::FontId::proportional(14.0), symbol)).collect();
                assert!(missing.is_empty(), "missing {missing}");
            });
        });
    }
}

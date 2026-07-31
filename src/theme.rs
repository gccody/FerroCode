use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, Style, TextStyle, Visuals,
};

pub const BG: Color32 = Color32::from_rgb(14, 15, 17);
pub const PANEL: Color32 = Color32::from_rgb(17, 18, 21);
pub const PANEL_ALT: Color32 = Color32::from_rgb(24, 25, 29);
pub const ELEVATED: Color32 = Color32::from_rgb(29, 30, 34);
pub const BORDER: Color32 = Color32::from_rgb(43, 45, 51);
pub const TEXT: Color32 = Color32::from_rgb(235, 235, 238);
pub const MUTED: Color32 = Color32::from_rgb(139, 141, 149);
pub const ACCENT: Color32 = Color32::from_rgb(124, 101, 224);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(43, 36, 68);
pub const SUCCESS: Color32 = Color32::from_rgb(76, 205, 145);
pub const WARNING: Color32 = Color32::from_rgb(244, 183, 74);
pub const DANGER: Color32 = Color32::from_rgb(242, 98, 112);
pub const USER_BUBBLE: Color32 = Color32::from_rgb(31, 31, 34);

pub fn apply(ctx: &egui::Context) {
    let mut style = Style::default();
    style.spacing.item_spacing = egui::vec2(7.0, 7.0);
    style.spacing.button_padding = egui::vec2(9.0, 5.0);
    style.spacing.interact_size.y = 30.0;
    style.visuals = visuals();
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(18.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
    ctx.set_style(style);
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL_ALT;
    v.extreme_bg_color = BG;
    v.faint_bg_color = PANEL_ALT;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT_SOFT;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_corner_radius = CornerRadius::same(10);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.bg_fill = PANEL_ALT;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.corner_radius = CornerRadius::same(7);
    v.widgets.hovered.bg_fill = ELEVATED;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(67, 70, 84));
    v.widgets.hovered.corner_radius = CornerRadius::same(7);
    v.widgets.active.bg_fill = ACCENT_SOFT;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.corner_radius = CornerRadius::same(7);
    v
}

pub fn short_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_owned();
    }
    let keep = max.saturating_sub(1);
    let tail: String = path
        .chars()
        .rev()
        .take(keep)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

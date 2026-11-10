//! Минималистичная тема в духе Material Design 3 (тёмная и светлая палитры).

use std::sync::Arc;

use eframe::egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextStyle, Visuals, vec2,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub background: Color32,
    pub container: Color32,
    pub container_high: Color32,
    pub container_highest: Color32,
    pub on_surface: Color32,
    pub on_variant: Color32,
    pub outline: Color32,
    pub outline_variant: Color32,
    pub primary: Color32,
    pub on_primary: Color32,
    pub primary_container: Color32,
    pub on_primary_container: Color32,
    pub secondary_container: Color32,
    pub on_secondary_container: Color32,
    pub error: Color32,
    pub error_container: Color32,
    pub on_error_container: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            background: rgb(0x141218),
            container: rgb(0x211F26),
            container_high: rgb(0x2B2930),
            container_highest: rgb(0x36343B),
            on_surface: rgb(0xE6E0E9),
            on_variant: rgb(0xCAC4D0),
            outline: rgb(0x938F99),
            outline_variant: rgb(0x49454F),
            primary: rgb(0xD0BCFF),
            on_primary: rgb(0x381E72),
            primary_container: rgb(0x4F378B),
            on_primary_container: rgb(0xEADDFF),
            secondary_container: rgb(0x4A4458),
            on_secondary_container: rgb(0xE8DEF8),
            error: rgb(0xF2B8B5),
            error_container: rgb(0x8C1D18),
            on_error_container: rgb(0xF9DEDC),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            background: rgb(0xFEF7FF),
            container: rgb(0xF3EDF7),
            container_high: rgb(0xECE6F0),
            container_highest: rgb(0xE6E0E9),
            on_surface: rgb(0x1D1B20),
            on_variant: rgb(0x49454F),
            outline: rgb(0x79747E),
            outline_variant: rgb(0xCAC4D0),
            primary: rgb(0x6750A4),
            on_primary: rgb(0xFFFFFF),
            primary_container: rgb(0xEADDFF),
            on_primary_container: rgb(0x21005D),
            secondary_container: rgb(0xE8DEF8),
            on_secondary_container: rgb(0x1D192B),
            error: rgb(0xB3261E),
            error_container: rgb(0xF9DEDC),
            on_error_container: rgb(0x410E0B),
        }
    }

    /// Мягкий цвет для аватара приложения: оттенок зависит от имени.
    pub fn avatar_colors(&self, name: &str) -> (Color32, Color32) {
        let hash = name
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        let hue = (hash % 360) as f32 / 360.0;
        let (bg_l, fg_l) = if self.dark { (0.30, 0.85) } else { (0.88, 0.25) };
        (hsl(hue, 0.35, bg_l), hsl(hue, 0.55, fg_l))
    }
}

fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(to(r), to(g), to(b))
}

/// Подключает системный шрифт Segoe UI (Windows), если он есть: родной вид и кириллица.
pub fn install_fonts(ctx: &Context) {
    let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("segoe".into(), Arc::new(FontData::from_owned(bytes)));
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "segoe".into());
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &Context, p: &Palette) {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.on_surface);
    v.panel_fill = p.background;
    v.window_fill = p.container_high;
    v.extreme_bg_color = p.container_high;
    v.faint_bg_color = p.container;
    v.hyperlink_color = p.primary;
    v.selection.bg_fill = p.primary_container;
    v.selection.stroke = Stroke::new(1.0, p.on_primary_container);
    v.window_stroke = Stroke::new(1.0, p.outline_variant);
    v.window_corner_radius = CornerRadius::same(28);
    v.menu_corner_radius = CornerRadius::same(12);

    let radius = CornerRadius::same(20);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.outline_variant);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.on_surface);
    for (w, fill, stroke) in [
        (&mut v.widgets.inactive, p.container_high, Stroke::new(1.0, p.outline)),
        (&mut v.widgets.hovered, p.container_highest, Stroke::new(1.0, p.on_surface)),
        (&mut v.widgets.active, p.secondary_container, Stroke::new(2.0, p.primary)),
        (&mut v.widgets.open, p.container_highest, Stroke::new(1.0, p.outline)),
    ] {
        w.corner_radius = radius;
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = stroke;
        w.fg_stroke = Stroke::new(1.0, p.on_surface);
    }
    v.widgets.noninteractive.corner_radius = radius;
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(16.0, 8.0);
        s.spacing.window_margin = Margin::same(24);
        s.spacing.interact_size.y = 32.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
    });
}

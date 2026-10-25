//! Look of the app: the Enshrouded palette (site colours, Flame amber, teal sky), bundled fonts, and the small set
//! of widgets every tab is built from (cards, section titles, value rows, deltas, buttons, segmented choices).

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke, Vec2};
use std::sync::Arc;

pub const BG0: Color32 = Color32::from_rgb(0x0D, 0x0B, 0x09);
pub const BG1: Color32 = Color32::from_rgb(0x17, 0x11, 0x0C);
pub const RAISED: Color32 = Color32::from_rgb(0x21, 0x18, 0x0F);
pub const HILITE: Color32 = Color32::from_rgb(0x33, 0x28, 0x1C);
pub const LINE: Color32 = Color32::from_rgb(0x2A, 0x20, 0x16);
pub const BORDER: Color32 = Color32::from_rgb(0x3A, 0x2C, 0x1E);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x56, 0x31, 0x22);
pub const TEXT: Color32 = Color32::from_rgb(0xF5, 0xEC, 0xE8);
pub const TEXT_SOFT: Color32 = Color32::from_rgb(0xCF, 0xC5, 0xA6);
pub const TEXT2: Color32 = Color32::from_rgb(0xB3, 0xA9, 0x8A);
pub const TEXT3: Color32 = Color32::from_rgb(0x8C, 0x82, 0x6A);
pub const HEAD: Color32 = Color32::from_rgb(0xEC, 0xD7, 0x99);
pub const FLAME: Color32 = Color32::from_rgb(0xE8, 0xB0, 0x4A);
pub const TEAL: Color32 = Color32::from_rgb(0x4F, 0xC3, 0xC8);
pub const GOOD: Color32 = Color32::from_rgb(0x89, 0xE1, 0x91);
pub const BAD: Color32 = Color32::from_rgb(0xE5, 0x53, 0x4B);
pub const WARN: Color32 = Color32::from_rgb(0xF0, 0xA0, 0x4B);
pub const WARN_BG: Color32 = Color32::from_rgb(0x24, 0x1A, 0x10);
pub const WARN_LINE: Color32 = Color32::from_rgb(0x5A, 0x3E, 0x1C);
pub const EDGE: Color32 = Color32::from_rgb(0x3A, 0x2A, 0x1E);

pub fn class_color(class: &str) -> Color32 {
    match class {
        "Ranger" => Color32::from_rgb(0x6F, 0xAE, 0x4E),
        "Mage" => Color32::from_rgb(0x4A, 0x86, 0xD8),
        "Warrior" => Color32::from_rgb(0xC8, 0x46, 0x3C),
        _ => Color32::from_rgb(0xD9, 0xA4, 0x41),
    }
}

pub fn rarity_color(r: &str) -> Color32 {
    match r {
        "Uncommon" => GOOD,
        "Rare" => Color32::from_rgb(0x7D, 0xA7, 0xEE),
        "Epic" => Color32::from_rgb(0xB8, 0x8C, 0xE6),
        "Legendary" => WARN,
        _ => TEXT_SOFT,
    }
}

pub fn heading_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("heading".into()))
}

pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn setup(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("alegreya".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/AlegreyaSans-Regular.ttf"))));
    fonts.font_data.insert("alegreya-bold".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/AlegreyaSans-Bold.ttf"))));
    fonts.font_data.insert("cinzel".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Cinzel.ttf"))));
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "alegreya".into());
    let mut bold = vec!["alegreya-bold".to_string()];
    bold.extend(fallback.iter().cloned());
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    let mut head = vec!["cinzel".to_string()];
    head.extend(fallback);
    fonts.families.insert(FontFamily::Name("heading".into()), head);
    ctx.set_fonts(fonts);

    // we set the window's dark title bar once ourselves (main.rs); egui's sync would keep re-sending it
    ctx.options_mut(|o| o.sync_window_theme = false);
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.global_style()).clone();
    use egui::TextStyle::*;
    style.text_styles = [
        (Small, FontId::proportional(13.0)),
        (Body, FontId::proportional(15.5)),
        (Button, FontId::proportional(15.0)),
        (Heading, heading_font(18.0)),
        (Monospace, FontId::monospace(13.0)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.combo_height = 420.0;
    style.interaction.tooltip_delay = 0.25;
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG0;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = egui::CornerRadius::same(8);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.extreme_bg_color = BG0;
    v.faint_bg_color = Color32::from_rgb(0x1B, 0x14, 0x0E);
    v.code_bg_color = RAISED;
    v.hyperlink_color = TEAL;
    v.warn_fg_color = WARN;
    v.error_fg_color = BAD;
    v.selection.bg_fill = BORDER_STRONG;
    v.selection.stroke = Stroke::new(1.0, FLAME);
    v.text_cursor.stroke = Stroke::new(2.0, FLAME);
    let r = egui::CornerRadius::same(6);
    v.widgets.noninteractive.bg_fill = BG1;
    v.widgets.noninteractive.weak_bg_fill = BG1;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.bg_fill = HILITE;
    v.widgets.hovered.weak_bg_fill = HILITE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, FLAME);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, HEAD);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.bg_fill = BORDER_STRONG;
    v.widgets.active.weak_bg_fill = BORDER_STRONG;
    v.widgets.active.bg_stroke = Stroke::new(1.0, FLAME);
    v.widgets.active.fg_stroke = Stroke::new(1.5, HEAD);
    v.widgets.active.corner_radius = r;
    v.widgets.open = v.widgets.active;
    v.slider_trailing_fill = true;
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// A content card: dark panel, thin border, generous padding, full width of its column.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_with(ui, BG1, BORDER, add)
}

pub fn card_with<R>(ui: &mut egui::Ui, fill: Color32, stroke: Color32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(10).inner_margin(egui::Margin::same(18)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    }).inner
}

/// A small inner tile (one value in a row of values).
pub fn tile(ui: &mut egui::Ui, fill: Color32, stroke: Color32, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 9)).show(ui, add).response
}

/// Card title in the heading face.
pub fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(heading_font(18.0)).color(HEAD));
}

/// Card title with a right-aligned note.
pub fn title_with(ui: &mut egui::Ui, text: &str, note: RichText) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).font(heading_font(18.0)).color(HEAD));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(note);
        });
    });
}

/// Small upper-case label above a group.
pub fn eyebrow(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(12.0).color(TEXT2));
}

/// A label / value row with the value right-aligned and a hairline under it.
pub fn kv(ui: &mut egui::Ui, label: impl Into<RichText>, value: RichText) -> egui::Response {
    let r = ui.horizontal(|ui| {
        ui.label(label.into().color(TEXT2));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(value);
        });
    }).response;
    let y = r.rect.bottom() + 3.0;
    ui.painter().hline(r.rect.x_range(), y, Stroke::new(1.0, LINE));
    ui.add_space(4.0);
    r
}

/// The change from `now` to `then` as a coloured label (nothing when unchanged).
pub fn delta(now: f64, then: f64, higher_better: bool, f: impl Fn(f64) -> String) -> Option<RichText> {
    let d = then - now;
    if d.abs() < 1e-6 || !d.is_finite() {
        return None;
    }
    let good = (d > 0.0) == higher_better;
    Some(RichText::new(f(d)).font(bold_font(15.0)).color(if good { GOOD } else { BAD }))
}

pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).font(bold_font(15.0)).color(BG1)).fill(FLAME).stroke(Stroke::NONE).corner_radius(6)
}

pub fn ghost(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(TEXT_SOFT)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BORDER)).corner_radius(6)
}

pub fn outline(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(HEAD)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, FLAME)).corner_radius(6)
}

/// A row of mutually exclusive choices drawn as one joined control.
pub fn segmented<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new().stroke(Stroke::new(1.0, BORDER)).corner_radius(6).fill(BG1).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            for (v, label) in options {
                let on = *value == *v;
                let b = egui::Button::new(RichText::new(*label).color(if on { HEAD } else { TEXT2 }).font(if on { bold_font(14.5) } else { FontId::proportional(14.5) }))
                    .fill(if on { HILITE } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(5);
                if ui.add(b).clicked() && !on {
                    *value = v.clone();
                    changed = true;
                }
            }
        });
    });
    changed
}

/// A setting row: name and a one-line explanation on the left, the control on the right; dimmed when it does not
/// affect the current build.
pub fn setting(ui: &mut egui::Ui, name: &str, help: &str, relevant: bool, control: impl FnOnce(&mut egui::Ui)) {
    let r = ui.scope(|ui| {
        if !relevant {
            ui.set_opacity(0.5);
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(name).font(bold_font(15.0)).color(TEXT));
                ui.label(RichText::new(help).size(13.0).color(TEXT2));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
        });
    }).response;
    ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 4.0, Stroke::new(1.0, LINE));
    ui.add_space(8.0);
}

/// One value tile in a flowing row (the hit chain, survival stats), optionally preceded by an operator.
pub struct Block {
    pub op: Option<&'static str>,
    pub label: String,
    pub value: String,
    pub strong: bool,
    pub color: Option<Color32>,
    pub tip: Option<String>,
}

impl Block {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Block { op: None, label: label.into(), value: value.into(), strong: false, color: None, tip: None }
    }
    pub fn op(mut self, op: &'static str) -> Self {
        self.op = Some(op);
        self
    }
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
    pub fn tip(mut self, t: impl Into<String>) -> Self {
        self.tip = Some(t.into());
        self
    }
}

/// Lays tiles out left to right, starting a new row when the next one would not fit (frames cannot be measured
/// before they are drawn, so widths come from the text). `per_row` gives equal-width tiles, that many per row.
pub fn blocks(ui: &mut egui::Ui, items: Vec<Block>, per_row: Option<usize>) {
    let avail = ui.available_width();
    let gap = 8.0;
    let pad = 26.0;
    let measure = |ui: &egui::Ui, t: &str, f: FontId| ui.painter().layout_no_wrap(t.to_string(), f, TEXT).size().x;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(gap, 10.0);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let mut used = 0.0;
        for (i, b) in items.into_iter().enumerate() {
            let inner = match per_row {
                Some(n) => ((avail - gap * (n as f32 - 1.0)) / n as f32 - pad).floor(),
                None => measure(ui, &b.label, FontId::proportional(12.5)).max(measure(ui, &b.value, bold_font(19.0))).max(36.0) + 2.0,
            };
            let opw = if b.op.is_some() && per_row.is_none() { 14.0 + gap } else { 0.0 };
            let need = inner + pad + gap + opw;
            let new_row = match per_row {
                Some(n) => i > 0 && i % n == 0,
                None => used > 0.0 && used + need > avail,
            };
            if new_row {
                ui.end_row();
                used = 0.0;
            }
            if let (Some(op), None) = (b.op, per_row) {
                ui.label(RichText::new(op).size(18.0).color(TEXT3));
            }
            let (fill, stroke, lc, vc) = if b.strong { (HILITE, FLAME, HEAD, HEAD) } else { (RAISED, BORDER, TEXT2, b.color.unwrap_or(TEXT)) };
            let r = tile(ui, fill, stroke, |ui| {
                ui.set_width(inner);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(&b.label).size(12.5).color(lc));
                    ui.label(RichText::new(&b.value).font(bold_font(19.0)).color(vc));
                });
            });
            if let Some(t) = b.tip {
                r.on_hover_text(t);
            }
            used += need;
        }
    });
}

/// A fixed-width table cell, left- or right-aligned, truncating long text.
pub fn cell(ui: &mut egui::Ui, width: f32, text: RichText, right: bool) -> egui::Response {
    let layout = if right { egui::Layout::right_to_left(egui::Align::Center) } else { egui::Layout::left_to_right(egui::Align::Center) };
    ui.allocate_ui_with_layout(Vec2::new(width, 22.0), layout, |ui| {
        ui.set_min_width(width);
        ui.add(egui::Label::new(text).truncate());
    }).response
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

//! The meter drawn as an image for the in-game overlay (ens-overlay draws it into the game's frames;
//! docs/plans/game-overlay.md). Same content as the meter window's Damage tab, compact: fight, DPS, the top sources
//! as bars, and the planner's DPS for the open build.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use ens_calc::meter::View;
use ens_overlay::frame::Frame;

pub const WIDTH: u32 = 300;
const BARS: usize = 5;

/// RGB colours, as the meter window's stylesheet.
const BG: [u8; 3] = [16, 13, 11];
const HEADER: [u8; 3] = [8, 7, 6];
const TEXT: [u8; 3] = [245, 236, 232];
const TEXT2: [u8; 3] = [185, 173, 162];
const FLAME: [u8; 3] = [232, 176, 74];
const GOOD: [u8; 3] = [137, 225, 145];
const BAR_COLORS: [[u8; 3]; 5] = [[232, 176, 74], [79, 195, 200], [180, 140, 224], [137, 225, 145], [229, 83, 75]];

/// The fonts: Segoe UI (bold for numbers) from Windows, Arial as the fallback. None when neither is there.
pub struct Fonts {
    regular: FontVec,
    bold: FontVec,
}

impl Fonts {
    pub fn load() -> Option<Fonts> {
        let read = |names: &[&str]| names.iter().find_map(|n| std::fs::read(format!("C:/Windows/Fonts/{n}")).ok()).and_then(|b| FontVec::try_from_vec(b).ok());
        Some(Fonts { regular: read(&["segoeui.ttf", "arial.ttf"])?, bold: read(&["segoeuib.ttf", "arialbd.ttf", "segoeui.ttf"])? })
    }
}

struct Canvas {
    f: Frame,
}

impl Canvas {
    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.f.width as i32 || y >= self.f.height as i32 || a <= 0.0 {
            return;
        }
        let i = (y as usize * self.f.width as usize + x as usize) * 4;
        let p = &mut self.f.pixels[i..i + 4];
        // BGRA
        for (k, ch) in [2usize, 1, 0].iter().enumerate() {
            p[*ch] = (p[*ch] as f32 * (1.0 - a) + c[k] as f32 * a).round() as u8;
        }
        p[3] = 255;
    }
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3], a: f32) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.blend(xx, yy, c, a);
            }
        }
    }
    /// Draw text with its baseline at `y`; returns the width drawn. Stops at `max_w` with an ellipsis.
    fn text(&mut self, font: &FontVec, size: f32, x: i32, y: i32, s: &str, c: [u8; 3], max_w: Option<f32>) -> f32 {
        let sf = font.as_scaled(PxScale::from(size));
        let ell = sf.h_advance(font.glyph_id('…'));
        let mut caret = 0.0f32;
        let chars: Vec<char> = s.chars().collect();
        for (k, ch) in chars.iter().enumerate() {
            let id = font.glyph_id(*ch);
            let adv = sf.h_advance(id);
            let clip = max_w.is_some_and(|m| caret + adv + if k + 1 < chars.len() { ell } else { 0.0 } > m);
            let (id, adv) = if clip { (font.glyph_id('…'), ell) } else { (id, adv) };
            let g = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x as f32 + caret, y as f32));
            if let Some(o) = font.outline_glyph(g) {
                let bb = o.px_bounds();
                o.draw(|gx, gy, cov| self.blend(bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32, c, cov));
            }
            caret += adv;
            if clip {
                break;
            }
        }
        caret
    }
    fn text_width(font: &FontVec, size: f32, s: &str) -> f32 {
        let sf = font.as_scaled(PxScale::from(size));
        s.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum()
    }
}

fn short(n: f64) -> String {
    // -0.0 (an idle rolling window) prints as 0.0
    let n = n + 0.0;
    let a = n.abs();
    if a >= 1e6 { format!("{:.2}M", n / 1e6) } else if a >= 1e4 { format!("{:.1}k", n / 1e3) } else if a >= 100.0 { format!("{n:.0}") } else { format!("{n:.1}") }
}

fn clock(s: f64) -> String {
    format!("{}:{:02}", (s / 60.0) as u64, (s % 60.0) as u64)
}

/// The meter view as an overlay image.
pub fn render(fonts: &Fonts, v: &View, planner_dps: Option<f64>) -> Frame {
    let rows = v.current.as_ref().map(|f| f.sources.len().min(BARS)).unwrap_or(0);
    let height = 22 + 50 + rows as u32 * 21 + if rows == 0 { 22 } else { 4 } + 20;
    let mut c = Canvas { f: Frame::new(WIDTH, height) };
    let w = WIDTH as i32;
    c.rect(0, 0, w, height as i32, BG, 1.0);
    // header: in-combat dot, fight name, timer
    c.rect(0, 0, w, 22, HEADER, 1.0);
    c.rect(8, 7, 8, 8, if v.in_combat { GOOD } else { TEXT2 }, 1.0);
    let f = v.current.as_ref();
    let timer = f.map(|f| clock(f.seconds)).unwrap_or_default();
    let tw = Canvas::text_width(&fonts.regular, 13.0, &timer);
    c.text(&fonts.regular, 13.0, w - 8 - tw as i32, 16, &timer, TEXT2, None);
    let label = f.map(|f| f.label.clone()).unwrap_or_else(|| "Damage meter".into());
    c.text(&fonts.regular, 13.0, 22, 16, &label, TEXT2, Some((w - 22 - 16) as f32 - tw));
    // DPS, rolling DPS, one line of stats
    let mut y = 22;
    let dps = f.map(|f| short(f.dps)).unwrap_or_else(|| "–".into());
    let dw = c.text(&fonts.bold, 26.0, 8, y + 28, &dps, FLAME, None);
    c.text(&fonts.regular, 13.0, 8 + dw as i32 + 6, y + 28, "DPS", TEXT2, None);
    if let Some(now) = f.and_then(|f| f.current_dps) {
        let s = format!("now {}/s", short(now));
        let nw = Canvas::text_width(&fonts.regular, 13.0, &s);
        c.text(&fonts.regular, 13.0, w - 8 - nw as i32, y + 28, &s, TEXT2, None);
    }
    if let Some(f) = f {
        c.text(&fonts.regular, 12.0, 8, y + 45, &format!("{} dmg · {:.0}% crit · top {}", short(f.damage), f.crit_pct, short(f.biggest)), TEXT2, None);
    }
    y += 50;
    // bars, widest = top source
    match f {
        Some(f) if !f.sources.is_empty() => {
            let top = f.sources.iter().map(|s| s.damage).fold(1.0, f64::max);
            for (k, s) in f.sources.iter().take(BARS).enumerate() {
                let by = y + k as i32 * 21;
                c.rect(6, by, w - 12, 19, [40, 35, 30], 1.0);
                c.rect(6, by, ((w - 12) as f64 * s.damage / top) as i32, 19, BAR_COLORS[k % BAR_COLORS.len()], 0.55);
                let val = format!("{} ({}/s) {:.0}%", short(s.damage), short(s.dps), s.share * 100.0);
                let vw = Canvas::text_width(&fonts.regular, 12.5, &val);
                c.text(&fonts.regular, 12.5, w - 12 - vw as i32, by + 14, &val, TEXT, None);
                c.text(&fonts.regular, 12.5, 12, by + 14, &s.name, TEXT, Some((w - 30) as f32 - vw));
            }
            y += rows as i32 * 21 + 4;
        }
        _ => {
            c.text(&fonts.regular, 12.5, 8, y + 15, "Waiting for your first hit…", TEXT2, None);
            y += 22;
        }
    }
    let foot = match planner_dps {
        Some(p) => format!("Planner: {} DPS for the open build", short(p)),
        None => "Unshrouded live meter".into(),
    };
    c.text(&fonts.regular, 11.5, 8, y + 14, &foot, TEXT2, Some((w - 16) as f32));
    c.f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_fight_with_a_bar_per_source() {
        let Some(fonts) = Fonts::load() else { return };
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let head = "ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,type0,type1,type2,type3,type4,type5,\
type6,type7,type8,type9,type10,type11,record,blocked,no_damage,loss_x10,unix_ms,attacker_level,target_level,target_health_max,root_source,\
target_id,root_target,source,permission,weapon_category,weapon_pide,program,material,attack_category,target_faction";
        let mut text = format!("{head}\n");
        for k in 0..6u64 {
            let (a, w) = if k % 2 == 0 { (1, 0x1573ffb0u32) } else { (9914, 0) };
            text += &format!("{},{a},77,-50,0,0x0,false,false,false,50,0,0,0,0,0,0,0,0,0,0,0,,false,false,0,{},0x13,0xa,0x64,{a},77,77,{a},1,{w},0,102728174,0,1,2\n",
                1000 + k * 500, 1_792_700_000_000 + 1000 + k * 500);
        }
        let mut m = ens_calc::meter::Meter::default();
        m.feed(&text);
        let v = m.view(&b, 1_792_700_000_000 + 4000);
        let f = render(&fonts, &v, Some(420.0));
        assert_eq!(f.width, WIDTH);
        assert_eq!(f.height, 22 + 50 + 2 * 21 + 4 + 20, "two sources, two bars");
        assert_eq!(f.pixels.len(), (f.width * f.height * 4) as usize);
        // the DPS digits are drawn in the flame colour
        let flame = f.pixels.chunks(4).filter(|p| p[2] > 200 && p[1] > 150 && p[0] < 110).count();
        assert!(flame > 30, "{flame} flame-coloured pixels");
        assert!(f.pixels.chunks(4).all(|p| p[3] == 255), "opaque");
        // ENS_OVERLAY_SHOT=<file.bmp> saves the image to look at
        if let Ok(path) = std::env::var("ENS_OVERLAY_SHOT") {
            let (w, h) = (f.width as usize, f.height as usize);
            let mut bmp = vec![];
            let size = 54 + w * h * 4;
            bmp.extend(b"BM");
            bmp.extend((size as u32).to_le_bytes());
            bmp.extend([0u8; 4]);
            bmp.extend(54u32.to_le_bytes());
            bmp.extend(40u32.to_le_bytes());
            bmp.extend((w as i32).to_le_bytes());
            bmp.extend((-(h as i32)).to_le_bytes());
            bmp.extend(1u16.to_le_bytes());
            bmp.extend(32u16.to_le_bytes());
            bmp.extend([0u8; 24]);
            bmp.extend(&f.pixels);
            std::fs::write(path, bmp).unwrap();
        }
    }
}

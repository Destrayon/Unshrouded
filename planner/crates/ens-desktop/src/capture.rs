//! The in-game meter's picture (docs/plans/overlay-parity.md): an off-screen copy of the meter window in overlay mode,
//! captured with WebView2's CapturePreview, so what is drawn in the game is the real meter UI.

use std::sync::mpsc::Sender;

/// WebView2 arguments for every window of the app (they share one browser, which refuses windows with other arguments):
/// wry's defaults (`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`) plus never pausing a window it
/// thinks is covered. The game covers the hidden pages the in-game meter is captured from (2026-10-24: they stopped
/// painting behind the game, the captures went stale and the meter fell back to its drawn picture). tauri.conf.json
/// gives the main window the same string.
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows --disable-renderer-backgrounding";

/// Off-screen: the pages captured for the game sit here, out of sight. Set after the window exists (the builder's
/// position was not applied: the windows cascaded onto the desktop).
fn off_screen(w: &tauri::WebviewWindow, y: f64) {
    let _ = w.set_position(tauri::PhysicalPosition::new(-12000.0, y));
}

/// The overlay copy of the meter: rendered off-screen, never focused, not in the taskbar.
pub const LABEL: &str = "overlay";
pub const WIDTH: f64 = 340.0;
pub const HEIGHT: f64 = 440.0;

/// `size`: the meter's size in CSS px as resized in the game (None: the meter window's).
pub fn open(app: &tauri::AppHandle, size: Option<(f64, f64)>) -> Result<tauri::WebviewWindow, String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(LABEL) {
        return Ok(w);
    }
    let w = tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("meter.html?overlay".into()))
        .additional_browser_args(BROWSER_ARGS)
        .title("Damage meter (in game)")
        .inner_size(size.map_or(WIDTH, |s| s.0), size.map_or(HEIGHT, |s| s.1))
        // far off-screen but shown: a hidden WebView2 stops painting, and a painted one is what gets captured
        .position(-8000.0, -8000.0)
        .decorations(false)
        .transparent(true)
        .skip_taskbar(true)
        .focused(false)
        .resizable(false)
        .build()
        .map_err(|e| e.to_string())?;
    off_screen(&w, -12000.0);
    Ok(w)
}

/// The breakdown tooltip (tip.html): `TIP` is a real window beside the pointer over the meter window, `TIP_GAME` an
/// off-screen copy captured like the overlay for the game, where the layer draws it beside the pointer.
pub const TIP: &str = "tip";
pub const TIP_GAME: &str = "tip-game";

pub fn open_tip(app: &tauri::AppHandle, label: &str) -> Result<tauri::WebviewWindow, String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(label) {
        return Ok(w);
    }
    let game = label == TIP_GAME;
    let w = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App("tip.html".into()))
        .additional_browser_args(BROWSER_ARGS)
        .title("Damage breakdown")
        .inner_size(300.0, 160.0)
        .position(if game { -8000.0 } else { 0.0 }, if game { -6000.0 } else { 0.0 })
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .resizable(false)
        .always_on_top(!game)
        // the window copy shows only while a bar is hovered; the game copy stays painted off-screen
        .visible(game)
        .build()
        .map_err(|e| e.to_string())?;
    if game {
        off_screen(&w, -11000.0);
    }
    Ok(w)
}

/// Ask the overlay webview for a PNG of what it shows; the bytes arrive on `tx` once WebView2 has written them.
#[cfg(windows)]
pub fn request(win: &tauri::WebviewWindow, tx: Sender<Vec<u8>>) {
    use webview2_com::CapturePreviewCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
    use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let _ = win.with_webview(move |pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else { return };
        let Ok(stream) = CreateStreamOnHGlobal(Default::default(), true) else { return };
        let read = stream.clone();
        let handler = CapturePreviewCompletedHandler::create(Box::new(move |r| {
            if r.is_ok() {
                if let Ok(h) = GetHGlobalFromStream(&read) {
                    let n = GlobalSize(h);
                    let p = GlobalLock(h) as *const u8;
                    if !p.is_null() && n > 0 {
                        let _ = tx.send(std::slice::from_raw_parts(p, n).to_vec());
                    }
                    let _ = GlobalUnlock(h);
                }
            }
            Ok(())
        }));
        let _ = core.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler);
    });
}

#[cfg(not(windows))]
pub fn request(_: &tauri::WebviewWindow, _: Sender<Vec<u8>>) {}

/// PNG bytes to (width, height, BGRA pixels with straight alpha).
pub fn decode(png_bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut d = png::Decoder::new(std::io::Cursor::new(png_bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let px = &buf[..info.buffer_size()];
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    match info.color_type {
        png::ColorType::Rgba => px.chunks_exact(4).for_each(|p| out.extend([p[2], p[1], p[0], p[3]])),
        png::ColorType::Rgb => px.chunks_exact(3).for_each(|p| out.extend([p[2], p[1], p[0], 255])),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).for_each(|p| out.extend([p[0], p[0], p[0], p[1]])),
        png::ColorType::Grayscale => px.iter().for_each(|&g| out.extend([g, g, g, 255])),
        png::ColorType::Indexed => return None,
    }
    Some((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rgba_and_rgb_png_to_bgra() {
        let encode = |color, px: &[u8]| {
            let mut out = vec![];
            let mut e = png::Encoder::new(&mut out, 2, 1);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(px).unwrap();
            out
        };
        let (w, h, px) = decode(&encode(png::ColorType::Rgba, &[255, 0, 0, 128, 0, 0, 255, 255])).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [0, 0, 255, 128, 255, 0, 0, 255], "BGRA, alpha kept");
        let (_, _, px) = decode(&encode(png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(px, [3, 2, 1, 255, 6, 5, 4, 255]);
    }
}

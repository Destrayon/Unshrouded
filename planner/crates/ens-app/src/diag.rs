//! Lag recorder: with ENS_LOG=1 (the "Record lag log" launcher sets it) every frame appends one line to
//! planner-log.txt next to the exe: time, gap since the previous frame, time slept by the frame cap, UI work, the
//! input that arrived, pointer and window position, and Windows' compositor counters (frames late / dropped /
//! missed), so a stutter can be matched to what the app and the compositor were doing at that moment.
//!
//! It also records the mouse straight from Windows (a low-level mouse hook on its own thread), so a gap between
//! frames can be told apart: moves that arrived during the gap mean the app was held up; none means it was idle.

use eframe::egui;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;
use std::time::Instant;

/// Mouse moves seen by the low-level hook: ms since the recorder started.
static MOUSE: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub struct Recorder {
    out: Option<BufWriter<File>>,
    start: Instant,
    last: Option<Instant>,
    events: String,
    dwm: Option<(u64, u64, u64, u64)>,
    last_end: Option<Instant>,
}

impl Recorder {
    pub fn new() -> Self {
        let on = std::env::var("ENS_LOG").is_ok() || std::env::var("ENS_TIMING").is_ok();
        let out = on
            .then(|| match std::env::var("ENS_LOGFILE") {
                Ok(f) => Some(std::path::PathBuf::from(f)),
                Err(_) => std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("planner-log.txt"))),
            })
            .flatten()
            .and_then(|p| File::create(p).ok())
            .map(BufWriter::new);
        let start = *START.get_or_init(Instant::now);
        let mut r = Recorder { out, start, last: None, events: String::new(), dwm: None, last_end: None };
        if r.out.is_some() {
            start_mouse_hook();
        }
        if let Some(o) = r.out.as_mut() {
            let _ = writeln!(o, "# Enshrouded Planner lag log. Columns: t_ms gap_ms slept_ms ui_ms prev (last frame build+draw, excluding present wait) after (end of last UI to this frame: drawing, presenting, idle) | mouse (hook: moves since last frame, ms of the first and last) | input | pointer | window | dwm: late dropped missed (since last frame) refreshes");
        }
        r
    }

    pub fn enabled(&self) -> bool {
        self.out.is_some()
    }

    /// Summarize the input egui is about to process this frame.
    pub fn input(&mut self, raw: &egui::RawInput) {
        if self.out.is_none() {
            return;
        }
        let mut counts: Vec<(String, u32)> = vec![];
        for e in &raw.events {
            let d = format!("{e:?}");
            let mut name: String = d.chars().take_while(|c| c.is_alphanumeric()).collect();
            if let egui::Event::PointerButton { pressed, .. } = e {
                name = if *pressed { "Press".into() } else { "Release".into() };
            }
            if let egui::Event::WindowFocused(f) = e {
                name = if *f { "Focus".into() } else { "Unfocus".into() };
            }
            match counts.iter_mut().find(|(n, _)| *n == name) {
                Some(c) => c.1 += 1,
                None => counts.push((name, 1)),
            }
        }
        self.events = counts.iter().map(|(n, c)| if *c > 1 { format!("{n}x{c}") } else { n.clone() }).collect::<Vec<_>>().join(",");
        if self.events.is_empty() {
            self.events = "-".into();
        }
    }

    pub fn frame(&mut self, frame_start: Instant, slept_ms: f64, ui_ms: f64, prev_frame_ms: f64, ctx: &egui::Context) {
        let Some(out) = self.out.as_mut() else { return };
        let t = frame_start.duration_since(self.start).as_secs_f64() * 1000.0;
        let gap = self.last.map(|l| frame_start.duration_since(l).as_secs_f64() * 1000.0).unwrap_or(0.0);
        self.last = Some(frame_start);
        let pointer = ctx.pointer_hover_pos().map(|p| format!("{:.0},{:.0}", p.x, p.y)).unwrap_or_else(|| "outside".into());
        let win = ctx.input(|i| i.viewport().outer_rect).map(|r| format!("{:.0},{:.0} {:.0}x{:.0}", r.left(), r.top(), r.width(), r.height())).unwrap_or_default();
        let dwm = match (dwm_counters(), self.dwm) {
            (Some(now), Some(before)) => {
                self.dwm = Some(now);
                format!("{} {} {} {}", now.0.saturating_sub(before.0), now.1.saturating_sub(before.1), now.2.saturating_sub(before.2), now.3.saturating_sub(before.3))
            }
            (Some(now), None) => {
                self.dwm = Some(now);
                "start".into()
            }
            _ => "n/a".into(),
        };
        let after = self.last_end.map(|e| frame_start.duration_since(e).as_secs_f64() * 1000.0).unwrap_or(0.0);
        let moves: Vec<f64> = MOUSE.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default();
        let mouse = match (moves.first(), moves.last()) {
            (Some(a), Some(b)) => format!("{} {:.1}..{:.1}", moves.len(), a, b),
            _ => "0".into(),
        };
        let _ = writeln!(out, "{t:.1} {gap:.1} {slept_ms:.1} {ui_ms:.2} prev {prev_frame_ms:.2} after {after:.1} | {mouse} | {} | {pointer} | {win} | {dwm}", self.events);
        self.last_end = Some(Instant::now());
        let _ = out.flush();
        self.events = "-".into();
    }
}

/// Low-level mouse hook on its own thread with its own message loop; the callback only timestamps the move.
#[cfg(target_os = "windows")]
fn start_mouse_hook() {
    #[repr(C)]
    struct Msg([u8; 64]);
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowsHookExW(id: i32, proc_: extern "system" fn(i32, usize, isize) -> isize, hmod: isize, thread: u32) -> isize;
        fn CallNextHookEx(hook: isize, code: i32, w: usize, l: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }
    extern "system" fn on_mouse(code: i32, w: usize, l: isize) -> isize {
        const WM_MOUSEMOVE: usize = 0x0200;
        if code >= 0 && w == WM_MOUSEMOVE {
            if let (Some(start), Ok(mut m)) = (START.get(), MOUSE.try_lock()) {
                if m.len() < 100_000 {
                    m.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        // SAFETY: passing the hook arguments on unchanged, as the hook chain requires
        unsafe { CallNextHookEx(0, code, w, l) }
    }
    std::thread::spawn(|| {
        const WH_MOUSE_LL: i32 = 14;
        // SAFETY: a process-local callback with the LowLevelMouseProc signature; this thread pumps its messages
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            if SetWindowsHookExW(WH_MOUSE_LL, on_mouse, module, 0) == 0 {
                return;
            }
            let mut msg = Msg([0; 64]);
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn start_mouse_hook() {}

/// Windows compositor counters: frames late, dropped, missed, and refreshes so far (DwmGetCompositionTimingInfo with
/// a null window = the whole desktop compositor).
#[cfg(target_os = "windows")]
fn dwm_counters() -> Option<(u64, u64, u64, u64)> {
    // DWM_TIMING_INFO, packed to 1 byte as in dwmapi.h
    #[repr(C, packed(1))]
    #[derive(Default, Clone, Copy)]
    struct Timing {
        cb_size: u32,
        rate_refresh: [u32; 2],
        qpc_refresh_period: u64,
        rate_compose: [u32; 2],
        qpc_vblank: u64,
        c_refresh: u64,
        c_dx_refresh: u32,
        qpc_compose: u64,
        c_frame: u64,
        c_dx_present: u32,
        c_refresh_frame: u64,
        c_frame_submitted: u64,
        c_dx_present_submitted: u32,
        c_frame_confirmed: u64,
        c_dx_present_confirmed: u32,
        c_refresh_confirmed: u64,
        c_dx_refresh_confirmed: u32,
        c_frames_late: u64,
        c_frames_outstanding: u32,
        c_frame_displayed: u64,
        qpc_frame_displayed: u64,
        c_refresh_frame_displayed: u64,
        c_frame_complete: u64,
        qpc_frame_complete: u64,
        c_frame_pending: u64,
        qpc_frame_pending: u64,
        c_frames_displayed: u64,
        c_frames_complete: u64,
        c_frames_pending: u64,
        c_frames_available: u64,
        c_frames_dropped: u64,
        c_frames_missed: u64,
        c_refresh_next_displayed: u64,
        c_refresh_next_presented: u64,
        c_refreshes_displayed: u64,
        c_refreshes_presented: u64,
        c_refresh_started: u64,
        c_pixels_received: u64,
        c_pixels_drawn: u64,
        c_buffers_empty: u64,
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmGetCompositionTimingInfo(hwnd: isize, info: *mut Timing) -> i32;
    }
    let mut t = Timing { cb_size: std::mem::size_of::<Timing>() as u32, ..Default::default() };
    // SAFETY: a correctly sized, writable DWM_TIMING_INFO; a null window asks for the desktop compositor's timing
    let hr = unsafe { DwmGetCompositionTimingInfo(0, &mut t) };
    if hr != 0 {
        return None;
    }
    let (late, dropped, missed, refresh) = (t.c_frames_late, t.c_frames_dropped, t.c_frames_missed, t.c_refresh);
    Some((late, dropped, missed, refresh))
}

#[cfg(not(target_os = "windows"))]
fn dwm_counters() -> Option<(u64, u64, u64, u64)> {
    None
}

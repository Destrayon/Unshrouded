//! Using the in-game meter (docs/plans/overlay-parity.md): the layer sees the game's keys and mouse while the game is
//! focused and sends what concerns the meter to the planner (UDP on 127.0.0.1, the port named in the frame), which
//! acts on its meter page. Hotkeys: F6 next tab, F7 see-through, F8 move, F9 hide/show. A left click or the wheel over
//! the meter goes to the meter, and the game does not get it (the window's mouse messages there are swallowed). Only
//! while the game shows a cursor (inventory, map, menus): in play, with the cursor hidden, every click is the game's.
//! Dragging the meter's title bar moves it (the layer draws it at the cursor at once and tells the planner where it was
//! left); dragging its bottom-right corner resizes it (the planner resizes the meter page). The pointer's position over
//! the meter goes too, so hovering a player's bar opens their breakdown.

/// Hotkeys and what they send.
pub const KEYS: [(u16, &str); 4] = [(0x75, "tab"), (0x76, "fade"), (0x77, "move"), (0x78, "hide")];

/// The meter's rectangle on the game's image, and the image's size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub image_w: u32,
    pub image_h: u32,
}

impl Rect {
    /// A point in the window's client area (client size cw × ch) as a point on the meter, if it is on it.
    pub fn hit(&self, px: i32, py: i32, cw: i32, ch: i32) -> Option<(i32, i32)> {
        if cw <= 0 || ch <= 0 {
            return None;
        }
        let x = (px as i64 * self.image_w as i64 / cw as i64) as i32 - self.x;
        let y = (py as i64 * self.image_h as i64 / ch as i64) as i32 - self.y;
        (x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h).then_some((x, y))
    }
}

/// What pressing the left button at a point of the meter does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grab {
    /// The title bar (except its buttons at the right): drag the meter.
    Move,
    /// The bottom-right corner: resize it.
    Resize,
    /// Anywhere else: a click on the page.
    Click,
}

/// The meter page's title bar is 30 CSS px high with a 32 px button strip at its right; the grip is 18 px square.
pub fn grab(x: i32, y: i32, w: u32, h: u32, scale: f32) -> Grab {
    let px = |css: f32| (css * scale).round() as i32;
    let (w, h) = (w as i32, h as i32);
    if x >= w - px(18.0) && y >= h - px(18.0) {
        Grab::Resize
    } else if y < px(30.0) && x < w - px(32.0) {
        Grab::Move
    } else {
        Grab::Click
    }
}

/// The smallest and largest meter, in CSS px.
pub const MIN_SIZE: (f32, f32) = (240.0, 180.0);

/// Rising edges: which of the watched keys went down since the last poll.
#[derive(Debug, Default)]
pub struct Edges {
    down: u32,
}

impl Edges {
    /// `now[k]`: whether watched key k is held now. Returns the keys that were up last time.
    pub fn update(&mut self, now: &[bool]) -> Vec<usize> {
        let mut out = vec![];
        let mut bits = 0;
        for (k, &d) in now.iter().enumerate() {
            if d {
                bits |= 1 << k;
                if self.down & (1 << k) == 0 {
                    out.push(k);
                }
            }
        }
        self.down = bits;
        out
    }
}

#[cfg(windows)]
pub use win::{cursor, poll, position, set_meter, size};

#[cfg(windows)]
mod win {
    use super::*;
    use std::net::UdpSocket;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    struct Drag {
        grab: Grab,
        /// Where the button went down, in image pixels, and the meter then.
        start: (i32, i32),
        rect: Rect,
        sent: std::time::Instant,
        /// The last frame that saw the button still down: a drag no frame has confirmed for 250 ms blocks nothing.
        seen: std::time::Instant,
    }

    impl Drag {
        fn live(&self) -> bool {
            self.seen.elapsed().as_millis() < 250
        }
    }

    struct State {
        hwnd: isize,
        rect: Option<Rect>,
        shown_at: Option<std::time::Instant>,
        port: u16,
        scale: f32,
        sock: Option<UdpSocket>,
        keys: Edges,
        drag: Option<Drag>,
        /// Where the meter was dragged to, drawn there until the planner's frames carry that place.
        moved: Option<(i32, i32)>,
        sent_place: Option<(f32, f32)>,
        /// The size being dragged to: the current picture is drawn stretched to it until the planner's re-rendered
        /// picture of that size arrives (or 1.5 s have passed).
        resized: Option<(u32, u32, std::time::Instant)>,
        /// The last pointer position sent (None: not over the meter), and when.
        hovered: Option<(i32, i32)>,
        hover_sent: Option<std::time::Instant>,
        /// The pointer on the game's image (image pixels) while the game is focused and shows its cursor.
        pointer: Option<(i32, i32)>,
    }

    static STATE: Mutex<State> = Mutex::new(State { hwnd: 0, rect: None, shown_at: None, port: 0, scale: 1.0, sock: None, keys: Edges { down: 0 }, drag: None, moved: None, sent_place: None, resized: None, hovered: None, hover_sent: None, pointer: None });
    static ORIGINAL: AtomicIsize = AtomicIsize::new(0);

    /// ENS_OVERLAY_LOG=<file>: what the layer sees and sends, for debugging.
    fn log(line: &str) {
        if let Some(p) = std::env::var_os("ENS_OVERLAY_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    fn send(s: &mut State, msg: &str) {
        log(&format!("send {msg:?} to port {}", s.port));
        if s.port == 0 {
            return;
        }
        if s.sock.is_none() {
            s.sock = UdpSocket::bind("127.0.0.1:0").ok();
        }
        if let Some(sock) = &s.sock {
            let _ = sock.send_to(msg.as_bytes(), ("127.0.0.1", s.port));
        }
    }

    fn client_size(hwnd: HWND) -> (i32, i32) {
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe { GetClientRect(hwnd, &mut r) };
        (r.right - r.left, r.bottom - r.top)
    }

    /// The meter as drawn this frame (None when it is not drawn), the size of the frame's picture, the planner's port and
    /// page scale from the frame, and the place the frame asks for.
    pub fn set_meter(rect: Option<Rect>, frame: (u32, u32), port: u16, scale: u16, place: crate::frame::Place) {
        let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((w, h, t)) = s.resized {
            if s.drag.is_none() && (frame.0.abs_diff(w) <= 2 && frame.1.abs_diff(h) <= 2 || t.elapsed().as_millis() > 1500) {
                s.resized = None;
            }
        }
        if rect.is_some() {
            s.shown_at = Some(std::time::Instant::now());
            s.rect = rect;
        }
        if port != 0 {
            s.port = port;
        }
        s.scale = if scale == 0 { 1.0 } else { scale as f32 / 100.0 };
        // the planner has taken the dragged place: draw where the frames say again
        if let Some((x, y)) = s.sent_place {
            if s.drag.is_none() && place.corner == 0 && (place.x - x).abs() < 0.002 && (place.y - y).abs() < 0.002 {
                (s.moved, s.sent_place) = (None, None);
            }
        }
    }

    /// The size to draw the meter at instead of its picture's while it is being resized (image pixels).
    pub fn size() -> Option<(u32, u32)> {
        STATE.lock().ok()?.resized.map(|(w, h, _)| (w, h))
    }

    /// The pointer on the game's image, for the tooltip beside it (None: hidden cursor or another window in front).
    pub fn cursor() -> Option<(i32, i32)> {
        STATE.lock().ok()?.pointer
    }

    /// Where to draw the meter instead of its frame's place while it is being dragged (top-left, image pixels).
    pub fn position() -> Option<(i32, i32)> {
        STATE.lock().ok()?.moved
    }

    /// Once per present: hook the game's window the first time, then send hotkeys and clicks on the meter.
    pub fn poll(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if s.hwnd != hwnd {
            s.hwnd = hwnd;
            // the window keeps calling into this DLL after the game destroys its Vulkan instance (when the loader
            // unloads layers): keep the DLL loaded until the process ends
            let mut module = std::ptr::null_mut();
            unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN, wndproc as *const () as *const u16, &mut module) };
            let old = unsafe { SetWindowLongPtrW(hwnd as HWND, GWLP_WNDPROC, wndproc as *const () as usize as isize) };
            ORIGINAL.store(old, Ordering::SeqCst);
            log(&format!("window {hwnd:#x}: wndproc hooked (previous {old:#x})"));
        }
        let h = hwnd as HWND;
        let fg = unsafe { GetForegroundWindow() };
        if fg != h && unsafe { GetAncestor(fg, GA_ROOT) } != h {
            s.keys.update(&[false; 5]);
            s.pointer = None;
            // a drag ends when the game loses focus, so it can never hold the game's clicks afterwards
            s.drag = None;
            return;
        }
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
        let mut now: Vec<bool> = KEYS.iter().map(|(vk, _)| held(*vk)).collect();
        let button = held(0x01);
        now.push(button);
        let pressed = s.keys.update(&now);
        let mut p = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut p);
            ScreenToClient(h, &mut p);
        }
        let (cw, ch) = client_size(h);
        s.pointer = s.rect.filter(|_| cw > 0 && ch > 0 && cursor_visible())
            .map(|r| (p.x * r.image_w as i32 / cw, p.y * r.image_h as i32 / ch));
        for k in pressed {
            if k < KEYS.len() {
                send(&mut s, KEYS[k].1);
            } else if let Some(r) = s.rect.filter(|_| showing(&s) && cursor_visible()) {
                if let Some((x, y)) = r.hit(p.x, p.y, cw, ch) {
                    let g = grab(x, y, r.w, r.h, s.scale);
                    if g == Grab::Click {
                        send(&mut s, &format!("click {x} {y}"));
                    } else {
                        let start = (r.x + x, r.y + y);
                        s.drag = Some(Drag { grab: g, start, rect: r, sent: std::time::Instant::now(), seen: std::time::Instant::now() });
                    }
                }
            }
        }
        // the pointer over the meter (with a visible cursor), at most every 50 ms, and once when it leaves
        let over = s.rect.filter(|_| showing(&s) && s.drag.is_none() && cursor_visible()).and_then(|r| r.hit(p.x, p.y, cw, ch));
        if over != s.hovered && (over.is_none() || s.hover_sent.is_none_or(|t| t.elapsed().as_millis() >= 50)) {
            match over {
                Some((x, y)) => send(&mut s, &format!("hover {x} {y}")),
                None => send(&mut s, "hover -1 -1"),
            }
            (s.hovered, s.hover_sent) = (over, Some(std::time::Instant::now()));
        }
        // the cursor hidden (back in play) ends a drag as letting go would
        if s.drag.is_some() && !cursor_visible() {
            s.drag = None;
        }
        if let (Some(d), true) = (s.drag.as_mut(), button) {
            d.seen = std::time::Instant::now();
        }
        let Some(d) = s.drag.as_ref().map(|d| (d.grab, d.start, d.rect, d.sent)) else { return };
        let (g, start, r, sent) = d;
        let (ix, iy) = if cw > 0 && ch > 0 { (p.x * r.image_w as i32 / cw, p.y * r.image_h as i32 / ch) } else { start };
        let (dx, dy) = (ix - start.0, iy - start.1);
        match g {
            Grab::Move => {
                let x = (r.x + dx).clamp(0, r.image_w as i32 - r.w as i32);
                let y = (r.y + dy).clamp(0, r.image_h as i32 - r.h as i32);
                s.moved = Some((x, y));
                if !button {
                    let place = (x as f32 / r.image_w as f32, y as f32 / r.image_h as f32);
                    s.sent_place = Some(place);
                    send(&mut s, &format!("moveto {:.5} {:.5}", place.0, place.1));
                }
            }
            Grab::Resize => {
                let (min_w, min_h) = ((MIN_SIZE.0 * s.scale) as i32, (MIN_SIZE.1 * s.scale) as i32);
                let w = (r.w as i32 + dx).clamp(min_w, (r.image_w as i32 - r.x).max(min_w));
                let h = (r.h as i32 + dy).clamp(min_h, (r.image_h as i32 - r.y).max(min_h));
                s.resized = Some((w as u32, h as u32, std::time::Instant::now()));
                if !button || sent.elapsed().as_millis() >= 60 {
                    send(&mut s, &format!("size {w} {h}"));
                    if let Some(d) = s.drag.as_mut() {
                        d.sent = std::time::Instant::now();
                    }
                }
            }
            Grab::Click => {}
        }
        if !button {
            s.drag = None;
        }
    }

    /// Whether the game shows a mouse cursor: in play it hides it (or sets none) and the mouse turns the camera.
    fn cursor_visible() -> bool {
        let mut ci: CURSORINFO = unsafe { std::mem::zeroed() };
        ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        (unsafe { GetCursorInfo(&mut ci) }) != 0 && ci.flags & CURSOR_SHOWING != 0 && !ci.hCursor.is_null()
    }

    fn showing(s: &State) -> bool {
        s.shown_at.is_some_and(|t| t.elapsed().as_millis() < 300)
    }

    /// Mouse messages over the meter do not reach the game; the wheel there scrolls the meter.
    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        const BUTTONS: [u32; 6] = [WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK];
        if BUTTONS.contains(&m) || m == WM_MOUSEWHEEL {
            let mut p = POINT { x: (l & 0xffff) as i16 as i32, y: ((l >> 16) & 0xffff) as i16 as i32 };
            if m == WM_MOUSEWHEEL {
                unsafe { ScreenToClient(h, &mut p) };
            }
            let (cw, ch) = client_size(h);
            if let Ok(mut s) = STATE.try_lock() {
                // while dragging the meter, the game gets no clicks wherever the cursor is; only a drag a frame confirmed
                // in the last 250 ms with the cursor showing, so a drag that was never let go cannot keep the game's clicks
                if s.drag.as_ref().is_some_and(|d| d.live()) && m != WM_MOUSEWHEEL && cursor_visible() {
                    return 0;
                }
                if let Some((x, y)) = s.rect.filter(|_| showing(&s) && cursor_visible()).and_then(|r| r.hit(p.x, p.y, cw, ch)) {
                    if m == WM_MOUSEWHEEL {
                        let dy = -((w >> 16) as i16 as i32) * 40 / 120;
                        send(&mut s, &format!("wheel {x} {y} {dy}"));
                    }
                    return 0;
                }
            }
        }
        let orig = ORIGINAL.load(Ordering::SeqCst);
        if orig == 0 {
            return unsafe { DefWindowProcW(h, m, w, l) };
        }
        unsafe { CallWindowProcW(Some(std::mem::transmute::<isize, unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>(orig)), h, m, w, l) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_window_points_onto_the_meter_and_finds_key_presses() {
        // meter at 31,288 on a 2560 × 1440 image shown in a 1280 × 720 window
        let r = Rect { x: 31, y: 288, w: 340, h: 440, image_w: 2560, image_h: 1440 };
        assert_eq!(r.hit(20, 150, 1280, 720), Some((9, 12)));
        assert_eq!(r.hit(10, 150, 1280, 720), None, "left of it");
        assert_eq!(r.hit(200, 150, 1280, 720), None, "right of it");
        assert_eq!(r.hit(20, 150, 0, 0), None);
        assert_eq!(grab(100, 10, 340, 440, 1.0), Grab::Move, "title bar");
        assert_eq!(grab(320, 10, 340, 440, 1.0), Grab::Click, "the see-through button in the title bar");
        assert_eq!(grab(335, 435, 340, 440, 1.0), Grab::Resize, "corner");
        assert_eq!(grab(100, 200, 340, 440, 1.0), Grab::Click);
        assert_eq!(grab(100, 34, 425, 550, 1.25), Grab::Move, "title bar is 37 px at 125 %");
        let mut e = Edges::default();
        assert_eq!(e.update(&[false, true, false]), vec![1]);
        assert_eq!(e.update(&[false, true, false]), Vec::<usize>::new(), "held: once");
        assert_eq!(e.update(&[true, false, false]), vec![0]);
        assert_eq!(e.update(&[false, true, false]), vec![1], "pressed again");
    }
}

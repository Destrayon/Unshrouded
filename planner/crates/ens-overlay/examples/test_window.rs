//! Live check of the layer without the game (docs/plans/game-overlay.md phase 5, overlay-parity.md phase 1): a Vulkan
//! window that clears every frame to blue and presents it, with ENS_OVERLAY_ANY=1 so the installed layer draws here
//! too, from a test frame (ENS_OVERLAY_FRAME): 200 × 100 px, the top half opaque red, the bottom half green at half
//! opacity, placed 5 % / 10 % from the top-right corner. After two seconds it captures its own window from the screen
//! and checks red, the green-blue blend and the untouched blue around it.
//!
//! Needs the layer installed (`ens overlay install`).
//!
//! `--live <prefix>`: the planner's real meter instead (run the planner with ENS_OVERLAY_FORCE=1 so it draws without
//! the game). After 2 s it presses F6 (next tab), after 4 s it clicks the meter's Taken tab, after 5 s it drags the
//! title bar 150 px right and 60 px down, after 6.5 s it drags the corner 80 px left and 60 px up, and it saves a screen
//! capture before and after each as <prefix>-0…4.bmp, for the input path layer → planner → meter page.
//! `cargo run --release -p ens-overlay --example test_window [shot.bmp]`; exit code 0 when the meter is on screen.
//! `--rgba` uses the game's swapchain format (R8G8B8A8_UNORM, enshrouded.log: "swapchain backbuffer format is
//! 'R8G8B8A8_unorm (37)'") instead of B8G8R8A8_UNORM. With `--pass` it leaves ENS_OVERLAY_ANY unset, as any other program would, and passes when the window is untouched.

use ash::vk;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const W: u32 = 800;
const H: u32 = 600;
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn pump() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn main() {
    // the layer reads this once, at the first Vulkan call
    let pass = std::env::args().any(|a| a == "--pass");
    if !pass {
        unsafe { std::env::set_var("ENS_OVERLAY_ANY", "1") };
    }
    let want = if std::env::args().any(|a| a == "--rgba") { vk::Format::R8G8B8A8_UNORM } else { vk::Format::B8G8R8A8_UNORM };
    let shot = std::env::args().skip(1).find(|a| !a.starts_with("--"));
    let live = std::env::args().skip_while(|a| a != "--live").nth(1);
    let test_frame = std::env::temp_dir().join(format!("ens-overlay-test-{}.bin", std::process::id()));
    if live.is_none() {
        std::fs::write(&test_frame, test_frame_bytes()).expect("test frame");
        unsafe { std::env::set_var("ENS_OVERLAY_FRAME", &test_frame) };
    }
    unsafe { SetProcessDPIAware() };
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = wide("ens_overlay_test");
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..unsafe { std::mem::zeroed() } };
    unsafe { RegisterClassW(&wc) };
    let mut r = RECT { left: 0, top: 0, right: W as i32, bottom: H as i32 };
    unsafe { AdjustWindowRect(&mut r, WS_OVERLAPPEDWINDOW, 0) };
    let title = wide("ens-overlay test window");
    let hwnd = unsafe {
        CreateWindowExW(WS_EX_TOPMOST, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 60, 60, r.right - r.left, r.bottom - r.top,
            std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null())
    };
    assert!(!hwnd.is_null(), "CreateWindowExW failed");
    unsafe { SetForegroundWindow(hwnd) };

    let entry = unsafe { ash::Entry::load() }.expect("vulkan-1.dll");
    let layers = unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
    let ours = layers.iter().any(|l| l.layer_name_as_c_str().is_ok_and(|n| n == c"VK_LAYER_ENS_meter"));
    println!("VK_LAYER_ENS_meter {}", if ours { "is listed by the loader" } else { "is NOT listed (run: ens overlay install)" });
    let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
    let exts = [ash::khr::surface::NAME.as_ptr(), ash::khr::win32_surface::NAME.as_ptr()];
    let instance = unsafe { entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&exts), None) }.expect("instance");
    let surf_i = ash::khr::surface::Instance::new(&entry, &instance);
    let win32 = ash::khr::win32_surface::Instance::new(&entry, &instance);
    let surface = unsafe { win32.create_win32_surface(&vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinst as isize).hwnd(hwnd as isize), None) }.expect("surface");
    let (pd, family) = unsafe { instance.enumerate_physical_devices() }.expect("devices").into_iter().find_map(|pd| {
        let fams = unsafe { instance.get_physical_device_queue_family_properties(pd) };
        fams.iter().enumerate().find(|(i, f)| f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
            && unsafe { surf_i.get_physical_device_surface_support(pd, *i as u32, surface) }.unwrap_or(false)).map(|(i, _)| (pd, i as u32))
    }).expect("a device that presents to the window");
    let prio = [1.0];
    let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
    let dext = [ash::khr::swapchain::NAME.as_ptr()];
    let device = unsafe { instance.create_device(pd, &vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_extension_names(&dext), None) }.expect("device");
    let queue = unsafe { device.get_device_queue(family, 0) };
    let sc_d = ash::khr::swapchain::Device::new(&instance, &device);
    let caps = unsafe { surf_i.get_physical_device_surface_capabilities(pd, surface) }.expect("caps");
    let formats = unsafe { surf_i.get_physical_device_surface_formats(pd, surface) }.expect("formats");
    let fmt = formats.iter().find(|f| f.format == want).copied().unwrap_or(formats[0]);
    println!("swapchain format {:?}, extent {}x{}", fmt.format, caps.current_extent.width, caps.current_extent.height);
    let sci = vk::SwapchainCreateInfoKHR::default().surface(surface).min_image_count(caps.min_image_count.max(2)).image_format(fmt.format)
        .image_color_space(fmt.color_space).image_extent(caps.current_extent).image_array_layers(1)
        // the app itself asks only for TRANSFER_DST for its clear; the layer adds what it needs either way
        .image_usage(vk::ImageUsageFlags::TRANSFER_DST).pre_transform(caps.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE).present_mode(vk::PresentModeKHR::FIFO).clipped(true);
    let swapchain = unsafe { sc_d.create_swapchain(&sci, None) }.expect("swapchain");
    let images = unsafe { sc_d.get_swapchain_images(swapchain) }.expect("images");
    let pool = unsafe { device.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None) }.unwrap();
    let cmd = unsafe { device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1)) }.unwrap()[0];
    let acquired = unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.unwrap();
    let rendered = unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.unwrap();
    let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None) }.unwrap();
    let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);

    let t0 = std::time::Instant::now();
    let mut frames = 0;
    let mut step = 0;
    let run_ms = if live.is_some() { 9000 } else { 2500 };
    while t0.elapsed().as_millis() < run_ms {
        pump();
        if let Some(prefix) = &live {
            let ms = t0.elapsed().as_millis();
            if (step == 0 && ms > 1900) || (step == 2 && ms > 3900) || (step == 4 && ms > 4900) || (step == 6 && ms > 6400) || (step == 8 && ms > 8700) {
                let px = capture(hwnd, caps.current_extent.width, caps.current_extent.height);
                save_bmp(&format!("{prefix}-{}.bmp", step / 2), &px, caps.current_extent.width);
                step += 1;
            }
            if step == 1 && ms > 2000 {
                unsafe { SetForegroundWindow(hwnd) };
                println!("foreground is the test window: {}", unsafe { GetForegroundWindow() } == hwnd);
                // from another thread, as a player would: the render loop keeps presenting meanwhile
                std::thread::spawn(|| key(0x75));
                step += 1;
            }
            if step == 3 && ms > 4000 {
                let (h, e) = (hwnd as usize, caps.current_extent);
                std::thread::spawn(move || click_taken(h as HWND, e));
                step += 1;
            }
            if step == 5 && ms > 5000 {
                let (h, e) = (hwnd as usize, caps.current_extent);
                std::thread::spawn(move || drag(h as HWND, e, false, 150, 60));
                step += 1;
            }
            if step == 7 && ms > 6500 {
                let (h, e) = (hwnd as usize, caps.current_extent);
                std::thread::spawn(move || drag(h as HWND, e, true, -80, -60));
                step += 1;
            }
        }
        unsafe {
            device.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            device.reset_fences(&[fence]).unwrap();
            let (index, _) = sc_d.acquire_next_image(swapchain, u64::MAX, acquired, vk::Fence::null()).expect("acquire");
            let image = images[index as usize];
            device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).unwrap();
            device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).unwrap();
            let b = |old, new, src, dst| vk::ImageMemoryBarrier::default().old_layout(old).new_layout(new).src_access_mask(src).dst_access_mask(dst)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED).image(image).subresource_range(range);
            device.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[],
                &[b(vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_WRITE)]);
            device.cmd_clear_color_image(cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearColorValue { float32: BLUE }, &[range]);
            device.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[],
                &[b(vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::empty())]);
            device.end_command_buffer(cmd).unwrap();
            let waits = [acquired];
            let stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [cmd];
            let signals = [rendered];
            device.queue_submit(queue, &[vk::SubmitInfo::default().wait_semaphores(&waits).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signals)], fence).unwrap();
            let scs = [swapchain];
            let idx = [index];
            sc_d.queue_present(queue, &vk::PresentInfoKHR::default().wait_semaphores(&signals).swapchains(&scs).image_indices(&idx)).expect("present");
        }
        frames += 1;
    }
    println!("{frames} frames presented");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let px = capture(hwnd, caps.current_extent.width, caps.current_extent.height);
    let ok = live.is_some() || check(&px, caps.current_extent.width, shot.as_deref(), !pass);
    let _ = std::fs::remove_file(&test_frame);

    unsafe {
        device.device_wait_idle().unwrap();
        device.destroy_fence(fence, None);
        device.destroy_semaphore(acquired, None);
        device.destroy_semaphore(rendered, None);
        device.destroy_command_pool(pool, None);
        eprintln!("cleanup: swapchain");
        sc_d.destroy_swapchain(swapchain, None);
        eprintln!("cleanup: device");
        device.destroy_device(None);
        eprintln!("cleanup: surface");
        surf_i.destroy_surface(surface, None);
        eprintln!("cleanup: instance");
        instance.destroy_instance(None);
        eprintln!("cleanup: window");
        DestroyWindow(hwnd);
    }
    if live.is_some() {
        if let Some(f) = ens_overlay::install::installed_dir().and_then(|d| std::fs::read(d.join(ens_overlay::frame::FILE)).ok()).and_then(|b| ens_overlay::frame::Frame::decode(&b)) {
            let o = ens_overlay::draw::place(f.width, f.height, caps.current_extent, f.place);
            println!("meter now {}x{} at {:?} (place {:?}, scale {})", f.width, f.height, o.map(|o| (o.x, o.y)), f.place, f.scale);
        }
    }
    println!("done, ok {ok}");
    std::process::exit(if ok { 0 } else { 1 });
}

/// The window's client area as it is on screen, BGRA rows top to bottom.
fn capture(hwnd: HWND, w: u32, h: u32) -> Vec<u8> {
    let mut origin = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(hwnd, &mut origin) };
    let mut out = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
        let old = SelectObject(mem, bmp);
        BitBlt(mem, 0, 0, w as i32, h as i32, screen, origin.x, origin.y, SRCCOPY);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w as i32;
        bi.bmiHeader.biHeight = -(h as i32);
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        GetDIBits(mem, bmp, 0, h, out.as_mut_ptr() as *mut c_void, &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
    }
    out
}

fn key(vk: u8) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};
    unsafe {
        keybd_event(vk, 0, 0, 0);
        std::thread::sleep(std::time::Duration::from_millis(60));
        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// Click the meter's Taken tab: the fourth of four tabs, 114 px down the meter page (meter.css at 100 %).
fn click_taken(hwnd: HWND, extent: vk::Extent2D) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP};
    let Some(frame) = ens_overlay::install::installed_dir()
        .and_then(|d| std::fs::read(d.join(ens_overlay::frame::FILE)).ok())
        .and_then(|b| ens_overlay::frame::Frame::decode(&b)) else {
        println!("no live frame to click on");
        return;
    };
    let o = ens_overlay::draw::place(frame.width, frame.height, extent, frame.place).expect("fits");
    let mut p = POINT { x: o.x + (frame.width as i32 * 7 / 8), y: o.y + 114 };
    unsafe {
        ClientToScreen(hwnd, &mut p);
        SetForegroundWindow(hwnd);
        SetCursorPos(p.x, p.y);
        std::thread::sleep(std::time::Duration::from_millis(80));
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        std::thread::sleep(std::time::Duration::from_millis(60));
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    let mut c = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(hwnd, &mut c) };
    let mut now = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut now) };
    println!("clicked at screen {},{} (client origin {},{}, cursor now {},{}; meter at {},{}, port {}; foreground {})", p.x, p.y, c.x, c.y, now.x, now.y, o.x, o.y, frame.port, unsafe { GetForegroundWindow() } == hwnd);
}

/// Drag the meter's title bar (or with `corner`, its bottom-right corner) by dx, dy client pixels, in steps.
fn drag(hwnd: HWND, extent: vk::Extent2D, corner: bool, dx: i32, dy: i32) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP};
    let Some(frame) = ens_overlay::install::installed_dir()
        .and_then(|d| std::fs::read(d.join(ens_overlay::frame::FILE)).ok())
        .and_then(|b| ens_overlay::frame::Frame::decode(&b)) else { return };
    let o = ens_overlay::draw::place(frame.width, frame.height, extent, frame.place).expect("fits");
    let (sx, sy) = if corner { (o.x + frame.width as i32 - 5, o.y + frame.height as i32 - 5) } else { (o.x + 100, o.y + 12) };
    let mut p = POINT { x: sx, y: sy };
    unsafe {
        ClientToScreen(hwnd, &mut p);
        SetCursorPos(p.x, p.y);
        std::thread::sleep(std::time::Duration::from_millis(80));
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        for k in 1..=10 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            SetCursorPos(p.x + dx * k / 10, p.y + dy * k / 10);
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    println!("dragged {} from {sx},{sy} by {dx},{dy} (meter {}x{} at {},{})", if corner { "the corner" } else { "the title" }, frame.width, frame.height, o.x, o.y);
}

fn save_bmp(path: &str, px: &[u8], w: u32) {
    let h = px.len() as u32 / 4 / w;
    let mut bmp = vec![];
    bmp.extend(b"BM");
    bmp.extend((54 + px.len() as u32).to_le_bytes());
    bmp.extend([0u8; 4]);
    bmp.extend(54u32.to_le_bytes());
    bmp.extend(40u32.to_le_bytes());
    bmp.extend((w as i32).to_le_bytes());
    bmp.extend((-(h as i32)).to_le_bytes());
    bmp.extend(1u16.to_le_bytes());
    bmp.extend(32u16.to_le_bytes());
    bmp.extend([0u8; 24]);
    bmp.extend(px);
    let _ = std::fs::write(path, bmp);
}

const PLACE: ens_overlay::frame::Place = ens_overlay::frame::Place { corner: 1, x: 0.05, y: 0.1 };

fn test_frame_bytes() -> Vec<u8> {
    let mut f = ens_overlay::frame::Frame::new(200, 100);
    f.seq = 1;
    f.unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    f.place = PLACE;
    for (i, p) in f.pixels.chunks_mut(4).enumerate() {
        // BGRA
        p.copy_from_slice(if i / 200 < 50 { &[0, 0, 255, 255] } else { &[0, 255, 0, 128] });
    }
    f.encode()
}

/// Check the test frame on screen: opaque red, half-transparent green over blue, blue around it.
fn check(px: &[u8], w: u32, shot: Option<&str>, expect_meter: bool) -> bool {
    let at = |x: u32, y: u32| [px[((y * w + x) * 4) as usize], px[((y * w + x) * 4 + 1) as usize], px[((y * w + x) * 4 + 2) as usize]];
    let h = px.len() as u32 / 4 / w;
    if let Some(path) = shot {
        let mut bmp = vec![];
        bmp.extend(b"BM");
        bmp.extend((54 + px.len() as u32).to_le_bytes());
        bmp.extend([0u8; 4]);
        bmp.extend(54u32.to_le_bytes());
        bmp.extend(40u32.to_le_bytes());
        bmp.extend((w as i32).to_le_bytes());
        bmp.extend((-(h as i32)).to_le_bytes());
        bmp.extend(1u16.to_le_bytes());
        bmp.extend(32u16.to_le_bytes());
        bmp.extend([0u8; 24]);
        bmp.extend(px);
        let _ = std::fs::write(path, bmp);
        println!("screen capture saved to {path}");
    }
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 8);
    let o = ens_overlay::draw::place(200, 100, vk::Extent2D { width: w, height: h }, PLACE).expect("fits");
    let (x0, y0) = (o.x as u32, o.y as u32);
    let (top, bottom, outside) = (at(x0 + 100, y0 + 25), at(x0 + 100, y0 + 75), at(x0 - 10, y0 + 50));
    println!("meter at {x0},{y0}: top {top:?}, bottom {bottom:?}, beside it {outside:?} (BGR)");
    const BLUE: [u8; 3] = [255, 0, 0];
    let ok = if expect_meter {
        near(top, [0, 0, 255]) && near(bottom, [127, 128, 0]) && near(outside, BLUE)
    } else {
        near(top, BLUE) && near(bottom, BLUE) && near(outside, BLUE)
    };
    println!("{}", match (ok, expect_meter) {
        (true, true) => "PASS: the layer blended the meter into the window's frames at its place",
        (true, false) => "PASS: outside the game the layer left the window untouched",
        _ => "FAIL",
    });
    ok
}

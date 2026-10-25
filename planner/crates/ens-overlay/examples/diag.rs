//! Diagnostic DLL (docs/plans/planner-loader.md): loaded into the running game, it writes diag.txt next to itself with
//! what the Vulkan loader and the registry look like from inside that process: the per-user and machine implicit layer
//! lists, the layers vulkan-1.dll enumerates now, and the Vulkan-related environment. Reads only.
//! Build: `cargo build --release -p ens-overlay --example diag`, then load it with the `inject_dll` example.

use std::ffi::c_void;
use std::fmt::Write as _;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Registry::*;

static mut ME: HMODULE = std::ptr::null_mut();

fn values(root: HKEY, key: &str) -> String {
    let k: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegOpenKeyExW(root, k.as_ptr(), 0, KEY_READ, &mut h) };
    if r != ERROR_SUCCESS {
        return format!("  (open failed: {r})\n");
    }
    let mut out = String::new();
    for i in 0.. {
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        let (mut ty, mut data, mut size) = (0u32, 0u32, 4u32);
        let r = unsafe { RegEnumValueW(h, i, name.as_mut_ptr(), &mut len, std::ptr::null(), &mut ty, &mut data as *mut u32 as *mut u8, &mut size) };
        if r != ERROR_SUCCESS {
            break;
        }
        let _ = writeln!(out, "  {} type {ty} value {data}", String::from_utf16_lossy(&name[..len as usize]));
    }
    unsafe { RegCloseKey(h) };
    out
}

fn subkeys(root: HKEY, key: &str) -> String {
    let k: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let mut h: HKEY = std::ptr::null_mut();
    let r = unsafe { RegOpenKeyExW(root, k.as_ptr(), 0, KEY_READ, &mut h) };
    if r != ERROR_SUCCESS {
        return format!("(open failed: {r})");
    }
    let mut out = vec![];
    for i in 0.. {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        if unsafe { RegEnumKeyExW(h, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) } != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    unsafe { RegCloseKey(h) };
    format!("{} keys: {}", out.len(), out.iter().filter(|n| n.starts_with('K') || n.starts_with('V') || n.starts_with('k')).cloned().collect::<Vec<_>>().join(", "))
}

#[repr(C)]
struct LayerProperties {
    name: [u8; 256],
    spec: u32,
    impl_version: u32,
    description: [u8; 256],
}

fn diag() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "exe {:?}", std::env::current_exe().ok());
    let _ = writeln!(s, "HKCU ImplicitLayers:\n{}", values(HKEY_CURRENT_USER, r"SOFTWARE\Khronos\Vulkan\ImplicitLayers"));
    let _ = writeln!(s, "HKLM ImplicitLayers:\n{}", values(HKEY_LOCAL_MACHINE, r"SOFTWARE\Khronos\Vulkan\ImplicitLayers"));
    let _ = writeln!(s, "HKCU Software subkeys with K/V: {}", subkeys(HKEY_CURRENT_USER, r"SOFTWARE"));
    let _ = writeln!(s, "HKCU SOFTWARE/Khronos subkeys: {}", subkeys(HKEY_CURRENT_USER, r"SOFTWARE\Khronos"));
    let mut cu: HKEY = std::ptr::null_mut();
    let r = unsafe { RegOpenCurrentUser(KEY_READ, &mut cu) };
    let _ = writeln!(s, "RegOpenCurrentUser: {r}; ImplicitLayers through it:\n{}", if r == ERROR_SUCCESS { values(cu, r"SOFTWARE\Khronos\Vulkan\ImplicitLayers") } else { String::new() });
    let _ = writeln!(s, "HKCU LoaderSettings:\n{}", values(HKEY_CURRENT_USER, r"SOFTWARE\Khronos\Vulkan\LoaderSettings"));
    let vk: Vec<u16> = "vulkan-1.dll\0".encode_utf16().collect();
    let m = unsafe { GetModuleHandleW(vk.as_ptr()) };
    if let Some(f) = unsafe { GetProcAddress(m, c"vkEnumerateInstanceLayerProperties".as_ptr() as _) } {
        let f: unsafe extern "system" fn(*mut u32, *mut LayerProperties) -> i32 = unsafe { std::mem::transmute(f) };
        let mut n = 0u32;
        unsafe { f(&mut n, std::ptr::null_mut()) };
        let mut v: Vec<LayerProperties> = (0..n).map(|_| unsafe { std::mem::zeroed() }).collect();
        let r = unsafe { f(&mut n, v.as_mut_ptr()) };
        let _ = writeln!(s, "vkEnumerateInstanceLayerProperties now: result {r}, {n} layers");
        for l in &v[..n as usize] {
            let end = l.name.iter().position(|&c| c == 0).unwrap_or(256);
            let _ = writeln!(s, "  {}", String::from_utf8_lossy(&l.name[..end]));
        }
    } else {
        let _ = writeln!(s, "vulkan-1.dll not loaded");
    }
    let _ = writeln!(s, "environment:");
    for (k, v) in std::env::vars_os() {
        let k = k.to_string_lossy().to_string();
        let up = k.to_ascii_uppercase();
        if ["VK_", "VULKAN", "LAYER", "DISABLE", "ENS_"].iter().any(|t| up.contains(t)) {
            let _ = writeln!(s, "  {k}={}", v.to_string_lossy());
        }
    }
    s
}

fn run() {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(ME, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])).with_file_name("diag.txt");
    let _ = std::fs::write(path, diag());
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut c_void) -> i32 {
    if reason == 1 {
        unsafe { ME = module };
        std::thread::spawn(run);
    }
    1
}

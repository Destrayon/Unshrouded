//! Registering the layer for the current Windows user: a manifest next to the DLL, named by a DWORD 0 value under
//! HKCU\Software\Khronos\Vulkan\ImplicitLayers (the Vulkan loader's per-user implicit layer list). Removing the value
//! turns the layer off; the files can stay.

use std::path::{Path, PathBuf};

pub const LAYER: &str = "VK_LAYER_ENS_meter";
pub const DLL: &str = "ens_overlay.dll";
pub const MANIFEST: &str = "VK_LAYER_ENS_meter.json";
const KEY: &str = r"Software\Khronos\Vulkan\ImplicitLayers";

/// The layer manifest. The library path is relative to the manifest, so the folder can move as a whole.
pub fn manifest() -> String {
    serde_like(&[
        ("name", LAYER),
        ("type", "GLOBAL"),
        ("library_path", &format!(".\\\\{DLL}")),
        ("api_version", "1.3.0"),
        ("implementation_version", "1"),
        ("description", "Unshrouded damage meter, drawn in enshrouded.exe only"),
    ])
}

fn serde_like(fields: &[(&str, &str)]) -> String {
    let mut s = String::from("{\n  \"file_format_version\": \"1.0.0\",\n  \"layer\": {\n");
    for (k, v) in fields {
        s += &format!("    \"{k}\": \"{v}\",\n");
    }
    s += "    \"disable_environment\": { \"ENS_OVERLAY_DISABLE\": \"1\" }\n  }\n}\n";
    s
}

/// Write the manifest into `dir` (which must hold the DLL) and register it. Returns the manifest path.
pub fn install(dir: &Path) -> Result<PathBuf, String> {
    if !dir.join(DLL).is_file() {
        return Err(format!("{} is missing", dir.join(DLL).display()));
    }
    let path = dir.join(MANIFEST);
    std::fs::write(&path, manifest()).map_err(|e| format!("{}: {e}", path.display()))?;
    let path = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    // canonicalize gives a \\?\ path, which the loader does not read
    let plain = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
    for old in registered() {
        if old != plain {
            reg::delete(KEY, &old.to_string_lossy());
        }
    }
    reg::set_dword(KEY, &plain.to_string_lossy(), 0)?;
    Ok(plain)
}

/// Remove every registration of this layer. Returns the manifests that were registered.
pub fn uninstall() -> Vec<PathBuf> {
    let old = registered();
    for p in &old {
        reg::delete(KEY, &p.to_string_lossy());
    }
    old
}

/// Manifests of this layer registered for the current user.
pub fn registered() -> Vec<PathBuf> {
    reg::values(KEY).into_iter().filter(|(name, v)| *v == 0 && name.to_ascii_lowercase().ends_with(&MANIFEST.to_ascii_lowercase())).map(|(n, _)| PathBuf::from(n)).collect()
}

/// The folder of the installed layer (where the planner writes overlay.bin), when it is installed and present.
pub fn installed_dir() -> Option<PathBuf> {
    registered().into_iter().find(|p| p.is_file()).and_then(|p| p.parent().map(Path::to_path_buf))
}

#[cfg(windows)]
mod reg {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn open(key: &str, create: bool) -> Option<HKEY> {
        let mut h: HKEY = std::ptr::null_mut();
        let k = wide(key);
        let r = unsafe {
            if create {
                RegCreateKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, std::ptr::null(), 0, KEY_READ | KEY_WRITE, std::ptr::null(), &mut h, std::ptr::null_mut())
            } else {
                RegOpenKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, KEY_READ | KEY_WRITE, &mut h)
            }
        };
        (r == ERROR_SUCCESS).then_some(h)
    }

    pub fn set_dword(key: &str, name: &str, v: u32) -> Result<(), String> {
        let h = open(key, true).ok_or_else(|| format!("cannot open HKCU\\{key}"))?;
        let n = wide(name);
        let r = unsafe { RegSetValueExW(h, n.as_ptr(), 0, REG_DWORD, &v as *const u32 as *const u8, 4) };
        unsafe { RegCloseKey(h) };
        if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("writing HKCU\\{key} failed ({r})")) }
    }

    pub fn delete(key: &str, name: &str) {
        if let Some(h) = open(key, false) {
            let n = wide(name);
            unsafe {
                RegDeleteValueW(h, n.as_ptr());
                RegCloseKey(h);
            }
        }
    }

    /// DWORD values of a key: (name, value).
    pub fn values(key: &str) -> Vec<(String, u32)> {
        let Some(h) = open(key, false) else { return vec![] };
        let mut out = vec![];
        for i in 0.. {
            let mut name = [0u16; 1024];
            let mut len = name.len() as u32;
            let mut ty = 0u32;
            let mut data = 0u32;
            let mut size = 4u32;
            let r = unsafe { RegEnumValueW(h, i, name.as_mut_ptr(), &mut len, std::ptr::null(), &mut ty, &mut data as *mut u32 as *mut u8, &mut size) };
            if r != ERROR_SUCCESS {
                // ERROR_MORE_DATA: a non-DWORD value; skip it, anything else ends the list
                if r == 234 {
                    continue;
                }
                break;
            }
            if ty == REG_DWORD {
                out.push((String::from_utf16_lossy(&name[..len as usize]), data));
            }
        }
        unsafe { RegCloseKey(h) };
        out
    }
}

#[cfg(not(windows))]
mod reg {
    pub fn set_dword(_: &str, _: &str, _: u32) -> Result<(), String> {
        Err("the overlay is Windows only".into())
    }
    pub fn delete(_: &str, _: &str) {}
    pub fn values(_: &str) -> Vec<(String, u32)> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_names_the_layer_and_its_off_switch() {
        let m = manifest();
        assert!(m.contains(r#""name": "VK_LAYER_ENS_meter""#));
        assert!(m.contains(r#""type": "GLOBAL""#));
        assert!(m.contains(r#""library_path": ".\\ens_overlay.dll""#), "{m}");
        assert!(m.contains(r#""disable_environment": { "ENS_OVERLAY_DISABLE": "1" }"#));
        assert!(m.contains(r#""file_format_version": "1.0.0""#));
    }
}

//! Finding the running game and loading a DLL into it; used by the `ens-inject` command and by the planner, which
//! starts the combat logger by itself (docs/plans/planner-loader.md).

use std::path::{Path, PathBuf};

pub const GAME_EXE: &str = "enshrouded.exe";

/// The running game: its process id and the full path of its exe.
#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub pid: u32,
    pub exe: PathBuf,
    /// When the process started (None when it cannot be read).
    pub started: Option<std::time::SystemTime>,
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx};
    use windows_sys::Win32::System::Threading::*;

    fn name(buf: &[u16]) -> String {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    pub fn find_game() -> Option<Game> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut pid = 0;
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    if name(&e.szExeFile).eq_ignore_ascii_case(GAME_EXE) {
                        pid = e.th32ProcessID;
                        break;
                    }
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
            if pid == 0 {
                return None;
            }
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            let z = windows_sys::Win32::Foundation::FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut created, mut exited, mut kernel, mut user) = (z, z, z, z);
            let started = (GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) != 0).then(|| {
                // FILETIME: 100 ns since 1601-01-01
                let t = (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64;
                std::time::UNIX_EPOCH + std::time::Duration::from_nanos(t.saturating_sub(116_444_736_000_000_000) * 100)
            });
            CloseHandle(h);
            (ok != 0).then(|| Game { pid, exe: PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])), started })
        }
    }

    pub fn modules(pid: u32) -> Vec<String> {
        let mut out = vec![];
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut m: MODULEENTRY32W = std::mem::zeroed();
            m.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
            if Module32FirstW(snap, &mut m) != 0 {
                loop {
                    out.push(name(&m.szModule));
                    if Module32NextW(snap, &mut m) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out
    }

    pub fn inject(pid: u32, dll: &Path) -> Result<(), String> {
        unsafe {
            let proc = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
            if proc.is_null() {
                return Err(format!("cannot open {GAME_EXE} (pid {pid})"));
            }
            let wide: Vec<u16> = dll.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
            let bytes = wide.len() * 2;
            let remote = VirtualAllocEx(proc, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() || WriteProcessMemory(proc, remote, wide.as_ptr() as _, bytes, std::ptr::null_mut()) == 0 {
                CloseHandle(proc);
                return Err(format!("cannot write into {GAME_EXE}"));
            }
            let k32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
            let load = GetProcAddress(GetModuleHandleW(k32.as_ptr()), c"LoadLibraryW".as_ptr() as _);
            let th = CreateRemoteThread(proc, std::ptr::null(), 0, std::mem::transmute(load), remote, 0, std::ptr::null_mut());
            if th.is_null() {
                CloseHandle(proc);
                return Err("cannot start the loader thread".into());
            }
            WaitForSingleObject(th, INFINITE);
            CloseHandle(th);
            VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
            CloseHandle(proc);
        }
        Ok(())
    }
}

/// The running game, if any.
pub fn find_game() -> Option<Game> {
    #[cfg(windows)]
    return win::find_game();
    #[cfg(not(windows))]
    None
}

/// Module (DLL) names loaded in a process; empty when it cannot be read.
pub fn modules(pid: u32) -> Vec<String> {
    #[cfg(windows)]
    return win::modules(pid);
    #[cfg(not(windows))]
    {
        let _ = pid;
        vec![]
    }
}

/// Whether a process has a module of this file name loaded.
pub fn has_module(pid: u32, file: &str) -> bool {
    modules(pid).iter().any(|m| m.eq_ignore_ascii_case(file))
}

/// Load `dll` into process `pid` (LoadLibraryW on a remote thread); returns once it has loaded.
pub fn inject(pid: u32, dll: &Path) -> Result<(), String> {
    if !dll.is_file() {
        return Err(format!("{} is missing", dll.display()));
    }
    #[cfg(windows)]
    return win::inject(pid, dll);
    #[cfg(not(windows))]
    Err("Windows only".into())
}

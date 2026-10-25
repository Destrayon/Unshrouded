//! Combat logger. Loaded into enshrouded.exe by `ens-inject`, it replaces the instruction that marks ApplyDamage's
//! damage event ready (see sig.rs) with a jump to a stub that hands the record and ApplyDamage's frame to `on_hit`,
//! then runs the replaced instruction and jumps back. It only reads game memory. Rows go to
//! `combat-<unix time>.csv` next to this DLL; `hook.log` says whether the hook was placed. A second hook (sig::client)
//! reads each hit the game's client is sent, for someone else's world, where ApplyDamage runs on their server; those
//! rows say `client` in the last column (`origin`), lack the per-type amounts, and are dropped when the same hit was
//! also logged by ApplyDamage (in your own world, both run). At that hook the logger also reads the players' names
//! (sig::client::players), and the first row after they change carries them in the last column (`names`).

pub mod ecs;
pub mod sig;

#[cfg(windows)]
mod imp {
    use crate::ecs::{self, Mem};
    use crate::sig::{self, record};
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::{HMODULE, MAX_PATH};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW, GetModuleHandleW};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION,
    };
    use windows_sys::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
    use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    struct Hit {
        /// The ammunition item and the used item (bow, staff…) of the hit's impact, read at ApplyDamage (0: unknown, or
        /// a client row: a joined player's PC has no impact data).
        ammo_item: u32,
        used_item: u32,
        /// The players' names (names column) when they changed since the last row written, else empty.
        names: String,
        /// From the client hook (no per-type amounts) rather than ApplyDamage.
        client: bool,
        /// When it was queued, so a client hit can wait for its ApplyDamage twin before being written.
        at: Instant,
        ms: u128,
        /// Wall-clock time of the hit (unix ms), so a row can be matched to the save that was current then.
        unix_ms: u128,
        rec: [u8; record::SIZE],
        types: [u8; record::TYPES_LEN],
        /// Raw 32-bit attribute values read through ecs.rs: the attacker's and target's Level, the target's
        /// Health_Max. None when any read or type check failed.
        attacker_level: Option<u32>,
        target_level: Option<u32>,
        target_health_max: Option<u32>,
    }

    /// The game's own memory, read so that a bad pointer fails the read instead of faulting.
    struct Own;
    impl Mem for Own {
        fn read(&self, addr: u64, out: &mut [u8]) -> bool {
            let mut n = 0usize;
            let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as _, out.as_mut_ptr() as _, out.len(), &mut n) };
            ok != 0 && n == out.len()
        }
    }

    static QUEUE: Mutex<Vec<Hit>> = Mutex::new(Vec::new());
    /// Client hits waiting a second for a matching ApplyDamage hit, and the (target, attacker, hit counter) of recent
    /// ApplyDamage hits and of client hits already taken.
    static PENDING: Mutex<Vec<Hit>> = Mutex::new(Vec::new());
    static SEEN: Mutex<Vec<(Instant, (u32, u32, u16, u64))>> = Mutex::new(Vec::new());
    /// The players' names (entity id, name, the local player), as last read, when, and whether the writer still has to
    /// write them.
    static ROSTER: Mutex<(Vec<(u32, String, bool)>, Option<Instant>, bool)> = Mutex::new((Vec::new(), None, false));

    /// The player list of the client UI `g` (analysis/player_names.md): each player's entity id and the name the game
    /// shows, the character name or, when that is empty, the Online ID. None when a read fails.
    fn read_roster(m: &impl Mem, g: u64) -> Option<Vec<(u32, String, bool)>> {
        use sig::client::players as P;
        let u64_at = |a: u64| { let mut b = [0u8; 8]; m.read(a, &mut b).then(|| u64::from_le_bytes(b)) };
        let text = |ptr: u64, len: u64| -> Option<String> {
            if ptr == 0 || len == 0 || len > 64 {
                return None;
            }
            let mut b = vec![0u8; len as usize];
            m.read(ptr, &mut b).then(|| String::from_utf8_lossy(&b).trim().to_string()).filter(|t| !t.is_empty())
        };
        let (list, count) = (u64_at(g + P::LIST)?, u64_at(g + P::COUNT)?);
        let local = u64_at(g + P::LOCAL)? as u32;
        if count > 16 || (count > 0 && list == 0) {
            return None;
        }
        let mut out = vec![];
        for k in 0..count {
            let e = list + k * P::ENTRY;
            let id = u64_at(e + P::ID)? as u32;
            let name = text(u64_at(e + P::NAME)?, u64_at(e + P::NAME_LEN)?)
                .or_else(|| text(u64_at(e + P::ONLINE)?, u64_at(e + P::ONLINE_LEN)?));
            if let (true, Some(n)) = (id != 0, name) {
                out.push((id, n, id == local));
            }
        }
        Some(out)
    }

    /// The names column: `id=name;id=name`, the local player's id marked with `*`, and `%`, `,`, `;`, `=`, `*` and
    /// line breaks in names written as %XX.
    fn names_cell(r: &[(u32, String, bool)]) -> String {
        let esc = |t: &str| t.chars().map(|c| match c { '%' | ',' | ';' | '=' | '*' | '\n' | '\r' | '"' => format!("%{:02X}", c as u32), c => c.to_string() }).collect::<String>();
        r.iter().map(|(id, n, me)| format!("{}{id}={}", if *me { "*" } else { "" }, esc(n))).collect::<Vec<_>>().join(";")
    }

    fn key(rec: &[u8], with_time: bool) -> (u32, u32, u16, u64) {
        let t = if with_time { u64::from_le_bytes(rec[sig::client::TIME..sig::client::TIME + 8].try_into().unwrap()) } else { 0 };
        (i32_at(rec, record::TARGET) as u32, i32_at(rec, record::ROOT_SOURCE) as u32,
            u16::from_le_bytes([rec[sig::client::HIT_COUNTER], rec[sig::client::HIT_COUNTER + 1]]), t)
    }

    /// Called by the client stub on the game's thread with rsi = the HitEvent being copied to the damage numbers.
    extern "system" fn on_client_hit(ev: *const u8, ui: *const u8) {
        // the players' names, about once a second
        if !ui.is_null() {
            let _ = std::panic::catch_unwind(|| {
                let due = ROSTER.lock().map(|r| r.1.is_none_or(|t| t.elapsed().as_millis() >= 1000)).unwrap_or(false);
                if due {
                    let read = read_roster(&Own, ui as u64);
                    if let Ok(mut r) = ROSTER.lock() {
                        r.1 = Some(Instant::now());
                        if let Some(list) = read.filter(|l| !l.is_empty() && *l != r.0) {
                            (r.0, r.2) = (list, true);
                        }
                    }
                }
            });
        }
        if ev.is_null() {
            return;
        }
        let _ = std::panic::catch_unwind(|| unsafe {
            let mut rec = [0u8; record::SIZE];
            std::ptr::copy_nonoverlapping(ev, rec.as_mut_ptr(), sig::client::EVENT_SIZE);
            // the same event may be copied on more than one frame: once per (target, attacker, counter, time)
            let k = key(&rec, true);
            if let Ok(mut seen) = SEEN.lock() {
                if seen.iter().any(|(_, s)| *s == k) {
                    return;
                }
                seen.push((Instant::now(), k));
            }
            let h = Hit {
                ammo_item: 0, used_item: 0, names: String::new(), client: true, at: Instant::now(),
                ms: START.lock().ok().and_then(|s| s.map(|s| s.elapsed().as_millis())).unwrap_or(0),
                unix_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
                rec, types: [0; record::TYPES_LEN], attacker_level: None, target_level: None, target_health_max: None,
            };
            if let Ok(mut q) = PENDING.lock() {
                if q.len() < 100_000 {
                    q.push(h);
                }
            }
        });
    }
    static START: Mutex<Option<Instant>> = Mutex::new(None);
    static mut DIR: Option<std::path::PathBuf> = None;

    /// Called by the stub on the game's thread: copy, queue, return. Never panics into the game.
    extern "system" fn on_hit(rec: *const u8, frame: *const u8) {
        let _ = std::panic::catch_unwind(|| unsafe {
            let mut h = Hit {
                ammo_item: 0,
                used_item: 0,
                names: String::new(),
                client: false,
                at: Instant::now(),
                ms: 0,
                unix_ms: 0,
                rec: [0; record::SIZE],
                types: [0; record::TYPES_LEN],
                attacker_level: None,
                target_level: None,
                target_health_max: None,
            };
            std::ptr::copy_nonoverlapping(rec, h.rec.as_mut_ptr(), record::SIZE);
            std::ptr::copy_nonoverlapping(frame.offset(record::FRAME_TYPES), h.types.as_mut_ptr(), record::TYPES_LEN);
            let (rbp, attacker, target) = (frame as u64, i32_at(&h.rec, record::ROOT_SOURCE) as u32, i32_at(&h.rec, record::TARGET) as u32);
            h.attacker_level = ecs::entity_attribute(&Own, rbp, attacker, ecs::SLOT_LEVEL, ecs::ATTR_LEVEL);
            h.target_level = ecs::entity_attribute(&Own, rbp, target, ecs::SLOT_LEVEL, ecs::ATTR_LEVEL);
            h.target_health_max = ecs::entity_attribute(&Own, rbp, target, ecs::SLOT_HEALTH, ecs::ATTR_HEALTH_MAX);
            // the arrow or spell (analysis/ammo.md): the record's impactId (+0xD4) → its impact's ImpactItemConfig
            if let Some((ammo, used)) = ecs::hit_items(&Own, rbp, i32_at(&h.rec, record::IMPACT_ID) as u32) {
                (h.ammo_item, h.used_item) = (ammo, used);
            }
            h.ms = START.lock().ok().and_then(|s| s.map(|s| s.elapsed().as_millis())).unwrap_or(0);
            h.unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            if let Ok(mut seen) = SEEN.lock() {
                seen.push((Instant::now(), key(&h.rec, false)));
            }
            if let Ok(mut q) = QUEUE.lock() {
                if q.len() < 100_000 {
                    q.push(h);
                }
            }
        });
    }

    fn dir() -> std::path::PathBuf {
        unsafe { (*std::ptr::addr_of!(DIR)).clone().unwrap_or_else(|| ".".into()) }
    }

    fn note(msg: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("hook.log")) {
            let _ = writeln!(f, "{msg}");
        }
    }

    fn i32_at(b: &[u8], o: usize) -> i32 {
        i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    fn f32_at(b: &[u8], o: usize) -> f32 {
        f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    fn header() -> String {
        // columns are only ever added at the end, so older readers keep working
        format!("ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,{},record,blocked,no_damage,loss_x10,unix_ms,attacker_level,target_level,target_health_max,{},origin,damage_types,names,ammo_item,used_item",
            (0..12).map(|i| format!("type{i}")).collect::<Vec<_>>().join(","), record::decoded_header())
    }

    /// One CSV row, columns in header() order. The 0x150-byte record goes in as hex only when `full` (a `full-records`
    /// file in the log folder): its fields `ens calc log` reads are in the decoded columns at the end.
    fn row(h: &Hit, full: bool) -> String {
        let flags = i32_at(&h.rec, record::FLAGS) as u32;
        let types: Vec<String> = (0..12).map(|i| f32_at(&h.types, i * 4).to_string()).collect();
        let hex: String = if full { h.rec.iter().map(|b| format!("{b:02x}")).collect() } else { String::new() };
        let raw = |v: Option<u32>| v.map(|v| format!("{v:#x}")).unwrap_or_default();
        format!("{},{},{},{},{},{:#x},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}", h.ms, i32_at(&h.rec, record::ENTITY_A),
            i32_at(&h.rec, record::ENTITY_B), i32_at(&h.rec, record::HEALTH), i32_at(&h.rec, record::STAMINA), flags,
            flags & 0x08 != 0, flags & 0x20 != 0, flags & 0xc0 != 0, types.join(","), hex,
            flags & record::FLAG_BLOCKED != 0, flags & record::FLAG_NO_DAMAGE != 0, f32_at(&h.rec, record::LOSS_X10), h.unix_ms,
            raw(h.attacker_level), raw(h.target_level), raw(h.target_health_max), record::decoded_row(&h.rec),
            if h.client { "client" } else { "apply" },
            u16::from_le_bytes([h.rec[sig::client::DAMAGE_TYPES], h.rec[sig::client::DAMAGE_TYPES + 1]]), h.names,
            h.ammo_item, h.used_item)
    }

    fn writer() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = dir().join(format!("combat-{stamp}.csv"));
        let Ok(mut f) = std::fs::File::create(&path) else { return note(&format!("cannot create {}", path.display())) };
        let _ = writeln!(f, "{}", header());
        let full = dir().join("full-records").exists();
        note(&format!("logging to {}{}", path.display(), if full { " (full records)" } else { "" }));
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let mut hits = std::mem::take(&mut *QUEUE.lock().unwrap());
            // client hits a second old: written unless ApplyDamage logged the same hit (your own world)
            let ready: Vec<Hit> = {
                let mut p = PENDING.lock().unwrap();
                let (old, young): (Vec<Hit>, Vec<Hit>) = std::mem::take(&mut *p).into_iter().partition(|h| h.at.elapsed().as_millis() >= 1000);
                *p = young;
                old
            };
            if let Ok(mut seen) = SEEN.lock() {
                seen.retain(|(t, _)| t.elapsed().as_secs() < 10);
                hits.extend(ready.into_iter().filter(|h| { let k = key(&h.rec, false); !seen.iter().any(|(_, s)| *s == k) }));
            }
            hits.sort_by_key(|h| h.ms);
            // new names ride on the first row written after they changed
            if let (Some(first), Ok(mut r)) = (hits.first_mut(), ROSTER.lock()) {
                if r.2 {
                    first.names = names_cell(&r.0);
                    r.2 = false;
                }
            }
            for h in hits {
                let _ = writeln!(f, "{}", row(&h, full));
            }
            let _ = f.flush();
        }
    }

    /// Executable memory within ±2 GB of `near`, so a 5-byte relative jump reaches it.
    unsafe fn alloc_near(near: usize, size: usize) -> *mut u8 {
        let mut addr = near.saturating_sub(0x7000_0000) & !0xffff;
        while addr < near + 0x7000_0000 {
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { VirtualQuery(addr as _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
                break;
            }
            if mbi.State == 0x10000 {
                // MEM_FREE
                let p = unsafe { VirtualAlloc(addr as _, size, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) };
                if !p.is_null() {
                    return p as *mut u8;
                }
            }
            addr = (mbi.BaseAddress as usize + mbi.RegionSize + 0xffff) & !0xffff;
        }
        std::ptr::null_mut()
    }

    /// The stub: save volatile registers, call on_hit(r8, rbp) on a 16-byte aligned stack, restore, run the replaced
    /// `mov byte ptr [r8 + 0x14c], 1`, jump back.
    fn stub(on_hit: u64, back: u64) -> Vec<u8> {
        // rcx = r8 (the record), rdx = rbp (ApplyDamage's frame)
        stub_with(on_hit, back, &[0x4c, 0x89, 0xc1, 0x48, 0x89, 0xea], &sig::REPLACED)
    }

    /// The stub with `args` loading rcx/rdx for the call and `replaced` the instruction the jump overwrote.
    fn stub_with(on_hit: u64, back: u64, args: &[u8], replaced: &[u8]) -> Vec<u8> {
        let mut s: Vec<u8> = vec![];
        s.extend([0x50, 0x51, 0x52, 0x41, 0x50, 0x41, 0x51, 0x41, 0x52, 0x41, 0x53, 0x9c, 0x53]); // push rax..r11, flags, rbx
        s.extend([0x48, 0x89, 0xe3]); // mov rbx, rsp
        // The game is mid-function here and expects every register as it was, the full width of the vector registers
        // included: the logger's code and the Windows calls it makes may use AVX and overwrite the upper halves the
        // ABI lets a callee change. So the whole extended state is saved with XSAVE and put back with XRSTOR (x87, SSE,
        // AVX and AVX-512: mask 0xE7; AMX tiles left out, a game thread does not hold them here).
        s.extend([0x48, 0x83, 0xe4, 0xc0]); // and rsp, -64
        for _ in 0..3 {
            // 12 KB below, a page at a time so Windows grows the thread's stack (it commits pages only in order)
            s.extend([0x48, 0x81, 0xec, 0x00, 0x10, 0x00, 0x00]); // sub rsp, 0x1000
            s.extend([0x48, 0x85, 0x24, 0x24]); // test [rsp], rsp
        }
        s.extend([0x48, 0x83, 0xec, 0x40]); // sub rsp, 0x40: shadow space, the save area at rsp + 0x40 (64-aligned)
        // XRSTOR faults on a header with stray bytes: zero the 64-byte header at area + 0x200 first
        s.extend([0x31, 0xc0]); // xor eax, eax
        for k in 0..8u32 {
            s.extend([0x48, 0x89, 0x84, 0x24]); // mov [rsp + disp32], rax
            s.extend((0x240 + 8 * k).to_le_bytes());
        }
        s.extend([0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xd2]); // mov eax, 0xE7; xor edx, edx
        s.extend([0x48, 0x0f, 0xae, 0x64, 0x24, 0x40]); // xsave64 [rsp + 0x40]
        s.extend(args);
        s.extend([0x48, 0xb8]);
        s.extend(on_hit.to_le_bytes()); // mov rax, on_hit
        s.extend([0xff, 0xd0]); // call rax
        s.extend([0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xd2]); // mov eax, 0xE7; xor edx, edx
        s.extend([0x48, 0x0f, 0xae, 0x6c, 0x24, 0x40]); // xrstor64 [rsp + 0x40]
        s.extend([0x48, 0x89, 0xdc]); // mov rsp, rbx
        s.extend([0x5b, 0x9d, 0x41, 0x5b, 0x41, 0x5a, 0x41, 0x59, 0x41, 0x58, 0x5a, 0x59, 0x58]); // pop in reverse
        s.extend(replaced);
        s.extend([0xff, 0x25, 0, 0, 0, 0]);
        s.extend(back.to_le_bytes()); // jmp [rip+0] → back
        s
    }

    unsafe fn install() -> Result<String, String> {
        let exe = unsafe { GetModuleHandleW(std::ptr::null()) };
        let mut mi: MODULEINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetModuleInformation(GetCurrentProcess(), exe, &mut mi, std::mem::size_of::<MODULEINFO>() as u32) } == 0 {
            return Err("GetModuleInformation failed".into());
        }
        let image = unsafe { std::slice::from_raw_parts(mi.lpBaseOfDll as *const u8, mi.SizeOfImage as usize) };
        let hits = sig::find(image);
        if hits.len() != 1 {
            return Err(format!("signature found {} times (expected 1): this game build is not supported", hits.len()));
        }
        let at = mi.lpBaseOfDll as usize + hits[0] + sig::PATCH_AT;
        let stub_at = unsafe { hook_at(at)? };
        // the client hook is a bonus: without it, your own world is still logged
        // an empty file named no-client-hook in the log folder leaves the client hook out (to compare a session without it)
        let client = if dir().join("no-client-hook").exists() {
            "; client hits not hooked: no-client-hook file".to_string()
        } else { match sig::client::find(image).as_slice() {
            [c] => match unsafe { patch(mi.lpBaseOfDll as usize + c + sig::client::PATCH_AT, &sig::client::REPLACED,
                stub_with(on_client_hit as *const () as u64, (mi.lpBaseOfDll as usize + c + sig::client::PATCH_AT + sig::client::PATCH_LEN) as u64,
                    &[0x48, 0x89, 0xf1, 0x4c, 0x89, 0xf2], &sig::client::REPLACED)) } {
                Ok(s) => format!("; client hits hooked, stub {s:#x}"),
                Err(e) => format!("; client hits not hooked: {e}"),
            },
            found => format!("; client hits not hooked: signature found {} times", found.len()),
        } };
        Ok(format!("hooked ApplyDamage at {:#x} (module base {:#x}), stub {:#x}{client}", at, mi.lpBaseOfDll as usize, stub_at))
    }

    /// Replace the 8-byte `mov byte ptr [r8 + 0x14c], 1` at `at` with a jump to a new stub; returns the stub address.
    pub(crate) unsafe fn hook_at(at: usize) -> Result<usize, String> {
        unsafe { patch(at, &sig::REPLACED, stub(on_hit as *const () as u64, (at + sig::PATCH_LEN) as u64)) }
    }

    /// Replace the instruction `replaced` (5 to 8 bytes) at `at` with a jump to `code` (padded with nops); returns the
    /// stub address.
    unsafe fn patch(at: usize, replaced: &[u8], code: Vec<u8>) -> Result<usize, String> {
        let target = unsafe { std::slice::from_raw_parts(at as *const u8, replaced.len()) };
        if target != replaced {
            return Err("hook point bytes differ (already hooked?)".into());
        }
        let mem = unsafe { alloc_near(at, 4096) };
        if mem.is_null() {
            return Err("no memory near the game module for the stub".into());
        }
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), mem, code.len()) };
        let rel = (mem as i64 - (at as i64 + 5)) as i32;
        // 8 bytes from `at`: the jump, nops up to the replaced instruction's end, then the bytes that follow unchanged
        let mut patch = [0x90u8; 8];
        let current = unsafe { std::ptr::read_unaligned(at as *const [u8; 8]) };
        patch[replaced.len()..].copy_from_slice(&current[replaced.len()..]);
        patch[0] = 0xe9;
        patch[1..5].copy_from_slice(&rel.to_le_bytes());
        let mut old = 0u32;
        if unsafe { VirtualProtect(at as _, 8, PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
            return Err("VirtualProtect failed".into());
        }
        // one 8-byte store, so a thread running the hooked code sees either the old or the new instruction
        unsafe { std::ptr::write_unaligned(at as *mut u64, u64::from_le_bytes(patch)) };
        unsafe { VirtualProtect(at as _, 8, old, &mut old) };
        Ok(mem as usize)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A stand-in for the end of ApplyDamage: `push rbp; mov rbp, rdx; mov r8, rcx; mov byte ptr [r8+0x14c], 1;
        /// pop rbp; ret`. After hooking, calling it must still set the ready byte, and on_hit must have copied the
        /// record (rcx) and the per-type array at frame (rdx) − 0x10.
        #[test]
        fn stub_logs_the_record_and_keeps_the_game_instruction() {
            let mut code = vec![0x55, 0x48, 0x89, 0xd5, 0x49, 0x89, 0xc8];
            let at_off = code.len();
            code.extend(sig::REPLACED);
            code.extend([0x5d, 0xc3]);
            unsafe {
                let f = VirtualAlloc(std::ptr::null(), 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as *mut u8;
                std::ptr::copy_nonoverlapping(code.as_ptr(), f, code.len());
                hook_at(f as usize + at_off).unwrap();
                let mut rec = [0u8; record::SIZE];
                rec[record::HEALTH..record::HEALTH + 4].copy_from_slice(&(-37i32).to_le_bytes());
                rec[record::FLAGS] = 0x08 | record::FLAG_BLOCKED as u8;
                rec[record::LOSS_X10..record::LOSS_X10 + 4].copy_from_slice(&370.0f32.to_le_bytes());
                let mut frame = [0u8; 0x100];
                frame[0x80 - 0x10..0x80 - 0x10 + 4].copy_from_slice(&37.0f32.to_le_bytes());
                let call: extern "system" fn(*mut u8, *const u8) = std::mem::transmute(f);
                call(rec.as_mut_ptr(), frame.as_ptr().add(0x80));
                assert_eq!(rec[0x14c], 1, "replaced instruction still runs");
                let q = std::mem::take(&mut *QUEUE.lock().unwrap());
                assert_eq!(q.len(), 1);
                assert_eq!(i32_at(&q[0].rec, record::HEALTH), -37);
                assert_eq!(f32::from_le_bytes(q[0].types[0..4].try_into().unwrap()), 37.0);
                assert_eq!(f32_at(&q[0].rec, record::LOSS_X10), 370.0);
                let (head, line) = (header(), row(&q[0], false));
                let cols: Vec<&str> = head.split(',').collect();
                let vals: Vec<&str> = line.split(',').collect();
                assert_eq!(cols.len(), vals.len(), "header names every column written");
                let get = |n: &str| vals[cols.iter().position(|c| *c == n).unwrap()];
                assert_eq!((get("crit"), get("blocked"), get("no_damage"), get("loss_x10")), ("true", "true", "false", "370"));
                // the old columns keep their places; the decoded ones follow them, then origin
                let n = record::DECODED.len();
                assert_eq!(cols[cols.len() - 13 - n..cols.len() - 5 - n], ["record", "blocked", "no_damage", "loss_x10", "unix_ms", "attacker_level",
                    "target_level", "target_health_max"]);
                assert_eq!(cols[cols.len() - 5 - n..cols.len() - 5].join(","), record::decoded_header());
                assert_eq!((cols[cols.len() - 5], get("origin")), ("origin", "apply"));
                assert_eq!((cols[cols.len() - 4], get("damage_types")), ("damage_types", "0"));
                assert_eq!((cols[cols.len() - 3], get("names")), ("names", ""));
                assert_eq!((get("ammo_item"), get("used_item")), ("0", "0"), "a frame of null pointers: no items");
                // compact rows leave the hex out; full rows carry it
                assert_eq!(get("record"), "");
                let full = row(&q[0], true);
                let hex = full.split(',').nth(cols.iter().position(|c| *c == "record").unwrap()).unwrap();
                assert_eq!(hex.len(), record::SIZE * 2);
                // a frame of null pointers: the attribute reads fail quietly and the columns stay empty
                assert_eq!((get("attacker_level"), get("target_level"), get("target_health_max")), ("", "", ""));
                assert!(get("unix_ms").parse::<u128>().unwrap() > 1_700_000_000_000, "wall-clock ms");
            }
        }
    }

    #[cfg(test)]
    mod client_tests {
        use super::*;

        /// A stand-in for update_ui_data's loop: `push rsi; push r14; mov rsi, rcx; xor r14, r14; add r14, 0x2e50; nop;
        /// mov rax, r14; pop r14; pop rsi; ret` (rsi and r14 are the caller's: saved and restored). After hooking, it
        /// still adds, on_client_hit queued the event once even when the same event comes twice, and the writer's rule
        /// drops it when ApplyDamage logged the same hit.
        #[test]
        fn client_stub_logs_each_event_once_and_keeps_the_game_instruction() {
            let mut code = vec![0x56, 0x41, 0x56, 0x48, 0x89, 0xce, 0x4d, 0x31, 0xf6];
            let at_off = code.len();
            code.extend(sig::client::REPLACED);
            code.extend([0x90, 0x4c, 0x89, 0xf0, 0x41, 0x5e, 0x5e, 0xc3]);
            unsafe {
                let f = VirtualAlloc(std::ptr::null(), 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as *mut u8;
                std::ptr::copy_nonoverlapping(code.as_ptr(), f, code.len());
                let at = f as usize + at_off;
                patch(at, &sig::client::REPLACED, stub_with(on_client_hit as *const () as u64, (at + sig::client::PATCH_LEN) as u64,
                    &[0x48, 0x89, 0xf1, 0x4c, 0x89, 0xf2], &sig::client::REPLACED)).unwrap();
                let mut ev = [0u8; sig::client::EVENT_SIZE];
                ev[record::HEALTH..record::HEALTH + 4].copy_from_slice(&(-55i32).to_le_bytes());
                ev[record::TARGET..record::TARGET + 4].copy_from_slice(&900u32.to_le_bytes());
                ev[record::ROOT_SOURCE..record::ROOT_SOURCE + 4].copy_from_slice(&50u32.to_le_bytes());
                ev[sig::client::HIT_COUNTER] = 7;
                ev[sig::client::DAMAGE_TYPES] = 0x18;
                let call: extern "system" fn(*const u8) -> u64 = std::mem::transmute(f);
                assert_eq!(call(ev.as_ptr()), 0x2e50, "the replaced add still runs");
                call(ev.as_ptr());
                let pending = std::mem::take(&mut *PENDING.lock().unwrap());
                assert_eq!(pending.len(), 1, "the same event copied twice is one hit");
                assert!(pending[0].client);
                assert_eq!(i32_at(&pending[0].rec, record::HEALTH), -55);
                let line = row(&pending[0], false);
                assert!(line.ends_with(",client,24,,0,0"), "{line}");
                // ApplyDamage logged the same (target, attacker, counter): the client copy is not written
                SEEN.lock().unwrap().push((Instant::now(), key(&pending[0].rec, false)));
                let k = key(&pending[0].rec, false);
                assert!(SEEN.lock().unwrap().iter().any(|(_, s)| *s == k));
            }
        }
    }

    #[cfg(test)]
    mod state_tests {
        use super::*;

        static mut SEEN_ARG: u64 = 0;

        /// Overwrites ymm0-15 entirely (as an AVX memcpy in the logger or Windows may) and records its first argument.
        extern "system" fn clobber(a: *const u8, _: *const u8) {
            unsafe {
                SEEN_ARG = a as u64;
                std::arch::asm!("vpcmpeqd ymm0, ymm0, ymm0", "vpcmpeqd ymm1, ymm1, ymm1", "vpcmpeqd ymm6, ymm6, ymm6",
                    "vpcmpeqd ymm15, ymm15, ymm15", out("ymm0") _, out("ymm1") _, out("ymm6") _, out("ymm15") _);
            }
        }

        /// The stub around a call that trashes the vector registers: every ymm register the game had is back after it,
        /// the general registers too, and the replaced instruction still runs. Needs AVX.
        #[test]
        fn the_stub_keeps_the_full_vector_and_general_registers() {
            if !std::arch::is_x86_feature_detected!("avx") {
                return;
            }
            // `mov [rcx], 7` stands for the replaced instruction (5+ bytes): mov dword ptr [rcx], 7 = c7 01 07 00 00 00
            let replaced = [0xc7u8, 0x01, 0x07, 0x00, 0x00, 0x00];
            unsafe {
                let f = VirtualAlloc(std::ptr::null(), 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as *mut u8;
                // the "game function": loads ymm0/ymm6/ymm15 and r11, runs the hooked spot, then returns what it holds
                let mut code: Vec<u8> = vec![];
                let at_off;
                {
                    // vmovdqu ymm0, [rdx]; vmovdqu ymm6, [rdx]; vmovdqu ymm15, [rdx]; mov r11, 0x1122334455667788
                    code.extend([0xc5, 0xfe, 0x6f, 0x02, 0xc5, 0xfe, 0x6f, 0x32, 0xc5, 0x7e, 0x6f, 0x3a]);
                    code.extend([0x49, 0xbb]);
                    code.extend(0x1122_3344_5566_7788u64.to_le_bytes());
                    at_off = code.len();
                    code.extend(replaced);
                    // vmovdqu [r8], ymm0; vmovdqu [r8+32], ymm6; vmovdqu [r8+64], ymm15; mov [r9], r11; vzeroupper; ret
                    code.extend([0xc4, 0xc1, 0x7e, 0x7f, 0x00, 0xc4, 0xc1, 0x7e, 0x7f, 0x70, 0x20, 0xc4, 0x41, 0x7e, 0x7f, 0x78, 0x40]);
                    code.extend([0x4d, 0x89, 0x19, 0xc5, 0xf8, 0x77, 0xc3]);
                }
                std::ptr::copy_nonoverlapping(code.as_ptr(), f, code.len());
                let at = f as usize + at_off;
                // pass rcx (the target of the replaced store) as the hook's argument
                patch(at, &replaced, stub_with(clobber as *const () as u64, (at + replaced.len()) as u64, &[0x48, 0x89, 0xc9, 0x31, 0xd2], &replaced)).unwrap();
                let pattern: [u8; 32] = std::array::from_fn(|i| i as u8 * 3 + 1);
                let mut out = [0u8; 96];
                let mut r11 = 0u64;
                let mut target = 0u32;
                let call: extern "system" fn(*mut u32, *const u8, *mut u8, *mut u64) = std::mem::transmute(f);
                call(&mut target, pattern.as_ptr(), out.as_mut_ptr(), &mut r11);
                assert_eq!(target, 7, "the replaced instruction ran");
                assert_eq!(std::ptr::addr_of!(SEEN_ARG).read(), &target as *const u32 as u64, "the hook got its argument");
                assert_eq!(&out[..32], &pattern, "ymm0 whole");
                assert_eq!(&out[32..64], &pattern, "ymm6 whole");
                assert_eq!(&out[64..], &pattern, "ymm15 whole");
                assert_eq!(r11, 0x1122_3344_5566_7788, "r11");
            }
        }
    }

    #[cfg(test)]
    mod roster_tests {
        use super::*;

        /// A fake UI data block with two players: one with a character name, one with only an Online ID; the names
        /// column escapes the separators.
        #[test]
        fn reads_each_players_name_and_writes_them_escaped() {
            use sig::client::players as P;
            let name_a = b"Alice".to_vec();
            let online_b = b"Bob, the Builder".to_vec();
            let mut entries = vec![0u8; 2 * P::ENTRY as usize];
            let put = |b: &mut Vec<u8>, at: u64, v: u64| b[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
            entries[0..4].copy_from_slice(&2u32.to_le_bytes());
            put(&mut entries, P::NAME, name_a.as_ptr() as u64);
            put(&mut entries, P::NAME_LEN, name_a.len() as u64);
            entries[P::ENTRY as usize..P::ENTRY as usize + 4].copy_from_slice(&7u32.to_le_bytes());
            put(&mut entries, P::ENTRY + P::ONLINE, online_b.as_ptr() as u64);
            put(&mut entries, P::ENTRY + P::ONLINE_LEN, online_b.len() as u64);
            let mut g = vec![0u8; (P::COUNT + 8) as usize];
            put(&mut g, P::LOCAL, 2);
            put(&mut g, P::LIST, entries.as_ptr() as u64);
            put(&mut g, P::COUNT, 2);
            let r = read_roster(&Own, g.as_ptr() as u64).unwrap();
            assert_eq!(r, vec![(2, "Alice".to_string(), true), (7, "Bob, the Builder".to_string(), false)]);
            assert_eq!(names_cell(&r), "*2=Alice;7=Bob%2C the Builder");
            put(&mut g, P::COUNT, 99);
            assert_eq!(read_roster(&Own, g.as_ptr() as u64), None, "a count no game has");
        }
    }

    #[unsafe(no_mangle)]
    extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut core::ffi::c_void) -> i32 {
        if reason == DLL_PROCESS_ATTACH {
            unsafe {
                DisableThreadLibraryCalls(module);
                let mut buf = [0u16; MAX_PATH as usize];
                let n = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
                let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
                DIR = path.parent().map(|p| p.to_path_buf());
            }
            std::thread::spawn(|| {
                *START.lock().unwrap() = Some(Instant::now());
                match unsafe { install() } {
                    Ok(m) => {
                        note(&m);
                        writer();
                    }
                    Err(e) => note(&format!("not hooked: {e}")),
                }
            });
        }
        1
    }
}

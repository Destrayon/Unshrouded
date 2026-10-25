//! Where to hook: the end of ApplyDamage (old build 0x1403d4d50, build 26082781 0x1403d50d0), where the finished
//! damage event record is copied to the node's output and marked ready with `mov byte ptr [r8 + 0x14c], 1`
//! (old build 0x1403d75da; analysis/fn_ApplyDamage_full.asm). Both copy paths reach that instruction with r8 = the
//! copied record and rbp = ApplyDamage's frame, so it is replaced by a jump to the logger.

/// Bytes ending in the hooked instruction; None = any byte. Unique in builds 1269579 and 26082781
/// (tools/sigfind.py target/enshrouded.exe <exe> 1403d75ba 48).
pub const SIG: [Option<u8>; 48] = {
    const B: [u8; 48] = [
        0x0f, 0x11, 0x41, 0x20, 0x0f, 0x10, 0x42, 0x40, 0x0f, 0x11, 0x49, 0x30, 0x0f, 0x11, 0x41, 0x40, 0x8b, 0x03, 0x41,
        0x89, 0x80, 0x94, 0x00, 0x00, 0x00, 0x45, 0x89, 0xb8, 0x48, 0x01, 0x00, 0x00, 0x41, 0xc6, 0x80, 0x4c, 0x01, 0x00,
        0x00, 0x01, 0x48, 0x8b, 0x85, 0x00, 0x07, 0x00, 0x00, 0xc6,
    ];
    let mut out = [None; 48];
    let mut i = 0;
    while i < 48 {
        out[i] = Some(B[i]);
        i += 1;
    }
    out
};
/// Offset of `mov byte ptr [r8 + 0x14c], 1` (8 bytes) inside SIG; execution continues right after it.
pub const PATCH_AT: usize = 32;
pub const PATCH_LEN: usize = 8;
pub const REPLACED: [u8; 8] = [0x41, 0xc6, 0x80, 0x4c, 0x01, 0x00, 0x00, 0x01];

/// The second hook (docs/plans/all-players-meter.md, analysis/client_damage.md): where the game's client copies each hit
/// it was sent into the UI's list of damage numbers, so hits are logged in someone else's world too, where ApplyDamage
/// runs on their server. In `update_ui_data` (old build FUN_140317ce0, analysis/client_dmg_140317ce0.c), the first loop
/// steps through the HitEvent stream with `puVar25 = (undefined8 *)(lVar22 + 8 + uVar21);` and copies each whole event
/// to `plVar35 = (longlong *)(local_248 + 0x2e50);` (`*puVar24 = *puVar25;` … `puVar24[0x1e] = puVar25[0x1e];`). The
/// hooked instruction is that loop's `add r14, 0x2e50` (old build 0x140317dad, build 26082781 0x140317f4d;
/// analysis/client_dmg_140317ce0_loop1.asm), where rsi = the current HitEvent.
pub mod client {
    /// 46 bytes ending 18 bytes after the hooked instruction; unique in builds 1269579 and 26082781 (measured 2026-10-24:
    /// 1 match each, at file offsets 0x317198 and 0x317338).
    pub const SIG: [u8; 46] = [
        0x41, 0x8b, 0xc7, 0x48, 0x8d, 0x77, 0x08, 0x48, 0x03, 0xf0, 0x44, 0x8b, 0xf9, 0x41, 0xff, 0xc5, 0x4c, 0x8b, 0x74, 0x24, 0x40,
        0x49, 0x81, 0xc6, 0x50, 0x2e, 0x00, 0x00, 0x49, 0x8b, 0x56, 0x08, 0x4d, 0x8d, 0x46, 0x10, 0x48, 0xff, 0xc2, 0x48, 0x89, 0x95,
        0xa0, 0x01, 0x00, 0x00,
    ];
    /// `add r14, 0x2e50` (7 bytes) inside SIG.
    pub const PATCH_AT: usize = 21;
    pub const PATCH_LEN: usize = 7;
    pub const REPLACED: [u8; 7] = [0x49, 0x81, 0xc6, 0x50, 0x2e, 0x00, 0x00];
    /// `keen::ecs::HitEvent` (kfc/types.json): 248 bytes, the first 0xF8 of the ApplyDamage record, so record:: offsets
    /// apply; it has no per-type amounts (those live only in ApplyDamage's frame).
    pub const EVENT_SIZE: usize = 0xf8;
    /// `GameEvent.timeStamp` (keen::Time, 8 bytes) and `hitCounter` (u16), which with target and attacker tell a
    /// repeated event apart from a new hit.
    pub const TIME: usize = 0x00;
    pub const HIT_COUNTER: usize = 0xd0;
    /// `damageTypes` (kfc/types.json HitEvent field, offset 220, type 3296: 2 bytes): one bit per damage type the hit
    /// carried; the client's only trace of the element, since it gets no per-type amounts.
    pub const DAMAGE_TYPES: usize = 0xdc;

    /// The players' names (analysis/player_names.md): at the hook, r14 = the client UI's `G38UiPlayData` (`G`), whose
    /// player list `player_ui` (old build FUN_1402b34f0) rebuilds every frame: `lVar50 = *(longlong *)(lVar41 + 0x4642c0)
    /// + *(longlong *)(lVar41 + 0x4642c8) * 0x240;` (array pointer, count, 0x240-byte entries). In an entry,
    /// `*piVar37 = *(int *)(uStack_2d618 + (longlong)pcVar23);` is the entity id (+0x00), and `*(undefined **)(piVar37 +
    /// 0x30) = puVar5; *(undefined **)(piVar37 + 0x32) = puVar6;` the character name (+0xC0 UTF-8 pointer, +0xC8 length);
    /// the entry constructor FUN_1402a9850's `mov qword ptr [rcx + 0xd8], rax` / `mov qword ptr [rcx + 0xe0], rdx` the
    /// Online ID (the Steam name: +0xD8 pointer, +0xE0 length).
    pub mod players {
        /// The local player's entity id: `if (*(int *)(uStack_2d618 + (longlong)pcVar23) == *(int *)(lVar41 + 0x18))`.
        pub const LOCAL: u64 = 0x18;
        pub const LIST: u64 = 0x4642c0;
        pub const COUNT: u64 = 0x4642c8;
        pub const ENTRY: u64 = 0x240;
        pub const ID: u64 = 0x00;
        pub const NAME: u64 = 0xc0;
        pub const NAME_LEN: u64 = 0xc8;
        pub const ONLINE: u64 = 0xd8;
        pub const ONLINE_LEN: u64 = 0xe0;
    }

    pub fn find(hay: &[u8]) -> Vec<usize> {
        hay.windows(SIG.len()).enumerate().filter(|(_, w)| *w == SIG).map(|(i, _)| i).collect()
    }
}

/// Every offset in `hay` where SIG matches.
pub fn find(hay: &[u8]) -> Vec<usize> {
    let mut out = vec![];
    if hay.len() < SIG.len() {
        return out;
    }
    let first = SIG[0].unwrap();
    for i in 0..=hay.len() - SIG.len() {
        if hay[i] == first && SIG.iter().zip(&hay[i..]).all(|(s, h)| s.map_or(true, |s| s == *h)) {
            out.push(i);
        }
    }
    out
}

/// Layout of the damage event record and of ApplyDamage's frame, read from the decompile of the old build
/// (analysis/pc_ApplyDamage.c; record = local_4f8 = rbp + 0x200, so local_X sits at record + 0x4f8 − X).
pub mod record {
    pub const SIZE: usize = 0x150;
    /// iVar12 = FUN_14121d8b0(impact): one of the two entities; the other is iVar1, stored at +0x148.
    /// Measured (2026-10-21, 84 rows): +0x148 reads 0 on 28 of them — only one of the two copy paths writes
    /// it — so a row is not always attributable to a target by this pair alone.
    pub const ENTITY_A: usize = 0x90;
    pub const ENTITY_B: usize = 0x148;
    /// `keen::ecs::BaseHitEvent` (kfc/types.json): rootSourceId 0x90 (the attacker) and targetId 0x94.
    pub const ROOT_SOURCE: usize = 0x90;
    pub const TARGET: usize = 0x94;
    /// Flags (uStack_460): 0x08 crit (random ≤ crit chance, line 1272), 0x20 backstab (local_res10, the ±angle test),
    /// 0x80 skill shot on a weak spot (DamageSusceptibility_SkillShot > 1, sets 0x40 too), 0x40 effective (a damage-type
    /// factor above 1.19, FUN_1403d84b0), 0x100 resisted (a factor below 0.81), 0x01 blocked, 0x200 no damage.
    pub const FLAGS: usize = 0x98;
    /// Health change (local_7b4: total of the per-type array after susceptibility and armor, negative = damage).
    pub const HEALTH: usize = 0xbc;
    /// Stamina change of a blocked hit (local_790).
    pub const STAMINA: usize = 0xc0;
    /// f32 (uStack_430 low half = 0x4f8 − 0x430): `uStack_430 = CONCAT44(.., (float)(int)-(local_790 + local_7b4) *
    /// 10.0)` at pc_ApplyDamage.c:3307, i.e. health + stamina loss times 10; zeroed at 3027 when neither is negative.
    pub const LOSS_X10: usize = 0xc8;
    /// impactId (`local_428 = (ulonglong)*(uint *)(local_res18 + 8) << 0x20;`, the high half of +0xD0): the live impact
    /// that dealt the hit (analysis/ammo.md §1).
    pub const IMPACT_ID: usize = 0xd4;
    /// FLAGS bit 0x01 (blocked): set when `cVar10 != 0` (pc_ApplyDamage.c:3365), where `cVar10 = (char)local_7ac`
    /// (3297); local_7ac's low byte is set to 1 on the paths that turn the hit into stamina loss (1727, 2245, 2895).
    pub const FLAG_BLOCKED: u32 = 0x01;
    /// FLAGS bit 0x200 (no damage): set when `local_7b0 != 0` (pc_ApplyDamage.c:3397); local_7b0 is set at 1637 on the
    /// path where the summed damage is forced to 0, and the same test appears at 3373.
    pub const FLAG_NO_DAMAGE: u32 = 0x200;
    /// Per-type damage as applied, 12 floats (local_708 = rbp − 0x10). Measured against a real session
    /// (2026-10-21, archer build, 84 rows): the slots sum to the recorded health change on 83 of them.
    /// Multi-type hits split across two slots and the secondary is exactly 0.30 × the primary, so a hit's
    /// total is the sum across slots, never the largest one. The one exception is a FLAG_BLOCKED row whose
    /// slots hold 71.16 while 0 damage landed — priced, then not applied.
    pub const FRAME_TYPES: isize = -0x10;
    pub const TYPES_LEN: usize = 0x30;

    /// The record's fields `ens calc log` reads, as named columns, so a row can leave the 0x150-byte hex record out.
    /// Offsets from `keen::ecs::BaseHitEvent` / `HitEvent` in kfc/types.json, the same as ens_calc::combat_log::record:
    /// programId 0x58, materialA 0x64, rootSourceId 0x90, targetId 0x94, rootTargetId 0xa0, sourceId 0xa4,
    /// permissionSourceId 0xa8, weaponPideId 0xac, weaponCategory 0xd8, attackCategory 0xdf, targetFaction 0xf0.
    pub const DECODED: [(&str, usize, usize); 11] = [
        ("root_source", 0x90, 4), ("target_id", 0x94, 4), ("root_target", 0xa0, 4), ("source", 0xa4, 4), ("permission", 0xa8, 4),
        ("weapon_category", 0xd8, 4), ("weapon_pide", 0xac, 4), ("program", 0x58, 4), ("material", 0x64, 4),
        ("attack_category", 0xdf, 1), ("target_faction", 0xf0, 1),
    ];

    /// The decoded columns' names, comma-separated, in [`DECODED`] order.
    pub fn decoded_header() -> String {
        DECODED.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(",")
    }

    /// The decoded columns of one record: unsigned decimal, in [`DECODED`] order.
    pub fn decoded_row(rec: &[u8]) -> String {
        DECODED.iter().map(|&(_, at, len)| {
            let mut v = [0u8; 4];
            v[..len].copy_from_slice(&rec[at..at + len]);
            u32::from_le_bytes(v).to_string()
        }).collect::<Vec<_>>().join(",")
    }
}

#[cfg(test)]
mod tests {
    /// Each decoded column reads its field from the record: 4-byte ids little-endian, 1-byte category and faction.
    #[test]
    fn decoded_columns_read_the_record_fields() {
        let mut rec = [0u8; super::record::SIZE];
        for (k, &(_, at, len)) in super::record::DECODED.iter().enumerate() {
            let v = 0x0102_0300u32 + k as u32 + 1;
            rec[at..at + len].copy_from_slice(&v.to_le_bytes()[..len]);
        }
        let row: Vec<u32> = super::record::decoded_row(&rec).split(',').map(|x| x.parse().unwrap()).collect();
        for (k, &(name, _, len)) in super::record::DECODED.iter().enumerate() {
            let v = 0x0102_0300u32 + k as u32 + 1;
            assert_eq!(row[k], if len == 1 { v & 0xff } else { v }, "{name}");
        }
        assert_eq!(super::record::decoded_header().split(',').count(), row.len());
    }

    /// The installed game (and the analysed copy) contain both hook points exactly once.
    #[test]
    #[ignore = "reads the installed game exe"]
    fn signature_is_unique_in_the_installed_game() {
        for exe in ["D:/SteamLibrary/steamapps/common/Enshrouded/enshrouded.exe", "../../../target/enshrouded.exe"] {
            let data = std::fs::read(exe).unwrap();
            let hits = super::find(&data);
            assert_eq!(hits.len(), 1, "{exe}");
            assert_eq!(data[hits[0] + super::PATCH_AT..][..super::PATCH_LEN], super::REPLACED, "{exe}");
            let hits = super::client::find(&data);
            assert_eq!(hits.len(), 1, "client hook in {exe}");
            assert_eq!(data[hits[0] + super::client::PATCH_AT..][..super::client::PATCH_LEN], super::client::REPLACED, "{exe}");
        }
    }
}

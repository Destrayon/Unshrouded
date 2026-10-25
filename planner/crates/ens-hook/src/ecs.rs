//! Reading an entity's attributes the way ApplyDamage does, from the three frame slots it fills once
//! (analysis/pc_ApplyDamage.c, old build; the asm writes each slot exactly once, analysis/fn_ApplyDamage_full.asm).
//! ApplyDamage fetches both entities' Level component but overwrites the result unused
//! (`FUN_1409b29a0(local_5f0,uVar18,puVar26[0x5f]);` then `local_5f0[0] = *plVar19;`), so the hook replays the
//! lookups itself. Every read goes through `Mem`, which fails instead of faulting; any failed read or failed type
//! check gives None and the column stays empty.

/// Frame slots, as offsets from ApplyDamage's rbp (local_X = rbp + 0x6f8 − X, see sig.rs `record`).
/// `local_770 = puVar21;` with `puVar21 = (undefined8 *)*puVar26;`: the entity store passed to FUN_1409a58c0.
/// asm: `mov qword ptr [rbp - 0x78], rsi`.
pub const FRAME_WORLD: isize = -0x78;
/// `local_778 = puVar26;` with `puVar26 = (undefined8 *)param_3[6];`: the component-id table.
/// asm: `mov qword ptr [rbp - 0x80], r13`.
pub const FRAME_COMPONENTS: isize = -0x80;
/// `local_768 = uVar25;` with `uVar25 = puVar26[0x1a];`: the attribute descriptor context passed to
/// FUN_140256680 / FUN_1402567e0. asm: `mov qword ptr [rbp - 0x70], rdi`.
pub const FRAME_ATTR_CTX: isize = -0x70;

/// Component slots in `puVar26` (analysis/component_slots.json maps slots 59+ to attribute ids, alphabetically).
/// `FUN_1409b29a0(local_5f0,uVar18,puVar26[0x5f]);` — Level (0x724bf568).
pub const SLOT_LEVEL: u64 = 0x5f;
/// `puVar21 = (undefined8 *)FUN_1409b29a0(local_178,uVar18,puVar26[0x59]);` — Health (0x8eb84995).
pub const SLOT_HEALTH: u64 = 0x59;
/// Attribute ids (kfc/items/attribute_ids.json): Level 1917580648, Health_Max 3699595454.
pub const ATTR_LEVEL: u32 = 0x724bf568;
pub const ATTR_HEALTH_MAX: u32 = 0xdc8358be;

/// Memory that may not be readable.
pub trait Mem {
    fn read(&self, addr: u64, out: &mut [u8]) -> bool;
    fn u64(&self, a: u64) -> Option<u64> {
        let mut b = [0; 8];
        self.read(a, &mut b).then(|| u64::from_le_bytes(b))
    }
    fn u32(&self, a: u64) -> Option<u32> {
        let mut b = [0; 4];
        self.read(a, &mut b).then(|| u32::from_le_bytes(b))
    }
    fn u16(&self, a: u64) -> Option<u16> {
        let mut b = [0; 2];
        self.read(a, &mut b).then(|| u16::from_le_bytes(b))
    }
    fn u8(&self, a: u64) -> Option<u8> {
        let mut b = [0; 1];
        self.read(a, &mut b).then(|| b[0])
    }
}

/// Open-addressing lookup shared by FUN_1409a8070 (entity ids, hashed) and FUN_14121bdf0 (attribute ids, not
/// hashed): `uVar3 = *(ulonglong *)(param_1 + 0x10);` capacity, `*(int *)(param_1 + 0x54) != 0` count, occupancy
/// bits at `*(longlong *)(param_1 + 8)`, keys at `*(longlong *)(param_1 + 0x28) + uVar4 * 4`. Returns the index.
fn find(m: &dyn Mem, map: u64, key: u32, hashed: bool) -> Option<u64> {
    let cap = m.u64(map + 0x10)?;
    if m.u32(map + 0x54)? == 0 || cap == 0 || cap > 1 << 28 {
        return None;
    }
    let h = if hashed {
        // `uVar5 = (uVar2 >> 0x10 ^ uVar2) * 0x45d9f3b;` twice, then `uVar5 >> 0x10 ^ uVar5`
        let mut x = (key >> 16 ^ key).wrapping_mul(0x45d9f3b);
        x = (x >> 16 ^ x).wrapping_mul(0x45d9f3b);
        x >> 16 ^ x
    } else {
        key
    };
    let (bits, keys) = (m.u64(map + 8)?, m.u64(map + 0x28)?);
    let mut i = h as u64 & (cap - 1);
    for _ in 0..cap {
        if m.u64(bits + (i >> 6) * 8)? >> (i & 63) & 1 == 0 {
            return None;
        }
        if m.u32(keys + i * 4)? == key {
            return Some(i);
        }
        i = if i + 1 < cap { i + 1 } else { 0 };
    }
    None
}

/// FUN_1409a58c0(world, id): `lVar2 = *(longlong *)(*param_1 + 0x30);`, index from the map at `lVar2 + 0x148`,
/// checked `uVar3 < *(ulonglong *)(lVar2 + 0x158)`, record `*(longlong *)(lVar2 + 0x188) + uVar3 * 8`.
pub fn entity(m: &dyn Mem, world: u64, id: u32) -> Option<u64> {
    let store = m.u64(m.u64(world)? + 0x30)?;
    let i = find(m, store + 0x148, id, true)?;
    if i >= m.u64(store + 0x158)? {
        return None;
    }
    let rec = m.u64(m.u64(store + 0x188)? + i * 8)?;
    (rec != 0).then_some(rec)
}

/// FUN_1409b29a0(out, record, puVar26[slot]): component id `uVar1 = *param_3;`, archetype `lVar2 = *(longlong
/// *)(param_2 + 0x18);`, presence bit at `lVar2 + (uVar1 >> 6) * 8`, address `*(ushort *)(lVar2 + 0x84 + uVar1 * 2)
/// + *(uint *)(param_2 + 0x30) * uVar3 + *(longlong *)(param_2 + 0x20)` with stride `uVar3 = *(ushort *)(lVar2 +
/// 0xa84 + uVar1 * 2)`.
pub fn component(m: &dyn Mem, components: u64, rec: u64, slot: u64) -> Option<u64> {
    let id = m.u16(m.u64(components + slot * 8)?)? as u64;
    let arch = m.u64(rec + 0x18)?;
    if m.u64(arch + (id >> 6) * 8)? >> (id & 63) & 1 == 0 {
        return None;
    }
    let off = m.u16(arch + 0x84 + id * 2)? as u64;
    let stride = m.u16(arch + 0xa84 + id * 2)? as u64;
    Some(off + m.u32(rec + 0x30)? as u64 * stride + m.u64(rec + 0x20)?)
}

/// FUN_1402567e0(out, component, ctx, attr): valid when `(*(byte *)((longlong)param_2 + 6) & 1) != 0`; descriptor
/// from FUN_14121bea0 → FUN_14121bdf0(ctx + 0x68, attr) (`uVar3 * 0x20 + *(longlong *)(param_1 + 0x40)`); its type
/// must match (`iVar4 != iVar3` fails); value `*(undefined4 *)((longlong)param_2 + (ulonglong)uVar2 * 4 +
/// (ulonglong)uVar1)` with `uVar1 = *(ushort *)(param_2 + 1)` and `uVar2 = *(ushort *)(local_10 + 3)`. Raw bits.
pub fn attribute(m: &dyn Mem, ctx: u64, comp: u64, attr: u32) -> Option<u32> {
    if m.u8(comp + 6)? & 1 == 0 {
        return None;
    }
    let descs = ctx + 0x68;
    let i = find(m, descs, attr, false)?;
    let desc = m.u64(descs + 0x40)? + i * 0x20;
    if m.u32(desc)? != m.u32(comp)? {
        return None;
    }
    m.u32(comp + m.u16(comp + 4)? as u64 + m.u16(desc + 0xc)? as u64 * 4)
}

/// The impact system in ApplyDamage's component table (analysis/ammo.md §2): `FUN_14121eff0(puVar26[0x22],(int)local_res18[8],iVar1)`,
/// asm 0x1403d5ecb `mov rcx, qword ptr [rsi + 0x110]`.
pub const TABLE_IMPACT_SYSTEM: u64 = 0x110;
/// The `ImpactItemConfig` component's slot in ApplyDamage's table: the impact_simulation component list's index 0x0b
/// plus the table's fixed offset 0x23 (Health 0x36 → `puVar26[0x59]`, Level 0x3c → `puVar26[0x5f]` agree).
pub const SLOT_IMPACT_ITEMS: u64 = 0x2e;
/// `ImpactItemConfig` (kfc/types.json): usedItem +0 and ammunitionItem +0x14, each an `ItemInstanceInfo` with itemId +8.
pub const USED_ITEM: u64 = 0x08;
pub const AMMO_ITEM: u64 = 0x1c;

/// FUN_14121d8e0(sys, id): the impact instance of a live impact id. `idx = id & 0xFFFFFF`, page `idx >> u8 [sys + 0xB0]`
/// (below `[sys + 0x90]`), slot `idx & [sys + 0xC0]`, page entry `[sys + 0x88] + page * 0x40` (slot below `[pe + 0x10]`),
/// element `[pe + 8] + slot * 0x200` whose generation `u32 [el] & 0x7FFFFFFF` must be `id >> 24`; instance `el + 8`.
pub fn impact_instance(m: &dyn Mem, sys: u64, id: u32) -> Option<u64> {
    if id == 0 {
        return None;
    }
    let idx = (id & 0xff_ffff) as u64;
    let page = idx >> m.u8(sys + 0xb0)?;
    let slot = idx & m.u64(sys + 0xc0)?;
    if page >= m.u64(sys + 0x90)? {
        return None;
    }
    let pe = m.u64(sys + 0x88)? + page * 0x40;
    if slot >= m.u64(pe + 0x10)? {
        return None;
    }
    let el = m.u64(pe + 8)? + slot * 0x200;
    (m.u32(el)? & 0x7fff_ffff == id >> 24).then_some(el + 8)
}

/// The items behind a hit, read from ApplyDamage's frame at `rbp` for the record's `impactId` (+0xD4): the impact
/// instance's entity (`mov dword ptr [r14 + 8], edi` in the instance init FUN_14121e4a0), its ImpactItemConfig, and
/// from it (ammunition item id, used item id): the arrow or spell, and the bow or staff. None when a read fails.
pub fn hit_items(m: &dyn Mem, rbp: u64, impact_id: u32) -> Option<(u32, u32)> {
    let at = |o: isize| m.u64(rbp.wrapping_add_signed(o as i64));
    let table = at(FRAME_COMPONENTS)?;
    let inst = impact_instance(m, m.u64(table + TABLE_IMPACT_SYSTEM)?, impact_id)?;
    let rec = entity(m, at(FRAME_WORLD)?, m.u32(inst + 8)?)?;
    let comp = component(m, table, rec, SLOT_IMPACT_ITEMS)?;
    Some((m.u32(comp + AMMO_ITEM)?, m.u32(comp + USED_ITEM)?))
}

/// One attribute of entity `id`, read from ApplyDamage's frame at `rbp`.
pub fn entity_attribute(m: &dyn Mem, rbp: u64, id: u32, slot: u64, attr: u32) -> Option<u32> {
    let at = |o: isize| m.u64(rbp.wrapping_add_signed(o as i64));
    let rec = entity(m, at(FRAME_WORLD)?, id)?;
    let comp = component(m, at(FRAME_COMPONENTS)?, rec, slot)?;
    attribute(m, at(FRAME_ATTR_CTX)?, comp, attr)
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeMap;

    /// Sparse memory: writes create bytes, reads of anything unwritten fail.
    #[derive(Default)]
    pub struct Fake(pub BTreeMap<u64, u8>);
    impl Fake {
        pub fn put(&mut self, a: u64, b: &[u8]) {
            for (i, x) in b.iter().enumerate() {
                self.0.insert(a + i as u64, *x);
            }
        }
        pub fn q(&mut self, a: u64, v: u64) {
            self.put(a, &v.to_le_bytes())
        }
        pub fn d(&mut self, a: u64, v: u32) {
            self.put(a, &v.to_le_bytes())
        }
        pub fn w(&mut self, a: u64, v: u16) {
            self.put(a, &v.to_le_bytes())
        }
        /// A map at `map` with one key in its home slot (`cap` 64), values at `vals`; returns the index used.
        pub fn map(&mut self, map: u64, key: u32, hashed: bool, vals: u64) -> u64 {
            let h = if hashed {
                let mut x = (key >> 16 ^ key).wrapping_mul(0x45d9f3b);
                x = (x >> 16 ^ x).wrapping_mul(0x45d9f3b);
                x >> 16 ^ x
            } else {
                key
            };
            let i = h as u64 & 63;
            let (bits, keys) = (map + 0x1000, map + 0x2000);
            self.q(map + 8, bits);
            self.q(map + 0x10, 64);
            self.q(map + 0x28, keys);
            self.q(map + 0x40, vals);
            self.d(map + 0x54, 1);
            self.q(bits, 1 << i);
            for k in 0..64 {
                self.d(keys + k * 4, if k == i { key } else { 0 });
            }
            i
        }
    }
    impl Mem for Fake {
        fn read(&self, a: u64, out: &mut [u8]) -> bool {
            for (i, o) in out.iter_mut().enumerate() {
                match self.0.get(&(a + i as u64)) {
                    Some(b) => *o = *b,
                    None => return false,
                }
            }
            true
        }
    }

    /// A frame at `rbp` from which entity `id` has Level `level` (raw bits). Returns the Fake.
    pub fn world_with(rbp: u64, id: u32, level: u32) -> Fake {
        let mut m = Fake::default();
        let (world, x, store, comps, ctx) = (0x10_0000, 0x11_0000, 0x12_0000, 0x20_0000, 0x30_0000);
        m.q(rbp.wrapping_add_signed(FRAME_WORLD as i64), world);
        m.q(rbp.wrapping_add_signed(FRAME_COMPONENTS as i64), comps);
        m.q(rbp.wrapping_add_signed(FRAME_ATTR_CTX as i64), ctx);
        m.q(world, x);
        m.q(x + 0x30, store);
        let recs = 0x13_0000;
        let i = m.map(store + 0x148, id, true, recs);
        let rec = 0x14_0000;
        m.q(recs + i * 8, rec);
        // component id 7 for the Level slot, row 2 of an archetype with offset 0x10, stride 0x40
        let cid_at = 0x21_0000;
        m.q(comps + SLOT_LEVEL * 8, cid_at);
        m.w(cid_at, 7);
        let arch = 0x15_0000;
        m.q(rec + 0x18, arch);
        m.q(rec + 0x20, 0x16_0000);
        m.d(rec + 0x30, 2);
        m.q(arch, 1 << 7);
        m.w(arch + 0x84 + 14, 0x10);
        m.w(arch + 0xa84 + 14, 0x40);
        let comp = 0x16_0000 + 0x10 + 2 * 0x40;
        // container type 0xabc, values from +0x8, flags valid; Level is value 1
        m.d(comp, 0xabc);
        m.w(comp + 4, 8);
        m.put(comp + 6, &[1]);
        m.d(comp + 8 + 4, level);
        let descs = 0x31_0000;
        let j = m.map(ctx + 0x68, ATTR_LEVEL, false, descs);
        m.d(descs + j * 0x20, 0xabc);
        m.w(descs + j * 0x20 + 0xc, 1);
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_the_entity_component_and_attribute_lookups() {
        let rbp = 0x40_0000;
        let m = fake::world_with(rbp, 1234, 25);
        assert_eq!(entity_attribute(&m, rbp, 1234, SLOT_LEVEL, ATTR_LEVEL), Some(25));
        assert_eq!(entity_attribute(&m, rbp, 99, SLOT_LEVEL, ATTR_LEVEL), None, "unknown entity");
        assert_eq!(entity_attribute(&m, rbp, 1234, SLOT_LEVEL, ATTR_HEALTH_MAX), None, "attribute not in the map");
        assert_eq!(entity_attribute(&m, rbp, 1234, SLOT_HEALTH, ATTR_LEVEL), None, "component slot unreadable");
        assert_eq!(entity_attribute(&m, rbp + 8, 1234, SLOT_LEVEL, ATTR_LEVEL), None, "frame unreadable");
    }

    /// An impact id resolves through the impact system's pages to its entity, whose ImpactItemConfig names the ammo and
    /// the weapon; a stale generation or an id past the pages does not.
    #[test]
    fn reads_the_ammo_and_weapon_of_a_hit() {
        let rbp = 0x40_0000;
        let mut m = fake::world_with(rbp, 1234, 25);
        let comps = 0x20_0000;
        let (sys, pages, elems) = (0x50_0000u64, 0x51_0000u64, 0x52_0000u64);
        m.q(comps + TABLE_IMPACT_SYSTEM, sys);
        m.put(sys + 0xb0, &[4]);
        m.q(sys + 0xc0, 0xf);
        m.q(sys + 0x90, 2);
        m.q(sys + 0x88, pages);
        // impact id: generation 3, page 1, slot 5 → element (page 1) at elems + 5 * 0x200
        let id = 3 << 24 | 1 << 4 | 5;
        m.q(pages + 0x40 + 8, elems);
        m.q(pages + 0x40 + 0x10, 16);
        let el = elems + 5 * 0x200;
        m.d(el, 0x8000_0003);
        m.d(el + 8 + 8, 1234);
        // ImpactItemConfig: component id 9 in the same archetype, offset 0x20, stride 0x40
        let cid_at = 0x21_0100;
        m.q(comps + SLOT_IMPACT_ITEMS * 8, cid_at);
        m.w(cid_at, 9);
        let arch = 0x15_0000;
        m.q(arch, 1 << 7 | 1 << 9);
        m.w(arch + 0x84 + 18, 0x20);
        m.w(arch + 0xa84 + 18, 0x40);
        let comp = 0x16_0000 + 0x20 + 2 * 0x40;
        m.d(comp + AMMO_ITEM, 397_674_071);
        m.d(comp + USED_ITEM, 4242);
        assert_eq!(hit_items(&m, rbp, id), Some((397_674_071, 4242)), "Eternal Frost Arrow I from its bow");
        assert_eq!(hit_items(&m, rbp, 4 << 24 | 1 << 4 | 5), None, "an older impact in the same slot");
        assert_eq!(hit_items(&m, rbp, 3 << 24 | 2 << 4 | 5), None, "past the pages");
        assert_eq!(hit_items(&m, rbp, 0), None);
    }
}

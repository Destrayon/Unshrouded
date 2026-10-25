//! Loader for the game's `.ksd` save files (`SV2C` container), enough to read a character: name, level, experience
//! and the unlocked skill-tree nodes. Layout and evidence: `docs/plans/save-character.md` sections 1 and 6.
//!
//! The file is only read. Nothing here is inferred from byte patterns: every offset below is one the plan documents.

use crate::{Bundle, SkillNode};
use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

const MAGIC: &[u8; 4] = b"SV2C";
const BLOCK_TAG: &[u8; 4] = b"S2BH";
const FILE_HEADER_LEN: usize = 48;
const BLOCK_HEADER_LEN: usize = 14;
const INDEX_RECORD_LEN: usize = 20;
/// Block type ids (`S2BH` header byte 8): plan section 1 and `FUN_140dcbf20` in section 6.
const TYPE_CHARACTER_DATA: u8 = 10;
const TYPE_PLAYER_CHARACTER: u8 = 9;
/// `player_knowledge` block type (save-character.md section 2: type 0x0f).
const TYPE_PLAYER_KNOWLEDGE: u8 = 15;
/// `player_character` offsets, plan section 6 (`FUN_140e1e320`).
const PC_LEVEL: usize = 58;
const PC_XP: usize = 62;
const PC_NODE_COUNT: usize = 66;
const PC_NODES: usize = 70;
const PC_NODE_LEN: usize = 5;
const PC_TAIL_LEN: usize = 22;
/// A decoded block larger than this is refused rather than allocated (the real blocks we read are a few KB).
const MAX_DECODED_BLOCK: u64 = 64 << 20;

#[derive(Debug)]
pub enum Error {
    Io { path: String, source: std::io::Error },
    /// The file is shorter than `what` needs.
    Truncated { what: &'static str, needed: usize, available: usize },
    BadMagic { found: [u8; 4] },
    /// The index (or a record in it) points outside the file.
    IndexOutOfFile { what: &'static str, offset: u64, len: u64, file_len: usize },
    BlockMissing { name: &'static str },
    BadBlockTag { name: &'static str, offset: usize },
    WrongBlockType { name: &'static str, expected: u8, found: u8 },
    UnknownFlag { name: &'static str, flag: u8 },
    Zstd { name: &'static str, message: String },
    DecodedTooLarge { name: &'static str },
    BadName(std::string::FromUtf8Error),
    /// The node count claims more records than the block holds.
    NodeCountOverrun { count: u32, block_len: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "read {path}: {source}"),
            Error::Truncated { what, needed, available } => {
                write!(f, "{what} is truncated: needs {needed} bytes, only {available} are there")
            }
            // KSC1 is the Early Access character format the 1.0 migration replaced
            Error::BadMagic { found } if found[..] == *b"KSC1" => write!(f, "this is an Early Access (pre-1.0) character file; the planner reads 1.0 saves (characterN.ksd written by the 1.0 game)"),
            Error::BadMagic { found } => write!(f, "not a save file: magic is {found:02x?}, expected \"SV2C\""),
            Error::IndexOutOfFile { what, offset, len, file_len } => {
                write!(f, "index points outside the file: {what} at {offset} (+{len}) in a {file_len}-byte file")
            }
            Error::BlockMissing { name } => write!(f, "the footer index has no `{name}` block"),
            Error::BadBlockTag { name, offset } => write!(f, "`{name}` block at {offset} does not start with S2BH"),
            Error::WrongBlockType { name, expected, found } => {
                write!(f, "`{name}` block has type {found}, expected {expected}")
            }
            Error::UnknownFlag { name, flag } => write!(f, "`{name}` block has compression flag {flag}, expected 0 or 1"),
            Error::Zstd { name, message } => write!(f, "`{name}` block: zstd decode failed: {message}"),
            Error::DecodedTooLarge { name } => write!(f, "`{name}` block decodes to more than {MAX_DECODED_BLOCK} bytes"),
            Error::BadName(e) => write!(f, "character name is not valid UTF-8: {e}"),
            Error::NodeCountOverrun { count, block_len } => write!(
                f,
                "player_character claims {count} skill nodes, which run past the end of its {block_len}-byte block"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// A character read from a save file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Save {
    pub name: String,
    /// The guid stored in `character_data`.
    pub guid: [u8; 16],
    pub level: u32,
    /// Experience inside the current level.
    pub xp: u32,
    /// Unlocked skill-tree nodes: `(node id, node level)`. Attributes are nodes too, one per point spent.
    pub nodes: Vec<(u32, u8)>,
    /// The last 22 bytes of `player_character`, kept raw (two u16, a u8, a bool and four u32; meaning unknown).
    pub tail: Vec<u8>,
    /// Equipment and inventory. Empty when the item part did not parse; `items_error` then says why, and the
    /// fields above are still good.
    pub items: Items,
    pub items_error: Option<String>,
    /// `player_knowledge` as (knowledge id, value) pairs; None when the block is missing or does not have the
    /// layout in docs/plans/found-skill-points.md.
    pub knowledge: Option<Vec<(u32, u32)>>,
}

/// One item record, as written by `FUN_140e1c3e0` (`docs/plans/save-character.md` §6). What the fields mean is
/// matched across every save on this machine in `docs/plans/save-gear.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveItem {
    /// `u8 rec+4`, written first: the slot number the record carries.
    pub slot: u8,
    /// `u32 rec+8`: FNV-1a 32 of the item's GUID bytes, see [`item_id`].
    pub id: u32,
    /// `u16 rec+0`: stack size.
    pub count: u16,
    /// `u8 rec+2` (mask bit 0): item level, including the +1 each unlocked perk adds.
    pub level: Option<u8>,
    /// `u8 rec+5`, `u8 rec+6` (mask bit 4): available perks, which the item factory `0x14040ab40` sets to
    /// min(rarity + 1, 5), and unlocked perks.
    pub perks: Option<(u8, u8)>,
}

impl SaveItem {
    /// Rarity index (0 Common … 4 Legendary, `keen::ItemRarity`) from the available-perk count, which is
    /// rarity + 1 capped at 5.
    pub fn rarity(&self) -> Option<u8> {
        self.perks.and_then(|(avail, _)| avail.checked_sub(1)).map(|r| r.min(4))
    }
}

/// The items in `player_character` after the skill nodes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Items {
    /// Items in fixed slots, keyed by mask bit = `keen::EquipmentSlot` value for 0–11 (5–9 head, chest, hands,
    /// legs, feet; 10–11 rings), then 12 shield, 13 grappling hook, 14 glider, 15 backpack.
    pub equipped: Vec<(u8, SaveItem)>,
    /// The backpack list, then the second list, in save order.
    pub inventory: Vec<SaveItem>,
    /// Position in `inventory` of the equipped arrow stack (mask bit 17) and spell (mask bit 18).
    pub arrow: Option<usize>,
    pub spell: Option<usize>,
}

/// Item id used in save records for a bundle item GUID (32 hex digits): FNV-1a 32 over the 16 GUID bytes.
pub fn item_id(guid: &str) -> Option<u32> {
    if guid.len() != 32 {
        return None;
    }
    let mut h: u32 = 0x811c_9dc5;
    for k in 0..16 {
        let byte = u8::from_str_radix(guid.get(2 * k..2 * k + 2)?, 16).ok()?;
        h = (h ^ byte as u32).wrapping_mul(0x0100_0193);
    }
    Some(h)
}

/// Reads little-endian values from a block, failing with the offset instead of panicking.
struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or_else(|| format!("runs past the item part's end at byte {}", self.at))?;
        let v = &self.b[self.at..end];
        self.at = end;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// One `FUN_140e1c3e0` record: u8 rec+4, u32 rec+8, u16 rec+0, u8 mask, then by mask bit 0 → u8 rec+2;
/// 1 → u32 rec+0xc, u32 rec+0x10; 2 → u32 rec+0x18, u32 rec+0x1c; 3 → u32 rec+0x24; 4 → u8 rec+5, u8 rec+6;
/// 5 → u32 rec+0x2c, u8 rec+0x30 (`save-character.md` §6, element records).
fn read_item(c: &mut Cursor) -> Result<SaveItem, String> {
    let at = c.at;
    let (slot, id, count, mask) = (c.u8()?, c.u32()?, c.u16()?, c.u8()?);
    if mask >> 6 != 0 {
        return Err(format!("item record at byte {at} has mask {mask:#x}; only bits 0-5 are known"));
    }
    let level = if mask & 1 != 0 { Some(c.u8()?) } else { None };
    for (bit, len) in [(2, 8), (4, 8), (8, 4)] {
        if mask & bit != 0 {
            c.take(len)?;
        }
    }
    let perks = if mask & 16 != 0 { Some((c.u8()?, c.u8()?)) } else { None };
    if mask & 32 != 0 {
        c.take(5)?;
    }
    Ok(SaveItem { slot, id, count, level, perks })
}

/// The item part of `player_character`: from the byte after the skill nodes to the 22-byte tail.
fn read_items(pc: &[u8], from: usize) -> Result<Items, String> {
    let mut c = Cursor { b: &pc[..pc.len() - PC_TAIL_LEN], at: from };
    // List 2: u32 count, then per element (FUN_140e19030) u32, u64, u64, u16 mask; by bit 0 → u8, 1 → u32,
    // 2 → f32, 3 → u8, 4 → u64, 5 → 9 × f32 and 4 × u32.
    let n2 = c.u32()?;
    for _ in 0..n2 {
        c.take(20)?;
        let m = c.u16()?;
        if m >> 6 != 0 {
            return Err(format!("list-2 element has mask {m:#x}; only bits 0-5 are known"));
        }
        for (bit, len) in [(1, 1), (2, 4), (4, 4), (8, 1), (16, 8), (32, 52)] {
            if m & bit != 0 {
                c.take(len)?;
            }
        }
    }
    // u32 mask: bits 0-15 an item in that fixed slot, bits 16-19 a u16 each; then four u8.
    let mask = c.u32()?;
    let mut items = Items::default();
    for bit in 0..16u8 {
        if mask >> bit & 1 != 0 {
            items.equipped.push((bit, read_item(&mut c)?));
        }
    }
    let mut refs = [None; 4];
    for (k, r) in refs.iter_mut().enumerate() {
        if mask >> (16 + k) & 1 != 0 {
            *r = Some(c.u16()? as usize);
        }
    }
    let four = [c.u8()?, c.u8()?, c.u8()?, c.u8()?];
    // The last list's count is not written; it is the 2nd + 4th of the four u8 in every save (§6), and landing
    // exactly on the tail checks it.
    for _ in 0..four[1] as usize + four[3] as usize {
        items.inventory.push(read_item(&mut c)?);
    }
    if c.at != c.b.len() {
        return Err(format!("the item list ends at byte {}, not at the tail at {}", c.at, c.b.len()));
    }
    let inside = |r: Option<usize>| r.filter(|&i| i < items.inventory.len());
    (items.arrow, items.spell) = (inside(refs[1]), inside(refs[2]));
    Ok(items)
}

/// A save's nodes resolved against the bundle's skill list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranks {
    /// Indexed by `bundle.skills` order, like `Build::ranks`.
    pub ranks: Vec<u8>,
    /// `bundle.skills` indices of nodes the save lists at level 0: unlocked with no rank.
    pub unlocked_zero_rank: Vec<usize>,
    /// Node ids in the save that no bundle skill has.
    pub unknown: Vec<u32>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn need(what: &'static str, data: &[u8], needed: usize) -> Result<(), Error> {
    if data.len() < needed {
        return Err(Error::Truncated { what, needed, available: data.len() });
    }
    Ok(())
}

/// The payload of the named block, decoded. `index` is the footer index of `file`.
fn block(file: &[u8], index: &[(String, usize, usize)], name: &'static str, ty: u8) -> Result<Vec<u8>, Error> {
    let &(_, offset, size) = index
        .iter()
        .find(|(n, _, _)| n == name)
        .ok_or(Error::BlockMissing { name })?;
    // The block is sized from the index, never to end of file: the index follows the last block and a zstd
    // decoder handed those bytes would try to read them as another frame.
    let raw = &file[offset..offset + size];
    if &raw[..4] != BLOCK_TAG {
        return Err(Error::BadBlockTag { name, offset });
    }
    let (found, flag) = (raw[8], raw[9]);
    if found != ty {
        return Err(Error::WrongBlockType { name, expected: ty, found });
    }
    let payload = &raw[BLOCK_HEADER_LEN..];
    match flag {
        0 => Ok(payload.to_vec()),
        1 => {
            let zstd_err = |message: String| Error::Zstd { name, message };
            let decoder = ruzstd::decoding::StreamingDecoder::new(payload).map_err(|e| zstd_err(e.to_string()))?;
            let mut out = Vec::new();
            decoder
                .take(MAX_DECODED_BLOCK + 1)
                .read_to_end(&mut out)
                .map_err(|e| zstd_err(e.to_string()))?;
            if out.len() as u64 > MAX_DECODED_BLOCK {
                return Err(Error::DecodedTooLarge { name });
            }
            Ok(out)
        }
        flag => Err(Error::UnknownFlag { name, flag }),
    }
}

/// Footer index as `(name, block offset, block size)`, every entry checked to lie inside the file.
fn read_index(file: &[u8]) -> Result<Vec<(String, usize, usize)>, Error> {
    need("file header", file, FILE_HEADER_LEN)?;
    if &file[..4] != MAGIC {
        return Err(Error::BadMagic { found: file[..4].try_into().unwrap() });
    }
    let file_len = file.len();
    let outside = |what, offset: u64, len: u64| Error::IndexOutOfFile { what, offset, len, file_len };
    let within = |offset: u64, len: u64| offset.checked_add(len).is_some_and(|end| end <= file_len as u64);

    // u32 at +40 is the index offset minus 40; u32 at +44 is the block count.
    let index_at = u32_at(file, 40) as u64 + 40;
    let count = u32_at(file, 44) as u64;
    let records_len = count * INDEX_RECORD_LEN as u64;
    if !within(index_at, records_len) {
        return Err(outside("footer index", index_at, records_len));
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let rec = (index_at + i * INDEX_RECORD_LEN as u64) as usize;
        // name_offset counts from the start of this record.
        let (name_off, name_len) = (u32_at(file, rec) as u64, u32_at(file, rec + 4) as u64);
        let (blk_off, blk_size) = (u32_at(file, rec + 8) as u64, u32_at(file, rec + 12) as u64);
        let name_at = rec as u64 + name_off;
        if !within(name_at, name_len) {
            return Err(outside("block name", name_at, name_len));
        }
        if blk_size < BLOCK_HEADER_LEN as u64 || !within(blk_off, blk_size) {
            return Err(outside("block", blk_off, blk_size));
        }
        let name = String::from_utf8_lossy(&file[name_at as usize..(name_at + name_len) as usize]).into_owned();
        out.push((name, blk_off as usize, blk_size as usize));
    }
    Ok(out)
}

impl Save {
    pub fn load(path: &Path) -> Result<Save, Error> {
        let file = std::fs::read(path).map_err(|source| Error::Io { path: path.display().to_string(), source })?;
        Save::parse(&file)
    }

    pub fn parse(file: &[u8]) -> Result<Save, Error> {
        let index = read_index(file)?;

        // character_data: u8 slot, four constant bytes, u32 name length, the name, the 16-byte guid.
        let cd = block(file, &index, "character_data", TYPE_CHARACTER_DATA)?;
        need("character_data", &cd, 9)?;
        let name_len = u32_at(&cd, 5) as usize;
        let name_end = 9usize.saturating_add(name_len);
        need("character_data", &cd, name_end.saturating_add(16))?;
        let name = String::from_utf8(cd[9..name_end].to_vec()).map_err(Error::BadName)?;
        let guid: [u8; 16] = cd[name_end..name_end + 16].try_into().unwrap();

        let pc = block(file, &index, "player_character", TYPE_PLAYER_CHARACTER)?;
        need("player_character", &pc, PC_NODES.max(PC_TAIL_LEN))?;
        let count = u32_at(&pc, PC_NODE_COUNT);
        let nodes_end = PC_NODES as u64 + count as u64 * PC_NODE_LEN as u64;
        if nodes_end > pc.len() as u64 {
            return Err(Error::NodeCountOverrun { count, block_len: pc.len() });
        }
        let nodes = (0..count as usize)
            .map(|i| {
                let at = PC_NODES + i * PC_NODE_LEN;
                (u32_at(&pc, at), pc[at + 4])
            })
            .collect();

        let knowledge = block(file, &index, "player_knowledge", TYPE_PLAYER_KNOWLEDGE).ok().and_then(|b| read_knowledge(&b));
        let (items, items_error) = match read_items(&pc, nodes_end as usize) {
            Ok(items) => (items, None),
            Err(e) => (Items::default(), Some(e)),
        };
        Ok(Save {
            name,
            guid,
            level: u32_at(&pc, PC_LEVEL),
            xp: u32_at(&pc, PC_XP),
            nodes,
            tail: pc[pc.len() - PC_TAIL_LEN..].to_vec(),
            items,
            items_error,
            knowledge,
        })
    }

    /// Resolve the save's nodes against the bundle's skills. A node at level 0 adds no rank but is reported in
    /// `unlocked_zero_rank`; an id the bundle does not have is reported in `unknown`, never dropped.
    pub fn ranks(&self, bundle: &Bundle) -> Ranks {
        let by_id: HashMap<u32, &SkillNode> = bundle.skills.iter().map(|s| (s.id, s)).collect();
        let len = bundle.skills.iter().map(|s| s.index + 1).max().unwrap_or(0).max(bundle.skills.len());
        let mut out = Ranks { ranks: vec![0; len], unlocked_zero_rank: Vec::new(), unknown: Vec::new() };
        for &(id, level) in &self.nodes {
            match by_id.get(&id) {
                None => out.unknown.push(id),
                Some(skill) if level == 0 => out.unlocked_zero_rank.push(skill.index),
                Some(skill) => out.ranks[skill.index] = level,
            }
        }
        out
    }
}

/// `player_knowledge`: u32 1, u32 n, n u32 knowledge ids, n u32 values. Matched on all four character saves
/// (`8 + 8n` = the block length, ids unique); docs/plans/found-skill-points.md.
fn read_knowledge(b: &[u8]) -> Option<Vec<(u32, u32)>> {
    if b.len() < 8 || u32_at(b, 0) != 1 {
        return None;
    }
    let n = u32_at(b, 4) as usize;
    if 8usize.checked_add(n.checked_mul(8)?)? != b.len() {
        return None;
    }
    Some((0..n).map(|k| (u32_at(b, 8 + 4 * k), u32_at(b, 8 + 4 * (n + k)))).collect())
}

impl Save {
    /// Skill points from the knowledge unlocks the character has (Shroud Roots, Elixir Wells): the amount of every
    /// `SKILL_POINT_UNLOCKS` id present with a non-zero value. None when the knowledge block was not read.
    pub fn found_skill_points(&self) -> Option<u32> {
        let k = self.knowledge.as_ref()?;
        let table: HashMap<u32, u32> = crate::skill_point_unlocks::SKILL_POINT_UNLOCKS.iter().copied().collect();
        Some(k.iter().filter(|(_, v)| *v != 0).filter_map(|(id, _)| table.get(id)).sum())
    }
}

/// Read a save file. See [`Save`].
pub fn load(path: &Path) -> Result<Save, Error> {
    Save::load(path)
}

/// Steam app id of Enshrouded; its cloud saves sit in `Steam/userdata/<account>/1203620/remote`.
const APP_ID: &str = "1203620";
const DEFAULT_STEAM: &str = "C:/Program Files (x86)/Steam";

/// A character save on disk and when the game last wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveFile {
    pub path: std::path::PathBuf,
    pub modified: std::time::SystemTime,
}

/// The folders that hold character saves: `ENS_SAVE_DIR` if set (one folder, or several joined with `;`),
/// otherwise every `userdata/*/1203620/remote` under `ENS_STEAM_DIR` or the default Steam install.
pub fn save_dirs() -> Vec<std::path::PathBuf> {
    if let Ok(v) = std::env::var("ENS_SAVE_DIR") {
        return v.split(';').filter(|s| !s.is_empty()).map(std::path::PathBuf::from).collect();
    }
    let steam = std::env::var("ENS_STEAM_DIR").unwrap_or_else(|_| DEFAULT_STEAM.into());
    let Ok(accounts) = std::fs::read_dir(Path::new(&steam).join("userdata")) else { return Vec::new() };
    let mut out: Vec<_> = accounts
        .flatten()
        .map(|a| a.path().join(APP_ID).join("remote"))
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// `characterN.ksd` files in `dirs`, most recently written first. Backups (in `remote/backup`) are not listed.
pub fn character_saves(dirs: &[std::path::PathBuf]) -> Vec<SaveFile> {
    let mut out: Vec<SaveFile> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.strip_prefix("character")
                .and_then(|r| r.strip_suffix(".ksd"))
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
        })
        .filter_map(|e| Some(SaveFile { modified: e.metadata().ok()?.modified().ok()?, path: e.path() }))
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
    out
}

/// `world_metadata` block type and offsets (save-character.md section 7, matched across five worlds).
const TYPE_WORLD_METADATA: u8 = 11;
const WM_SAVED: usize = 45;
const WM_FLAME: usize = 53;
const WM_NAME_LEN: usize = 57;
const WM_NAME: usize = 61;

/// A world save: its name, Flame level and when the game last wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    pub path: std::path::PathBuf,
    pub name: String,
    pub flame_level: u32,
    /// Unix seconds from `world_metadata` (the time the world was last saved).
    pub saved_unix: u64,
}

/// Read the `world_metadata` block of a `worldN.ksd` file.
pub fn read_world(path: &Path) -> Result<World, Error> {
    let file = std::fs::read(path).map_err(|source| Error::Io { path: path.display().to_string(), source })?;
    let index = read_index(&file)?;
    let wm = block(&file, &index, "world_metadata", TYPE_WORLD_METADATA)?;
    need("world_metadata", &wm, WM_NAME)?;
    let name_end = WM_NAME.saturating_add(u32_at(&wm, WM_NAME_LEN) as usize);
    need("world_metadata", &wm, name_end)?;
    Ok(World {
        path: path.to_path_buf(),
        name: String::from_utf8(wm[WM_NAME..name_end].to_vec()).map_err(Error::BadName)?,
        flame_level: u32_at(&wm, WM_FLAME),
        saved_unix: u32_at(&wm, WM_SAVED) as u64 | (u32_at(&wm, WM_SAVED + 4) as u64) << 32,
    })
}

/// `worldN.ksd` files in `dirs` that read, most recently saved first.
pub fn world_saves(dirs: &[std::path::PathBuf]) -> Vec<World> {
    let mut out: Vec<World> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.strip_prefix("world")
                .and_then(|r| r.strip_suffix(".ksd"))
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
        })
        .filter_map(|e| read_world(&e.path()).ok())
        .collect();
    out.sort_by(|a, b| b.saved_unix.cmp(&a.saved_unix).then(a.path.cmp(&b.path)));
    out
}

/// The world played at `unix` seconds: the game saves the world and the character together, so this is the world
/// saved closest to that time. A character save does not name its world.
pub fn world_at(worlds: &[World], unix: u64) -> Option<&World> {
    worlds.iter().min_by_key(|w| w.saved_unix.abs_diff(unix))
}

/// Writing a character file, for a throwaway test character (docs/test-character.md). Only the payload of the edited
/// block changes; every other byte is copied: the header (with its file size and index offset updated, its save
/// counter at +28 raised by one and its save time at +32 set to now), the other blocks, each block header's version and
/// schema field. Each index record's `hash` is the low 32 bits of XXH3-64 over the raw block (14-byte header + stored
/// payload): 574 of 574 blocks on this machine (docs/plans/save-hash.md); the edited block gets its new one, the rest
/// keep theirs. The index is 16-byte aligned with zero padding, as in every save on this machine.
pub mod write {
    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    /// The footer index's per-block hash: low 32 bits of XXH3-64 over the raw block.
    pub fn block_hash(raw: &[u8]) -> u32 {
        xxhash_rust::xxh3::xxh3_64(raw) as u32
    }

    /// `file` with block `name` (of type `ty`) decoded, passed to `edit`, and stored again (zstd when it was).
    pub fn rewrite_block(file: &[u8], name: &'static str, ty: u8, edit: impl FnOnce(&mut Vec<u8>)) -> Result<Vec<u8>, Error> {
        let index = read_index(file)?;
        let at = u32_at(file, 40) as usize + 40;
        let count = u32_at(file, 44) as usize;
        let mut order: Vec<usize> = (0..index.len()).collect();
        order.sort_by_key(|&i| index[i].1);
        let mut out = file[..FILE_HEADER_LEN].to_vec();
        let mut placed = vec![(0usize, 0usize, None::<u32>); index.len()];
        let mut edit = Some(edit);
        for &i in &order {
            let (n, off, size) = &index[i];
            let raw = &file[*off..off + size];
            let start = out.len();
            if n == name {
                let mut payload = block(file, &index, name, ty)?;
                if let Some(e) = edit.take() {
                    e(&mut payload);
                }
                out.extend(&raw[..BLOCK_HEADER_LEN]);
                if raw[9] == 1 {
                    out.extend(compress_to_vec(&payload[..], CompressionLevel::Fastest));
                } else {
                    out.extend(&payload);
                }
            } else {
                out.extend(raw);
            }
            let rehash = (n == name).then(|| block_hash(&out[start..]));
            placed[i] = (start, out.len() - start, rehash);
        }
        while out.len() % 16 != 0 {
            out.push(0);
        }
        let new_at = out.len();
        for i in 0..count {
            let rec = &file[at + i * INDEX_RECORD_LEN..at + (i + 1) * INDEX_RECORD_LEN];
            out.extend(&rec[..8]); // name offset (relative to the record) and length: unchanged
            out.extend((placed[i].0 as u32).to_le_bytes());
            out.extend((placed[i].1 as u32).to_le_bytes());
            match placed[i].2 {
                Some(h) => out.extend(h.to_le_bytes()),
                None => out.extend(&rec[16..20]), // unchanged block: its hash still holds
            }
        }
        out.extend(&file[at + count * INDEX_RECORD_LEN..]); // the names, and anything after them
        let size = out.len() as u32;
        out[8..12].copy_from_slice(&size.to_le_bytes());
        out[40..44].copy_from_slice(&((new_at - 40) as u32).to_le_bytes());
        let counter = u32_at(file, 28).wrapping_add(1);
        out[28..32].copy_from_slice(&counter.to_le_bytes());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(u32_at(file, 32));
        out[32..36].copy_from_slice(&now.max(u32_at(file, 32)).to_le_bytes());
        Ok(out)
    }

    /// What to change in a character (docs/test-character.md): level (experience in it set to 0), the skill-node
    /// list as (node id, level) pairs, and item records to add to the backpack. A record is written as given except
    /// its first byte, the slot, which becomes the next free backpack slot.
    #[derive(Debug, Clone, Default)]
    pub struct CharacterEdit {
        pub level: Option<u32>,
        pub nodes: Option<Vec<(u32, u8)>>,
        pub backpack: Vec<Vec<u8>>,
    }

    /// Byte positions in `player_character`'s item part (layout as `read_items` walks it): the four u8 after the
    /// fixed slots, the arrow / spell / other references, and where each inventory record starts and ends.
    struct ItemLayout {
        refs: Vec<usize>,
        four: usize,
        records: Vec<(usize, usize)>,
    }

    fn item_layout(pc: &[u8], from: usize) -> Result<ItemLayout, String> {
        let mut c = Cursor { b: &pc[..pc.len() - PC_TAIL_LEN], at: from };
        let n2 = c.u32()?;
        for _ in 0..n2 {
            c.take(20)?;
            let m = c.u16()?;
            for (bit, len) in [(1, 1), (2, 4), (4, 4), (8, 1), (16, 8), (32, 52)] {
                if m & bit != 0 {
                    c.take(len)?;
                }
            }
        }
        let mask = c.u32()?;
        for bit in 0..16 {
            if mask >> bit & 1 != 0 {
                read_item(&mut c)?;
            }
        }
        let mut refs = vec![];
        for k in 0..4 {
            if mask >> (16 + k) & 1 != 0 {
                refs.push(c.at);
                c.u16()?;
            }
        }
        let four = c.at;
        let counts = [c.u8()?, c.u8()?, c.u8()?, c.u8()?];
        let mut records = vec![];
        for _ in 0..counts[1] as usize + counts[3] as usize {
            let start = c.at;
            read_item(&mut c)?;
            records.push((start, c.at));
        }
        if c.at != c.b.len() {
            return Err(format!("the item list ends at byte {}, not at the tail at {}", c.at, c.b.len()));
        }
        Ok(ItemLayout { refs, four, records })
    }

    /// The new `player_character` payload for `edit`.
    pub fn edited_player_character(pc: &[u8], edit: &CharacterEdit) -> Result<Vec<u8>, String> {
        let mut pc = pc.to_vec();
        if let Some(level) = edit.level {
            pc[PC_LEVEL..PC_LEVEL + 4].copy_from_slice(&level.to_le_bytes());
            pc[PC_XP..PC_XP + 4].copy_from_slice(&0u32.to_le_bytes());
        }
        let old = u32_at(&pc, PC_NODE_COUNT) as usize;
        let old_end = PC_NODES + old * PC_NODE_LEN;
        if let Some(nodes) = &edit.nodes {
            let mut out = pc[..PC_NODE_COUNT].to_vec();
            out.extend((nodes.len() as u32).to_le_bytes());
            for (id, lv) in nodes {
                out.extend(id.to_le_bytes());
                out.push(*lv);
            }
            out.extend(&pc[old_end..]);
            pc = out;
        }
        if !edit.backpack.is_empty() {
            let nodes_end = PC_NODES + u32_at(&pc, PC_NODE_COUNT) as usize * PC_NODE_LEN;
            let lay = item_layout(&pc, nodes_end)?;
            let in_backpack = pc[lay.four + 1] as usize;
            let add = edit.backpack.len();
            if in_backpack + add > 255 {
                return Err("more than 255 backpack records".into());
            }
            let next_slot = lay.records[..in_backpack].iter().map(|&(s, _)| pc[s] as usize + 1).max().unwrap_or(0);
            let insert_at = lay.records.get(in_backpack).map(|r| r.0).unwrap_or(pc.len() - PC_TAIL_LEN);
            // references point at a position in the inventory list; those past the backpack move with the insert
            for &r in &lay.refs {
                let v = u16::from_le_bytes([pc[r], pc[r + 1]]) as usize;
                if v >= in_backpack {
                    pc[r..r + 2].copy_from_slice(&((v + add) as u16).to_le_bytes());
                }
            }
            pc[lay.four + 1] = (in_backpack + add) as u8;
            let mut recs = vec![];
            for (k, r) in edit.backpack.iter().enumerate() {
                let mut r = r.clone();
                r[0] = (next_slot + k) as u8;
                recs.extend(r);
            }
            pc.splice(insert_at..insert_at, recs);
        }
        Ok(pc)
    }

    /// `file` with `edit` applied to its character.
    pub fn edit_character(file: &[u8], edit: &CharacterEdit) -> Result<Vec<u8>, Error> {
        let index = read_index(file)?;
        let pc = block(file, &index, "player_character", TYPE_PLAYER_CHARACTER)?;
        let new = edited_player_character(&pc, edit).map_err(|message| Error::Zstd { name: "player_character", message })?;
        rewrite_block(file, "player_character", TYPE_PLAYER_CHARACTER, |p| *p = new)
    }

    /// Set the character's level (`player_character` +58) with no experience inside it (+62).
    pub fn set_level(file: &[u8], level: u32) -> Result<Vec<u8>, Error> {
        rewrite_block(file, "player_character", TYPE_PLAYER_CHARACTER, |pc| {
            if pc.len() >= PC_XP + 4 {
                pc[PC_LEVEL..PC_LEVEL + 4].copy_from_slice(&level.to_le_bytes());
                pc[PC_XP..PC_XP + 4].copy_from_slice(&0u32.to_le_bytes());
            }
        })
    }
}

/// Builders for synthetic save files, for tests here and in the crates that read saves.
#[doc(hidden)]
pub mod fixture {
    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    pub fn character_data(name: &str, guid: [u8; 16]) -> Vec<u8> {
        let mut v = vec![4, 0x93, 0x7b, 0x40, 0xb6];
        v.extend((name.len() as u32).to_le_bytes());
        v.extend(name.as_bytes());
        v.extend(guid);
        v.extend([0u8; 30]); // the rest of the block, not read
        v
    }

    pub fn player_character(level: u32, xp: u32, nodes: &[(u32, u8)], count_override: Option<u32>) -> Vec<u8> {
        let mut v = vec![0u8; PC_NODES];
        v[PC_LEVEL..PC_LEVEL + 4].copy_from_slice(&level.to_le_bytes());
        v[PC_XP..PC_XP + 4].copy_from_slice(&xp.to_le_bytes());
        let count = count_override.unwrap_or(nodes.len() as u32);
        v[PC_NODE_COUNT..PC_NODE_COUNT + 4].copy_from_slice(&count.to_le_bytes());
        for &(id, lvl) in nodes {
            v.extend(id.to_le_bytes());
            v.push(lvl);
        }
        v.extend([0xAA; 9]); // stands in for the lists between the nodes and the tail
        v.extend(7u16.to_le_bytes());
        v.extend(9u16.to_le_bytes());
        v.extend(1u8..=18); // 22-byte tail: 4 bytes above + these 18
        v
    }

    pub fn block_bytes(ty: u8, zstd: bool, payload: &[u8]) -> Vec<u8> {
        let mut b = b"S2BH".to_vec();
        b.extend(2u32.to_le_bytes());
        b.extend([ty, zstd as u8]);
        b.extend(5u32.to_le_bytes());
        if zstd {
            b.extend(compress_to_vec(payload, CompressionLevel::Fastest));
        } else {
            b.extend(payload);
        }
        b
    }

    /// A container with the two blocks, `padding` bytes before the index, and names after the records.
    pub fn container(blocks: &[(&str, Vec<u8>)], padding: usize, guid: [u8; 16]) -> Vec<u8> {
        let mut f = vec![0u8; FILE_HEADER_LEN];
        f[..4].copy_from_slice(b"SV2C");
        f[4..6].copy_from_slice(&2u16.to_le_bytes());
        f[6..8].copy_from_slice(&48u16.to_le_bytes());
        f[12..28].copy_from_slice(&guid);
        let mut spans = Vec::new();
        for (_, b) in blocks {
            spans.push((f.len(), b.len()));
            f.extend(b);
        }
        f.extend(vec![0xEE; padding]);
        let index_at = f.len();
        let mut name_pos = blocks.len() * INDEX_RECORD_LEN;
        let mut names: Vec<u8> = Vec::new();
        for (i, (name, _)) in blocks.iter().enumerate() {
            let rec = i * INDEX_RECORD_LEN;
            f.extend(((name_pos - rec) as u32).to_le_bytes());
            f.extend((name.len() as u32).to_le_bytes());
            f.extend((spans[i].0 as u32).to_le_bytes());
            f.extend((spans[i].1 as u32).to_le_bytes());
            f.extend(super::write::block_hash(&blocks[i].1).to_le_bytes()); // as the game writes it
            names.extend(name.as_bytes());
            name_pos += name.len();
        }
        f.extend(names);
        let size = f.len() as u32;
        f[8..12].copy_from_slice(&size.to_le_bytes());
        f[40..44].copy_from_slice(&((index_at - 40) as u32).to_le_bytes());
        f[44..48].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
        f
    }

    /// A world file whose `world_metadata` has the layout of save-character.md section 7.
    pub fn world_file(name: &str, flame: u32, saved_unix: u64) -> Vec<u8> {
        let mut wm = vec![0u8; WM_SAVED];
        wm.extend(saved_unix.to_le_bytes());
        wm.extend(flame.to_le_bytes());
        wm.extend((name.len() as u32).to_le_bytes());
        wm.extend(name.as_bytes());
        wm.extend([0u8; 16]);
        container(&[("world_metadata", block_bytes(TYPE_WORLD_METADATA, false, &wm))], 0, [0; 16])
    }

    /// One item record in `FUN_140e1c3e0` order, with only the level and perk groups.
    pub fn item_record(slot: u8, id: u32, count: u16, level: Option<u8>, perks: Option<(u8, u8)>) -> Vec<u8> {
        let mask = level.is_some() as u8 | (perks.is_some() as u8) << 4;
        let mut v = vec![slot];
        v.extend(id.to_le_bytes());
        v.extend(count.to_le_bytes());
        v.push(mask);
        v.extend(level);
        if let Some((a, u)) = perks {
            v.extend([a, u]);
        }
        v
    }

    /// The item part of `player_character`: no list-2 elements, the fixed slots, arrow and spell references, and
    /// the backpack (the second list empty).
    pub fn item_section(equipped: &[(u8, Vec<u8>)], arrow: Option<u16>, spell: Option<u16>, backpack: &[Vec<u8>]) -> Vec<u8> {
        let mut mask = 0u32;
        for (bit, _) in equipped {
            mask |= 1 << bit;
        }
        mask |= (arrow.is_some() as u32) << 17 | (spell.is_some() as u32) << 18;
        let mut v = 0u32.to_le_bytes().to_vec();
        v.extend(mask.to_le_bytes());
        for (_, r) in equipped {
            v.extend(r);
        }
        for r in [arrow, spell].into_iter().flatten() {
            v.extend(r.to_le_bytes());
        }
        v.extend([5, backpack.len() as u8, 0, 0]);
        for r in backpack {
            v.extend(r);
        }
        v
    }

    /// A `player_knowledge` block payload from (knowledge id, value) pairs.
    pub fn knowledge_block(pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut v = 1u32.to_le_bytes().to_vec();
        v.extend((pairs.len() as u32).to_le_bytes());
        for (id, _) in pairs {
            v.extend(id.to_le_bytes());
        }
        for (_, val) in pairs {
            v.extend(val.to_le_bytes());
        }
        v
    }

    /// A character file with a `player_knowledge` block holding `pairs`.
    pub fn character_file_with_knowledge(name: &str, level: u32, pairs: &[(u32, u32)]) -> Vec<u8> {
        let guid = [7u8; 16];
        container(
            &[
                ("character_data", block_bytes(TYPE_CHARACTER_DATA, true, &character_data(name, guid))),
                ("player_character", block_bytes(TYPE_PLAYER_CHARACTER, true, &player_character(level, 0, &[], None))),
                ("player_knowledge", block_bytes(TYPE_PLAYER_KNOWLEDGE, true, &knowledge_block(pairs))),
            ],
            0,
            guid,
        )
    }

    /// A character file with a real item part (from [`item_section`]) between the nodes and the tail.
    pub fn character_file_with_items(name: &str, level: u32, xp: u32, nodes: &[(u32, u8)], items: &[u8]) -> Vec<u8> {
        let guid = [7u8; 16];
        let mut pc = player_character(level, xp, nodes, None);
        let at = PC_NODES + nodes.len() * PC_NODE_LEN;
        pc.splice(at..at + 9, items.iter().copied());
        container(
            &[
                ("character_data", block_bytes(TYPE_CHARACTER_DATA, true, &character_data(name, guid))),
                ("player_character", block_bytes(TYPE_PLAYER_CHARACTER, true, &pc)),
            ],
            0,
            guid,
        )
    }

    /// A two-block character file this reader accepts: `name`, `level`, `xp` and `nodes`, both blocks zstd.
    pub fn character_file(name: &str, level: u32, xp: u32, nodes: &[(u32, u8)]) -> Vec<u8> {
        let guid = [7u8; 16];
        container(
            &[
                ("character_data", block_bytes(TYPE_CHARACTER_DATA, true, &character_data(name, guid))),
                ("player_character", block_bytes(TYPE_PLAYER_CHARACTER, true, &player_character(level, xp, nodes, None))),
            ],
            0,
            guid,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use crate::default_bundle_path;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
    }

    /// Nodes recorded from character4 (Roth) on 2026-10-21.
    const NODES: [(u32, u8); 31] = [
        (134064960, 1), (2282076069, 1), (856922754, 3), (1442728401, 3), (137870545, 1), (226037501, 3),
        (1311628529, 0), (2506510512, 0), (770186136, 0), (521283386, 3), (1542383404, 3), (2877121847, 0),
        (1573526808, 1), (3179300754, 1), (3060709718, 1), (1417816412, 0), (3651771067, 1), (4292213056, 1),
        (802872125, 0), (289243756, 0), (2116701756, 0), (1313439627, 1), (3005573120, 3), (3844459022, 1),
        (2369077220, 1), (494749233, 1), (2701798535, 1), (2778233134, 0), (2757603307, 1), (530860338, 1),
        (3040026531, 1),
    ];

    fn sample(cd_zstd: bool, pc_zstd: bool, padding: usize, pc: Vec<u8>) -> Vec<u8> {
        let guid = [7u8; 16];
        container(
            &[
                ("character_data", block_bytes(TYPE_CHARACTER_DATA, cd_zstd, &character_data("Roth", guid))),
                ("player_character", block_bytes(TYPE_PLAYER_CHARACTER, pc_zstd, &pc)),
            ],
            padding,
            guid,
        )
    }

    #[test]
    fn reads_fields_from_stored_and_zstd_blocks() {
        let nodes = [(10u32, 3u8), (20, 0), (30, 1)];
        // Either block can be the compressed one; the last block is followed by padding and then the index.
        for (cd_zstd, pc_zstd) in [(false, true), (true, false), (true, true)] {
            for padding in [0, 7, 15] {
                let file = sample(cd_zstd, pc_zstd, padding, player_character(14, 1234, &nodes, None));
                let s = Save::parse(&file).unwrap_or_else(|e| panic!("zstd {cd_zstd}/{pc_zstd} pad {padding}: {e}"));
                assert_eq!(s.name, "Roth");
                assert_eq!(s.guid, [7u8; 16]);
                assert_eq!((s.level, s.xp), (14, 1234));
                assert_eq!(s.nodes, nodes);
                assert_eq!(s.tail.len(), 22);
                assert_eq!(&s.tail[..4], &[7, 0, 9, 0]);
                assert_eq!(u16_at(&s.tail, 0), 7);
                assert_eq!(u16_at(&s.tail, 2), 9);
            }
        }
    }

    #[test]
    fn load_reads_a_file_from_disk() {
        let dir = std::env::temp_dir().join(format!("ens-save-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.ksd");
        std::fs::write(&path, sample(false, true, 3, player_character(2, 5, &[], None))).unwrap();
        let s = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!((s.level, s.xp, s.nodes.len()), (2, 5, 0));
        assert!(matches!(load(&dir.join("missing.ksd")), Err(Error::Io { .. })));
    }

    #[test]
    fn character_saves_lists_only_character_files_newest_first() {
        let dir = std::env::temp_dir().join(format!("ens-saves-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("backup")).unwrap();
        for n in ["character1.ksd", "character3.ksd", "world2.ksd", "character.ksd", "characterX.ksd", "backup/character9.ksd"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(dir.join("character1.ksd")).unwrap().set_modified(old).unwrap();
        let found = character_saves(&[dir.clone()]);
        std::fs::remove_dir_all(&dir).ok();
        let names: Vec<_> = found.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["character3.ksd", "character1.ksd"]);
    }

    #[test]
    fn reads_equipment_inventory_and_ammo_refs() {
        let head = item_record(0, 111, 1, Some(13), Some((3, 3)));
        let ring = item_record(5, 222, 1, Some(20), Some((5, 5)));
        let bow = item_record(0, 333, 1, Some(15), Some((5, 0)));
        let arrows = item_record(1, 444, 464, None, None);
        let items = item_section(&[(5, head), (10, ring)], Some(1), None, &[bow, arrows]);
        let s = Save::parse(&character_file_with_items("Roth", 13, 0, &[(9, 1)], &items)).unwrap();
        assert_eq!(s.items_error, None);
        assert_eq!(s.nodes, [(9, 1)]);
        let eq: Vec<_> = s.items.equipped.iter().map(|(bit, it)| (*bit, it.id, it.level, it.rarity())).collect();
        assert_eq!(eq, [(5, 111, Some(13), Some(2)), (10, 222, Some(20), Some(4))]);
        assert_eq!(s.items.inventory.iter().map(|i| (i.id, i.count)).collect::<Vec<_>>(), [(333, 1), (444, 464)]);
        assert_eq!((s.items.arrow, s.items.spell), (Some(1), None));

        // an item part that does not land on the tail leaves items empty but keeps the character
        let mut bad = items.clone();
        bad.push(0);
        let s = Save::parse(&character_file_with_items("Roth", 13, 0, &[], &bad)).unwrap();
        assert_eq!((s.level, s.items.inventory.len()), (13, 0));
        assert!(s.items_error.unwrap().contains("not at the tail"));
    }

    #[test]
    fn found_skill_points_sum_the_unlocks_present() {
        // 604285380 grants 3 and 365535033 grants 1 (game data); 999 is not a skill-point unlock; a zero value
        // does not count
        let file = character_file_with_knowledge("Roth", 13, &[(604285380, 1), (365535033, 1), (999, 1), (15481918, 0)]);
        let s = Save::parse(&file).unwrap();
        assert_eq!(s.knowledge.as_ref().map(|k| k.len()), Some(4));
        assert_eq!(s.found_skill_points(), Some(4));
        // no knowledge block: unknown, not zero
        let s = Save::parse(&character_file("Roth", 13, 0, &[])).unwrap();
        assert_eq!((s.knowledge.is_none(), s.found_skill_points()), (true, None));
        // a block whose length does not fit the layout is not read
        assert_eq!(read_knowledge(&[1, 0, 0, 0, 5, 0, 0, 0, 0]), None);
        let table = crate::skill_point_unlocks::SKILL_POINT_UNLOCKS;
        assert_eq!((table.len(), table.iter().map(|x| x.1).sum::<u32>()), (78, 114));
    }

    #[test]
    fn item_id_is_fnv1a_of_the_guid_bytes() {
        // the ids Banebringer and Tiny Backpack records carry in the live save (2026-10-21)
        let bundle = Bundle::load(default_bundle_path()).expect("bundle");
        let by_name = |n: &str| bundle.items.iter().find(|(_, i)| i.name == n).map(|(g, _)| item_id(g).unwrap());
        assert_eq!(by_name("Banebringer"), Some(2296172399));
        assert_eq!(by_name("Tiny Backpack"), Some(531058391));
        assert_eq!(item_id("zz"), None);
    }

    #[test]
    fn errors_are_distinct() {
        let good = sample(false, true, 0, player_character(1, 1, &[(1, 1)], None));

        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(matches!(Save::parse(&bad), Err(Error::BadMagic { .. })));

        let mut bad = good.clone();
        let index_at = u32_at(&bad, 40) + 40;
        bad[40..44].copy_from_slice(&(index_at + 100_000).to_le_bytes());
        assert!(matches!(Save::parse(&bad), Err(Error::IndexOutOfFile { what: "footer index", .. })));

        // a record whose block runs past the file
        let mut bad = good.clone();
        let rec = index_at as usize;
        bad[rec + 12..rec + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Save::parse(&bad), Err(Error::IndexOutOfFile { what: "block", .. })));

        // block type 9 where character_data wants 10
        let file = container(
            &[
                ("character_data", block_bytes(TYPE_PLAYER_CHARACTER, false, &character_data("Roth", [0; 16]))),
                ("player_character", block_bytes(TYPE_PLAYER_CHARACTER, false, &player_character(1, 1, &[], None))),
            ],
            0,
            [0; 16],
        );
        assert!(matches!(
            Save::parse(&file),
            Err(Error::WrongBlockType { name: "character_data", expected: 10, found: 9 })
        ));

        // node count of 1000 in a block that holds one node
        let file = sample(false, false, 0, player_character(1, 1, &[(1, 1)], Some(1000)));
        assert!(matches!(Save::parse(&file), Err(Error::NodeCountOverrun { count: 1000, .. })));

        // missing block
        let file = container(&[("character_data", block_bytes(10, false, &character_data("Roth", [0; 16])))], 0, [0; 16]);
        assert!(matches!(Save::parse(&file), Err(Error::BlockMissing { name: "player_character" })));

        // corrupt zstd payload
        let mut file = sample(false, true, 0, player_character(1, 1, &[(1, 1)], None));
        let idx = read_index(&file).unwrap();
        let (_, off, _) = idx.iter().find(|(n, _, _)| n == "player_character").unwrap().clone();
        file[off + BLOCK_HEADER_LEN..off + BLOCK_HEADER_LEN + 4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(Save::parse(&file), Err(Error::Zstd { .. })));

        // all messages differ
        assert!(Save::parse(&good[..10]).unwrap_err().to_string().contains("truncated"));
    }

    #[test]
    fn bundle_has_every_recorded_node_id() {
        let bundle = Bundle::load(default_bundle_path()).expect("bundle");
        for (id, _) in NODES {
            assert!(bundle.skills.iter().any(|s| s.id == id), "node id {id} is not a SkillNode.id in the bundle");
        }
    }

    #[test]
    fn ranks_maps_levels_zero_rank_and_unknown() {
        let bundle = Bundle::load(default_bundle_path()).expect("bundle");
        let mut nodes = NODES.to_vec();
        nodes.push((1, 2)); // not a node id in the bundle
        let save = Save { name: "t".into(), guid: [0; 16], level: 13, xp: 1, nodes, tail: vec![0; 22], items: Items::default(), items_error: None, knowledge: None };
        let r = save.ranks(&bundle);
        assert_eq!(r.ranks.len(), bundle.skills.len());
        assert_eq!(r.unknown, vec![1]);
        assert_eq!(r.unlocked_zero_rank.len(), NODES.iter().filter(|n| n.1 == 0).count());
        for (id, lvl) in NODES {
            let s = bundle.skills.iter().find(|s| s.id == id).unwrap();
            assert_eq!(r.ranks[s.index], lvl);
            assert_eq!(r.unlocked_zero_rank.contains(&s.index), lvl == 0);
        }
        assert_eq!(r.ranks.iter().map(|&x| x as u32).sum::<u32>(), NODES.iter().map(|n| n.1 as u32).sum::<u32>());
    }

    /// The folder of real saves the ignored tests below read: ENS_SAVE_DIR, set to Steam's
    /// userdata/<account>/1203620/remote folder.
    fn real_saves() -> Option<std::path::PathBuf> {
        let d = std::env::var_os("ENS_SAVE_DIR").map(std::path::PathBuf::from);
        if d.is_none() {
            eprintln!("SKIPPED: set ENS_SAVE_DIR to Steam's userdata/<account>/1203620/remote folder");
        }
        d
    }

    #[test]
    #[ignore = "reads a real save from ENS_SAVE_DIR"]
    fn reads_real_character4() {
        let Some(dir) = real_saves() else { return };
        let path = &dir.join("character4.ksd");
        if !path.exists() {
            eprintln!("SKIPPED: {} is not on this machine", path.display());
            return;
        }
        let s = load(path).unwrap();
        assert_eq!(s.name, "Roth");
        assert!(s.level >= 13, "level {}", s.level);
        assert!(s.xp > 0);
        for pair in NODES {
            assert!(s.nodes.contains(&pair), "recorded node {pair:?} missing from {:?}", s.nodes);
        }
        assert_eq!(s.items_error, None);
        assert!(s.found_skill_points().unwrap() >= 32, "Roth had found 32 points on 2026-10-21");
        assert!(s.items.equipped.len() >= 7, "armour and rings: {:?}", s.items.equipped);
        println!("character4: level {} xp {} nodes {}", s.level, s.xp, s.nodes.len());
        println!("equipped {:?}", s.items.equipped.iter().map(|(b, i)| (*b, i.id, i.level, i.perks)).collect::<Vec<_>>());
        println!("inventory {} records, arrow at {:?}, spell at {:?}", s.items.inventory.len(), s.items.arrow, s.items.spell);
        println!("tail = {:02x?}", s.tail);
        println!("tail u16 #1 = {}, u16 #2 = {}", u16_at(&s.tail, 0), u16_at(&s.tail, 2));
    }

    #[test]
    fn world_metadata_gives_flame_name_and_time_and_picks_the_world_by_time() {
        let dir = std::env::temp_dir().join(format!("ens-world-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("world1.ksd"), fixture::world_file("Old", 1, 1_700_000_000)).unwrap();
        std::fs::write(dir.join("world2.ksd"), fixture::world_file("Mine", 4, 1_792_644_040)).unwrap();
        std::fs::write(dir.join("world3.ksd"), b"not a save").unwrap();
        let w = world_saves(std::slice::from_ref(&dir));
        assert_eq!(w.iter().map(|w| (w.name.as_str(), w.flame_level)).collect::<Vec<_>>(), [("Mine", 4), ("Old", 1)]);
        assert_eq!(world_at(&w, 1_792_640_000).unwrap().name, "Mine");
        assert_eq!(world_at(&w, 1_700_000_100).unwrap().name, "Old");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Raw bytes of every block and every index hash, by block name.
    fn blocks_and_hashes(f: &[u8]) -> Vec<(String, Vec<u8>, u32)> {
        let at = u32_at(f, 40) as usize + 40;
        read_index(f).unwrap().into_iter().enumerate()
            .map(|(i, (n, off, size))| (n, f[off..off + size].to_vec(), u32_at(f, at + i * INDEX_RECORD_LEN + 16)))
            .collect()
    }

    /// The writer changes the level and nothing else: the other blocks, the hashes and every other field read back
    /// the same, and the file is still a valid container (size and index offset updated).
    fn check_set_level(f: &[u8]) {
        let before = Save::parse(f).unwrap();
        let out = write::set_level(f, 19).unwrap();
        let after = Save::parse(&out).unwrap();
        assert_eq!((after.level, after.xp), (19, 0));
        assert_eq!((&after.name, after.guid, &after.nodes, &after.items, &after.knowledge, &after.tail),
                   (&before.name, before.guid, &before.nodes, &before.items, &before.knowledge, &before.tail));
        assert_eq!(u32_at(&out, 8) as usize, out.len());
        let (a, z) = (blocks_and_hashes(f), blocks_and_hashes(&out));
        assert_eq!(a.len(), z.len());
        for ((n, raw, h), (n2, raw2, h2)) in a.iter().zip(&z) {
            assert_eq!(n, n2);
            assert_eq!(*h2, write::block_hash(raw2), "{n}: the index hash is XXH3-64 (low 32) of the raw block");
            if n != "player_character" {
                assert_eq!((raw, h), (raw2, h2), "{n} must be copied byte for byte");
            }
        }
        assert_eq!(u32_at(&out, 28), u32_at(f, 28).wrapping_add(1), "save counter raised");
        // a no-op rewrite reads back identical
        let same = write::rewrite_block(f, "player_character", TYPE_PLAYER_CHARACTER, |_| {}).unwrap();
        assert_eq!(Save::parse(&same).unwrap().level, before.level);
    }

    /// Nodes replaced and items added read back as written; the rest of the character is untouched.
    #[test]
    fn writer_sets_nodes_and_adds_items() {
        let f = fixture::character_file_with_items("Test", 3, 0, &[(7, 1)], &fixture::item_section(&[], None, None, &[]));
        let wand = fixture::item_record(9, 0x1234_5678, 1, Some(12), Some((1, 0)));
        let spell = fixture::item_record(9, 0x0bad_cafe, 100, None, None);
        let edit = write::CharacterEdit { level: Some(19), nodes: Some(vec![(11, 1), (22, 3)]), backpack: vec![wand, spell] };
        let s = Save::parse(&write::edit_character(&f, &edit).unwrap()).unwrap();
        assert_eq!((s.level, s.nodes.clone(), s.items_error.clone()), (19, vec![(11, 1), (22, 3)], None));
        let inv: Vec<(u8, u32, u16, Option<u8>)> = s.items.inventory.iter().map(|i| (i.slot, i.id, i.count, i.level)).collect();
        assert_eq!(inv, [(0, 0x1234_5678, 1, Some(12)), (1, 0x0bad_cafe, 100, None)]);
        // adding again continues the slot numbers
        let s2 = Save::parse(&write::edit_character(&write::edit_character(&f, &edit).unwrap(),
            &write::CharacterEdit { backpack: vec![fixture::item_record(0, 7, 1, None, None)], ..Default::default() }).unwrap()).unwrap();
        assert_eq!(s2.items.inventory.last().map(|i| (i.slot, i.id)), Some((2, 7)));
    }

    #[test]
    fn writer_changes_only_the_level() {
        check_set_level(&fixture::character_file_with_knowledge("Test", 1, &[(7, 1)]));
    }

    #[test]
    #[ignore = "reads real saves from ENS_SAVE_DIR"]
    fn writer_on_real_saves() {
        let Some(dir) = real_saves() else { return };
        for i in 1..=5 {
            let p = dir.join(format!("character{i}.ksd"));
            if let Ok(f) = std::fs::read(&p) {
                // the game's own hashes follow the rule the writer uses
                for (n, raw, h) in blocks_and_hashes(&f) {
                    assert_eq!(h, write::block_hash(&raw), "{}: {n}", p.display());
                }
                check_set_level(&f);
                println!("{}: ok", p.display());
            }
        }
    }

    #[test]
    #[ignore = "reads a real save from ENS_SAVE_DIR"]
    fn reads_real_world2() {
        let Some(dir) = real_saves() else { return };
        let path = &dir.join("world2.ksd");
        if !path.exists() {
            eprintln!("SKIPPED: {} is not on this machine", path.display());
            return;
        }
        let w = read_world(path).unwrap();
        assert_eq!((w.name.as_str(), w.flame_level), ("My World 3", 2), "Roth plays this world at Flame 2 (2026-10-21)");
        assert!(w.saved_unix > 1_790_000_000);
    }
}

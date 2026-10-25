//! Saved builds: one JSON file per build in the app's data folder, written after every change. See
//! `docs/plans/saved-builds-owned-gear.md`.

use crate::model::Fight;
use ens_calc::{Build, Equip};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 2: fights keep their enemy's template name (enemy_key) and Health × replaces the fixed health factor.
const VERSION: u32 = 2;

/// An item the character has, as Load from game found it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnedItem {
    pub item: Equip,
    pub count: u16,
    /// Worn when the save was read (armour, rings, shield).
    pub equipped: bool,
}

/// Where the owned items came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnedSource {
    pub character: String,
    pub path: String,
    /// Unix seconds when Load from game read them.
    pub loaded: u64,
}

/// Everything a saved build holds. Undo history and optimizer results are not saved.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedBuild {
    pub version: u32,
    pub id: String,
    pub name: String,
    /// Unix seconds of the last change.
    pub edited: u64,
    pub loadouts: Vec<Build>,
    pub current: usize,
    #[serde(default)]
    pub notes: String,
    pub fight: Fight,
    /// Optimizer goals by label, most important first.
    #[serde(default)]
    pub ranking: Vec<String>,
    #[serde(default)]
    pub owned: Vec<OwnedItem>,
    #[serde(default)]
    pub owned_from: Option<OwnedSource>,
    /// Optimizer settings (weapons, must-have / avoided skills, gear source, milestones).
    #[serde(default)]
    pub opt: Option<crate::optim::OptSettings>,
    /// The game data it was saved against (ens_calc::build::data_tag), and the tree's skill ids in node order then,
    /// so ranks stored by position can be moved onto a changed tree. Absent in builds saved before them.
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub nodes: Vec<u32>,
}

/// One row of the builds list.
#[derive(Clone, Debug, Serialize)]
pub struct BuildInfo {
    pub id: String,
    pub name: String,
    pub character: Option<String>,
    pub level: u32,
    pub loadouts: usize,
    pub edited: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A fresh build id: letters and digits only, so it is always a safe file name.
pub fn new_id() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("b{ms}")
}

fn path_of(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("{id:?} is not a build id"));
    }
    Ok(dir.join(format!("{id}.json")))
}

/// Write a build, through a temporary file so a crash mid-write never leaves half a file.
pub fn save(dir: &Path, b: &SavedBuild) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = path_of(dir, &b.id)?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&SavedBuild { version: VERSION, ..b.clone() }).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("replace {}: {e}", path.display()))
}

pub fn load(dir: &Path, id: &str) -> Result<SavedBuild, String> {
    let path = path_of(dir, id)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let b: SavedBuild = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if b.loadouts.is_empty() {
        return Err(format!("{} has no loadouts", path.display()));
    }
    Ok(b)
}

pub fn delete(dir: &Path, id: &str) -> Result<(), String> {
    let path = path_of(dir, id)?;
    std::fs::remove_file(&path).map_err(|e| format!("delete {}: {e}", path.display()))
}

/// Saved builds, most recently edited first. Files that do not read as a build are left out.
pub fn list(dir: &Path) -> Vec<BuildInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<BuildInfo> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().strip_suffix(".json")?.to_string();
            let b = load(dir, &id).ok()?;
            let level = b.loadouts.get(b.current).or(b.loadouts.first()).map(|l| l.level).unwrap_or(1);
            Some(BuildInfo { id, name: b.name, character: b.owned_from.map(|o| o.character), level, loadouts: b.loadouts.len(), edited: b.edited })
        })
        .collect();
    out.sort_by(|a, b| b.edited.cmp(&a.edited).then(a.id.cmp(&b.id)));
    out
}

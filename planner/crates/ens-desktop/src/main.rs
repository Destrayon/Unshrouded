//! Enshrouded Planner — desktop app on Tauri: the Rust engine (ens-calc, ens-opt) behind commands, the interface in
//! ui/ drawn by WebView2. All planner state (builds, loadouts, undo, fight settings) lives here; the page sends
//! actions and draws the JSON it gets back.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod gamewatch;
mod live;
mod loader;
mod overlay_img;
mod game;
mod labels;
mod model;
mod optim;
mod store;
mod views;

use ens_calc::{Build, Engine};
use ens_data::Bundle;
use ens_opt::{Goal, Optimizer, Plan, WeaponPlan, GOALS};
use model::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tauri::State;

static BUNDLE_JSON: &str = include_str!("../../../data/bundle.json");
/// Active-skill icons by node id ({on, off} PNG data URLs); source in the file.
static SKILL_ICONS_JSON: &str = include_str!("../../../data/skill_icons.json");
/// Friendly names and icons for attacks (program ids) and weapon categories, from the game data (kfc).
static ATTACK_NAMES_JSON: &str = include_str!("../../../data/attack_names.json");
/// Arrow and spell items by ItemId (analysis/ammo.md, planner/data/ammo.json).
static AMMO_JSON: &str = include_str!("../../../data/ammo.json");

#[derive(Clone, serde::Serialize)]
pub struct Pick {
    pub guid: String,
    pub label: String,
    pub rarity: String,
}

pub struct Lists {
    pub weapons: Vec<Pick>,
    pub ammo: Vec<Pick>,
    pub gear: Vec<Vec<Pick>>,
    pub foods: Vec<Pick>,
    pub gems: Vec<Pick>,
    pub summons: Vec<Pick>,
}

pub struct OptResult {
    pub ranks: Vec<u8>,
    pub order: Vec<usize>,
    pub goals: HashMap<&'static str, f64>,
    pub gear: Option<(Option<ens_calc::Equip>, Vec<Option<ens_calc::Equip>>)>,
    pub summary: Summary,
    /// The plan the result was made for (weapons and ammo resolved); Apply uses it for ammo and carried weapons.
    pub plan: Option<Plan>,
}

pub struct Planner {
    pub b: &'static Bundle,
    pub engine: Engine<'static>,
    pub opt: &'static Optimizer<'static>,
    pub enemies: Vec<EnemyPick>,
    pub lists: Lists,
    pub build: Build,
    pub build_name: String,
    pub loadouts: Vec<Build>,
    pub current: usize,
    pub undo: Vec<Build>,
    pub redo: Vec<Build>,
    last_edit: Option<(String, Instant)>,
    pub fight: Fight,
    pub notes: String,
    pub ranking: Vec<Goal>,
    pub opt_result: Option<OptResult>,
    summary_cache: Option<(String, Summary)>,
    /// The saved build this state is written to, and the folder of saved builds (None: nothing is saved).
    pub build_id: String,
    pub store: Option<std::path::PathBuf>,
    /// Items the character has, from the last Load from game.
    pub owned: Vec<store::OwnedItem>,
    pub owned_from: Option<store::OwnedSource>,
    /// Optimizer settings and the last milestone plan (not saved).
    pub opt_settings: optim::OptSettings,
    pub ms_result: Option<Vec<ens_opt::MilestoneResult>>,
    /// A message for the page to show once (a build or code made with other game data).
    pub notice: Option<String>,
}

/// A gem's internal name in words: T2_Gem_GemOfToxicReaction -> "Gem of Toxic Reaction" (variant suffixes such as
/// _OverFog dropped).
fn gem_internal_name(debug: &str) -> Option<String> {
    let core = debug.split("_Gem_").nth(1)?.split('_').next()?;
    let mut out = String::new();
    for (k, c) in core.chars().enumerate() {
        if k > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    Some(out.replace(" Of ", " of "))
}

fn lists(b: &Bundle) -> Lists {
    let label = |i: &ens_data::Item| {
        // an upgradable (Eternal) arrow or spell goes up one level per unlocked perk, so its label shows the range
        let up = if ens_calc::upgradable(i) { ens_calc::unlocked_perks(i, i.rarity.as_deref().unwrap_or("Legendary"), None) } else { 0 };
        let lv = i.levels.as_ref().map(|l| if l.min == l.max && l.min > 0 {
            if up > 0 { format!(" L{}\u{2013}{}", l.min, l.min + up) } else { format!(" L{}", l.min) }
        } else { String::new() }).unwrap_or_default();
        format!("{}{}", i.name, lv)
    };
    let pick = |f: &dyn Fn(&ens_data::Item) -> bool| {
        let mut v: Vec<Pick> = b.items.iter().filter(|(_, i)| !i.name.is_empty() && f(i))
            .map(|(g, i)| Pick { guid: g.clone(), label: label(i), rarity: i.rarity.clone().unwrap_or_default() }).collect();
        v.sort_by(|a, b| a.label.cmp(&b.label).then(a.rarity.cmp(&b.rarity)));
        v.dedup_by(|a, b| a.label == b.label && a.rarity == b.rarity);
        v
    };
    Lists {
        weapons: pick(&|i| i.category.as_deref() == Some("Weapons") && i.damage.is_some()),
        // a spell without damage data (Trap Fireball, Fireworks: trap and festival items, not player spells) is left out
        ammo: pick(&|i| (i.slot.as_deref() == Some("Arrow") || i.slot.as_deref() == Some("Spell") && i.damage.is_some()) || i.ammo.as_deref() == Some("Arrow") && i.category.as_deref() == Some("Ammunition")),
        gear: ens_calc::build::GEAR_SLOTS.iter().map(|slot| pick(&|i| i.slot.as_deref() == Some(*slot))).collect(),
        foods: pick(&|i| i.buff.as_ref().and_then(|x| x.slot.as_deref()) == Some("Food")),
        gems: pick(&|i| i.category.as_deref() == Some("WeaponGem")).into_iter().map(|mut g| {
            // a gem renamed for 1.0 keeps its internal name (T2_Gem_GemOfToxicReaction), which patch notes still use:
            // the label carries it too, so a search for either name finds the gem
            if let Some(alias) = b.gems.get(&g.guid).and_then(|gm| gem_internal_name(&gm.debug)) {
                if alias.to_lowercase().replace(' ', "") != g.label.to_lowercase().replace(' ', "") {
                    g.label = format!("{} (also {alias})", g.label);
                }
            }
            g
        }).collect(),
        summons: pick(&|i| ens_calc::is_summon_spell(b, i)),
    }
}

impl Planner {
    fn new() -> Self {
        let b: &'static Bundle = Box::leak(Box::new(Bundle::from_json(BUNDLE_JSON).expect("bundled game data")));
        let opt: &'static Optimizer<'static> = Box::leak(Box::new(Optimizer::new(b)));
        let (loadouts, current, build_name) = Self::starting_loadouts(b);
        let mut p = Planner {
            b,
            engine: Engine::new(b),
            opt,
            enemies: enemies(b),
            lists: lists(b),
            build: loadouts[current].clone(),
            loadouts,
            current,
            build_name,
            undo: vec![],
            redo: vec![],
            last_edit: None,
            fight: Fight::default(),
            notes: String::new(),
            ranking: GOALS.to_vec(),
            opt_result: None,
            summary_cache: None,
            build_id: store::new_id(),
            store: None,
            owned: vec![],
            owned_from: None,
            opt_settings: optim::OptSettings::default(),
            ms_result: None,
            notice: None,
        };
        p.sync_enemy_level();
        p
    }

    /// A starting point for a new player (R1-DR-12): the preset weapon of `kind` and its ammo at the character's level,
    /// a skill tree from one quick optimizer pass, the armour that is best with that tree among items of that level
    /// (rolled rarities at Rare), and the three foods that add the most DPS. About 3 s.
    fn fill_preset(&mut self, kind: &str) -> Result<(), String> {
        let mut base = Build::new(self.b, self.build.level);
        (base.flame_level, base.found_points, base.name) = (self.build.flame_level, self.build.found_points, self.build.name.clone());
        self.build = ens_calc::build::preset(self.b, &base, kind, base.level).ok_or("No weapon of that kind for this level.")?;
        let plan = optim::plan_from(self);
        let empty = vec![0u8; self.b.skills.len()];
        let pl = self.opt.prepare(&plan, &empty);
        let goals = self.opt.goals(&pl, &empty);
        let (best, order) = self.opt.optimize(&pl, 1, 7);
        let pl = self.opt.optimize_gear(&pl, &best.ranks, &goals, "Rare");
        self.build = optim::build_from(&self.build, &pl, &best.ranks, &order, self.build.name.clone());
        let c = self.conditions();
        for _ in 0..3 {
            let pick = self.lists.foods.iter().filter(|f| !self.build.foods.contains(&f.guid))
                .filter(|f| self.b.items[&f.guid].levels.as_ref().map(|l| l.min <= self.build.level).unwrap_or(true))
                .map(|f| {
                    let mut t = self.build.clone();
                    t.foods.push(f.guid.clone());
                    (summarize(&self.engine, &t, &c).dps, f.guid.clone())
                })
                .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(b.1.cmp(&a.1)));
            match pick {
                Some((_, g)) => self.build.foods.push(g),
                None => break,
            }
        }
        Ok(())
    }

    /// This loadout as plain text anyone can read without the app (chat, forum, phone): headline numbers, the fight they
    /// were made under, gear, skills, the game data and the build code.
    fn text_summary(&mut self) -> String {
        let b = self.b;
        let s = summarize(&self.engine, &self.build, &self.conditions());
        let name = |g: &str| b.items.get(g).map(|i| i.name.clone()).unwrap_or_else(|| g.to_string());
        let eq = |e: &ens_calc::Equip| format!("{}{}", name(&e.item), e.level.map(|l| format!(" L{l}")).unwrap_or_default());
        let mut t = vec![format!("{} \u{b7} level {} \u{b7} Flame {}", self.build.name, self.build.level, self.build.flame_level)];
        t.push(format!("Total DPS {:.1} ({:.0} s fight) \u{b7} expected hit {:.1} \u{b7} crit {:.1}% \u{b7} health {:.0}", s.dps, self.fight.fight_s, s.expected_hit, s.crit * 100.0, s.health));
        let f = &self.fight;
        let mut fight = vec![format!("{} L{}", self.enemies[f.enemy.min(self.enemies.len() - 1)].label, f.enemy_level)];
        fight.push(if f.pack_size > 1.0 { format!("{} enemies within {} m", f.pack_size, f.pack_radius) } else { "single target".into() });
        if f.hit_kind != "Normal" {
            fight.push(format!("{} {:.0}%", f.hit_kind.to_lowercase(), f.hit_share * 100.0));
        }
        fight.extend(f.changed());
        t.push(format!("Fight: {}", fight.join(", ")));
        // an upgradable (Eternal) arrow or spell with the level and perks it is priced at
        let ammo = |a: &str| match b.items.get(a).filter(|i| ens_calc::upgradable(i)) {
            Some(it) => {
                let e = self.build.upgrade_of(a);
                let rar = e.rarity.clone().or(it.rarity.clone()).unwrap_or_else(|| "Legendary".into());
                format!("{} L{}, {}/{} perks", it.name, self.engine.ammo_level(&self.build, a, it), ens_calc::unlocked_perks(it, &rar, e.perks), ens_calc::unlocked_perks(it, &rar, None))
            }
            None => name(a),
        };
        if let Some(w) = &self.build.weapon {
            t.push(format!("Weapon: {}{}{}", eq(w), self.build.ammo.as_deref().map(|a| format!(" + {}", ammo(a))).unwrap_or_default(),
                self.build.gem.as_ref().map(|g| format!(" + {}", name(&g.item))).unwrap_or_default()));
            let tot = self.engine.total(&self.build, &self.conditions().target);
            if let (Some((main, _)), Some((fb, _))) = (&tot.replaced, &tot.fallback) {
                t.push(format!("Counted as {fb}: it does more than {main} and costs no mana."));
            }
        }
        let gear: Vec<String> = self.build.gear.iter().flatten().chain(self.build.offhand.iter()).map(eq).collect();
        if !gear.is_empty() {
            t.push(format!("Gear: {}", gear.join(", ")));
        }
        if !self.build.foods.is_empty() {
            t.push(format!("Food: {}", self.build.foods.iter().map(|g| name(g)).collect::<Vec<_>>().join(", ")));
        }
        if !self.build.summons.is_empty() {
            t.push(format!("Summons: {}", self.build.summons.iter().map(|g| ammo(g)).collect::<Vec<_>>().join(", ")));
        }
        let skills: Vec<String> = b.skills.iter().enumerate().filter(|(i, _)| self.build.ranks[*i] > 0)
            .map(|(i, sk)| if sk.max > 1 { format!("{} {}", sk.name, self.build.ranks[i]) } else { sk.name.clone() }).collect();
        t.push(format!("Skills ({} points): {}", self.build.points(b), skills.join(", ")));
        t.push(format!("Data: {}", ens_calc::build::patch_label(b)));
        t.push(format!("Code: {}", self.share_code()));
        t.join("\n")
    }

    /// This loadout's build code, with the fight setup the numbers were made under.
    fn share_code(&self) -> String {
        let mut f = self.fight.clone();
        f.enemy_key = self.enemies.get(f.enemy).and_then(|e| e.name.clone());
        let mut extra = serde_json::Map::new();
        extra.insert("fight".into(), serde_json::to_value(&f).unwrap_or_default());
        self.build.to_code_with(self.b, extra)
    }

    /// Until the player sets the enemy level, the enemy is at the character's level (capped at the item level cap).
    fn sync_enemy_level(&mut self) {
        if !self.fight.enemy_level_set {
            self.fight.enemy_level = self.build.level.clamp(1, ens_calc::build::item_level_cap(self.b));
        }
    }

    /// A new build: one loadout, a fresh level-1 character at Flame level 1 (the sample build with ENS_SAMPLE).
    fn starting_loadouts(b: &Bundle) -> (Vec<Build>, usize, String) {
        let sample = std::env::var("ENS_SAMPLE").is_ok();
        let mut build = if sample { Build::sample_bow(b) } else { Build::new(b, 1) };
        build.name = "Main".into();
        (vec![build], 0, if sample { "Frost Ranger".into() } else { "My build".into() })
    }

    /// Remove loadout `k` (never the last one); the planner moves to the loadout before it.
    fn delete_loadout(&mut self, k: usize) {
        if self.loadouts.len() < 2 || k >= self.loadouts.len() {
            return;
        }
        self.loadouts[self.current] = self.build.clone();
        self.loadouts.remove(k);
        self.current = if self.current > k || self.current == self.loadouts.len() { self.current.saturating_sub(1) } else { self.current };
        self.build = self.loadouts[self.current].clone();
        (self.undo, self.redo, self.last_edit) = (vec![], vec![], None);
    }

    /// This state as a saved build.
    fn snapshot(&self) -> store::SavedBuild {
        let mut loadouts = self.loadouts.clone();
        loadouts[self.current] = self.build.clone();
        store::SavedBuild {
            version: 0,
            id: self.build_id.clone(),
            name: self.build_name.clone(),
            edited: store::now(),
            loadouts,
            current: self.current,
            notes: self.notes.clone(),
            fight: self.fight.clone(),
            ranking: self.ranking.iter().map(|g| g.label().to_string()).collect(),
            owned: self.owned.clone(),
            owned_from: self.owned_from.clone(),
            opt: Some(self.opt_settings.clone()),
            data: Some(ens_calc::build::data_tag(self.b)),
            nodes: self.b.skills.iter().map(|s| s.id).collect(),
        }
    }

    /// Replace this state with a saved build. A build saved against another tree has its ranks moved by skill id;
    /// one saved before builds recorded their tree, with another tree size, is refused. A build saved with other
    /// game numbers opens with a notice.
    fn restore(&mut self, mut s: store::SavedBuild) -> Result<(), String> {
        let n = self.b.skills.len();
        let here: Vec<u32> = self.b.skills.iter().map(|s| s.id).collect();
        if !s.nodes.is_empty() && s.nodes != here {
            for l in s.loadouts.iter_mut() {
                ens_calc::build::remap_ranks(l, &s.nodes, self.b);
            }
        }
        let tag = ens_calc::build::data_tag(self.b);
        self.notice = match &s.data {
            Some(d) if *d != tag => Some(format!("\"{}\" was saved with other game data ({d}; this planner has {tag}). Its skills were kept by skill, but its numbers may differ from when it was saved.", s.name)),
            Some(_) => None,
            None => Some(format!("\"{}\" was saved before builds recorded their game data (this planner has {tag}). Its skills are read by position, and its numbers may differ from when it was saved.", s.name)),
        };
        if s.loadouts.iter().any(|l| l.ranks.len() != n || l.gear.len() != ens_calc::build::GEAR_SLOTS.len()) {
            return Err(format!("Build \"{}\" was made for different game data and can't be opened.", s.name));
        }
        self.current = s.current.min(s.loadouts.len() - 1);
        self.build = s.loadouts[self.current].clone();
        self.loadouts = s.loadouts;
        self.build_id = s.id;
        self.build_name = s.name;
        self.notes = s.notes;
        self.fight = s.fight;
        // the enemy by its template name (the list's order can change between versions); a fight saved before names
        // were kept has its index into the old list, which is rebuilt to find the name
        let legacy = self.fight.enemy_key.is_none() && self.fight.enemy > 0;
        if legacy {
            self.fight.enemy_key = model::legacy_enemy_names(self.b).get(self.fight.enemy).cloned().flatten();
        }
        if let Some(k) = self.fight.enemy_key.clone() {
            self.fight.enemy = model::find_enemy(&self.enemies, &k).unwrap_or(0);
        }
        if legacy {
            let enemy = self.enemies[self.fight.enemy].label.clone();
            let note = format!("\"{}\" was saved by an older planner: its enemy is {enemy}, and its Health setting now multiplies that enemy's own health (1 = as the game has it).", self.build_name);
            self.notice = Some(match self.notice.take() { Some(n) => format!("{n} {note}"), None => note });
        }
        let ranking: Vec<Goal> = s.ranking.iter().filter_map(|l| GOALS.iter().copied().find(|g| g.label() == l)).collect();
        self.ranking = if ranking.len() == GOALS.len() { ranking } else { GOALS.to_vec() };
        self.owned = s.owned;
        self.owned_from = s.owned_from;
        self.opt_settings = s.opt.unwrap_or_default();
        self.ms_result = None;
        (self.undo, self.redo, self.last_edit, self.opt_result, self.summary_cache) = (vec![], vec![], None, None, None);
        Ok(())
    }

    /// Write this build to its file, when there is a store.
    fn autosave(&self) -> Result<(), String> {
        match &self.store {
            Some(dir) => store::save(dir, &self.snapshot()),
            None => Ok(()),
        }
    }

    /// Start a fresh build (saved at once when there is a store).
    fn new_build(&mut self) -> Result<(), String> {
        let (loadouts, current, _) = Self::starting_loadouts(self.b);
        let s = store::SavedBuild {
            version: 0, id: store::new_id(), name: "New build".into(), edited: store::now(), loadouts, current,
            notes: String::new(), fight: Fight::default(), ranking: vec![], owned: vec![], owned_from: None, opt: None,
            data: Some(ens_calc::build::data_tag(self.b)), nodes: self.b.skills.iter().map(|s| s.id).collect(),
        };
        self.restore(s)?;
        self.autosave()
    }

    fn open_build(&mut self, id: &str) -> Result<(), String> {
        let dir = self.store.clone().ok_or("builds are not being saved")?;
        let s = store::load(&dir, id)?;
        self.restore(s)
    }

    /// A copy of a saved build under a new id and "(copy)" name; the copy is opened.
    fn duplicate_build(&mut self, id: &str) -> Result<(), String> {
        let dir = self.store.clone().ok_or("builds are not being saved")?;
        let mut s = store::load(&dir, id)?;
        // ids are milliseconds; a copy made in the same millisecond as its source gets the next one
        s.id = store::new_id();
        while s.id == id || store::list(&dir).iter().any(|b| b.id == s.id) {
            s.id = format!("{}1", s.id);
        }
        s.name = format!("{} (copy)", s.name);
        s.edited = store::now();
        store::save(&dir, &s)?;
        self.restore(s)
    }

    /// Delete a saved build. Deleting the open build opens the next newest one, or a new build when none is left.
    fn delete_build(&mut self, id: &str) -> Result<(), String> {
        let dir = self.store.clone().ok_or("builds are not being saved")?;
        store::delete(&dir, id)?;
        if id == self.build_id {
            match store::list(&dir).first() {
                Some(next) => self.open_build(&next.id.clone())?,
                None => self.new_build()?,
            }
        }
        Ok(())
    }

    /// Use `dir` for saved builds: open the newest build there, or save this one as the first.
    fn attach_store(&mut self, dir: std::path::PathBuf) -> Result<(), String> {
        self.store = Some(dir.clone());
        match store::list(&dir).first() {
            Some(newest) => self.open_build(&newest.id.clone()),
            None => self.autosave(),
        }
    }

    fn builds(&self) -> Value {
        let list = self.store.as_ref().map(|d| store::list(d)).unwrap_or_default();
        json!({ "current": self.build_id, "saving": self.store.is_some(), "builds": list })
    }

    pub fn conditions(&self) -> Conditions {
        self.fight.conditions(self.b, &self.enemies)
    }

    pub fn summary(&mut self) -> Summary {
        let k = key_of(&self.build, &self.fight);
        if self.summary_cache.as_ref().map(|(x, _)| x != &k).unwrap_or(true) {
            let c = self.conditions();
            self.summary_cache = Some((k, summarize(&self.engine, &self.build, &c)));
        }
        self.summary_cache.as_ref().unwrap().1.clone()
    }

    pub fn item_name(&self, g: &str) -> String {
        self.b.items.get(g).map(|i| i.name.clone()).unwrap_or_default()
    }

    pub fn weapon_kind(&self, guid: &str) -> Option<String> {
        self.b.items.get(guid).and_then(|i| i.weapon_category.as_ref()).and_then(|c| self.b.weapon_categories.get(c)).and_then(|c| c.kind.clone())
    }

    pub fn total_ranks(&self, name: &str) -> u32 {
        self.b.skills.iter().enumerate().filter(|(_, s)| s.name == name).map(|(i, _)| self.build.ranks[i] as u32).sum()
    }

    fn switch_loadout(&mut self, k: usize) {
        if k == self.current || k >= self.loadouts.len() {
            return;
        }
        self.loadouts[self.current] = self.build.clone();
        self.current = k;
        self.build = self.loadouts[k].clone();
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
    }

    /// Apply one action from the page. Build edits go on the undo stack; repeated edits of the same control within
    /// a second (dragging a number) count as one step.
    fn act(&mut self, a: &Value) -> Result<(), String> {
        let r = self.act_inner(a);
        self.sync_enemy_level();
        r
    }

    fn act_inner(&mut self, a: &Value) -> Result<(), String> {
        let kind = a.get("type").and_then(|v| v.as_str()).ok_or("action without a type")?;
        let s = |k: &str| a.get(k).and_then(|v| v.as_str()).map(|x| x.to_string());
        let n = |k: &str| a.get(k).and_then(|v| v.as_f64());
        let before = self.build.clone();
        match kind {
            "undo" => {
                if let Some(p) = self.undo.pop() {
                    self.redo.push(std::mem::replace(&mut self.build, p));
                }
                self.last_edit = None;
                return Ok(());
            }
            "redo" => {
                if let Some(p) = self.redo.pop() {
                    self.undo.push(std::mem::replace(&mut self.build, p));
                }
                self.last_edit = None;
                return Ok(());
            }
            // an empty or zero field is refused, not read as level 1
            "level" => {
                let v = n("value").filter(|v| v.is_finite() && *v >= 1.0).ok_or("Level must be a number from 1 to 50; it was not changed.")?;
                self.build.level = v.clamp(1.0, ens_calc::build::level_cap(self.b) as f64) as u32;
            }
            "flame" => self.build.flame_level = n("value").unwrap_or(1.0).clamp(1.0, ens_calc::build::FLAME_LEVEL_CAP as f64) as u32,
            "rested" => self.build.rested = a.get("value").and_then(|v| v.as_bool()).unwrap_or(true),
            "heavy" => self.build.heavy = a.get("value").and_then(|v| v.as_bool()).unwrap_or(false),
            // start from a preset weapon (and its arrow or spell) at the character's level
            "preset" => {
                let kind = s("kind").ok_or("no kind")?;
                self.fill_preset(&kind)?;
            }
            // skill points from the Shroud Roots and Elixir Wells found (an empty field: back to the even-spread estimate)
            "found_points" => {
                let all: u32 = ens_data::skill_point_unlocks::SKILL_POINT_UNLOCKS.iter().map(|x| x.1).sum();
                self.build.found_points = n("value").filter(|v| v.is_finite()).map(|v| v.clamp(0.0, all as f64) as u32);
            }
            "allocate" => {
                let i = n("node").ok_or("no node")? as usize;
                if i >= self.build.ranks.len() {
                    return Err("That skill node is not in this tree.".into());
                }
                if let Some(p) = path_to(self.opt, &self.build.ranks, i) {
                    self.build = with_path(&self.build, &p);
                }
            }
            "remove" => {
                let i = n("node").ok_or("no node")? as usize;
                if i >= self.build.ranks.len() {
                    return Err("That skill node is not in this tree.".into());
                }
                if can_remove(self.opt, &self.build, i) {
                    self.build.ranks[i] -= 1;
                    if let Some(k) = self.build.order.iter().rposition(|&x| x == i) {
                        self.build.order.remove(k);
                    }
                }
            }
            "reset_tree" => {
                self.build.ranks.iter_mut().for_each(|r| *r = 0);
                self.build.order.clear();
            }
            "set_ranks" => {
                if let Some(r) = a.get("ranks").and_then(|v| serde_json::from_value::<Vec<u8>>(v.clone()).ok()) {
                    if r.len() == self.build.ranks.len() {
                        // a step of the stored order keeps that much of it; any other tree gets its order worked out later
                        let total: usize = r.iter().map(|&x| x as usize).sum();
                        let prefix: Vec<usize> = self.build.order.iter().take(total).copied().collect();
                        self.build.ranks = r;
                        self.build.order = prefix;
                        if !self.build.order_matches() {
                            self.build.order.clear();
                        }
                    }
                }
            }
            "from_game" => {
                if let Some(nb) = a.get("build").and_then(|v| serde_json::from_value::<Build>(v.clone()).ok()) {
                    if nb.ranks.len() == self.build.ranks.len() && nb.gear.len() == self.build.gear.len() {
                        self.build = nb;
                    }
                }
                if let Some(l) = n("level") {
                    self.build.level = l.clamp(1.0, ens_calc::build::level_cap(self.b) as f64) as u32;
                }
                if let Some(r) = a.get("ranks").and_then(|v| serde_json::from_value::<Vec<u8>>(v.clone()).ok()) {
                    if r.len() == self.build.ranks.len() {
                        self.build.ranks = r;
                        if !self.build.order_matches() {
                            self.build.order.clear();
                        }
                    }
                }
            }
            "equip" => {
                let slot = Slot::parse(&s("slot").ok_or("no slot")?).ok_or("bad slot")?;
                if let Some(k) = n("owned") {
                    let o = self.owned.get(k as usize).ok_or("no such owned item")?.clone();
                    self.build = game::with_owned(self.b, &self.build, slot, &o);
                } else {
                    let g = s("guid").ok_or("no item")?;
                    self.build = apply_slot(self.b, &self.build, slot, &g);
                }
            }
            "best_owned" => {
                if self.owned.is_empty() {
                    return Err("Load from game first: nothing owned is known yet.".into());
                }
                let c = self.conditions();
                let summons: Vec<String> = self.lists.summons.iter().map(|x| x.guid.clone()).collect();
                self.build = game::best_owned(self.b, &self.build, &self.owned, &summons, |t| {
                    let s = summarize(&self.engine, t, &c);
                    (s.dps, s.hits_survived)
                });
            }
            "carry_weapon" => {
                let g = s("guid").ok_or("no item")?;
                let picked = apply_slot(self.b, &self.build, Slot::Weapon, &g).weapon.ok_or("no weapon")?;
                if self.build.weapon.is_none() {
                    self.build.weapon = Some(picked);
                } else {
                    self.build.carried.push(ens_calc::Carried { weapon: picked, gem: None, ammo: None });
                }
            }
            "use_weapon" => self.build.use_carried(n("index").ok_or("no index")? as usize),
            "drop_weapon" => {
                let k = n("index").ok_or("no index")? as usize;
                if k < self.build.carried.len() {
                    self.build.carried.remove(k);
                }
            }
            // the level (and perks) of an upgradable summon spell the build keeps up
            "upgrade" => {
                let g = s("guid").ok_or("no item")?;
                let b = self.b;
                let it = b.items.get(&g).filter(|i| ens_calc::upgradable(i)).ok_or("That item does not upgrade.")?;
                if self.build.ammo.as_deref() != Some(g.as_str()) && !self.build.summons.contains(&g) {
                    return Err("That item is not in this build.".into());
                }
                let mut e = self.build.upgrade_of(&g);
                let rar = e.rarity.clone().or(it.rarity.clone()).unwrap_or_else(|| "Legendary".into());
                if let Some(p) = n("perks") {
                    let max = ens_calc::unlocked_perks(it, &rar, None);
                    let p = (p.max(0.0) as u32).min(max);
                    e.perks = if p == max { None } else { Some(p) };
                }
                if let Some(l) = n("level") {
                    // each unlocked perk raises the level by one (0x140404650 keeps Level − unlockedPerks within the
                    // item's range), so a level past the item's own max unlocks that many perks and a lower one fewer
                    let r = it.levels.as_ref().map(|r| (r.min, r.max)).unwrap_or((1, ens_calc::build::item_level_cap(b)));
                    let maxp = ens_calc::unlocked_perks(it, &rar, None);
                    let top = (r.1 + maxp).min(ens_calc::build::item_level_cap(b));
                    let l = (l.max(0.0) as u32).clamp(r.0, top.max(r.0));
                    let p = l.saturating_sub(r.1).min(maxp);
                    e.perks = if p == maxp { None } else { Some(p) };
                    e.level = Some(l);
                }
                self.build.upgrades.insert(g, e);
            }
            "unequip" => {
                let slot = Slot::parse(&s("slot").ok_or("no slot")?).ok_or("bad slot")?;
                match slot {
                    Slot::Weapon => self.build.weapon = None,
                    Slot::Ammo => self.build.ammo = None,
                    Slot::Gem => self.build.gem = None,
                    Slot::Armor(k) => self.build.gear[k] = None,
                    Slot::Food => {
                        if let Some(k) = n("index") {
                            if (k as usize) < self.build.foods.len() {
                                self.build.foods.remove(k as usize);
                            }
                        }
                    }
                    Slot::Summon => {
                        if let Some(k) = n("index") {
                            if (k as usize) < self.build.summons.len() {
                                self.build.summons.remove(k as usize);
                            }
                        }
                    }
                }
            }
            "item" => {
                let slot = Slot::parse(&s("slot").ok_or("no slot")?).ok_or("bad slot")?;
                let b = self.b;
                if let Some(e) = equip_mut(b, &mut self.build, slot) {
                    if let Some(r) = s("rarity") {
                        e.rarity = Some(r);
                    }
                    if let Some(l) = n("level") {
                        e.level = Some(l.clamp(1.0, ens_calc::build::item_level_cap(b) as f64) as u32);
                    }
                    if let Some(p) = n("perks") {
                        if let Some(it) = b.items.get(&e.item) {
                            let max = ens_calc::unlocked_perks(it, e.rarity.as_deref().or(it.rarity.as_deref()).unwrap_or("Legendary"), None);
                            let p = (p.max(0.0) as u32).min(max);
                            e.perks = if p == max { None } else { Some(p) };
                        }
                    }
                }
            }
            "fight" => {
                // a value that does not fit is refused with a message, and the setting keeps the value in use
                let mut f = serde_json::from_value::<Fight>(a.get("fight").cloned().unwrap_or_default())
                    .map_err(|e| format!("That combat setting was not changed: {}.", e.to_string().split(" at line").next().unwrap_or("not a valid value")))?;
                if f.enemy_level == 0 || f.enemy_level > ens_calc::build::item_level_cap(self.b) {
                    return Err(format!("That combat setting was not changed: enemy level must be 1 to {}.", ens_calc::build::item_level_cap(self.b)));
                }
                f.check()?;
                // an enemy level the player typed stays; until then it follows the character
                f.enemy_level_set = self.fight.enemy_level_set || f.enemy_level != self.fight.enemy_level;
                f.enemy_key = self.enemies.get(f.enemy).and_then(|e| e.name.clone());
                self.fight = f;
                return Ok(());
            }
            "reset_play" => {
                let d = Fight::default();
                let f = &mut self.fight;
                (f.sprint_start, f.evades_per_min, f.air_share, f.glides_per_min, f.blocks_per_min, f.night, f.elixir, f.summon_idle, f.summon_attack_s, f.summon_life_s, f.fight_s, f.orb_pickup) =
                    (d.sprint_start, d.evades_per_min, d.air_share, d.glides_per_min, d.blocks_per_min, d.night, d.elixir, d.summon_idle, d.summon_attack_s, d.summon_life_s, d.fight_s, d.orb_pickup);
                return Ok(());
            }
            // names are cut at the length a code may carry (ens_calc::build::MAX_NAME_CHARS)
            "build_name" => {
                self.build_name = s("value").unwrap_or_default().chars().take(ens_calc::build::MAX_NAME_CHARS).collect();
                return Ok(());
            }
            "loadout_name" => self.build.name = s("value").unwrap_or_default().chars().take(ens_calc::build::MAX_NAME_CHARS).collect(),
            "delete_loadout" => {
                self.delete_loadout(n("index").ok_or("no index")? as usize);
                return Ok(());
            }
            "switch_loadout" => {
                self.switch_loadout(n("index").unwrap_or(0.0) as usize);
                return Ok(());
            }
            "new_loadout" => {
                self.loadouts[self.current] = self.build.clone();
                let mut nb = self.build.clone();
                nb.name = format!("Loadout {}", self.loadouts.len() + 1);
                self.loadouts.push(nb);
                let k = self.loadouts.len() - 1;
                self.switch_loadout(k);
                return Ok(());
            }
            "load_code" => {
                let code = s("code").unwrap_or_default();
                let (b, warning) = Build::from_code_for(&code, self.b)?;
                // a code shared from this planner carries its author's fight setup: without it the same code shows
                // other numbers to everyone whose Combat setup differs
                let fight = match Build::code_field(&code, "fight").map(serde_json::from_value::<model::Fight>) {
                    Some(Ok(mut f)) => {
                        f.enemy = f.enemy_key.as_deref().and_then(|k| model::find_enemy(&self.enemies, k)).unwrap_or(0);
                        f.check().ok().map(|_| f)
                    }
                    _ => None,
                };
                self.build = b;
                let note = fight.is_some().then(|| "The code's fight setup (enemy, armour, group, hit kind and pacing) is now in Combat setup, so the numbers are the ones its author saw; Ctrl+Z undoes it.".to_string());
                if let Some(f) = fight {
                    self.fight = f;
                }
                self.notice = match (warning, note) {
                    (Some(w), Some(n)) => Some(format!("{w} {n}")),
                    (w, n) => w.or(n),
                };
            }
            "notes" => {
                self.notes = s("value").unwrap_or_default();
                return Ok(());
            }
            "ranking" => {
                if let Some(v) = a.get("order").and_then(|v| v.as_array()) {
                    let r: Vec<Goal> = v.iter().filter_map(|x| x.as_str()).filter_map(|l| GOALS.iter().copied().find(|g| g.label() == l)).collect();
                    if r.len() == GOALS.len() {
                        self.ranking = r;
                    }
                }
                return Ok(());
            }
            "opt_settings" => {
                let v = a.get("settings").cloned().ok_or("no settings")?;
                self.opt_settings = serde_json::from_value(v).map_err(|e| format!("bad optimizer settings: {e}"))?;
                return Ok(());
            }
            "apply_milestone" => {
                let k = n("index").map(|x| x as usize);
                optim::apply_milestones(self, k)?;
                self.last_edit = None;
                return Ok(());
            }
            "apply_opt" => {
                if let Some(o) = &self.opt_result {
                    match &o.plan {
                        // a result made for another level or Flame level is not applied: it would change this
                        // loadout's character level and its tree was chosen for other points
                        Some(pl) if (pl.level, pl.flame_level) != (self.build.level, self.build.flame_level) => {
                            return Err(format!("That optimizer result was made for level {} at Flame level {}; this loadout is level {} at Flame level {}. Run the optimizer again on this loadout.",
                                pl.level, pl.flame_level, self.build.level, self.build.flame_level));
                        }
                        Some(pl) => {
                            let name = self.build.name.clone();
                            self.build = optim::build_from(&self.build, pl, &o.ranks, &o.order, name);
                            // a tree-only run keeps the gear on
                            if o.gear.is_none() {
                                self.build.gear = before.gear.clone();
                            }
                        }
                        None => {
                            self.build.ranks = o.ranks.clone();
                            self.build.order = o.order.clone();
                            if let Some((w, g)) = &o.gear {
                                self.build.weapon = w.clone();
                                self.build.gear = g.clone();
                            }
                        }
                    }
                }
            }
            "opt_step" => {
                if let Some(o) = &self.opt_result {
                    let k = (n("step").unwrap_or(0.0) as usize).min(o.order.len());
                    let mut r = vec![0u8; self.b.skills.len()];
                    for &i in o.order.iter().take(k) {
                        r[i] += 1;
                    }
                    self.build.ranks = r;
                    self.build.order = o.order[..k].to_vec();
                }
            }
            other => return Err(format!("unknown action {other}")),
        }
        if self.build != before {
            let key = format!("{kind}{}", s("slot").unwrap_or_default());
            let coalesce = self.last_edit.as_ref().map(|(k, t)| *k == key && t.elapsed().as_millis() < 1000 && matches!(kind, "level" | "flame" | "item" | "loadout_name")).unwrap_or(false);
            if !coalesce {
                self.undo.push(before);
                if self.undo.len() > 200 {
                    self.undo.remove(0);
                }
            }
            self.redo.clear();
            self.last_edit = Some((key, Instant::now()));
        }
        Ok(())
    }
}

impl Planner {
    /// Read the most recently written character save in `dirs` and put its level, skill ranks, gear and weapons into
    /// the current loadout as one undo step, and keep everything the character has as the owned items. Answers what
    /// was found, for the page to show.
    fn load_game(&mut self, dirs: &[std::path::PathBuf]) -> Value {
        let Some(file) = ens_data::save::character_saves(dirs).into_iter().next() else {
            return json!({ "found": false, "dirs": dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>() });
        };
        let save = match ens_data::save::load(&file.path) {
            Ok(s) => s,
            Err(e) => return json!({ "found": true, "path": file.path.display().to_string(), "error": e.to_string() }),
        };
        let r = save.ranks(self.b);
        let before = self.build.clone();
        let mut nb = self.build.clone();
        nb.level = save.level.clamp(1, ens_calc::build::level_cap(self.b));
        // known found points replace the even-spread guess; an unreadable knowledge block keeps what the build had
        if let Some(f) = save.found_skill_points() {
            nb.found_points = Some(f);
        }
        // the world saved together with this character gives the Flame level
        let saved = file.modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let worlds = ens_data::save::world_saves(dirs);
        let world = ens_data::save::world_at(&worlds, saved);
        if let Some(w) = world {
            nb.flame_level = w.flame_level.clamp(1, ens_calc::build::FLAME_LEVEL_CAP);
        }
        if r.ranks.len() == nb.ranks.len() && nb.ranks != r.ranks {
            nb.ranks = r.ranks.clone();
            nb.order.clear();
        }
        // Gear only when the item part parsed; otherwise the build keeps its gear and the page says why.
        let loaded = save.items_error.is_none().then(|| {
            let c = self.conditions();
            game::apply_items(self.b, &mut nb, &save.items, |t| summarize(&self.engine, t, &c).dps)
        });
        if save.items_error.is_none() {
            self.owned = game::owned_items(self.b, &save.items);
            self.owned_from = Some(store::OwnedSource { character: save.name.clone(), path: file.path.display().to_string(), loaded: store::now() });
        }
        let _ = self.act(&json!({ "type": "from_game", "build": nb }));
        let weapon = self.build.weapon.as_ref().map(|w| self.item_name(&w.item));
        json!({
            "found": true,
            "path": file.path.display().to_string(),
            "name": save.name,
            "level": save.level,
            "points": r.ranks.iter().map(|&x| x as u32).sum::<u32>(),
            "foundPoints": save.found_skill_points(),
            "world": world.map(|w| w.name.clone()),
            "flame": world.map(|w| w.flame_level),
            "unknown": r.unknown.len(),
            "age_s": file.modified.elapsed().map(|d| d.as_secs()).unwrap_or(0),
            "updated": self.build != before,
            "gear": loaded.as_ref().map(|l| l.gear),
            "weapons": loaded.as_ref().map(|l| l.weapons),
            "weapon": weapon,
            "unknownItems": loaded.as_ref().map(|l| l.unknown),
            "itemsError": save.items_error,
            "owned": self.owned.len(),
        })
    }
}

type St<'a> = State<'a, Mutex<Planner>>;

/// The live meter's view of the newest combat log (live.rs), with the planner's DPS for the open build beside it.
/// Polled by the meter window every 250 ms; reads only what the recorder wrote since the last poll.
#[tauri::command]
fn meter_state(live: State<Mutex<live::Live>>, st: St, status: State<Mutex<loader::Status>>) -> Value {
    let Some(dir) = live::log_dir() else {
        return json!({"error": "No Combat Logger folder found next to the planner (set ENS_LOG_DIR to the recorder's folder)."});
    };
    let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
    let view = match live_view(&mut l, &st, &dir) {
        Ok(v) => v,
        Err(e) => return json!({"error": e}),
    };
    let mut v = serde_json::to_value(view).unwrap_or_default();
    v["log"] = json!(l.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()));
    v["dir"] = json!(dir.display().to_string());
    v["planner_dps"] = json!(l.planner.as_ref().map(|(d, _)| *d));
    v["joined"] = json!(status.lock().map(|s| s.joined).unwrap_or(false));
    v
}

/// Read what the recorder appended and the meter's view of it now; shared by the meter window and the game overlay.
fn live_view(l: &mut live::Live, st: &St, dir: &std::path::Path) -> Result<ens_calc::meter::View, String> {
    l.poll(dir)?;
    // the planner's number for the build that is open, refreshed every 5 s (the meter must not hold the planner)
    if l.planner.as_ref().is_none_or(|(_, t)| t.elapsed().as_secs() >= 5) {
        if let Ok(p) = st.try_lock() {
            l.planner = Some((summarize(&p.engine, &p.build, &p.conditions()).dps, Instant::now()));
        }
    }
    let b = lock(st).b;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    Ok(l.meter.view(b, now))
}

/// The planner as the meter's loader (loader.rs, docs/plans/planner-loader.md). While Extensions → Live DPS meter is
/// on: follow the game; once it reaches its menu, load the combat logger into it (once per game process); while the
/// player is in a world, draw the meter into overlay.bin four times a second (rewritten on change and every 2 s so the
/// layer knows the planner runs), and remove the file otherwise so the layer draws nothing.
fn loader_thread(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let fonts = overlay_img::Fonts::load();
        let mut watch = gamewatch::Watch::default();
        let (mut tried, mut logger_error) = (None::<u32>, None::<String>);
        let mut registered_at = None::<Instant>;
        let (mut modules, mut modules_at) = (Vec::<String>::new(), None::<Instant>);
        let (mut last, mut written, mut seq, mut shown) = (Vec::new(), Instant::now(), 0u64, true);
        // the in-game picture: the overlay copy of the meter window, captured (capture.rs)
        let (cap_tx, cap_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tip_tx, tip_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (mut tip_pending, mut tip_img) = (None::<Instant>, None::<(u32, u32, Vec<u8>, Instant)>);
        let (mut overlay_win, mut pending, mut captured) = (None::<tauri::WebviewWindow>, None::<Instant>, None::<(ens_overlay::frame::Frame, Instant)>);
        let mut shot = std::env::var_os("ENS_OVERLAY_SHOT").map(std::path::PathBuf::from);
        let force = std::env::var("ENS_OVERLAY_FORCE").is_ok_and(|v| v == "1");
        let mut stale = false;
        let input_port = input_thread(app.clone());
        let (mut last_capture, mut inputs_seen) = (None::<Instant>, 0u64);
        loop {
            // 50 ms: a click or key in the game shows on the meter within about a tenth of a second
            std::thread::sleep(std::time::Duration::from_millis(50));
            let on = app.state::<Mutex<loader::Settings>>().lock().map(|s| s.meter).unwrap_or(false);
            let dir = live::log_dir();
            // keep the layer registered while the switch is on (a registration made by another program, or removed
            // since, may not be the one the game sees)
            if on && registered_at.is_none_or(|t| t.elapsed().as_secs() >= 10) {
                registered_at = Some(Instant::now());
                if let (None, Some(d)) = (ens_overlay::install::installed_dir(), dir.as_ref()) {
                    if let Err(e) = loader::install_layer(d) {
                        eprintln!("overlay: {e}");
                    }
                }
            }
            watch.poll();
            let pid = watch.pid;
            if tried.is_some() && tried != pid {
                (tried, logger_error) = (None, None);
            }
            match pid {
                None => modules.clear(),
                Some(p) if modules_at.is_none_or(|t| t.elapsed().as_secs() >= 2) => (modules, modules_at) = (ens_inject::modules(p), Some(Instant::now())),
                _ => {}
            }
            let has = |m: &str| modules.iter().any(|x| x.eq_ignore_ascii_case(m));
            let ready = watch.state.is_some() || watch.since.is_some_and(|t| t.elapsed().as_secs() >= 30);
            let seen = loader::Seen { running: pid.is_some(), ready, in_world: watch.in_world(), logger_loaded: has("ens_hook.dll"), logger_tried: tried.is_some() };
            let mut act = loader::decide(on, seen);
            // ENS_OVERLAY_FORCE=1: draw as if in a world (testing without the game)
            act.show_meter |= on && force;
            let (place, hidden) = app.state::<Mutex<loader::Settings>>().lock().map(|s| (s.place(), s.hidden)).unwrap_or_default();
            act.show_meter &= !hidden;
            if let (true, Some(p)) = (act.inject_logger, pid) {
                tried = pid;
                let r = match &dir {
                    Some(d) => ens_inject::inject(p, &d.join("ens_hook.dll")),
                    None => Err("no Combat Logger folder found".into()),
                };
                logger_error = r.err();
                modules_at = None;
            }
            let out = ens_overlay::install::installed_dir().or_else(|| dir.clone());
            let mut showing = false;
            if !on {
                if let Some(w) = overlay_win.take() {
                    let _ = w.close();
                }
                captured = None;
            }
            if let (true, Some(out), Some(dir)) = (act.show_meter, out.as_ref(), dir.as_ref()) {
                if overlay_win.is_none() {
                    let size = app.state::<Mutex<loader::Settings>>().lock().ok().and_then(|s| s.size);
                    overlay_win = capture::open(&app, size).map_err(|e| eprintln!("overlay window: {e}")).ok();
                }
                while let Ok(png) = cap_rx.try_recv() {
                    pending = None;
                    if let Some((w, h, px)) = capture::decode(&png) {
                        let mut f = ens_overlay::frame::Frame::new(w, h);
                        f.pixels = px;
                        if let Some(path) = shot.take() {
                            let opaque = f.pixels.chunks(4).filter(|p| p[3] == 255).count();
                            eprintln!("overlay capture {w}x{h}: {opaque} of {} pixels opaque", w * h);
                            let _ = ens_overlay::frame::write_atomic(&path, &f);
                        }
                        captured = Some((f, Instant::now()));
                    }
                }
                // a new picture four times a second, and at once after input from the game (tab, click, resize)
                let inputs = INPUTS.load(std::sync::atomic::Ordering::Relaxed);
                let due = last_capture.is_none_or(|t| t.elapsed().as_millis() >= 250) || inputs != inputs_seen;
                if let (Some(w), true) = (&overlay_win, due && pending.is_none_or(|t| t.elapsed().as_millis() >= 1000)) {
                    capture::request(w, cap_tx.clone());
                    (pending, last_capture, inputs_seen) = (Some(Instant::now()), Some(Instant::now()), inputs);
                    // the breakdown tooltip, while a player's bar is hovered in the game
                    let tip_on = TIP_PLAYER.lock().map(|p| p.is_some()).unwrap_or(false);
                    match (tip_on, app.get_webview_window(capture::TIP_GAME)) {
                        (true, Some(t)) if tip_pending.is_none_or(|p| p.elapsed().as_millis() >= 1000) => {
                            capture::request(&t, tip_tx.clone());
                            tip_pending = Some(Instant::now());
                        }
                        (false, _) => tip_img = None,
                        _ => {}
                    }
                }
                while let Ok(png) = tip_rx.try_recv() {
                    tip_pending = None;
                    if let Some((w, h, px)) = capture::decode(&png) {
                        tip_img = Some((w, h, px, Instant::now()));
                    }
                }
                // the captured meter, or the drawn one while there is none (the overlay copy is still loading)
                let fresh = captured.as_ref().filter(|(_, t)| t.elapsed().as_secs() < 3).map(|(f, _)| f.clone());
                // overlay.log next to the frame: when the picture went stale and when it came back
                if fresh.is_some() == stale {
                    stale = !stale;
                    let line = format!("{} picture {} (window {})
", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                        if stale { "stale: falling back to the drawn meter" } else { "live" },
                        overlay_win.as_ref().and_then(|w| w.outer_position().ok()).map(|p| format!("at {},{}", p.x, p.y)).unwrap_or_else(|| "missing".into()));
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(out.join("overlay.log")) {
                        let _ = f.write_all(line.as_bytes());
                    }
                }
                let frame = fresh.or_else(|| {
                    let fonts = fonts.as_ref()?;
                    let live: State<Mutex<live::Live>> = app.state();
                    let st: St = app.state();
                    let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
                    let view = live_view(&mut l, &st, dir).ok()?;
                    Some(overlay_img::render(fonts, &view, l.planner.as_ref().map(|(d, _)| *d)))
                });
                if let Some(mut frame) = frame {
                    frame.place = place;
                    frame.port = input_port;
                    if let Some((w, h, px, _)) = tip_img.as_ref().filter(|t| t.3.elapsed().as_secs() < 3 && TIP_PLAYER.lock().map(|p| p.is_some()).unwrap_or(false)) {
                        frame.extras.push(ens_overlay::frame::Extra { kind: ens_overlay::frame::AT_CURSOR, width: *w, height: *h, pixels: px.clone() });
                    }
                    frame.scale = overlay_win.as_ref().and_then(|w| w.scale_factor().ok()).map_or(0, |f| (f * 100.0).round() as u16);
                    showing = true;
                    if frame.pixels != last || written.elapsed().as_secs() >= 2 || !shown {
                        seq += 1;
                        frame.seq = seq;
                        frame.unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                        if let Err(e) = ens_overlay::frame::write_atomic(&out.join(ens_overlay::frame::FILE), &frame) {
                            eprintln!("overlay: {e}");
                        }
                        (last, written, shown) = (frame.pixels, Instant::now(), true);
                    }
                }
            }
            if !showing && shown {
                if let Some(out) = out.as_ref() {
                    let _ = std::fs::remove_file(out.join(ens_overlay::frame::FILE));
                }
                shown = false;
            }
            let status = loader::Status {
                meter: on,
                game: pid.is_some(),
                state: watch.state.clone(),
                in_world: watch.in_world(),
                joined: watch.joined(),
                logger: has("ens_hook.dll"),
                logger_error: logger_error.clone(),
                log: dir.as_ref().and_then(|d| live::newest(d)).and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())),
                layer_installed: ens_overlay::install::installed_dir().is_some(),
                overlay_loaded: has(ens_overlay::install::DLL),
                showing,
                picture_age: captured.as_ref().map(|(_, t)| t.elapsed().as_secs_f64()),
                error: if dir.is_none() {
                    Some("no Combat Logger folder found next to the planner".into())
                } else {
                    None
                },
                dir: dir.as_ref().map(|d| d.display().to_string()),
            };
            if let Ok(mut s) = app.state::<Mutex<loader::Status>>().lock() {
                *s = status;
            }
        }
    });
}

/// The player whose breakdown the game's tooltip shows (None: no tooltip), set by the in-game meter page's hover.
static TIP_PLAYER: Mutex<Option<u32>> = Mutex::new(None);

/// The breakdown tooltip over the meter window: shown beside the pointer (screen CSS px) for that player.
#[tauri::command]
// async: Tauri runs it off the main thread; building a window in a synchronous command can deadlock on Windows
async fn tip_show(app: tauri::AppHandle, player: u32, x: f64, y: f64) -> Result<(), String> {
    let w = capture::open_tip(&app, capture::TIP)?;
    let _ = w.eval(format!("showPlayer({player})"));
    let _ = w.set_position(tauri::LogicalPosition::new(x + 18.0, y + 18.0));
    w.show().map_err(|e| e.to_string())
}

#[tauri::command]
fn tip_move(app: tauri::AppHandle, x: f64, y: f64) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(capture::TIP) {
        let _ = w.set_position(tauri::LogicalPosition::new(x + 18.0, y + 18.0));
    }
}

#[tauri::command]
fn tip_hide(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(capture::TIP) {
        let _ = w.hide();
        let _ = w.eval("showPlayer(null)");
    }
}

/// The tooltip page asks for its window to fit what it shows (CSS px).
#[tauri::command]
fn tip_fit(window: tauri::WebviewWindow, w: f64, h: f64) {
    let _ = window.set_size(tauri::LogicalSize::new(w.clamp(50.0, 800.0), h.clamp(30.0, 1200.0)));
}

/// The in-game meter page's hover: whose breakdown the game's tooltip shows (None: hide it).
#[tauri::command]
// async for the same reason as tip_show
async fn tip_game(app: tauri::AppHandle, player: Option<u32>) -> Result<(), String> {
    let w = capture::open_tip(&app, capture::TIP_GAME)?;
    let _ = w.eval(match player { Some(p) => format!("showPlayer({p})"), None => "showPlayer(null)".into() });
    *TIP_PLAYER.lock().map_err(|e| e.to_string())? = player;
    INPUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// How many inputs from the game have arrived: the loader captures the meter at once when this changes.
static INPUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Keys and clicks from the game (the layer's input.rs) on 127.0.0.1: tab and see-through go to the overlay page,
/// move and hide change the saved settings, clicks and the wheel land on the page at that point. Returns the port
/// (0 when none could be opened: the meter then only shows).
fn input_thread(app: tauri::AppHandle) -> u16 {
    use tauri::Manager;
    let Ok(sock) = std::net::UdpSocket::bind("127.0.0.1:0") else { return 0 };
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            if !from.ip().is_loopback() {
                continue;
            }
            let Some(msg) = std::str::from_utf8(&buf[..n]).ok().and_then(loader::parse_input) else { continue };
            if !matches!(msg, loader::Input::Hover(..)) {
                eprintln!("overlay input: {msg:?}");
            }
            INPUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let page = app.get_webview_window(capture::LABEL);
            let scale = page.as_ref().and_then(|w| w.scale_factor().ok()).unwrap_or(1.0);
            let css = |v: i32| v as f64 / scale;
            let js = match msg {
                loader::Input::Tab => Some("overlayCmd('tab')".to_string()),
                loader::Input::Fade => Some("overlayCmd('fade')".to_string()),
                loader::Input::Click(x, y) => Some(format!("overlayClick({}, {})", css(x), css(y))),
                loader::Input::Hover(x, _) if x < 0 => Some("overlayHover(-1, -1)".to_string()),
                loader::Input::Hover(x, y) => Some(format!("overlayHover({}, {})", css(x), css(y))),
                loader::Input::Wheel(x, y, dy) => Some(format!("overlayWheel({}, {}, {})", css(x), css(y), dy)),
                loader::Input::Move | loader::Input::Hide | loader::Input::MoveTo(..) | loader::Input::Size(..) => {
                    let path = app.path().app_data_dir().map(|d| d.join("extensions.json"));
                    if let Ok(mut s) = app.state::<Mutex<loader::Settings>>().lock() {
                        match msg {
                            // F8 goes to the next preset spot
                            loader::Input::Move => (s.place, s.moved) = ((s.place + 1) % loader::PLACES.len(), None),
                            loader::Input::MoveTo(x, y) => s.moved = Some((x, y)),
                            loader::Input::Size(w, h) => {
                                let size = (css(w).max(input_min().0), css(h).max(input_min().1));
                                s.size = Some(size);
                                if let Some(page) = &page {
                                    let _ = page.set_size(tauri::LogicalSize::new(size.0, size.1));
                                }
                            }
                            _ => s.hidden = !s.hidden,
                        }
                        if let Ok(p) = path {
                            let _ = s.save(&p);
                        }
                    }
                    None
                }
            };
            if let (Some(js), Some(w)) = (js, page) {
                let _ = w.eval(js);
            }
        }
    });
    port
}

/// The smallest in-game meter, in CSS px (input.rs in ens-overlay keeps resizing above it too).
fn input_min() -> (f64, f64) {
    (ens_overlay::input::MIN_SIZE.0 as f64, ens_overlay::input::MIN_SIZE.1 as f64)
}

/// The Extensions panel's view: the switch and what the loader sees.
#[tauri::command]
fn extensions_state(status: State<Mutex<loader::Status>>, settings: State<Mutex<loader::Settings>>) -> Value {
    let s = status.lock().map(|s| s.clone()).unwrap_or_default();
    let mut v = serde_json::to_value(&s).unwrap_or_default();
    v["restart_needed"] = json!(s.restart_needed());
    if let Ok(t) = settings.lock() {
        v["place"] = json!(if t.moved.is_some() { -1 } else { (t.place % loader::PLACES.len()) as i64 });
        v["hidden"] = json!(t.hidden);
        v["size"] = json!(t.size);
    }
    v
}

/// Where the in-game meter goes (an index into loader::PLACES) and whether it is hidden; the same as F8 and F9.
#[tauri::command]
fn extensions_layout(app: tauri::AppHandle, place: i64, hidden: bool, reset_size: bool) -> Result<(), String> {
    use tauri::Manager;
    let path = app.path().app_data_dir().map_err(|e| e.to_string())?.join("extensions.json");
    let state = app.state::<Mutex<loader::Settings>>();
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.hidden = hidden;
    // -1: keep the spot it was dragged to
    if place >= 0 {
        (s.place, s.moved) = (place as usize % loader::PLACES.len(), None);
    }
    if reset_size {
        s.size = None;
        if let Some(page) = app.get_webview_window(capture::LABEL) {
            let _ = page.set_size(tauri::LogicalSize::new(capture::WIDTH, capture::HEIGHT));
        }
    }
    s.save(&path)
}

/// Turn Extensions → Live DPS meter on or off. On registers the in-game layer (used from the next game start); off
/// removes the registration, and the loader stops drawing at once.
#[tauri::command]
fn extensions_set(app: tauri::AppHandle, meter: bool) -> Result<Value, String> {
    use tauri::Manager;
    let path = app.path().app_data_dir().map_err(|e| e.to_string())?.join("extensions.json");
    {
        let state = app.state::<Mutex<loader::Settings>>();
        let mut s = state.lock().map_err(|e| e.to_string())?;
        s.meter = meter;
        s.save(&path)?;
    }
    if meter {
        let dir = live::log_dir().ok_or("no Combat Logger folder found next to the planner")?;
        loader::install_layer(&dir)?;
    } else {
        ens_overlay::install::uninstall();
    }
    Ok(json!({"meter": meter}))
}

/// Open the live meter: a small frameless window that stays on top of the game (borderless windowed mode).
#[tauri::command]
// async for the same reason as tip_show
async fn meter_open(app: tauri::AppHandle) -> Result<(), String> {
    open_meter(&app)
}

fn open_meter(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("meter") {
        let _ = w.show();
        return w.set_focus().map_err(|e| e.to_string());
    }
    tauri::WebviewWindowBuilder::new(app, "meter", tauri::WebviewUrl::App("meter.html".into()))
        .additional_browser_args(capture::BROWSER_ARGS)
        .title("Damage meter")
        .inner_size(340.0, 440.0)
        .min_inner_size(240.0, 160.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(false)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn lock<'a>(st: &'a St) -> std::sync::MutexGuard<'a, Planner> {
    st.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
fn catalog(st: St) -> Value {
    views::catalog(&lock(&st))
}

#[tauri::command]
fn state(st: St) -> Value {
    views::state(&mut lock(&st))
}

/// Start from a preset (runs the optimizer briefly, so it stays off the window's thread).
#[tauri::command]
async fn preset(st: St<'_>, kind: String) -> Result<Value, String> {
    let mut p = lock(&st);
    p.act(&json!({"type": "preset", "kind": kind}))?;
    p.autosave()?;
    Ok(views::state(&mut p))
}

#[tauri::command]
fn act(st: St, action: Value) -> Result<Value, String> {
    let mut p = lock(&st);
    p.act(&action)?;
    p.autosave()?;
    Ok(views::state(&mut p))
}

/// The saved builds, newest first, and which one is open.
#[tauri::command]
fn builds(st: St) -> Value {
    lock(&st).builds()
}

/// Open, start, copy or delete a saved build; answers the builds list.
#[tauri::command]
fn build_file(st: St, op: String, id: Option<String>) -> Result<Value, String> {
    let mut p = lock(&st);
    let id = id.unwrap_or_default();
    match op.as_str() {
        "open" => p.open_build(&id)?,
        "new" => p.new_build()?,
        "duplicate" => p.duplicate_build(&id)?,
        "delete" => p.delete_build(&id)?,
        other => return Err(format!("unknown build operation {other}")),
    }
    Ok(p.builds())
}

#[tauri::command]
fn summary(st: St, preview: Option<Value>) -> Value {
    views::summary(&mut lock(&st), preview.as_ref())
}

#[tauri::command]
fn overview(st: St) -> Value {
    views::overview(&mut lock(&st))
}

#[tauri::command]
fn calcs(st: St) -> Value {
    views::calcs(&mut lock(&st))
}

#[tauri::command]
fn sheet(st: St, query: String, all: bool) -> Value {
    views::sheet(&lock(&st), &query, all)
}

#[tauri::command]
fn code(st: St) -> String {
    lock(&st).share_code()
}

#[tauri::command]
fn text_summary(st: St) -> String {
    lock(&st).text_summary()
}

/// Every item that fits a slot, each tried in the build (runs off the UI thread).
#[tauri::command]
async fn candidates(st: St<'_>, slot: String, owned: Option<bool>, all: Option<bool>) -> Result<Value, String> {
    let slot = Slot::parse(&slot).ok_or("bad slot")?;
    let owned = owned.unwrap_or(false);
    let all = all.unwrap_or(false);
    let (opt, build, c, guids, b, mine) = {
        let p = lock(&st);
        let summons: Vec<String> = p.lists.summons.iter().map(|x| x.guid.clone()).collect();
        let mine: Vec<(usize, store::OwnedItem)> = if owned {
            game::owned_for_slot(p.b, &p.build, &p.owned, slot, &summons).into_iter().map(|k| (k, p.owned[k].clone())).collect()
        } else {
            vec![]
        };
        (p.opt, p.build.clone(), p.conditions(), if owned { vec![] } else { views::candidate_guids(&p, slot, all) }, p.b, mine)
    };
    Ok(candidate_table(opt, b, &build, &c, slot, guids, &mine))
}

/// Candidates within this share of each other's total DPS are a sidegrade, not a ranking: with no measured vectors
/// yet the model's error is unknown, and gaps under 1% are within it.
const SIDEGRADE_BAND: f64 = 0.01;

/// Every candidate for `slot` tried in the build, best first, one row per name and result (R1-DR-09: every
/// candidate is evaluated, none dropped before ranking).
fn candidate_table(opt: &Optimizer, b: &Bundle, build: &Build, c: &Conditions, slot: Slot, guids: Vec<String>, mine: &[(usize, store::OwnedItem)]) -> Value {
    let (build, c) = (build.clone(), c.clone());
    let now = summarize(&opt.engine, &build, &c);
    let cur = equipped(&build, slot);
    // (guid, dps, hits survived, owned index, the owned item's rarity / level)
    let mut rows: Vec<(String, f64, f64, Option<usize>, Option<ens_calc::Equip>)> = guids.into_iter().map(|g| {
        let nb = apply_slot(b, &build, slot, &g);
        let s = summarize(&opt.engine, &nb, &c);
        (g, s.dps, s.hits_survived, None, None)
    }).collect();
    rows.extend(mine.iter().map(|(k, o)| {
        let s = summarize(&opt.engine, &game::with_owned(opt.bundle, &build, slot, o), &c);
        (o.item.item.clone(), s.dps, s.hits_survived, Some(*k), Some(o.item.clone()))
    }));
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)));
    // the data has several copies of some items (one per drop source); keep one row per name and result
    let mut seen = std::collections::HashSet::new();
    rows.retain(|(g, d, s, _, e)| {
        let lv = e.as_ref().map(|e| (e.rarity.clone(), e.level, e.perks));
        seen.insert((b.items.get(g).map(|i| i.name.clone()).unwrap_or_default(), (d * 10.0).round() as i64, (s * 100.0).round() as i64, lv))
    });
    let worn = |e: &Option<ens_calc::Equip>| match (slot, e) {
        (Slot::Weapon, Some(e)) => build.weapon.as_ref() == Some(e),
        (Slot::Gem, Some(e)) => build.gem.as_ref() == Some(e),
        (Slot::Armor(k), Some(e)) => build.gear[k].as_ref() == Some(e),
        _ => true,
    };
    // sidegrade groups: a row within SIDEGRADE_BAND of the row above it shares that row's group, so no two
    // neighbours closer than the band are ever ranked against each other (a group can span more than the band)
    let mut group = vec![0usize; rows.len()];
    for i in 1..rows.len() {
        group[i] = group[i - 1];
        if rows[i].1 < rows[i - 1].1 * (1.0 - SIDEGRADE_BAND) - 1e-9 {
            group[i] += 1;
        }
    }
    let list: Vec<Value> = rows.iter().enumerate().map(|(i, (g, d, s, k, e))| {
        let it = &b.items[g];
        let (rarity, level) = candidate_shown(b, &build, slot, g, e);
        let tied = group.iter().filter(|x| **x == group[i]).count() > 1;
        // within the band of the row directly above: the only claim the page makes (groups chain, so a group can
        // span far more than the band)
        let tie_above = i > 0 && group[i] == group[i - 1];
        // the perks the item carries at the rarity the row shows, so the list can be searched by perk
        let perks: Vec<String> = it.perks.iter().filter_map(|pid| b.perks.get(pid)).take(ens_calc::active_perks(&rarity)).map(|x| x.name.clone()).collect();
        json!({"guid": g, "name": it.name, "rarity": rarity, "dps": d, "survived": s, "owned": k, "perks": perks,
               "level": level, "group": group[i], "tied": tied, "tieAbove": tie_above,
               "equipped": cur.as_deref() == Some(g.as_str()) && worn(e) && !matches!(slot, Slot::Food | Slot::Summon)})
    }).collect();
    json!({"slot": slot.name(), "now": {"dps": now.dps, "survived": now.hits_survived}, "rows": list})
}

/// The rarity and level a candidate row was priced at: an owned item's own, else what `apply_slot` gives it.
fn candidate_shown(b: &Bundle, build: &Build, slot: Slot, guid: &str, owned: &Option<ens_calc::Equip>) -> (String, Option<u32>) {
    let it = &b.items[guid];
    if let Some(e) = owned {
        return (e.rarity.clone().or(it.rarity.clone()).unwrap_or_default(), e.level);
    }
    match slot {
        Slot::Weapon => {
            let w = apply_slot(b, build, slot, guid).weapon;
            (w.as_ref().and_then(|w| w.rarity.clone()).or(it.rarity.clone()).unwrap_or_default(), w.and_then(|w| w.level))
        }
        _ => (it.rarity.clone().unwrap_or_default(), None),
    }
}

/// DPS gained per skill point for every node, path included (runs off the UI thread).
#[tauri::command]
async fn heatmap(st: St<'_>) -> Result<Vec<Option<f64>>, String> {
    let (opt, build, c) = {
        let p = lock(&st);
        (p.opt, p.build.clone(), p.conditions())
    };
    let base = opt.engine.total(&build, &c.target).total;
    Ok((0..build.ranks.len()).map(|i| match path_to(opt, &build.ranks, i) {
        Some(p) if !p.is_empty() => {
            let pts: u32 = p.iter().map(|&j| opt.bundle.skills[j].cost.max(1)).sum();
            Some((opt.engine.total(&with_path(&build, &p), &c.target).total - base) / pts as f64)
        }
        _ => None,
    }).collect())
}

/// Run the optimizer for this loadout (off the UI thread) and keep the result for Apply.
#[tauri::command]
async fn optimize(st: St<'_>) -> Result<Value, String> {
    let (opt, plan, build, c, settings, owned, summons) = {
        let p = lock(&st);
        let summons: Vec<String> = p.lists.summons.iter().map(|x| x.guid.clone()).collect();
        (p.opt, optim::plan_from(&p), p.build.clone(), p.conditions(), p.opt_settings.clone(), p.owned.clone(), summons)
    };
    optim::RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
    let empty = vec![0u8; opt.bundle.skills.len()];
    let iters = settings.iters();
    let (best, order, pl, gear_pick) = match settings.gear.as_str() {
        "best" => {
            let (best, order, pl) = opt.optimize_with_gear(&plan, iters, 7, &settings.rarity, 2);
            let g = Some((pl.weapon.clone(), pl.gear.clone()));
            (best, order, pl, g)
        }
        "owned" if !owned.is_empty() => {
            let pl = opt.prepare(&plan, &empty);
            let (best, order) = opt.optimize(&pl, iters, 7);
            let mut nb = optim::build_from(&build, &pl, &best.ranks, &[], build.name.clone());
            nb = game::best_owned(opt.bundle, &nb, &owned, &summons, |t| {
                let s = summarize(&opt.engine, t, &c);
                (s.dps, s.hits_survived)
            });
            let mut pl2 = pl.clone();
            (pl2.weapon, pl2.ammo, pl2.gear) = (nb.weapon.clone(), nb.ammo.clone(), nb.gear.clone());
            pl2.weapons = std::iter::once(WeaponPlan { weapon: nb.weapon.clone(), ammo: nb.ammo.clone(), share: 1.0, ..Default::default() })
                .chain(nb.carried.iter().map(|w| WeaponPlan { weapon: Some(w.weapon.clone()), ammo: w.ammo.clone(), share: 0.0, ..Default::default() }))
                .collect();
            let g = Some((nb.weapon.clone(), nb.gear.clone()));
            (best, order, pl2, g)
        }
        _ => {
            let pl = opt.prepare(&plan, &empty);
            let (best, order) = opt.optimize(&pl, iters, 7);
            (best, order, pl, None)
        }
    };
    optim::RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut nb = optim::build_from(&build, &pl, &best.ranks, &[], build.name.clone());
    if gear_pick.is_none() {
        nb.gear = build.gear.clone();
    }
    let summary = summarize(&opt.engine, &nb, &c);
    let mut p = lock(&st);
    p.opt_result = Some(OptResult { ranks: best.ranks, order, goals: best.goals, gear: gear_pick, summary, plan: Some(pl) });
    Ok(views::opt_result(&mut p))
}

/// Trees and gear for every milestone in the settings (runs off the UI thread; progress through `opt_progress`).
#[tauri::command]
async fn milestones(st: St<'_>) -> Result<Value, String> {
    let (opt, plan, settings) = {
        let p = lock(&st);
        (p.opt, optim::plan_from(&p), p.opt_settings.clone())
    };
    if settings.milestones.is_empty() {
        return Err("Add at least one milestone.".into());
    }
    let mut list = settings.milestones.clone();
    list.sort_by_key(|m| m.level);
    optim::DONE.store(0, std::sync::atomic::Ordering::Relaxed);
    optim::TOTAL.store(list.len(), std::sync::atomic::Ordering::Relaxed);
    optim::RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
    let opts = ens_opt::MilestoneOptions { gear: settings.gear != "keep", respec: settings.respec, iters: settings.iters() };
    let res = opt.milestones(&plan, &list, &opts, &|done, _| optim::DONE.store(done, std::sync::atomic::Ordering::Relaxed));
    optim::RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut p = lock(&st);
    p.ms_result = Some(res);
    Ok(optim::milestones_view(&p))
}

/// The last milestone plan, for the page (after making loadouts from it).
#[tauri::command]
fn milestones_view(st: St) -> Value {
    optim::milestones_view(&lock(&st))
}

/// The order to buy the open loadout's tree in (best gain per point first, paths included), with the character
/// level each step needs (runs off the UI thread).
#[tauri::command]
async fn tree_order(st: St<'_>) -> Result<Value, String> {
    let (opt, plan, ranks, found, stored) = {
        let p = lock(&st);
        let mut plan = optim::plan_from(&p);
        (plan.weapons, plan.required, plan.avoided) = (vec![], vec![], vec![]);
        let stored = (!p.build.order.is_empty() && p.build.order_matches() && steps_connected(p.opt, &p.build.order)).then(|| p.build.order.clone());
        (p.opt, plan, p.build.ranks.clone(), p.build.found_points, stored)
    };
    let fresh = stored.is_none();
    let v = tree_order_view(opt, &plan, &ranks, found, stored);
    if fresh {
        // keep the worked-out order with the build, if the tree is still the one it was worked out for
        let mut p = lock(&st);
        if p.build.ranks == ranks {
            p.build.order = serde_json::from_value(v["order"].clone()).unwrap_or_default();
            p.autosave()?;
        }
    }
    Ok(v)
}

/// Whether the tree stays connected after every step of `order`.
fn steps_connected(opt: &Optimizer, order: &[usize]) -> bool {
    let mut r = vec![0u8; opt.bundle.skills.len()];
    order.iter().all(|&i| {
        r[i] += 1;
        opt.connected(&r)
    })
}

/// The steps of a tree: `stored` (the build's own order) when given, else the order that gains most per point
/// (worked out here). Each step has the character level it needs.
fn tree_order_view(opt: &Optimizer, plan: &Plan, ranks: &[u8], found: Option<u32>, stored: Option<Vec<usize>>) -> Value {
    let mut order = stored.unwrap_or_else(|| opt.leveling_order(plan, &vec![0; ranks.len()], ranks));
    // a tree that is not connected (made by hand) cannot be bought in order: its unreachable ranks go at the end
    let mut got = vec![0u8; ranks.len()];
    for &i in &order {
        got[i] += 1;
    }
    let ordered = order.len();
    for i in 0..ranks.len() {
        for _ in got[i]..ranks[i] {
            order.push(i);
        }
    }
    let mut spent = 0u32;
    let levels: Vec<Option<u32>> = order
        .iter()
        .map(|&i| {
            spent += opt.bundle.skills[i].cost;
            (1..=ens_calc::build::level_cap(opt.bundle)).find(|&l| ens_opt::points_available(l, found) >= spent)
        })
        .collect();
    json!({ "order": order, "levels": levels, "unordered": order.len() - ordered })
}

/// Attack and weapon names and icons for the meter's breakdown tooltip (data/attack_names.json).
#[tauri::command]
fn attack_names() -> Value {
    let mut v: Value = serde_json::from_str(ATTACK_NAMES_JSON).unwrap_or_default();
    v["ammo"] = serde_json::from_str::<Value>(AMMO_JSON).ok().and_then(|a| a.get("items").cloned()).unwrap_or_default();
    v
}

/// Skill icons by node id: {"<id>": {"on": data URL, "off": data URL}} (active skills only; the game has none for
/// passives and attributes).
#[tauri::command]
fn skill_icons() -> Value {
    serde_json::from_str::<Value>(SKILL_ICONS_JSON).ok().and_then(|v| v.get("icons").cloned()).unwrap_or(Value::Null)
}

/// How far the running optimizer job is.
#[tauri::command]
fn opt_progress() -> Value {
    optim::progress()
}

/// What the optimizer settings can choose from: weapon categories, arrow and spell families, default milestones.
#[tauri::command]
fn opt_catalog(st: St) -> Value {
    let p = lock(&st);
    json!({
        "categories": p.opt.weapon_categories(),
        "arrows": p.opt.ammo_families("Arrow"),
        "spells": p.opt.ammo_families("Spell"),
        "summons": p.opt.summon_families(),
        "milestones": ens_opt::DEFAULT_MILESTONES,
        "settings": p.opt_settings,
    })
}

#[tauri::command]
fn load_game(st: St, dir: Option<String>) -> Result<Value, String> {
    let mut p = lock(&st);
    // a folder the player chose (when the Steam folder has no save) comes first
    let mut dirs: Vec<std::path::PathBuf> = dir.filter(|d| !d.trim().is_empty()).map(|d| std::path::PathBuf::from(d.trim())).into_iter().collect();
    dirs.extend(ens_data::save::save_dirs());
    let v = p.load_game(&dirs);
    p.autosave()?;
    Ok(v)
}

fn main() {
    tauri::Builder::default()
        .manage(Mutex::new(Planner::new()))
        .manage(Mutex::new(live::Live::default()))
        .manage(Mutex::new(loader::Status::default()))
        .setup(|app| {
            // saved builds live in the app's data folder; the test copy (ENS_SAMPLE) keeps its own folder
            use tauri::Manager;
            let sub = if std::env::var("ENS_SAMPLE").is_ok() { "builds-test" } else { "builds" };
            match app.path().app_data_dir() {
                Ok(dir) => {
                    let st: State<Mutex<Planner>> = app.state();
                    if let Err(e) = lock(&st).attach_store(dir.join(sub)) {
                        eprintln!("saved builds: {e}");
                    }
                }
                Err(e) => eprintln!("saved builds: no app data folder: {e}"),
            }
            let ext = app.path().app_data_dir().map(|d| loader::Settings::load(&d.join("extensions.json"))).unwrap_or_default();
            app.manage(Mutex::new(ext));
            // --meter: only the live meter (the planner window stays hidden; its build still prices the meter's comparison)
            if std::env::args().any(|a| a == "--meter") {
                if let Some(w) = tauri::Manager::get_webview_window(app, "main") {
                    let _ = w.hide();
                }
                if let Err(e) = open_meter(app.handle()) {
                    eprintln!("live meter: {e}");
                }
            }
            loader_thread(app.handle().clone());
            // ENS_SAMPLE (testing with the sample build): a maximized window titled as the test copy
            if std::env::var("ENS_SAMPLE").is_ok() {
                if let Some(w) = tauri::Manager::get_webview_window(app, "main") {
                    let _ = w.set_title("Unshrouded (test)");
                    let _ = w.maximize();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![catalog, state, act, preset, summary, overview, calcs, sheet, code, text_summary, candidates, heatmap, optimize, load_game, builds, build_file, milestones, milestones_view, opt_progress, opt_catalog, tree_order, skill_icons, meter_state, meter_open, extensions_state, extensions_set, extensions_layout, tip_show, tip_move, tip_hide, tip_fit, tip_game, attack_names])
        // the hidden pages captured for the game are windows too: closing the planner (or, with --meter, the meter
        // window) ends the app instead of leaving it running unseen
        .on_window_event(|w, e| {
            use tauri::Manager;
            if let tauri::WindowEvent::Destroyed = e {
                let main_shown = w.app_handle().get_webview_window("main").is_some_and(|m| m.is_visible().unwrap_or(false));
                if w.label() == "main" || (w.label() == "meter" && !main_shown) {
                    w.app_handle().exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start the planner window");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commands answer for the sample build: 422.8 total DPS (since R6-PS-01 its Eternal Frost Arrow III counts at its
    /// upgraded level 50 with its perks; the egui app showed 409.0 before R6).
    #[test]
    fn commands_answer_for_the_sample_build() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let mut p = Planner::new();
        let cat = views::catalog(&p);
        assert_eq!(cat["skills"].as_array().unwrap().len(), p.b.skills.len());
        assert!(!cat["edges"].as_array().unwrap().is_empty());
        let s = views::summary(&mut p, None);
        let dps = s["now"]["dps"].as_f64().unwrap();
        assert!((dps - 422.8).abs() < 0.1, "dps {dps}");
        let o = views::overview(&mut p);
        assert!(o["chain"].as_array().unwrap().len() >= 5);
        assert!(!o["insights"].as_array().unwrap().is_empty());
        assert_eq!(views::calcs(&mut p)["sections"].as_array().unwrap().len(), 6);
        assert!(!views::sheet(&p, "", false).as_array().unwrap().is_empty());
        // regeneration reads as the game's attribute, with the per-second refill (× 0.2) after it
        let regen = views::sheet(&p, "mana regeneration", false);
        let v = regen[0]["value"].as_str().unwrap().to_string();
        let raw = p.engine.sheet(&p.build).values["Mana_Recharge"];
        assert_eq!(v, format!("{} ({:.2} /s)", labels::trim_num(raw), raw * 0.2), "{regen}");
        assert!(!views::candidate_guids(&p, Slot::Weapon, false).is_empty());
        let node = p.b.skills.iter().position(|x| x.kind != "Root").unwrap();
        let pv = views::summary(&mut p, Some(&json!({"node": node})));
        assert!(pv.get("preview").is_some() || p.build.ranks[node] > 0);
        p.act(&json!({"type": "level", "value": 40})).unwrap();
        assert_eq!(p.build.level, 40);
        p.act(&json!({"type": "undo"})).unwrap();
        assert_eq!(p.build.level, 50);
        let st = views::state(&mut p);
        assert_eq!(st["slots"].as_array().unwrap().len(), 12);
    }
}

#[cfg(test)]
mod weapons {
    use super::*;

    /// Carry a second weapon, take it in hand (gem and ammo go with each weapon), drop one; one undo step each.
    #[test]
    fn carry_use_and_drop_weapons() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let mut p = Planner::new();
        let bow = p.build.weapon.clone().unwrap();
        let (gem, ammo) = (p.build.gem.clone(), p.build.ammo.clone());
        let sword = p.lists.weapons.iter().find(|w| p.b.items[&w.guid].slot.as_deref() == Some("MeleeWeapon")).unwrap().guid.clone();

        p.act(&json!({ "type": "carry_weapon", "guid": sword })).unwrap();
        assert_eq!(p.build.weapon.as_ref(), Some(&bow), "carrying does not change the weapon in hand");
        assert_eq!(p.build.carried.len(), 1);
        let st = views::state(&mut p);
        assert_eq!(st["carried"][0]["name"], json!(p.item_name(&sword)));
        assert!(st["carried"][0]["dps"].as_f64().unwrap().is_finite());

        p.act(&json!({ "type": "use_weapon", "index": 0 })).unwrap();
        assert_eq!(p.build.weapon.as_ref().map(|w| w.item.as_str()), Some(sword.as_str()));
        assert_eq!((p.build.gem.is_none(), p.build.ammo.is_none()), (true, true));
        assert_eq!(p.build.carried[0].weapon, bow);
        assert_eq!((&p.build.carried[0].gem, &p.build.carried[0].ammo), (&gem, &ammo));

        p.act(&json!({ "type": "use_weapon", "index": 0 })).unwrap();
        assert_eq!((p.build.weapon.as_ref(), &p.build.gem, &p.build.ammo), (Some(&bow), &gem, &ammo));

        p.act(&json!({ "type": "drop_weapon", "index": 0 })).unwrap();
        assert!(p.build.carried.is_empty());
        p.act(&json!({ "type": "undo" })).unwrap();
        assert_eq!(p.build.carried.len(), 1);
    }
}

#[cfg(test)]
mod from_save {
    use super::*;

    /// Loading from the game takes the newest save's level and ranks, as one undo step.
    #[test]
    fn load_game_applies_the_newest_save() {
        let dir = std::env::temp_dir().join(format!("ens-load-game-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Planner::new();
        let attr = p.b.skills.iter().find(|s| s.kind == "Attribute").unwrap();
        std::fs::write(dir.join("character1.ksd"), ens_data::save::fixture::character_file("Old", 3, 0, &[])).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(dir.join("character1.ksd")).unwrap().set_modified(old).unwrap();
        std::fs::write(dir.join("character4.ksd"), ens_data::save::fixture::character_file("Roth", 13, 100, &[(attr.id, 1)])).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        std::fs::write(dir.join("world1.ksd"), ens_data::save::fixture::world_file("Elsewhere", 6, now - 86_400)).unwrap();
        std::fs::write(dir.join("world2.ksd"), ens_data::save::fixture::world_file("Home", 3, now)).unwrap();
        let dirs = [dir.clone()];
        let level_before = p.build.level;
        let undo_before = p.undo.len();

        let s = p.load_game(&dirs);
        assert_eq!((s["name"].as_str(), s["level"].as_u64(), s["updated"].as_bool()), (Some("Roth"), Some(13), Some(true)));
        assert_eq!(p.build.level, 13);
        assert_eq!((p.build.flame_level, s["flame"].as_u64(), s["world"].as_str()), (3, Some(3), Some("Home")), "world saved with the character");
        assert_eq!(p.build.ranks[attr.index], 1);
        assert_eq!(p.build.ranks.iter().map(|&x| x as u32).sum::<u32>(), 1);
        assert_eq!(p.undo.len(), undo_before + 1);

        // loading the same save again changes nothing and adds no undo step
        assert_eq!(p.load_game(&dirs)["updated"], json!(false));
        assert_eq!(p.undo.len(), undo_before + 1);
        p.act(&json!({ "type": "undo" })).unwrap();
        assert_eq!(p.build.level, level_before);

        // with an item part: gear, the arrow, and both weapons (one in hand, one carried)
        let id = |name: &str| p.b.items.iter().find(|(_, i)| i.name == name).map(|(g, _)| (g.clone(), ens_data::save::item_id(g).unwrap())).unwrap();
        let (hood, hood_id) = id("Marksman Hood");
        let (bow, bow_id) = id("Banebringer");
        let (axe, axe_id) = id("Cyclone Axe");
        let (arrow, arrow_id) = id("Wooden Arrow");
        use ens_data::save::fixture::{character_file_with_items, item_record, item_section};
        let items = item_section(
            &[(5, item_record(0, hood_id, 1, Some(13), Some((3, 3))))],
            Some(1),
            None,
            &[item_record(0, bow_id, 1, Some(15), Some((5, 0))), item_record(1, arrow_id, 400, None, None), item_record(2, axe_id, 1, Some(15), Some((5, 0)))],
        );
        std::fs::write(dir.join("character4.ksd"), character_file_with_items("Roth", 13, 100, &[], &items)).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(dir.join("character4.ksd")).unwrap().set_modified(later).unwrap();
        p.build.weapon = None;
        let s = p.load_game(&dirs);
        assert_eq!((s["gear"].as_u64(), s["weapons"].as_u64(), s["itemsError"].is_null()), (Some(1), Some(2), true));
        // owned: the worn hood, both weapons and the arrows, each with its own fields
        assert_eq!(s["owned"].as_u64(), Some(4));
        assert_eq!(p.owned.iter().filter(|o| o.equipped).count(), 1);
        let arrows = p.owned.iter().find(|o| o.item.item == arrow).unwrap();
        assert_eq!(arrows.count, 400);
        assert_eq!(p.owned_from.as_ref().map(|o| o.character.as_str()), Some("Roth"));
        let head = p.build.gear[0].clone().unwrap();
        assert_eq!((head.item.as_str(), head.rarity.as_deref(), head.level, head.perks), (hood.as_str(), Some("Rare"), Some(13), Some(3)));
        assert!(p.build.gear[1..].iter().all(|g| g.is_none()), "slots the save leaves empty are emptied");
        let in_hand = p.build.weapon.as_ref().unwrap().item.clone();
        assert!(in_hand == bow || in_hand == axe);
        assert_eq!(p.build.carried.len(), 1);
        let bow_ammo = if in_hand == bow { p.build.ammo.clone() } else { p.build.carried[0].ammo.clone() };
        assert_eq!(bow_ammo, Some(arrow), "the bow fires the equipped arrow");
        let axe_ammo = if in_hand == axe { p.build.ammo.clone() } else { p.build.carried[0].ammo.clone() };
        assert_eq!(axe_ammo, None);

        // the weapon in hand stays in hand on the next load
        p.act(&json!({ "type": "use_weapon", "index": 0 })).unwrap();
        let chosen = p.build.weapon.clone();
        p.load_game(&dirs);
        assert_eq!(p.build.weapon.as_ref().map(|w| &w.item), chosen.as_ref().map(|w| &w.item));

        let none = p.load_game(&[dir.join("missing")]);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(none["found"], json!(false));
    }

    /// The newest real character save fills the planner (run with --ignored --nocapture).
    #[test]
    #[ignore = "reads the player's real saves under the Steam folder"]
    fn load_game_reads_the_real_save() {
        let mut p = Planner::new();
        let s = p.load_game(&ens_data::save::save_dirs());
        println!("{s}");
        assert_eq!(s["found"], json!(true));
        assert!(s.get("error").is_none());
        assert_eq!(p.build.level as u64, s["level"].as_u64().unwrap());
        assert_eq!(s["unknown"], json!(0));
        assert_eq!((s["itemsError"].is_null(), s["unknownItems"].as_u64()), (true, Some(0)));
        let st = views::state(&mut p);
        println!("points {} / {} (found {})", st["spent"], st["have"], st["foundPoints"]);
        assert!(st["spent"].as_u64() <= st["have"].as_u64(), "a loaded character is never over budget");
        println!("owned {}: {:?}", p.owned.len(), p.owned.iter().map(|o| p.item_name(&o.item.item)).collect::<Vec<_>>());
        assert!(p.owned.len() >= 7 + 6, "armour, rings and weapons at least");
        let before = p.summary().dps;
        println!("best owned: gear {:?}", p.build.gear.iter().map(|g| g.as_ref().map(|e| p.item_name(&e.item)).unwrap_or_default()).collect::<Vec<_>>());
        println!("best owned: in hand {:?} + {:?}, dps {before:.1} -> {:.1}", p.build.weapon.as_ref().map(|e| p.item_name(&e.item)), p.build.ammo.as_ref().map(|a| p.item_name(a)), p.summary().dps);
        assert!(p.summary().dps >= before - 1e-6, "never worse than the loaded gear");
        let names = |v: Vec<Option<&ens_calc::Equip>>| v.into_iter().map(|e| e.map(|e| p.item_name(&e.item)).unwrap_or_default()).collect::<Vec<_>>();
        println!("gear {:?}", names(p.build.gear.iter().map(|g| g.as_ref()).collect()));
        println!("in hand {:?} ammo {:?}", names(vec![p.build.weapon.as_ref()]), p.build.ammo.as_ref().map(|a| p.item_name(a)));
        println!("carried {:?}", names(p.build.carried.iter().map(|c| Some(&c.weapon)).collect()));
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// How long the optimizer takes for the sample build, with and without gear (run with --ignored --nocapture).
    #[test]
    #[ignore]
    fn optimizer_time() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let p = Planner::new();
        let plan = Plan {
            level: p.build.level, flame_level: p.build.flame_level, weapon: p.build.weapon.clone(), ammo: p.build.ammo.clone(),
            gear: p.build.gear.clone(), foods: p.build.foods.clone(), summons: p.build.summons.clone(), ranking: p.ranking.clone(),
            attacks_per_s: p.build.weapon.as_ref().map(|w| ens_opt::attack_rate(&p.engine, &p.build, w, p.build.ammo.as_deref())).unwrap_or(1.0),
            target: p.conditions().target,
            found_points: p.build.found_points,
            weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: Default::default(),
        };
        let t = Instant::now();
        let _ = p.opt.optimize(&plan, 40, 7);
        eprintln!("tree only: {:.1}s", t.elapsed().as_secs_f64());
        let t = Instant::now();
        let _ = p.opt.optimize_with_gear(&plan, 40, 7, "Legendary", 2);
        eprintln!("with gear: {:.1}s", t.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
mod stat_screen {
    use super::*;

    /// Roth's character screen, 2026-10-21 (level 13, Flame 2): Constitution 3, Spirit 1, Endurance 5, Strength 1,
    /// Dexterity 8, Intelligence 1, Health 250, Mana 120, Stamina 302, Mana Regeneration 5, Stamina Regeneration
    /// 146. The newest save loaded at Flame 2 must read the same (run with --ignored --nocapture).
    #[test]
    #[ignore = "reads the player's real save under the Steam folder"]
    fn loaded_save_matches_the_character_screen() {
        let mut p = Planner::new();
        let s = p.load_game(&ens_data::save::save_dirs());
        assert_eq!(s["name"], json!("Roth"));
        p.build.flame_level = 2;
        let v = p.engine.sheet(&p.build).values;
        let got: Vec<(&str, f64)> = ["Constitution", "Spirit", "Endurance", "Strength", "Dexterity", "Intelligence", "Health", "Mana", "Stamina", "Mana_Recharge", "Stamina_Recharge"]
            .iter().map(|k| (*k, v[*k])).collect();
        println!("{got:?}");
        let want = [3.0, 1.0, 5.0, 1.0, 8.0, 1.0, 250.0, 120.0, 302.0, 5.0, 146.0];
        assert_eq!(got.iter().map(|x| x.1).collect::<Vec<_>>(), want);
    }
}

#[cfg(test)]
mod saved_builds {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ens-builds-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Every change is saved; a second planner opening the same folder gets the build back exactly.
    #[test]
    fn builds_round_trip_through_the_store() {
        let dir = temp_dir("round");
        let mut p = Planner::new();
        p.attach_store(dir.clone()).unwrap();
        assert_eq!(store::list(&dir).len(), 1, "the first build is saved on attach");

        p.act(&json!({ "type": "build_name", "value": "Frost archer" })).unwrap();
        p.act(&json!({ "type": "level", "value": 17 })).unwrap();
        p.act(&json!({ "type": "notes", "value": "farm Talion's Bow" })).unwrap();
        let mut f = p.fight.clone();
        f.enemy_level = 22;
        p.act(&json!({ "type": "fight", "fight": f })).unwrap();
        p.owned = vec![store::OwnedItem { item: ens_calc::Equip { item: p.lists.weapons[0].guid.clone(), rarity: Some("Epic".into()), level: Some(12), perks: Some(1) }, count: 1, equipped: false }];
        p.owned_from = Some(store::OwnedSource { character: "Roth".into(), path: "x".into(), loaded: 5 });
        p.autosave().unwrap();
        let id = p.build_id.clone();

        let mut q = Planner::new();
        q.attach_store(dir.clone()).unwrap();
        assert_eq!(q.build_id, id, "the newest build opens");
        assert_eq!((q.build_name.as_str(), q.build.level, q.notes.as_str(), q.fight.enemy_level), ("Frost archer", 17, "farm Talion's Bow", 22));
        assert_eq!((q.loadouts.len(), q.current), (p.loadouts.len(), p.current));
        assert_eq!(q.owned, p.owned);
        assert_eq!(q.builds()["builds"][0]["character"], json!("Roth"));

        // a new build, a copy, and deleting the open build
        q.new_build().unwrap();
        assert_eq!(store::list(&dir).len(), 2);
        assert_ne!(q.build_id, id);
        q.duplicate_build(&id).unwrap();
        assert_eq!(q.build_name, "Frost archer (copy)");
        assert_eq!(store::list(&dir).len(), 3);
        let copy = q.build_id.clone();
        q.delete_build(&copy).unwrap();
        assert_eq!(store::list(&dir).len(), 2);
        assert_ne!(q.build_id, copy, "deleting the open build opens another");
        assert!(q.open_build("../escape").is_err(), "ids are letters and digits only");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod owned_gear {
    use super::*;
    use store::OwnedItem;

    fn own(g: &str, level: u32) -> OwnedItem {
        OwnedItem { item: ens_calc::Equip { item: g.to_string(), rarity: Some("Rare".into()), level: Some(level), perks: Some(1) }, count: 1, equipped: false }
    }

    /// Best of what I own, scored by a stand-in rating so the expected picks are exact.
    #[test]
    fn best_owned_picks_per_slot_without_reusing_a_ring() {
        let p = Planner::new();
        let b = p.b;
        let find = |slot: &str| p.lists.gear.iter().flatten().filter(|x| b.items[&x.guid].slot.as_deref() == Some(slot)).map(|x| x.guid.clone()).collect::<Vec<_>>();
        let (rings, heads) = (find("Armour_Ring"), find("Armour_Head"));
        let bows: Vec<String> = p.lists.weapons.iter().filter(|w| b.items[&w.guid].ammo.as_deref() == Some("Arrow")).map(|w| w.guid.clone()).take(2).collect();
        let arrows: Vec<String> = p.lists.ammo.iter().filter(|a| b.items[&a.guid].slot.as_deref() == Some("Arrow")).map(|a| a.guid.clone()).take(2).collect();
        // rings 0, 1, 2 with ratings 1, 5, 3; one head (rating 2); bows rated 10 / 20; arrows 1 / 4
        let owned = vec![own(&rings[0], 10), own(&rings[1], 11), own(&rings[2], 12), own(&heads[0], 9), own(&bows[0], 15), own(&bows[1], 16), own(&arrows[0], 1), own(&arrows[1], 1)];
        let rate = |g: &str| -> f64 {
            [(&rings[0], 1.0), (&rings[1], 5.0), (&rings[2], 3.0), (&heads[0], 2.0), (&bows[0], 10.0), (&bows[1], 20.0), (&arrows[0], 1.0), (&arrows[1], 4.0)]
                .iter().find(|(x, _)| x.as_str() == g).map(|(_, r)| *r).unwrap_or(0.0)
        };
        let score = |t: &Build| {
            let gear: f64 = t.gear.iter().flatten().map(|e| rate(&e.item)).sum();
            let w = t.weapon.as_ref().map(|e| rate(&e.item)).unwrap_or(0.0) * t.ammo.as_deref().map(rate).unwrap_or(0.0);
            (gear + w, 0.0)
        };
        let mut start = p.build.clone();
        start.gear[1] = Some(own(&find("Armour_UpperBody")[0], 5).item); // a chest piece the character does not own
        let nb = game::best_owned(b, &start, &owned, &[], score);
        let ring = |k: usize| nb.gear[k].as_ref().map(|e| e.item.clone());
        assert_eq!((ring(5), ring(6)), (Some(rings[1].clone()), Some(rings[2].clone())), "best ring, then the next best, never the same piece");
        assert_eq!(nb.gear[0].as_ref().map(|e| (e.item.clone(), e.level)), Some((heads[0].clone(), Some(9))), "the owned piece as owned");
        assert!(nb.gear[1].is_none(), "a slot nothing owned fits is emptied");
        assert_eq!((nb.weapon.as_ref().map(|e| e.item.clone()), nb.ammo.clone()), (Some(bows[1].clone()), Some(arrows[1].clone())));
        assert_eq!(nb.carried.iter().map(|c| c.weapon.item.clone()).collect::<Vec<_>>(), vec![bows[0].clone()]);
    }

    /// The owned filter lists only owned items, with their own level, and equipping one keeps that level.
    #[test]
    fn equip_an_owned_item_keeps_its_fields() {
        let mut p = Planner::new();
        let ring = p.lists.gear[5][0].guid.clone();
        p.owned = vec![own(&ring, 17)];
        let fits = game::owned_for_slot(p.b, &p.build, &p.owned, Slot::Armor(5), &[]);
        assert_eq!(fits, vec![0]);
        assert!(game::owned_for_slot(p.b, &p.build, &p.owned, Slot::Armor(0), &[]).is_empty());
        p.act(&json!({ "type": "equip", "slot": "armor5", "owned": 0 })).unwrap();
        assert_eq!(p.build.gear[5].as_ref().map(|e| e.level), Some(Some(17)));
        assert!(p.act(&json!({ "type": "equip", "slot": "armor5", "owned": 3 })).is_err());
        p.owned.clear();
        assert!(p.act(&json!({ "type": "best_owned" })).is_err(), "nothing owned yet");
    }
}

#[cfg(test)]
mod loadouts {
    use super::*;

    /// A new build is one level-1 loadout; loadouts can be deleted down to one, and the planner lands on a neighbour.
    #[test]
    fn new_build_is_one_level_one_loadout_and_loadouts_delete() {
        let mut p = Planner::new();
        p.new_build().unwrap();
        assert_eq!((p.loadouts.len(), p.current, p.loadouts[0].name.as_str()), (1, 0, "Main"));
        // other tests run with ENS_SAMPLE set, which starts from the sample build instead
        assert!(p.build.level == 1 || std::env::var("ENS_SAMPLE").is_ok(), "level {}", p.build.level);

        p.act(&json!({ "type": "new_loadout" })).unwrap();
        p.act(&json!({ "type": "level", "value": 12 })).unwrap();
        p.act(&json!({ "type": "new_loadout" })).unwrap();
        assert_eq!((p.loadouts.len(), p.current), (3, 2));
        p.act(&json!({ "type": "delete_loadout", "index": 2 })).unwrap();
        assert_eq!((p.loadouts.len(), p.current, p.build.level), (2, 1, 12), "deleting the open last loadout opens the one before");
        p.act(&json!({ "type": "delete_loadout", "index": 0 })).unwrap();
        assert_eq!((p.loadouts.len(), p.current, p.build.level), (1, 0, 12), "the open loadout stays open when an earlier one goes");
        p.act(&json!({ "type": "delete_loadout", "index": 0 })).unwrap();
        assert_eq!(p.loadouts.len(), 1, "the last loadout is never deleted");
    }
}

#[cfg(test)]
mod optimizer_v2 {
    use super::*;

    /// Settings are saved with the build; a two-milestone plan runs from them and becomes loadouts.
    #[test]
    fn milestone_plan_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ens-opt-v2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut p = Planner::new();
        p.attach_store(dir.clone()).unwrap();
        let fireball_family = "Fireball";
        let ms = json!([{ "level": 12, "flame_level": 3 }, { "level": 18, "flame_level": 4 }]);
        let required = p.b.skills.iter().find(|s| s.name == "Marksman").unwrap().id;
        let settings = json!({
            "weapons": [
                { "category": "Bow", "ammo_family": "Wooden Arrow", "share": 3.0 },
                { "category": "Staff", "ammo_family": fireball_family, "share": 1.0 }
            ],
            "required": [required], "gear": "best", "milestones": ms,
        });
        p.act(&json!({ "type": "opt_settings", "settings": settings })).unwrap();
        p.autosave().unwrap();
        let mut q = Planner::new();
        q.attach_store(dir.clone()).unwrap();
        assert_eq!(q.opt_settings, p.opt_settings, "settings come back with the build");

        let plan = optim::plan_from(&p);
        assert_eq!((plan.weapons.len(), plan.required.len()), (2, 1));
        let opts = ens_opt::MilestoneOptions { gear: true, respec: false, iters: 1 };
        p.ms_result = Some(p.opt.milestones(&plan, &p.opt_settings.milestones, &opts, &|_, _| {}));
        let v = optim::milestones_view(&p);
        let rows = v["milestones"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0]["level"].as_u64(), rows[1]["flame"].as_u64()), (Some(12), Some(4)));
        assert!(rows[0]["gear"].as_array().unwrap().len() >= 9, "two weapons and seven armour rows");
        assert!(rows[1]["dps"].as_f64().unwrap() > 0.0);
        let marks = p.b.skills.iter().position(|s| s.id == required).unwrap();
        assert!(p.ms_result.as_ref().unwrap().iter().all(|m| m.ranks[marks] > 0), "the must-have skill is in every milestone");

        let before = p.loadouts.len();
        p.act(&json!({ "type": "apply_milestone" })).unwrap();
        assert_eq!(p.loadouts.len(), before + 2);
        let l18 = p.loadouts.iter().find(|l| l.name == "Level 18").unwrap();
        assert_eq!((l18.level, l18.flame_level), (18, 4));
        assert!(l18.order_matches() && !l18.order.is_empty(), "the loadout carries the order from an empty tree to level 18");
        assert!(steps_connected(p.opt, &l18.order));
        assert_eq!(p.opt.category_of(&l18.weapon.as_ref().unwrap().item), Some("Bow"), "the main weapon (larger share) in hand");
        assert_eq!(l18.carried.len(), 1, "the staff is carried");
        assert!(l18.carried[0].ammo.as_ref().is_some_and(|a| p.b.items[a].name.starts_with("Fireball")));
        // applying one milestone again replaces its loadout and opens it
        p.act(&json!({ "type": "apply_milestone", "index": 0 })).unwrap();
        assert_eq!(p.loadouts.len(), before + 2);
        assert_eq!(p.build.name, "Level 12");
        std::fs::remove_dir_all(&dir).ok();
    }

    use ens_opt::SummonChoice;

    /// "Best summons" in the settings: a staff build's milestone picks summons and the loadout made from it has them;
    /// a chosen family comes back at the level's tier; "keep" leaves the loadout's summons alone.
    #[test]
    fn summons_are_optimized_and_applied() {
        let mut p = Planner::new();
        let settings = json!({
            "weapons": [{ "category": "Staff", "share": 1.0 }], "gear": "keep",
            "milestones": [{ "level": 30, "flame_level": 5 }], "summons": { "mode": "best" },
        });
        p.act(&json!({ "type": "opt_settings", "settings": settings })).unwrap();
        assert_eq!(p.opt_settings.summons, SummonChoice::Best);
        let plan = optim::plan_from(&p);
        let opts = ens_opt::MilestoneOptions { gear: false, respec: false, iters: 1 };
        let res = p.opt.milestones(&plan, &p.opt_settings.milestones, &opts, &|_, _| {});
        assert!(!res[0].plan.summons.is_empty(), "a level 30 staff build takes a summon");
        let nb = optim::build_from(&p.build, &res[0].plan, &res[0].ranks, &res[0].order, "Level 30".into());
        assert_eq!(nb.summons, res[0].plan.summons, "the loadout gets the chosen summons");

        let fixed = Plan { level: 30, summon_choice: SummonChoice::Fixed(vec!["Ice Totem".into()]), ..plan.clone() };
        let pl = p.opt.prepare(&fixed, &vec![0; p.b.skills.len()]);
        assert_eq!(pl.summons.iter().map(|g| p.b.items[g].name.as_str()).collect::<Vec<_>>(), ["Ice Totem I"], "level 30: tier I");

        let mut base = p.build.clone();
        base.summons = vec![pl.summons[0].clone()];
        let keep = Plan { summon_choice: SummonChoice::Keep, summons: vec![], ..plan };
        assert_eq!(optim::build_from(&base, &keep, &[], &[], "x".into()).summons, base.summons, "keep leaves them alone");
    }
}

#[cfg(test)]
mod ui_fixtures {
    use super::*;

    /// Writes the backend's answers for the page's optimizer flow to tmp/uitest/fixtures.json, so the real page can
    /// be loaded in a browser with a stand-in backend (run with --ignored).
    #[test]
    #[ignore = "writes tmp/uitest/fixtures.json for the page test harness"]
    fn dump_page_fixtures() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let mut p = Planner::new();
        p.opt_settings.weapons = vec![
            WeaponPlan { category: Some("Bow".into()), ammo_family: Some("Explosive Arrow".into()), share: 3.0, ..Default::default() },
            WeaponPlan { category: Some("Two-Handed Axe".into()), share: 1.0, ..Default::default() },
        ];
        p.opt_settings.milestones = vec![ens_opt::Milestone { level: 15, flame_level: 3 }, ens_opt::Milestone { level: 20, flame_level: 4 }];
        let plan = optim::plan_from(&p);
        let opts = ens_opt::MilestoneOptions { gear: true, respec: false, iters: 1 };
        p.ms_result = Some(p.opt.milestones(&plan, &p.opt_settings.milestones, &opts, &|_, _| {}));
        let pl = p.opt.prepare(&plan, &vec![0; p.b.skills.len()]);
        let (best, order) = p.opt.optimize(&pl, 1, 7);
        let c = p.conditions();
        let mut nb = optim::build_from(&p.build, &pl, &best.ranks, &[], p.build.name.clone());
        nb.gear = p.build.gear.clone();
        let summary = summarize(&p.engine, &nb, &c);
        p.opt_result = Some(OptResult { ranks: best.ranks, order, goals: best.goals, gear: None, summary, plan: Some(pl) });
        let opt_catalog = json!({
            "categories": p.opt.weapon_categories(), "arrows": p.opt.ammo_families("Arrow"), "spells": p.opt.ammo_families("Spell"), "summons": p.opt.summon_families(),
            "milestones": ens_opt::DEFAULT_MILESTONES, "settings": p.opt_settings,
        });
        let tree = {
            let mut tp = optim::plan_from(&p);
            (tp.weapons, tp.required, tp.avoided) = (vec![], vec![], vec![]);
            tree_order_view(p.opt, &tp, &p.build.ranks, None, None)
        };
        let out = json!({
            "catalog": views::catalog(&p),
            "state": views::state(&mut p),
            "summary": views::summary(&mut p, None),
            "overview": views::overview(&mut p),
            "opt_catalog": opt_catalog,
            "milestones": optim::milestones_view(&p),
            "optimize": views::opt_result(&mut p),
            "builds": p.builds(),
            "tree_order": tree,
            "calcs": views::calcs(&mut p),
            "sheet": views::sheet(&p, "", false),
        });
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tmp/uitest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fixtures.json"), serde_json::to_string(&out).unwrap()).unwrap();
    }
}

#[cfg(test)]
mod tree_steps {
    use super::*;

    /// The order covers every rank of the tree, each step stays connected, and levels never go down.
    #[test]
    fn tree_order_covers_the_tree() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let mut p = Planner::new();
        let mut plan = optim::plan_from(&p);
        (plan.weapons, plan.required, plan.avoided) = (vec![], vec![], vec![]);
        plan.level = 20;
        p.build.ranks = p.opt.optimize(&plan, 1, 3).0.ranks;
        let v = tree_order_view(p.opt, &plan, &p.build.ranks, None, None);
        let order: Vec<usize> = serde_json::from_value(v["order"].clone()).unwrap();
        assert_eq!(v["unordered"], json!(0), "a connected tree is fully ordered");
        let mut r = vec![0u8; p.build.ranks.len()];
        for &i in &order {
            r[i] += 1;
            assert!(p.opt.connected(&r), "step {} leaves the tree connected", p.b.skills[i].name);
        }
        assert_eq!(r, p.build.ranks);
        let levels: Vec<Option<u32>> = serde_json::from_value(v["levels"].clone()).unwrap();
        assert!(levels.windows(2).all(|w| w[0].unwrap_or(99) <= w[1].unwrap_or(99)));
    }
}

#[cfg(test)]
mod tree_steps_unconnected {
    use super::*;

    /// A hand-made tree that is not connected still lists every rank; the unreachable ones are counted.
    #[test]
    fn unconnected_tree_lists_every_rank() {
        // SAFETY: set before any other thread reads the environment in this test
        unsafe { std::env::set_var("ENS_SAMPLE", "1") };
        let p = Planner::new();
        let mut plan = optim::plan_from(&p);
        (plan.weapons, plan.required, plan.avoided) = (vec![], vec![], vec![]);
        let v = tree_order_view(p.opt, &plan, &p.build.ranks, None, None);
        let order: Vec<usize> = serde_json::from_value(v["order"].clone()).unwrap();
        assert_eq!(order.len(), p.build.ranks.iter().map(|&r| r as usize).sum::<usize>());
        assert!(v["unordered"].as_u64().unwrap() > 0 || p.opt.connected(&p.build.ranks));
    }
}

#[cfg(test)]
mod stored_order {
    use super::*;

    /// Clicks record the path in the order bought; removing takes that skill's last entry; reset clears it; the
    /// optimizer's result and its steps carry the optimizer's order; the stepper uses a stored order as it is.
    #[test]
    fn the_tree_remembers_its_order() {
        let mut p = Planner::new();
        p.act(&json!({ "type": "reset_tree" })).unwrap();
        assert!(p.build.order.is_empty());
        let skill = |n: &str| p.b.skills.iter().position(|s| s.name == n).unwrap();
        let (a, b) = (skill("Marksman"), skill("Spellslinger"));
        let pa = path_to(p.opt, &p.build.ranks, a).unwrap();
        p.act(&json!({ "type": "allocate", "node": a })).unwrap();
        let pb = path_to(p.opt, &p.build.ranks, b).unwrap();
        p.act(&json!({ "type": "allocate", "node": b })).unwrap();
        p.act(&json!({ "type": "allocate", "node": a })).unwrap();
        let mut want = pa.clone();
        want.extend(&pb);
        want.push(a);
        assert_eq!(p.build.order, want, "the path of each click, in click order");
        assert!(p.build.order_matches());

        // the stepper shows exactly that order
        let plan = optim::plan_from(&p);
        let v = tree_order_view(p.opt, &plan, &p.build.ranks, None, Some(p.build.order.clone()));
        assert_eq!(serde_json::from_value::<Vec<usize>>(v["order"].clone()).unwrap(), want);
        assert!(steps_connected(p.opt, &p.build.order));

        // removing a rank takes off that skill's last entry
        p.act(&json!({ "type": "remove", "node": a })).unwrap();
        assert_eq!(p.build.order, want[..want.len() - 1].to_vec());
        // stepping back and setting the tree keeps the prefix
        let mut r = vec![0u8; p.build.ranks.len()];
        for &i in &want[..pa.len()] {
            r[i] += 1;
        }
        p.act(&json!({ "type": "set_ranks", "ranks": r })).unwrap();
        assert_eq!(p.build.order, pa);
        p.act(&json!({ "type": "undo" })).unwrap();
        assert_eq!(p.build.order, want[..want.len() - 1].to_vec(), "undo brings the order back with the tree");

        // the optimizer's result carries its leveling order, must-have nodes included
        p.opt_settings.required = vec![p.b.skills[b].id];
        let mut plan = optim::plan_from(&p);
        plan.level = 15;
        let (best, order) = p.opt.optimize(&plan, 1, 3);
        let mut check = p.build.clone();
        (check.ranks, check.order) = (best.ranks.clone(), order.clone());
        assert!(check.order_matches(), "the leveling order lists every rank, the must-have path too");
        let c = p.conditions();
        let summary = summarize(&p.engine, &check, &c);
        p.opt_result = Some(OptResult { ranks: best.ranks.clone(), order: order.clone(), goals: best.goals, gear: None, summary, plan: None });
        p.act(&json!({ "type": "apply_opt" })).unwrap();
        assert_eq!(p.build.order, order);
        p.act(&json!({ "type": "opt_step", "step": 5 })).unwrap();
        assert_eq!(p.build.order, order[..5].to_vec());
        assert!(p.build.order_matches());
    }
}

/// Regression tests for findings from the review loop (review/ledger.yaml).
#[cfg(test)]
mod review {
    use super::*;

    fn guid(b: &Bundle, name: &str) -> String {
        let mut g: Vec<&String> = b.items.iter().filter(|(_, i)| i.name == name).map(|(g, _)| g).collect();
        // the data keeps one copy per drop level: take the highest
        g.sort_by_key(|g| (std::cmp::Reverse(b.items[*g].levels.as_ref().map(|l| l.min)), (*g).clone()));
        g[0].clone()
    }

    /// R1-DR-09: with no weapon yet, every weapon in the list is ranked, so the last ones alphabetically can be found.
    #[test]
    fn weapon_candidates_cover_every_weapon_on_first_pick() {
        let mut p = Planner::new();
        p.build = Build::new(p.b, 50);
        let guids = views::candidate_guids(&p, Slot::Weapon, false);
        let want: std::collections::BTreeSet<String> = guids.iter().map(|g| p.b.items[g].name.clone()).collect();
        assert!(want.contains("Spiked Greatsword") && want.contains("Summit Greatsword"), "the list itself should hold them");
        let t = candidate_table(p.opt, p.b, &p.build, &p.conditions(), Slot::Weapon, guids, &[]);
        let got: std::collections::BTreeSet<String> = t["rows"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(want.difference(&got).cloned().collect::<Vec<_>>(), Vec::<String>::new());
    }

    /// R1-PS-02: typed values stop at the 1.0 caps: Flame 11, item level 55, character level 50.
    #[test]
    fn act_clamps_flame_to_11_and_item_level_to_55() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        p.act(&json!({"type": "flame", "value": 20})).unwrap();
        assert_eq!(p.build.flame_level, 11);
        p.act(&json!({"type": "item", "slot": "weapon", "level": 105})).unwrap();
        assert_eq!(p.build.weapon.as_ref().unwrap().level, Some(55));
        p.act(&json!({"type": "level", "value": 100})).unwrap();
        assert_eq!(p.build.level, 50);
        let mut over = p.build.clone();
        over.level = 100;
        p.act(&json!({"type": "load_code", "code": over.to_code()})).unwrap();
        let o = views::overview(&mut p);
        assert!(o["insights"].to_string().contains("cannot exist"), "a level-100 code loads without a warning");
    }

    /// R1-SB-04: a code with too few gear slots is refused and the planner keeps working.
    #[test]
    fn malformed_gear_array_in_code_is_rejected_by_the_app() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let before = p.build.clone();
        let mut bad = before.clone();
        bad.gear.clear();
        assert!(p.act(&json!({"type": "load_code", "code": bad.to_code()})).is_err());
        assert_eq!(p.build, before);
        let _ = views::state(&mut p);
        let _ = views::overview(&mut p);
    }

    /// R1-SB-06: the key the heatmap and candidate tables refresh on changes with level, Flame, item level, rarity
    /// and food, and the page keys on it.
    #[test]
    fn heatmap_refreshes_on_level_flame_and_item_changes() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let key = |p: &mut Planner| views::state(p)["calcKey"].as_str().unwrap().to_string();
        let mut last = key(&mut p);
        let soup = p.lists.foods[0].guid.clone();
        for a in [json!({"type": "flame", "value": 2}), json!({"type": "level", "value": 20}), json!({"type": "item", "slot": "weapon", "level": 20}),
                  json!({"type": "item", "slot": "weapon", "rarity": "Common"}), json!({"type": "equip", "slot": "food", "guid": soup})] {
            p.act(&a).unwrap();
            let k = key(&mut p);
            assert_ne!(k, last, "{a} left the key unchanged");
            last = k;
        }
        let js = include_str!("../ui/app.js");
        let heat = &js[js.find("async function loadHeat").unwrap()..];
        assert!(heat[..heat.find("
}").unwrap()].contains("ST.calcKey"), "loadHeat must key on ST.calcKey");
    }

    /// R1-DR-05: the skill-point budget says when it is an estimate, and the player can enter their real total.
    #[test]
    fn skill_point_budget_is_overridable_and_marked_estimate() {
        let mut p = Planner::new();
        p.build = Build::new(p.b, 30);
        assert_eq!(views::state(&mut p)["foundPoints"], Value::Null);
        p.act(&json!({"type": "found_points", "value": 0})).unwrap();
        let st = views::state(&mut p);
        assert_eq!((st["foundPoints"].as_u64(), st["have"].as_u64()), (Some(0), Some(58)), "level 30 with nothing found has 58 points");
        p.act(&json!({"type": "found_points", "value": 500})).unwrap();
        assert_eq!(p.build.found_points, Some(114));
        p.act(&json!({"type": "found_points", "value": null})).unwrap();
        assert_eq!(p.build.found_points, None);
        let js = include_str!("../ui/app.js");
        assert!(js.contains("(estimate)") && js.contains(r#"id="found""#));
    }

    /// R1-TC-06: candidates within 1% of each other are grouped and marked as ties.
    #[test]
    fn candidates_group_results_within_error_band() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let guids = views::candidate_guids(&p, Slot::Ammo, false);
        let t = candidate_table(p.opt, p.b, &p.build, &p.conditions(), Slot::Ammo, guids, &[]);
        let rows = t["rows"].as_array().unwrap();
        let mut any = false;
        for w in rows.windows(2) {
            let (a, b) = (w[0]["dps"].as_f64().unwrap(), w[1]["dps"].as_f64().unwrap());
            if a > 0.0 && (a - b) / a < 0.005 {
                assert_eq!(w[0]["group"], w[1]["group"], "{} and {} are {a} and {b}", w[0]["name"], w[1]["name"]);
                assert!(w[0]["tied"].as_bool().unwrap() && w[1]["tied"].as_bool().unwrap());
                any = true;
            }
            if a > 0.0 && (a - b) / a > 0.02 {
                assert_ne!(w[0]["group"], w[1]["group"]);
            }
        }
        assert!(any, "the sample bow's arrows include near ties");
    }

    /// The planner is for players: no page, Calculations section, enemy or footer shows measurement status,
    /// game-code provenance, exe addresses, analysis files or the game's placeholder enemies (docs/plans/public-cleanup.md).
    #[test]
    fn public_text_has_no_internal_details() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let text = format!("{}{}{}{}{}", views::calcs(&mut p), views::catalog(&p), views::overview(&mut p),
            include_str!("../ui/app.js"), include_str!("../ui/meter.js"));
        for bad in ["measured in game", "not measured", "How sure", "golden", "exe code", "0x140", "analysis/", "PLACEMENT HELPER", "DONOTUSE", "dmgMod", "from #"] {
            assert!(!text.contains(bad), "players see {bad:?}");
        }
        assert!(!views::calcs(&mut p)["sections"].as_array().unwrap().iter().any(|s| s.get("proof").is_some()));
    }

    /// R1-SB-07: a combat setting that does not parse is refused with a message and the fight stays as it was.
    #[test]
    fn invalid_fight_values_report_an_error() {
        let mut p = Planner::new();
        let before = p.fight.clone();
        for bad in [json!(-3), json!(1.5), json!(5000000000u64), json!(0), json!(900)] {
            let mut f = serde_json::to_value(&p.fight).unwrap();
            f["enemy_level"] = bad.clone();
            let r = p.act(&json!({"type": "fight", "fight": f}));
            assert!(r.is_err(), "enemy level {bad} accepted");
            assert!(p.fight == before);
        }
    }

    /// R1-DR-06: the enemy is at the character's level until the player sets its level.
    #[test]
    fn default_enemy_level_tracks_character_level() {
        let mut p = Planner::new();
        p.act(&json!({"type": "level", "value": 30})).unwrap();
        assert_eq!(p.fight.enemy_level, 30);
        p.act(&json!({"type": "level", "value": 12})).unwrap();
        assert_eq!(p.fight.enemy_level, 12);
        let mut f = serde_json::to_value(&p.fight).unwrap();
        f["enemy_level"] = json!(40);
        p.act(&json!({"type": "fight", "fight": f})).unwrap();
        p.act(&json!({"type": "level", "value": 20})).unwrap();
        assert_eq!(p.fight.enemy_level, 40, "a level the player set stays");
    }

    /// R1-DR-07: the node panel's coverage line comes from the coverage audit in player words: a skill the numbers
    /// count is never called "not in the numbers", and no raw codes ("flag", "triggered") are shown.
    #[test]
    fn node_coverage_note_matches_engine_effect() {
        let p = Planner::new();
        let cat = views::catalog(&p);
        let skill = |n: &str| cat["skills"].as_array().unwrap().iter().find(|s| s["name"] == n).unwrap().clone();
        for n in ["Relentless", "Opportunity", "Two-Handed Specialization"] {
            assert_eq!(skill(n)["coverage"]["text"], "Counted in the numbers", "{n}");
        }
        assert!(skill("Breach")["coverage"]["text"].as_str().unwrap().starts_with("Not in the numbers"));
        let js = include_str!("../ui/app.js");
        assert!(!js.contains("Handled by game systems") && js.contains("s.coverage.text"));
        assert!(cat["skills"].as_array().unwrap().iter().all(|s| s.get("unparsed").is_none()));
    }

    /// R1-DR-08: every rail number explains itself, the bonus pool says it adds before it multiplies, and Merciless
    /// uses the game's word (overpowered).
    #[test]
    fn rail_and_chain_terms_have_tooltips() {
        let js = include_str!("../ui/app.js");
        for k in ["Total DPS", "Expected hit", "Time to kill", "Crit chance", "Hits survived", "Mana uptime", "Health"] {
            assert!(js.contains(&format!("\"{k}\": \"")), "no tip for {k}");
        }
        assert!(!js.contains("on staggered enemies") && js.contains("overpowered enemy"));
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let o = views::overview(&mut p);
        let pool = o["chain"].as_array().unwrap().iter().find(|c| c["label"] == "Damage bonuses").unwrap();
        assert!(pool["tip"].as_str().unwrap().contains("add together"));
    }

    /// R1-DR-10: with a weapon in hand the list keeps to its type and says so; "All weapon types" lists every type.
    #[test]
    fn weapon_candidates_offer_other_types_with_notice() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let kinds = |gs: &[String]| gs.iter().filter_map(|g| p.weapon_kind(g)).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(kinds(&views::candidate_guids(&p, Slot::Weapon, false)).len(), 1);
        assert!(kinds(&views::candidate_guids(&p, Slot::Weapon, true)).len() > 4);
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"id="alltypes""#) && js.contains("Showing weapons of the type you hold"));
    }

    /// R2-PS-01: skills that move no number are labelled "Not in the numbers", never as counted or as an action the
    /// numbers price.
    #[test]
    fn unparsed_flag_skills_move_numbers_or_are_labelled_not_counted() {
        let p = Planner::new();
        let cat = views::catalog(&p);
        let text = |n: &str| cat["skills"].as_array().unwrap().iter().find(|s| s["name"] == n).unwrap()["coverage"]["text"].as_str().unwrap().to_string();
        for n in ["Heavy Hitter", "Venom Mastery", "Snap"] {
            assert!(text(n).starts_with("Not in the numbers"), "{n}: {}", text(n));
        }
        // its extra wand projectiles are priced now (R2-PS-01 reopened)
        assert_eq!(text("Arcane Proliferation"), "Counted in the numbers");
    }

    /// R1-SB-03: every combat number is held to its field's range, and an empty level field is refused.
    #[test]
    fn backend_clamps_every_numeric_input_to_game_caps() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        p.sync_enemy_level();
        assert!(p.fight.check().is_ok(), "the defaults are in range");
        let before = p.fight.clone();
        for (k, v) in [("pack_size", json!(1000)), ("pack_size", json!(4e9)), ("health_mult", json!(0)), ("health_mult", json!(-3)), ("pack_radius", json!(-1))] {
            let mut f = serde_json::to_value(&p.fight).unwrap();
            f[k] = v.clone();
            assert!(p.act(&json!({"type": "fight", "fight": f})).is_err(), "{k} = {v} accepted");
            assert!(p.fight == before);
        }
        let level = p.build.level;
        assert!(p.act(&json!({"type": "level", "value": 0})).is_err());
        assert!(p.act(&json!({"type": "level", "value": null})).is_err());
        assert_eq!(p.build.level, level);
    }

    /// R2-TC-03: a candidate within 1% of the row above it is always in that row's group.
    #[test]
    fn candidates_within_sidegrade_band_of_a_neighbour_share_its_group() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        for slot in [Slot::Weapon, Slot::Ammo] {
            let t = candidate_table(p.opt, p.b, &p.build, &p.conditions(), slot, views::candidate_guids(&p, slot, false), &[]);
            for w in t["rows"].as_array().unwrap().windows(2) {
                let (a, b) = (w[0]["dps"].as_f64().unwrap(), w[1]["dps"].as_f64().unwrap());
                if b >= a * (1.0 - SIDEGRADE_BAND) {
                    assert_eq!(w[0]["group"], w[1]["group"], "{} {a} and {} {b}", w[0]["name"], w[1]["name"]);
                }
            }
        }
    }

    /// R2-PS-04: the summon pace row describes the engine's measured 2.6 s default and says where it was measured.
    #[test]
    fn summon_rate_proof_row_matches_engine_default_interval() {
        let p = ens_calc::proof::get("summon.rate").unwrap();
        assert!(p.step.contains(&format!("{} s", ens_calc::SUMMON_ATTACK_S)), "{}", p.step);
        assert!(ens_calc::proof::measured("summon.rate").is_some_and(|m| m.contains("1273043")));
    }

    /// R2-FM-06: a saved build records its tree; reopened after two nodes swap places, its ranks follow the skills.
    #[test]
    fn saved_build_ranks_survive_node_reorder_by_skill_id() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let want = p.build.ranks.clone();
        let i = want.iter().position(|r| *r > 0).unwrap();
        let j = want.iter().position(|r| *r == 0).unwrap();
        let mut s = p.snapshot();
        assert_eq!(s.data.as_deref(), Some(ens_calc::build::data_tag(p.b).as_str()));
        // as if saved against a tree where nodes i and j had swapped places
        s.nodes.swap(i, j);
        for l in s.loadouts.iter_mut() {
            l.ranks.swap(i, j);
        }
        p.restore(s.clone()).unwrap();
        assert_eq!(p.build.ranks, want, "ranks follow the skill ids");
        assert!(p.notice.is_none(), "same game data: no notice");
        s.data = Some("1-00000000-00000000".into());
        p.restore(s).unwrap();
        assert!(p.notice.as_deref().is_some_and(|n| n.contains("other game data")));
    }

    /// R2-FM-09: the spell list holds only spells with damage data.
    #[test]
    fn spell_picker_lists_only_player_spells() {
        let p = Planner::new();
        let names: Vec<String> = p.lists.ammo.iter().map(|x| p.b.items[&x.guid].name.clone()).collect();
        assert!(!names.iter().any(|n| n == "Trap Fireball" || n == "Fireworks"), "non-player spells listed");
        assert!(p.lists.ammo.iter().all(|x| p.b.items[&x.guid].slot.as_deref() != Some("Spell") || p.b.items[&x.guid].damage.is_some()));
    }

    /// R2-FM-08: every level field in the page takes its limit from the caps the backend sends, never a typed number.
    #[test]
    fn ui_caps_match_bundle_caps() {
        let p = Planner::new();
        let caps = &views::catalog(&p)["caps"];
        assert_eq!((caps["level"].as_u64(), caps["item"].as_u64(), caps["flame"].as_u64()), (Some(50), Some(55), Some(11)));
        let js = include_str!("../ui/app.js");
        for typed in [r#"max="50""#, r#"max="55""#, r#"max="11""#, r#"max="20""#, "? 105 :", "Math.min(50", ": 20, Math.round"] {
            assert!(!js.contains(typed), "app.js still types a cap: {typed}");
        }
    }

    /// R1-DR-12: a fresh build offers presets; one click equips a weapon (and its ammo) that gives a DPS.
    #[test]
    fn fresh_launch_offers_archetype_presets() {
        let mut p = Planner::new();
        p.build = Build::new(p.b, 20);
        let o = views::overview(&mut p);
        let presets = o["presets"].as_array().expect("presets while no weapon is equipped");
        assert!(presets.len() >= 5);
        for x in presets {
            let mut q = Planner::new();
            q.build = Build::new(q.b, 20);
            q.act(&json!({"type": "preset", "kind": x["kind"]})).unwrap();
            let w = q.build.weapon.clone().unwrap();
            assert!(q.b.items[&w.item].levels.as_ref().unwrap().min <= 20, "{}", x["label"]);
            assert!(q.summary().dps > 0.0, "{} gives no DPS", x["label"]);
            assert!(q.build.problems(q.b).is_empty(), "{}: {:?}", x["label"], q.build.problems(q.b));
            // a whole starting build: tree, armour and food, not only a weapon
            assert!(q.build.points(q.b) > 0 && q.build.gear.iter().flatten().count() >= 5 && q.build.foods.len() == 3, "{}: tree {} gear {} foods {}",
                    x["label"], q.build.points(q.b), q.build.gear.iter().flatten().count(), q.build.foods.len());
            assert!(q.build.points(q.b) <= ens_opt::points_available(q.build.level, q.build.found_points), "{} spends more points than it has", x["label"]);
        }
        assert!(include_str!("../ui/app.js").contains("data-do=\"preset:"));
    }

    /// R2-TC-03 (reopened): the page marks a row only against the row above it, and that mark means within 1%; it
    /// never claims a whole chained group is equal.
    #[test]
    fn sidegrade_mark_is_pairwise_with_the_row_above() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let t = candidate_table(p.opt, p.b, &p.build, &p.conditions(), Slot::Weapon, views::candidate_guids(&p, Slot::Weapon, true), &[]);
        let rows = t["rows"].as_array().unwrap();
        assert!(rows[0]["tieAbove"] == json!(false));
        for w in rows.windows(2) {
            let (a, b) = (w[0]["dps"].as_f64().unwrap(), w[1]["dps"].as_f64().unwrap());
            assert_eq!(w[1]["tieAbove"].as_bool().unwrap(), b >= a * (1.0 - SIDEGRADE_BAND) - 1e-9, "{} {a} / {} {b}", w[0]["name"], w[1]["name"]);
        }
        let js = include_str!("../ui/app.js");
        assert!(!js.contains("within 1% of each other") && !js.contains("groupTop") && js.contains("r.tieAbove"));
    }

    /// R3-AA-01: after a refused change the page takes the backend's state again, so a refused value never stays in its
    /// copy of the settings (and a failed preset is not reported as ready).
    #[test]
    fn refused_setting_restores_the_backend_state() {
        let js = include_str!("../ui/app.js");
        let act = &js[js.find("async function act(a)").unwrap()..];
        let act = &act[..act.find("
}
").unwrap()];
        let catch = &act[act.find("catch (e)").unwrap()..];
        assert!(catch.contains("ST = await invoke(\"state\")"), "act's error path keeps the page's copy");
        let preset = &js[js.find("async function startPreset").unwrap()..];
        assert!(preset[..preset.find("
}
").unwrap()].contains("if (ok) flash"));
        // and the backend keeps its own value
        let mut p = Planner::new();
        let before = p.fight.clone();
        let mut f = serde_json::to_value(&p.fight).unwrap();
        f["zone_stay"] = json!(0.02);
        assert!(p.act(&json!({"type": "fight", "fight": f})).is_err());
        assert!(views::state(&mut p)["fight"] == serde_json::to_value(&before).unwrap());
    }

    /// R3-AA-02: messages go to an on-screen live region (errors stay until dismissed), not a clipped footer, and the
    /// footer no longer claims every number is proven.
    #[test]
    fn status_messages_are_a_visible_live_region() {
        let html = include_str!("../ui/index.html");
        let toast = &html[html.find("id=\"status\"").unwrap() - 5..];
        assert!(toast[..200].contains("aria-live") && toast[..200].contains("role=\"status\""));
        let footer = &html[html.find("<footer>").unwrap()..html.find("</footer>").unwrap()];
        assert!(!footer.contains("id=\"status\"") && !footer.contains("proven"));
        let css = include_str!("../ui/styles.css");
        assert!(css.contains(".toast { position: fixed"));
        let js = include_str!("../ui/app.js");
        let flash = &js[js.find("function flash(msg, bad)").unwrap()..];
        assert!(flash[..flash.find("
}
").unwrap()].contains("if (!bad) flash.t = setTimeout"), "errors must not vanish on a timer");
    }

    /// R3-AA-03: the top bar wraps instead of widening the page, two-column views stack on narrower windows, and text
    /// size can be raised from the keyboard.
    #[test]
    fn layout_fits_the_window_and_text_can_grow() {
        let css = include_str!("../ui/styles.css");
        let header = &css[css.find("
header {").unwrap()..];
        assert!(header[..header.find('}').unwrap()].contains("flex-wrap: wrap"));
        assert!(css.contains("@media (max-width: 1440px)"));
        let js = include_str!("../ui/app.js");
        assert!(js.contains("function setZoom") && js.contains(r#"["=", "+", "-", "0"].includes(e.key)"#));
    }

    /// R3-AA-04: equipping and buying skills work from the keyboard: candidate rows are focusable buttons that Enter
    /// equips, the tree search selects a skill on Enter, and the node panel has rank buttons.
    #[test]
    fn equip_and_allocate_work_from_the_keyboard() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"tabindex="0" role="button" aria-label="Equip"#));
        assert!(js.contains(r#"(e.key !== "Enter" && e.key !== " ")"#) && js.contains("tr.click()"));
        assert!(js.contains(r##"$("#tsearch").addEventListener("keydown""##));
        assert!(js.contains(r#"data-node-act="allocate""#) && js.contains(r#"data-node-act="remove""#) && js.contains("type: b.dataset.nodeAct"));
    }

    /// R3-AA-05: every setup control is named after its setting, and the milestone fields have names.
    #[test]
    fn setup_inputs_have_accessible_names() {
        let js = include_str!("../ui/app.js");
        let f = &js[js.find("function setting(name").unwrap()..];
        assert!(f[..f.find("
}
").unwrap()].contains("aria-label"));
        assert!(js.contains(r#"aria-label="Milestone character level""#) && js.contains(r#"aria-label="Milestone Flame level""#));
    }

    /// R3-AA-06: the wand projectile factor and the zone hits each show as a row in Calculations.
    #[test]
    fn hidden_multipliers_have_breakdown_rows() {
        let mut p = Planner::new();
        let rows = |p: &mut Planner| views::calcs(p)["sections"].as_array().unwrap().iter().find(|s| s["id"] == "rate").unwrap()["rows"].to_string();
        let mut st = Build::new(p.b, 50);
        st.weapon = Some(ens_calc::Equip { item: guid(p.b, "Clearwater Staff"), rarity: Some("Legendary".into()), level: Some(50), perks: None });
        st.ammo = Some(guid(p.b, "Blizzard I"));
        p.build = st;
        assert!(rows(&mut p).contains("Zone hits on each enemy"));
        let mut wand = Build::new(p.b, 50);
        wand.weapon = Some(ens_calc::Equip { item: guid(p.b, "Electric Initialiser"), rarity: Some("Legendary".into()), level: Some(50), perks: None });
        wand.set_rank_by_name(p.b, "Arcane Proliferation", 1);
        p.build = wand;
        assert!(rows(&mut p).contains("Extra wand projectiles"));
    }

    /// R3-AA-07: a change returns focus to the control that made it, and preset buttons show they are busy.
    #[test]
    fn focus_returns_after_a_change_and_presets_show_busy() {
        let js = include_str!("../ui/app.js");
        let act = &js[js.find("async function act(a)").unwrap()..];
        let act = &act[..act.find("
}
").unwrap()];
        assert!(act.contains("focusKey(document.activeElement)") && act.contains("refocus(key)"));
        assert!(js.contains(r#"b.setAttribute("aria-busy", "true")"#) && js.contains("b.disabled = true"));
    }

    /// R3-AA-08: dimmed setup rows mute their colour instead of fading the text to an unreadable contrast.
    #[test]
    fn dimmed_rows_keep_readable_text() {
        let css = include_str!("../ui/styles.css");
        assert!(!css.contains(".setting.dim { opacity"));
        assert!(css.contains(".setting.dim .nm, .setting.dim .hp { color: var(--text3); }"));
    }

    /// R3-AA-03 (reopened): segmented button groups (hit kind, heatmap mode) wrap instead of running past the edge at
    /// large text sizes.
    #[test]
    fn segmented_buttons_wrap() {
        let css = include_str!("../ui/styles.css");
        let seg = &css[css.find("
.seg {").unwrap()..];
        assert!(seg[..seg.find('}').unwrap()].contains("flex-wrap: wrap"));
    }

    /// R4-DR-01, R4-PS-01: the enemy's health follows the picked template's own health factor (bosses far tougher);
    /// the setting only multiplies it, 1 by default.
    #[test]
    fn enemy_health_uses_the_templates_factor() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        assert_eq!(p.fight.health_mult, 1.0);
        let (k, factor) = p.enemies.iter().enumerate()
            .filter_map(|(k, e)| e.name.as_ref().and_then(|n| p.b.enemy_types.get(n)).and_then(|t| t.health_factor).map(|f| (k, f)))
            .find(|(_, f)| *f >= 10.0).expect("a boss template");
        p.fight.enemy = k;
        let c = p.conditions();
        assert!((c.hp_factor - factor).abs() < 1e-9);
        assert_eq!(c.target.enemy_health, ens_calc::enemy_health(p.b, p.fight.enemy_level, factor));
        p.fight.enemy = 0;
        assert_eq!(p.conditions().hp_factor, 1.0, "the training dummy has no template");
    }

    /// R1-DR-02 (reopened by R4-DR-03): the node panel explains a scenario zero in every view, not only with the
    /// heatmap on, and says when the chosen hit kind cannot be made yet.
    #[test]
    fn scenario_note_shows_without_the_heatmap() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains("${scenarioNote(s)}${heat}"), "the note is outside the heatmap line");
        assert!(!js.contains("path included</div>${scenarioNote(s)}"));
        assert!(js.contains("ST.hitKindProblem"));
        let mut p = Planner::new();
        let mut gs = Build::new(p.b, 50);
        gs.weapon = Some(ens_calc::Equip { item: guid(p.b, "Shroud Vessel's Greatsword"), rarity: None, level: None, perks: None });
        p.build = gs;
        p.fight.hit_kind = "Merciless".into();
        assert!(views::state(&mut p)["hitKindProblem"].as_str().is_some_and(|w| w.contains("Merciless Attack")));
    }

    /// R4-PS-02: a gem that adds nothing says why in Calculations (what it needs, or that it is not modeled), and the
    /// coverage audit lists every gem.
    #[test]
    fn idle_gems_say_what_they_need() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let gem = |n: &str| p.b.items.iter().find(|(g, i)| i.name == n && p.b.gems.contains_key(*g)).map(|(g, _)| g.clone()).unwrap();
        let extra = |p: &mut Planner| views::calcs(p)["sections"].as_array().unwrap().iter().find(|s| s["id"] == "extra").unwrap()["rows"].to_string();
        p.build.gem = Some(ens_calc::Equip { item: gem("Gem of Toxic Betrayal"), rarity: None, level: Some(50), perks: None });
        assert!(extra(&mut p).contains("counts with backstab hits"));
        p.build.gem = Some(ens_calc::Equip { item: gem("Gem of Overpowering"), rarity: None, level: Some(50), perks: None });
        assert!(extra(&mut p).contains("does not model this gem"));
        let cov: Value = serde_json::from_str(include_str!("../../../data/coverage.json")).unwrap();
        assert_eq!(cov["entries"].as_object().unwrap().keys().filter(|k| k.starts_with("gem:")).count(), p.b.gems.len());
    }

    /// R4-PS-04: a saved build without a data stamp opens with a notice; a new build does not get one.
    #[test]
    fn unstamped_saved_builds_open_with_a_notice() {
        let mut p = Planner::new();
        let mut s = p.snapshot();
        s.data = None;
        s.nodes.clear();
        p.restore(s).unwrap();
        assert!(p.notice.as_deref().is_some_and(|n| n.contains("before builds recorded")));
        p.notice = None;
        p.new_build().ok();
        assert!(p.notice.is_none(), "a new build gets no notice");
    }

    /// R4-DR-06: sneak, merciless and backstab bonuses read as player words.
    #[test]
    fn hit_kind_adders_have_labels() {
        assert_eq!(labels::attr_label("DamageModMerciless_Adder"), "Merciless attack damage");
        assert_eq!(labels::attr_label("DamageModSneak_Adder"), "Sneak attack damage");
        assert_eq!(labels::attr_label("DamageModBackstab_Adder"), "Backstab damage");
    }

    /// R4-DR-10: the enemy picker leaves out test, debug, retired and work-in-progress templates; a saved fight finds
    /// its enemy by template name.
    #[test]
    fn enemy_list_has_no_internal_templates() {
        let mut p = Planner::new();
        for e in &p.enemies {
            let l = e.name.clone().unwrap_or_default().to_lowercase();
            assert!(!["deprecat", "test", "debug", "_old", "wip"].iter().any(|w| l.contains(w)), "{l}");
        }
        let k = p.enemies.len() / 2;
        let mut f = serde_json::to_value(&p.fight).unwrap();
        f["enemy"] = json!(k);
        p.act(&json!({"type": "fight", "fight": f})).unwrap();
        let mut s = p.snapshot();
        assert_eq!(s.fight.enemy_key, p.enemies[k].name);
        s.fight.enemy = 0;
        p.restore(s).unwrap();
        assert_eq!(p.fight.enemy, k, "the enemy is found by name");
    }

    /// R4-DR-09: when no save is found, the page asks for the save folder and passes it to Load from game.
    #[test]
    fn load_game_asks_for_the_save_folder() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains("function askSaveDir") && js.contains(r#"invoke("load_game", { dir: savedDir() || null })"#));
        assert!(!js.contains("Set ENS_SAVE_DIR"));
    }

    /// R4-DR-04: choosing sneak or merciless starts at a fifth of the attacking time, and the setting says it is a guess.
    #[test]
    fn special_hit_share_starts_at_a_guess() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"?? (["Sneak", "Merciless"].includes(hk.dataset.hk) ? 0.2 : 1);"#));
        assert!(js.contains("20% is a starting guess, not a game value"));
    }

    /// R4-DR-05: the rail's Fighting card shows light or heavy attacks for a melee weapon, and the special hit share.
    #[test]
    fn fighting_card_shows_attack_style() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"ST.heavy ? "heavy attacks" : "light combo""#));
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        assert_eq!(views::state(&mut p)["weaponKind"], "RangedBow");
    }

    /// R4-DR-07: the terms a new player meets are spelled out (attack multiplier, weapon base damage, the bonus pool,
    /// measured damage, optimizer goals).
    #[test]
    fn jargon_is_spelled_out() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let c = views::calcs(&mut p).to_string();
        assert!(!c.contains("mean AttackMod") && c.contains("Weapon base damage at item level") && c.contains("adds to the damage bonus pool"));
        assert!(!views::catalog(&p)["game"].as_str().unwrap().contains("golden vectors"));
        let js = include_str!("../ui/app.js");
        assert!(js.contains("const GOAL_TIPS") && js.contains("GOAL_TIPS[g]"));
    }

    /// R4-DR-08: presets say which level they build for and stay reachable after a weapon is equipped.
    #[test]
    fn presets_name_their_level_and_stay_reachable() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        let o = views::overview(&mut p);
        assert!(o["presets"].as_array().is_some_and(|a| !a.is_empty()) && o["hasWeapon"] == json!(true));
        let js = include_str!("../ui/app.js");
        assert!(js.contains("Start over from a preset") && js.contains("for a level-${ST.level} character"));
    }

    /// R3-AA-03 (second reopen): the body grid can shrink to the window, and large text stacks every layout to one
    /// column with a narrower rail.
    #[test]
    fn large_text_stacks_the_layout() {
        let css = include_str!("../ui/styles.css");
        assert!(css.contains("min-height: 0; min-width: 0; }") && css.contains("#app > * { min-width: 0; }"));
        assert!(css.contains("html.big .grid2, html.big .grid3"));
        assert!(include_str!("../ui/app.js").contains(r#"classList.toggle("big", v >= 1.5)"#));
    }

    /// R3-AA-03 (reopened in R5): the section tabs wrap to a second row instead of running past the window at large
    /// text, so the page never shifts sideways when a far tab takes focus.
    #[test]
    fn section_tabs_wrap_at_large_text() {
        let css = include_str!("../ui/styles.css");
        let tabs = css.lines().find(|l| l.starts_with("nav.tabs {")).unwrap();
        assert!(tabs.contains("flex-wrap: wrap"), "{tabs}");
    }

    /// R4-DR-10 (reopened): enemy labels carry no internal tags (hook points, health-bar variants), and every label is
    /// unique.
    #[test]
    fn enemy_labels_have_no_internal_tags() {
        let p = Planner::new();
        let mut labels = std::collections::HashSet::new();
        for e in &p.enemies {
            let l = e.label.to_lowercase();
            assert!(!l.contains("hookpt") && !l.contains("healthbar") && !l.contains(" hook"), "{}", e.label);
            assert!(labels.insert(e.label.clone()), "two enemies read {}", e.label);
        }
    }

    /// The enemy picker reads in-game names, one entry per enemy (docs/plans/enemy-names.md): a named template stands
    /// for its variants, a fight saved against a variant finds that entry, and unnamed bosses read in words.
    #[test]
    fn enemies_have_in_game_names_and_variants_merge() {
        let mut p = Planner::new();
        let find = |p: &Planner, label: &str| p.enemies.iter().position(|e| e.label == label).unwrap_or_else(|| panic!("no {label}"));
        let scalper = find(&p, "Scavenger Scalper");
        assert_eq!(p.enemies[scalper].name.as_deref(), Some("Enemy_Scavenger_Melee01"), "the template the game names stands for it");
        assert!(p.enemies[scalper].aliases.iter().any(|a| a == "Enemy_Scavenger_Melee05_Night_Patrol_Guard"));
        assert_eq!(model::find_enemy(&p.enemies, "Enemy_Scavenger_Melee05_Night_Patrol_Guard"), Some(scalper));
        for n in ["Cyclops", "Fell Cyclops", "Drak'Dal Hunter", "Hollow Wretch", "Vorgoth Phase 1 Humanoid (boss)"] {
            find(&p, n);
        }
        assert!(p.enemies.len() < 200, "{} entries", p.enemies.len());
        for e in &p.enemies {
            assert!(!e.label.contains('_') && !e.label.contains(" · ") && !e.label.contains(" #"), "{}", e.label);
        }
        // a fight saved against a variant opens on the merged entry
        let mut s = p.snapshot();
        s.fight.enemy_key = Some("Enemy_Scavenger_Melee05_Night_Patrol_Guard".into());
        p.restore(s).unwrap();
        assert_eq!(p.fight.enemy, scalper);
    }

    /// R5-PS-01: a fight saved before enemy names were kept opens against the same enemy (its index is read in the old
    /// list) and says so.
    #[test]
    fn old_saved_fight_finds_its_enemy() {
        let mut p = Planner::new();
        let legacy = model::legacy_enemy_names(p.b);
        let (old_index, name) = legacy.iter().enumerate()
            .filter_map(|(i, n)| n.clone().map(|n| (i, n)))
            .find(|(i, n)| model::find_enemy(&p.enemies, n).is_some_and(|k| k != *i))
            .expect("an enemy whose index moved");
        let mut s = p.snapshot();
        s.fight.enemy = old_index;
        s.fight.enemy_key = None;
        p.restore(s).unwrap();
        assert_eq!(Some(p.fight.enemy), model::find_enemy(&p.enemies, &name));
        assert!(p.notice.as_deref().is_some_and(|n| n.contains("older planner")));
    }

    /// R5-AA-01: every text field gets the dark input style (inputs without a type included) and the page declares a dark
    /// colour scheme, so typed text is readable.
    #[test]
    fn text_fields_are_readable() {
        let css = include_str!("../ui/styles.css");
        assert!(css.contains("color-scheme: dark;") && css.contains("input:not([type])"));
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"<input type="text" id="savedir""#) && js.contains(r#"<input type="text" list="skilllist""#));
    }

    /// R5-AA-02: the dialogs are modal for the keyboard: the page behind is inert, Tab wraps inside, Enter in the save
    /// folder field submits it, and closing returns focus to the control that opened it.
    #[test]
    fn save_dir_dialog_traps_focus_submits_on_enter_and_restores_focus() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r##"$("#app").inert = true;"##) && js.contains(r##"$("#app").inert = false;"##));
        assert!(js.contains(r#"DLG_OPENER = document.activeElement && document.activeElement.id"#) && js.contains("if (o) o.focus();"));
        assert!(js.contains(r#"if (e.key !== "Tab") return;"#) && js.contains("first.focus()") && js.contains("last.focus()"));
        assert!(js.contains(r#"<form class="dlg" id="sdform""#) && js.contains(r##"$("#sdform").onsubmit"##) && js.contains(r#"type="submit" class="btn" id="sdok""#));
        assert!(js.contains(r##"$("#sdcancel").onclick = () => closeBuilds();"##));
        // the toast stays outside the inert page so it is still announced while a dialog is open
        let html = include_str!("../ui/index.html");
        assert!(html.find(r#"id="status""#) > html.find("</footer>\n</div>"));
    }

    /// R5-AA-03: the footer wraps instead of clipping, so the data line and the shortcut list stay visible at large text.
    #[test]
    fn footer_wraps_without_clipping_at_200_percent_text() {
        let css = include_str!("../ui/styles.css");
        let footer = css.lines().find(|l| l.starts_with("footer {")).unwrap();
        assert!(footer.contains("flex-wrap: wrap") && !footer.contains("overflow: hidden") && !footer.contains("nowrap"), "{footer}");
    }

    /// R5-AA-04: a tip is shown for keyboard focus too and read as the focused control's description; the goal names
    /// that carry the goal definitions take focus.
    #[test]
    fn data_tip_content_reachable_by_focus_and_described_by() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"document.addEventListener("focusin""#) && js.contains(r#"setAttribute("aria-describedby", "tip")"#));
        assert!(js.contains(r#"document.addEventListener("focusout""#));
        assert!(js.contains(r#"<span class="g" tabindex="0" data-tip="${esc(GOAL_TIPS[g] || "")}">"#));
    }

    /// R5-AA-05: every segmented button says whether it is the selected one.
    #[test]
    fn hit_kind_buttons_expose_aria_pressed() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"<button data-hk="${k}" class="${f.hit_kind === k ? "on" : ""}" aria-pressed="${f.hit_kind === k}">"#));
        assert!(js.contains(r#"aria-pressed="${!ui.heat}""#) && js.contains(r#"aria-pressed="${k === cur}""#));
        assert!(js.contains(r#"aria-pressed="${(ui.optView || "level") === k}""#));
    }

    /// R5-AA-06: a change to Total DPS is announced through a polite live region (only when the number changes).
    #[test]
    fn total_dps_change_is_announced_politely() {
        let html = include_str!("../ui/index.html");
        assert!(html.contains(r#"<div id="dpslive" class="sr-only" aria-live="polite" aria-atomic="true"></div>"#));
        let js = include_str!("../ui/app.js");
        assert!(js.contains("const said = `Total DPS ${fx(n.dps)}`;") && js.contains("if (live.dataset.said !== said)"));
        assert!(include_str!("../ui/styles.css").contains(".sr-only {"));
    }

    /// R5-AA-07: pressing the hit kind already chosen changes nothing, and each kind keeps the share the player gave it.
    #[test]
    fn reselecting_hit_kind_keeps_player_share() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains("if (hk && hk.dataset.hk !== ST.fight.hit_kind) {"));
        assert!(js.contains("ui.hitShares = Object.assign(ui.hitShares || {}, { [ST.fight.hit_kind]: ST.fight.hit_share });"));
        assert!(js.contains("ST.fight.hit_share = ui.hitShares[hk.dataset.hk] ??"));
    }

    /// R6-PS-01: an Eternal spell's entry in the ammo list shows the levels its upgrades reach, not one fixed level.
    #[test]
    fn eternal_ammo_label_shows_its_upgrade_range() {
        let p = Planner::new();
        let l = lists(p.b);
        let all: Vec<&Pick> = l.ammo.iter().collect();
        assert!(all.iter().any(|x| x.label == "Eternal Ice Bolt IV L45\u{2013}50"), "{:?}", all.iter().filter(|x| x.label.starts_with("Eternal Ice")).map(|x| &x.label).collect::<Vec<_>>());
        assert!(all.iter().any(|x| x.label == "Ice Bolt IV L47"));
    }

    /// R6-GW-01: a success message after an action shows only when the action went through with nothing to report, so
    /// a refused build code or a one-time data notice is never covered by "Build loaded.".
    #[test]
    fn load_code_reports_failure_and_notice_not_success() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains("if (ST.notice) { flash(ST.notice, true); clean = false; }") && js.contains("return clean;"));
        assert!(js.contains(r##"if (await act({ type: "load_code", code: $("#codebox").value })) flash("Build loaded.");"##));
        assert!(!js.contains(r#"}); flash("Build loaded.")"#));
        // the backend refuses a broken code with an error the page shows
        let mut p = Planner::new();
        assert!(p.act(&json!({ "type": "load_code", "code": "not a build code" })).is_err());
    }

    /// R6-GW-02: a shared code carries the author's fight setup, so a reader with another Combat setup gets the author's
    /// numbers (and is told); a code without one keeps the reader's setup.
    #[test]
    fn shared_code_carries_fight_scenario() {
        let mut author = Planner::new();
        author.build = Build::sample_bow(author.b);
        author.fight.pack_size = 5.0;
        author.fight.armor_physical = 200.0;
        author.fight.hit_kind = "Backstab".into();
        author.fight.hit_share = 0.5;
        let k = author.enemies.iter().position(|e| e.name.is_some()).unwrap() + 1;
        author.fight.enemy = k.min(author.enemies.len() - 1);
        let want = summarize(&author.engine, &author.build, &author.conditions()).dps;
        let code = author.share_code();
        let mut reader = Planner::new();
        assert!((summarize(&reader.engine, &author.build, &reader.conditions()).dps - want).abs() > 1.0, "the setups differ");
        reader.act(&json!({ "type": "load_code", "code": code })).unwrap();
        assert_eq!(reader.enemies[reader.fight.enemy].name, author.enemies[author.fight.enemy].name);
        assert!(reader.fight.pack_size == 5.0 && reader.fight.armor_physical == 200.0 && reader.fight.hit_kind == "Backstab");
        assert!((summarize(&reader.engine, &reader.build, &reader.conditions()).dps - want).abs() < 1e-6 * want.max(1.0));
        assert!(reader.notice.as_deref().is_some_and(|n| n.contains("fight setup")));
        // a plain build code leaves the reader's setup alone
        let mut other = Planner::new();
        other.fight.pack_size = 3.0;
        other.act(&json!({ "type": "load_code", "code": author.build.to_code_for(author.b) })).unwrap();
        assert_eq!(other.fight.pack_size, 3.0);
    }

    /// R6-GW-05: every Combat setup value off its default is listed on the rail's Fighting card, beside Total DPS.
    #[test]
    fn rail_fighting_card_lists_every_nondefault_setting_that_moves_dps() {
        let mut p = Planner::new();
        assert!(views::state(&mut p)["fightChanges"].as_array().unwrap().is_empty());
        p.fight.summon_attack_s = 8.0;
        p.fight.zone_stay = 0.25;
        p.fight.armor_physical = 200.0;
        let ch: Vec<String> = views::state(&mut p)["fightChanges"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        assert!(ch.iter().any(|c| c.contains("companion attacks every 8 s")), "{ch:?}");
        assert!(ch.iter().any(|c| c.contains("zones 25%")) && ch.iter().any(|c| c.contains("armour 200/0")), "{ch:?}");
        assert!(include_str!("../ui/app.js").contains("ST.fightChanges.map(esc)"));
    }

    /// R6-PS-02: gems renamed for 1.0 are found under the names the patch notes use too.
    #[test]
    fn gem_search_matches_patch_note_names() {
        let p = Planner::new();
        let l = lists(p.b);
        let find = |q: &str| l.gems.iter().filter(|g| g.label.to_lowercase().contains(&q.to_lowercase())).map(|g| g.label.clone()).collect::<Vec<_>>();
        assert!(find("Toxic Reaction").iter().any(|x| x.starts_with("Gem of Toxic Betrayal")), "{:?}", find("Toxic"));
        assert!(find("Obsession").iter().any(|x| x.starts_with("Gem of Battle Mania")));
        assert!(find("Hyperspecialization").iter().all(|x| !x.contains("also")), "same name, no alias");
    }

    /// R6-GW-04: a loadout copies as plain text anyone can read without the app: its numbers, the fight they assume, its
    /// gear and skills, the game data, and the code to load it.
    #[test]
    fn shared_build_has_plain_text_summary_export() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        p.fight.pack_size = 4.0;
        let s = summarize(&p.engine, &p.build, &p.conditions());
        let t = p.text_summary();
        assert!(t.contains(&format!("Total DPS {:.1}", s.dps)), "{t}");
        assert!(t.contains("4 enemies within") && t.contains("Weapon: ") && t.contains("Skills (") && t.contains(&ens_calc::build::patch_label(p.b)));
        let code = t.lines().find_map(|l| l.strip_prefix("Code: ")).unwrap();
        assert_eq!(Build::from_code_for(code, p.b).unwrap().0, p.build);
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"id="copytext""#) && js.contains(r#"invoke("text_summary")"#));
    }

    /// R7-TC-02: an Eternal arrow or spell shows the level and perks it is priced at and lets the player set them, as
    /// a weapon does; summon spells get a level too; an owned one keeps the state its save records; the text export
    /// says it.
    #[test]
    fn eternal_ammo_upgrade_state_is_editable_and_shown() {
        let mut p = Planner::new();
        let b = p.b;
        let staff = guid(b, "Luminary Staff");
        let bolt = guid(b, "Eternal Ice Bolt IV");
        p.build = Build::new(b, 50);
        p.build.weapon = Some(ens_calc::Equip { item: staff, rarity: Some("Legendary".into()), level: Some(50), perks: None });
        p.build.ammo = Some(bolt.clone());
        let slot = |p: &mut Planner| views::state(p)["slots"].as_array().unwrap().iter().find(|s| s["id"] == "ammo").cloned().unwrap();
        let s = slot(&mut p);
        assert_eq!((s["item"]["level"].as_u64(), s["item"]["perksMax"].as_u64(), s["item"]["perks"].as_u64()), (Some(50), Some(5), Some(5)), "{s}");
        let full = summarize(&p.engine, &p.build, &p.conditions()).dps;
        p.act(&json!({ "type": "item", "slot": "ammo", "perks": 0 })).unwrap();
        p.act(&json!({ "type": "item", "slot": "ammo", "level": 45 })).unwrap();
        let s = slot(&mut p);
        assert_eq!((s["item"]["level"].as_u64(), s["item"]["perks"].as_u64()), (Some(45), Some(0)), "{s}");
        assert!(summarize(&p.engine, &p.build, &p.conditions()).dps < full, "a fresh Eternal spell does less");
        assert!(p.text_summary().contains("Eternal Ice Bolt IV L45, 0/5 perks"), "{}", p.text_summary());
        // a summon spell's level
        let wretch = guid(b, "Eternal Hollow Wretch II");
        p.build.summons = vec![wretch.clone()];
        let sm = views::state(&mut p)["slots"].as_array().unwrap().iter().find(|s| s["id"] == "summon").cloned().unwrap();
        let top = sm["items"][0]["levelMax"].as_u64().unwrap();
        assert_eq!(sm["items"][0]["level"].as_u64(), Some(top));
        p.act(&json!({ "type": "upgrade", "guid": wretch, "level": sm["items"][0]["levelMin"] })).unwrap();
        assert_eq!(p.build.upgrades[&wretch].level, sm["items"][0]["levelMin"].as_u64().map(|x| x as u32));
        // an owned Eternal spell keeps its recorded state
        let fresh = ens_calc::Equip { item: bolt.clone(), rarity: Some("Common".into()), level: Some(45), perks: Some(0) };
        let o = store::OwnedItem { item: fresh.clone(), count: 1, equipped: false };
        let mut nb = p.build.clone();
        nb.upgrades.clear();
        let nb = game::with_owned(b, &nb, Slot::Ammo, &o);
        assert_eq!(nb.upgrades.get(&bolt), Some(&fresh));
        assert!(include_str!("../ui/app.js").contains(r#"act({ type: "upgrade", guid: e.target.dataset.upgrade"#));
    }

    /// R7-RT-03: the Overview lists every loadout side by side under the current fight.
    #[test]
    fn loadouts_compare_side_by_side() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        p.act(&json!({ "type": "new_loadout" })).ok();
        if p.loadouts.len() < 2 {
            p.loadouts.push(Build::new(p.b, 10));
        }
        let o = views::overview(&mut p);
        let ls = o["loadouts"].as_array().unwrap();
        assert_eq!(ls.len(), p.loadouts.len());
        assert!(ls.iter().any(|l| l["current"] == true) && ls.iter().all(|l| l["dps"].is_number()));
        assert!(include_str!("../ui/app.js").contains("function loadoutTable(ls)"));
    }

    /// R7-RT-06: each candidate row carries its perks, and the page's search matches them as well as the name.
    #[test]
    fn item_search_matches_perks() {
        let p = Planner::new();
        let c = p.conditions();
        let g = guid(p.b, "Hunter's Ring");
        let t = candidate_table(&p.opt, p.b, &p.build, &c, Slot::Armor(5), vec![g], &[]);
        let perks = t["rows"][0]["perks"].as_array().unwrap();
        assert!(!perks.is_empty(), "{t}");
        assert!(include_str!("../ui/app.js").contains("|| perkHit(r).length"));
    }

    /// R8-SB-01: a hit kind from a share code never reaches the page as markup: a code whose fight names an unknown hit
    /// kind keeps the reader's setup, the fight action refuses one, and the page escapes the value where it writes it.
    #[test]
    fn hit_kind_help_text_escapes_markup() {
        let mut author = Planner::new();
        author.build = Build::sample_bow(author.b);
        author.fight.hit_kind = r#"<img src=x onerror="document.title='x'">"#.into();
        let code = author.share_code();
        let mut reader = Planner::new();
        reader.act(&json!({ "type": "load_code", "code": code })).unwrap();
        assert_eq!(reader.fight.hit_kind, "Normal");
        let mut f = serde_json::to_value(&reader.fight).unwrap();
        f["hit_kind"] = json!("<b>x</b>");
        assert!(reader.act(&json!({ "type": "fight", "fight": f })).is_err());
        assert!(include_str!("../ui/app.js").contains("spent on ${esc(f.hit_kind.toLowerCase())} attacks"));
    }

    /// R8-SB-02: names are cut to the length a code may carry, so a pasted giant name cannot bloat every save.
    #[test]
    fn oversized_names_are_cut() {
        let mut p = Planner::new();
        p.act(&json!({ "type": "loadout_name", "value": "x".repeat(100_000) })).unwrap();
        assert_eq!(p.build.name.chars().count(), ens_calc::build::MAX_NAME_CHARS);
    }

    /// R8-SB-04: the headline names the fight length in use, on the rail and in the text export.
    #[test]
    fn fight_length_label_matches_the_setting() {
        let mut p = Planner::new();
        p.build = Build::sample_bow(p.b);
        p.fight.fight_s = 10.0;
        assert!(p.text_summary().contains("(10 s fight)"), "{}", p.text_summary());
        let js = include_str!("../ui/app.js");
        assert!(js.contains("Total DPS · ${fx(ST.fight.fight_s || 60, 0)} s fight") && !js.contains("Total DPS · 60 s fight"));
    }

    /// R8-PS-01, R8-PS-02, R8-PS-04: the Calculations tab lists what the planner does not model (wand heavy attacks
    /// among them), a wand build says its heavy attack is not priced, and a heavy melee build names the conflicting
    /// source for its multiplier.
    #[test]
    fn app_shows_not_modeled_systems_list() {
        let mut p = Planner::new();
        p.build = Build::new(p.b, 50);
        p.build.weapon = Some(ens_calc::Equip { item: guid(p.b, "Luminous Wand"), rarity: None, level: None, perks: None });
        let c = views::calcs(&mut p).to_string();
        assert!(c.contains("Wand heavy attack (not counted)") && c.contains("Summon scrolls (not in the numbers)"), "{c}");
        p.build.weapon = Some(ens_calc::Equip { item: guid(p.b, "Shroud Vessel's Greatsword"), rarity: None, level: None, perks: None });
        p.build.heavy = true;
        let c = views::calcs(&mut p).to_string();
        assert!(c.contains("Heavy attack damage") && !c.contains("Wand heavy attack (not counted)"));
    }

    /// R8-SB-05: an out-of-range node is refused with a message instead of crashing the planner.
    #[test]
    fn allocate_and_remove_reject_out_of_range_nodes() {
        let mut p = Planner::new();
        let n = p.build.ranks.len();
        assert!(p.act(&json!({ "type": "allocate", "node": n + 5 })).is_err());
        assert!(p.act(&json!({ "type": "remove", "node": n })).is_err());
    }

    /// R8-SB-06: an optimizer result made for another level is not applied to the open loadout, so its level stays.
    #[test]
    fn apply_opt_keeps_the_open_loadouts_level() {
        let mut p = Planner::new();
        let plan = optim::plan_from(&p);
        let pl = p.opt.prepare(&plan, &vec![0; p.b.skills.len()]);
        let (best, order) = p.opt.optimize(&pl, 1, 3);
        let c = p.conditions();
        let summary = summarize(&p.engine, &p.build, &c);
        p.opt_result = Some(OptResult { ranks: best.ranks, order, goals: best.goals, gear: None, summary, plan: Some(pl) });
        let lvl = p.build.level;
        p.build.level = lvl + 7;
        assert!(p.act(&json!({ "type": "apply_opt" })).is_err());
        assert_eq!(p.build.level, lvl + 7);
    }

    /// R8-TC-08: a summon's level sets its unlocked perks too, so it can only take levels the game allows, and a
    /// record the game cannot have is flagged.
    #[test]
    fn summon_upgrade_level_matches_unlocked_perks() {
        let mut p = Planner::new();
        let b = p.b;
        // a fresh build: another test may have set ENS_SAMPLE, which starts the planner on the sample bow
        p.build = Build::new(b, 50);
        p.build.weapon = Some(ens_calc::Equip { item: guid(b, "Luminary Staff"), rarity: None, level: None, perks: None });
        let wretch = guid(b, "Eternal Hollow Wretch II");
        p.build.summons = vec![wretch.clone()];
        let r = b.items[&wretch].levels.as_ref().unwrap().clone();
        p.act(&json!({ "type": "upgrade", "guid": wretch, "level": r.min })).unwrap();
        let e = p.build.upgrades[&wretch].clone();
        assert_eq!((e.level, e.perks), (Some(r.min), Some(r.min.saturating_sub(r.max))));
        assert!(p.build.problems(b).is_empty(), "{:?}", p.build.problems(b));
        p.build.upgrades.insert(wretch.clone(), ens_calc::Equip { item: wretch.clone(), rarity: None, level: Some(r.min), perks: None });
        assert!(!p.build.problems(b).is_empty(), "level {} with every perk unlocked passed", r.min);
    }

    /// R8-TC-07, R8-PS-03, R8-PS-05: loadouts within 1% are not called a winner; the milestone tooltip names its
    /// community source; an Early Access save is named as such.
    #[test]
    fn small_r8_fixes() {
        let js = include_str!("../ui/app.js");
        assert!(js.contains(r#"(best - l.dps) / best < 0.01 ? "≈ best""#));
        assert!(js.contains("community pacing guide, not game data"));
        let e = ens_data::save::Error::BadMagic { found: *b"KSC1" };
        assert!(e.to_string().contains("Early Access"));
    }

    /// R1-DR-01: a weapon candidate is priced at the rarity and level its row shows: its own (fixed) level,
    /// its own rarity when that is fixed, and the rarity you hold when the item's rarity rolls on drop.
    #[test]
    fn weapon_candidate_row_uses_its_own_rarity_and_level_or_says_so() {
        let p = Planner::new();
        let b = p.b;
        let mut build = Build::new(b, 30);
        build.weapon = Some(ens_calc::Equip { item: guid(b, "Copperblade Greatsword"), rarity: Some("Common".into()), level: Some(30), perks: None });
        for name in ["Shroud Vessel's Greatsword", "Guard Of The North Sword"] {
            let g = guid(b, name);
            let it = &b.items[&g];
            let w = apply_slot(b, &build, Slot::Weapon, &g).weapon.unwrap();
            assert_eq!(w.level, it.levels.as_ref().map(|l| l.min), "{name}");
            if it.generate_rarity != Some(true) {
                assert_eq!(w.rarity, it.rarity, "{name} has a fixed rarity");
            }
            let (rarity, level) = candidate_shown(b, &build, Slot::Weapon, &g, &None);
            assert_eq!((Some(rarity), level), (w.rarity.clone(), w.level), "{name}: the row shows what was priced");
        }
    }
}

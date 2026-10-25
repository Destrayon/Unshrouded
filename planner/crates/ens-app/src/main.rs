//! Enshrouded Planner — desktop app (egui/eframe).
//!
//! Top bar: build name, loadouts, level, Flame level, points, undo/redo, share. Tabs: Overview (where the damage
//! comes from, how a hit is built, survival, what holds the build back), Skill tree, Gear (slot cards and a
//! candidate list per slot), Combat setup (the enemy and how you play), Calculations, Optimizer, Notes. The narrow
//! left rail always shows the headline numbers and what the hovered node or item would change.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diag;
mod labels;
mod theme;

use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, RichText, Sense, Stroke, Vec2};
use ens_calc::{Build, Engine, Equip, Target};
use ens_data::Bundle;
use ens_opt::{attack_rate, points_available, Goal, Optimizer, Plan, GOALS};
use labels::{attr_label, attr_value, contribution, trim_num};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use theme::*;

static BUNDLE_JSON: &str = include_str!("../../../data/bundle.json");

const ARMOR_SLOTS: [&str; 7] = ["Head", "Chest", "Hands", "Legs", "Feet", "Ring", "Ring 2"];
const SOURCE_COLORS: [Color32; 6] = [FLAME, TEAL, Color32::from_rgb(0x9C, 0xA6, 0xEA), GOOD, Color32::from_rgb(0xD9, 0x7A, 0x9A), TEXT2];

fn main() -> eframe::Result {
    let bundle: &'static Bundle = Box::leak(Box::new(Bundle::from_json(BUNDLE_JSON).expect("bundled game data")));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 920.0]).with_min_inner_size([1180.0, 720.0]).with_title("Unshrouded")
            .with_maximized(std::env::var("ENS_MAX").is_ok()),
        // wgpu's default queues two frames ahead, which makes hovers and clicks trail the mouse on Windows
        // (egui issue #5037); the low-latency preset queues one
        // and with vsync every frame waits for the next refresh: egui repaints on every move/resize message, which
        // Windows sends at the mouse's report rate during a drag, so the waits pile up in Windows' move/resize loop
        // and the window and pointer trail the mouse. The app only repaints on demand, so presenting without vsync
        // costs nothing while idle.
        wgpu_options: eframe::WgpuConfiguration {
            surface: surface_config(),
            wgpu_setup: wgpu_setup(),
            ..Default::default()
        },
        ..Default::default()
    };
    // Our own event loop so raw device input can be switched off. winit otherwise registers for raw mouse input
    // (WM_INPUT) while the window has focus; a high-report-rate mouse then sends thousands of messages a second,
    // and Windows only delivers a repaint when the queue is empty, so frames were held back 30-1300 ms (worst at
    // the window edges). egui only needs ordinary pointer positions.
    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event().build().map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
    event_loop.listen_device_events(winit::event_loop::DeviceEvents::Never);
    let mut app = eframe::create_native(
        "Enshrouded Planner",
        options,
        Box::new(move |cc| {
            theme::setup(&cc.egui_ctx);
            dark_title_bar(cc);
            let mut app = App::new(bundle);
            app.gpu = cc.wgpu_render_state.as_ref().map(|r| {
                let i = r.adapter.get_info();
                format!("{:?} · {}", i.backend, i.name)
            }).unwrap_or_default();
            Ok(Box::new(app))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut app).map_err(|e| eframe::Error::AppCreation(Box::new(e)))
}

/// How frames are handed to Windows. ENS_PRESENT (fifo, mailbox, immediate) and ENS_LATENCY (frames queued) override
/// it for lag testing.
fn surface_config() -> eframe::SurfaceConfig {
    use eframe::wgpu::PresentMode;
    let present_mode = match std::env::var("ENS_PRESENT").as_deref() {
        Ok("fifo") => PresentMode::Fifo,
        Ok("immediate") => PresentMode::Immediate,
        Ok("autovsync") => PresentMode::AutoVsync,
        _ => PresentMode::Mailbox,
    };
    let latency = std::env::var("ENS_LATENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    eframe::SurfaceConfig { present_mode, desired_maximum_frame_latency: Some(latency) }
}

/// Graphics adapter choice: on Windows prefer Direct3D 12 (the system's own presentation path, which the compositor
/// and the mouse pointer cope with best) over Vulkan, which wgpu otherwise tends to pick; then a discrete GPU; else
/// whatever is compatible with the window.
fn wgpu_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::wgpu;
    let mut s = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    // Direct3D 12 presents through a DirectComposition visual instead of a swap chain on the window itself: with
    // the window's own swap chain, a maximized window stuttered whenever the pointer moved onto the title bar or
    // taskbar (Windows moving the window on and off a hardware overlay plane); the user confirmed this is smooth.
    // WGPU_DX12_PRESENTATION_SYSTEM=Hwnd still selects the old way.
    s.instance_descriptor.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual.with_env();
    s.native_adapter_selector = Some(Arc::new(|adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        let ok = |a: &&wgpu::Adapter| surface.map(|s| a.is_surface_supported(s)).unwrap_or(true);
        let rank = |a: &wgpu::Adapter| {
            let i = a.get_info();
            let backend = match i.backend {
                wgpu::Backend::Dx12 if cfg!(target_os = "windows") => 0,
                wgpu::Backend::Vulkan | wgpu::Backend::Metal | wgpu::Backend::Dx12 => 1,
                _ => 2,
            };
            let device = match i.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                _ => 2,
            };
            (backend, device)
        };
        adapters.iter().filter(ok).min_by_key(|a| rank(a)).cloned().ok_or_else(|| "no graphics adapter can draw to this window".to_string())
    }));
    eframe::egui_wgpu::WgpuSetup::CreateNew(s)
}

/// Dark window title bar even when Windows itself is in light mode, set once through the documented DWM attribute
/// (DWMWA_USE_IMMERSIVE_DARK_MODE = 20, Windows 10 20H1 and later). egui's own theme sync is off (theme::setup):
/// it re-sends the theme and fakes a deactivate/activate pair to repaint the title bar, which flickers and lags.
#[cfg(target_os = "windows")]
fn dark_title_bar(cc: &eframe::CreationContext) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    let Ok(h) = cc.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = h.as_raw() {
        let on: i32 = 1;
        // SAFETY: a valid window handle from eframe and a 4-byte BOOL, as the attribute requires
        unsafe {
            DwmSetWindowAttribute(w.hwnd.get(), 20, &on as *const i32 as *const _, 4);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn dark_title_bar(_cc: &eframe::CreationContext) {}

/// An enemy to calculate against: its template's susceptibility, FactionType, class and flying flag.
#[derive(Clone)]
struct EnemyPick {
    label: String,
    /// template name in the bundle's enemyTypes (None = training dummy)
    name: Option<String>,
    sus: HashMap<String, f64>,
    faction: Option<String>,
    class: Option<String>,
    flying: bool,
}

#[derive(Clone)]
struct Pick {
    guid: String,
    label: String,
    rarity: String,
}

struct OptDone {
    /// Chosen weapon and armour/rings when the optimizer also picked gear.
    gear: Option<(Option<Equip>, Vec<Option<Equip>>)>,
    ranks: Vec<u8>,
    order: Vec<usize>,
    goals: HashMap<&'static str, f64>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Overview,
    Tree,
    Gear,
    Setup,
    Calcs,
    Optimizer,
    Notes,
}

/// A gear slot on the Gear tab.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Weapon,
    Ammo,
    Gem,
    Armor(usize),
    Food,
    Summon,
}

impl Slot {
    fn name(self) -> &'static str {
        match self {
            Slot::Weapon => "Weapon",
            Slot::Ammo => "Ammo / spell",
            Slot::Gem => "Weapon gem",
            Slot::Armor(k) => ARMOR_SLOTS[k],
            Slot::Food => "Food",
            Slot::Summon => "Summons kept up",
        }
    }
}

/// One line of "where your damage comes from".
#[derive(Clone)]
struct Source {
    name: String,
    dps: f64,
    note: String,
}

/// The numbers the rail and the Overview show, for the current build or a preview of a change.
#[derive(Clone, Default)]
struct Summary {
    dps: f64,
    attacking_dps: f64,
    expected_hit: f64,
    ttk: f64,
    mana_uptime: f64,
    mana_per_s: f64,
    has_mana_cost: bool,
    hits_survived: f64,
    crit: f64,
    health: f64,
    health_regen: f64,
    mana_regen: f64,
    armor_phys: f64,
    armor_mag: f64,
    hit_taken: f64,
    hit_raw: f64,
    sources: Vec<Source>,
    fallback: Option<(String, f64)>,
}

#[derive(Clone)]
struct Conditions {
    target: Target,
    enemy_level: u32,
    hp_factor: f64,
}

fn summarize(engine: &Engine, b: &Build, c: &Conditions) -> Summary {
    let sheet = engine.sheet(b);
    let g = |k: &str| sheet.values.get(k).copied().unwrap_or(0.0);
    let tot = engine.total(b, &c.target);
    let hp = ens_calc::enemy_health(engine.bundle, c.enemy_level, c.hp_factor);
    let (raw, taken) = engine.incoming_target(b, c.enemy_level, &c.target);
    let st = engine.attributes(b);
    let up = tot.mana_uptime;
    let mut sources = vec![];
    let main_name = b.ammo.as_deref().or(b.weapon.as_ref().map(|w| w.item.as_str())).and_then(|g| engine.bundle.items.get(g)).map(|i| i.name.clone()).unwrap_or_else(|| "Main attack".into());
    if tot.main.regular_dps > 0.0 {
        let note = if up < 0.999 { format!("Main attack while mana lasts · {:.0}% of the fight", up * 100.0) } else { "Main attack".into() };
        sources.push(Source { name: main_name, dps: tot.main.regular_dps * up, note });
    }
    if tot.main.special_dps > 0.0 {
        sources.push(Source { name: "Special ability".into(), dps: tot.main.special_dps * up, note: format!("{:.3} uses per second", tot.main.special_per_s) });
    }
    if let Some((n, fd)) = &tot.fallback {
        sources.push(Source { name: n.clone(), dps: fd * (1.0 - up), note: format!("Used when out of mana · {:.0}% of the fight", (1.0 - up) * 100.0) });
    }
    for s in &tot.summons {
        if s.total_dps > 0.0 {
            sources.push(Source { name: s.name.clone(), dps: s.total_dps, note: format!("{} · {:.1} alive", if s.kind == "totem" { "Totem" } else { "Companion" }, s.count) });
        }
    }
    for (n, d, how) in &tot.skill_damage {
        if *d > 0.0 {
            sources.push(Source { name: n.clone(), dps: *d, note: how.clone() });
        }
    }
    let listed: f64 = sources.iter().map(|s| s.dps).sum();
    let rest = tot.total - listed;
    if rest.abs() > 0.05 {
        let names: Vec<String> = tot.buffs.iter().map(|(n, u, _)| format!("{n} ({:.0}% up)", u * 100.0)).collect();
        sources.push(Source { name: "Triggered buffs".into(), dps: rest, note: if names.is_empty() { "Skills that raise damage while active".into() } else { names.join(", ") } });
    }
    sources.sort_by(|a, b| b.dps.partial_cmp(&a.dps).unwrap_or(std::cmp::Ordering::Equal));
    Summary {
        dps: tot.total,
        attacking_dps: tot.main.total,
        expected_hit: tot.main.hit.expected,
        ttk: if tot.total > 0.0 { hp / tot.total } else { f64::INFINITY },
        mana_uptime: up,
        mana_per_s: tot.main_mana_per_s,
        has_mana_cost: tot.main_mana_per_s > 0.0,
        hits_survived: g("Health") / taken.max(1.0),
        crit: if b.weapon.is_some() { tot.main.hit.crit_chance } else { g("CritChance") },
        health: g("Health"),
        health_regen: engine.regen_per_s(&st, "Health"),
        mana_regen: g("Mana regen /s"),
        armor_phys: g("ArmorPoints_Physical"),
        armor_mag: g("ArmorPoints_Magical"),
        hit_taken: taken,
        hit_raw: raw,
        sources,
        fallback: tot.fallback.clone(),
    }
}

/// Shortest chain of unallocated nodes from the allocated tree (or a root) to `target`, target included.
fn path_to(opt: &Optimizer, ranks: &[u8], target: usize) -> Option<Vec<usize>> {
    let skills = &opt.bundle.skills;
    if skills[target].kind == "Root" {
        return None;
    }
    if ranks[target] > 0 {
        return if (ranks[target] as u32) < skills[target].max.max(1) { Some(vec![target]) } else { None };
    }
    let mut prev = vec![usize::MAX; ranks.len()];
    let mut seen = vec![false; ranks.len()];
    let mut q = VecDeque::new();
    for i in 0..ranks.len() {
        if ranks[i] > 0 || skills[i].kind == "Root" {
            seen[i] = true;
            q.push_back(i);
        }
    }
    while let Some(u) = q.pop_front() {
        if u == target {
            break;
        }
        for &v in &opt.adj[u] {
            if !seen[v] && skills[v].kind != "Root" {
                seen[v] = true;
                prev[v] = u;
                q.push_back(v);
            }
        }
    }
    if !seen[target] {
        return None;
    }
    let mut path = vec![];
    let mut u = target;
    while ranks[u] == 0 && skills[u].kind != "Root" {
        path.push(u);
        u = prev[u];
        if u == usize::MAX {
            return None;
        }
    }
    path.reverse();
    Some(path)
}

fn with_path(b: &Build, path: &[usize]) -> Build {
    let mut nb = b.clone();
    for &i in path {
        nb.ranks[i] += 1;
    }
    nb
}

fn rarity_of(b: &Bundle, guid: &str) -> String {
    b.items.get(guid).and_then(|i| i.rarity.clone()).unwrap_or_default()
}

/// The build with `guid` put into `slot` (weapons keep the equipped rarity and level so they compare like for like).
fn apply_slot(bundle: &Bundle, b: &Build, slot: Slot, guid: &str) -> Build {
    let mut nb = b.clone();
    match slot {
        Slot::Weapon => {
            let lvl = nb.weapon.as_ref().and_then(|w| w.level);
            let rar = nb.weapon.as_ref().and_then(|w| w.rarity.clone()).or_else(|| Some(rarity_of(bundle, guid)));
            nb.weapon = Some(Equip { item: guid.to_string(), rarity: rar, level: lvl, perks: None });
        }
        Slot::Ammo => nb.ammo = Some(guid.to_string()),
        Slot::Gem => {
            let lvl = nb.gem.as_ref().and_then(|g| g.level);
            nb.gem = Some(Equip { item: guid.to_string(), rarity: None, level: lvl, perks: None });
        }
        Slot::Armor(k) => nb.gear[k] = Some(Equip { item: guid.to_string(), rarity: Some(rarity_of(bundle, guid)), level: None, perks: None }),
        Slot::Food => {
            if !nb.foods.iter().any(|f| f == guid) && nb.foods.len() < 4 {
                nb.foods.push(guid.to_string());
            }
        }
        Slot::Summon => {
            if !nb.summons.iter().any(|f| f == guid) && nb.summons.len() < 3 {
                nb.summons.push(guid.to_string());
            }
        }
    }
    nb
}

fn key_of(b: &Build, c: &Conditions) -> String {
    let t = &c.target;
    format!("{}|{:?}|{:?}|{}|{}", b.to_code(),
            (t.armor_physical, t.armor_magical, t.pack_size, t.pack_radius, t.susceptibility.len(), t.susceptibility.get("Fire").map(|x| x.to_bits())),
            (&t.faction, &t.enemy_class, t.flying, &t.incoming_type, &t.hit_kind, t.weak_spot, t.soaked, &t.enemy_type, format!("{:?}", t.play)), c.enemy_level, c.hp_factor)
}

/// A candidate item for a slot, tried in the build.
#[derive(Clone)]
struct Cand {
    guid: String,
    dps: f64,
    survived: f64,
}

/// Something the Overview suggests fixing, with an optional one-click action.
enum Act {
    Tab(Tab),
    Slot(Slot),
    Ammo(String),
}

struct Insight {
    warn: bool,
    title: String,
    body: String,
    action: Option<(String, Act)>,
}

struct App {
    b: &'static Bundle,
    engine: Engine<'static>,
    opt: &'static Optimizer<'static>,
    build: Build,
    build_name: String,
    loadouts: Vec<Build>,
    current: usize,
    undo: Vec<Build>,
    redo: Vec<Build>,
    snapshot: Build,
    tab: Tab,
    weapons: Vec<Pick>,
    ammo: Vec<Pick>,
    gear_lists: Vec<Vec<Pick>>,
    foods: Vec<Pick>,
    summon_list: Vec<Pick>,
    gem_list: Vec<Pick>,
    enemies: Vec<EnemyPick>,
    enemy_filter: String,
    enemy: usize,
    target_armor: (f64, f64),
    enemy_level: u32,
    enemy_hp_factor: f64,
    pack: (f64, f64),
    orb_pickup: f64,
    incoming_type: String,
    hit_kind: String,
    weak_spot: bool,
    soaked: bool,
    play: ens_calc::Play,
    pan: Vec2,
    zoom: f32,
    hovered: Option<usize>,
    selected: Option<usize>,
    search: String,
    focus_search: bool,
    heatmap: bool,
    heat: Option<(String, Vec<f64>)>,
    heat_rx: Option<(String, Receiver<Vec<f64>>)>,
    slot: Slot,
    cand: Option<(String, Vec<Cand>)>,
    cand_rx: Option<(String, Receiver<Vec<Cand>>)>,
    cand_hover: Option<String>,
    /// Generation counters: a background job stops as soon as a newer one for the same view starts.
    heat_gen: Arc<AtomicU64>,
    cand_gen: Arc<AtomicU64>,
    summary_cache: Option<(String, Summary)>,
    preview_cache: Option<(String, Summary)>,
    filter: String,
    ranking: Vec<Goal>,
    opt_gear: bool,
    gear_rarity: String,
    running: Option<Receiver<OptDone>>,
    last_opt: Option<OptDone>,
    opt_summary: Option<Summary>,
    order_step: usize,
    code: String,
    status: String,
    sheet_query: String,
    sheet_all: bool,
    notes: String,
    scale: f32,
    /// ENS_SHOT=<png path>: render a few frames, save a screenshot, exit (self-check without a human).
    shot: Option<String>,
    /// Graphics backend and adapter in use (shown in the status bar).
    gpu: String,
    /// Frame pacing: when the last frame started and the window rectangle then.
    last_frame: Option<std::time::Instant>,
    last_rect: Option<egui::Rect>,
    /// ENS_LOG=1: per-frame lag log (diag.rs).
    rec: diag::Recorder,
    frames: u32,
}

impl App {
    fn new(b: &'static Bundle) -> Self {
        let opt: &'static Optimizer<'static> = Box::leak(Box::new(Optimizer::new(b)));
        let label = |i: &ens_data::Item| {
            let lv = i.levels.as_ref().map(|l| if l.min == l.max && l.min > 0 { format!(" L{}", l.min) } else { String::new() }).unwrap_or_default();
            format!("{}{}", i.name, lv)
        };
        let pick = |f: &dyn Fn(&ens_data::Item) -> bool| {
            let mut v: Vec<Pick> = b.items.iter().filter(|(_, i)| !i.name.is_empty() && f(i))
                .map(|(g, i)| Pick { guid: g.clone(), label: label(i), rarity: i.rarity.clone().unwrap_or_default() }).collect();
            v.sort_by(|a, b| a.label.cmp(&b.label).then(a.rarity.cmp(&b.rarity)));
            v.dedup_by(|a, b| a.label == b.label && a.rarity == b.rarity);
            v
        };
        let weapons = pick(&|i| i.category.as_deref() == Some("Weapons") && i.damage.is_some());
        let ammo = pick(&|i| matches!(i.slot.as_deref(), Some("Arrow") | Some("Spell")) || i.ammo.as_deref() == Some("Arrow") && i.category.as_deref() == Some("Ammunition"));
        let gear_lists = ens_calc::build::GEAR_SLOTS.iter().map(|slot| pick(&|i| i.slot.as_deref() == Some(*slot))).collect();
        let foods = pick(&|i| i.buff.as_ref().and_then(|x| x.slot.as_deref()) == Some("Food"));
        let gem_list = pick(&|i| i.category.as_deref() == Some("WeaponGem"));
        let summon_list = pick(&|i| ens_calc::is_summon_spell(b, i));
        // enemies: every hostile template (faction, class, flying, susceptibility from the game data). Faction names as
        // the game shows them come from the "Damage against …" perks that target each faction's mod.
        let mut faction_label: HashMap<String, String> = HashMap::new();
        for p in b.perks.values() {
            if let (Some(rest), [e]) = (p.name.strip_prefix("Damage against "), p.effects.as_slice()) {
                if let Some(a) = &e.attribute {
                    faction_label.entry(a.clone()).or_insert_with(|| rest.to_string());
                }
            }
        }
        let mut enemies = vec![EnemyPick { label: "Training dummy (no resistances)".into(), name: None, sus: HashMap::new(), faction: None, class: None, flying: false }];
        let mut list: Vec<EnemyPick> = b.enemy_types.iter().map(|(name, t)| {
            let fac = ens_calc::faction_against_attribute(&t.faction).and_then(|a| faction_label.get(a).cloned()).unwrap_or_else(|| t.faction.clone());
            let pretty = name.trim_start_matches("Base_").trim_start_matches("Enemy_").replace('_', " ");
            let class = match t.class.as_str() { "Ranger" => "ranged", "Mage" => "mage", _ => "melee" };
            EnemyPick {
                label: format!("{fac} · {pretty} ({class}{}{})", if t.flying { ", flying" } else { "" }, if t.boss { ", boss" } else { "" }),
                name: Some(name.clone()),
                sus: t.susceptibility.clone(),
                faction: Some(t.faction.clone()),
                class: Some(t.class.clone()),
                flying: t.flying,
            }
        }).collect();
        list.sort_by(|a, b| a.label.cmp(&b.label));
        enemies.extend(list);
        let shot = std::env::var("ENS_SHOT").ok();
        let mut build = if shot.is_some() { Build::sample_bow(b) } else { Build::new(b, 30) };
        build.name = "Endgame".into();
        if shot.is_none() {
            build.flame_level = 6;
        }
        let mut leveling = build.clone();
        leveling.name = "Leveling".into();
        leveling.level = 25;
        leveling.ranks = vec![0; b.skills.len()];
        let tab = match std::env::var("ENS_TAB").as_deref() {
            Ok("tree") => Tab::Tree,
            Ok("equipment") | Ok("gear") => Tab::Gear,
            Ok("setup") => Tab::Setup,
            Ok("calcs") => Tab::Calcs,
            Ok("optimizer") => Tab::Optimizer,
            Ok("notes") => Tab::Notes,
            _ => Tab::Overview,
        };
        App {
            b,
            engine: Engine::new(b),
            opt,
            snapshot: build.clone(),
            loadouts: vec![leveling, build.clone()],
            current: 1,
            build,
            build_name: if shot.is_some() { "Frost Ranger".into() } else { "My build".into() },
            undo: vec![],
            redo: vec![],
            tab,
            weapons,
            ammo,
            gear_lists,
            foods,
            summon_list,
            gem_list,
            enemies,
            enemy_filter: String::new(),
            enemy: 0,
            target_armor: (0.0, 0.0),
            enemy_level: 50,
            enemy_hp_factor: 1.0,
            pack: (1.0, 4.5),
            orb_pickup: 0.5,
            incoming_type: "Pierce".into(),
            hit_kind: "Normal".into(),
            weak_spot: false,
            soaked: false,
            play: ens_calc::Play::default(),
            pan: Vec2::ZERO,
            zoom: 0.11,
            hovered: None,
            selected: None,
            search: String::new(),
            focus_search: false,
            heatmap: false,
            heat: None,
            heat_rx: None,
            slot: Slot::Weapon,
            cand: None,
            cand_rx: None,
            heat_gen: Arc::new(AtomicU64::new(0)),
            cand_gen: Arc::new(AtomicU64::new(0)),
            cand_hover: None,
            summary_cache: None,
            preview_cache: None,
            filter: String::new(),
            ranking: vec![Goal::Damage, Goal::Survival, Goal::Movement, Goal::Sustain, Goal::Utility],
            opt_gear: true,
            gear_rarity: "Legendary".into(),
            running: None,
            last_opt: None,
            opt_summary: None,
            order_step: 0,
            code: String::new(),
            status: String::new(),
            sheet_query: String::new(),
            sheet_all: false,
            notes: String::new(),
            scale: 1.0,
            shot,
            gpu: String::new(),
            last_frame: None,
            last_rect: None,
            rec: diag::Recorder::new(),
            frames: 0,
        }
    }

    /// The picked enemy's own health factor (its template's, 1 for the training dummy) times the multiplier setting.
    fn hp_factor(&self) -> f64 {
        let e = &self.enemies[self.enemy];
        e.name.as_ref().and_then(|n| self.b.enemy_types.get(n)).and_then(|t| t.health_factor).unwrap_or(1.0) * self.enemy_hp_factor
    }

    fn target(&self) -> Target {
        let e = &self.enemies[self.enemy];
        Target {
            susceptibility: e.sus.clone(),
            armor_physical: self.target_armor.0,
            armor_magical: self.target_armor.1,
            faction: e.faction.clone(),
            enemy_type: e.name.clone(),
            enemy_class: e.class.clone(),
            flying: e.flying,
            pack_size: self.pack.0,
            pack_radius: self.pack.1,
            enemy_health: ens_calc::enemy_health(self.b, self.enemy_level, self.hp_factor()),
            orb_pickup: self.orb_pickup,
            incoming_type: Some(self.incoming_type.clone()),
            hit_kind: if self.hit_kind == "Normal" { None } else { Some(self.hit_kind.clone()) },
            hit_share: None,
            weak_spot: self.weak_spot,
            soaked: self.soaked,
            play: self.play.clone(),
        }
    }

    fn conditions(&self) -> Conditions {
        Conditions { target: self.target(), enemy_level: self.enemy_level, hp_factor: self.hp_factor() }
    }

    fn summary(&mut self) -> Summary {
        let c = self.conditions();
        let k = key_of(&self.build, &c);
        if self.summary_cache.as_ref().map(|(x, _)| x != &k).unwrap_or(true) {
            self.summary_cache = Some((k, summarize(&self.engine, &self.build, &c)));
        }
        self.summary_cache.as_ref().unwrap().1.clone()
    }

    /// The change being previewed: the hovered tree node (allocate its path, or remove a rank if allocated and
    /// full) or the hovered candidate on the Gear tab.
    fn preview_build(&self) -> Option<(Build, String)> {
        if let Some(g) = &self.cand_hover {
            let name = self.b.items.get(g).map(|i| i.name.clone()).unwrap_or_default();
            let verb = if matches!(self.slot, Slot::Food | Slot::Summon) { "Add" } else { "Equip" };
            return Some((apply_slot(self.b, &self.build, self.slot, g), format!("{verb} {name}")));
        }
        let i = self.hovered?;
        let s = &self.b.skills[i];
        if let Some(p) = path_to(self.opt, &self.build.ranks, i) {
            let pts: u32 = p.iter().map(|&j| self.b.skills[j].cost).sum();
            let what = if p.len() > 1 { format!("Allocate {} ({} nodes, {} points)", s.name, p.len(), pts) } else { format!("Add a rank of {} ({} points)", s.name, pts) };
            return Some((with_path(&self.build, &p), what));
        }
        if self.build.ranks[i] > 0 && self.can_remove(i) {
            let mut nb = self.build.clone();
            nb.ranks[i] -= 1;
            return Some((nb, format!("Remove a rank of {}", s.name)));
        }
        None
    }

    fn preview(&mut self) -> Option<(Summary, String)> {
        let (nb, what) = self.preview_build()?;
        let c = self.conditions();
        let k = key_of(&nb, &c);
        if self.preview_cache.as_ref().map(|(x, _)| x != &k).unwrap_or(true) {
            self.preview_cache = Some((k, summarize(&self.engine, &nb, &c)));
        }
        Some((self.preview_cache.as_ref().unwrap().1.clone(), what))
    }

    fn total_ranks(&self, name: &str) -> u32 {
        self.b.skills.iter().enumerate().filter(|(_, s)| s.name == name).map(|(i, _)| self.build.ranks[i] as u32).sum()
    }

    fn can_remove(&self, i: usize) -> bool {
        if self.build.ranks[i] == 0 {
            return false;
        }
        if self.build.ranks[i] > 1 {
            return true;
        }
        let mut t = self.build.ranks.clone();
        t[i] = 0;
        self.opt.connected(&t)
    }

    fn switch_loadout(&mut self, k: usize) {
        if k == self.current {
            return;
        }
        self.loadouts[self.current] = self.build.clone();
        self.current = k;
        self.build = self.loadouts[k].clone();
        self.snapshot = self.build.clone();
        self.undo.clear();
        self.redo.clear();
    }

    fn item_name(&self, guid: &str) -> String {
        self.b.items.get(guid).map(|i| i.name.clone()).unwrap_or_default()
    }

    fn weapon_kind(&self, guid: &str) -> Option<String> {
        self.b.items.get(guid).and_then(|i| i.weapon_category.as_ref()).and_then(|c| self.b.weapon_categories.get(c)).and_then(|c| c.kind.clone())
    }

    fn equipped(&self, slot: Slot) -> Option<String> {
        match slot {
            Slot::Weapon => self.build.weapon.as_ref().map(|e| e.item.clone()),
            Slot::Ammo => self.build.ammo.clone(),
            Slot::Gem => self.build.gem.as_ref().map(|e| e.item.clone()),
            Slot::Armor(k) => self.build.gear[k].as_ref().map(|e| e.item.clone()),
            Slot::Food => self.build.foods.first().cloned(),
            Slot::Summon => self.build.summons.first().cloned(),
        }
    }

    fn gem_named(&self, word: &str) -> bool {
        self.build.gem.as_ref().map(|g| self.item_name(&g.item).contains(word)).unwrap_or(false)
    }

    fn do_act(&mut self, a: Act) {
        match a {
            Act::Tab(t) => self.tab = t,
            Act::Slot(s) => {
                self.tab = Tab::Gear;
                self.slot = s;
            }
            Act::Ammo(g) => self.build.ammo = Some(g),
        }
    }

    // ---------------------------------------------------------------- top bar and tabs

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("UNSHROUDED").font(heading_font(19.0)).color(HEAD));
            ui.add_space(6.0);
            ui.separator();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("BUILD").size(11.0).color(TEXT2));
                ui.add(egui::TextEdit::singleline(&mut self.build_name).frame(egui::Frame::NONE).font(bold_font(16.0)).desired_width(130.0));
            });
            let mut switch = None;
            for (k, l) in self.loadouts.iter().enumerate() {
                let on = k == self.current;
                let (name, lvl) = if on { (&self.build.name, self.build.level) } else { (&l.name, l.level) };
                let btn = egui::Button::new(RichText::new(format!("{name} · L{lvl}")).color(if on { HEAD } else { TEXT2 }).font(if on { bold_font(14.5) } else { FontId::proportional(14.5) }))
                    .fill(if on { HILITE } else { Color32::TRANSPARENT })
                    .stroke(Stroke::new(1.0, if on { FLAME } else { BORDER }))
                    .corner_radius(6);
                if ui.add(btn).on_hover_text("Switch loadout (each keeps its own tree, gear and level)").clicked() {
                    switch = Some(k);
                }
            }
            if let Some(k) = switch {
                self.switch_loadout(k);
            }
            if ui.add(ghost("+")).on_hover_text("New loadout (a copy of this one)").clicked() {
                self.loadouts[self.current] = self.build.clone();
                let mut nb = self.build.clone();
                nb.name = format!("Loadout {}", self.loadouts.len() + 1);
                self.loadouts.push(nb);
                let k = self.loadouts.len() - 1;
                self.switch_loadout(k);
            }
            ui.add(egui::TextEdit::singleline(&mut self.build.name).desired_width(84.0)).on_hover_text("Rename this loadout");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary("Share build")).on_hover_text("Copy this loadout's build code").clicked() {
                    self.code = self.build.to_code_for(self.b);
                    ui.ctx().copy_text(self.code.clone());
                    self.status = "Build code copied to the clipboard.".into();
                }
                if ui.add_enabled(!self.redo.is_empty(), ghost("↷")).on_hover_text("Redo (Ctrl+Y)").clicked() {
                    self.do_redo();
                }
                if ui.add_enabled(!self.undo.is_empty(), ghost("↶")).on_hover_text("Undo (Ctrl+Z)").clicked() {
                    self.do_undo();
                }
                ui.add_space(8.0);
                let spent = self.build.points(self.b);
                let have = points_available(self.build.level, self.build.found_points);
                ui.label(RichText::new(format!("/ {have}")).color(TEXT2));
                ui.label(RichText::new(format!("{spent}")).font(bold_font(15.5)).color(if spent > have { BAD } else { TEXT }));
                ui.label(RichText::new("Points").color(TEXT2));
                ui.add_space(6.0);
                ui.add(egui::DragValue::new(&mut self.build.flame_level).range(1..=ens_calc::build::FLAME_LEVEL_CAP));
                ui.label(RichText::new("Flame").color(TEXT2));
                ui.add_space(6.0);
                ui.add(egui::DragValue::new(&mut self.build.level).range(1..=ens_calc::build::level_cap(self.b)));
                ui.label(RichText::new("Level").color(TEXT2));
            });
        });
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (t, name) in [(Tab::Overview, "OVERVIEW"), (Tab::Tree, "SKILL TREE"), (Tab::Gear, "GEAR"), (Tab::Setup, "COMBAT SETUP"), (Tab::Calcs, "CALCULATIONS"), (Tab::Optimizer, "OPTIMIZER"), (Tab::Notes, "NOTES")] {
                let on = self.tab == t;
                let r = ui.add(egui::Button::new(RichText::new(name).font(heading_font(14.0)).color(if on { HEAD } else { TEXT2 })).fill(Color32::TRANSPARENT).stroke(Stroke::NONE).min_size(Vec2::new(0.0, 34.0)));
                if on {
                    let rr = r.rect;
                    ui.painter().line_segment([rr.left_bottom() + Vec2::new(8.0, 4.0), rr.right_bottom() + Vec2::new(-8.0, 4.0)], Stroke::new(2.0, FLAME));
                }
                if r.clicked() {
                    self.tab = t;
                }
            }
        });
    }

    fn do_undo(&mut self) {
        if let Some(p) = self.undo.pop() {
            self.redo.push(self.build.clone());
            self.build = p;
            self.snapshot = self.build.clone();
        }
    }

    fn do_redo(&mut self) {
        if let Some(n) = self.redo.pop() {
            self.undo.push(self.build.clone());
            self.build = n;
            self.snapshot = self.build.clone();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let (undo, redo, find, up, down, reset) = ctx.input(|i| {
            let c = i.modifiers.command;
            (
                c && !i.modifiers.shift && i.key_pressed(Key::Z),
                c && (i.key_pressed(Key::Y) || i.modifiers.shift && i.key_pressed(Key::Z)),
                c && i.key_pressed(Key::F),
                c && (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus)),
                c && i.key_pressed(Key::Minus),
                c && i.key_pressed(Key::Num0),
            )
        });
        if undo && !ctx.egui_wants_keyboard_input() {
            self.do_undo();
        }
        if redo && !ctx.egui_wants_keyboard_input() {
            self.do_redo();
        }
        if find {
            self.tab = Tab::Tree;
            self.focus_search = true;
        }
        if up || down || reset {
            self.scale = if reset { 1.0 } else { (self.scale + if up { 0.1 } else { -0.1 }).clamp(0.7, 2.0) };
            ctx.set_zoom_factor(self.scale);
        }
        // record history once a change settles (not on every slider tick)
        if self.build != self.snapshot && !ctx.input(|i| i.pointer.any_down()) {
            self.undo.push(std::mem::replace(&mut self.snapshot, self.build.clone()));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    // ---------------------------------------------------------------- left rail

    fn rail(&mut self, ui: &mut egui::Ui) {
        let now = self.summary();
        let prev = self.preview();
        let p = prev.as_ref().map(|x| &x.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(14.0);
            eyebrow(ui, "Total DPS · 60 s fight");
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{:.1}", now.dps)).font(heading_font(40.0)).color(HEAD));
                if let Some(d) = p.and_then(|p| delta(now.dps, p.dps, true, |d| format!("{d:+.1}"))) {
                    ui.label(d);
                }
            });
            match &prev {
                Some((_, what)) => {
                    ui.label(RichText::new(format!("Previewing: {what}")).size(13.0).color(TEAL));
                }
                None => {
                    ui.label(RichText::new("Hover a node or item to preview it").size(13.0).color(TEXT3));
                }
            }
            ui.add_space(12.0);
            // values with their preview change on the same line
            let line = |ui: &mut egui::Ui, label: &str, value: String, color: Color32, change: Option<RichText>| {
                let r = ui.horizontal(|ui| {
                    ui.label(RichText::new(label).color(TEXT2));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(c) = change {
                            ui.label(c);
                        }
                        ui.label(RichText::new(value).font(bold_font(15.5)).color(color));
                    });
                }).response;
                ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, Stroke::new(1.0, LINE));
                ui.add_space(5.0);
            };
            line(ui, "Expected hit", format!("{:.1}", now.expected_hit), TEXT, p.and_then(|p| delta(now.expected_hit, p.expected_hit, true, |d| format!("{d:+.1}"))));
            line(ui, "Time to kill", if now.ttk.is_finite() { format!("{:.1} s", now.ttk) } else { "—".into() }, TEXT,
                 p.and_then(|p| if now.ttk.is_finite() && p.ttk.is_finite() { delta(now.ttk, p.ttk, false, |d| format!("{d:+.1}")) } else { None }));
            line(ui, "Crit chance", format!("{:.1}%", now.crit * 100.0), TEXT, p.and_then(|p| delta(now.crit * 100.0, p.crit * 100.0, true, |d| format!("{d:+.1}"))));
            let sc = if now.hits_survived < 1.0 { BAD } else if now.hits_survived < 2.0 { WARN } else { TEXT };
            line(ui, "Hits survived", format!("{:.1}", now.hits_survived), sc, p.and_then(|p| delta(now.hits_survived, p.hits_survived, true, |d| format!("{d:+.1}"))));
            if now.has_mana_cost {
                let mc = if now.mana_uptime < 0.6 { WARN } else { TEXT };
                line(ui, "Mana uptime", format!("{:.0}%", now.mana_uptime * 100.0), mc, p.and_then(|p| delta(now.mana_uptime * 100.0, p.mana_uptime * 100.0, true, |d| format!("{d:+.0}"))));
            }
            line(ui, "Health", format!("{:.0}", now.health), TEXT, p.and_then(|p| delta(now.health, p.health, true, |d| format!("{d:+.0}"))));
            ui.add_space(14.0);
            if self.tab == Tab::Tree {
                eyebrow(ui, "Points by region");
                for c in ["Ranger", "Warrior", "Mage"] {
                    let pts: u32 = self.b.skills.iter().enumerate().filter(|(_, s)| s.class == c).map(|(i, s)| self.build.ranks[i] as u32 * s.cost).sum();
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 5.0, class_color(c));
                        ui.label(RichText::new(c).color(if pts > 0 { TEXT } else { TEXT3 }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(pts.to_string()).font(bold_font(15.0)).color(if pts > 0 { TEXT } else { TEXT3 }));
                        });
                    });
                }
                ui.add_space(14.0);
            }
            if self.tab != Tab::Setup {
                card_with(ui, RAISED, BORDER, |ui| {
                    eyebrow(ui, "Fighting");
                    let e = &self.enemies[self.enemy];
                    ui.label(RichText::new(format!("{} · L{}", e.label, self.enemy_level)).font(bold_font(15.0)));
                    let group = if self.pack.0 > 1.0 { format!("{:.0} enemies within {:.1} m", self.pack.0, self.pack.1) } else { "Single target".into() };
                    ui.label(RichText::new(group).size(13.0).color(TEXT2));
                    if ui.link("Change in Combat setup").clicked() {
                        self.tab = Tab::Setup;
                    }
                });
            }
        });
    }

    // ---------------------------------------------------------------- overview tab

    fn insights(&self, s: &Summary) -> Vec<Insight> {
        let mut out = vec![];
        let spent = self.build.points(self.b);
        let have = points_available(self.build.level, self.build.found_points);
        if self.build.weapon.is_none() {
            out.push(Insight { warn: true, title: "No weapon equipped".into(), body: "Pick a weapon to see how much damage this build does.".into(), action: Some(("Pick a weapon".into(), Act::Slot(Slot::Weapon))) });
        }
        if spent > have {
            out.push(Insight { warn: true, title: format!("{} points over budget", spent - have), body: format!("Level {} gives {have} skill points; this tree spends {spent}.", self.build.level), action: Some(("Open the skill tree".into(), Act::Tab(Tab::Tree))) });
        }
        if s.has_mana_cost && s.mana_uptime < 0.99 {
            let ammo = self.build.ammo.as_deref().map(|g| self.item_name(g)).unwrap_or_else(|| "Your attack".into());
            out.push(Insight {
                warn: true,
                title: format!("Mana runs out after {:.0} s", s.mana_uptime * 60.0),
                body: format!("{ammo} needs {:.1} mana/s and you regenerate {:.1}/s, so it is up {:.0}% of a 60 s fight.", s.mana_per_s, s.mana_regen, s.mana_uptime * 100.0),
                action: Some(("See the mana budget".into(), Act::Tab(Tab::Calcs))),
            });
        }
        if let Some((n, fd)) = &s.fallback {
            if *fd > s.attacking_dps + 0.05 {
                let guid = self.b.items.iter().filter(|(_, i)| &i.name == n && i.category.as_deref() == Some("Ammunition"))
                    .filter(|(_, i)| i.levels.as_ref().map(|l| l.min <= self.build.level).unwrap_or(true)).map(|(g, _)| g.clone()).min();
                let gain = fd - s.dps;
                out.push(Insight {
                    warn: true,
                    title: format!("{n} alone would do more"),
                    body: format!("{n}: {fd:.1} DPS with no mana. Your mana attack: {:.1} DPS while it lasts.", s.attacking_dps),
                    action: guid.map(|g| (format!("Switch to {n} ({gain:+.1})"), Act::Ammo(g))),
                });
            }
        }
        if s.hits_survived < 2.0 && s.health > 0.0 {
            out.push(Insight {
                warn: s.hits_survived < 1.0,
                title: if s.hits_survived < 1.0 { "One hit can kill you".into() } else { "Two hits can kill you".into() },
                body: format!("This enemy's biggest hit deals {:.0} after your {:.0} physical and {:.0} magical armour; your {:.0} health survives {:.1} of them.", s.hit_taken, s.armor_phys, s.armor_mag, s.health, s.hits_survived),
                action: self.build.gear.iter().position(|g| g.is_none()).filter(|k| *k < 5).map(|k| ("Fill armour slots".into(), Act::Slot(Slot::Armor(k)))),
            });
        }
        let empty: Vec<&str> = ARMOR_SLOTS.iter().enumerate().filter(|(k, _)| self.build.gear[*k].is_none()).map(|(_, n)| *n).collect();
        let gem_empty = self.build.gem.is_none() && self.build.weapon.is_some();
        if !empty.is_empty() || gem_empty {
            let n = empty.len() + gem_empty as usize;
            let mut names: Vec<String> = empty.iter().map(|s| s.to_lowercase()).collect();
            if gem_empty {
                names.push("weapon gem".into());
            }
            let first = if let Some(k) = self.build.gear.iter().position(|g| g.is_none()) { Slot::Armor(k) } else { Slot::Gem };
            out.push(Insight { warn: false, title: format!("{n} gear slot{} empty", if n == 1 { " is" } else { "s are" }), body: format!("Empty: {}. The Gear tab ranks every item for a slot by what it adds.", names.join(", ")), action: Some(("Fill gear slots".into(), Act::Slot(first))) });
        }
        if have > spent {
            out.push(Insight { warn: false, title: format!("{} skill point{} unspent", have - spent, if have - spent == 1 { "" } else { "s" }), body: "The optimizer can place them for your goals.".into(), action: Some(("Open the optimizer".into(), Act::Tab(Tab::Optimizer))) });
        }
        if self.build.foods.len() < 3 {
            out.push(Insight { warn: false, title: format!("{} food slot{} unused", 3 - self.build.foods.len(), if self.build.foods.len() == 2 { "" } else { "s" }), body: "Food adds stats for the whole fight.".into(), action: Some(("Add food".into(), Act::Slot(Slot::Food))) });
        }
        out.sort_by_key(|i| !i.warn);
        out
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        let s = self.summary();
        let insights = self.insights(&s);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(20.0);
            let w = ui.available_width();
            let lw = ((w - 20.0) * 0.6).max(420.0);
            let mut act = None;
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(lw);
                    self.sources_card(ui, &s);
                    ui.add_space(16.0);
                    self.hit_chain_card(ui);
                    ui.add_space(16.0);
                    self.survival_card(ui, &s);
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.set_width((w - lw - 20.0).max(300.0));
                    card(ui, |ui| {
                        title(ui, "What's holding you back");
                        ui.add_space(6.0);
                        if insights.is_empty() {
                            ui.label(RichText::new("Nothing obvious. Try the skill-tree heatmap or the optimizer to squeeze out more.").color(TEXT2));
                        }
                        for (k, ins) in insights.into_iter().enumerate() {
                            let (fill, stroke, badge) = if ins.warn { (WARN_BG, WARN_LINE, WARN) } else { (RAISED, BORDER, BORDER_STRONG) };
                            egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(8).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_top(|ui| {
                                    let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                                    ui.painter().circle_filled(r.center(), 13.0, badge);
                                    ui.painter().text(r.center(), Align2::CENTER_CENTER, (k + 1).to_string(), bold_font(14.0), if ins.warn { BG1 } else { TEXT });
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(&ins.title).font(bold_font(16.0)));
                                        ui.add(egui::Label::new(RichText::new(&ins.body).color(TEXT_SOFT)).wrap());
                                        if let Some((label, a)) = ins.action {
                                            if ui.add(outline(&label)).clicked() {
                                                act = Some(a);
                                            }
                                        }
                                    });
                                });
                            });
                            ui.add_space(8.0);
                        }
                    });
                });
            });
            if let Some(a) = act {
                self.do_act(a);
            }
            ui.add_space(20.0);
        });
    }

    fn sources_card(&mut self, ui: &mut egui::Ui, s: &Summary) {
        card(ui, |ui| {
            title_with(ui, "Where your damage comes from", RichText::new(format!("{:.1} DPS", s.dps)).color(TEXT2));
            ui.add_space(8.0);
            let total: f64 = s.sources.iter().map(|x| x.dps.max(0.0)).sum();
            let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
            ui.painter().rect_filled(bar, 7.0, LINE);
            if total > 0.0 {
                let mut x = bar.left();
                for (k, src) in s.sources.iter().enumerate() {
                    let wpx = (src.dps.max(0.0) / total) as f32 * bar.width();
                    if wpx <= 0.5 {
                        continue;
                    }
                    let r = egui::Rect::from_min_size(egui::pos2(x, bar.top()), Vec2::new(wpx, bar.height()));
                    ui.painter().rect_filled(r, 0.0, SOURCE_COLORS[k.min(SOURCE_COLORS.len() - 1)]);
                    x += wpx;
                }
            }
            ui.add_space(10.0);
            if s.sources.is_empty() {
                ui.label(RichText::new("No damage yet: equip a weapon or a summon.").color(TEXT2));
            }
            for (k, src) in s.sources.iter().enumerate() {
                let r = ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().rect_filled(dot.shrink(1.0), 2.0, SOURCE_COLORS[k.min(SOURCE_COLORS.len() - 1)]);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(&src.name).font(bold_font(15.5)));
                        ui.label(RichText::new(&src.note).size(13.0).color(TEXT2));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_sized([56.0, 20.0], egui::Label::new(RichText::new(if total > 0.0 { format!("{:.0}%", src.dps / total * 100.0) } else { "—".into() }).color(TEXT2)));
                        ui.label(RichText::new(format!("{:.1}", src.dps)).font(bold_font(16.0)));
                    });
                }).response;
                ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 4.0, Stroke::new(1.0, LINE));
                ui.add_space(8.0);
            }
        });
    }

    fn hit_chain_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            let mut go = false;
            ui.horizontal(|ui| {
                title(ui, "How one hit is built");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    go = ui.link("Full breakdown").clicked();
                });
            });
            if go {
                self.tab = Tab::Calcs;
            }
            ui.add_space(8.0);
            let Some(w) = self.build.weapon.clone() else {
                ui.label(RichText::new("Pick a weapon on the Gear tab to see how a hit is built.").color(TEXT2));
                return;
            };
            let t = self.target();
            let h = self.engine.hit(&self.build, &w, self.build.ammo.as_deref(), 1.0, &t);
            let (secs, how) = self.engine.attack_interval(&self.build, &w, self.build.ammo.as_deref());
            let d = self.engine.dps(&self.build, &w, self.build.ammo.as_deref(), &t, 1.0 / secs.max(0.05));
            let mut chain = vec![];
            let wname = self.item_name(&w.item);
            if h.ammo_damage > 0.0 {
                let aname = self.build.ammo.as_deref().map(|g| self.item_name(g)).unwrap_or_default();
                chain.push(Block::new("Weapon + ammo", trim_num(h.item_damage + h.ammo_damage)).tip(format!("{wname} {} + {aname} {}", trim_num(h.item_damage), trim_num(h.ammo_damage))));
            } else {
                chain.push(Block::new("Weapon", trim_num(h.item_damage)).tip(wname));
            }
            let mut pool = 1.0;
            let mut pool_tip = vec![];
            for (k, v) in &h.pool_terms {
                if let Some(name) = k.strip_prefix('×') {
                    let name = name.trim();
                    let label = if name.contains("scale") { name.replace(" scale", "").replace(['(', ')'], "") } else { attr_label(name) };
                    chain.push(Block::new(label, format!("{v:.2}")).op("×").tip(name.to_string()));
                } else if *v != 0.0 {
                    pool += v;
                    pool_tip.push(format!("{} {:+.0}%", attr_label(k.trim_start_matches('+').trim()), v * 100.0));
                }
            }
            if !pool_tip.is_empty() {
                chain.push(Block::new("Damage bonuses", format!("{pool:.2}")).op("×").tip(pool_tip.join("\n")));
            }
            let types: Vec<String> = h.per_type.iter().filter(|(_, v)| *v > 0.05).map(|(k, v)| format!("{v:.1} {k}")).collect();
            chain.push(Block::new("By damage type", types.join(" + ")).op("→"));
            if h.crit_chance > 0.0 && h.non_crit > 0.0 {
                chain.push(Block::new(format!("Crit {:.0}%", h.crit_chance * 100.0), format!("{:.3}", h.expected / h.non_crit)).op("×").tip(format!("Each crit adds {:.0}% to the damage pool", (h.crit_damage_mod - 1.0) * 100.0)));
            }
            if h.expected > 0.0 && (h.after_armor / h.expected - 1.0).abs() > 1e-4 {
                chain.push(Block::new("Enemy armour", format!("{:.3}", h.after_armor / h.expected)).op("×"));
            }
            chain.push(Block::new("Expected hit", format!("{:.1}", h.after_armor)).op("=").strong());
            blocks(ui, chain, None);
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{:.2} projectiles per attack", d.projectiles)).color(TEXT2));
                ui.label(RichText::new("·").color(TEXT3));
                ui.label(RichText::new(format!("one attack every {secs:.2} s ({how})")).color(TEXT2));
                ui.label(RichText::new("·").color(TEXT3));
                ui.label(RichText::new(format!("{:.1} DPS while attacking", d.total)).font(bold_font(15.0)));
            });
        });
    }

    fn survival_card(&mut self, ui: &mut egui::Ui, s: &Summary) {
        card(ui, |ui| {
            title_with(ui, "Survival", RichText::new(format!("against {} hits", self.incoming_type)).color(TEXT2));
            ui.add_space(8.0);
            let sc = if s.hits_survived < 1.0 { BAD } else if s.hits_survived < 2.0 { WARN } else { GOOD };
            blocks(ui, vec![
                Block::new("Health", format!("{:.0}", s.health)),
                Block::new("Physical armour", format!("{:.0}", s.armor_phys)),
                Block::new("Magical armour", format!("{:.0}", s.armor_mag)),
                Block::new("Biggest hit taken", format!("{:.0}", s.hit_taken)),
                Block::new("Hits survived", format!("{:.1}", s.hits_survived)).color(sc),
                Block::new("Health regen", format!("{:.1} /s", s.health_regen)),
            ], Some(3));
            if s.hit_raw > s.hit_taken + 0.5 {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Armour and resistances cut that hit from {:.0} to {:.0}.", s.hit_raw, s.hit_taken)).size(13.5).color(TEXT2));
            }
        });
    }

    // ---------------------------------------------------------------- skill tree tab

    fn tree_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let hits = if self.search.len() >= 2 { self.b.skills.iter().filter(|s| self.matches(s)).count() } else { 0 };
            let r = ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search skills (Ctrl+F)").desired_width(260.0));
            if self.focus_search {
                r.request_focus();
                self.focus_search = false;
            }
            if self.search.len() >= 2 {
                ui.label(RichText::new(format!("{hits} found")).color(TEAL));
            }
            ui.add_space(8.0);
            let mut heat = self.heatmap;
            segmented(ui, &mut heat, &[(false, "Region colours"), (true, "DPS per point")]);
            self.heatmap = heat;
            if self.heatmap && self.heat_rx.is_some() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(ghost("Reset tree")).clicked() {
                    self.build.ranks.iter_mut().for_each(|r| *r = 0);
                }
            });
        });
    }

    fn matches(&self, s: &ens_data::SkillNode) -> bool {
        if self.search.len() < 2 {
            return false;
        }
        let q = self.search.to_lowercase();
        s.name.to_lowercase().contains(&q)
            || s.desc.iter().any(|d| d.to_lowercase().contains(&q))
            || s.effects.iter().any(|e| e.attribute.as_deref().map(|a| a.to_lowercase().contains(&q) || attr_label(a).to_lowercase().contains(&q)).unwrap_or(false))
    }

    fn poll_heat(&mut self, dragging: bool) {
        let c = self.conditions();
        let k = key_of(&self.build, &c);
        if let Some((rk, rx)) = &self.heat_rx {
            if let Ok(v) = rx.try_recv() {
                self.heat = Some((rk.clone(), v));
                self.heat_rx = None;
            }
        }
        // no new job while a value is being dragged: it would be stale a frame later
        if dragging || !self.heatmap || self.heat.as_ref().map(|h| h.0 == k).unwrap_or(false) || self.heat_rx.as_ref().map(|h| h.0 == k).unwrap_or(false) {
            return;
        }
        let (tx, rx) = channel();
        let opt = self.opt;
        let build = self.build.clone();
        let job = self.heat_gen.clone();
        let me = job.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let base = opt.engine.total(&build, &c.target).total;
            let v: Vec<f64> = (0..build.ranks.len())
                .map_while(|i| (job.load(Ordering::SeqCst) == me).then_some(i))
                .map(|i| match path_to(opt, &build.ranks, i) {
                    Some(p) if !p.is_empty() => {
                        let pts: u32 = p.iter().map(|&j| opt.bundle.skills[j].cost.max(1)).sum();
                        (opt.engine.total(&with_path(&build, &p), &c.target).total - base) / pts as f64
                    }
                    _ => f64::NAN,
                })
                .collect();
            if v.len() == build.ranks.len() {
                let _ = tx.send(v);
            }
        });
        self.heat_rx = Some((k, rx));
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        let dragging = ui.input(|i| i.pointer.any_down());
        self.poll_heat(dragging);
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(0x12, 0x0D, 0x09));
        for (k, c) in [(0.62, Color32::from_rgb(0x15, 0x10, 0x0B)), (0.42, Color32::from_rgb(0x19, 0x13, 0x0D)), (0.24, Color32::from_rgb(0x1E, 0x16, 0x0F))] {
            painter.circle_filled(rect.center(), rect.height() * k, c);
        }
        if resp.dragged() {
            self.pan += resp.drag_delta() / self.zoom;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (1.0 + scroll * 0.0015)).clamp(0.03, 0.8);
            }
        }
        let skills = &self.b.skills;
        let (mut cx, mut cy, mut n) = (0.0, 0.0, 0.0);
        for s in skills {
            cx += s.x;
            cy += s.y;
            n += 1.0;
        }
        let center = Vec2::new((cx / n) as f32, (cy / n) as f32);
        let to_screen = |x: f64, y: f64| rect.center() + (Vec2::new(x as f32, y as f32) - center + self.pan) * self.zoom;
        let pos: Vec<Pos2> = skills.iter().map(|s| to_screen(s.x, s.y)).collect();

        // region names placed beyond each region's outermost node
        for c in ["Ranger", "Warrior", "Mage"] {
            let pts: Vec<Vec2> = skills.iter().filter(|s| s.class == c).map(|s| Vec2::new(s.x as f32, s.y as f32) - center).collect();
            if pts.is_empty() {
                continue;
            }
            let dir = (pts.iter().fold(Vec2::ZERO, |a, p| a + *p) / pts.len() as f32).normalized();
            let reach = pts.iter().map(|p| p.dot(dir)).fold(0.0, f32::max);
            let at = rect.center() + (dir * (reach + 260.0) + self.pan) * self.zoom;
            painter.text(at, Align2::CENTER_CENTER, c.to_uppercase(), heading_font((180.0 * self.zoom).clamp(13.0, 30.0).round()), class_color(c).gamma_multiply(0.85));
        }

        let preview_path: Vec<usize> = self.hovered.and_then(|i| path_to(self.opt, &self.build.ranks, i)).unwrap_or_default();
        let on_path = |i: usize| preview_path.contains(&i);
        let ranks = &self.build.ranks;
        let live = |i: usize| ranks[i] > 0 || skills[i].kind == "Root";
        for (i, nbrs) in self.opt.adj.iter().enumerate() {
            for &j in nbrs {
                if j > i {
                    let lit = live(i) && live(j) && (ranks[i] > 0 || ranks[j] > 0);
                    let prev = (on_path(i) || on_path(j)) && (on_path(i) || live(i)) && (on_path(j) || live(j));
                    let (w, col) = if lit { (3.0, FLAME) } else if prev { (3.0, TEAL) } else { (1.2, EDGE) };
                    painter.line_segment([pos[i], pos[j]], Stroke::new(w, col));
                }
            }
        }
        let heat = if self.heatmap { self.heat.as_ref().map(|h| &h.1) } else { None };
        let heat_max = heat.map(|h| h.iter().filter(|x| x.is_finite()).fold(0.0f64, |a, b| a.max(*b))).unwrap_or(0.0);
        let mut hovered = None;
        let mouse = resp.hover_pos();
        for (i, s) in skills.iter().enumerate() {
            let col = class_color(&s.class);
            let r = (match s.kind.as_str() {
                "Root" => 60.0,
                "Attribute" => 34.0,
                "ActiveSkill" => 70.0,
                _ => 55.0,
            }) * self.zoom;
            let r = r.max(3.0);
            let rank = ranks[i];
            let mut fill = if s.kind == "Root" { BORDER_STRONG } else if rank > 0 { col } else { BG1 };
            let mut stroke = if s.kind == "Root" { Stroke::new(2.5, FLAME) } else if rank > 0 { Stroke::new(2.0, HEAD) } else { Stroke::new(1.8, col.gamma_multiply(0.8)) };
            if let Some(h) = heat {
                if rank == 0 && h[i].is_finite() && heat_max > 0.0 {
                    let t = (h[i] / heat_max).clamp(0.0, 1.0) as f32;
                    fill = lerp_color(RAISED, FLAME, t.sqrt());
                }
            }
            if on_path(i) {
                fill = if rank > 0 { fill } else { TEAL.gamma_multiply(0.35) };
                stroke = Stroke::new(2.5, TEAL);
            }
            painter.circle(pos[i], r, fill, stroke);
            if self.matches(s) {
                painter.circle_stroke(pos[i], r + 5.0, Stroke::new(2.0, TEAL));
            }
            if self.selected == Some(i) {
                painter.circle_stroke(pos[i], r + 9.0, Stroke::new(1.5, TEXT));
            }
            if self.zoom > 0.08 && s.kind != "Root" && rank > 0 && s.max > 1 {
                painter.text(pos[i], Align2::CENTER_CENTER, format!("{}/{}", rank, s.max), bold_font(11.5), BG0);
            }
            if self.zoom > 0.16 && s.kind != "Attribute" && s.kind != "Root" {
                painter.text(pos[i] + Vec2::new(0.0, r + 8.0), Align2::CENTER_TOP, &s.name, FontId::proportional(12.5), if rank > 0 { HEAD } else { TEXT2 });
            }
            if let Some(m) = mouse {
                if m.distance(pos[i]) <= r.max(6.0) {
                    hovered = Some(i);
                }
            }
        }
        if self.shot.is_some() && hovered.is_none() {
            hovered = self.hovered;
        }
        self.hovered = hovered;
        if let Some(i) = hovered {
            if resp.clicked() {
                self.selected = Some(i);
                if let Some(p) = path_to(self.opt, &self.build.ranks, i) {
                    self.build = with_path(&self.build, &p);
                }
            }
            if resp.secondary_clicked() && self.can_remove(i) {
                self.build.ranks[i] -= 1;
                self.selected = Some(i);
            }
        }
        painter.text(rect.left_bottom() + Vec2::new(16.0, -14.0), Align2::LEFT_BOTTOM, "Click: add a rank (and the path to it) · Right-click: remove · Drag: pan · Scroll: zoom", FontId::proportional(13.0), TEXT3);
    }

    fn skill_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(14.0);
            match self.hovered.or(self.selected) {
                Some(i) => {
                    let s = &self.b.skills[i];
                    let col = class_color(&s.class);
                    card_with(ui, RAISED, FLAME, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&s.name).font(heading_font(19.0)).color(HEAD));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(format!("{} / {}", self.build.ranks[i], s.max.max(1))).font(bold_font(15.0)).color(col));
                            });
                        });
                        ui.label(RichText::new(format!("{} · {} · {} point{} per rank", s.class, kind_label(&s.kind), s.cost, if s.cost == 1 { "" } else { "s" })).size(13.0).color(TEXT2));
                        if self.total_ranks(&s.name) > self.build.ranks[i] as u32 {
                            ui.label(RichText::new(format!("{} ranks across all copies of this skill", self.total_ranks(&s.name))).size(13.0).color(TEXT2));
                        }
                        if let Some(d) = s.desc.first() {
                            ui.add_space(6.0);
                            ui.add(egui::Label::new(RichText::new(d.replace("**", "")).color(TEXT)).wrap());
                        }
                        for e in &s.effects {
                            let attr = e.attribute.as_deref().unwrap_or("?");
                            ui.add_space(6.0);
                            ui.label(RichText::new(attr_label(attr)).size(13.0).color(TEXT2)).on_hover_text(attr);
                            ui.horizontal_wrapped(|ui| {
                                for l in 1..=s.max.max(1) {
                                    let v = e.value.as_ref().map(|v| v.eval(&|a| if a == "Level" { l as f64 } else { 0.0 })).unwrap_or(0.0);
                                    let on = l == self.build.ranks[i] as u32;
                                    egui::Frame::new().fill(if on { HILITE } else { BG1 }).stroke(Stroke::new(1.0, if on { FLAME } else { BORDER })).corner_radius(5).inner_margin(egui::Margin::symmetric(9, 4)).show(ui, |ui| {
                                        ui.label(RichText::new(contribution(attr, &format!("{:?}", e.operation), v)).color(if on { HEAD } else { TEXT }));
                                    });
                                }
                            });
                            if !e.when.is_empty() {
                                ui.label(RichText::new("Only under a condition (see the game's tooltip)").size(13.0).color(TEXT3));
                            }
                        }
                        if let Some(u) = &s.unparsed {
                            ui.add_space(6.0);
                            ui.label(RichText::new(format!("Handled by game systems ({u}); not in the numbers.")).size(13.0).color(TEXT3));
                        }
                        if let (Some(h), true) = (self.heat.as_ref().map(|h| &h.1), self.heatmap) {
                            if h[i].is_finite() {
                                ui.add_space(6.0);
                                ui.label(RichText::new(format!("{:+.2} DPS per point, path included", h[i])).color(TEAL));
                            }
                        }
                    });
                }
                None => {
                    card_with(ui, RAISED, BORDER, |ui| {
                        ui.label(RichText::new("Hover a node to see what it does and how it would change your numbers in the left panel.").color(TEXT2));
                    });
                }
            }
            ui.add_space(14.0);
            eyebrow(ui, "Allocated skills");
            let mut names: Vec<(String, u32, String)> = vec![];
            let mut seen = HashSet::new();
            for (i, s) in self.b.skills.iter().enumerate() {
                if self.build.ranks[i] > 0 && s.kind != "Attribute" && seen.insert(s.name.clone()) {
                    names.push((s.name.clone(), self.total_ranks(&s.name), s.class.clone()));
                }
            }
            if names.is_empty() {
                ui.label(RichText::new("None yet.").color(TEXT3));
            }
            for (n, r, c) in names {
                ui.horizontal(|ui| {
                    let (d, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().circle_filled(d.center(), 4.0, class_color(&c));
                    ui.label(RichText::new(n).color(TEXT_SOFT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(r.to_string()).color(TEXT2));
                    });
                });
            }
        });
    }

    // ---------------------------------------------------------------- gear tab

    fn slot_card(&mut self, ui: &mut egui::Ui, slot: Slot, width: f32) {
        let on = self.slot == slot;
        let (name, sub, rarity) = match slot {
            Slot::Food => {
                let names: Vec<String> = self.build.foods.iter().map(|g| self.item_name(g)).collect();
                (if names.is_empty() { None } else { Some(names.join(", ")) }, format!("{} / 3 slots", self.build.foods.len()), String::new())
            }
            Slot::Summon => {
                let names: Vec<String> = self.build.summons.iter().map(|g| self.item_name(g)).collect();
                (if names.is_empty() { None } else { Some(names.join(", ")) }, format!("{} kept up", self.build.summons.len()), String::new())
            }
            _ => match self.equipped(slot) {
                Some(g) => {
                    let it = self.b.items.get(&g);
                    let (rar, lvl, perks) = match slot {
                        Slot::Weapon => self.build.weapon.as_ref().map(|e| (e.rarity.clone(), e.level, e.perks)).unwrap_or_default(),
                        Slot::Armor(k) => self.build.gear[k].as_ref().map(|e| (e.rarity.clone(), e.level, e.perks)).unwrap_or_default(),
                        Slot::Gem => (None, self.build.gem.as_ref().and_then(|e| e.level), None),
                        _ => (None, None, None),
                    };
                    let rar = rar.or(it.and_then(|i| i.rarity.clone())).unwrap_or_default();
                    let mut bits = vec![];
                    if !rar.is_empty() && slot != Slot::Gem {
                        bits.push(rar.clone());
                    }
                    if matches!(slot, Slot::Weapon | Slot::Armor(_) | Slot::Gem) {
                        bits.push(format!("level {}", lvl.unwrap_or(self.build.level)));
                    }
                    if let (Some(it), Slot::Weapon | Slot::Armor(_)) = (it, slot) {
                        let max = ens_calc::unlocked_perks(it, &if rar.is_empty() { "Legendary".to_string() } else { rar.clone() }, None);
                        if max > 0 {
                            bits.push(format!("{} / {max} perks", perks.unwrap_or(max).min(max)));
                        }
                    }
                    if let (Some(it), Slot::Weapon) = (it, slot) {
                        bits.push(format!("{:.0} damage", ens_calc::item_damage(self.b, it, lvl.unwrap_or(self.build.level))));
                    }
                    (Some(self.item_name(&g)), bits.join(" · "), rar)
                }
                None => (None, "Empty".into(), String::new()),
            },
        };
        let filled = name.is_some();
        let stroke = if on { Stroke::new(1.0, FLAME) } else if filled { Stroke::new(1.0, BORDER) } else { Stroke::new(1.0, BORDER_STRONG) };
        let resp = egui::Frame::new().fill(if on { HILITE } else { BG1 }).stroke(stroke).corner_radius(8).inner_margin(egui::Margin::symmetric(14, 11)).show(ui, |ui| {
            ui.set_width(width - 28.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(slot.name().to_uppercase()).size(12.0).color(TEXT2));
                match &name {
                    Some(n) => ui.add(egui::Label::new(RichText::new(n).font(bold_font(16.0)).color(if rarity.is_empty() { TEXT } else { rarity_color(&rarity) })).truncate()),
                    None => ui.label(RichText::new("Empty").color(TEXT3)),
                };
                if filled {
                    ui.add(egui::Label::new(RichText::new(&sub).size(13.0).color(TEXT_SOFT)).truncate());
                }
            });
        }).response.interact(Sense::click());
        if resp.clicked() {
            self.slot = slot;
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }

    fn gear(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        let w = ui.available_width();
        let lw = 430.0f32.min(w * 0.4);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(lw);
                egui::ScrollArea::vertical().id_salt("slots").show(ui, |ui| {
                    title(ui, "Equipped");
                    ui.add_space(6.0);
                    self.slot_card(ui, Slot::Weapon, lw);
                    ui.add_space(8.0);
                    let half = (lw - 8.0) / 2.0;
                    let pairs: Vec<(Slot, Slot)> = vec![
                        (Slot::Ammo, Slot::Gem),
                        (Slot::Armor(0), Slot::Armor(1)),
                        (Slot::Armor(2), Slot::Armor(3)),
                        (Slot::Armor(4), Slot::Armor(5)),
                        (Slot::Armor(6), Slot::Food),
                    ];
                    for (a, b) in pairs {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            self.slot_card(ui, a, half);
                            self.slot_card(ui, b, half);
                        });
                        ui.add_space(8.0);
                    }
                    self.slot_card(ui, Slot::Summon, lw);
                    ui.add_space(20.0);
                });
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(w - lw - 24.0);
                card(ui, |ui| self.slot_detail(ui));
            });
        });
    }

    fn candidates(&self, slot: Slot) -> Vec<String> {
        let picks: Vec<&Pick> = match slot {
            Slot::Weapon => {
                let kind = self.build.weapon.as_ref().and_then(|w| self.weapon_kind(&w.item));
                self.weapons.iter().filter(|p| kind.is_none() || self.weapon_kind(&p.guid) == kind).collect()
            }
            Slot::Ammo => {
                let bow = self.build.weapon.as_ref().and_then(|w| self.weapon_kind(&w.item)).as_deref() == Some("RangedBow");
                let summons: HashSet<&String> = self.summon_list.iter().map(|p| &p.guid).collect();
                self.ammo.iter().filter(|p| {
                    let it = &self.b.items[&p.guid];
                    let arrow = it.slot.as_deref() == Some("Arrow") || it.ammo.as_deref() == Some("Arrow");
                    arrow == bow && !summons.contains(&p.guid)
                }).collect()
            }
            Slot::Gem => self.gem_list.iter().collect(),
            Slot::Armor(k) => self.gear_lists[k].iter().collect(),
            Slot::Food => self.foods.iter().filter(|p| !self.build.foods.contains(&p.guid)).collect(),
            Slot::Summon => self.summon_list.iter().filter(|p| !self.build.summons.contains(&p.guid)).collect(),
        };
        picks.into_iter().map(|p| p.guid.clone()).collect()
    }

    fn poll_candidates(&mut self, ctx: &egui::Context) {
        let c = self.conditions();
        let k = format!("{:?}|{}", self.slot, key_of(&self.build, &c));
        if let Some((rk, rx)) = &self.cand_rx {
            if let Ok(v) = rx.try_recv() {
                self.cand = Some((rk.clone(), v));
                self.cand_rx = None;
            }
        }
        let fresh = self.cand.as_ref().map(|x| x.0 == k).unwrap_or(false);
        if fresh || self.cand_rx.as_ref().map(|x| x.0 == k).unwrap_or(false) || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let cands = self.candidates(self.slot);
        let slot = self.slot;
        let (tx, rx) = channel();
        let opt = self.opt;
        let build = self.build.clone();
        let ctx = ctx.clone();
        let job = self.cand_gen.clone();
        let me = job.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let n = cands.len().min(400);
            let mut out: Vec<Cand> = cands.into_iter().take(400).map_while(|g| (job.load(Ordering::SeqCst) == me).then_some(g)).map(|g| {
                let nb = apply_slot(opt.bundle, &build, slot, &g);
                let s = summarize(&opt.engine, &nb, &c);
                Cand { guid: g, dps: s.dps, survived: s.hits_survived }
            }).collect();
            if out.len() < n {
                return;
            }
            out.sort_by(|a, b| b.dps.partial_cmp(&a.dps).unwrap_or(std::cmp::Ordering::Equal).then(b.survived.partial_cmp(&a.survived).unwrap_or(std::cmp::Ordering::Equal)));
            // the data has several copies of some items (one per drop source); keep one row per name and result
            let mut seen = HashSet::new();
            out.retain(|x| seen.insert((opt.bundle.items.get(&x.guid).map(|i| i.name.clone()).unwrap_or_default(), (x.dps * 10.0).round() as i64, (x.survived * 100.0).round() as i64)));
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.cand_rx = Some((k, rx));
    }

    fn slot_detail(&mut self, ui: &mut egui::Ui) {
        let slot = self.slot;
        self.poll_candidates(&ui.ctx().clone());
        let now = self.summary();
        // the equipped item and its controls
        ui.horizontal(|ui| {
            title(ui, slot.name());
            if self.cand_rx.is_some() {
                ui.spinner();
            }
        });
        ui.add_space(6.0);
        match slot {
            Slot::Food | Slot::Summon => {
                let list = if slot == Slot::Food { self.build.foods.clone() } else { self.build.summons.clone() };
                let cap = if slot == Slot::Food { 4 } else { 3 };
                ui.label(RichText::new(if slot == Slot::Food { "One food per type; 3 slots, 4 with the Dessert Stomach skill." } else { "Summon spells you keep up; each counts up to its cap and costs mana." }).color(TEXT2));
                ui.horizontal_wrapped(|ui| {
                    let mut remove = None;
                    for (k, g) in list.iter().enumerate() {
                        egui::Frame::new().fill(RAISED).stroke(Stroke::new(1.0, BORDER)).corner_radius(14).inner_margin(egui::Margin::symmetric(12, 5)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(self.item_name(g)).color(TEXT));
                                if ui.add(egui::Button::new(RichText::new("×").color(TEXT2)).frame(false)).on_hover_text("Remove").clicked() {
                                    remove = Some(k);
                                }
                            });
                        });
                    }
                    if let Some(k) = remove {
                        if slot == Slot::Food {
                            self.build.foods.remove(k);
                        } else {
                            self.build.summons.remove(k);
                        }
                    }
                    if list.is_empty() {
                        ui.label(RichText::new("None yet").color(TEXT3));
                    }
                });
                if list.len() >= cap {
                    ui.label(RichText::new("All slots are full; remove one to add another.").size(13.0).color(TEXT3));
                }
            }
            _ => {
                if let Some(g) = self.equipped(slot) {
                    let it = self.b.items.get(&g).cloned();
                    let mut unequip = false;
                    card_with(ui, RAISED, BORDER, |ui| {
                        ui.horizontal(|ui| {
                            let rar = match slot {
                                Slot::Weapon => self.build.weapon.as_ref().and_then(|e| e.rarity.clone()),
                                Slot::Armor(k) => self.build.gear[k].as_ref().and_then(|e| e.rarity.clone()),
                                _ => None,
                            }.or(it.as_ref().and_then(|i| i.rarity.clone())).unwrap_or_default();
                            ui.label(RichText::new(self.item_name(&g)).font(bold_font(18.0)).color(if rar.is_empty() { TEXT } else { rarity_color(&rar) }));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                unequip = ui.add(ghost("Unequip")).clicked();
                            });
                        });
                        // rarity, level and perks for items that have them
                        let e = match slot {
                            Slot::Weapon => self.build.weapon.as_mut(),
                            Slot::Armor(k) => self.build.gear[k].as_mut(),
                            Slot::Gem => self.build.gem.as_mut(),
                            _ => None,
                        };
                        if let (Some(e), Some(it)) = (e, it.as_ref()) {
                            ui.horizontal_wrapped(|ui| {
                                if slot != Slot::Gem {
                                    let mut r = e.rarity.clone().or(it.rarity.clone()).unwrap_or_default();
                                    ui.label(RichText::new("Rarity").color(TEXT2));
                                    egui::ComboBox::from_id_salt("slotrar").selected_text(RichText::new(&r).color(rarity_color(&r))).show_ui(ui, |ui| {
                                        for x in ["Common", "Uncommon", "Rare", "Epic", "Legendary"] {
                                            ui.selectable_value(&mut r, x.to_string(), RichText::new(x).color(rarity_color(x)));
                                        }
                                    });
                                    e.rarity = Some(r);
                                    ui.add_space(8.0);
                                }
                                let lvl_max = if slot == Slot::Gem { 105 } else { 55 };
                                let mut lvl = e.level.unwrap_or(self.build.level);
                                ui.label(RichText::new(if slot == Slot::Gem { "Gem level" } else { "Item level" }).color(TEXT2));
                                ui.add(egui::DragValue::new(&mut lvl).range(1..=lvl_max));
                                e.level = Some(lvl);
                                if slot != Slot::Gem {
                                    let max = ens_calc::unlocked_perks(it, e.rarity.as_deref().or(it.rarity.as_deref()).unwrap_or("Legendary"), None);
                                    if max > 0 {
                                        ui.add_space(8.0);
                                        let mut n = e.perks.unwrap_or(max).min(max);
                                        ui.label(RichText::new("Perks unlocked").color(TEXT2));
                                        ui.add(egui::DragValue::new(&mut n).range(0..=max).suffix(format!(" / {max}")));
                                        e.perks = if n == max { None } else { Some(n) };
                                    }
                                }
                            });
                            if slot != Slot::Gem {
                                let rar = e.rarity.clone().or(it.rarity.clone()).unwrap_or_else(|| "Legendary".into());
                                let n = ens_calc::perk_count(e, &rar);
                                let perks: Vec<&String> = it.perks.iter().filter(|p| !p.is_empty()).collect();
                                if !perks.is_empty() {
                                    ui.add_space(6.0);
                                    for (k, pid) in perks.iter().enumerate() {
                                        let pname = self.b.perks.get(*pid).map(|p| p.name.clone()).unwrap_or_else(|| "Unknown perk".into());
                                        let on = k < n;
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(if on { "◆" } else { "◇" }).color(if on { FLAME } else { TEXT3 }));
                                            ui.label(RichText::new(pname).color(if on { TEXT } else { TEXT3 }));
                                            if !on {
                                                ui.label(RichText::new("locked").size(12.5).color(TEXT3));
                                            }
                                        });
                                    }
                                }
                            }
                        }
                    });
                    if unequip {
                        match slot {
                            Slot::Weapon => self.build.weapon = None,
                            Slot::Ammo => self.build.ammo = None,
                            Slot::Gem => self.build.gem = None,
                            Slot::Armor(k) => self.build.gear[k] = None,
                            _ => {}
                        }
                    }
                } else {
                    ui.label(RichText::new("Nothing equipped here. Pick one below; the list is ranked by what it does for this build.").color(TEXT2));
                }
            }
        }
        ui.add_space(12.0);
        // candidates, each tried in the build
        ui.horizontal(|ui| {
            ui.label(RichText::new(match slot { Slot::Food => "Food to add", Slot::Summon => "Summons to add", _ => "Options for this slot" }).font(heading_font(16.0)).color(HEAD));
            ui.label(RichText::new("each tried in your build").color(TEXT2));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search").desired_width(200.0));
            });
        });
        ui.add_space(6.0);
        self.cand_hover = None;
        let Some((_, rows)) = self.cand.clone() else {
            ui.label(RichText::new("Working out every option…").color(TEXT2));
            return;
        };
        let cur = self.equipped(slot);
        let (best, worst) = rows.iter().fold((f64::MIN, f64::MAX), |(a, b), r| (a.max(r.dps), b.min(r.dps)));
        let f = self.filter.to_lowercase();
        let colw = [ui.available_width() - 440.0, 100.0, 90.0, 90.0, 120.0];
        ui.horizontal(|ui| {
            for (k, h) in ["Item", "Rarity", "Total DPS", "vs now", "Hits survived"].iter().enumerate() {
                cell(ui, colw[k], RichText::new(*h).size(13.0).color(TEXT2), k >= 2);
            }
        });
        ui.painter().hline(ui.min_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
        ui.add_space(4.0);
        let mut equip = None;
        egui::ScrollArea::vertical().id_salt("cands").max_height(ui.available_height() - 10.0).show(ui, |ui| {
            let shown: Vec<&Cand> = rows.iter().filter(|c| f.is_empty() || self.item_name(&c.guid).to_lowercase().contains(&f)).take(80).collect();
            for c in shown {
                let Some(it) = self.b.items.get(&c.guid) else { continue };
                let rar = it.rarity.clone().unwrap_or_default();
                let equipped = cur.as_deref() == Some(c.guid.as_str()) && !matches!(slot, Slot::Food | Slot::Summon);
                let row = ui.horizontal(|ui| {
                    cell(ui, colw[0], RichText::new(format!("{}{}", it.name, if equipped { "  (equipped)" } else { "" })).font(bold_font(15.0)).color(if equipped { HEAD } else { TEXT }), false);
                    cell(ui, colw[1], RichText::new(&rar).color(rarity_color(&rar)), false);
                    cell(ui, colw[2], RichText::new(format!("{:.1}", c.dps)).font(bold_font(15.0)), true);
                    let d = c.dps - now.dps;
                    let t = if best > worst { ((c.dps - worst) / (best - worst)) as f32 } else { 1.0 };
                    let dtext = if equipped || d.abs() < 0.05 { RichText::new("—").color(TEXT3) } else { RichText::new(format!("{d:+.1}")).font(bold_font(15.0)).color(if d > 0.0 { lerp_color(TEXT_SOFT, GOOD, t) } else { BAD }) };
                    cell(ui, colw[3], dtext, true);
                    let ds = c.survived - now.hits_survived;
                    let stext = if ds.abs() < 0.05 { RichText::new(format!("{:.1}", c.survived)).color(TEXT2) } else { RichText::new(format!("{:.1} ({ds:+.1})", c.survived)).color(if ds > 0.0 { GOOD } else { BAD }) };
                    cell(ui, colw[4], stext, true);
                }).response;
                let r = row.interact(Sense::click());
                if r.hovered() {
                    ui.painter().rect_filled(r.rect.expand2(Vec2::new(4.0, 2.0)), 5.0, Color32::from_rgba_unmultiplied(0x33, 0x28, 0x1C, 90));
                    if !equipped {
                        self.cand_hover = Some(c.guid.clone());
                    }
                }
                if r.clicked() && !equipped {
                    equip = Some(c.guid.clone());
                }
                r.on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().hline(row.rect.x_range(), row.rect.bottom() + 3.0, Stroke::new(1.0, LINE));
                ui.add_space(5.0);
            }
        });
        if let Some(g) = equip {
            self.build = apply_slot(self.b, &self.build, slot, &g);
        }
    }

    // ---------------------------------------------------------------- combat setup tab

    fn setup(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(20.0);
            ui.columns(3, |cols| {
                card(&mut cols[0], |ui| {
                    title(ui, "The enemy");
                    ui.add_space(8.0);
                    ui.label(RichText::new("Enemy").font(bold_font(15.0)));
                    ui.add(egui::TextEdit::singleline(&mut self.enemy_filter).hint_text("Search by name or faction").desired_width(f32::INFINITY));
                    let f = self.enemy_filter.to_lowercase();
                    egui::ComboBox::from_id_salt("enemy").width(ui.available_width()).height(440.0).selected_text(self.enemies[self.enemy].label.clone()).show_ui(ui, |ui| {
                        for (i, e) in self.enemies.iter().enumerate() {
                            if i == 0 || f.is_empty() || e.label.to_lowercase().contains(&f) {
                                ui.selectable_value(&mut self.enemy, i, &e.label);
                            }
                        }
                    });
                    ui.label(RichText::new("Sets resistances, faction bonuses and the hits it deals.").size(13.0).color(TEXT2));
                    ui.add_space(10.0);
                    setting(ui, "Level", "Enemy health and damage scale with it", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.enemy_level).range(1..=ens_calc::build::item_level_cap(self.b)));
                    });
                    setting(ui, "Health factor", "× the game's health formula for this level", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.enemy_hp_factor).range(0.1..=3.0).speed(0.01));
                    });
                    setting(ui, "Physical armour", "Reduces your cut, pierce and blunt hits", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.target_armor.0).range(0.0..=500.0));
                    });
                    setting(ui, "Magical armour", "Reduces your fire, ice and fog hits", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.target_armor.1).range(0.0..=500.0));
                    });
                    setting(ui, "Its hits deal", "Damage type used for Hits survived", true, |ui| {
                        egui::ComboBox::from_id_salt("incoming").selected_text(&self.incoming_type).show_ui(ui, |ui| {
                            for t in ["Cut", "Pierce", "Blunt", "Fire", "Ice", "Fog", "Lightning", "Poison"] {
                                ui.selectable_value(&mut self.incoming_type, t.to_string(), t);
                            }
                        });
                    });
                    setting(ui, "Target soaked", "Wet enemies: bonuses that need it apply", true, |ui| {
                        ui.checkbox(&mut self.soaked, "");
                    });
                });
                card(&mut cols[1], |ui| {
                    title(ui, "Your hits");
                    ui.add_space(8.0);
                    segmented(ui, &mut self.hit_kind, &[("Normal".to_string(), "Normal"), ("Sneak".to_string(), "Sneak"), ("Merciless".to_string(), "Merciless"), ("Backstab".to_string(), "Backstab")]);
                    ui.label(RichText::new("Sneak: unnoticed. Merciless: on staggered enemies. Backstab: from behind.").size(13.0).color(TEXT2));
                    ui.add_space(10.0);
                    setting(ui, "Weak-spot hits", "Skill shots on enemies that have a weak spot", true, |ui| {
                        ui.checkbox(&mut self.weak_spot, "");
                    });
                });
                cols[1].add_space(16.0);
                card(&mut cols[1], |ui| {
                    title(ui, "The group");
                    ui.add_space(8.0);
                    setting(ui, "Enemies", "How many are fighting you at once", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.pack.0).range(1.0..=8.0).speed(0.1).fixed_decimals(0));
                    });
                    setting(ui, "Spread", "How far apart they stand", true, |ui| {
                        ui.add(egui::DragValue::new(&mut self.pack.1).range(2.0..=10.0).speed(0.05).suffix(" m"));
                    });
                    ui.label(RichText::new("Decides how many enemies explosions, chains and Storm's Eye reach, and whether group buffs such as Warden apply.").size(13.0).color(TEXT2));
                });
                let swift = self.gem_named("Swiftness");
                let agility = self.gem_named("Agility");
                let aero = self.gem_named("Aerobatics");
                let meteor = self.gem_named("Meteoric");
                let retal = self.gem_named("Retaliation");
                let lyc = self.gem_named("Lycanthropy");
                let mania = self.gem_named("Battle Mania");
                let companions = !self.build.summons.is_empty() || self.gem_named("Necromancy") || self.gem_named("Skulls") || self.total_ranks("Necromancer") > 0;
                card(&mut cols[2], |ui| {
                    title(ui, "How you play");
                    ui.label(RichText::new("Dimmed rows only matter with gear this build doesn't have.").size(13.0).color(TEXT2));
                    ui.add_space(8.0);
                    setting(ui, "Orbs picked up", "Health and mana orbs you walk over", true, |ui| {
                        let mut pct = self.orb_pickup * 100.0;
                        ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).suffix(" %"));
                        self.orb_pickup = pct / 100.0;
                    });
                    setting(ui, "Companions idle", "Time summons spend not attacking", companions, |ui| {
                        let mut idle = self.play.summon_idle * 100.0;
                        ui.add(egui::DragValue::new(&mut idle).range(0.0..=100.0).suffix(" %"));
                        self.play.summon_idle = idle / 100.0;
                    });
                    setting(ui, "Sprint into fights", "Gem of Swiftness", swift, |ui| {
                        ui.checkbox(&mut self.play.sprint_start, "");
                    });
                    setting(ui, "Evades per minute", "Gem of Agility", agility, |ui| {
                        ui.add(egui::DragValue::new(&mut self.play.evades_per_min).range(0.0..=120.0));
                    });
                    setting(ui, "Attacks in the air", "Gem of Aerobatics", aero, |ui| {
                        let mut air = self.play.air_share * 100.0;
                        ui.add(egui::DragValue::new(&mut air).range(0.0..=100.0).suffix(" %"));
                        self.play.air_share = air / 100.0;
                    });
                    setting(ui, "Glide landings per minute", "Gem of Meteoric Impact", meteor, |ui| {
                        ui.add(egui::DragValue::new(&mut self.play.glides_per_min).range(0.0..=30.0));
                    });
                    setting(ui, "Hits blocked per minute", "Gem of Retaliation", retal, |ui| {
                        ui.add(egui::DragValue::new(&mut self.play.blocks_per_min).range(0.0..=120.0));
                    });
                    setting(ui, "Night", "Gem of Lycanthropy", lyc, |ui| {
                        ui.checkbox(&mut self.play.night, "");
                    });
                    setting(ui, "Elixir active", "Gem of Battle Mania", mania, |ui| {
                        ui.checkbox(&mut self.play.elixir, "");
                    });
                    if ui.add(ghost("Reset to defaults")).clicked() {
                        self.play = ens_calc::Play::default();
                        self.orb_pickup = 0.5;
                    }
                });
            });
            ui.add_space(20.0);
        });
    }

    // ---------------------------------------------------------------- calculations tab

    fn section_card(ui: &mut egui::Ui, id: &str, name: &str, summary: RichText, open: bool, body: impl FnOnce(&mut egui::Ui)) {
        card(ui, |ui| {
            let sid = ui.make_persistent_id(id);
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), sid, open)
                .show_header(ui, |ui| {
                    ui.label(RichText::new(name).font(heading_font(17.0)).color(HEAD));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(summary);
                    });
                })
                .body(|ui| {
                    ui.add_space(6.0);
                    body(ui);
                });
        });
        ui.add_space(12.0);
    }

    fn calcs(&mut self, ui: &mut egui::Ui) {
        let s = self.summary();
        ui.add_space(20.0);
        let w = ui.available_width();
        let lw = ((w - 20.0) * 0.56).max(440.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(lw);
                egui::ScrollArea::vertical().id_salt("calc_left").show(ui, |ui| self.calcs_left(ui, &s));
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(w - lw - 24.0);
                egui::ScrollArea::vertical().id_salt("calc_right").show(ui, |ui| self.character_sheet(ui));
            });
        });
    }

    fn calcs_left(&mut self, ui: &mut egui::Ui, s: &Summary) {
        let Some(w) = self.build.weapon.clone() else {
            card(ui, |ui| {
                ui.label(RichText::new("Pick a weapon on the Gear tab to see the damage calculations.").color(TEXT2));
            });
            return;
        };
        let t = self.target();
        let h = self.engine.hit(&self.build, &w, self.build.ammo.as_deref(), 1.0, &t);
        let (secs, how) = self.engine.attack_interval(&self.build, &w, self.build.ammo.as_deref());
        let d = self.engine.dps(&self.build, &w, self.build.ammo.as_deref(), &t, 1.0 / secs.max(0.05));
        let tot = self.engine.total(&self.build, &t);
        let st = self.engine.attributes(&self.build);
        let line = |ui: &mut egui::Ui, a: &str, b: String| {
            kv(ui, a, RichText::new(b).color(TEXT));
        };
        let strong = |ui: &mut egui::Ui, a: &str, b: String| {
            kv(ui, RichText::new(a).font(bold_font(15.0)).color(HEAD), RichText::new(b).font(bold_font(15.5)).color(HEAD));
        };
        let wname = self.item_name(&w.item);
        let aname = self.build.ammo.as_deref().map(|g| self.item_name(g));
        Self::section_card(ui, "c_hit", "One hit", RichText::new(format!("{:.1} expected", h.after_armor)).font(bold_font(15.0)), true, |ui| {
            line(ui, &format!("Weapon base at level {}", w.level.unwrap_or(self.build.level)), trim_num(h.base_damage));
            line(ui, &format!("{wname} damage"), trim_num(h.item_damage));
            if let Some(a) = &aname {
                line(ui, &format!("+ {a} damage"), trim_num(h.ammo_damage));
            }
            for (k, v) in &h.pool_terms {
                if let Some(name) = k.strip_prefix('×') {
                    let name = name.trim();
                    let label = if name.contains("scale") { format!("× {} scaling", name.replace(" scale", "").replace(['(', ')'], "")) } else { format!("× {}", attr_label(name)) };
                    line(ui, &label, format!("× {v:.3}"));
                } else if *v != 0.0 {
                    let key = k.trim_start_matches('+').trim();
                    let r = kv(ui, RichText::new(format!("+ {} (bonus pool)", attr_label(key))).color(TEXT), RichText::new(format!("{:+.1}%", v * 100.0)).color(TEXT));
                    let srcs: Vec<String> = st.sources(key).iter().map(|c| format!("{}  {}", contribution(key, &format!("{:?}", c.op), c.value), c.source)).collect();
                    if !srcs.is_empty() {
                        for sline in &srcs {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new(sline).size(13.5).color(TEXT2));
                            });
                        }
                        ui.add_space(4.0);
                    }
                    let _ = r;
                }
            }
            for (k, v) in &h.per_type {
                line(ui, &format!("{k} damage"), format!("{v:.1}"));
            }
            line(ui, "Non-crit hit", format!("{:.1}", h.non_crit));
            line(ui, &format!("Crit {:.1}%, each adding {:.0}% to the pool", h.crit_chance * 100.0, (h.crit_damage_mod - 1.0) * 100.0), format!("{:.1} on average", h.expected));
            strong(ui, "Expected hit after enemy armour", format!("{:.1}", h.after_armor));
        });
        Self::section_card(ui, "c_rate", "Attack rate", RichText::new(format!("{:.2} projectiles every {secs:.2} s", d.projectiles)).color(TEXT_SOFT), false, |ui| {
            line(ui, "Attack cycle", format!("{secs:.2} s"));
            line(ui, "How the cycle is made", how.clone());
            line(ui, "Projectiles per attack", format!("{:.2}", d.projectiles));
            if d.aoe_radius > 0.0 {
                line(ui, "Area effect", format!("{:.1} m blast × {:.1} → {:.2} enemies", d.aoe_radius, d.aoe_attack_mod, d.aoe_targets));
            }
            line(ui, "Regular DPS", format!("{:.1}", d.regular_dps));
            if d.special_projectiles > 0.0 {
                line(ui, "Special: Focus per hit / cost", format!("{} / {}", d.focus_per_hit, d.focus_cost));
                line(ui, "Special: projectiles × attack mod", format!("{:.2} × {:.1}", d.special_projectiles, d.special_attack_mod));
                line(ui, "Special uses per second", format!("{:.3}", d.special_per_s));
                line(ui, "Special DPS", format!("{:.1}", d.special_dps));
            }
            strong(ui, "DPS while attacking", format!("{:.1}", d.total));
        });
        let mana_note = if tot.main_mana_per_s > 0.0 { RichText::new(format!("{:.0}% uptime", tot.main_uptime * 100.0)).color(if tot.main_uptime < 0.6 { WARN } else { TEXT_SOFT }) } else { RichText::new("no mana cost").color(TEXT3) };
        Self::section_card(ui, "c_mana", "Mana budget", mana_note, false, |ui| {
            line(ui, "Mana pool", format!("{:.0}", self.engine.sheet(&self.build).values.get("Mana").copied().unwrap_or(0.0)));
            line(ui, "Regeneration", format!("{:.2} /s", s.mana_regen));
            line(ui, "The attack needs", format!("{:.2} /s", tot.main_mana_per_s));
            let summon_mana: f64 = tot.summons.iter().map(|x| x.mana_per_s * x.uptime).sum();
            if summon_mana > 0.0 {
                line(ui, "Summons need (paid first)", format!("{summon_mana:.2} /s"));
            }
            for pr in tot.procs.iter().filter(|p| p.resource == "Mana") {
                line(ui, &format!("+ {}", pr.skill), format!("{:+.2} /s", pr.per_s));
            }
            line(ui, "Uptime over a 60 s fight", format!("{:.0}%", tot.main_uptime * 100.0));
            if let Some((n, fd)) = &tot.fallback {
                line(ui, &format!("Out of mana: {n}"), format!("{fd:.1} DPS"));
            }
            strong(ui, "Total DPS", format!("{:.1}", tot.total));
        });
        let extra = tot.skill_damage.len() + tot.buffs.len() + tot.procs.len();
        Self::section_card(ui, "c_extra", "Gems, skills, procs, buffs", if extra == 0 { RichText::new("none").color(TEXT3) } else { RichText::new(format!("{extra} active")).color(TEXT_SOFT) }, false, |ui| {
            if extra == 0 {
                ui.label(RichText::new("Nothing in this build deals its own damage or triggers on hits.").color(TEXT2));
            }
            for (n, dd, how) in &tot.skill_damage {
                line(ui, n, format!("+{dd:.1} DPS ({how})"));
            }
            for (n, u, with) in &tot.buffs {
                line(ui, &format!("{n} (buff)"), format!("up {:.0}% · {with:.1} DPS while up", u * 100.0));
            }
            for pr in &tot.procs {
                let what = if pr.orbs > 0.0 {
                    format!("{:.0}% per {} → {:.2} {} orbs of {:.0} → +{:.2}/s", pr.chance * 100.0, pr.event, pr.orbs, pr.resource, pr.amount, pr.per_s)
                } else {
                    format!("+{:.2} {}/s ({:.0}% per {})", pr.per_s, pr.resource, pr.chance * 100.0, pr.event)
                };
                line(ui, &pr.skill, what);
            }
        });
        let sums = &tot.summons;
        Self::section_card(ui, "c_summons", "Summons", if sums.is_empty() { RichText::new("none").color(TEXT3) } else { RichText::new(format!("{:.1} DPS", sums.iter().map(|x| x.total_dps).sum::<f64>())).color(TEXT_SOFT) }, false, |ui| {
            if sums.is_empty() {
                ui.label(RichText::new("No summons, totems or companions in this build.").color(TEXT2));
            }
            for x in sums {
                line(ui, &format!("{} ×{:.1}", x.name, x.count), format!("{:.0} per hit · {} → {:.1} DPS each, up {:.0}% → {:.1} DPS", x.hit, x.note, x.each_dps, x.uptime * 100.0, x.total_dps));
            }
        });
        let hp = ens_calc::enemy_health(self.b, self.enemy_level, self.hp_factor());
        Self::section_card(ui, "c_fight", "Fight and survival", RichText::new(format!("{:.1} s to kill · {:.1} hits survived", hp / tot.total.max(0.01), s.hits_survived)).color(TEXT_SOFT), false, |ui| {
            line(ui, "Enemy health (game formula, solo)", format!("{hp:.0}"));
            line(ui, "Time to kill", format!("{:.1} s", hp / tot.total.max(0.01)));
            line(ui, "Enemy's biggest hit", format!("{:.0}", s.hit_raw));
            line(ui, "After your armour and resistances", format!("{:.0}", s.hit_taken));
            line(ui, "Your health", format!("{:.0}", s.health));
            strong(ui, "Hits survived", format!("{:.1}", s.hits_survived));
        });
    }

    fn character_sheet(&mut self, ui: &mut egui::Ui) {
        let st = self.engine.attributes(&self.build);
        let sheet = self.engine.sheet(&self.build);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                title(ui, "Character sheet");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.sheet_all, "Show unchanged");
                });
            });
            ui.add(egui::TextEdit::singleline(&mut self.sheet_query).hint_text("Find a stat (e.g. crit, bow, armour)").desired_width(f32::INFINITY));
            ui.add_space(8.0);
            let mut keys: Vec<String> = sheet.values.keys().cloned().collect();
            for c in &st.contributions {
                if !keys.contains(&c.attribute) {
                    keys.push(c.attribute.clone());
                }
            }
            let q = self.sheet_query.to_lowercase();
            let main = ["Health", "Mana", "Stamina", "Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit", "CritChance", "CritDamageMod", "Mana regen /s", "Stamina regen /s", "ArmorPoints_Physical", "ArmorPoints_Magical"];
            let mut rows: Vec<(&'static str, String, String, String)> = keys.into_iter().filter_map(|k| {
                let changed = !st.sources(&k).is_empty();
                if !(self.sheet_all || changed || main.contains(&k.as_str())) {
                    return None;
                }
                // the per-second regen rows already show these raw recharge values
                if k.ends_with("_Recharge") && !self.sheet_all {
                    return None;
                }
                let label = attr_label(&k);
                if !q.is_empty() && !label.to_lowercase().contains(&q) && !k.to_lowercase().contains(&q) {
                    return None;
                }
                let v = sheet.values.get(&k).copied().unwrap_or_else(|| st.get(&k));
                let base = self.engine.attrs.base(&k);
                let val = attr_value(&k, v, base);
                Some((labels::group(&k), label, val, k))
            }).collect();
            let order = ["Attributes", "Resources", "Offence", "Defence", "Other"];
            rows.sort_by(|a, b| order.iter().position(|g| *g == a.0).cmp(&order.iter().position(|g| *g == b.0)).then(a.1.cmp(&b.1)));
            let mut group = "";
            for (g, label, val, key) in rows {
                if g != group {
                    ui.add_space(8.0);
                    eyebrow(ui, g);
                    group = g;
                }
                let srcs = st.sources(&key);
                let r = kv(ui, RichText::new(&label).color(TEXT_SOFT), RichText::new(&val).font(bold_font(15.0)).color(if srcs.is_empty() { TEXT2 } else { TEXT }));
                let mut tip = format!("{key}");
                for c in &srcs {
                    tip.push_str(&format!("\n{}  {}", contribution(&key, &format!("{:?}", c.op), c.value), c.source));
                }
                r.on_hover_text(tip);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Hover a stat to see every source that sets it.").size(13.0).color(TEXT3));
        });
    }

    // ---------------------------------------------------------------- optimizer tab

    fn optimizer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        let w = ui.available_width();
        let lw = 420.0f32.min(w * 0.42);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(lw);
                card(ui, |ui| {
                    title(ui, "Goals, most important first");
                    ui.add_space(8.0);
                    let mut swap = None;
                    for (i, g) in self.ranking.iter().enumerate() {
                        let r = ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}", i + 1)).font(bold_font(15.0)).color(FLAME));
                            ui.label(RichText::new(g.label()).font(bold_font(15.5)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if i + 1 < GOALS.len() && ui.add(egui::Button::new(RichText::new("▼").size(17.0).color(TEXT_SOFT)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BORDER))).on_hover_text("Lower priority").clicked() {
                                    swap = Some((i, i + 1));
                                }
                                if i > 0 && ui.add(egui::Button::new(RichText::new("▲").size(17.0).color(TEXT_SOFT)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BORDER))).on_hover_text("Higher priority").clicked() {
                                    swap = Some((i, i - 1));
                                }
                            });
                        }).response;
                        ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, Stroke::new(1.0, LINE));
                        ui.add_space(6.0);
                    }
                    if let Some((a, c)) = swap {
                        self.ranking.swap(a, c);
                    }
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.opt_gear, "Also choose gear: armour, rings and a weapon of this type");
                    if self.opt_gear {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Rarity for items that roll it").color(TEXT2));
                            egui::ComboBox::from_id_salt("grar").selected_text(RichText::new(&self.gear_rarity).color(rarity_color(&self.gear_rarity))).show_ui(ui, |ui| {
                                for x in ["Common", "Uncommon", "Rare", "Epic", "Legendary"] {
                                    ui.selectable_value(&mut self.gear_rarity, x.to_string(), RichText::new(x).color(rarity_color(x)));
                                }
                            });
                        });
                        ui.label(RichText::new("Fixed-rarity items keep their own rarity. Armour set bonuses are not counted yet.").size(13.0).color(TEXT3));
                    }
                    ui.add_space(10.0);
                    let busy = self.running.is_some();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, primary(if busy { "Optimizing…" } else { "Optimize this loadout" })).clicked() {
                            self.start_optimizer(ui.ctx());
                        }
                        if busy {
                            ui.spinner();
                        }
                    });
                    ui.label(RichText::new("Uses this loadout's level, weapon, ammo, gear, food, summons and the Combat setup.").size(13.0).color(TEXT2));
                });
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(w - lw - 24.0);
                card(ui, |ui| self.optimizer_result(ui));
            });
        });
    }

    fn optimizer_result(&mut self, ui: &mut egui::Ui) {
        title(ui, "Result");
        ui.add_space(8.0);
        let now = self.summary();
        let Some(o) = &self.last_opt else {
            ui.label(RichText::new(if self.running.is_some() { "Searching for the best tree… this takes a few seconds." } else { "Run the optimizer to see the best tree for your goals here, next to what you have now." }).color(TEXT2));
            return;
        };
        if let Some(r) = &self.opt_summary {
            let head = |ui: &mut egui::Ui| {
                ui.horizontal(|ui| {
                    cell(ui, 220.0, RichText::new("").size(13.0), false);
                    cell(ui, 100.0, RichText::new("Now").size(13.0).color(TEXT2), true);
                    cell(ui, 100.0, RichText::new("Proposed").size(13.0).color(TEXT2), true);
                    cell(ui, 90.0, RichText::new("Change").size(13.0).color(TEXT2), true);
                });
            };
            head(ui);
            let rowf = |ui: &mut egui::Ui, label: &str, a: f64, b: f64, higher: bool, fmt: &dyn Fn(f64) -> String| {
                let r = ui.horizontal(|ui| {
                    cell(ui, 220.0, RichText::new(label).color(TEXT_SOFT), false);
                    cell(ui, 100.0, RichText::new(fmt(a)).color(TEXT), true);
                    cell(ui, 100.0, RichText::new(fmt(b)).font(bold_font(15.5)).color(HEAD), true);
                    let d = delta(a, b, higher, |d| format!("{d:+.1}")).unwrap_or_else(|| RichText::new("—").color(TEXT3));
                    cell(ui, 90.0, d, true);
                }).response;
                ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, Stroke::new(1.0, LINE));
                ui.add_space(5.0);
            };
            rowf(ui, "Total DPS", now.dps, r.dps, true, &|v| format!("{v:.1}"));
            rowf(ui, "Expected hit", now.expected_hit, r.expected_hit, true, &|v| format!("{v:.1}"));
            rowf(ui, "Hits survived", now.hits_survived, r.hits_survived, true, &|v| format!("{v:.1}"));
            rowf(ui, "Health", now.health, r.health, true, &|v| format!("{v:.0}"));
            if now.has_mana_cost || r.has_mana_cost {
                rowf(ui, "Mana uptime %", now.mana_uptime * 100.0, r.mana_uptime * 100.0, true, &|v| format!("{v:.0}"));
            }
        }
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} of {} points used", self.b.skills.iter().enumerate().map(|(i, s)| o.ranks[i] as u32 * s.cost).sum::<u32>(), points_available(self.build.level, self.build.found_points))).color(TEXT2));
        let mut gs: Vec<_> = o.goals.iter().collect();
        gs.sort_by(|a, b| a.0.cmp(b.0));
        ui.horizontal_wrapped(|ui| {
            for (k, v) in gs {
                tile(ui, RAISED, BORDER, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(*k).size(12.5).color(TEXT2));
                        ui.label(RichText::new(format!("{v:.1}")).font(bold_font(16.0)));
                    });
                });
            }
        });
        if let Some((wpn, gear)) = &o.gear {
            ui.add_space(10.0);
            eyebrow(ui, "Gear it chose");
            let name = |e: &Option<Equip>| e.as_ref().and_then(|e| self.b.items.get(&e.item).map(|i| (e, i))).map(|(e, i)| {
                let r = e.rarity.clone().or(i.rarity.clone()).unwrap_or_default();
                RichText::new(i.name.clone()).color(rarity_color(&r))
            }).unwrap_or_else(|| RichText::new("Empty").color(TEXT3));
            let line = |ui: &mut egui::Ui, slot: &str, new: &Option<Equip>, old: &Option<Equip>| {
                let changed = new.as_ref().map(|e| &e.item) != old.as_ref().map(|e| &e.item);
                let r = ui.horizontal(|ui| {
                    cell(ui, 90.0, RichText::new(slot).color(TEXT2), false);
                    ui.label(name(new));
                    if changed {
                        ui.label(RichText::new("changed").size(12.5).color(TEAL));
                    }
                }).response;
                ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, Stroke::new(1.0, LINE));
                ui.add_space(4.0);
            };
            line(ui, "Weapon", wpn, &self.build.weapon);
            for (k, slot) in ARMOR_SLOTS.iter().enumerate() {
                line(ui, slot, &gear[k], &self.build.gear[k]);
            }
        }
        ui.add_space(12.0);
        let n = o.order.len();
        let mut apply = None;
        let mut apply_gear = None;
        ui.horizontal(|ui| {
            if ui.add(primary("Apply result")).clicked() {
                apply = Some(o.ranks.clone());
                apply_gear = o.gear.clone();
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Leveling order").color(TEXT2));
            if ui.add(ghost("◀")).clicked() && self.order_step > 0 {
                self.order_step -= 1;
            }
            ui.label(RichText::new(format!("{}/{n}", self.order_step)).font(bold_font(15.0)));
            if ui.add(ghost("▶")).clicked() && self.order_step < n {
                self.order_step += 1;
            }
            if ui.add(ghost("Show the tree at this step")).clicked() {
                let mut r = vec![0u8; self.b.skills.len()];
                for &i in o.order.iter().take(self.order_step) {
                    r[i] += 1;
                }
                apply = Some(r);
            }
        });
        if self.order_step > 0 {
            let i = o.order[self.order_step - 1];
            ui.label(RichText::new(format!("Step {}: {}", self.order_step, self.b.skills[i].name)).color(TEXT_SOFT));
        }
        if let Some(r) = apply {
            self.build.ranks = r;
        }
        if let Some((w, gear)) = apply_gear {
            self.build.weapon = w;
            self.build.gear = gear;
        }
    }

    fn start_optimizer(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let opt = self.opt;
        let plan = Plan {
            level: self.build.level,
            flame_level: self.build.flame_level,
            weapon: self.build.weapon.clone(),
            ammo: self.build.ammo.clone(),
            gear: self.build.gear.clone(),
            foods: self.build.foods.clone(),
            summons: self.build.summons.clone(),
            ranking: self.ranking.clone(),
            attacks_per_s: self.build.weapon.as_ref().map(|w| attack_rate(&self.engine, &self.build, w, self.build.ammo.as_deref())).unwrap_or(1.0),
            target: self.target(),
            found_points: self.build.found_points,
            weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: Default::default(),
        };
        let ctx = ctx.clone();
        let (with_gear, rarity) = (self.opt_gear, self.gear_rarity.clone());
        std::thread::spawn(move || {
            let done = if with_gear {
                let (best, order, p) = opt.optimize_with_gear(&plan, 40, 7, &rarity, 2);
                OptDone { gear: Some((p.weapon, p.gear)), ranks: best.ranks, order, goals: best.goals }
            } else {
                let (best, order) = opt.optimize(&plan, 40, 7);
                OptDone { gear: None, ranks: best.ranks, order, goals: best.goals }
            };
            let _ = tx.send(done);
            ctx.request_repaint();
        });
        self.running = Some(rx);
    }

    // ---------------------------------------------------------------- notes tab

    fn notes(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(20.0);
            ui.set_max_width(820.0);
            card(ui, |ui| {
                title(ui, "Build code");
                ui.label(RichText::new("A build code holds this loadout's tree, gear, food and level. Paste one to load it.").color(TEXT2));
                ui.add_space(6.0);
                ui.add(egui::TextEdit::multiline(&mut self.code).desired_rows(3).desired_width(f32::INFINITY).hint_text("Paste a build code here"));
                ui.horizontal(|ui| {
                    if ui.add(primary("Load into this loadout")).clicked() {
                        match Build::from_code_for(&self.code, self.b) {
                            Ok((b, warning)) => {
                                self.build = b;
                                self.status = warning.unwrap_or_else(|| "Build loaded.".into());
                            }
                            Err(e) => self.status = e,
                        }
                    }
                    if ui.add(ghost("Copy this loadout's code")).clicked() {
                        self.code = self.build.to_code_for(self.b);
                        ui.ctx().copy_text(self.code.clone());
                        self.status = "Build code copied to the clipboard.".into();
                    }
                });
            });
            ui.add_space(16.0);
            card(ui, |ui| {
                title(ui, "Notes");
                ui.add_space(6.0);
                ui.add(egui::TextEdit::multiline(&mut self.notes).desired_rows(16).desired_width(f32::INFINITY).hint_text("Rotation, gear to farm, reminders…"));
            });
        });
    }
}

fn kind_label(k: &str) -> &str {
    match k {
        "PassiveSkill" => "Passive",
        "ActiveSkill" => "Active",
        "Attribute" => "Attribute",
        x => x,
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.rec.input(raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t0 = std::time::Instant::now();
        // Frames never wait for the screen (Mailbox), so egui's hover/fade animations would redraw ~1000 times a
        // second for 0.2 s after every pointer change; that floods the compositor, which also draws the mouse
        // pointer. Cap those to ~165/s (a fast monitor's refresh) by sleeping out the rest of the frame, except while
        // the window itself moves or resizes (Windows' move/resize loop must not wait).
        let rect = ui.ctx().input(|i| i.viewport().outer_rect.or(i.viewport().inner_rect));
        let moving = rect != self.last_rect;
        self.last_rect = rect;
        let mut slept = 0.0;
        if let Some(prev) = self.last_frame {
            let min = std::time::Duration::from_micros(6_000);
            let since = t0.duration_since(prev);
            if !moving && since < min && std::env::var("ENS_NOCAP").is_err() {
                std::thread::sleep(min - since);
                slept = (min - since).as_secs_f64() * 1000.0;
            }
        }
        let t1 = std::time::Instant::now();
        if std::env::var("ENS_SPIN").is_ok() {
            ui.ctx().request_repaint();
        }
        self.last_frame = Some(t1);
        self.frame_ui(ui);
        if self.rec.enabled() {
            let ctx = ui.ctx().clone();
            let prev = frame.info().cpu_usage.map(|s| s as f64 * 1000.0).unwrap_or(0.0);
            self.rec.frame(t0, slept, t1.elapsed().as_secs_f64() * 1000.0, prev, &ctx);
        }
    }
}

impl App {
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        if let Some(rx) = &self.running {
            if let Ok(done) = rx.try_recv() {
                self.order_step = done.order.len();
                let mut nb = self.build.clone();
                nb.ranks = done.ranks.clone();
                if let Some((w, g)) = &done.gear {
                    nb.weapon = w.clone();
                    nb.gear = g.clone();
                }
                self.opt_summary = Some(summarize(&self.engine, &nb, &self.conditions()));
                self.last_opt = Some(done);
                self.running = None;
                self.status = "Optimizer finished.".into();
            }
        }
        if self.running.is_some() || self.heat_rx.is_some() || self.cand_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(path) = self.shot.clone() {
            self.frames += 1;
            if self.frames == 2 {
                if self.tab == Tab::Tree {
                    self.hovered = self.b.skills.iter().position(|s| s.name == "Fatal Precision").or_else(|| self.b.skills.iter().position(|s| s.name == "Sniper"));
                }
                self.search = std::env::var("ENS_SEARCH").unwrap_or_default();
                self.heatmap = std::env::var("ENS_HEAT").is_ok();
            }
            let wait_jobs = self.heat_rx.is_some() || self.cand_rx.is_some();
            if self.frames >= 14 && !wait_jobs || self.frames == 600 {
                self.frames = 10_000;
                ctx.request_screenshot(move |img| {
                    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let _ = image::save_buffer(&path, &bytes, w, h, image::ExtendedColorType::Rgba8);
                    std::process::exit(0);
                });
            }
            ctx.request_repaint();
        }
        egui::Panel::top("top").frame(egui::Frame::new().fill(BG1).inner_margin(egui::Margin::symmetric(20, 10))).show(ui, |ui| self.top_bar(ui));
        egui::Panel::top("tabs").frame(egui::Frame::new().fill(BG1).stroke(Stroke::new(1.0, BORDER)).inner_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 4 })).show(ui, |ui| self.tabs(ui));
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(BG1).stroke(Stroke::new(1.0, BORDER)).inner_margin(egui::Margin::symmetric(20, 6))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Enshrouded {} · build {} · {} skill nodes · {} perks · {} items", self.b.meta.get("version").and_then(|v| v.as_str()).unwrap_or("?"), self.b.meta.get("build").map(|v| v.to_string()).unwrap_or_default(), self.b.skills.len(), self.b.perks.len(), self.b.items.len())).size(12.5).color(TEXT3));
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).size(12.5).color(GOOD));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("Ctrl+Z undo · Ctrl+F search tree · Ctrl+= text size · {}", self.gpu)).size(12.5).color(TEXT3));
                });
            });
        });
        egui::Panel::left("rail").exact_size(280.0).resizable(false).frame(egui::Frame::new().fill(BG1).stroke(Stroke::new(1.0, BORDER)).inner_margin(egui::Margin::symmetric(20, 6))).show(ui, |ui| self.rail(ui));
        if self.tab == Tab::Tree {
            egui::Panel::right("skill").default_size(340.0).frame(egui::Frame::new().fill(BG1).stroke(Stroke::new(1.0, BORDER)).inner_margin(egui::Margin::symmetric(16, 6))).show(ui, |ui| self.skill_panel(ui));
        } else {
            self.hovered = None;
        }
        if self.tab != Tab::Gear {
            self.cand_hover = None;
        }
        let pad = egui::Frame::new().fill(BG0).inner_margin(egui::Margin::symmetric(24, 0));
        match self.tab {
            Tab::Tree => {
                egui::Panel::top("treebar").frame(egui::Frame::new().fill(BG0).inner_margin(egui::Margin::symmetric(16, 10))).show(ui, |ui| self.tree_toolbar(ui));
                egui::CentralPanel::no_frame().show(ui, |ui| self.tree(ui));
            }
            Tab::Overview => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.overview(ui));
            }
            Tab::Gear => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.gear(ui));
            }
            Tab::Setup => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.setup(ui));
            }
            Tab::Calcs => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.calcs(ui));
            }
            Tab::Optimizer => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.optimizer(ui));
            }
            Tab::Notes => {
                egui::CentralPanel::default().frame(pad).show(ui, |ui| self.notes(ui));
            }
        }
    }
}

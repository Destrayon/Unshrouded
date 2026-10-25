//! `ens` — command line for the Enshrouded 1.0 planner.

use ens_data::{default_bundle_path, Bundle};
use std::collections::BTreeMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bundle = match Bundle::load(default_bundle_path()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["data", "stats"] | [] => stats(&bundle),
        ["data", "skill", name] => skill(&bundle, name),
        ["data", "enemies"] => enemies(&bundle, ""),
        ["data", "enemies", filter] => enemies(&bundle, filter),
        ["calc", "sample"] => sample(&bundle),
        ["calc", "hit", weapon, char_level, target] => hit(&bundle, weapon, char_level, target),
        ["calc", "log", csv, weapon, level, target] if !weapon.starts_with("--") => log(&bundle, csv, weapon, level, target),
        ["calc", "log", csv, rest @ ..] => log_vs_save(&bundle, csv, rest),
        ["calc", "measure", csv, name, rest @ ..] => measure(&bundle, csv, name, rest),
        ["validate"] => validate(&bundle),
        ["calc", "summoner"] => summoner(&bundle),
        ["calc", "load"] => load(&bundle, None),
        ["calc", "load", path] => load(&bundle, Some(path)),
        ["save", "set-level", path, level] => save_set_level(path, level),
        ["save", "apply", path, spec] => save_apply(path, spec),
        ["overlay", "install"] => overlay_install(None),
        ["overlay", "install", dir] => overlay_install(Some(dir)),
        ["overlay", "uninstall"] => overlay_uninstall(),
        ["overlay", "status"] => overlay_status(),
        ["proof"] => write_proof(),
        ["audit"] => {
            let t0 = std::time::Instant::now();
            let r = ens_calc::audit::run(&bundle);
            std::fs::write("../docs/research/coverage.md", r.markdown()).expect("write coverage");
            std::fs::write("data/coverage.json", serde_json::to_string_pretty(&r.json()).unwrap()).expect("write coverage json");
            for k in ["skill", "perk", "food", "buff item", "damage item", "gem"] {
                println!("{k}: {:?}", r.counts(k));
            }
            println!("top gaps: {:?}", r.gaps().iter().take(15).map(|g| (g.0.as_str(), g.1)).collect::<Vec<_>>());
            println!("{:.1}s", t0.elapsed().as_secs_f64());
            write_proof();
        }
        ["opt", lvl] => opt(&bundle, lvl.parse().unwrap_or(50), false),
        ["opt", lvl, "--gear"] => opt(&bundle, lvl.parse().unwrap_or(50), true),
        _ => eprintln!("usage: ens data stats | ens data skill <name> | ens data enemies [filter] | ens calc sample | ens calc load [save.ksd] | ens overlay install [dir] | ens overlay uninstall | ens overlay status"),
    }
}

/// The recorder's folder: the given one, ENS_LOG_DIR, or a "Combat Logger" folder beside this exe or up to five
/// folders above it (the layout of the release folder and of the repository).
fn logger_dir(given: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(d) = given.map(String::from).or_else(|| std::env::var("ENS_LOG_DIR").ok()) {
        return Some(d.into());
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(6).map(|a| a.join("Combat Logger")).find(|d| d.is_dir())
}

/// `ens overlay install [dir]`: put ens_overlay.dll (from beside this exe) into the recorder's folder, write the
/// layer manifest there and register it for the current user.
fn overlay_install(dir: Option<&str>) {
    use ens_overlay::install;
    let Some(dir) = logger_dir(dir) else {
        eprintln!("no Combat Logger folder found; pass it: ens overlay install <folder>");
        std::process::exit(1);
    };
    let built = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join(install::DLL)));
    if let Some(src) = built.filter(|p| p.is_file()) {
        if let Err(e) = std::fs::copy(&src, dir.join(install::DLL)) {
            // the game (or another Vulkan program) has the old copy loaded
            if dir.join(install::DLL).is_file() {
                eprintln!("kept the existing {} ({e}); close the game to update it", install::DLL);
            } else {
                eprintln!("copying {}: {e}", src.display());
                std::process::exit(1);
            }
        }
    }
    match install::install(&dir) {
        Ok(m) => {
            println!("installed {} -> {}", install::LAYER, m.display());
            println!("it draws only inside enshrouded.exe; start the game after the planner (or the live meter) is running");
            println!("turn it off for one launch with ENS_OVERLAY_DISABLE=1, or for good with: ens overlay uninstall");
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn overlay_uninstall() {
    let old = ens_overlay::install::uninstall();
    if old.is_empty() {
        println!("{} was not installed", ens_overlay::install::LAYER);
    }
    for p in old {
        println!("removed {}", p.display());
    }
}

fn overlay_status() {
    let reg = ens_overlay::install::registered();
    if reg.is_empty() {
        println!("{} is not installed (ens overlay install)", ens_overlay::install::LAYER);
    }
    for p in reg {
        let frame = p.with_file_name(ens_overlay::frame::FILE);
        let age = std::fs::metadata(&frame).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        println!("{}  manifest {}  frame {}", p.display(), if p.is_file() { "present" } else { "MISSING" },
            age.map(|a| format!("written {:.1} s ago", a.as_secs_f64())).unwrap_or_else(|| "not written yet".into()));
    }
}

/// A character from a save: the given `characterN.ksd`, or the most recently written one in the Steam save folders.
/// Prints level, experience and the skill nodes as the planner would rank them, grouped by node type.
fn load(b: &Bundle, path: Option<&str>) {
    use ens_data::save;
    let path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let dirs = save::save_dirs();
            match save::character_saves(&dirs).into_iter().next() {
                Some(f) => f.path,
                None => {
                    eprintln!("no characterN.ksd found in {dirs:?} (set ENS_SAVE_DIR to the save folder)");
                    std::process::exit(1);
                }
            }
        }
    };
    let s = match save::load(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let r = s.ranks(b);
    println!("{}  {}", path.display(), s.name);
    println!("level {}  experience in level {}  nodes {}", s.level, s.xp, s.nodes.len());
    let mut by_kind: BTreeMap<&str, BTreeMap<&str, u32>> = BTreeMap::new();
    for (i, &rank) in r.ranks.iter().enumerate().filter(|(_, r)| **r > 0) {
        let sk = &b.skills[i];
        *by_kind.entry(sk.kind.as_str()).or_default().entry(sk.name.as_str()).or_default() += rank as u32;
    }
    for (kind, names) in &by_kind {
        let list: Vec<String> = names.iter().map(|(n, k)| format!("{n} {k}")).collect();
        println!("  {kind}: {}", list.join(", "));
    }
    if !r.unlocked_zero_rank.is_empty() {
        let list: Vec<&str> = r.unlocked_zero_rank.iter().map(|&i| b.skills[i].name.as_str()).collect();
        println!("  listed at rank 0: {}", list.join(", "));
    }
    if !r.unknown.is_empty() {
        println!("  node ids not in the game data: {:?}", r.unknown);
    }
    if let Some(e) = &s.items_error {
        println!("items not read: {e}");
        return;
    }
    let names: std::collections::HashMap<u32, &ens_data::Item> = b.items.iter().filter_map(|(g, i)| Some((save::item_id(g)?, i))).collect();
    let show = |it: &save::SaveItem| {
        let name = names.get(&it.id).map_or_else(|| format!("unknown item {}", it.id), |i| i.name.clone());
        let rarity = it.rarity().map(|r| ["Common", "Uncommon", "Rare", "Epic", "Legendary"][r as usize]);
        let perks = it.perks.map(|(a, u)| format!(" perks {u}/{a}")).unwrap_or_default();
        let level = it.level.map(|l| format!(" L{l}")).unwrap_or_default();
        format!("{name}{}{level}{perks}", rarity.map(|r| format!(" {r}")).unwrap_or_default())
    };
    println!("equipped:");
    for (bit, it) in &s.items.equipped {
        println!("  slot {bit:2}  {}", show(it));
    }
    let weapons: Vec<String> = s.items.inventory.iter()
        .filter(|it| names.get(&it.id).is_some_and(|i| i.category.as_deref() == Some("Weapons") && i.damage.is_some()))
        .map(show).collect();
    println!("weapons carried: {}", weapons.join(", "));
    for (what, at) in [("arrow", s.items.arrow), ("spell", s.items.spell)] {
        if let Some(i) = at {
            println!("{what} equipped: {} ×{}", show(&s.items.inventory[i]), s.items.inventory[i].count);
        }
    }
}

/// Compare the player's hits in a recorder log with the planner, for the character in a save: `ens calc log <csv>
/// [--save <characterN.ksd>] [--flame N] [--target <enemy>]`. Without --save the newest character save is used. Each
/// group of hits is compared against the resistances of the target's faction (from the log), or against --target;
/// the weapon and arrow behind it are the ones the character owns that predict it best. Food and timed buffs are not
/// counted. Without --flame the Flame level is the world's, from the world saved closest to the log's last hit (or
/// to the character save, for logs without clock time).
/// What `ens calc log` and `ens calc measure` read before comparing: the save, the log's hits, the Flame level, the
/// character built from the save, the --target enemy and what the save owns.
struct LogSession {
    s: ens_data::save::Save,
    save_path: std::path::PathBuf,
    hits: Vec<ens_calc::combat_log::LogHit>,
    flame: u32,
    flame_from: String,
    build: ens_calc::Build,
    fixed: Option<ens_calc::Target>,
    fixed_key: Option<String>,
    weapons: Vec<ens_calc::Equip>,
    ammo: Vec<String>,
}

fn log_session(b: &Bundle, eng: &ens_calc::Engine, csv: &str, args: &[&str]) -> Result<LogSession, String> {
    use ens_calc::combat_log as cl;
    use ens_calc::Target;
    use ens_data::save;
    let opt = |name: &str| args.iter().position(|a| *a == name).and_then(|i| args.get(i + 1)).map(|s| s.to_string());
    let save_path = match opt("--save") {
        Some(p) => std::path::PathBuf::from(p),
        None => save::character_saves(&save::save_dirs()).into_iter().next().map(|f| f.path)
            .ok_or("no character save found; pass --save <characterN.ksd>")?,
    };
    let s = save::load(&save_path).map_err(|e| e.to_string())?;
    if let Some(e) = &s.items_error {
        return Err(format!("the save's items could not be read: {e}"));
    }
    let text = std::fs::read_to_string(csv).map_err(|e| format!("cannot read {csv}: {e}"))?;
    let mut hits = cl::parse(&text).map_err(|e| format!("{csv}: {e}"))?;
    // rows from the client hook carry no per-type amounts: nothing to measure the formula against
    hits.retain(|h| !h.from_client());
    let when = hits.last().and_then(|h| h.unix_ms).map(|ms| (ms / 1000) as u64).or_else(|| {
        let m = std::fs::metadata(&save_path).and_then(|m| m.modified()).ok()?;
        m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
    });
    let dirs: Vec<std::path::PathBuf> = save_path.parent().map(|d| vec![d.to_path_buf()]).unwrap_or_default();
    let worlds = save::world_saves(&dirs);
    let world = when.and_then(|t| save::world_at(&worlds, t));
    let flame: u32 = match (opt("--flame").and_then(|v| v.parse().ok()), world) {
        (Some(f), _) => f,
        (None, Some(w)) => w.flame_level.clamp(1, ens_calc::build::FLAME_LEVEL_CAP),
        (None, None) => 1,
    };
    let flame_from = match (opt("--flame").is_some(), world) {
        (true, _) => String::new(),
        (false, Some(w)) => format!(" from world {:?}", w.name),
        (false, None) => "; no world save found, pass --flame N".into(),
    };
    let build = cl::character(b, eng, &s, flame);
    let (fixed, fixed_key) = match opt("--target") {
        Some(name) => {
            let key = ens_calc::build::find_enemy(b, &name)?;
            let et = &b.enemy_types[&key];
            (Some(Target {
                susceptibility: et.susceptibility.clone(),
                faction: Some(et.faction.clone()),
                enemy_class: Some(et.class.clone()),
                flying: et.flying,
                enemy_type: Some(key.clone()),
                ..Target::default()
            }), Some(key))
        }
        None => (None, None),
    };
    let (weapons, ammo) = cl::owned(b, &s);
    Ok(LogSession { s, save_path, hits, flame, flame_from, build, fixed, fixed_key, weapons, ammo })
}

fn log_vs_save(b: &Bundle, csv: &str, args: &[&str]) {
    use ens_calc::combat_log as cl;
    use ens_calc::Engine;
    use ens_data::save;
    let eng = Engine::new(b);
    let LogSession { s, save_path, hits, flame, flame_from, build, fixed, weapons, ammo, .. } = match log_session(b, &eng, csv, args) {
        Ok(x) => x,
        Err(e) => return eprintln!("{e}"),
    };
    let name = |g: &str| b.items.get(g).map(|i| i.name.clone()).unwrap_or_else(|| g.to_string());
    let cat_name = |id: u32| b.weapon_categories.iter().find(|(g, _)| save::item_id(g) == Some(id)).map(|(_, c)| c.name.clone()).unwrap_or_else(|| format!("{id:#x}"));
    let slot_names = ["Cut", "Pierce", "Blunt", "Fire", "Ice", "Shroud", "Shock", "Healing", "Poison", "Wood", "Stone", "Metal"];

    let Some(me) = cl::player(&hits) else { return println!("no hits with a weapon in {csv}") };
    let mine = hits.iter().filter(|h| h.attacker == me).count();
    let on_others = hits.iter().filter(|h| h.attacker != me && h.summon_of(me).is_none() && h.health < 0).count();
    println!("{} - {} (level {}, Flame {flame}{flame_from}), {} rows", s.name, save_path.display(), s.level, hits.len());
    println!("your hits {mine} (attacker {me}); damaging hits by others {on_others}; food and timed buffs not counted");
    let my_levels: Vec<f64> = hits.iter().filter(|h| h.attacker == me).filter_map(|h| h.attacker_level).collect();
    match cl::median(&my_levels) {
        Some(l) if (l - s.level as f64).abs() < 0.5 => println!("your level in the log: {l} (matches the save)"),
        Some(l) => println!("your level in the log: {l}, the save says {}: the save is from another time, or the level column is misread", s.level),
        None => println!("this log has no level columns (recorded before 2026-10-22)"),
    }
    let my_health: Vec<f64> = hits.iter().filter(|h| h.target == me).filter_map(|h| h.target_health_max).collect();
    if let Some(hp) = cl::median(&my_health) {
        let planned = eng.sheet(&build).values.get("Health").copied().unwrap_or(0.0);
        println!("your max health in the log: {hp}, the planner: {planned:.0} (food and timed buffs not counted)");
    }
    if let (Some(a), Some(z)) = (hits.first().and_then(|h| h.unix_ms), hits.last().and_then(|h| h.unix_ms)) {
        println!("logged from unix {} to {} ({:.0} min)", a / 1000, z / 1000, (z.saturating_sub(a)) as f64 / 60000.0);
    } else {
        println!("this log has no clock time: the save may be newer than the hits");
    }
    println!();
    // your summons: what the log measured next to the planner's summon hit and attack interval for the summon spells
    // the save owns (the planner assumes each summon attacks back to back; the log says how often it really does)
    let comps = cl::companions(&hits, me);
    if !comps.is_empty() {
        println!("your summons in the log ({} of them):", comps.len());
        for c in comps.iter().take(8) {
            let rate = c.per_s().map(|r| format!("one every {:.1}s over {:.0}s", 1.0 / r, c.span_s)).unwrap_or_else(|| "one swing".into());
            println!("  summon {}{}: {} swings, {rate}, median swing {:.0} ({})", c.source,
                c.level.map(|l| format!(" (level {l})")).unwrap_or_default(), c.hits, c.median_hit,
                cl::program_label(c.program).map(str::to_string).unwrap_or_else(|| format!("program {:#x}", c.program)));
        }
        let mut sb = build.clone();
        sb.summons = ammo.iter().filter(|a| b.items.get(*a).is_some_and(|i| ens_calc::is_summon_spell(&b, i))).cloned().collect();
        for x in eng.summons(&sb, &fixed.clone().unwrap_or_default()) {
            println!("  planner: {} x{:.0}: hit {:.0}, one every {:.2}s ({:.2} hits/s each) - {}", x.name, x.count, x.hit, x.interval_s,
                if x.interval_s > 0.0 { 1.0 / x.interval_s } else { 0.0 }, x.note);
        }
        println!();
    }
    for c in cl::compare(b, &eng, &build, &weapons, &ammo, &hits, me, fixed.as_ref()).iter().take(16) {
        let g = &c.group;
        let types: Vec<&str> = g.pattern.iter().map(|&k| slot_names[k]).collect();
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for h in &g.hits {
            *kinds.entry(h.kind()).or_default() += 1;
        }
        let program = cl::program_label(g.program).map(str::to_string).unwrap_or_else(|| format!("program {:#x}", g.program));
        let blast = g.blast_collisions.map(|n| format!(" ({n} {} in the blast)", if n == 1 { "thing" } else { "things" })).unwrap_or_default();
        println!("{}, {} damage, {program}{blast}, on {}: {} hits {:?}", cat_name(g.weapon_category), types.join(" + "),
            cl::faction_name(g.target_faction), g.hits.len(), kinds);
        let levels = g.target_levels().map(|(lo, hi)| if lo == hi { format!(", target level {lo}") } else { format!(", target levels {lo}-{hi}") });
        match (&fixed, c.faction) {
            _ if c.identified.is_some() => {
                let i = c.identified.as_ref().unwrap();
                let hp = g.hits.iter().find_map(|h| h.target_health_max).unwrap_or(0.0);
                let others = match (i.names.len(), i.options.len()) {
                    (1, _) => String::new(),
                    (n, 1) => format!("; all {n} types with this health have its resistances"),
                    (n, o) if i.picked => format!("; {} of the {n} types with this health have its resistances, chosen from {o} sets because it explains the damage", i.shared),
                    (n, o) => format!("; {} of the {n} types with this health have its resistances, the most common among ordinary enemies of {o} sets", i.shared),
                };
                println!("  compared against {} (max health {hp}{}){others}", i.key, levels.clone().unwrap_or_default());
            }
            (Some(t), _) => println!("  compared against {}{}", t.enemy_type.as_deref().unwrap_or("?"), levels.unwrap_or_default()),
            (None, Some((shared, of))) => println!("  compared against {} resistances (shared by {shared} of its {of} enemy types){}",
                cl::faction_name(g.target_faction), levels.unwrap_or_default()),
            (None, None) => println!("  compared against a target with no resistances (no enemy types in this faction){}", levels.unwrap_or_default()),
        }
        let (Some(m), Some(best)) = (c.measured, &c.best) else {
            println!("  {}\n", if c.measured.is_none() { "only crit or backstab hits: nothing to match a weapon on" } else { "the save has no weapon of this type with ammo that fits" });
            continue;
        };
        let msum: f64 = m.iter().sum();
        println!("  best match: {}{} ({} rarity, item level {})", name(&best.weapon.item),
            best.ammo.as_ref().map(|a| format!(" with {}", name(a))).unwrap_or_default(),
            best.weapon.rarity.clone().unwrap_or_else(|| "listed".into()), best.weapon.level.map(|l| l.to_string()).unwrap_or_else(|| "?".into()));
        let ratio = |a: f64, p: f64| if p > 0.0 { format!("{:.2}", a / p) } else { "-".into() };
        if let Some((lo, hi)) = best.range {
            println!("  priced as a weapon special (Strong Shot): the weapon base damage at AttackMod {:.1} on the average roll (the roll spans {:.0}-{:.0} on a normal hit)",
                best.attack_mod, lo * scale_of(c.kind, best), hi * scale_of(c.kind, best));
        } else if (best.attack_mod - 1.0).abs() > 1e-9 {
            println!("  priced at AttackMod {:.2} ({})", best.attack_mod, if g.blast_collisions.is_some() { "Shrapnel Blast: 10% per thing in the blast" } else { "Echoing Explosives" });
        }
        let scale = if c.kind == "crit" { best.crit_multiplier } else { 1.0 };
        let predicted = match c.kind {
            "skill shot" => best.weak,
            _ => best.normal * scale,
        };
        match best.range {
            // a random roll: each hit's implied roll must fall in [0, 1), allowing for the game's rounding
            Some((lo, _)) if lo > 0.0 => {
                let rolls: Vec<f64> = g.hits.iter().filter(|h| matches!(h.kind(), "normal" | "crit"))
                    .map(|h| h.damage() / (lo * if h.crit { best.crit_multiplier } else { 1.0 }) - 1.0).collect();
                let inside = rolls.iter().filter(|r| (-0.01..1.01).contains(*r)).count();
                let shown: Vec<String> = rolls.iter().map(|r| format!("{r:.2}")).collect();
                println!("  {} hit: measured {msum:.0}, average roll {predicted:.0}; implied rolls {}: {inside} of {} inside [0, 1)",
                    c.kind, shown.join(" "), rolls.len());
            }
            _ => println!("  {} hit: measured {msum:.0}, predicted {predicted:.0} (measured / predicted {})", c.kind, ratio(msum, predicted)),
        }
        for k in &g.pattern {
            println!("    {:7} measured {:7.1}  predicted {:7.1}", slot_names[*k], m[*k], best.per_slot[*k] * scale);
        }
        let med = |kind: &str| cl::median(&g.hits.iter().filter(|h| h.kind() == kind).map(|h| h.damage()).collect::<Vec<_>>());
        if let (Some(cr), Some(n)) = (med("crit"), med("normal")) {
            println!("  crit / normal: measured {:.2}, predicted {:.2}", cr / n, best.crit_multiplier);
        }
        if let (Some(w), Some(n)) = (med("skill shot"), med("normal")) {
            println!("  skill shot / normal: measured {:.2}, predicted {:.2}", w / n, if best.normal > 0.0 { best.weak / best.normal } else { 0.0 });
        }

        println!();
    }
}

/// `ens calc measure <log.csv> <name> [--save <ksd>] [--target <enemy>] [--flame N] [--buffs "Food;Food"]
/// [--scenario sneak|merciless] [--first-swing] [--out <dir>]`: the log's hit groups as a measurement file for review/measurements/
/// (ens_calc::measure). Prints each row beside the planner's prediction, and every group left out with the reason.
fn measure(b: &Bundle, csv: &str, name: &str, args: &[&str]) {
    use ens_calc::combat_log as cl;
    use ens_calc::measure;
    let eng = ens_calc::Engine::new(b);
    let opt = |k: &str| args.iter().position(|a| *a == k).and_then(|i| args.get(i + 1)).map(|s| s.to_string());
    let ls = match log_session(b, &eng, csv, args) {
        Ok(x) => x,
        Err(e) => return eprintln!("{e}"),
    };
    let scenario = opt("--scenario");
    if let Some(sc) = scenario.as_deref().filter(|sc| !matches!(*sc, "sneak" | "merciless" | "backstab")) {
        return eprintln!("--scenario {sc}: use sneak or merciless (the log marks crits and backstabs itself)");
    }
    let mut buffs = vec![];
    for f in opt("--buffs").unwrap_or_default().split(';').map(str::trim).filter(|f| !f.is_empty()) {
        match ens_calc::build::find_item(b, f) {
            Some(_) => buffs.push(f.to_string()),
            None => return eprintln!("--buffs: no food or elixir named {f:?}"),
        }
    }
    let file = std::path::Path::new(csv).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| csv.to_string());
    let session = measure::Session { buffs, scenario, source: format!("combat log {file}"), first_swing: args.contains(&"--first-swing") };
    let Some(me) = cl::player(&ls.hits) else { return println!("no hits with a weapon in {csv}") };
    println!("{} (level {}, Flame {}{}) from {}", ls.s.name, ls.s.level, ls.flame, ls.flame_from, ls.save_path.display());
    if session.buffs.is_empty() {
        println!("no --buffs given: the rows say no food was eaten (the save does not record food)");
    }
    let compared = cl::compare(b, &eng, &ls.build, &ls.weapons, &ls.ammo, &ls.hits, me, ls.fixed.as_ref());
    let (rows, skipped) = measure::rows(b, &eng, &ls.build, &compared, ls.fixed_key.as_deref(), &session);
    println!();
    for r in &rows {
        let f = |k: &str| r.fields.get(k).cloned().unwrap_or_default();
        let lo: f64 = f("measured_min").parse().unwrap_or(0.0);
        let hi: f64 = f("measured_max").parse().unwrap_or(0.0);
        let verdict = if r.predicted >= lo && r.predicted <= hi { "inside" } else { "OUTSIDE" };
        let seen: Vec<String> = r.values.iter().map(|(v, n)| format!("{v} x{n}")).collect();
        println!("row: {} L{} vs {}, {}: measured {lo}..{hi} over {} hits ({}); the planner predicts {} ({verdict})",
            f("weapon"), f("weapon_level"), f("target"), f("scenario"), f("n_hits"), seen.join(", "), r.predicted);
    }
    // one line per reason, naming the groups it left out
    let mut why: Vec<(&str, Vec<&str>)> = vec![];
    for k in &skipped {
        match why.iter_mut().find(|(w, _)| *w == k.why.as_str()) {
            Some((_, v)) => v.push(&k.what),
            None => why.push((&k.why, vec![&k.what])),
        }
    }
    for (w, whats) in &why {
        let shown: Vec<&str> = whats.iter().take(6).copied().collect();
        let more = if whats.len() > 6 { format!(" and {} more", whats.len() - 6) } else { String::new() };
        println!("left out ({}): {}{more}
  because {w}", whats.len(), shown.join("; "));
    }
    if rows.is_empty() {
        return println!("\nno rows: nothing written");
    }
    let dir = opt("--out").unwrap_or_else(|| "../review/measurements".into());
    let day = {
        let secs = ls.hits.last().and_then(|h| h.unix_ms).map(|ms| ms / 1000).unwrap_or_else(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
        });
        civil_date(secs)
    };
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' }).collect();
    let path = std::path::Path::new(&dir).join(format!("{day}-{slug}.csv"));
    if path.exists() {
        return eprintln!("\n{} already exists: pick another name", path.display());
    }
    let mut text = measure::COLUMNS.join(",") + "\n";
    for r in &rows {
        text += &(r.csv() + "\n");
    }
    match std::fs::write(&path, text) {
        Ok(()) => println!("\nwrote {} rows to {}", rows.len(), path.display()),
        Err(e) => eprintln!("\ncannot write {}: {e}", path.display()),
    }
}

/// YYYY-MM-DD (UTC) of a unix time.
fn civil_date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// What a group's measured kind is compared at, relative to the normal hit: crits at the crit multiplier.
fn scale_of(kind: &str, p: &ens_calc::combat_log::Predicted) -> f64 {
    if kind == "crit" { p.crit_multiplier } else { 1.0 }
}

/// Is the game running? (Writing a save while it is would be overwritten by its next save.)
fn game_running() -> bool {
    std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq enshrouded.exe", "/NH"]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("enshrouded.exe"))
        .unwrap_or(false)
}

/// `ens save set-level <characterN.ksd> <level>`: for a throwaway test character (docs/test-character.md). Refuses
/// while the game runs; keeps the file as it was in `<file>.bak` first.
/// The game did not keep a file edited this way (2026-10-22, save-character.md section 1).
fn save_set_level(path: &str, level: &str) {
    let Ok(level) = level.parse::<u32>() else { return eprintln!("level must be a number") };
    if game_running() {
        return eprintln!("the game is running: close it first, or its next save overwrites this edit");
    }
    let file = match std::fs::read(path) {
        Ok(f) => f,
        Err(e) => return eprintln!("cannot read {path}: {e}"),
    };
    let out = match ens_data::save::write::set_level(&file, level) {
        Ok(o) => o,
        Err(e) => return eprintln!("{e}"),
    };
    let check = match ens_data::save::Save::parse(&out) {
        Ok(s) => s,
        Err(e) => return eprintln!("the rewritten file does not read back ({e}); nothing written"),
    };
    let bak = format!("{path}.bak");
    if let Err(e) = std::fs::write(&bak, &file) {
        return eprintln!("cannot write {bak}: {e}");
    }
    if let Err(e) = std::fs::write(path, &out) {
        return eprintln!("cannot write {path}: {e}");
    }
    println!("{}: level {} (was kept in {bak})", check.name, check.level);
}

/// `ens save apply <characterN.ksd> <spec.json>`: the test-character edit (docs/test-character.md). The spec is
/// `{"level": 19, "nodes": [[id, level], ...], "backpack": ["record hex", ...]}` (kfc/test_character_spec.py builds
/// it). Refuses while the game runs; keeps the file as it was in `<file>.bak` first.
fn save_apply(path: &str, spec: &str) {
    use ens_data::save::{write, Save};
    if game_running() {
        return eprintln!("the game is running: close it first, or its next save overwrites this edit");
    }
    let v: serde_json::Value = match std::fs::read_to_string(spec).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return eprintln!("{spec}: {e}"),
    };
    let unhex = |h: &str| (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect::<Vec<u8>>();
    let edit = write::CharacterEdit {
        level: v.get("level").and_then(|x| x.as_u64()).map(|x| x as u32),
        nodes: v.get("nodes").and_then(|x| x.as_array()).map(|a| {
            a.iter().filter_map(|n| Some((n.get(0)?.as_u64()? as u32, n.get(1)?.as_u64()? as u8))).collect()
        }),
        backpack: v.get("backpack").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|h| h.as_str()).map(unhex).collect()).unwrap_or_default(),
    };
    let file = match std::fs::read(path) {
        Ok(f) => f,
        Err(e) => return eprintln!("cannot read {path}: {e}"),
    };
    let out = match write::edit_character(&file, &edit) {
        Ok(o) => o,
        Err(e) => return eprintln!("{e}"),
    };
    let check = match Save::parse(&out) {
        Ok(s) if s.items_error.is_none() => s,
        Ok(s) => return eprintln!("the rewritten file's items do not read back ({:?}); nothing written", s.items_error),
        Err(e) => return eprintln!("the rewritten file does not read back ({e}); nothing written"),
    };
    let bak = format!("{path}.bak");
    if let Err(e) = std::fs::write(&bak, &file) {
        return eprintln!("cannot write {bak}: {e}");
    }
    if let Err(e) = std::fs::write(path, &out) {
        return eprintln!("cannot write {path}: {e}");
    }
    println!("{}: level {}, {} nodes, {} backpack items (was kept in {bak})", check.name, check.level, check.nodes.len(), check.items.inventory.len());
}

/// Hostile enemy templates as `key<TAB>faction class boss`, sorted by key. A key is listed when it contains `filter`
/// (case-insensitive) or has every word of it ("scavenger weakling"), the same word rule `calc hit` resolves names by.
fn enemies(b: &Bundle, filter: &str) {
    let f = filter.to_lowercase();
    for (k, t) in &b.enemy_types {
        if f.is_empty() || k.to_lowercase().contains(&f) || ens_calc::build::enemy_key_matches(k, filter) {
            println!("{k}\t{} {} {}", t.faction, t.class, if t.boss { "boss" } else { "-" });
        }
    }
}

fn stats(b: &Bundle) {
    println!("game {} build {}", b.meta["version"], b.meta["build"]);
    let mut kinds: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for s in &b.skills {
        let e = kinds.entry(s.kind.as_str()).or_default();
        e.0 += 1;
        if !s.effects.is_empty() {
            e.1 += 1;
        }
        if s.unparsed.as_deref() == Some("flag") {
            e.2 += 1;
        }
        if s.unparsed.as_deref() == Some("triggered") || s.triggered {
            e.3 += 1;
        }
    }
    println!("skills: {} nodes", b.skills.len());
    for (k, (n, eff, flag, trig)) in &kinds {
        println!("  {k:13} {n:4}  attribute effects {eff:4}  flags {flag:3}  triggered {trig:3}");
    }
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    for it in b.items.values() {
        *cats.entry(format!("{} / {}", it.category.as_deref().unwrap_or("-"), it.slot.as_deref().unwrap_or("-"))).or_default() += 1;
    }
    println!("items: {}", b.items.len());
    for (k, n) in cats.iter().filter(|(_, n)| **n >= 10) {
        println!("  {k:40} {n}");
    }
    let perk_eff = b.perks.values().filter(|p| !p.effects.is_empty()).count();
    let perk_dmg = b.perks.values().filter(|p| p.damage.is_some()).count();
    println!("perks: {} (attribute effects {perk_eff}, damage distribution {perk_dmg})", b.perks.len());
    let buff_eff = b.buffs.values().filter(|p| !p.effects.is_empty()).count();
    println!("buffs: {} (with effects {buff_eff}); items applying a buff: {}", b.buffs.len(), b.items.values().filter(|i| i.buff.is_some()).count());
    println!("weapon categories: {}; attribute containers: {}; enemies: {}", b.weapon_categories.len(), b.attributes.len(), b.enemies.len());
    if let Some(r) = &b.rarity_drop_rates {
        println!("rarity drop rates (common..legendary): {r:?}");
    }
}

fn skill(b: &Bundle, name: &str) {
    for s in b.skills.iter().filter(|s| s.name.eq_ignore_ascii_case(name)).take(1) {
        println!("{} [{} {}] cost {} max {}", s.name, s.class, s.kind, s.cost, s.max);
        for e in &s.effects {
            let per_rank: Vec<String> = (1..=s.max.max(1))
                .map(|lvl| {
                    let v = e.value.as_ref().map(|v| v.eval(&|a| if a == "Level" { lvl as f64 } else { 0.0 })).unwrap_or(0.0);
                    format!("{v:.3}")
                })
                .collect();
            println!("  {:?} {} by {}", e.operation, e.attribute.as_deref().unwrap_or("?"), per_rank.join(" / "));
        }
        if let Some(u) = &s.unparsed {
            println!("  ({u}: {} program calls)", s.program.len());
        }
    }
}

fn find_item<'a>(b: &'a Bundle, name: &str, level: Option<u32>) -> Option<String> {
    let mut c: Vec<(&String, &ens_data::Item)> = b.items.iter().filter(|(_, i)| i.name == name).collect();
    c.sort_by_key(|(_, i)| i.levels.as_ref().map(|l| (l.max - l.min, l.min)).unwrap_or((0, 0)));
    if let Some(l) = level {
        if let Some((g, _)) = c.iter().find(|(_, i)| i.levels.as_ref().map(|r| r.min == l).unwrap_or(false)) {
            return Some((*g).clone());
        }
    }
    c.last().map(|(g, _)| (*g).clone())
}

/// A fresh character (no skills, food or gear but the weapon, perks locked) at `level` hitting `target` (an enemy
/// template name or "dummy"): the per-scenario damage the game shows, rounded like its damage text. Same setup rules
/// as tests/golden.rs (weapon_level doubles as the character level).
struct Fresh {
    attrs: (f64, f64, f64),
    crit_chance: f64,
    /// (scenario, predicted damage) for normal, crit, backstab, sneak
    hits: Vec<(&'static str, f64)>,
}

fn fresh(b: &Bundle, weapon: &str, level: &str, target: &str) -> Result<Fresh, String> {
    use ens_calc::{Build, Engine, Equip, Target};
    let eng = Engine::new(b);
    let level: u32 = level.parse().map_err(|_| format!("level {level:?} is not a number"))?;
    let mut build = Build::new(b, level.clamp(1, ens_calc::build::level_cap(b)));
    let mut parts = weapon.splitn(2, " + ");
    let wname = parts.next().unwrap_or("").trim();
    let wg = ens_calc::build::find_item(b, wname).ok_or_else(|| format!("no weapon named {wname:?}"))?;
    build.weapon = Some(Equip { item: wg, rarity: None, level: None, perks: Some(0) });
    if let Some(a) = parts.next() {
        build.ammo = Some(ens_calc::build::find_item(b, a.trim()).ok_or_else(|| format!("no ammo or spell named {:?}", a.trim()))?);
    }
    let mut t = Target::default();
    if target != "dummy" {
        let key = ens_calc::build::find_enemy(b, target)?;
        let et = &b.enemy_types[&key];
        t.susceptibility = et.susceptibility.clone();
        t.faction = Some(et.faction.clone());
        t.enemy_class = Some(et.class.clone());
        t.flying = et.flying;
        t.enemy_type = Some(key);
    }
    let sheet = eng.sheet(&build);
    let attr = |k: &str| sheet.values.get(k).copied().unwrap_or(0.0);
    let w = build.weapon.clone().unwrap();
    let mut out = Fresh { attrs: (attr("Strength"), attr("Dexterity"), attr("Intelligence")), crit_chance: 0.0, hits: vec![] };
    for (name, kind) in [("normal", None), ("backstab", Some("Backstab")), ("sneak", Some("Sneak"))] {
        t.hit_kind = kind.map(String::from);
        let h = eng.hit(&build, &w, build.ammo.as_deref(), 1.0, &t);
        let armor = if h.expected > 0.0 { h.after_armor / h.expected } else { 1.0 };
        out.hits.push((name, (h.non_crit * armor).round()));
        if name == "normal" {
            out.crit_chance = h.crit_chance;
            out.hits.push(("crit", (h.non_crit * h.crit_multiplier * armor).round()));
        }
    }
    Ok(out)
}

fn hit(b: &Bundle, weapon: &str, level: &str, target: &str) {
    match fresh(b, weapon, level, target) {
        Err(e) => eprintln!("{e}"),
        Ok(f) => {
            println!("{weapon}, fresh character level {level} (Str {} Dex {} Int {}), vs {target}; crit chance {:.0}%",
                f.attrs.0, f.attrs.1, f.attrs.2, f.crit_chance * 100.0);
            for (s, v) in &f.hits {
                println!("  {s:9} {v}");
            }
        }
    }
}

/// Compare a combat log from ens_hook.dll with the fresh-character prediction. Hits are grouped by the two entity ids
/// the game records; the group whose normal hits sit closest to the prediction is taken as you hitting the target.
/// Prints every group, then that group's rows in review/measurements/ format (the CSV columns of golden_vectors.csv
/// without vector_id and source_file).
fn log(b: &Bundle, csv: &str, weapon: &str, level: &str, target: &str) {
    let f = match fresh(b, weapon, level, target) {
        Ok(f) => f,
        Err(e) => return eprintln!("{e}"),
    };
    let Ok(text) = std::fs::read_to_string(csv) else { return eprintln!("cannot read {csv}") };
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let col = |n: &str| header.iter().position(|h| *h == n);
    let (Some(ea), Some(eb), Some(hc), Some(cr), Some(bs)) = (col("entity_a"), col("entity_b"), col("health_change"), col("crit"), col("backstab")) else {
        return eprintln!("{csv} is not a combat log from ens_hook.dll");
    };
    // columns added later; absent in older logs, where `extra` stays empty
    let (bl, nd, lx) = (col("blocked"), col("no_damage"), col("loss_x10"));
    // (entity_a, entity_b) -> (blocked hits, no-damage hits, summed loss_x10 / 10)
    let mut extra: BTreeMap<(i64, i64), (usize, usize, f64)> = BTreeMap::new();
    // (entity_a, entity_b) -> scenario -> damages
    let mut groups: BTreeMap<(i64, i64), BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    for l in lines {
        let c: Vec<&str> = l.split(',').collect();
        if bl.is_some() || nd.is_some() || lx.is_some() {
            let get = |i: Option<usize>| i.and_then(|i| c.get(i)).copied();
            let id = |i: usize| -> i64 { c.get(i).and_then(|v| v.parse().ok()).unwrap_or(0) };
            let e = extra.entry((id(ea), id(eb))).or_default();
            e.0 += (get(bl) == Some("true")) as usize;
            e.1 += (get(nd) == Some("true")) as usize;
            e.2 += get(lx).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 10.0;
        }
        let Some(dmg) = c.get(hc).and_then(|v| v.parse::<f64>().ok()).filter(|v| *v < 0.0).map(|v| -v) else { continue };
        let key = (c[ea].parse().unwrap_or(0), c[eb].parse().unwrap_or(0));
        let scen = match (c[cr] == "true", c[bs] == "true") {
            (false, false) => "normal",
            (true, false) => "crit",
            (false, true) => "backstab",
            (true, true) => "backstab+crit",
        };
        groups.entry(key).or_default().entry(scen).or_default().push(dmg);
    }
    let want = |s: &str| f.hits.iter().find(|h| h.0 == s).map(|h| h.1);
    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    let mut best: Option<((i64, i64), f64)> = None;
    for (k, sc) in &groups {
        println!("entities {} -> {}:", k.0, k.1);
        for (s, v) in sc {
            let lo = v.iter().cloned().fold(f64::MAX, f64::min);
            let hi = v.iter().cloned().fold(0.0, f64::max);
            let p = want(s).map(|p| format!("predicted {p}")).unwrap_or_default();
            println!("  {s:13} {} hits, {lo}..{hi} {p}", v.len());
        }
        if let Some((nb, nn, loss)) = extra.get(k) {
            println!("  blocked {nb}, no damage {nn}, total health + stamina loss {loss}");
        }
        if let (Some(v), Some(p)) = (sc.get("normal"), want("normal")) {
            let d = (median(v) - p).abs() / p.max(1.0);
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((*k, d));
            }
        }
    }
    let Some((k, d)) = best else { return println!("no normal hits to compare") };
    println!("\nyour hits are most likely entities {} -> {} (normal hits {:.0}% from the prediction)", k.0, k.1, d * 100.0);
    // golden_vectors.csv's weapon_level is both the item and the character level; a fresh character's damage does
    // not depend on its own level (no attribute points), so record the weapon's real item level
    let wname = weapon.split(" + ").next().unwrap_or("").trim();
    let item_level = ens_calc::build::find_item(b, wname)
        .and_then(|g| b.items.get(&g))
        .and_then(|i| i.levels.as_ref())
        .map(|l| level.parse::<u32>().unwrap_or(1).clamp(l.min, l.max))
        .unwrap_or(1);
    println!("patch,weapon,weapon_level,perks,str,dex,int,skills,buffs,target,scenario,measured_min,measured_max,n_hits,video_link");
    for (s, v) in &groups[&k] {
        if s.contains('+') {
            continue;
        }
        let lo = v.iter().cloned().fold(f64::MAX, f64::min);
        let hi = v.iter().cloned().fold(0.0, f64::max);
        println!("{},\"{weapon}\",{item_level},0,{},{},{},,,{target},{s},{lo},{hi},{},{csv}", b.meta["version"].as_str().unwrap_or("?"),
            f.attrs.0, f.attrs.1, f.attrs.2, v.len());
    }
}

fn sample(b: &Bundle) {
    use ens_calc::{Build, Engine, Equip, Target};
    let eng = Engine::new(b);
    let mut build = Build::new(b, 50);
    build.flame_level = 10;
    for i in 0..b.skills.len() {
        let s = &b.skills[i];
        if s.name == "Dexterity" { build.ranks[i] = 1; }
    }
    for (n, r) in [("Marksman", 3), ("Sniper", 3), ("Fatal Precision", 3), ("Skill Shot", 3), ("Graceful Stride", 1), ("Ranger", 3), ("Multi Shot", 1)] {
        build.set_rank_by_name(b, n, r);
    }
    build.weapon = find_item(b, "Tarred Bow", None).map(|g| Equip { item: g, rarity: Some("Legendary".into()), level: Some(50), perks: None });
    build.ammo = find_item(b, "Eternal Frost Arrow III", None);
    build.foods = ["Pumpkin Soup", "Chicken Soup"].iter().filter_map(|n| find_item(b, n, None)).collect();
    let sheet = eng.sheet(&build);
    println!("points {}", build.points(b));
    let mut keys: Vec<_> = sheet.values.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in keys { println!("  {k:24} {v:.3}"); }
    let target = Target::default();
    let w = build.weapon.clone().unwrap();
    let h = eng.hit(&build, &w, build.ammo.as_deref(), 1.0, &target);
    println!("hit: base {} item {} + ammo {} attack x{}", h.base_damage, h.item_damage, h.ammo_damage, h.attack_mod);
    for (k, v) in &h.pool_terms { println!("  pool {k:40} {v:+.3}"); }
    for (k, v) in &h.per_type { println!("  {k:10} {v:.1}"); }
    println!("  non-crit {:.1}  crit {:.0}% x{:.2}  expected {:.1}", h.non_crit, h.crit_chance * 100.0, h.crit_multiplier, h.expected);
    let mut bb = build.clone();
    bb.set_rank_by_name(b, "Blessed Arrows", 3);
    bb.set_rank_by_name(b, "Vitality Surge", 3);
    let tot = eng.total(&bb, &target);
    for p in &tot.procs { println!("proc {} {} +{} x chance {} {} -> {:.2}/s", p.skill, p.resource, p.amount, p.chance, if p.on_crit { "on crit" } else { "on hit" }, p.per_s); }
    println!("bow mana: needs {:.1}/s, uptime {:.0}%, fallback {:?}, total dps {:.1}", tot.main_mana_per_s, tot.main_uptime * 100.0, tot.fallback, tot.total);
    let prof = eng.attack_profile(&build, build.ammo.as_deref(), "Primary");
    println!("projectiles per attack {:.2}: {:?}", prof.projectiles, prof.parts);
    let mut b2 = build.clone();
    b2.set_rank_by_name(b, "Bow Specialization", 1);
    let (secs, how) = eng.attack_interval(&b2, &w, b2.ammo.as_deref());
    println!("attack cycle {secs:.2}s ({how})");
    let d = eng.dps(&b2, &w, b2.ammo.as_deref(), &target, 1.0 / secs);
    println!("dps (+Bow Specialization): regular {:.1} special {:.1} (focus {}/{} -> {:.3}/s, {:.2} arrows x{}) total {:.1}", d.regular_dps, d.special_dps, d.focus_per_hit, d.focus_cost, d.special_per_s, d.special_projectiles, d.special_attack_mod, d.total);
    let st = eng.attributes(&build);
    for c in st.sources("CritChance_Adder_Ranged").iter().chain(st.sources("WeaponCategoryDamageMod_Bow").iter()).chain(st.sources("RangedDamageMod").iter()) {
        println!("  src {:32} {:?} {:.3}  <- {}", c.attribute, c.op, c.value, c.source);
    }
}

fn validate(b: &Bundle) {
    let r = ens_calc::validate::run(b);
    println!("skills checked {} ({} ranks): {} mismatches", r.skills_checked, r.skill_ranks_checked, r.skill_mismatches.len());
    for m in &r.skill_mismatches { println!("  S {m}"); }
    println!("absolute-unit effects matched against start value: {}", r.relative_matches.len());
    for m in &r.relative_matches { println!("  R {m}"); }
    println!("hidden effects (not in tooltip): {}", r.hidden_effects.len());
    for m in &r.hidden_effects { println!("  H {m}"); }
    println!("triggered/flag skills checked {}: {} tooltip numbers not found in program", r.triggered_checked, r.triggered_missing.len());
    for m in r.triggered_missing.iter().take(120) { println!("  T {m}"); }
    println!("perks checked {}: {} mismatches", r.perks_checked, r.perk_mismatches.len());
    for m in r.perk_mismatches.iter().take(80) { println!("  P {m}"); }
}

fn opt(b: &Bundle, level: u32, gear: bool) {
    use ens_calc::{Build, Target};
    use ens_opt::{Goal, Optimizer, Plan};
    let sample = Build::sample_bow(b);
    let o = Optimizer::new(b);
    let plan = Plan {
        level,
        flame_level: (level / 5).max(1),
        weapon: sample.weapon.clone(),
        ammo: sample.ammo.clone(),
        gear: sample.gear.clone(),
        foods: sample.foods.clone(),
        summons: sample.summons.clone(),
        ranking: vec![Goal::Damage, Goal::Survival, Goal::Movement, Goal::Sustain, Goal::Utility],
        attacks_per_s: 1.0,
        target: Target::default(),
        found_points: None,
        weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: Default::default(),
    };
    let t0 = std::time::Instant::now();
    let (best, order, chosen) = if gear { o.optimize_with_gear(&plan, 20, 7, "Legendary", 2) } else { let (s, ord) = o.optimize(&plan, 20, 7); (s, ord, plan.clone()) };
    if gear {
        let name = |e: &Option<ens_calc::Equip>| e.as_ref().and_then(|e| b.items.get(&e.item)).map(|i| format!("{} ({})", i.name, i.rarity.clone().unwrap_or_default())).unwrap_or_else(|| "-".into());
        println!("weapon {}", name(&chosen.weapon));
        for (k, e) in chosen.gear.iter().enumerate() { println!("gear {} {}", ens_calc::build::GEAR_SLOTS[k], name(e)); }
    }
    let pts: u32 = best.ranks.iter().enumerate().map(|(i, r)| *r as u32 * b.skills[i].cost).sum();
    println!("level {level}: {pts} / {} points, connected {}, {:.1}s", ens_opt::level_points(level), o.connected(&best.ranks), t0.elapsed().as_secs_f64());
    let mut gs: Vec<_> = best.goals.iter().collect();
    gs.sort_by(|a, b| a.0.cmp(b.0));
    println!("goals {gs:?}");
    let mut names: std::collections::BTreeMap<&str, u32> = Default::default();
    for (i, r) in best.ranks.iter().enumerate() {
        if *r > 0 && b.skills[i].kind != "Attribute" { *names.entry(b.skills[i].name.as_str()).or_default() += *r as u32; }
    }
    println!("skills {names:?}");
    println!("first 15 steps: {}", order.iter().take(15).map(|&i| b.skills[i].name.as_str()).collect::<Vec<_>>().join(" > "));
}

fn summoner(b: &Bundle) {
    use ens_calc::{build::find_item, Build, Engine, Equip, Target};
    let eng = Engine::new(b);
    let mut s = Build::new(b, 50);
    s.flame_level = 10;
    for (n, r) in [("Advanced Necromancy", 3), ("Totem Mastery", 3), ("Iceman", 3), ("Mage Apprentice", 3), ("Mana Currents", 3)] { s.set_rank_by_name(b, n, r); }
    let mut spirit = 0;
    for (i, sk) in b.skills.iter().enumerate() { if sk.name == "Spirit" && spirit < 10 { s.ranks[i] = 1; spirit += 1; } }
    s.weapon = find_item(b, "Eldermere's Staff").map(|g| Equip { item: g, rarity: Some("Legendary".into()), level: Some(50), perks: None });
    s.ammo = find_item(b, "Eternal Ice Bolt IV");
    s.summons = ["Eternal Hollow Wretch II", "Eternal Ice Totem II"].iter().filter_map(|n| find_item(b, n)).collect();
    for x in eng.summons(&s, &Target::default()) {
        println!("{} x{}: hit {:.1}, {} -> {:.1} each, mana {:.1}/s, uptime {:.0}% -> {:.1} DPS", x.name, x.count, x.hit, x.note, x.each_dps, x.mana_per_s, x.uptime * 100.0, x.total_dps);
    }
    let t = eng.total(&s, &Target::default());
    let sh = eng.sheet(&s);
    println!("mana pool {:.0}, regen {:.1}/s; staff dps {:.1} needs {:.1} mana/s, uptime {:.0}%; TOTAL {:.1}", sh.values["Mana"], sh.values["Mana regen /s"], t.main.total, t.main_mana_per_s, t.main_uptime * 100.0, t.total);
}

/// The user's proof file: the provenance ledger plus the latest coverage audit.
fn write_proof() {
    let md = ens_calc::proof::markdown();
    std::fs::write("../docs/research/provenance.md", &md).expect("write ledger");
    let cov = std::fs::read_to_string("../docs/research/coverage.md").unwrap_or_default();
    let full = if cov.is_empty() { md.clone() } else { format!("{md}\n{}", cov.replacen("# Coverage", "# Coverage: which picks change the numbers", 1)) };
    std::fs::write("../Calculation Proof.md", &full).expect("write proof file");
    println!("{}", md.lines().find(|l| l.contains(" proven · ")).unwrap_or(""));
}

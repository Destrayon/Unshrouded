//! Build optimizer: searches the skill tree for the best build under ranked goals, using `ens-calc` for the numbers.
//!
//! Search (ported from the planner's JS optimizer): greedy purchases of whole path bundles (cheapest connected path
//! to a node, scored by gain per point) with a two-step lookahead, then local search (drop 1-4 removable ranks,
//! regreedy, accept by annealing). The leveling order replays the greedy at increasing point budgets.

use ens_calc::{Build, Engine, Equip, Target};
use ens_data::Bundle;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::cmp::Reverse;

/// Goals the player can rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Goal {
    Damage,
    Survival,
    Movement,
    Sustain,
    Utility,
}

pub const GOALS: [Goal; 5] = [Goal::Damage, Goal::Survival, Goal::Movement, Goal::Sustain, Goal::Utility];

impl Goal {
    pub fn label(self) -> &'static str {
        match self {
            Goal::Damage => "Damage",
            Goal::Survival => "Survival",
            Goal::Movement => "Movement",
            Goal::Sustain => "Sustain",
            Goal::Utility => "Utility",
        }
    }
}

/// What the build is for.
#[derive(Debug, Clone)]
pub struct Plan {
    pub level: u32,
    pub flame_level: u32,
    pub weapon: Option<Equip>,
    pub ammo: Option<String>,
    pub gear: Vec<Option<Equip>>,
    pub foods: Vec<String>,
    pub summons: Vec<String>,
    /// Goals in priority order.
    pub ranking: Vec<Goal>,
    /// Attacks per second of the main attack (from the weapon type; see `attack_rate`).
    pub attacks_per_s: f64,
    pub target: Target,
    /// Skill points found (Shroud Roots, Elixir Wells) when known; see `points_available`.
    pub found_points: Option<u32>,
    /// The weapons the build fights with, each with its share of fight time. Empty: the one `weapon` / `ammo` above.
    /// `prepare` fills this (and keeps `weapon` / `ammo` as the main weapon, the one with the largest share).
    pub weapons: Vec<WeaponPlan>,
    /// Skill nodes the tree must have (bought with their path before anything else) and nodes it must not have.
    pub required: Vec<usize>,
    pub avoided: Vec<usize>,
    /// Rarity assumed for items the optimizer picks that roll their rarity; None keeps the item's listed rarity.
    pub rarity: Option<String>,
    /// Which summon spells the build keeps up (docs/plans/optimizer-summons.md); `Keep` uses `summons` as given.
    pub summon_choice: SummonChoice,
}

/// The summons the optimizer works with.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", content = "families", rename_all = "lowercase")]
pub enum SummonChoice {
    /// The plan's `summons` as they are (the open loadout's).
    #[default]
    Keep,
    /// Summon families ("Ice Totem", "Hollow Wretch"), each at the highest tier the level allows (none: no summons).
    Fixed(Vec<String>),
    /// The summon families that raise the score most, chosen with the tree and gear.
    Best,
}

/// A weapon the build fights with.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct WeaponPlan {
    /// Weapon category as the game data names it ("Bow", "Two-Handed Axe"); None takes the category of `weapon`.
    #[serde(default)]
    pub category: Option<String>,
    /// A weapon to use; None lets the optimizer pick the best of the category the level allows.
    #[serde(default)]
    pub weapon: Option<Equip>,
    /// Set by `prepare` when the optimizer picked the weapon, so the gear search may change it.
    #[serde(default)]
    pub chosen: bool,
    /// Arrow or spell family ("Explosive Arrow"): the highest tier the level allows.
    #[serde(default)]
    pub ammo_family: Option<String>,
    /// A fixed arrow or spell. With neither this nor a family, the best one the level allows.
    #[serde(default)]
    pub ammo: Option<String>,
    /// Set by `prepare` when it picked the ammo (from a family or open), so a later `prepare` picks again.
    #[serde(default)]
    pub ammo_chosen: bool,
    /// Share of fight time with this weapon; shares are normalised over the plan's weapons.
    #[serde(default = "one")]
    pub share: f64,
}

fn one() -> f64 {
    1.0
}

/// A level to plan for, with the Flame level the character will have by then.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Milestone {
    pub level: u32,
    pub flame_level: u32,
}

/// Default milestones: the last level of each region band in the gear guide (`docs/research/enshrouded-bow-gear-2026-10-19.md`:
/// Springlands 1–9, Revelwood 10–15 at Flame 3, Nomad Highlands 16–20 at Flame 4, Kindlewastes 21–27 at Flame 5,
/// Albaneve Summits 28–35 at Flame 6, Veilwater Basin 36–45 at Flame 8, Cinder's Cradle 46–50 at Flame 10).
/// Community guidance on pacing, not game data.
pub const DEFAULT_MILESTONES: [Milestone; 7] = [
    Milestone { level: 9, flame_level: 2 },
    Milestone { level: 15, flame_level: 3 },
    Milestone { level: 20, flame_level: 4 },
    Milestone { level: 27, flame_level: 5 },
    Milestone { level: 35, flame_level: 6 },
    Milestone { level: 45, flame_level: 8 },
    Milestone { level: 50, flame_level: 10 },
];

#[derive(Debug, Clone)]
pub struct MilestoneOptions {
    /// Also pick gear and weapons for each milestone.
    pub gear: bool,
    /// Rebuild the tree from scratch at each milestone instead of growing the previous one.
    pub respec: bool,
    /// Local-search rounds per tree search.
    pub iters: usize,
}

/// One milestone's result.
#[derive(Debug, Clone)]
pub struct MilestoneResult {
    pub level: u32,
    pub flame_level: u32,
    pub budget: u32,
    pub spent: u32,
    pub ranks: Vec<u8>,
    /// Purchases since the previous milestone, in order (node indices, one entry per rank).
    pub added: Vec<usize>,
    /// Every purchase from an empty tree to this milestone's tree, in order.
    pub order: Vec<usize>,
    /// Nodes the previous milestone had more ranks in (only with respec).
    pub removed: Vec<usize>,
    pub goals: HashMap<&'static str, f64>,
    /// The plan as used: level, Flame level, gear and resolved weapons.
    pub plan: Plan,
}

/// An arrow or spell name without its tier ("Explosive Arrow II" → "Explosive Arrow").
pub fn ammo_family(name: &str) -> &str {
    for suffix in [" V", " IV", " III", " II", " I"] {
        if let Some(base) = name.strip_suffix(suffix) {
            return base;
        }
    }
    name
}

/// Skill points available at a character level: 2 per level, plus Shroud Roots and Elixir Wells spread over the
/// journey (212 at level 50: 98 + 60 + 54).
pub fn level_points(level: u32) -> u32 {
    let l = level.max(1) as f64;
    (2.0 * (l - 1.0) + 114.0 * (l - 1.0) / 49.0).round() as u32
}

/// Skill points to spend: 2 per level after the first (balancing skillPointsPerLevel) plus the points from what the
/// character has found when that is known, else `level_points`' even spread.
pub fn points_available(level: u32, found: Option<u32>) -> u32 {
    match found {
        Some(f) => 2 * (level.max(1) - 1) + f,
        None => level_points(level),
    }
}

/// Worker threads for the search; 0 = one per core. Tests set 1 to compare against the one-at-a-time search.
pub static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `f` over `items` on all cores, results in input order. Small inputs run on the calling thread.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let cap = THREADS.load(std::sync::atomic::Ordering::Relaxed);
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let threads = (if cap == 0 { cores } else { cap }).min(items.len());
    if threads <= 1 || items.len() < 8 {
        return items.iter().map(&f).collect();
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|sc| {
        let handles: Vec<_> = items.chunks(chunk).map(|c| sc.spawn(|| c.iter().map(&f).collect::<Vec<R>>())).collect();
        handles.into_iter().flat_map(|h| h.join().expect("optimizer worker panicked")).collect()
    })
}

/// Attacks per second from the item data (see `Engine::attack_interval`).
pub fn attack_rate(engine: &Engine, build: &Build, weapon: &Equip, ammo: Option<&str>) -> f64 {
    1.0 / engine.attack_interval(build, weapon, ammo).0.max(0.05)
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub ranks: Vec<u8>,
    pub score: f64,
    pub goals: HashMap<&'static str, f64>,
}

pub struct Optimizer<'a> {
    pub bundle: &'a Bundle,
    pub engine: Engine<'a>,
    pub adj: Vec<Vec<usize>>,
    pub roots: Vec<usize>,
    /// Utility value per rank (seconds saved per hour; ASSUMED, carried over from the JS planner).
    utility: HashMap<&'static str, f64>,
}

impl<'a> Optimizer<'a> {
    pub fn new(bundle: &'a Bundle) -> Self {
        let n = bundle.skills.len();
        let idx: HashMap<u32, usize> = bundle.skills.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
        let mut adj = vec![Vec::new(); n];
        for l in &bundle.skill_links {
            let a = l.get("sourceNode").and_then(|x| x.get("value")).and_then(|x| x.as_u64());
            let b = l.get("targetNode").and_then(|x| x.get("value")).and_then(|x| x.as_u64());
            if let (Some(a), Some(b)) = (a, b) {
                if let (Some(&i), Some(&j)) = (idx.get(&(a as u32)), idx.get(&(b as u32))) {
                    adj[i].push(j);
                    adj[j].push(i);
                }
            }
        }
        let roots = bundle.skills.iter().enumerate().filter(|(_, s)| s.kind == "Root").map(|(i, _)| i).collect();
        let utility = HashMap::from([
            ("Well Rested", 25.0), ("Inner Fires", 25.0), ("Good Metabolism", 50.0), ("Quality Gear", 20.0), ("Sweet Tooth", 10.0),
            ("Last Meal", 40.0), ("Calm Spirit", 20.0), ("Vukah Language", 20.0), ("Mithridatist", 10.0), ("Eagle Eye", 30.0),
            ("Shroud Mitigation", 10.0), ("Eternal Spark", 15.0), ("Saviour", 12.0), ("Healing Revive", 6.0), ("Water Aura", 15.0),
            ("Life Burst", 30.0), ("Lumberjack", 8.0), ("Mason", 8.0), ("Prospector", 8.0),
        ]);
        Optimizer { bundle, engine: Engine::new(bundle), adj, roots, utility }
    }

    fn build_of(&self, plan: &Plan, ranks: &[u8]) -> Build {
        let mut b = Build::new(self.bundle, plan.level);
        b.flame_level = plan.flame_level;
        b.ranks = ranks.to_vec();
        b.weapon = plan.weapon.clone();
        b.ammo = plan.ammo.clone();
        b.gear = plan.gear.clone();
        b.foods = plan.foods.clone();
        b.summons = plan.summons.clone();
        b
    }

    /// The plan's weapons with normalised shares: (weapon, ammo, share). One entry for a plan without `weapons`.
    pub fn weapon_mix(&self, plan: &Plan) -> Vec<(Option<Equip>, Option<String>, f64)> {
        if plan.weapons.is_empty() {
            return vec![(plan.weapon.clone(), plan.ammo.clone(), 1.0)];
        }
        let total: f64 = plan.weapons.iter().map(|w| w.share.max(0.0)).sum();
        let n = plan.weapons.len() as f64;
        plan.weapons
            .iter()
            .map(|w| (w.weapon.clone(), w.ammo.clone(), if total > 0.0 { w.share.max(0.0) / total } else { 1.0 / n }))
            .collect()
    }

    /// Goal values for a build:
    /// * Damage = kills per hour against an enemy of the plan's level (enemy health from the game's formula, health
    ///   factor 0.85 typical; 3 s between fights; 65% of play time spent fighting/travelling between fights — ASSUMED);
    /// * Survival = enemy hits survived (enemy hit from the game's formula, reduced by the build's armor);
    /// * Movement = sprint seconds from stamina pool + regeneration; Sustain = mana + stamina regeneration;
    /// * Utility = seconds per hour from quality-of-life skills (ASSUMED values).
    pub fn goals(&self, plan: &Plan, ranks: &[u8]) -> HashMap<&'static str, f64> {
        let b = self.build_of(plan, ranks);
        let sheet = self.engine.sheet(&b);
        let g = |k: &str| sheet.values.get(k).copied().unwrap_or(0.0);
        let mut out = HashMap::new();
        // several weapons: damage is each weapon's DPS (with its own ammo) weighted by its share of fight time; the
        // other goals use the main weapon in hand
        let mix = self.weapon_mix(plan);
        let dps = if mix.len() == 1 {
            self.engine.total(&b, &plan.target).total
        } else {
            mix.iter()
                .map(|(w, a, share)| {
                    let mut bw = b.clone();
                    (bw.weapon, bw.ammo) = (w.clone(), a.clone());
                    share * self.engine.total(&bw, &plan.target).total
                })
                .sum()
        };
        let hp = ens_calc::enemy_health(self.bundle, plan.level, 0.85);
        let ttk = if dps > 0.0 { hp / dps } else { 1e6 };
        out.insert("Damage", 3600.0 * 0.65 / (ttk + 3.0));
        let (_, taken) = self.engine.incoming_target(&b, plan.level, &plan.target);
        out.insert("Survival", g("Health") / taken.max(1.0));
        // sprint costs ~15 stamina/s (ASSUMED): seconds of sprint from a full bar + sustained share from regeneration
        out.insert("Movement", g("Stamina") / 15.0 + g("Stamina regen /s") / 15.0 * 30.0);
        out.insert("Sustain", g("Mana regen /s") * 60.0 + g("Mana") + g("Stamina regen /s") * 10.0);
        let ranks_eff = self.engine.effective_ranks(&b);
        let util: f64 = self.bundle.skills.iter().enumerate().map(|(i, s)| self.utility.get(s.name.as_str()).copied().unwrap_or(0.0) * ranks_eff[i] as f64).sum();
        out.insert("Utility", util);
        out
    }

    /// Weighted score relative to the empty build: each goal as a ratio gain, weighted by rank (1/position).
    pub fn score(&self, plan: &Plan, ranks: &[u8], base: &HashMap<&'static str, f64>) -> Scored {
        let goals = self.goals(plan, ranks);
        let mut score = 0.0;
        let n = plan.ranking.len().max(1) as f64;
        let wsum: f64 = (1..=plan.ranking.len()).map(|i| 1.0 / i as f64).sum();
        for (pos, goal) in plan.ranking.iter().enumerate() {
            let w = (1.0 / (pos + 1) as f64) / wsum * n;
            let k = goal.label();
            let v = goals.get(k).copied().unwrap_or(0.0);
            let b = base.get(k).copied().unwrap_or(0.0);
            // log gains: equal value for doubling any goal, diminishing returns within a goal
            let gain = if k == "Utility" { v / 1800.0 } else if b > 0.0 && v > 0.0 { (v / b).ln() } else { 0.0 };
            score += w * gain;
        }
        Scored { ranks: ranks.to_vec(), score, goals }
    }

    fn owned(&self, ranks: &[u8], i: usize) -> bool {
        self.bundle.skills[i].kind == "Root" || ranks[i] > 0
    }

    fn cost(&self, ranks: &[u8]) -> u32 {
        ranks.iter().enumerate().map(|(i, r)| *r as u32 * self.bundle.skills[i].cost).sum()
    }

    /// Cheapest path costs to every node from the owned set.
    fn paths(&self, ranks: &[u8], blocked: &[usize]) -> (Vec<u32>, Vec<Option<usize>>) {
        let n = ranks.len();
        let mut dist = vec![u32::MAX; n];
        let mut prev = vec![None; n];
        let mut heap = BinaryHeap::new();
        for i in 0..n {
            if self.owned(ranks, i) {
                dist[i] = 0;
                heap.push(Reverse((0, i)));
            }
        }
        while let Some(Reverse((d, u))) = heap.pop() {
            if d > dist[u] {
                continue;
            }
            for &v in &self.adj[u] {
                if self.owned(ranks, v) || blocked.contains(&v) {
                    continue;
                }
                let nd = d + self.bundle.skills[v].cost;
                if nd < dist[v] {
                    dist[v] = nd;
                    prev[v] = Some(u);
                    heap.push(Reverse((nd, v)));
                }
            }
        }
        (dist, prev)
    }

    fn bundle_to(&self, ranks: &[u8], v: usize, prev: &[Option<usize>]) -> Vec<usize> {
        let mut path = vec![];
        let mut u = Some(v);
        while let Some(x) = u {
            if self.owned(ranks, x) {
                break;
            }
            path.push(x);
            u = prev[x];
        }
        path.reverse();
        path
    }

    /// Greedy purchases up to `budget` points; returns final ranks and the purchase order (node indices).
    pub fn greedy(&self, plan: &Plan, start: &[u8], budget: u32, base: &HashMap<&'static str, f64>) -> (Vec<u8>, Vec<usize>) {
        let mut ranks = start.to_vec();
        let mut order = vec![];
        let mut cur = self.score(plan, &ranks, base).score;
        loop {
            let spent = self.cost(&ranks);
            if spent >= budget {
                break;
            }
            let left = budget - spent;
            let (dist, prev) = self.paths(&ranks, &plan.avoided);
            let mut cands: Vec<(Vec<usize>, u32)> = vec![];
            for v in 0..ranks.len() {
                let s = &self.bundle.skills[v];
                if s.kind == "Root" || ranks[v] as u32 >= s.max.max(1) || plan.avoided.contains(&v) {
                    continue;
                }
                let (steps, c) = if ranks[v] > 0 { (vec![v], s.cost) } else if dist[v] != u32::MAX { (self.bundle_to(&ranks, v, &prev), dist[v]) } else { continue };
                if c == 0 || c > left {
                    continue;
                }
                cands.push((steps, c));
            }
            // evaluated on all cores; the pick below walks them in node order, so ties go to the lowest node as before
            let scores = par_map(&cands, |(steps, _)| {
                let mut r = ranks.clone();
                for &x in steps {
                    r[x] += 1;
                }
                self.score(plan, &r, base).score
            });
            let mut best: Option<(f64, Vec<usize>, f64)> = None;
            for ((steps, c), f) in cands.into_iter().zip(scores) {
                let gain = (f - cur) / c as f64;
                if best.as_ref().map(|b| gain > b.0).unwrap_or(true) {
                    best = Some((gain, steps, f));
                }
            }
            match best {
                Some((g, steps, f)) if g > 1e-9 => {
                    for &x in &steps {
                        ranks[x] += 1;
                        order.push(x);
                    }
                    cur = f;
                }
                _ => break,
            }
        }
        (ranks, order)
    }

    /// Best build at a level: greedy plus local search.
    pub fn optimize(&self, plan: &Plan, iters: usize, seed: u64) -> (Scored, Vec<usize>) {
        let empty = vec![0u8; self.bundle.skills.len()];
        self.optimize_from(plan, &empty, &empty, iters, seed)
    }

    /// Best build at a level grown from `start`, never going below `locked` (ranks the player keeps, e.g. the tree of
    /// the previous milestone): greedy plus local search. The order lists the purchases from `start` to the result.
    pub fn optimize_from(&self, plan: &Plan, start: &[u8], locked: &[u8], iters: usize, seed: u64) -> (Scored, Vec<usize>) {
        let empty = vec![0u8; self.bundle.skills.len()];
        let base = self.goals(plan, &empty);
        let budget = points_available(plan.level, plan.found_points);
        let begin = start.to_vec();
        let start = self.with_required(plan, start, budget);
        let (mut ranks, _) = self.greedy(plan, &start, budget, &base);
        let mut best = self.score(plan, &ranks, &base);
        let mut rng = seed.max(1);
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..iters {
            let mut cand = best.ranks.clone();
            let drops = 1 + (next() % 4) as usize;
            for _ in 0..drops {
                let rm: Vec<usize> = self.removable(&cand).into_iter()
                    .filter(|&i| !(plan.required.contains(&i) && cand[i] == 1) && cand[i] > locked.get(i).copied().unwrap_or(0))
                    .collect();
                if rm.is_empty() {
                    break;
                }
                let i = rm[(next() % rm.len() as u64) as usize];
                cand[i] -= 1;
            }
            let (r2, _) = self.greedy(plan, &cand, budget, &base);
            let s = self.score(plan, &r2, &base);
            if s.score > best.score + 1e-9 {
                best = s;
                ranks = r2;
            }
        }
        // from where the caller started, so must-have nodes bought up front are in the order too
        let order = self.leveling_order(plan, &begin, &ranks);
        (best, order)
    }

    /// The order to buy `target` in, from `start` (ranks `start` has beyond `target` are dropped first): at each
    /// step the node (with its path inside the target tree) that gains most per point. One entry per rank.
    pub fn leveling_order(&self, plan: &Plan, start: &[u8], target: &[u8]) -> Vec<usize> {
        let empty = vec![0u8; self.bundle.skills.len()];
        let base = self.goals(plan, &empty);
        let mut order = vec![];
        let mut cur: Vec<u8> = start.iter().zip(target).map(|(&s, &t)| s.min(t)).collect();
        if !self.connected(&cur) {
            cur = empty.clone();
        }
        let allowed: HashSet<usize> = (0..target.len()).filter(|&i| target[i] > 0).collect();
        while cur != target {
            let (dist, prev) = self.paths(&cur, &plan.avoided);
            let mut best_step: Option<(f64, Vec<usize>)> = None;
            let now = self.score(plan, &cur, &base).score;
            let mut cands: Vec<(Vec<usize>, u32)> = vec![];
            let mut nodes: Vec<usize> = allowed.iter().copied().collect();
            nodes.sort();
            for v in nodes {
                if cur[v] >= target[v] {
                    continue;
                }
                let (steps, c) = if cur[v] > 0 { (vec![v], self.bundle.skills[v].cost) } else if dist[v] != u32::MAX { (self.bundle_to(&cur, v, &prev), dist[v]) } else { continue };
                if steps.iter().any(|x| !allowed.contains(x)) || c == 0 {
                    continue;
                }
                cands.push((steps, c));
            }
            let scores = par_map(&cands, |(steps, _)| {
                let mut r = cur.clone();
                for &x in steps {
                    r[x] += 1;
                }
                self.score(plan, &r, &base).score
            });
            for ((steps, c), f) in cands.into_iter().zip(scores) {
                let g = (f - now) / c as f64;
                if best_step.as_ref().map(|b| g > b.0).unwrap_or(true) {
                    best_step = Some((g, steps));
                }
            }
            let Some((_, steps)) = best_step else { break };
            for x in steps {
                cur[x] += 1;
                order.push(x);
            }
        }
        order
    }

    /// The game-data category name of a weapon item ("Bow", "One-Handed Axe").
    pub fn category_of(&self, item: &str) -> Option<&str> {
        let c = self.bundle.items.get(item)?.weapon_category.as_ref()?;
        self.bundle.weapon_categories.get(c).map(|c| c.name.as_str())
    }

    /// Weapon categories that have at least one weapon with damage, sorted.
    pub fn weapon_categories(&self) -> Vec<String> {
        let mut v: Vec<String> = self.bundle.items.iter()
            .filter(|(_, i)| i.category.as_deref() == Some("Weapons") && i.damage.is_some() && !i.name.is_empty())
            .filter_map(|(g, _)| self.category_of(g).map(str::to_string))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// An arrow, or a spell that is an attack: it deals damage and summons nothing (drains, heals and summon spells are
    /// never the attack the optimizer picks).
    fn is_attack(&self, i: &ens_data::Item) -> bool {
        i.slot.as_deref() != Some("Spell") || ens_calc::deals_damage(i) && !ens_calc::is_summon_spell(self.bundle, i)
    }

    /// Arrow (`kind` "Arrow") or spell ("Spell") families, sorted.
    pub fn ammo_families(&self, kind: &str) -> Vec<String> {
        let mut v: Vec<String> = self.bundle.items.values()
            .filter(|i| i.slot.as_deref() == Some(kind) && !i.name.is_empty() && self.is_attack(i))
            .map(|i| ammo_family(&i.name).to_string())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Arrows or spells of `kind` the level allows (lowest level of the item's range at most `level`), one per name.
    fn ammo_at(&self, kind: &str, level: u32) -> Vec<String> {
        let mut v: Vec<(String, String)> = self.bundle.items.iter()
            .filter(|(_, i)| i.slot.as_deref() == Some(kind) && !i.name.is_empty() && self.is_attack(i) && i.levels.as_ref().map(|l| l.min).unwrap_or(0) <= level)
            .map(|(g, i)| (i.name.clone(), g.clone()))
            .collect();
        v.sort();
        v.dedup_by(|a, b| a.0 == b.0);
        v.into_iter().map(|(_, g)| g).collect()
    }

    /// Summon spell families ("Ice Totem", "Hollow Wretch", "Shock Wisp"), sorted.
    pub fn summon_families(&self) -> Vec<String> {
        let mut v: Vec<String> = self.bundle.items.values()
            .filter(|i| !i.name.is_empty() && ens_calc::is_summon_spell(self.bundle, i))
            .map(|i| ammo_family(&i.name).to_string())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The highest tier of a summon family the level allows.
    pub fn summon_tier(&self, family: &str, level: u32) -> Option<String> {
        self.bundle.items.iter()
            .filter(|(_, i)| ammo_family(&i.name) == family && ens_calc::is_summon_spell(self.bundle, i))
            .filter_map(|(g, i)| {
                let min = i.levels.as_ref().map(|l| l.min).unwrap_or(0);
                (min <= level).then_some((min, g.clone()))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
            .map(|(_, g)| g)
    }

    /// Best summons: starting from none, add the summon family (at the level's tier) that raises the score most, while
    /// one does; the build keeps up each one it takes.
    pub fn optimize_summons(&self, plan: &Plan, ranks: &[u8], base: &HashMap<&'static str, f64>) -> Plan {
        let mut p = plan.clone();
        p.summons.clear();
        let mut cur = self.score(&p, ranks, base).score;
        let cands: Vec<String> = self.summon_families().iter().filter_map(|f| self.summon_tier(f, p.level)).collect();
        loop {
            let tries: Vec<Plan> = cands.iter().filter(|g| !p.summons.contains(g)).map(|g| {
                let mut t = p.clone();
                t.summons.push(g.clone());
                t
            }).collect();
            let scores = par_map(&tries, |t| self.score(t, ranks, base).score);
            let pick = scores.iter().enumerate().filter(|(_, sc)| **sc > cur + 1e-9).max_by(|a, b| a.1.total_cmp(b.1)).map(|(j, sc)| (j, *sc));
            match pick {
                Some((j, sc)) => (p, cur) = (tries[j].clone(), sc),
                None => break,
            }
        }
        p
    }

    /// The highest tier of an arrow or spell family the level allows.
    pub fn ammo_tier(&self, family: &str, level: u32) -> Option<String> {
        self.bundle.items.iter()
            .filter(|(_, i)| matches!(i.slot.as_deref(), Some("Arrow") | Some("Spell")) && ammo_family(&i.name) == family && self.is_attack(i))
            .filter_map(|(g, i)| {
                let min = i.levels.as_ref().map(|l| l.min).unwrap_or(0);
                (min <= level).then_some((min, g.clone()))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
            .map(|(_, g)| g)
    }

    /// Weapons of a category the level allows, one per name (the copy with the highest level range).
    pub fn weapon_candidates(&self, category: &str, level: u32) -> Vec<String> {
        let mut best: HashMap<&str, (&String, u32)> = HashMap::new();
        for (g, i) in &self.bundle.items {
            if i.name.is_empty() || i.category.as_deref() != Some("Weapons") || i.damage.is_none() || self.category_of(g) != Some(category) {
                continue;
            }
            let (min, max) = i.levels.as_ref().map(|l| (l.min, l.max)).unwrap_or((0, 0));
            if min > level {
                continue;
            }
            let e = best.entry(i.name.as_str()).or_insert((g, max));
            if max > e.1 || (max == e.1 && g < e.0) {
                *e = (g, max);
            }
        }
        let mut v: Vec<String> = best.into_values().map(|(g, _)| g.clone()).collect();
        v.sort();
        v
    }

    fn picked(&self, item: &str, rarity: Option<&String>) -> Equip {
        let rolled = self.bundle.items.get(item).and_then(|i| i.generate_rarity).unwrap_or(false);
        Equip { item: item.to_string(), rarity: if rolled { rarity.cloned() } else { None }, level: None, perks: None }
    }

    /// DPS of the plan's gear with one weapon and ammo and the given tree.
    fn weapon_dps(&self, plan: &Plan, ranks: &[u8], weapon: &Equip, ammo: Option<&String>) -> f64 {
        let mut b = self.build_of(plan, ranks);
        (b.weapon, b.ammo) = (Some(weapon.clone()), ammo.cloned());
        self.engine.total(&b, &plan.target).total
    }

    /// Resolve the plan's weapons for its level: a weapon left open (or picked by an earlier `prepare`) becomes the
    /// best of its category, an ammo family becomes its highest allowed tier, open ammo becomes the best allowed
    /// arrow or spell for the weapon. A plan without `weapons` gets one from `weapon` / `ammo` (the gear search may
    /// change that weapon within its category). `weapon` / `ammo` become the main weapon's (largest share).
    pub fn prepare(&self, plan: &Plan, ranks: &[u8]) -> Plan {
        let mut p = plan.clone();
        if let SummonChoice::Fixed(families) = &p.summon_choice {
            p.summons = families.iter().filter_map(|f| self.summon_tier(f, p.level)).collect();
        }
        if p.weapons.is_empty() {
            if let Some(w) = &p.weapon {
                p.weapons.push(WeaponPlan {
                    category: self.category_of(&w.item).map(str::to_string), weapon: Some(w.clone()), chosen: true,
                    ammo_family: None, ammo: p.ammo.clone(), ammo_chosen: false, share: 1.0,
                });
            }
            return p;
        }
        for k in 0..p.weapons.len() {
            let wp = p.weapons[k].clone();
            let category = wp.category.clone().or_else(|| wp.weapon.as_ref().and_then(|w| self.category_of(&w.item)).map(str::to_string));
            let ammo_for = |weapon: &Equip| -> Vec<String> {
                let kind = self.bundle.items.get(&weapon.item).and_then(|i| i.ammo.clone());
                match kind.as_deref() {
                    Some(kind @ ("Arrow" | "Spell")) => match (&wp.ammo_family, &wp.ammo) {
                        (Some(f), _) => self.ammo_tier(f, p.level).into_iter().collect(),
                        (None, Some(a)) if !wp.ammo_chosen => vec![a.clone()],
                        _ => self.ammo_at(kind, p.level),
                    },
                    _ => vec![],
                }
            };
            let weapons: Vec<Equip> = match (&wp.weapon, &category) {
                (Some(w), _) if !wp.chosen => vec![w.clone()],
                (_, Some(c)) => self.weapon_candidates(c, p.level).iter().map(|g| self.picked(g, p.rarity.as_ref())).collect(),
                (Some(w), None) => vec![w.clone()],
                (None, None) => vec![],
            };
            let pairs: Vec<(Equip, Option<String>)> = weapons.iter().flat_map(|w| {
                let ammo = ammo_for(w);
                if ammo.is_empty() { vec![(w.clone(), None)] } else { ammo.into_iter().map(|a| (w.clone(), Some(a))).collect() }
            }).collect();
            let scores = par_map(&pairs, |(w, a)| self.weapon_dps(&p, ranks, w, a.as_ref()));
            let mut best: Option<(usize, f64)> = None;
            for (j, d) in scores.into_iter().enumerate() {
                if best.is_none_or(|(_, b)| d > b + 1e-12) {
                    best = Some((j, d));
                }
            }
            if let Some((j, _)) = best {
                let (w, a) = pairs[j].clone();
                let chosen = wp.weapon.is_none() || wp.chosen;
                let ammo_chosen = wp.ammo_family.is_some() || wp.ammo.is_none() || wp.ammo_chosen;
                p.weapons[k] = WeaponPlan { category, chosen, ammo_chosen, weapon: Some(w), ammo: a, ..wp };
            }
        }
        let main = p.weapons.iter().enumerate().max_by(|a, b| a.1.share.total_cmp(&b.1.share).then(b.0.cmp(&a.0))).map(|(k, _)| k);
        if let Some(k) = main {
            (p.weapon, p.ammo) = (p.weapons[k].weapon.clone(), p.weapons[k].ammo.clone());
        }
        p
    }

    /// Items the optimizer may choose: one list per armour/ring slot (GEAR_SLOTS order), then one list per weapon of
    /// the plan (`weapons`, after `prepare`) that the optimizer picked, with weapons of its category; a weapon the
    /// player fixed gets an empty list. Items whose level range starts above the character level are left out;
    /// several copies of one item (one per drop source) collapse to the highest-level copy.
    pub fn gear_candidates(&self, plan: &Plan) -> Vec<Vec<String>> {
        let mut out = vec![];
        for slot in ens_calc::build::GEAR_SLOTS {
            let mut best: HashMap<&str, (&String, u32)> = HashMap::new();
            for (g, i) in &self.bundle.items {
                if i.name.is_empty() || i.slot.as_deref() != Some(slot) {
                    continue;
                }
                let (min, max) = i.levels.as_ref().map(|l| (l.min, l.max)).unwrap_or((0, 0));
                if min > plan.level {
                    continue;
                }
                let e = best.entry(i.name.as_str()).or_insert((g, max));
                if max > e.1 || (max == e.1 && g < e.0) {
                    *e = (g, max);
                }
            }
            let mut v: Vec<String> = best.into_values().map(|(g, _)| g.clone()).collect();
            v.sort();
            out.push(v);
        }
        for w in &plan.weapons {
            out.push(match (&w.category, w.chosen) {
                (Some(c), true) => self.weapon_candidates(c, plan.level),
                _ => vec![],
            });
        }
        out
    }

    /// Coordinate search over gear with the skill ranks fixed: for each slot try every candidate (on all cores) and
    /// keep the best if it beats the current item, until a full pass changes nothing (at most 3 passes). Items that
    /// roll their rarity get `rarity`; fixed-rarity items keep their own. The weapon keeps the plan's weapon level and
    /// rarity. The two ring slots never hold the same ring.
    pub fn optimize_gear(&self, plan: &Plan, ranks: &[u8], base: &HashMap<&'static str, f64>, rarity: &str) -> Plan {
        let cands = self.gear_candidates(plan);
        let mut p = plan.clone();
        let mut cur = self.score(&p, ranks, base).score;
        let equip = |g: &String| {
            let rolled = self.bundle.items.get(g).and_then(|i| i.generate_rarity).unwrap_or(false);
            Equip { item: g.clone(), rarity: if rolled { Some(rarity.to_string()) } else { None }, level: None, perks: None }
        };
        let n_gear = ens_calc::build::GEAR_SLOTS.len();
        for _ in 0..3 {
            let mut changed = false;
            for (k, list) in cands.iter().enumerate() {
                let is_weapon = k >= n_gear;
                let wk = k.saturating_sub(n_gear);
                if is_weapon && p.weapons.get(wk).and_then(|w| w.weapon.as_ref()).is_none() {
                    continue;
                }
                let other_ring = match k {
                    5 => p.gear[6].as_ref().map(|e| e.item.clone()),
                    6 => p.gear[5].as_ref().map(|e| e.item.clone()),
                    _ => None,
                };
                let held = if is_weapon { p.weapons[wk].weapon.as_ref().map(|e| e.item.clone()) } else { p.gear[k].as_ref().map(|e| e.item.clone()) };
                let tries: Vec<Plan> = list
                    .iter()
                    .filter(|g| Some(*g) != other_ring.as_ref() && Some(*g) != held.as_ref())
                    .map(|g| {
                        let mut t = p.clone();
                        if is_weapon {
                            let w = p.weapons[wk].weapon.as_ref().unwrap();
                            let nw = Equip { item: g.clone(), rarity: w.rarity.clone(), level: w.level, perks: w.perks };
                            if t.weapon.as_ref() == Some(w) {
                                t.weapon = Some(nw.clone());
                            }
                            t.weapons[wk].weapon = Some(nw);
                        } else {
                            t.gear[k] = Some(equip(g));
                        }
                        t
                    })
                    .collect();
                let scores = par_map(&tries, |t| self.score(t, ranks, base).score);
                // best of the slot, first in list order on ties
                let mut pick: Option<(usize, f64)> = None;
                for (j, sc) in scores.into_iter().enumerate() {
                    if sc > cur + 1e-9 && pick.is_none_or(|(_, b)| sc > b + 1e-12) {
                        pick = Some((j, sc));
                    }
                }
                if let Some((j, sc)) = pick {
                    p = tries[j].clone();
                    cur = sc;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        p
    }

    /// Skills and gear together: optimize the tree, then the gear for that tree, then the tree again for the new
    /// gear, for up to `rounds` rounds or until the gear stops changing. Scores are compared against one baseline
    /// (the empty tree with the plan's original gear). Returns the build, its leveling order and the chosen gear.
    pub fn optimize_with_gear(&self, plan: &Plan, iters: usize, seed: u64, rarity: &str, rounds: usize) -> (Scored, Vec<usize>, Plan) {
        let empty = vec![0u8; self.bundle.skills.len()];
        let mut p = self.prepare(plan, &empty);
        let base = self.goals(&p, &empty);
        let best_summons = p.summon_choice == SummonChoice::Best;
        if best_summons {
            p = self.optimize_summons(&p, &empty, &base);
        }
        let (mut best, mut order) = self.optimize(&p, iters, seed);
        for _ in 0..rounds {
            let mut np = self.optimize_gear(&p, &best.ranks, &base, rarity);
            if best_summons {
                np = self.optimize_summons(&np, &best.ranks, &base);
            }
            if np.gear == p.gear && np.weapons == p.weapons && np.summons == p.summons {
                break;
            }
            p = np;
            let (b2, o2) = self.optimize(&p, iters, seed);
            if self.score(&p, &b2.ranks, &base).score > self.score(&p, &best.ranks, &base).score {
                best = b2;
                order = o2;
            }
        }
        let goals = self.goals(&p, &best.ranks);
        best.goals = goals;
        (best, order, p)
    }

    /// `ranks` plus each required node (one rank) with the cheapest path to it that avoids the avoided nodes, in the
    /// plan's order, as far as the budget reaches.
    pub fn with_required(&self, plan: &Plan, ranks: &[u8], budget: u32) -> Vec<u8> {
        let mut r = ranks.to_vec();
        for &v in &plan.required {
            if v >= r.len() || r[v] > 0 || plan.avoided.contains(&v) {
                continue;
            }
            let (dist, prev) = self.paths(&r, &plan.avoided);
            if dist[v] == u32::MAX || self.cost(&r) + dist[v] > budget {
                continue;
            }
            for x in self.bundle_to(&r, v, &prev) {
                r[x] += 1;
            }
        }
        r
    }

    /// Trees and gear for a series of levels. Each milestone resolves the plan's weapons and ammo for its level and
    /// Flame level, grows the tree from the previous milestone's (kept unless `opts.respec`), and with `opts.gear`
    /// picks gear and weapons the level allows, then grows the tree once more for that gear. `progress(done, total)`
    /// is called after each milestone.
    pub fn milestones(&self, template: &Plan, list: &[Milestone], opts: &MilestoneOptions, progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<MilestoneResult> {
        let n = self.bundle.skills.len();
        let mut prev = vec![0u8; n];
        let mut prev_order: Vec<usize> = vec![];
        let mut prev_plan = template.clone();
        let mut out = vec![];
        for (k, m) in list.iter().enumerate() {
            let mut p = prev_plan.clone();
            (p.level, p.flame_level) = (m.level, m.flame_level);
            // found points are known only for the character as it is now
            p.found_points = if m.level == template.level { template.found_points } else { None };
            let budget = points_available(p.level, p.found_points);
            let fits = self.cost(&prev) <= budget;
            let (start, locked) = if opts.respec || !fits { (vec![0u8; n], vec![0u8; n]) } else { (prev.clone(), prev.clone()) };
            p = self.prepare(&p, &start);
            let base = self.goals(&p, &vec![0u8; n]);
            // best summons first, so the tree is built for them; checked again after the tree below
            if p.summon_choice == SummonChoice::Best {
                p = self.optimize_summons(&p, &start, &base);
            }
            let (mut best, mut order) = self.optimize_from(&p, &start, &locked, opts.iters, 7 + k as u64);
            if opts.gear || p.summon_choice == SummonChoice::Best {
                let mut np = if opts.gear { self.optimize_gear(&p, &best.ranks, &base, p.rarity.as_deref().unwrap_or("Legendary")) } else { p.clone() };
                if np.summon_choice == SummonChoice::Best {
                    np = self.optimize_summons(&np, &best.ranks, &base);
                }
                if np.gear != p.gear || np.weapons != p.weapons || np.summons != p.summons {
                    p = np;
                    let (b2, o2) = self.optimize_from(&p, &start, &locked, opts.iters, 7 + k as u64);
                    if self.score(&p, &b2.ranks, &base).score >= self.score(&p, &best.ranks, &base).score {
                        (best, order) = (b2, o2);
                    }
                }
            }
            best.goals = self.goals(&p, &best.ranks);
            let removed: Vec<usize> = (0..n).filter(|&i| prev[i] > best.ranks[i]).collect();
            let full: Vec<usize> = if start.iter().any(|&r| r > 0) { prev_order.iter().copied().chain(order.iter().copied()).collect() } else { order.clone() };
            out.push(MilestoneResult { level: m.level, flame_level: m.flame_level, budget, spent: self.cost(&best.ranks), ranks: best.ranks.clone(), added: order, order: full.clone(), removed, goals: best.goals.clone(), plan: p.clone() });
            prev_order = full;
            prev = best.ranks;
            prev_plan = p;
            progress(k + 1, list.len());
        }
        out
    }

    /// Ranks that can be removed without disconnecting the tree.
    fn removable(&self, ranks: &[u8]) -> Vec<usize> {
        let mut out = vec![];
        for i in 0..ranks.len() {
            if ranks[i] > 1 {
                out.push(i);
            } else if ranks[i] == 1 {
                let mut test = ranks.to_vec();
                test[i] = 0;
                if self.connected(&test) {
                    out.push(i);
                }
            }
        }
        out
    }

    pub fn connected(&self, ranks: &[u8]) -> bool {
        let mut seen = vec![false; ranks.len()];
        let mut st: Vec<usize> = self.roots.clone();
        for &r in &self.roots {
            seen[r] = true;
        }
        while let Some(u) = st.pop() {
            for &v in &self.adj[u] {
                if !seen[v] && ranks[v] > 0 {
                    seen[v] = true;
                    st.push(v);
                }
            }
        }
        (0..ranks.len()).all(|i| ranks[i] == 0 || seen[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn points_at_50() {
        assert_eq!(level_points(50), 212);
        assert_eq!(level_points(1), 0);
        // Roth (level 13) had found 32 points: 24 + 32
        assert_eq!(points_available(13, Some(32)), 56);
        assert_eq!(points_available(13, None), level_points(13));
    }

    fn plan_for(b: &ens_data::Bundle, level: u32) -> Plan {
        let s = Build::sample_bow(b);
        Plan {
            level, flame_level: 3, weapon: s.weapon.clone(), ammo: s.ammo.clone(), gear: vec![None; s.gear.len()], foods: vec![], summons: vec![],
            ranking: GOALS.to_vec(), attacks_per_s: 1.0, target: Target::default(), found_points: None, weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: SummonChoice::Keep,
        }
    }

    /// Explosive Arrow resolves to the highest tier the level allows (I at 15, II at 27, III at 44 in the game data).
    #[test]
    fn ammo_family_tiers_follow_the_level() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let name = |lvl| o.ammo_tier("Explosive Arrow", lvl).map(|g| b.items[&g].name.clone());
        assert_eq!(name(10), None);
        assert_eq!(name(20).as_deref(), Some("Explosive Arrow I"));
        assert_eq!(name(30).as_deref(), Some("Explosive Arrow II"));
        assert_eq!(name(50).as_deref(), Some("Explosive Arrow III"));
        assert_eq!(ammo_family("Eternal Frost Arrow III"), "Eternal Frost Arrow");
        assert!(o.weapon_categories().iter().any(|c| c == "Two-Handed Axe"));
        assert!(o.ammo_families("Spell").iter().any(|c| c == "Fireball"));
    }

    /// A summon family takes the highest tier the level allows, none means none, and best summons takes one that adds
    /// to the score.
    #[test]
    fn summon_kinds_resolve_by_level_and_best_takes_one_that_helps() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let fams = o.summon_families();
        assert!(fams.iter().any(|f| f == "Ice Totem") && fams.iter().any(|f| f == "Hollow Wretch"), "{fams:?}");
        let name = |g: &String| b.items[g].name.clone();
        // Ice Totem I from level 17, II from 38 (game data)
        assert_eq!(o.summon_tier("Ice Totem", 10), None, "none yet");
        assert_eq!(o.summon_tier("Ice Totem", 20).map(|g| name(&g)).as_deref(), Some("Ice Totem I"));
        assert_eq!(o.summon_tier("Ice Totem", 45).map(|g| name(&g)).as_deref(), Some("Ice Totem II"));
        let mut plan = plan_for(&b, 30);
        plan.summon_choice = SummonChoice::Fixed(vec!["Hollow Wretch".into()]);
        let p = o.prepare(&plan, &vec![0u8; b.skills.len()]);
        assert_eq!(p.summons.len(), 1);
        assert!(name(&p.summons[0]).contains("Hollow Wretch"));
        plan.summon_choice = SummonChoice::Fixed(vec![]);
        assert!(o.prepare(&plan, &vec![0u8; b.skills.len()]).summons.is_empty(), "no summons");
        // summons are cast from a staff: a staff build at level 30 gains from one
        plan.summon_choice = SummonChoice::Best;
        plan.weapons = vec![WeaponPlan { category: Some("Staff".into()), share: 1.0, ..Default::default() }];
        let empty = vec![0u8; b.skills.len()];
        let plan = o.prepare(&plan, &empty);
        let base = o.goals(&plan, &empty);
        let best = o.optimize_summons(&plan, &empty, &base);
        assert!(!best.summons.is_empty(), "a summon adds damage to a level 30 staff build");
        assert!(o.score(&best, &empty, &base).score > o.score(&Plan { summons: vec![], ..plan.clone() }, &empty, &base).score);
    }

    /// An open weapon becomes the best of its category for the level, with an arrow the level allows.
    #[test]
    fn prepare_picks_a_weapon_of_the_category() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let mut plan = plan_for(&b, 15);
        plan.weapons = vec![
            WeaponPlan { category: Some("Bow".into()), ammo_family: Some("Explosive Arrow".into()), share: 3.0, ..Default::default() },
            WeaponPlan { category: Some("Two-Handed Axe".into()), share: 1.0, ..Default::default() },
        ];
        let p = o.prepare(&plan, &vec![0; b.skills.len()]);
        let w0 = p.weapons[0].weapon.as_ref().unwrap();
        assert_eq!(o.category_of(&w0.item), Some("Bow"));
        assert!(b.items[&w0.item].levels.as_ref().map(|l| l.min).unwrap_or(0) <= 15);
        assert_eq!(p.weapons[0].ammo.as_ref().map(|g| b.items[g].name.as_str()), Some("Explosive Arrow I"));
        assert_eq!(o.category_of(&p.weapons[1].weapon.as_ref().unwrap().item), Some("Two-Handed Axe"));
        assert_eq!(p.weapons[1].ammo, None, "an axe takes no ammo");
        assert_eq!(p.weapon.as_ref(), Some(w0), "the main weapon is the one with the larger share");
        assert!(p.weapons[0].chosen && p.weapons[0].ammo_chosen);
        // at level 30 the same plan moves to the next tier
        let mut later = p.clone();
        later.level = 30;
        let p30 = o.prepare(&later, &vec![0; b.skills.len()]);
        assert_eq!(p30.weapons[0].ammo.as_ref().map(|g| b.items[g].name.as_str()), Some("Explosive Arrow II"));
    }

    /// Damage with two weapons is the share-weighted DPS of each.
    #[test]
    fn two_weapons_weigh_damage_by_share() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let mut plan = plan_for(&b, 20);
        plan.weapons = vec![
            WeaponPlan { category: Some("Bow".into()), share: 1.0, ..Default::default() },
            WeaponPlan { category: Some("One-Handed Axe".into()), share: 1.0, ..Default::default() },
        ];
        let empty = vec![0u8; b.skills.len()];
        let p = o.prepare(&plan, &empty);
        let kills = |shares: [f64; 2]| {
            let mut q = p.clone();
            q.weapons[0].share = shares[0];
            q.weapons[1].share = shares[1];
            o.goals(&q, &empty)["Damage"]
        };
        let (bow, axe, half) = (kills([1.0, 0.0]), kills([0.0, 1.0]), kills([1.0, 1.0]));
        assert!(bow != axe, "the two weapons differ");
        assert!(half > bow.min(axe) - 1e-9 && half < bow.max(axe) + 1e-9, "{bow} {axe} {half}");
    }

    /// A must-have node is in the tree (with its path); an avoided node never is.
    #[test]
    fn required_and_avoided_nodes() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let mut plan = plan_for(&b, 20);
        let free = o.optimize(&plan, 1, 3).0.ranks;
        // avoid the best tree's first non-attribute pick, require a mage node a bow build would not take
        let avoid = (0..free.len()).find(|&i| free[i] > 0 && b.skills[i].kind == "PassiveSkill").unwrap();
        let want = b.skills.iter().position(|s| s.name == "Spellslinger").unwrap();
        assert_eq!(free[want], 0, "the bow build does not take Spellslinger on its own");
        plan.avoided = vec![avoid];
        plan.required = vec![want];
        let (best, _) = o.optimize(&plan, 2, 3);
        assert!(best.ranks[want] > 0, "the required node is in the tree");
        assert_eq!(best.ranks[avoid], 0, "the avoided node is not");
        assert!(o.connected(&best.ranks));
        assert!(o.cost(&best.ranks) <= points_available(20, None));
    }

    /// Milestones grow one tree: each fits its budget and keeps the previous one; gear and ammo stay within the level.
    #[test]
    fn milestones_grow_within_budget_and_level() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let mut plan = plan_for(&b, 10);
        plan.weapons = vec![WeaponPlan { category: Some("Bow".into()), share: 1.0, ..Default::default() }];
        let list = [Milestone { level: 10, flame_level: 3 }, Milestone { level: 15, flame_level: 3 }, Milestone { level: 20, flame_level: 4 }];
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let opts = MilestoneOptions { gear: true, respec: false, iters: 1 };
        let res = o.milestones(&plan, &list, &opts, &|_, _| { calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed); });
        assert_eq!((res.len(), calls.load(std::sync::atomic::Ordering::Relaxed)), (3, 3));
        let min_level = |g: &str| b.items[g].levels.as_ref().map(|l| l.min).unwrap_or(0);
        for (k, m) in res.iter().enumerate() {
            assert!(m.spent <= m.budget, "level {}: {} of {}", m.level, m.spent, m.budget);
            assert!(o.connected(&m.ranks));
            assert!(!m.added.is_empty(), "level {} buys something", m.level);
            if k > 0 {
                assert!(m.ranks.iter().zip(&res[k - 1].ranks).all(|(a, b)| a >= b), "level {} keeps the earlier tree", m.level);
                assert!(m.removed.is_empty());
            }
            for e in m.plan.gear.iter().flatten().chain(m.plan.weapons.iter().filter_map(|w| w.weapon.as_ref())) {
                assert!(min_level(&e.item) <= m.level, "{} at level {}", b.items[&e.item].name, m.level);
            }
            if let Some(a) = &m.plan.weapons[0].ammo {
                assert!(min_level(a) <= m.level);
            }
            assert_eq!(o.category_of(&m.plan.weapons[0].weapon.as_ref().unwrap().item), Some("Bow"));
        }
        // respec: each milestone is free to drop earlier picks
        let opts = MilestoneOptions { gear: false, respec: true, iters: 1 };
        let res = o.milestones(&plan, &list[..2], &opts, &|_, _| {});
        assert!(res.iter().all(|m| m.spent <= m.budget));
    }

    /// The search on all cores picks exactly what the one-at-a-time search picks.
    #[test]
    fn parallel_search_matches_sequential() {
        let b = ens_data::Bundle::load(ens_data::default_bundle_path()).unwrap();
        let o = Optimizer::new(&b);
        let s = Build::sample_bow(&b);
        let plan = Plan {
            level: 12, flame_level: 3, weapon: s.weapon.clone(), ammo: s.ammo.clone(), gear: s.gear.clone(), foods: vec![], summons: vec![],
            ranking: GOALS.to_vec(), attacks_per_s: 1.0, target: Target::default(), found_points: None, weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: SummonChoice::Keep,
        };
        THREADS.store(1, std::sync::atomic::Ordering::Relaxed);
        let (one, order_one) = o.optimize(&plan, 2, 7);
        THREADS.store(0, std::sync::atomic::Ordering::Relaxed);
        let (all, order_all) = o.optimize(&plan, 2, 7);
        assert_eq!(one.ranks, all.ranks);
        assert_eq!(order_one, order_all);
        assert!(one.ranks.iter().any(|&r| r > 0));
    }
}

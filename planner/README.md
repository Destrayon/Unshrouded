# Unshrouded — build planner and DPS meter for Enshrouded (1.0)

A build planner and simulator for Enshrouded 1.0, written in Rust. The game build its data was read from is in `planner/data/bundle.json` (`meta.kfcVersion`) and on the app's footer. Every skill, perk, food, weapon, spell and enemy number comes from the game's own data files and compiled code. Nothing is copied from wikis.

## Run it
- **Desktop app:** `Unshrouded.exe` (built as `planner-desktop.exe`), installed by `Unshrouded-Setup-<version>.exe` (see `installer/`). The game data is built in. `enshrouded-planner.exe` is the older egui window, kept as a fallback.
- **Command line:** `ens.exe`
  - `ens data stats` — what the data bundle contains
  - `ens data skill "Fatal Precision"` — a skill's exact effect per rank
  - `ens calc sample` / `ens calc summoner` — worked damage breakdowns
  - `ens opt 50` — optimize a skill tree for level 50
  - `ens validate` — check every decoded effect against the game's tooltips

## Using the app
- **Top bar:** build name, loadouts (for example Leveling and Endgame, each with its own tree, gear and level; **+** copies the current one), level, Flame level, points spent, undo/redo and **Copy build code**.
- **Summary sidebar (always visible):** total DPS, expected hit, time to kill, hits survived, mana uptime, crit chance, character stats and the target (enemy, level, health factor, group, armor). While you hover a tree node or a weapon in the comparison, it shows what would change, in green (better) or red (worse).
- **Skill tree:** clicking a node allocates the shortest path to it, and hovering shows that path in teal first. Right-click removes a rank. Drag to pan, scroll to zoom. **Search** rings matching nodes (names, descriptions and attributes). **Heatmap** colors each node by the DPS it adds per skill point. The right panel shows the node's effect per rank and a short hit breakdown.
- **Equipment:** weapon (rarity and item level), ammo or spell, five armour slots, two rings, summons and food. The comparison scores every weapon of the same type at your weapon's level and rarity, colored from best (green) to worst (red). Hover a row to preview it, or click it to equip.
- **Calculations:** the full hit breakdown, summons, every source of any attribute, and the full character sheet.
- **Optimizer:** rank the goals, optimize the tree, then apply the result or step through a leveling order.
- **Notes:** paste or copy build codes and keep notes.
- **Shortcuts:** Ctrl+Z / Ctrl+Y undo and redo, Ctrl+F searches the tree, and Ctrl+= / Ctrl+− / Ctrl+0 change the text size.

## How a hit is calculated
Formulas and addresses are in `docs/research/engine-formulas.md`.
1. **Weapon base damage** = `trunc(10 × (1 + 0.15 × item level))`. Item damage multiplies that by the weapon's speedMod and dmgMod. Rarity doesn't affect damage.
2. **Multiplied in:**
   - the attack's AttackMod
   - `1 + 0.05 × scaling attribute` (Dexterity for bows and daggers, Intelligence for staffs and wands, Strength otherwise)
   - the weapon-category mod, such as Marksman's `WeaponCategoryDamageMod_Bow`
3. **Added in one pool:**
   - `DamageMod`
   - the attack-category mod (Ranged, Magic or Melee) and its per-element mod
   - "damage against" mods
   - on a crit, `CritDamageMod − 1`
4. **Crit chance** = template base 5% plus adders. Crit damage starts at ×1.5.
5. **Armor** is a flat reduction. 20% of every hit always gets through, and the rest is reduced by the shared physical or magical armor pool.
6. **Enemy weakness** (DamageSusceptibility) multiplies each damage type.

## What is modeled from data
- **Skills, perks and food** are decoded from the game's impact programs, including branch conditions such as `IsSkillUnlocked`.
- **Gear perks:** only the first 1–5 perks are active, depending on rarity.
- **Attack cadence:**
  - bow draw plus release, from item configs and the RangedCombat duration mods
  - staff spell charge plus cast
  - wand attack speed
  - melee combo chains from `SetActionOportunity` follow-ups (Swift Blades shortens them)
- **Extra projectiles:** Multi Shot and its Spread and Trigger variants come from the shoot sequence's skill- and tag-gated spawns, each with its own chance.
- **Focus specials:** Focus per hit comes from the weapon config, against a 100-point bar. The special's projectiles and AttackMod come from its sequence.
- **Explosions:** blast radius comes from the spawned template's collider.
- **Mana and stamina:**
  - regeneration = `Recharge × mod × 0.2` per second, after a 0.5 s pause and a ramp-up
  - on-hit procs (Blessed Arrows, Vitality Surge) come from their trigger programs
  - summons are funded first from a 60-second fight budget, then the main attack, then a fallback mana-free arrow
- **Enemies:**
  - health = the game's balancing formula (expected player hit × hits-to-kill × health factor)
  - enemy hit damage = the game's formula (expected player health ÷ hits-to-die)
- **Gear armor** = `(23 + 15 × item level) × armor share`.

## Assumptions (flagged in the app)
- **Hollow Wretch:** attacks every 2 s, and the summon spell supplies its damage. Totems scale with Intelligence.
- **Fights:** last 60 seconds, the main attack runs half of that, and 3 seconds pass between fights.
- **Utility skills:** their seconds-per-hour values are estimates.
- **Gear armor** reuses a balancing getter whose call site couldn't be located.
- **Arrow damage** takes its level from the bow (the spawn sets `addWeaponReference`). Arrow tiers change damage type and special effects only.

## Rebuilding the data
After a game patch, run the steps listed in `kfc/pipeline.json` in order: they rewrite every intermediate the bundle
export reads, then `planner/data/bundle.json`, the item-stats table, the coverage report and the proof file, and end with
the test suite. The bundle records the game build it was read from (`meta.kfcVersion`). Scripts find the repository
themselves; set `ENS_GAME` when the game is not in `D:/SteamLibrary/steamapps/common/Enshrouded`, and `ENS_BUNDLE_OUT` to
export a bundle somewhere else (to compare, say). `pipeline.json` also lists the few inputs that have no producer script
and where their values came from. The desktop app compiles the bundle in, so rebuild it after a data change.

# Unshrouded

**Build planner and DPS meter for Enshrouded (1.0).**

Unshrouded shows the numbers Enshrouded keeps hidden: what a skill, perk, food or piece of gear really adds to your
damage, how long your mana lasts, and how many hits you survive. Every number comes from the game's own data, not from
wikis.

## Features

- **Build planner** — the full skill tree, gear, ammo and spells, gems, food and summons, with your total DPS,
  expected hit, time to kill and hits survived updating as you change anything. Hover a node or item to preview it.
- **Calculations** — the whole damage breakdown, step by step, and a character sheet showing every source of every stat.
- **Optimizer** — ranks your goals and finds a skill tree (and optionally gear and summons) for your level, or a path
  of milestones while leveling. Step through any tree in the order to take it.
- **Load from game** — reads your character's level, skills and owned gear from your save.
- **Live DPS meter** — a Details-style meter drawn inside the game window (so it shows up on stream), with every
  player's damage and a breakdown per attack and ammo on hover. Also available as a separate window.
- **Build codes** — share a build, with its fight setup, as a short code.

## Install

Download `Unshrouded-Setup-<version>.exe` from [Releases](../../releases) and run it. It installs for your Windows user
only (no admin rights needed) and adds a Start menu entry. Windows may warn that the installer is unrecognised because
it isn't code-signed; choose **More info → Run anyway**.

Requires Windows 10 or 11 (64-bit) with the Microsoft Edge WebView2 runtime, which Windows 11 already has.

## The live meter

Turn it on under **Extensions → Live DPS meter**. While Unshrouded is open it waits for Enshrouded, starts the combat
logger once the game reaches its menu, and shows the meter only while you are in a world.

- **F6** next tab · **F7** see-through · **F8** move · **F9** hide
- With the cursor visible (inventory open), drag the title to move it and the corner to resize it.

How it works: the combat logger is a small DLL loaded into the game that **reads** each hit's damage record and writes
it to a CSV in the install folder's `Combat Logger` folder; it never changes the game's memory or sends anything over
the network. The in-game picture is drawn by a per-user Vulkan layer that only activates in `enshrouded.exe`.
Uninstalling removes both. Enshrouded has no anti-cheat, but this is unofficial software that hooks the game process:
use it at your own risk.

## Build from source

```
cd planner
cargo build --release --workspace
cargo test --release --workspace
```

`planner/target/release/planner-desktop.exe` is the app. To build the installer, install
[Inno Setup 6](https://jrsoftware.org/isinfo.php) and run `ISCC.exe installer\unshrouded.iss` from the repository root.

| Crate | What it is |
| --- | --- |
| `ens-data` | the game data bundle (`planner/data/bundle.json`) and save-file reader |
| `ens-calc` | the damage, survival and sustain math, and the combat-log reader |
| `ens-opt` | the skill-tree and gear optimizer |
| `ens-desktop` | the app (Tauri + WebView2) |
| `ens-hook` | the combat logger DLL |
| `ens-overlay` | the in-game meter (Vulkan layer) |
| `ens-inject` | loads the combat logger into the game |
| `ens-cli` | `ens` command-line tools |
| `ens-app` | an older egui window, kept as a fallback |

## Not affiliated

Unshrouded is a fan-made tool. It is not made, endorsed or supported by Keen Games. Enshrouded and its game data,
names and icons belong to Keen Games GmbH.

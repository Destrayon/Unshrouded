//! The planner as the meter's loader (docs/plans/planner-loader.md): the Extensions switch, what to do about the game
//! right now, and the status the Extensions panel shows.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use ens_overlay::frame::Place;

/// Where the in-game meter can go, in the order F8 moves it: on the left below the game's health bars and status list
/// (2560 × 1440: 31 px in, 288 px down), then the other corners clear of the quest tracker and the hotbar.
pub const PLACES: [Place; 4] = [
    Place { corner: 0, x: 0.012, y: 0.2 },
    Place { corner: 1, x: 0.012, y: 0.16 },
    Place { corner: 3, x: 0.012, y: 0.12 },
    Place { corner: 2, x: 0.012, y: 0.12 },
];

/// Persisted in the app's data folder as extensions.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Live DPS meter: combat logger plus in-game meter while the planner is open.
    #[serde(default)]
    pub meter: bool,
    /// Which of PLACES the in-game meter is at.
    #[serde(default)]
    pub place: usize,
    /// Hidden with F9 (the logger keeps recording).
    #[serde(default)]
    pub hidden: bool,
    /// Where the meter was dragged in the game (top-left as fractions of the game's image); replaces `place`.
    #[serde(default)]
    pub moved: Option<(f32, f32)>,
    /// The meter's size in CSS px after resizing it in the game (None: the meter window's 340 × 440).
    #[serde(default)]
    pub size: Option<(f64, f64)>,
}

impl Settings {
    /// Where the in-game meter goes now.
    pub fn place(&self) -> Place {
        match self.moved {
            Some((x, y)) => Place { corner: 0, x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) },
            None => PLACES[self.place % PLACES.len()],
        }
    }
}

/// A message from the layer (input.rs in ens-overlay).
#[derive(Debug, PartialEq)]
pub enum Input {
    Tab,
    Fade,
    Move,
    Hide,
    Click(i32, i32),
    Wheel(i32, i32, i32),
    /// Dragged to this top-left corner (fractions of the game's image).
    MoveTo(f32, f32),
    /// Resized to this many image pixels.
    Size(i32, i32),
    /// The pointer over the meter at this point (-1, -1: it left).
    Hover(i32, i32),
}

pub fn parse_input(msg: &str) -> Option<Input> {
    let mut w = msg.split_whitespace();
    let mut n = || w.next().and_then(|v| v.parse::<i32>().ok());
    Some(match msg.split_whitespace().next()? {
        "tab" => Input::Tab,
        "fade" => Input::Fade,
        "move" => Input::Move,
        "hide" => Input::Hide,
        "click" => {
            n();
            Input::Click(n()?, n()?)
        }
        "wheel" => {
            n();
            Input::Wheel(n()?, n()?, n()?)
        }
        "moveto" => {
            let mut f = msg.split_whitespace().skip(1).map(|v| v.parse::<f32>().ok().filter(|v| v.is_finite()));
            Input::MoveTo(f.next()??, f.next()??)
        }
        "hover" => {
            n();
            Input::Hover(n()?, n()?)
        }
        "size" => {
            n();
            Input::Size(n()?.max(1), n()?.max(1))
        }
        _ => return None,
    })
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default()).map_err(|e| e.to_string())
    }
}

/// What the game looks like this tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct Seen {
    pub running: bool,
    /// The game has reached its main menu or further (any SessionPlayer state logged), or has run 30 s.
    pub ready: bool,
    pub in_world: bool,
    pub logger_loaded: bool,
    /// The logger was already tried in this game process (a failed load is not retried every tick).
    pub logger_tried: bool,
}

/// What the loader does this tick.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Act {
    pub inject_logger: bool,
    pub show_meter: bool,
}

pub fn decide(on: bool, s: Seen) -> Act {
    Act {
        inject_logger: on && s.running && s.ready && !s.logger_loaded && !s.logger_tried,
        show_meter: on && s.running && s.in_world,
    }
}

/// For the Extensions panel.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub meter: bool,
    pub game: bool,
    /// The game's SessionPlayer state (Local = main menu, Local_InSession = in a world).
    pub state: Option<String>,
    pub in_world: bool,
    /// In someone else's world (their server works out the damage: nothing to log).
    pub joined: bool,
    pub logger: bool,
    pub logger_error: Option<String>,
    /// The newest combat log's file name.
    pub log: Option<String>,
    pub layer_installed: bool,
    /// The in-game meter's layer is loaded in the running game.
    pub overlay_loaded: bool,
    pub showing: bool,
    /// Seconds since the last picture of the meter page (None: none taken); the in-game meter falls back to a drawn
    /// picture after 3 s.
    pub picture_age: Option<f64>,
    pub error: Option<String>,
    pub dir: Option<String>,
}

impl Status {
    /// The game runs without the layer: it started before the switch was turned on (or the loader skipped it).
    pub fn restart_needed(&self) -> bool {
        self.meter && self.game && !self.overlay_loaded
    }
}

/// Turn the in-game layer on: put ens_overlay.dll (from beside the planner) into the logger's folder and register
/// it. The layer is picked up when the game next starts.
pub fn install_layer(dir: &Path) -> Result<PathBuf, String> {
    use ens_overlay::install::{install, DLL};
    let built = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join(DLL))).filter(|p| p.is_file());
    if let Some(src) = built {
        // a running game holds the old copy; keeping it is fine
        if std::fs::copy(&src, dir.join(DLL)).is_err() && !dir.join(DLL).is_file() {
            return Err(format!("cannot copy {} into {}", src.display(), dir.display()));
        }
    }
    install(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_once_and_shows_only_in_a_world() {
        let menu = Seen { running: true, ready: true, ..Default::default() };
        assert_eq!(decide(false, menu), Act::default(), "switch off: nothing");
        assert_eq!(decide(true, Seen::default()), Act::default(), "no game: nothing");
        assert_eq!(decide(true, Seen { ready: false, ..menu }), Act::default(), "still loading: wait");
        assert_eq!(decide(true, menu), Act { inject_logger: true, show_meter: false }, "main menu: start the logger, no meter");
        assert!(!decide(true, Seen { logger_loaded: true, ..menu }).inject_logger, "already recording");
        assert!(!decide(true, Seen { logger_tried: true, ..menu }).inject_logger, "a failed load is not retried");
        let world = Seen { in_world: true, logger_loaded: true, ..menu };
        assert_eq!(decide(true, world), Act { inject_logger: false, show_meter: true });
        assert_eq!(decide(false, world), Act::default());
        let dir = std::env::temp_dir().join(format!("ens-ext-{}", std::process::id()));
        let path = dir.join("extensions.json");
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings { meter: true, place: 2, hidden: false, moved: Some((0.3, 0.4)), size: Some((400.0, 500.0)) };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert_eq!(s.place(), Place { corner: 0, x: 0.3, y: 0.4 }, "dragged wins over the preset");
        assert_eq!(Settings { moved: None, ..s.clone() }.place(), PLACES[2]);
        assert_eq!(parse_input("moveto 0.25 0.5"), Some(Input::MoveTo(0.25, 0.5)));
        assert_eq!(parse_input("moveto NaN 0.5"), None);
        assert_eq!(parse_input("size 400 520"), Some(Input::Size(400, 520)));
        assert_eq!(parse_input("hover -1 -1"), Some(Input::Hover(-1, -1)));
        std::fs::write(&path, r#"{"meter": true}"#).unwrap();
        assert_eq!(Settings::load(&path).place, 0, "settings from before the place was kept");
        assert_eq!(parse_input("click 12 40"), Some(Input::Click(12, 40)));
        assert_eq!(parse_input("wheel 3 4 -40"), Some(Input::Wheel(3, 4, -40)));
        assert_eq!(parse_input("tab"), Some(Input::Tab));
        assert_eq!(parse_input("click 12"), None);
        assert_eq!(parse_input("rm -rf"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

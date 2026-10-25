//! Watching the running game for the planner's loader (docs/plans/planner-loader.md): whether enshrouded.exe runs, whether
//! the player is in a world, and which of our DLLs it has loaded.
//!
//! The world state comes from enshrouded.log next to the game's exe, which logs every SessionPlayer state change:
//! `[SessionPlayer] finished transition from 'Local' to 'Local_InSession' (current='Local_InSession')!` on entering a
//! world and `... (current='Local')!` on leaving it. Other players' slots log the same way (`Remote_InSession`), so only
//! transitions from or to a `Local` state count. `[Session]` lines say what kind of game it is: `Host_Offline` in your
//! own world, `Client_Online` in someone else's, where their server works out the damage and only the combat logger's
//! client hook (the hits the server sends) sees it.

use std::io::{Read, Seek, SeekFrom};


/// The local player's state a log line ends in, if it is a finished SessionPlayer transition of the local player.
pub fn session_state(line: &str) -> Option<&str> {
    let rest = line.split("[SessionPlayer] finished transition").nth(1)?;
    if !rest.contains("'Local") {
        return None;
    }
    current(rest)
}

/// The kind of game a log line ends in (Host_Offline, Client_Online, Lobby, …), if it is a finished Session transition.
pub fn game_kind(line: &str) -> Option<&str> {
    current(line.split("[Session] finished transition").nth(1)?)
}

fn current(rest: &str) -> Option<&str> {
    rest.split("(current='").nth(1)?.split('\'').next()
}

/// In a world: a SessionPlayer state of the `*_InSession` kind (local and, by the same naming, joined sessions).
pub fn in_world(state: &str) -> bool {
    state.contains("InSession")
}

#[derive(Debug, Default)]
pub struct Watch {
    pub pid: Option<u32>,
    offset: u64,
    partial: String,
    /// The latest SessionPlayer state in this game's log.
    pub state: Option<String>,
    /// The latest Session state: Host_Offline, Client_Online, …
    pub kind: Option<String>,
    pub since: Option<std::time::Instant>,
}

impl Watch {
    /// Look for the game and read what its log gained since the last call. A new game process starts over.
    pub fn poll(&mut self) {
        let Some(g) = ens_inject::find_game() else {
            *self = Watch::default();
            return;
        };
        if self.pid != Some(g.pid) {
            *self = Watch { pid: Some(g.pid), since: Some(std::time::Instant::now()), ..Default::default() };
        }
        let Some(log) = g.exe.parent().map(|d| d.join("enshrouded.log")) else { return };
        let Ok(mut f) = std::fs::File::open(&log) else { return };
        // a game that has just started may not have replaced the last session's log yet: wait for its own
        let modified = f.metadata().and_then(|m| m.modified()).ok();
        if let (Some(m), Some(start)) = (modified, g.started) {
            if m + std::time::Duration::from_secs(2) < start {
                return;
            }
        }
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            (self.offset, self.partial) = (0, String::new());
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.take(len - self.offset).read_to_end(&mut buf).is_err() {
            return;
        }
        self.offset += buf.len() as u64;
        self.feed(&String::from_utf8_lossy(&buf));
    }

    /// Take log text (possibly ending mid-line) and keep the latest SessionPlayer state.
    pub fn feed(&mut self, text: &str) {
        self.partial.push_str(text);
        let Some(end) = self.partial.rfind('\n') else { return };
        let done: String = self.partial.drain(..=end).collect();
        for line in done.lines() {
            if let Some(s) = session_state(line) {
                self.state = Some(s.to_string());
            }
            if let Some(k) = game_kind(line) {
                self.kind = Some(k.to_string());
            }
        }
    }

    pub fn running(&self) -> bool {
        self.pid.is_some()
    }

    pub fn in_world(&self) -> bool {
        self.running() && self.state.as_deref().is_some_and(in_world)
    }

    /// In someone else's world: the combat logger cannot see damage there.
    pub fn joined(&self) -> bool {
        self.running() && self.kind.as_deref().is_some_and(|k| k.starts_with("Client"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_entering_and_leaving_a_world_from_the_log() {
        let enter = "[I 00:00:39,247] [SessionPlayer] finished transition from 'Local' to 'Local_InSession' (current='Local_InSession')!";
        let leave = "[I 00:05:02,100] [SessionPlayer] finished transition from 'Local_InSession' to 'Local' (current='Local')!";
        let started = "[I 00:00:39,247] [SessionPlayer] started transition from 'Local' to 'Local_InSession' (current='<invalid>')!";
        assert_eq!(session_state(enter), Some("Local_InSession"));
        assert_eq!(session_state(started), None, "only finished transitions count");
        assert_eq!(session_state("[I 00:00:39,247] [Session] finished transition from 'Lobby' to 'Host_Offline' (current='Host_Offline')!"), None);
        let mut w = Watch { pid: Some(1), ..Default::default() };
        w.feed("[I 00:00:02,704] [SessionPlayer] finished transition from 'Free' to 'Local' (current='Local')!\n");
        assert!(!w.in_world(), "main menu");
        // a line split across two reads
        let (a, b) = enter.split_at(40);
        w.feed(a);
        assert!(!w.in_world());
        w.feed(&format!("{b}\n{started}\n"));
        assert!(w.in_world());
        w.feed(&format!("{leave}\n"));
        assert!(!w.in_world(), "back on the main menu");
        // another player joining or leaving does not change ours
        w.feed(&format!("{enter}\n[I 00:01:00,000] [SessionPlayer] finished transition from 'Remote_InSession' to 'Free' (current='Free')!\n"));
        assert!(w.in_world(), "a friend left; we are still in the world");
        assert!(!w.joined());
        w.feed("[I 00:00:55,075] [Session] finished transition from 'Lobby' to 'Client_Online' (current='Client_Online')!\n");
        assert!(w.joined(), "in someone else's world");
        // real logs, if this machine has them (the current one may be on the main menu: the game's backups count too)
        let dir = std::path::Path::new("D:/SteamLibrary/steamapps/common/Enshrouded");
        let logs: Vec<String> = std::iter::once(dir.join("enshrouded.log"))
            .chain(std::fs::read_dir(dir.join("backup")).into_iter().flatten().flatten().map(|e| e.path()))
            .filter_map(|p| std::fs::read_to_string(p).ok()).collect();
        if !logs.is_empty() {
            let states: Vec<_> = logs.iter().flat_map(|l| l.lines().filter_map(session_state)).collect();
            assert!(states.iter().any(|s| in_world(s)), "{states:?}");
        }
    }
}

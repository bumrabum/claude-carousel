//! What the user decided about cards ("done", "later") plus settings.
//! Saved as `state.json` in the app's data folder; safe to edit by hand.

use crate::scanner::Card;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How many earlier cards to keep for browsing.
const HISTORY_MAX: usize = 20;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Show the carousel again every N minutes while it has cards.
    pub remind_every_min: u64,
    /// Hide the carousel after N seconds without the mouse over it.
    pub auto_hide_sec: u64,
    /// "Later" hides a card for this many minutes.
    pub snooze_min: i64,
    /// Global shortcut that shows / hides the carousel. "CommandOrControl" is Cmd on Mac, Ctrl on Windows.
    pub hotkey: String,
    /// Cards for sessions active within this many days.
    pub recent_days: i64,
    /// Make "where we stopped → next step" lines with the `claude` command.
    pub summaries: bool,
    /// Model for those lines (a small, fast one is enough).
    pub summary_model: String,
    /// Full path to `claude`; empty = find it automatically.
    pub claude_path: String,
    /// The open carousel flips to the next card every N seconds; 0 = off.
    pub auto_flip_sec: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            remind_every_min: 20,
            auto_hide_sec: 15,
            snooze_min: 60,
            hotkey: "CommandOrControl+Shift+C".into(),
            recent_days: 3,
            summaries: true,
            summary_model: "haiku".into(),
            claude_path: String::new(),
            auto_flip_sec: 4,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub settings: Settings,
    /// Card id -> Claude's activity time when you pressed "Done".
    /// The card comes back only if Claude does something after that.
    pub done: HashMap<String, i64>,
    /// Card id -> hidden until this time (unix ms).
    pub snoozed: HashMap<String, i64>,
    /// Cards you opened or marked done, newest first, to flip back through.
    pub history: Vec<Card>,
}

impl State {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn is_visible(&self, card: &Card, now: i64) -> bool {
        let done = self.done.get(&card.id).is_some_and(|&at| at >= card.activity_at);
        let snoozed = self.snoozed.get(&card.id).is_some_and(|&until| until > now);
        !done && !snoozed
    }

    pub fn mark_done(&mut self, card: &Card) {
        self.done.insert(card.id.clone(), card.activity_at);
        self.snoozed.remove(&card.id);
        self.history.retain(|c| c.id != card.id);
        self.history.insert(0, card.clone());
        self.history.truncate(HISTORY_MAX);
    }

    pub fn snooze(&mut self, card: &Card, now: i64) {
        self.snoozed.insert(card.id.clone(), now + self.settings.snooze_min * 60_000);
    }

    /// Add sessions marked done before history existed (or done on another run) to the history.
    pub fn backfill_history(&mut self, all_cards: &[Card]) {
        for card in all_cards {
            let done = self.done.get(&card.id).is_some_and(|&at| at >= card.activity_at);
            if done && self.history.len() < HISTORY_MAX && !self.history.iter().any(|h| h.id == card.id) {
                self.history.push(card.clone());
            }
        }
    }

    /// Forget sessions that are gone and snoozes that ran out, so the file stays small.
    pub fn prune(&mut self, all_cards: &[Card], now: i64) {
        let alive: HashSet<&str> = all_cards.iter().map(|c| c.id.as_str()).collect();
        self.done.retain(|id, _| alive.contains(id.as_str()));
        self.snoozed.retain(|id, &mut until| until > now && alive.contains(id.as_str()));
    }
}

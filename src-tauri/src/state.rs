//! What the user decided about cards ("done", "later") plus settings.
//! Saved as `state.json` in the app's data folder; safe to edit by hand.

use crate::scanner::Card;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Show the carousel again every N minutes while something is waiting.
    pub remind_every_min: u64,
    /// Hide the carousel after N seconds without the mouse over it.
    pub auto_hide_sec: u64,
    /// "Later" hides a card for this many minutes.
    pub snooze_min: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { remind_every_min: 20, auto_hide_sec: 15, snooze_min: 60 }
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
    }

    pub fn snooze(&mut self, card: &Card, now: i64) {
        self.snoozed.insert(card.id.clone(), now + self.settings.snooze_min * 60_000);
    }

    /// Forget sessions that are gone and snoozes that ran out, so the file stays small.
    pub fn prune(&mut self, all_cards: &[Card], now: i64) {
        let alive: HashSet<&str> = all_cards.iter().map(|c| c.id.as_str()).collect();
        self.done.retain(|id, _| alive.contains(id.as_str()));
        self.snoozed.retain(|id, &mut until| until > now && alive.contains(id.as_str()));
    }
}

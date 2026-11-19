// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Claude Carousel: a tray app that reminds you where each recent Claude session
//! stopped and what to do next, as a small carousel in the top-right corner.
//!
//! Flow: a background thread scans sessions every minute (`scanner`),
//! filters out what you marked done / later (`state`), queues sessions for a
//! short "stopped → next step" line (`summarizer`, a second thread), updates the
//! tray count (sessions waiting for you) and pops the carousel window when a new
//! session starts waiting or the reminder interval passes. The window
//! (src/main.ts) asks for cards and calls the commands below.

mod actions;
mod scanner;
mod state;
mod summarizer;

use scanner::{Card, Paths};
use state::State;
use summarizer::{CallSettings, NextStep, Summarizer};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const SCAN_EVERY: Duration = Duration::from_secs(60);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const WINDOW: &str = "carousel";
const TRAY: &str = "main";

struct Shared {
    paths: Paths,
    state_file: PathBuf,
    summarizer: Summarizer,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    state: State,
    /// Cards currently shown (after done/later filtering): waiting first, then newest.
    cards: Vec<Card>,
    /// Ids of sessions that were waiting in the previous scan, to notice new ones.
    known_waiting: HashSet<String>,
    last_popup: Option<Instant>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

// ---------- commands called from the window ----------

/// A card plus its "stopped → next step" line, as the window gets it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CardView {
    #[serde(flatten)]
    card: Card,
    next_step: Option<NextStep>,
    /// The next step was made before the latest activity in the session.
    outdated: bool,
    /// A (newer) next-step line is being made right now or soon.
    summarizing: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    /// Recent sessions: waiting for you first, then newest.
    cards: Vec<CardView>,
    /// Earlier cards (opened / done), newest first, without the ones shown above.
    history: Vec<CardView>,
    auto_hide_sec: u64,
    auto_flip_sec: u64,
    /// Why next steps can't be made right now (e.g. `claude` is signed out).
    summary_error: Option<String>,
}

#[tauri::command]
fn get_cards(shared: tauri::State<'_, Shared>) -> View {
    let g = shared.inner.lock().unwrap();
    let view = |card: &Card| {
        let (next_step, outdated, summarizing) = shared.summarizer.get(card);
        CardView { card: card.clone(), next_step, outdated, summarizing }
    };
    let history = g
        .state
        .history
        .iter()
        .filter(|h| !g.cards.iter().any(|c| c.id == h.id))
        .map(view)
        .collect();
    View {
        cards: g.cards.iter().map(view).collect(),
        history,
        auto_hide_sec: g.state.settings.auto_hide_sec,
        auto_flip_sec: g.state.settings.auto_flip_sec,
        summary_error: shared.summarizer.last_error(),
    }
}

#[tauri::command]
fn open_card(app: AppHandle, id: String) -> Result<(), String> {
    let card = find_card(&app, &id).ok_or("card not found")?;
    actions::open(&card)?;
    update_card(&app, &id, |state, card| state.opened(card, now_ms()));
    hide_window(app);
    Ok(())
}

#[tauri::command]
fn snooze(app: AppHandle, id: String) {
    update_card(&app, &id, |state, card| state.snooze(card, now_ms()));
}

/// "Don't remind": hide the card until something new happens in the session.
#[tauri::command]
fn mark_done(app: AppHandle, id: String) {
    update_card(&app, &id, |state, card| state.mark_done(card));
}

/// Try making next steps again after a failure (e.g. after signing in to `claude`).
#[tauri::command]
fn retry_summaries(app: AppHandle) {
    app.state::<Shared>().summarizer.retry();
    let _ = app.emit("cards-changed", ());
    std::thread::spawn(move || tick(&app));
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.hide();
    }
}

fn find_card(app: &AppHandle, id: &str) -> Option<Card> {
    let shared = app.state::<Shared>();
    let g = shared.inner.lock().unwrap();
    g.cards.iter().chain(&g.state.history).find(|c| c.id == id).cloned()
}

/// Change the saved state for one card, drop it from the list, refresh UI.
fn update_card(app: &AppHandle, id: &str, change: impl Fn(&mut State, &Card)) {
    let shared = app.state::<Shared>();
    let count = {
        let mut g = shared.inner.lock().unwrap();
        if let Some(card) = g.cards.iter().find(|c| c.id == id).cloned() {
            change(&mut g.state, &card);
            g.state.save(&shared.state_file);
        }
        g.cards.retain(|c| c.id != id);
        waiting_count(&g.cards)
    };
    refresh_ui(app, count);
}

fn mark_all_done(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let mut g = shared.inner.lock().unwrap();
    for card in std::mem::take(&mut g.cards) {
        g.state.mark_done(&card);
    }
    g.state.save(&shared.state_file);
    drop(g);
    refresh_ui(app, 0);
}

// ---------- scanning and showing ----------

/// Second thread: makes "next step" lines one by one, refreshing the window after each.
fn summary_loop(app: AppHandle) {
    loop {
        let shared = app.state::<Shared>();
        let settings = {
            let g = shared.inner.lock().unwrap();
            CallSettings {
                model: g.state.settings.summary_model.clone(),
                claude_path: g.state.settings.claude_path.clone(),
            }
        };
        if shared.summarizer.work_one(&settings, now_ms()) {
            let _ = app.emit("cards-changed", ());
        } else {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

fn scan_loop(app: AppHandle) {
    loop {
        tick(&app);
        std::thread::sleep(SCAN_EVERY);
    }
}

fn waiting_count(cards: &[Card]) -> usize {
    cards.iter().filter(|c| c.waiting).count()
}

fn tick(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let now = now_ms();
    let recent_ms = {
        let g = shared.inner.lock().unwrap();
        g.state.settings.recent_days.max(1) * DAY_MS
    };
    let all = scanner::scan(&shared.paths, now, recent_ms);
    let (count, popup) = {
        let mut g = shared.inner.lock().unwrap();
        g.state.prune(&all, now);
        g.state.backfill_history(&all);
        shared.summarizer.prune(&all);
        let visible: Vec<Card> = all.into_iter().filter(|c| g.state.is_visible(c, now)).collect();
        let waiting: HashSet<String> =
            visible.iter().filter(|c| c.waiting).map(|c| c.id.clone()).collect();
        // Pop up right away when a session starts waiting; otherwise every few minutes.
        let new_waiting = waiting.iter().any(|id| !g.known_waiting.contains(id));
        let every = Duration::from_secs(g.state.settings.remind_every_min * 60);
        let due = g.last_popup.map_or(true, |t| t.elapsed() >= every);
        let popup = !visible.is_empty() && (new_waiting || due);
        if popup {
            g.last_popup = Some(Instant::now());
        }
        if g.state.settings.summaries {
            shared.summarizer.enqueue(&visible, now);
        }
        g.known_waiting = waiting;
        g.cards = visible;
        g.state.save(&shared.state_file);
        (waiting_count(&g.cards), popup)
    };
    refresh_ui(app, count);
    if popup {
        show_window(app);
    }
}

fn refresh_ui(app: &AppHandle, count: usize) {
    if let Some(tray) = app.tray_by_id(TRAY) {
        let label = if count > 0 { count.to_string() } else { String::new() };
        let _ = tray.set_title(Some(label)); // number next to the icon (macOS menu bar)
        let _ = tray.set_tooltip(Some(format!("Claude Carousel — waiting: {count}")));
    }
    let _ = app.emit("cards-changed", ());
}

/// Put the window in the top-right corner of the main screen and show it.
fn show_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window(WINDOW) else { return };
    if let (Ok(Some(screen)), Ok(size)) = (w.primary_monitor(), w.outer_size()) {
        let scale = screen.scale_factor();
        let margin = (12.0 * scale) as i32;
        let top = (36.0 * scale) as i32; // below the macOS menu bar
        let x = screen.position().x + screen.size().width as i32 - size.width as i32 - margin;
        let y = screen.position().y + top;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = app.emit("shown", ());
}

/// Hotkey: hide if open, otherwise show and take keyboard focus (so ← → Enter Esc work).
fn toggle_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window(WINDOW) else { return };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
    } else {
        show_window(app);
        let _ = w.set_focus();
    }
}

fn register_hotkey(app: &tauri::App, hotkey: &str) {
    if let Err(e) = app.global_shortcut().register(hotkey) {
        eprintln!("Claude Carousel: could not register hotkey {hotkey:?}: {e}");
    }
}

// ---------- app setup ----------

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "show", "Show", true, None::<&str>)?,
            &MenuItem::with_id(app, "rescan", "Check now", true, None::<&str>)?,
            &MenuItem::with_id(app, "all_done", "Mark all as done", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
        ],
    )?;
    TrayIconBuilder::with_id(TRAY)
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Claude Carousel")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_window(app),
            "rescan" => {
                let app = app.clone();
                std::thread::spawn(move || tick(&app));
            }
            "all_done" => mark_all_done(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            // Menu-bar-only app: no Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app.path().app_data_dir()?;
            let _ = std::fs::create_dir_all(&data_dir);
            let state_file = data_dir.join("state.json");
            let paths = Paths::detect().expect("home folder not found");
            let state = State::load(&state_file);
            register_hotkey(app, &state.settings.hotkey);
            app.manage(Shared {
                inner: Mutex::new(Inner { state, ..Default::default() }),
                summarizer: Summarizer::new(&data_dir),
                paths,
                state_file,
            });
            setup_tray(app)?;

            let handle = app.handle().clone();
            std::thread::spawn(move || scan_loop(handle));
            let handle = app.handle().clone();
            std::thread::spawn(move || summary_loop(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_cards,
            open_card,
            snooze,
            mark_done,
            hide_window,
            retry_summaries
        ])
        .run(tauri::generate_context!())
        .expect("error while running Claude Carousel");
}

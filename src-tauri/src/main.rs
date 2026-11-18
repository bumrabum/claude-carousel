// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Claude Carousel: a tray app that reminds you about Claude sessions
//! waiting for your reply, as a small carousel in the top-right corner.
//!
//! Flow: a background thread scans sessions every minute (`scanner`),
//! filters out what you marked done / later (`state`), updates the tray
//! count and pops the carousel window when something new appears or the
//! reminder interval passes. The window (src/main.ts) asks for cards
//! and calls the commands below.

mod actions;
mod scanner;
mod state;

use scanner::{Card, Paths};
use state::State;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

const SCAN_EVERY: Duration = Duration::from_secs(60);
const WINDOW: &str = "carousel";
const TRAY: &str = "main";

struct Shared {
    paths: Paths,
    state_file: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    state: State,
    /// Cards currently shown (after done/later filtering), newest first.
    cards: Vec<Card>,
    /// Card ids seen in the previous scan, to notice new ones.
    known: HashSet<String>,
    last_popup: Option<Instant>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

// ---------- commands called from the window ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    cards: Vec<Card>,
    auto_hide_sec: u64,
}

#[tauri::command]
fn get_cards(shared: tauri::State<'_, Shared>) -> View {
    let g = shared.inner.lock().unwrap();
    View { cards: g.cards.clone(), auto_hide_sec: g.state.settings.auto_hide_sec }
}

#[tauri::command]
fn open_card(app: AppHandle, id: String) -> Result<(), String> {
    let card = find_card(&app, &id).ok_or("card not found")?;
    actions::open(&card)?;
    update_card(&app, &id, |state, card| state.mark_done(card));
    hide_window(app);
    Ok(())
}

#[tauri::command]
fn mark_done(app: AppHandle, id: String) {
    update_card(&app, &id, |state, card| state.mark_done(card));
}

#[tauri::command]
fn snooze(app: AppHandle, id: String) {
    update_card(&app, &id, |state, card| state.snooze(card, now_ms()));
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
    g.cards.iter().find(|c| c.id == id).cloned()
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
        g.cards.len()
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

fn scan_loop(app: AppHandle) {
    loop {
        tick(&app);
        std::thread::sleep(SCAN_EVERY);
    }
}

fn tick(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let now = now_ms();
    let all = scanner::scan(&shared.paths, now);
    let (count, popup) = {
        let mut g = shared.inner.lock().unwrap();
        g.state.prune(&all, now);
        let visible: Vec<Card> = all.into_iter().filter(|c| g.state.is_visible(c, now)).collect();
        let has_new = visible.iter().any(|c| !g.known.contains(&c.id));
        let every = Duration::from_secs(g.state.settings.remind_every_min * 60);
        let due = g.last_popup.map_or(true, |t| t.elapsed() >= every);
        let popup = !visible.is_empty() && (has_new || due);
        if popup {
            g.last_popup = Some(Instant::now());
        }
        g.known = visible.iter().map(|c| c.id.clone()).collect();
        g.cards = visible;
        g.state.save(&shared.state_file);
        (g.cards.len(), popup)
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
        let _ = tray.set_tooltip(Some(format!("Claude Carousel — ждут ответа: {count}")));
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

// ---------- app setup ----------

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "show", "Показать", true, None::<&str>)?,
            &MenuItem::with_id(app, "rescan", "Проверить сейчас", true, None::<&str>)?,
            &MenuItem::with_id(app, "all_done", "Отметить все как просмотренные", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?,
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
        .setup(|app| {
            // Menu-bar-only app: no Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state_file = app.path().app_data_dir()?.join("state.json");
            let paths = Paths::detect().expect("home folder not found");
            app.manage(Shared {
                inner: Mutex::new(Inner { state: State::load(&state_file), ..Default::default() }),
                paths,
                state_file,
            });
            setup_tray(app)?;

            let handle = app.handle().clone();
            std::thread::spawn(move || scan_loop(handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_cards, open_card, mark_done, snooze, hide_window])
        .run(tauri::generate_context!())
        .expect("error while running Claude Carousel");
}

# Claude Carousel

A small app for Mac and Windows that sits in the menu bar / tray. It reminds you about Claude sessions
where Claude answered and you haven't come back. Every so often it shows a small
carousel of cards in the top-right corner. Flip through them and open the one you need.

## What a card shows

- Where it's from: **Code** (Code tab in the Claude app), **Cowork**, or **CLI** (`claude` in a terminal or in VS Code / another IDE)
- Project folder and how long ago Claude answered
- Session title and a short summary of Claude's last answer
- Buttons:
  - **Open** jumps into the session. Code goes to the session in the Claude app; CLI opens a new terminal with `claude --resume <id>`.
  - **Later** hides the card for 1 hour.
  - **Done** hides it until Claude answers there again.

When nothing is waiting, the carousel shows "All caught up". The arrows still flip to **earlier** cards: the last 20 sessions you opened or marked Done (grey, with only **Open**). When cards are waiting, the earlier ones come after them.

Controls: arrows ‹ › or the mouse wheel / trackpad swipe, ← → keys, Enter to open, Esc to hide.
The carousel hides itself after 15 seconds unless the mouse is over it.
Click the tray icon or press **Cmd+Shift+C** (Ctrl+Shift+C on Windows) to show it now; the same keys hide it. The tray menu has "Check now", "Mark all as done" and "Quit".
On a Mac, the number next to the menu-bar icon is how many sessions are waiting.

## How it decides a session is waiting

Everything is read from local files and nothing is sent anywhere.
All of the folders below are under `~/Library/Application Support/Claude` on Mac and `%APPDATA%\Claude` on Windows, except `~/.claude/projects`.

| Source | Where | "Waiting" means |
|---|---|---|
| Code | `claude-code-sessions/**/local_*.json` | Claude was active after you last opened the session (`lastActivityAt > lastFocusedAt`) |
| Cowork | `local-agent-mode-sessions/**/local_*.json` + transcript | The last message in the transcript is a finished Claude answer |
| CLI | `~/.claude/projects/<folder>/<id>.jsonl` | Same as Cowork. Scripts and bots built on the Agent SDK are skipped |

Every source also has to meet two rules:

- The last activity is more than 5 minutes ago, so a session where Claude is still working is skipped.
- It is less than 7 days ago.

Cowork and CLI have no "last opened" time. So the first time you run the app, it may show sessions you've already read.
Press "Done" on them (or "Mark all as done" in the tray menu), and each will only come back after a new answer from Claude.

Chats on claude.ai are stored only in the cloud, so they aren't covered.

## Settings

The file `state.json` is in the app's data folder:
`~/Library/Application Support/dev.local.claude-carousel/` on Mac, `%APPDATA%\dev.local.claude-carousel\` on Windows.

```json
"settings": { "remindEveryMin": 20, "autoHideSec": 15, "snoozeMin": 60, "hotkey": "CommandOrControl+Shift+C" }
```

`hotkey` uses names like `CommandOrControl`, `Alt`, `Shift` and a key (`C`, `F5`, `Space`). Pick another one if the default clashes with an app you use: in Chrome, Cmd+Shift+C opens the element inspector, and in Windows Terminal, Ctrl+Shift+C is copy.
The same file stores which cards you marked "Done" or "Later", and the earlier cards (`history`). Restart the app after editing it.

## Run and build

You need Node 18+ and Rust (`rustup`).

```bash
npm install
npm run tauri dev      # run in dev mode
npm run tauri build -- --bundles app   # Mac: src-tauri/target/release/bundle/macos/Claude Carousel.app
cd src-tauri && cargo test                                 # scanner tests
cd src-tauri && cargo test real -- --ignored --nocapture   # what the scanner sees on this machine
```

To install it, drag `Claude Carousel.app` into `/Applications`. To start it at login, add it in System Settings → General → Login Items.
On Windows, `npm run tauri build` creates an installer in `bundle/nsis/`. The Windows version has to be built on Windows (or in CI). You can't build it from a Mac.
A `.dmg` (without `--bundles app`) needs permission to control Finder.

## Code layout

```
src/                 the carousel window (HTML/TS/CSS)
src-tauri/src/
  main.rs            tray, timer, window, commands for the UI
  scanner.rs         finds waiting sessions (Code, Cowork, CLI) + tests
  state.rs           "Done" / "Later" + settings (state.json)
  actions.rs         "Open" a session (claude:// link or terminal)
src-tauri/icons/     icons; source.svg is the original, the rest are generated with `npx tauri icon`
```

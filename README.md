# Claude Carousel

A small app for Mac and Windows that sits in the menu bar / tray. For each recent Claude session it
reminds you **where you stopped** and **what to do next**, in a few words. Every so often it shows a
small carousel of cards in the top-right corner that flips by itself, so you can catch up in seconds.

## What a card shows

- Big: **→ the next step for you**, e.g. "→ Check the Bidvertiser report in 2 days".
- Small: **Stopped:** where the work stopped, e.g. "Stopped: waiting for the client to allow the IP".
- Grey: the session's title.
- On top: where it's from (**Code** = Code tab in the Claude app, **Cowork**, **CLI** = `claude` in a terminal or an IDE),
  **● Waiting** if Claude answered and you haven't come back, the project folder and how long ago.
- Buttons:
  - **Open** jumps into the session and hides the card for 1 hour, like Later. Code goes to the session in the Claude app;
    CLI opens a new terminal with `claude --resume <id>`.
  - **Later** hides the card for 1 hour.
  - **Don't remind** hides the card until something new happens in the session (you or Claude write there).
  - **Retry** appears only when next steps could not be made (see below).

Sessions waiting for you come first. After the recent sessions come **earlier** cards: the last 20 you opened or
hid with Don't remind (grey, only **Open**). With no recent sessions the carousel shows "All caught up", and the arrows
still flip to the earlier cards.

## Controls

- The open carousel flips to the next card every 10 seconds and hides after one round (at most 2 minutes).
  Hovering the mouse pauses it; flipping by hand stops auto-flip.
- Arrows ‹ ›, mouse wheel / trackpad swipe, ← → keys; Enter opens, Esc hides.
- **Cmd+Shift+C** (Ctrl+Shift+C on Windows) or a click on the tray icon shows / hides it.
- It also pops up by itself when a session starts waiting for you, and every 20 minutes while there are cards.
- Tray menu: "Check now", "Mark all as done" (Don't remind for all current cards), "Quit". On a Mac, the number next to the icon is how many sessions are waiting.

## How the next step is made

The app runs your own `claude` command in the background, one session at a time, with a small fast model (Haiku):

```
claude -p --model haiku --output-format json --no-session-persistence --tools "" --strict-mcp-config --setting-sources project --system-prompt <prompt>
```

- Input is only the **end** of the conversation: the last ~12 messages, text only (no tool output), at most ~8000 characters.
- The model gets no tools, so it only reads that text. `--no-session-persistence` means it leaves no new session on disk.
- The text comes back in the language the session is in.
- Results are cached in `summaries.json` and made again when something new happens in the session: within about a
  minute after Claude finishes a reply. While Claude is still working, the card keeps the old step and says it will update.
- This uses your Claude subscription (or API key), a little per session.
- If it fails, the card says why and shows **Retry**. The most common reason: the terminal `claude` is signed out. Run
  `claude auth login` in the Terminal app, then press Retry. Failures are also written to `summaries.log`.

## Which sessions it reads

Everything is read from local files. All of the folders below are under `~/Library/Application Support/Claude` on Mac
and `%APPDATA%\Claude` on Windows, except `~/.claude/projects`.

| Source | Where | "Waiting" means |
|---|---|---|
| Code | `claude-code-sessions/**/local_*.json` | Claude was active after you last opened the session (`lastActivityAt > lastFocusedAt`) |
| Cowork | `local-agent-mode-sessions/**/local_*.json` + transcript | The last message in the transcript is a finished Claude answer |
| CLI | `~/.claude/projects/<folder>/<id>.jsonl` | Same as Cowork. Scripts and bots built on the Agent SDK are skipped |

A card is made for every session that is not archived and was active in the last 3 days (`recentDays`).
"Waiting" also needs the last activity to be more than 5 minutes ago.

Cowork and CLI have no "last opened" time, so they count as waiting until you press "Don't remind".
Chats on claude.ai are stored only in the cloud, so they aren't covered.

## Settings

The app's data folder: `~/Library/Application Support/dev.local.claude-carousel/` on Mac,
`%APPDATA%\dev.local.claude-carousel\` on Windows. Settings are in `state.json`:

```json
"settings": {
  "remindEveryMin": 20,
  "autoHideSec": 15,
  "snoozeMin": 60,
  "hotkey": "CommandOrControl+Shift+C",
  "recentDays": 3,
  "summaries": true,
  "summaryModel": "haiku",
  "claudePath": "",
  "autoFlipSec": 10
}
```

- `hotkey` uses names like `CommandOrControl`, `Alt`, `Shift` and a key (`C`, `F5`, `Space`). Pick another one if the
  default clashes with an app you use: in Chrome, Cmd+Shift+C opens the element inspector, and in Windows Terminal, Ctrl+Shift+C is copy.
- `summaries: false` turns next steps off; cards then show the end of Claude's last answer.
- `claudePath` is the full path to `claude` if the app can't find it (empty = look in the usual places and your login shell).
- `autoFlipSec: 0` turns auto-flip off.

`state.json` also stores which cards you hid with Don't remind (`done`) or with Open / Later (`snoozed`),
and the earlier cards (`history`).
Restart the app after editing it. The same folder has `summaries.json` (next steps) and `summaries.log` (failures).

## Run and build

You need Node 18+ and Rust (`rustup`).

```bash
npm install
npm run tauri dev                      # run in dev mode
npm run tauri build -- --bundles app   # Mac: src-tauri/target/release/bundle/macos/Claude Carousel.app
cd src-tauri && cargo test                                           # tests
cd src-tauri && cargo test real_machine -- --ignored --nocapture     # what the scanner sees on this machine
cd src-tauri && cargo test real_summaries -- --ignored --nocapture   # next steps for 5 recent sessions (calls claude)
```

To install it, drag `Claude Carousel.app` into `/Applications`. To start it at login, add it in System Settings → General → Login Items.
On Windows, `npm run tauri build` creates an installer in `bundle/nsis/`. The Windows version has to be built on Windows (or in CI).
A `.dmg` (without `--bundles app`) needs permission to control Finder.

## Code layout

```
src/                 the carousel window (HTML/TS/CSS)
src-tauri/src/
  main.rs            tray, timers, window, hotkey, commands for the UI
  scanner.rs         finds recent sessions (Code, Cowork, CLI), "waiting" flag, conversation excerpt + tests
  summarizer.rs      "stopped → next step" via `claude -p`, cache, background queue + tests
  state.rs           "Open" / "Later" / "Don't remind" decisions, earlier cards, settings (state.json) + tests
  actions.rs         "Open" a session (claude:// link or terminal)
src-tauri/icons/     icons; source.svg is the original, the rest are generated with `npx tauri icon`
```

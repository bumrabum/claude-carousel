//! Finds Claude sessions where Claude has answered and is waiting for you.
//!
//! Three sources, all read straight from disk (read-only):
//! - Code:   Claude desktop app, Code tab. One JSON file per session with
//!           `lastActivityAt` / `lastFocusedAt`, so "unread" is exact.
//! - Cowork: Claude desktop app, Cowork tab. JSON per session + its transcript.
//! - CLI:    `claude` in a terminal or an IDE extension. Transcripts in `~/.claude/projects`.
//! For Cowork and CLI there is no "last opened" time, so a session counts as
//! waiting when the last message in its transcript is a finished Claude reply.

use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Ignore sessions that changed less than this long ago (Claude may still be working).
pub const QUIET_MS: i64 = 5 * 60 * 1000;
/// Ignore sessions older than this.
pub const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// How much of a transcript's end we read to find the last message.
const TAIL_BYTES: u64 = 512 * 1024;
const SUMMARY_CHARS: usize = 220;
const TITLE_CHARS: usize = 70;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Code,
    Cowork,
    Cli,
}

/// One "come back here" card.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    /// Unique across sources: "<source>:<session id>".
    pub id: String,
    pub source: Source,
    pub session_id: String,
    pub title: String,
    /// Folder name the session works in.
    pub project: String,
    pub cwd: String,
    pub summary: String,
    /// Unix ms of Claude's last activity.
    pub activity_at: i64,
}

/// Where the data lives on this machine.
pub struct Paths {
    /// `~/Library/Application Support/Claude` (Mac) or `%APPDATA%\Claude` (Windows).
    pub claude_app: PathBuf,
    /// `~/.claude/projects`
    pub cli_projects: PathBuf,
}

impl Paths {
    pub fn detect() -> Option<Self> {
        Some(Self {
            claude_app: dirs::config_dir()?.join("Claude"),
            cli_projects: dirs::home_dir()?.join(".claude").join("projects"),
        })
    }
}

/// All waiting sessions, newest first. Does not apply "done"/"snoozed" — see `state.rs`.
pub fn scan(paths: &Paths, now: i64) -> Vec<Card> {
    let transcripts = index_transcripts(&paths.cli_projects);
    let mut claimed = HashSet::new(); // transcript ids owned by the desktop app
    let mut cards = scan_code(paths, &transcripts, &mut claimed, now);
    cards.extend(scan_cowork(paths, now));
    cards.extend(scan_cli(&transcripts, &claimed, now));
    cards.sort_by_key(|c| -c.activity_at);
    cards
}

// ---------- sources ----------

fn scan_code(
    paths: &Paths,
    transcripts: &HashMap<String, PathBuf>,
    claimed: &mut HashSet<String>,
    now: i64,
) -> Vec<Card> {
    let mut cards = Vec::new();
    for file in session_files(&paths.claude_app.join("claude-code-sessions")) {
        let Some(s) = read_json(&file) else { continue };
        for key in ["cliSessionId", "lastAssistantCliSessionId"] {
            if let Some(id) = s[key].as_str() {
                claimed.insert(id.to_string());
            }
        }
        if s["isArchived"].as_bool() == Some(true) {
            continue;
        }
        let activity = s["lastActivityAt"].as_i64().unwrap_or(0);
        let focused = s["lastFocusedAt"].as_i64().unwrap_or(0);
        if activity <= focused || !in_window(activity, now) {
            continue;
        }
        let transcript_id = s["lastAssistantCliSessionId"]
            .as_str()
            .or(s["cliSessionId"].as_str());
        let summary = code_summary(&s)
            .or_else(|| {
                let path = transcripts.get(transcript_id?)?;
                read_tail(path).last_text
            })
            .unwrap_or_default();
        let session_id = str_of(&s, "sessionId");
        let cwd = str_of(&s, "cwd");
        cards.push(Card {
            id: format!("code:{session_id}"),
            source: Source::Code,
            title: clean(&non_empty(str_of(&s, "title"), folder_name(&cwd)), TITLE_CHARS),
            project: folder_name(&cwd),
            summary: clean(&summary, SUMMARY_CHARS),
            session_id,
            cwd,
            activity_at: activity,
        });
    }
    cards
}

/// Short status the desktop app already wrote for the last turn, if it is current.
fn code_summary(s: &Value) -> Option<String> {
    let pts = &s["postTurnSummary"];
    let fresh = match (pts["summarizes_uuid"].as_str(), s["lastAssistantUuid"].as_str()) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if fresh {
        for key in ["needs_action", "status_detail"] {
            if let Some(t) = pts[key].as_str().filter(|t| !t.trim().is_empty()) {
                return Some(t.to_string());
            }
        }
    }
    s["turnWrapUp"]["description"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
}

fn scan_cowork(paths: &Paths, now: i64) -> Vec<Card> {
    let mut cards = Vec::new();
    for file in session_files(&paths.claude_app.join("local-agent-mode-sessions")) {
        let Some(s) = read_json(&file) else { continue };
        if s["isArchived"].as_bool() == Some(true) {
            continue;
        }
        // The transcript lives in a folder named like the JSON file, without ".json".
        let Some(transcript) = newest_jsonl(&file.with_extension("")) else { continue };
        let activity = s["lastActivityAt"]
            .as_i64()
            .unwrap_or(0)
            .max(mtime_ms(&transcript));
        if !in_window(activity, now) {
            continue;
        }
        let tail = read_tail(&transcript);
        if !tail.claude_finished {
            continue;
        }
        let session_id = str_of(&s, "sessionId");
        cards.push(Card {
            id: format!("cowork:{session_id}"),
            source: Source::Cowork,
            title: clean(&non_empty(str_of(&s, "title"), "Cowork task".into()), TITLE_CHARS),
            project: "Cowork".into(),
            summary: clean(&tail.last_text.unwrap_or_default(), SUMMARY_CHARS),
            session_id,
            cwd: String::new(),
            activity_at: activity,
        });
    }
    cards
}

fn scan_cli(
    transcripts: &HashMap<String, PathBuf>,
    claimed: &HashSet<String>,
    now: i64,
) -> Vec<Card> {
    let mut cards = Vec::new();
    for (id, path) in transcripts {
        if claimed.contains(id) {
            continue;
        }
        let activity = mtime_ms(path);
        if !in_window(activity, now) {
            continue;
        }
        let tail = read_tail(path);
        // Terminal and IDE sessions (cli, claude-vscode, ...); skip scripts and bots built on the Agent SDK.
        let by_person = tail.entrypoint.as_deref().map_or(true, |e| !e.starts_with("sdk"));
        if !tail.claude_finished || !by_person {
            continue;
        }
        let cwd = tail.cwd.unwrap_or_default();
        let title = tail
            .title
            .or_else(|| first_prompt(path))
            .unwrap_or_else(|| folder_name(&cwd));
        cards.push(Card {
            id: format!("cli:{id}"),
            source: Source::Cli,
            session_id: id.clone(),
            title: clean(&title, TITLE_CHARS),
            project: folder_name(&cwd),
            summary: clean(&tail.last_text.unwrap_or_default(), SUMMARY_CHARS),
            cwd,
            activity_at: activity,
        });
    }
    cards
}

// ---------- transcripts (.jsonl) ----------

/// What the end of a transcript tells us.
#[derive(Default, Debug)]
struct Tail {
    /// The last message is a finished Claude reply (text, no pending tool call).
    claude_finished: bool,
    last_text: Option<String>,
    title: Option<String>,
    cwd: Option<String>,
    entrypoint: Option<String>,
}

/// Map transcript id -> path for every `~/.claude/projects/<folder>/<id>.jsonl`.
fn index_transcripts(projects: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for dir in read_dir_paths(projects) {
        for file in read_dir_paths(&dir) {
            if file.extension().is_some_and(|e| e == "jsonl") {
                if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
                    map.insert(stem.to_string(), file);
                }
            }
        }
    }
    map
}

fn read_tail(path: &Path) -> Tail {
    let mut tail = Tail::default();
    let Ok(text) = read_end(path, TAIL_BYTES) else { return tail };
    for line in text.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
        match e["type"].as_str() {
            Some("custom-title") => {
                if let Some(t) = e["customTitle"].as_str() {
                    tail.title = Some(t.to_string());
                }
            }
            Some(kind @ ("user" | "assistant")) => {
                if e["isSidechain"].as_bool() == Some(true) || e["isMeta"].as_bool() == Some(true) {
                    continue;
                }
                if let Some(c) = e["cwd"].as_str() {
                    tail.cwd = Some(c.to_string());
                }
                if let Some(ep) = e["entrypoint"].as_str() {
                    tail.entrypoint = Some(ep.to_string());
                }
                let (text, has_tool) = message_parts(&e["message"]["content"]);
                if kind == "assistant" {
                    if has_tool {
                        tail.claude_finished = false;
                    } else if let Some(t) = text {
                        tail.claude_finished = true;
                        tail.last_text = Some(t);
                    }
                } else {
                    // A prompt or a tool result: the ball is in Claude's court.
                    tail.claude_finished = false;
                }
            }
            _ => {}
        }
    }
    tail
}

/// Text of a message and whether it contains a tool call / tool result.
fn message_parts(content: &Value) -> (Option<String>, bool) {
    if let Some(s) = content.as_str() {
        return (Some(s.to_string()), false);
    }
    let mut text = String::new();
    let mut has_tool = false;
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or("")),
            Some("tool_use" | "tool_result") => has_tool = true,
            _ => {}
        }
    }
    ((!text.trim().is_empty()).then_some(text), has_tool)
}

/// First thing the user typed, used as a title when the session has none.
fn first_prompt(path: &Path) -> Option<String> {
    let mut head = String::new();
    File::open(path).ok()?.take(64 * 1024).read_to_string(&mut head).ok();
    head.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["type"] == "user" && e["isMeta"].as_bool() != Some(true))
        .filter_map(|e| message_parts(&e["message"]["content"]).0)
        // Skip injected context like <ide_opened_file> or <command-name>.
        .map(|t| strip_tags(&t))
        .find(|t| !t.trim().is_empty())
}

/// Remove `<tag>...</tag>` blocks and stray tags, keep the rest.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = after[..end].split_whitespace().next().unwrap_or("");
        let close = format!("</{name}>");
        rest = match after.find(&close).filter(|_| !name.starts_with('/')) {
            Some(c) => &after[c + close.len()..],
            None => &after[end + 1..],
        };
    }
    out.push_str(rest);
    out
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(i64, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for p in read_dir_paths(&d) {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name().is_some_and(|n| n != "audit.jsonl")
            {
                let m = mtime_ms(&p);
                if best.as_ref().map_or(true, |(bm, _)| m > *bm) {
                    best = Some((m, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

// ---------- small helpers ----------

/// `<base>/<account>/<org>/local_*.json` — layout used by both desktop session stores.
fn session_files(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for account in read_dir_paths(base) {
        for org in read_dir_paths(&account) {
            for f in read_dir_paths(&org) {
                let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with("local_") && name.ends_with(".json") {
                    out.push(f);
                }
            }
        }
    }
    out
}

fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Last `max` bytes of a file, starting at a whole line.
fn read_end(path: &Path, max: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        text = text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
    }
    Ok(text)
}

fn mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn in_window(activity: i64, now: i64) -> bool {
    let age = now - activity;
    (QUIET_MS..=MAX_AGE_MS).contains(&age)
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn non_empty(s: String, fallback: String) -> String {
    if s.trim().is_empty() { fallback } else { s }
}

fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// One line of plain text: no markdown marks, collapsed spaces, cut to `max` chars.
fn clean(s: &str, max: usize) -> String {
    let plain = drop_link_targets(s).replace("**", "").replace('`', "").replace('#', "");
    let one_line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        one_line.chars().take(max - 1).collect::<String>() + "…"
    }
}

/// Markdown `[text](target)` -> `text`.
fn drop_link_targets(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("](") {
        match rest[i..].find(')') {
            Some(j) => {
                out.push_str(&rest[..i]);
                rest = &rest[i + j + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out.replace('[', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("claude-carousel-test-{name}"));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn paths(&self) -> Paths {
            Paths {
                claude_app: self.root.join("Claude"),
                cli_projects: self.root.join("projects"),
            }
        }
        fn write(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
            p
        }
        fn code_session(&self, id: &str, extra: Value) {
            let mut v = json!({"sessionId": id, "title": format!("T {id}"), "cwd": "/x/proj"});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            self.write(&format!("Claude/claude-code-sessions/a/o/{id}.json"), &v.to_string());
        }
    }

    fn assistant_text(t: &str) -> String {
        json!({"type":"assistant","cwd":"/x/cli-proj","message":{"content":[{"type":"text","text":t}]}}).to_string()
    }
    fn assistant_tool() -> String {
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"1"}]}}).to_string()
    }
    fn user_prompt(t: &str) -> String {
        json!({"type":"user","entrypoint":"cli","cwd":"/x/cli-proj","message":{"content":t}}).to_string()
    }

    #[test]
    fn code_sessions_use_focus_time() {
        let fx = Fixture::new("code");
        let t = NOW - HOUR;
        fx.code_session("local_unread", json!({"lastActivityAt": t, "lastFocusedAt": t - 1,
            "postTurnSummary": {"needs_action": "run **wrangler login**"}}));
        fx.code_session("local_read", json!({"lastActivityAt": t, "lastFocusedAt": t + 1}));
        fx.code_session("local_archived", json!({"lastActivityAt": t, "isArchived": true}));
        fx.code_session("local_busy", json!({"lastActivityAt": NOW - 1000}));
        fx.code_session("local_old", json!({"lastActivityAt": NOW - 30 * 24 * HOUR}));

        let cards = scan(&fx.paths(), NOW);
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].id, "code:local_unread");
        assert_eq!(cards[0].summary, "run wrangler login");
        assert_eq!(cards[0].project, "proj");
    }

    #[test]
    fn cli_sessions_need_a_finished_reply() {
        let fx = Fixture::new("cli");
        let done = [user_prompt("fix the bug"), assistant_tool(), assistant_text("Fixed it.")].join("\n");
        let working = [user_prompt("go"), assistant_tool()].join("\n");
        let titled = [
            json!({"type":"custom-title","customTitle":"My title"}).to_string(),
            user_prompt("hi"),
            assistant_text("Hello"),
        ]
        .join("\n");
        for (id, body) in [("done", &done), ("working", &working), ("titled", &titled), ("owned", &done)] {
            let p = fx.write(&format!("projects/p/{id}.jsonl"), body);
            let mtime = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_millis((NOW - HOUR) as u64);
            File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap();
        }
        // "owned" belongs to a desktop session, so the CLI scan must skip it.
        fx.code_session("local_x", json!({"cliSessionId": "owned", "lastActivityAt": 0}));

        let mut ids: Vec<_> = scan(&fx.paths(), NOW).into_iter().map(|c| (c.id, c.title)).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![("cli:done".into(), "fix the bug".into()), ("cli:titled".into(), "My title".into())]
        );
    }

    #[test]
    fn clean_makes_one_short_line() {
        assert_eq!(clean("## Hi\n\n**there**  `x`", 50), "Hi there x");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(clean("see [api.js:4](src/api.js#L4) now", 50), "see api.js:4 now");
        assert_eq!(strip_tags("<ide_opened_file>x.md</ide_opened_file> fix it"), " fix it");
    }

    /// Prints what the scanner sees on this machine: `cargo test real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_machine() {
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        for c in scan(&Paths::detect().unwrap(), now) {
            println!("{:?} | {} | {} | {}h ago\n    {}", c.source, c.project, c.title,
                (now - c.activity_at) / HOUR, c.summary);
        }
    }
}

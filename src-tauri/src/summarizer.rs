//! "Where we stopped → next step" for each session, in a few words.
//!
//! Made by the `claude` command-line tool in headless mode (`claude -p`) with a small,
//! fast model, one session at a time in a background thread. Input is only the end of
//! the conversation (`scanner::excerpt`). Results are cached in `summaries.json` and
//! made again only when the session has changed.

use crate::scanner::{self, Card};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Give up on one call after this long.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// After a failed call, wait this long before trying the same session again.
const RETRY_AFTER_MS: i64 = 30 * 60 * 1000;
const LINE_CHARS: usize = 90;

const PROMPT: &str = r#"You help a busy person who runs many Claude sessions at the same time and keeps forgetting where each one stands.
You get the end of one session: messages from PERSON and from CLAUDE.
Reply with JSON only, no other text: {"stopped": "...", "next": "..."}
- Write both values in the language PERSON writes in.
- stopped: where the work stopped, at most 8 words.
- next: the single most useful next action for the PERSON (not for Claude), starting with a verb, at most 8 words.
  If Claude asked a question or waits for a decision or approval, say what to answer or decide.
  If the work is finished and nothing is left, say that the session can be archived."#;

/// What a card shows.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NextStep {
    pub stopped: String,
    pub next: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// The session's activity time this summary was made for.
    activity_at: i64,
    #[serde(flatten)]
    step: NextStep,
}

struct Job {
    card_id: String,
    activity_at: i64,
    transcript: PathBuf,
    title: String,
    project: String,
    waiting: bool,
}

#[derive(Default)]
struct Inner {
    cache: HashMap<String, Entry>,
    /// Card id -> don't retry before this time (unix ms).
    failed: HashMap<String, i64>,
    queue: VecDeque<Job>,
    queued: HashSet<String>,
    /// Where `claude` was found (looked up once).
    claude: Option<PathBuf>,
    /// Why the last call failed, shown on cards without a next step; cleared by a success.
    last_error: Option<String>,
}

/// Settings the worker needs for each call.
pub struct CallSettings {
    pub model: String,
    pub claude_path: String,
}

pub struct Summarizer {
    cache_file: PathBuf,
    /// Failed calls are written here (`summaries.log`).
    log_file: PathBuf,
    /// Folder the `claude` calls run in (the app's data folder, no project files there).
    work_dir: PathBuf,
    inner: Mutex<Inner>,
}

impl Summarizer {
    pub fn new(data_dir: &Path) -> Self {
        let cache_file = data_dir.join("summaries.json");
        let cache = std::fs::read(&cache_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            cache_file,
            log_file: data_dir.join("summaries.log"),
            work_dir: data_dir.to_path_buf(),
            inner: Mutex::new(Inner { cache, ..Default::default() }),
        }
    }

    /// The summary for a card, whether it was made for an older state of the session,
    /// and whether a newer one is being made.
    pub fn get(&self, card: &Card) -> (Option<NextStep>, bool, bool) {
        let g = self.inner.lock().unwrap();
        let entry = g.cache.get(&card.id);
        let outdated = entry.is_some_and(|e| e.activity_at < card.activity_at);
        (entry.map(|e| e.step.clone()), outdated, g.queued.contains(&card.id))
    }

    /// Queue sessions that have no up-to-date summary. Cards come in display order,
    /// so waiting sessions are summarized first.
    pub fn enqueue(&self, cards: &[Card], now: i64) {
        let mut g = self.inner.lock().unwrap();
        for card in cards {
            let Some(transcript) = &card.transcript else { continue };
            let fresh = g.cache.get(&card.id).is_some_and(|e| e.activity_at >= card.activity_at);
            let failed = g.failed.get(&card.id).is_some_and(|&until| until > now);
            if card.working || fresh || failed || g.queued.contains(&card.id) {
                continue;
            }
            g.queued.insert(card.id.clone());
            g.queue.push_back(Job {
                card_id: card.id.clone(),
                activity_at: card.activity_at,
                transcript: transcript.clone(),
                title: card.title.clone(),
                project: card.project.clone(),
                waiting: card.waiting,
            });
        }
    }

    /// Why next steps can't be made right now, if the last try failed (in plain words).
    pub fn last_error(&self) -> Option<String> {
        self.inner.lock().unwrap().last_error.as_deref().map(friendly_error)
    }

    /// "Retry": forget failures and look for `claude` again; the next scan queues everything again.
    pub fn retry(&self) {
        let mut g = self.inner.lock().unwrap();
        g.failed.clear();
        g.last_error = None;
        g.claude = None;
    }

    /// Forget summaries of sessions that are no longer recent.
    pub fn prune(&self, all_cards: &[Card]) {
        let alive: HashSet<&str> = all_cards.iter().map(|c| c.id.as_str()).collect();
        self.inner.lock().unwrap().cache.retain(|id, _| alive.contains(id.as_str()));
    }

    /// Make the next queued summary. Returns false when the queue is empty.
    pub fn work_one(&self, settings: &CallSettings, now: i64) -> bool {
        let (job, claude) = {
            let mut g = self.inner.lock().unwrap();
            let Some(job) = g.queue.pop_front() else { return false };
            if g.claude.is_none() {
                g.claude = find_claude(&settings.claude_path);
            }
            (job, g.claude.clone())
        };
        let result = match (claude, scanner::excerpt(&job.transcript)) {
            (None, _) => Err("`claude` command not found".to_string()),
            (_, None) => Err("empty conversation".to_string()),
            (Some(claude), Some(excerpt)) => {
                let input = format!(
                    "Session title: {}\nFolder: {}\nClaude is waiting for PERSON's reply: {}\n\n{}",
                    job.title,
                    job.project,
                    if job.waiting { "yes" } else { "no" },
                    excerpt
                );
                call_claude(&claude, &settings.model, &self.work_dir, &input)
            }
        };
        let mut g = self.inner.lock().unwrap();
        g.queued.remove(&job.card_id);
        match result {
            Ok(step) => {
                g.last_error = None;
                g.cache.insert(job.card_id, Entry { activity_at: job.activity_at, step });
                if let Ok(json) = serde_json::to_vec_pretty(&g.cache) {
                    let _ = std::fs::write(&self.cache_file, json);
                }
            }
            Err(e) => {
                self.log(&format!("{} failed: {e}", job.card_id));
                g.failed.insert(job.card_id, now + RETRY_AFTER_MS);
                g.last_error = Some(e);
            }
        }
        true
    }
}

impl Summarizer {
    fn log(&self, line: &str) {
        eprintln!("Claude Carousel: {line}");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log_file) {
            let _ = writeln!(f, "{stamp} {line}");
        }
    }
}

// ---------- running `claude` ----------

fn call_claude(claude: &Path, model: &str, work_dir: &Path, input: &str) -> Result<NextStep, String> {
    let mut cmd = Command::new(claude);
    cmd.args([
        "-p",
        "--model",
        model,
        "--output-format",
        "json",
        "--no-session-persistence", // leaves no new session on disk
        "--tools",
        "", // read only: no tools at all
        "--strict-mcp-config",
        "--setting-sources",
        "project", // the work dir has no project settings: no hooks or plugins
        "--system-prompt",
        PROMPT,
    ])
    .current_dir(work_dir)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = std::fs::create_dir_all(work_dir);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start claude: {e}"))?;

    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > CALL_TIMEOUT => {
                let _ = child.kill();
                return Err("claude took too long".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = reader.join().unwrap_or_default();
    let v: Value = serde_json::from_str(&out).map_err(|_| format!("unexpected output: {}", short(&out)))?;
    let text = v["result"].as_str().unwrap_or("");
    if v["is_error"].as_bool() == Some(true) {
        return Err(text.to_string());
    }
    parse_reply(text)
}

/// The model's reply: JSON, maybe wrapped in some text or a code block.
fn parse_reply(text: &str) -> Result<NextStep, String> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err(format!("no JSON in reply: {}", short(text)));
    };
    let v: Value = serde_json::from_str(&text[start..=end]).map_err(|e| e.to_string())?;
    let field = |k: &str| {
        v[k].as_str()
            .map(|s| shorten(s.trim()))
            .filter(|s| !s.is_empty())
            .ok_or(format!("reply has no {k:?}"))
    };
    Ok(NextStep { stopped: field("stopped")?, next: field("next")? })
}

/// Turn the usual failures into something a person can act on.
fn friendly_error(e: &str) -> String {
    let lower = e.to_lowercase();
    if lower.contains("authenticate") || lower.contains("log in") || lower.contains("login") {
        "claude is signed out. Run `claude auth login` in Terminal, then Retry.".into()
    } else if lower.contains("not found") || lower.contains("cannot start claude") {
        "the `claude` command was not found. Install Claude Code or set claudePath, then Retry.".into()
    } else {
        short(e)
    }
}

fn shorten(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= LINE_CHARS {
        one_line
    } else {
        one_line.chars().take(LINE_CHARS - 1).collect::<String>() + "…"
    }
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Apps started from the Dock / Start menu don't get your shell's PATH,
/// so look in the usual install places first, then ask a login shell.
fn find_claude(custom: &str) -> Option<PathBuf> {
    if !custom.trim().is_empty() {
        return Some(PathBuf::from(custom.trim()));
    }
    let home = dirs::home_dir()?;
    #[cfg(target_os = "windows")]
    let candidates = vec![
        home.join(".local").join("bin").join("claude.exe"),
        home.join("AppData").join("Roaming").join("npm").join("claude.cmd"),
    ];
    #[cfg(not(target_os = "windows"))]
    let candidates = vec![
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".npm-global/bin/claude"),
    ];
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    #[cfg(target_os = "windows")]
    let output = {
        use std::os::windows::process::CommandExt;
        Command::new("where").arg("claude").creation_flags(0x0800_0000).output()
    };
    #[cfg(not(target_os = "windows"))]
    let output = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        Command::new(shell).args(["-lc", "command -v claude"]).output()
    };
    let text = String::from_utf8_lossy(&output.ok()?.stdout).to_string();
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_wrapped_json() {
        let want = NextStep { stopped: "Ads launched".into(), next: "Check ad stats".into() };
        assert_eq!(parse_reply(r#"{"stopped":"Ads launched","next":"Check ad stats"}"#).unwrap(), want);
        assert_eq!(
            parse_reply("Sure:\n```json\n{\"stopped\": \" Ads  launched \", \"next\": \"Check ad stats\"}\n```").unwrap(),
            want
        );
    }

    #[test]
    fn rejects_bad_replies() {
        assert!(parse_reply("I can't help with that").is_err());
        assert!(parse_reply(r#"{"stopped":"x"}"#).is_err());
        assert!(parse_reply(r#"{"stopped":"x","next":"  "}"#).is_err());
    }

    /// Summarizes the most recent sessions on this machine with the real `claude`:
    /// `cargo test real_summaries -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_summaries() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let cards = scanner::scan(&scanner::Paths::detect().unwrap(), now, 3 * 24 * 3_600_000);
        let dir = std::env::temp_dir().join("claude-carousel-real-summaries");
        std::fs::create_dir_all(&dir).unwrap();
        let s = Summarizer::new(&dir);
        let cards: Vec<Card> = cards.into_iter().filter(|c| !c.working).take(5).collect();
        s.enqueue(&cards, now);
        let settings = CallSettings { model: "haiku".into(), claude_path: String::new() };
        while s.work_one(&settings, now) {}
        for c in &cards {
            let (step, _, _) = s.get(c);
            println!("{} | waiting={}\n    {:?}", c.title, c.waiting, step);
        }
    }
}

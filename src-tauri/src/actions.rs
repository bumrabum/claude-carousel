//! "Open" a session in the place it lives.

use crate::scanner::{Card, Source};
use std::process::Command;

/// Windows: run helper `cmd` without flashing a console window.
#[cfg(target_os = "windows")]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}

pub fn open(card: &Card) -> Result<(), String> {
    match card.source {
        // Desktop app link that jumps to any (non-archived) Code session by id.
        Source::Code => open_url(&format!(
            "claude://code/continue?session={}&source=desktop_action",
            card.session_id
        )),
        // The app has no "open by id" link for Cowork; this one jumps to the task
        // only when it waits on a permission prompt, otherwise it just opens the app.
        Source::Cowork => open_url(&format!(
            "claude://code/needs-input?session={}&source=desktop_action",
            card.session_id
        )),
        Source::Cli => resume_in_terminal(&card.cwd, &card.session_id),
    }
}

fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        hidden(&mut c);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// New terminal window in the session's folder running `claude --resume <id>`.
fn resume_in_terminal(cwd: &str, session_id: &str) -> Result<(), String> {
    if !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("unexpected session id".into());
    }
    #[cfg(target_os = "macos")]
    {
        // Shell: cd '<cwd>' && claude --resume <id>   (single quotes escaped for sh)
        let shell = format!("cd '{}' && claude --resume {}", cwd.replace('\'', r"'\''"), session_id);
        // AppleScript string: escape backslashes and double quotes.
        let script_text = shell.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "tell application \"Terminal\"\n activate\n do script \"{script_text}\"\nend tell"
        );
        Command::new("osascript")
            .args(["-e", &script])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        hidden(&mut Command::new("cmd"))
            .args(["/C", "start", "", "/D", cwd, "cmd", "/K", "claude", "--resume", session_id])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = cwd;
        Err("opening a terminal is only set up for Mac and Windows".into())
    }
}

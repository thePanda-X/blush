//! Shell mode (`!command`): run a command in the project directory and hand
//! its output to Claude with the next prompt.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Output kept for display; the copy sent to Claude is shorter.
const KEEP: usize = 200_000;
/// How much output goes to Claude with the next prompt.
pub const CONTEXT_CHARS: usize = 30_000;

pub struct ShellResult {
    pub code: Option<i32>,
    pub output: String,
}

/// The shell `!` commands run in: PowerShell on Windows (pwsh if
/// installed), `$SHELL` (or sh) elsewhere.
pub fn program() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        static PS: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
        let ps = *PS.get_or_init(|| if which::which("pwsh").is_ok() { "pwsh" } else { "powershell.exe" });
        // UTF-8 output, so non-ASCII text survives the pipe.
        let pre = "[Console]::OutputEncoding=[Text.Encoding]::UTF8;$OutputEncoding=[Text.Encoding]::UTF8;";
        (ps.into(), vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), pre.into()])
    }
    #[cfg(not(windows))]
    {
        let sh = std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "sh".into());
        (sh, vec!["-c".into()])
    }
}

pub fn name() -> String {
    let (p, _) = program();
    Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(p)
}

/// Run `cmd` to completion. Its process id goes into `pid` as soon as it
/// starts, so [`kill_tree`] can stop it and everything it spawned.
pub async fn run(cmd: &str, cwd: &Path, pid: Arc<AtomicU32>) -> std::io::Result<ShellResult> {
    let (prog, mut args) = program();
    // PowerShell takes the prelude and the command as one script, then
    // reports a failing native command's own exit code rather than 1.
    match args.last_mut() {
        Some(last) if !last.starts_with('-') => {
            last.push_str(cmd);
            last.push_str("
if ($LASTEXITCODE) { exit $LASTEXITCODE } elseif (-not $?) { exit 1 }");
        }
        _ => args.push(cmd.to_string()),
    }
    let mut c = tokio::process::Command::new(&prog);
    c.args(&args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    // Its own process group, so stopping it reaches its children too.
    #[cfg(unix)]
    c.process_group(0);
    let child = c.spawn()?;
    if let Some(id) = child.id() {
        pid.store(id, Ordering::SeqCst);
    }
    let out = child.wait_with_output().await?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    let text = strip_ansi(&text.replace("\r\n", "\n"));
    Ok(ShellResult { code: out.status.code(), output: truncate_middle(text.trim_end(), KEEP) })
}

/// Stop a command started by [`run`] and everything it spawned.
pub fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The note Claude gets with the next prompt.
pub fn context_note(cmd: &str, r: &ShellResult) -> String {
    let code = r.code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into());
    let body = if r.output.trim().is_empty() { "(no output)".to_string() } else { truncate_middle(&r.output, CONTEXT_CHARS) };
    format!("I ran this command in my shell ({}), exit code {code}:\n$ {cmd}\n```\n{body}\n```", name())
}

/// Keep the head and the tail of long output.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let half = max / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().rev().take(half).collect::<Vec<_>>().into_iter().rev().collect();
    let cut = s.chars().count() - 2 * half;
    format!("{head}\n… [{cut} characters omitted] …\n{tail}")
}

/// Drop ANSI escape sequences (colours, cursor movement).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                it.next();
                // CSI: parameters, then one final byte in @..~
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            Some(']') => {
                it.next();
                // OSC: until BEL or ST
                while let Some(d) = it.next() {
                    if d == '\x07' || (d == '\x1b' && it.peek() == Some(&'\\')) {
                        if d == '\x1b' {
                            it.next();
                        }
                        break;
                    }
                }
            }
            _ => {
                it.next();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes_and_truncates() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m \x1b]0;title\x07ok"), "red ok");
        let t = truncate_middle(&"x".repeat(100), 20);
        assert!(t.starts_with("xxxxxxxxxx\n…") && t.ends_with("…\nxxxxxxxxxx"), "{t}");
        assert_eq!(truncate_middle("short", 20), "short");
    }

    #[tokio::test]
    async fn runs_a_command() {
        let pid = Arc::new(AtomicU32::new(0));
        let r = run("echo blush-shell-test", &std::env::temp_dir(), pid.clone()).await.unwrap();
        assert_eq!(r.code, Some(0));
        assert_ne!(pid.load(Ordering::SeqCst), 0);
        assert!(r.output.contains("blush-shell-test"), "{}", r.output);
        let note = context_note("echo hi", &r);
        assert!(note.contains("exit code 0") && note.contains("$ echo hi"));
        // A native command's own exit code comes through.
        let cmd = if cfg!(windows) { "cmd /c exit 5" } else { "exit 5" };
        let r = run(cmd, &std::env::temp_dir(), Arc::new(AtomicU32::new(0))).await.unwrap();
        assert_eq!(r.code, Some(5));
    }

    #[tokio::test]
    async fn kill_tree_stops_children() {
        let pid = Arc::new(AtomicU32::new(0));
        // A shell that starts a long-running child of its own.
        let cmd = if cfg!(windows) { "cmd /c ping -n 30 127.0.0.1" } else { "sleep 30; echo done" };
        let p2 = pid.clone();
        let task = tokio::spawn(async move { run(cmd, &std::env::temp_dir(), p2).await });
        for _ in 0..100 {
            if pid.load(Ordering::SeqCst) != 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let started = std::time::Instant::now();
        kill_tree(pid.load(Ordering::SeqCst));
        // The pipes close only once every process holding them is gone.
        let r = tokio::time::timeout(std::time::Duration::from_secs(10), task).await;
        assert!(r.is_ok(), "children kept running after kill_tree");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}

//! Narrator: read a short summary of each finished turn aloud with the
//! system's built-in voice (`say` on macOS, System.Speech on Windows,
//! speech-dispatcher / espeak on Linux).

use std::io::Write as _;
use std::process::{Child, Command, Stdio};

#[derive(Default)]
pub struct Narrator {
    child: Option<Child>,
}

impl Narrator {
    pub fn speaking(&mut self) -> bool {
        match &mut self.child {
            Some(c) => match c.try_wait() {
                Ok(None) => true,
                _ => {
                    self.child = None;
                    false
                }
            },
            None => false,
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Speak `text`, interrupting anything already playing.
    pub fn speak(&mut self, text: &str) -> Result<(), String> {
        self.stop();
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let mut child = spawn_tts().map_err(|e| format!("couldn't start text-to-speech: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        self.child = Some(child);
        Ok(())
    }
}

impl Drop for Narrator {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_tts() -> std::io::Result<Child> {
    let mut c = tts_command()?;
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    c.spawn()
}

#[cfg(target_os = "macos")]
fn tts_command() -> std::io::Result<Command> {
    let mut c = Command::new("say");
    c.args(["-f", "-"]);
    Ok(c)
}

#[cfg(windows)]
fn tts_command() -> std::io::Result<Command> {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("powershell.exe");
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::InputEncoding=[Text.Encoding]::UTF8; Add-Type -AssemblyName System.Speech; \
         $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Speak([Console]::In.ReadToEnd())",
    ]);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    Ok(c)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn tts_command() -> std::io::Result<Command> {
    if which::which("spd-say").is_ok() {
        // -e: read the text from stdin; -w: block until done so we can track it.
        let mut c = Command::new("spd-say");
        c.args(["-w", "-e"]);
        return Ok(c);
    }
    for bin in ["espeak-ng", "espeak"] {
        if which::which(bin).is_ok() {
            let mut c = Command::new(bin);
            c.arg("--stdin");
            return Ok(c);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "install speech-dispatcher (spd-say) or espeak-ng"))
}

/// A spoken summary of an agent reply: its opening sentences, without
/// markdown, code or links, capped at roughly 300 characters.
pub fn summarize(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || t.is_empty() || t.starts_with('|') || t.starts_with("---") {
            if !out.is_empty() && t.is_empty() && out.len() > 120 {
                break;
            }
            continue;
        }
        let t = t.trim_start_matches(['#', '>', '-', '*', '+', ' ']);
        let t = strip_inline(t);
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&t);
        if out.len() > 300 {
            break;
        }
    }
    cut_at_sentence(&out, 300)
}

fn strip_inline(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' => {}
            '[' => {
                // [text](url) -> text
                let mut label = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    label.push(d);
                }
                if chars.peek() == Some(&'(') {
                    for d in chars.by_ref() {
                        if d == ')' {
                            break;
                        }
                    }
                }
                out.push_str(&label);
            }
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

fn cut_at_sentence(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];
    match head.rfind(['.', '!', '?', '。', '！', '？']) {
        Some(i) if i > max / 3 => head[..=i + head[i..].chars().next().map_or(0, |c| c.len_utf8() - 1)].to_string(),
        _ => format!("{}…", head.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_skips_code_and_markup() {
        let md = "I added **div** to `calc.py`.\n\n```py\ndef div(a, b): ...\n```\n\nSee [the docs](http://x).";
        assert_eq!(summarize(md), "I added div to calc.py. See the docs.");
    }

    #[test]
    fn summary_is_capped_at_a_sentence() {
        let md = format!("{} Second sentence goes on. {}", "First sentence is here.".repeat(8), "x".repeat(400));
        let s = summarize(&md);
        assert!(s.len() <= 301 && s.ends_with('.'), "{s}");
    }
}

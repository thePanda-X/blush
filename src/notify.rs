//! Attention signals: the terminal bell, plus a desktop notification when
//! the terminal isn't focused (Windows toast, macOS Notification Center,
//! `notify-send` on Linux). Best effort: failures are silent.
//!
//! The text often comes from Claude's reply or a tool title, so it never
//! goes into a script: it's passed in environment variables (or as plain
//! arguments) and escaped by the receiving side.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// BEL: most terminals flash the tab or taskbar, or play a sound.
pub fn bell() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

/// Show a desktop notification without blocking.
pub fn desktop(title: &str, body: &str) {
    let title = clean(title, 80);
    let body = clean(body, 200);
    std::thread::spawn(move || {
        let _ = spawn(&title, &body).and_then(|mut c| c.wait());
    });
}

/// One line, no control characters, at most `max` characters.
fn clean(s: &str, max: usize) -> String {
    let one: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let one = one.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max { format!("{}…", one.chars().take(max - 1).collect::<String>()) } else { one }
}

/// A toast through the WinRT API, under PowerShell's own app id (no
/// registration or modules needed). The script is constant; the text
/// arrives in environment variables and is XML-escaped by PowerShell.
#[cfg_attr(not(windows), allow(dead_code))]
const TOAST_SCRIPT: &str = "$ErrorActionPreference='Stop';\
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] > $null;\
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType=WindowsRuntime] > $null;\
$t = [Security.SecurityElement]::Escape($env:BLUSH_TOAST_TITLE);\
$b = [Security.SecurityElement]::Escape($env:BLUSH_TOAST_BODY);\
$x = New-Object Windows.Data.Xml.Dom.XmlDocument;\
$x.LoadXml(\"<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>\");\
$n = New-Object Windows.UI.Notifications.ToastNotification $x;\
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($n)";

#[cfg(windows)]
fn spawn(title: &str, body: &str) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", TOAST_SCRIPT]);
    c.env("BLUSH_TOAST_TITLE", title).env("BLUSH_TOAST_BODY", body);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    quiet(&mut c).spawn()
}

#[cfg(target_os = "macos")]
fn spawn(title: &str, body: &str) -> std::io::Result<std::process::Child> {
    // AppleScript reads the text from its arguments, never from the script.
    let script = "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";
    quiet(Command::new("osascript").args(["-e", script, title, body])).spawn()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn(title: &str, body: &str) -> std::io::Result<std::process::Child> {
    quiet(Command::new("notify-send").args(["--app-name=blush", "--", title, body])).spawn()
}

fn quiet(c: &mut Command) -> &mut Command {
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_text() {
        assert_eq!(clean("a\nb\x1b[31m  c", 50), "a b [31m c");
        assert_eq!(clean("abcdef", 4), "abc…");
    }

    /// Hostile text stays data: build the toast (without showing it) from
    /// text full of quote characters PowerShell would otherwise act on.
    #[cfg(windows)]
    #[test]
    fn toast_text_is_never_code() {
        use std::os::windows::process::CommandExt;
        let evil = "x’);Write-Output PWNED;$x.LoadXml(’ <b>&amp;</b> \"q\" 'y'";
        let script = TOAST_SCRIPT.replace(".Show($n)", "| Out-Null; Write-Output $x.GetXml()");
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .env("BLUSH_TOAST_TITLE", evil)
            .env("BLUSH_TOAST_BODY", "I’ve finished")
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
        assert!(!text.lines().any(|l| l.trim() == "PWNED"), "{text}");
        assert!(text.contains("I’ve finished") || text.contains("I&#8217;ve") || text.contains("finished"), "{text}");
    }
}

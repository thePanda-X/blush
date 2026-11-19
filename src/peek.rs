//! Quick Peek: open or reveal a file a tool touched, and describe it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Resolve a tool location against the session's working directory.
pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() { path } else { cwd.join(path) }
}

/// "calc.py · Python · 1.6 KB · modified 3m ago"
pub fn describe(p: &Path) -> String {
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
    let Ok(meta) = std::fs::metadata(p) else { return format!("{name} · not found") };
    if meta.is_dir() {
        return format!("{name}/ · folder");
    }
    let kind = kind_of(p);
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .map(|d| format!(" · modified {} ago", short_age(d.as_secs())))
        .unwrap_or_default();
    format!("{name} · {kind} · {}{age}", human_size(meta.len()))
}

fn kind_of(p: &Path) -> String {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let k = match ext.as_str() {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "go" => "Go",
        "md" => "Markdown",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => "image",
        "mp4" | "mov" | "webm" | "mkv" => "video",
        "pdf" => "PDF",
        "" => "file",
        other => return other.to_uppercase(),
    };
    k.to_string()
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

fn short_age(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// Files whose default "open" action is to run them (Windows Script Host,
/// batch files, installers, binaries). These are revealed instead.
fn runs_when_opened(p: &Path) -> bool {
    const RUNNABLE: &[&str] = &[
        "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi", "msc", "scr",
        "lnk", "hta", "cpl", "jar", "reg", "sh", "command", "app", "url",
    ];
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| RUNNABLE.contains(&e.to_ascii_lowercase().as_str()))
}

pub enum Opened {
    Opened,
    /// Executable or script: shown in the file manager rather than run.
    Revealed,
}

/// Open with the system's default app, unless that would execute the file.
pub fn open(p: &Path) -> std::io::Result<Opened> {
    if runs_when_opened(p) {
        return reveal(p).map(|_| Opened::Revealed);
    }
    open_default(p).map(|_| Opened::Opened)
}

fn open_default(p: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = Command::new("open");
        c.arg(p);
        c
    };
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("explorer.exe");
        c.arg(p);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = {
        let mut c = Command::new("xdg-open");
        c.arg(p);
        c
    };
    detach(&mut c)
}

/// Show the file selected in Finder / Explorer / the file manager.
pub fn reveal(p: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = Command::new("open");
        c.arg("-R").arg(p);
        c
    };
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("explorer.exe");
        // explorer needs `/select,<path>` as one raw argument.
        // It also only understands backslashes.
        let path = p.display().to_string().replace('/', "\\");
        c.raw_arg(format!("/select,\"{path}\""));
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = {
        let mut c = Command::new("xdg-open");
        c.arg(p.parent().unwrap_or(p));
        c
    };
    detach(&mut c)
}

fn detach(c: &mut Command) -> std::io::Result<()> {
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // Reap it so `open` / `xdg-open` don't linger as zombies on Unix.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_kinds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1640), "1.6 KB");
        assert_eq!(kind_of(Path::new("a/calc.py")), "Python");
        assert_eq!(kind_of(Path::new("x.zig")), "ZIG");
    }
}

//! `blush web`: a browser companion for blush sessions.
//!
//! ```text
//!   browser ──ws /ws──▶ hub (blush web serve) ◀──ws /host── blush TUI (shares its session)
//!                         │
//!                         └─ spawns ─▶ blush web host (a headless session opened from the web)
//! ```
//!
//! The hub is a small axum server. Every running TUI (and every session the
//! web opens) is a *host*: it streams its transcript to the hub as diffs and
//! takes commands (prompts, stop, permission answers) back. The hub caches
//! each host's state and relays it to the browsers attached to it.

pub mod assets;
pub mod host;
pub mod hub;
pub mod sync;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::config::home_dir;

/// What `blush web serve` writes once it's listening.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerInfo {
    pub pid: u32,
    pub port: u16,
    pub bind: String,
    /// Unix seconds.
    pub started: i64,
    pub version: String,
}

pub fn web_dir() -> PathBuf {
    home_dir().join("web")
}

fn info_path() -> PathBuf {
    web_dir().join("server.json")
}

fn token_path() -> PathBuf {
    web_dir().join("token")
}

pub fn log_path() -> PathBuf {
    home_dir().join("logs").join("web.log")
}

/// The access token browsers need. Created once and kept, so a link saved on
/// a phone keeps working across restarts.
pub fn token() -> Result<String> {
    let path = token_path();
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 16 {
            return Ok(t);
        }
    }
    let mut buf = [0u8; 18];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("no randomness for the web token: {e}"))?;
    let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(web_dir())?;
    std::fs::write(&path, &t).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(&path);
    Ok(t)
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_permissions(_: &std::path::Path) {}

pub fn read_info() -> Option<ServerInfo> {
    serde_json::from_str(&std::fs::read_to_string(info_path()).ok()?).ok()
}

pub fn write_info(info: &ServerInfo) -> Result<()> {
    std::fs::create_dir_all(web_dir())?;
    std::fs::write(info_path(), serde_json::to_string_pretty(info)?)?;
    Ok(())
}

/// Remove server.json, but only if it still describes this process.
pub fn clear_info(pid: u32) {
    if read_info().is_some_and(|i| i.pid == pid) {
        let _ = std::fs::remove_file(info_path());
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(1500)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// The running server, if server.json points at one that answers.
pub fn running() -> Option<ServerInfo> {
    let info = read_info()?;
    let url = format!("http://127.0.0.1:{}/api/health", info.port);
    let mut res = agent().get(&url).call().ok()?;
    let v: serde_json::Value = serde_json::from_str(&res.body_mut().read_to_string().ok()?).ok()?;
    (v.get("pid").and_then(|p| p.as_u64()) == Some(info.pid as u64)).then_some(info)
}

/// Addresses other devices can use, best first, then localhost.
pub fn urls(info: &ServerInfo, token: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    let local_only = info.bind == "127.0.0.1" || info.bind == "localhost" || info.bind == "::1";
    if !local_only {
        let mut ips: Vec<(u8, String)> = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !i.is_loopback())
            .filter_map(|i| match i.ip() {
                std::net::IpAddr::V4(v4) if !v4.is_link_local() => {
                    // Private LAN ranges first, then Tailscale-style CGNAT, then the rest.
                    let o = v4.octets();
                    let rank = if v4.is_private() {
                        0
                    } else if o[0] == 100 && (64..128).contains(&o[1]) {
                        1
                    } else {
                        2
                    };
                    Some((rank, v4.to_string()))
                }
                _ => None,
            })
            .collect();
        ips.sort();
        ips.dedup();
        hosts.extend(ips.into_iter().map(|(_, ip)| ip));
        if info.bind != "0.0.0.0" && info.bind != "::" {
            hosts = vec![info.bind.clone()];
        }
    }
    hosts.push("localhost".into());
    hosts.into_iter().map(|h| format!("http://{h}:{}/?token={token}", info.port)).collect()
}

/// A QR code for `text`, drawn with half blocks (two rows per line).
pub fn qr(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = code.width();
    let dark = |x: isize, y: isize| -> bool {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && code[(x as usize, y as usize)] == qrcode::Color::Dark
    };
    // A quiet zone of 1 module; dark modules print as spaces so the code
    // reads on a dark terminal (light background = "on").
    let mut out = String::new();
    let (lo, hi) = (-1isize, w as isize + 1);
    let mut y = lo;
    while y < hi {
        for x in lo..hi {
            let top = !dark(x, y);
            let bottom = !dark(x, y + 1) && y + 1 < hi;
            out.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    Some(out)
}

fn uptime(started: i64) -> String {
    let secs = (chrono::Utc::now().timestamp() - started).max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// Live session count from the hub, for `blush web`.
fn session_count(info: &ServerInfo, token: &str) -> Option<u64> {
    let url = format!("http://127.0.0.1:{}/api/status", info.port);
    let mut res = agent().get(&url).header("x-blush-token", token).call().ok()?;
    let v: serde_json::Value = serde_json::from_str(&res.body_mut().read_to_string().ok()?).ok()?;
    v.get("sessions").and_then(|s| s.as_u64())
}

/// Human-readable status, for `blush web` and `/web`.
pub fn status_text(with_qr: bool) -> String {
    match running() {
        Some(info) => describe(&info, "is running", with_qr),
        None => "The web server isn't running. Start it with `blush web up` (or /web up).".into(),
    }
}

fn describe(info: &ServerInfo, verb: &str, with_qr: bool) -> String {
    let token = token().unwrap_or_default();
    let urls = urls(info, &token);
    let mut s = format!("blush web {verb} on port {} (pid {}, up {})", info.port, info.pid, uptime(info.started));
    if let Some(n) = session_count(info, &token) {
        s.push_str(&format!(" · {n} live session{}", if n == 1 { "" } else { "s" }));
    }
    s.push_str("\n\nOpen on your phone or any browser:\n");
    for u in &urls {
        s.push_str(&format!("  {u}\n"));
    }
    if with_qr && let Some(code) = urls.first().and_then(|u| qr(u)) {
        s.push('\n');
        s.push_str(&code);
    }
    s.push_str("\nStop it with `blush web down`.");
    s
}

/// Start the server in the background (or report the one already running).
/// `blush web serve` reads the port and bind address from the config.
pub fn up(with_qr: bool) -> Result<String> {
    if let Some(info) = running() {
        return Ok(describe(&info, "is already running", with_qr));
    }
    token()?;
    let exe = std::env::current_exe().context("locating the blush binary")?;
    let log = log_path();
    std::fs::create_dir_all(log.parent().unwrap())?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["web", "serve"])
        .stdin(std::process::Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    let mut child = spawn_detached(&mut cmd).context("starting the web server")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(info) = running() {
            return Ok(describe(&info, "is up", with_qr));
        }
        if let Ok(Some(status)) = child.try_wait() {
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(4).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            bail!("the web server exited ({status}).\n{}", tail.join("\n"));
        }
        if Instant::now() > deadline {
            bail!("the web server didn't come up in time; see {}", log.display());
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Stop the server. Ok(false) when it wasn't running.
pub fn down() -> Result<bool> {
    let Some(info) = running() else {
        // A stale file from a crash: tidy it up.
        if let Some(i) = read_info() {
            clear_info(i.pid);
        }
        return Ok(false);
    };
    let token = token()?;
    let url = format!("http://127.0.0.1:{}/api/shutdown", info.port);
    let _ = agent().post(&url).header("x-blush-token", &token).send_empty();
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if running().is_none() {
            clear_info(info.pid);
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    kill_pid(info.pid);
    clear_info(info.pid);
    Ok(true)
}

/// Spawn a process that outlives this one and its terminal.
pub fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Leave the terminal's job object when it allows it, so closing the
        // terminal doesn't take the server down with it.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(c) => Ok(c),
            Err(_) => {
                cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
                cmd.spawn()
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()
    }
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// `blush web [status|up|down]` from the command line.
pub fn cli(action: Option<&str>) -> i32 {
    match action.unwrap_or("status") {
        "status" => {
            println!("{}", status_text(true));
            0
        }
        "up" | "start" => match up(true) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        },
        "down" | "stop" => match down() {
            Ok(true) => {
                println!("blush web stopped.");
                0
            }
            Ok(false) => {
                println!("The web server wasn't running.");
                0
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        },
        other => {
            eprintln!("unknown action `{other}` (use: blush web, blush web up, blush web down)");
            2
        }
    }
}

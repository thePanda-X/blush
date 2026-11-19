//! Self-updater.
//!
//! On launch blush asks GitHub whether the tracked branch has moved past the
//! commit this binary was built from (stamped in by `build.rs`). If it has,
//! the source is fetched into `~/.blush/src`, rebuilt with cargo, swapped in
//! over the running executable and relaunched with the same arguments, all
//! behind a full-screen animation. Any failure falls back to launching the
//! current version.

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{cursor, execute, queue};

use crate::config::{self, UpdateConfig};

/// Commit this binary was built from ("" when built outside a git checkout).
const BUILD_SHA: &str = env!("BLUSH_GIT_SHA");
/// Set to skip the launch-time check for one run.
const SKIP_ENV: &str = "BLUSH_NO_UPDATE";
/// Set on the relaunched process so it doesn't check again.
const RELAUNCHED_ENV: &str = "BLUSH_JUST_UPDATED";

fn build_dirty() -> bool {
    env!("BLUSH_GIT_DIRTY") == "1"
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

// ───────────────────────────────────────────────────────────── entry points

/// Called before the TUI starts. Returns `Some(exit_code)` when an update was
/// installed and the new binary ran in our place; `None` to carry on normally.
pub fn on_launch(cfg: &UpdateConfig) -> Option<i32> {
    cleanup_old_binaries();
    if !cfg.auto || cfg!(debug_assertions) || env_flag(SKIP_ENV) || env_flag(RELAUNCHED_ENV) {
        return None;
    }
    // Dev builds (dirty tree / unknown commit) are never replaced behind your back.
    if BUILD_SHA.is_empty() || build_dirty() {
        return None;
    }
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return None;
    }
    let Ok(Status::Behind(rel)) = check(cfg, BUILD_SHA) else { return None };
    if tools_missing().is_some() {
        return None;
    }
    match animate(cfg, &rel) {
        Outcome::Updated(exe) => relaunch(&exe),
        Outcome::Skipped | Outcome::Failed => None,
    }
}

/// `blush --update`: update now (even dev builds), then exit.
pub fn run_now(cfg: &UpdateConfig) -> i32 {
    cleanup_old_binaries();
    let status = if BUILD_SHA.is_empty() { Ok(Status::Unknown) } else { check(cfg, BUILD_SHA) };
    let rel = match status {
        Err(e) => {
            eprintln!("blush: couldn't check GitHub for updates: {e:#}");
            return 1;
        }
        Ok(Status::UpToDate) if !build_dirty() => {
            println!("blush is up to date ({})", env!("BLUSH_VERSION"));
            return 0;
        }
        Ok(Status::Behind(r)) => r,
        Ok(_) => match latest(cfg) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("blush: couldn't check GitHub for updates: {e:#}");
                return 1;
            }
        },
    };
    if let Some(tool) = tools_missing() {
        eprintln!("blush: updating needs `{tool}` on your PATH");
        return 1;
    }
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        return match animate(cfg, &rel) {
            Outcome::Updated(_) => 0,
            _ => 1,
        };
    }
    println!("updating blush → {} …", short(&rel.sha));
    let sh = Shared::new(&rel);
    match work(cfg, &sh) {
        Ok(_) => {
            println!("blush updated to {}", short(&sh.state().target));
            0
        }
        Err(e) => {
            eprintln!("blush: update failed: {e:#}");
            1
        }
    }
}

fn tools_missing() -> Option<&'static str> {
    ["git", "cargo"].into_iter().find(|t| which::which(t).is_err())
}

// ───────────────────────────────────────────────────────────── github

#[derive(Clone, Debug)]
struct Release {
    sha: String,
    /// Commits between this build and `sha` (0 = unknown).
    ahead_by: u64,
    /// Subject line of the newest commit.
    message: String,
}

enum Status {
    UpToDate,
    Behind(Release),
    /// This build has commits GitHub doesn't (or GitHub doesn't know the commit).
    Diverged,
    Unknown,
}

fn api(path: &str) -> Result<serde_json::Value> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().into();
    let mut resp = agent
        .get(&format!("https://api.github.com/repos/{path}"))
        .header("User-Agent", concat!("blush-updater/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()?;
    Ok(serde_json::from_reader(resp.body_mut().as_reader())?)
}

fn subject(v: &serde_json::Value) -> String {
    v["commit"]["message"].as_str().unwrap_or_default().lines().next().unwrap_or_default().trim().to_string()
}

fn check(cfg: &UpdateConfig, base: &str) -> Result<Status> {
    let v = match api(&format!("{}/compare/{base}...{}", cfg.repo, cfg.branch)) {
        Ok(v) => v,
        Err(e) => {
            return match e.downcast_ref::<ureq::Error>() {
                Some(ureq::Error::StatusCode(404 | 422)) => Ok(Status::Unknown),
                _ => Err(e),
            };
        }
    };
    Ok(match v["status"].as_str() {
        Some("ahead") => {
            let newest = v["commits"].as_array().and_then(|c| c.last()).ok_or_else(|| anyhow!("empty compare"))?;
            Status::Behind(Release {
                sha: newest["sha"].as_str().unwrap_or_default().to_string(),
                ahead_by: v["ahead_by"].as_u64().unwrap_or(0),
                message: subject(newest),
            })
        }
        Some("diverged") => Status::Diverged,
        _ => Status::UpToDate,
    })
}

fn latest(cfg: &UpdateConfig) -> Result<Release> {
    let v = api(&format!("{}/commits/{}", cfg.repo, cfg.branch))?;
    Ok(Release { sha: v["sha"].as_str().unwrap_or_default().to_string(), ahead_by: 0, message: subject(&v) })
}

// ───────────────────────────────────────────────────────────── the work

const STAGES: usize = 5;
const CHECK: usize = 0;
const FETCH: usize = 1;
const BUILD: usize = 2;
const INSTALL: usize = 3;
const RELAUNCH: usize = 4;

struct State {
    stage: usize,
    started: Instant,
    took: [Option<Duration>; STAGES],
    /// cargo's "N/M" unit counter.
    built: Option<(u32, u32)>,
    krate: String,
    target: String,
}

struct Shared {
    st: Mutex<State>,
    cancel: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Shared {
    fn new(rel: &Release) -> Arc<Shared> {
        Arc::new(Shared {
            st: Mutex::new(State {
                stage: CHECK,
                started: Instant::now(),
                took: [None; STAGES],
                built: None,
                krate: String::new(),
                target: rel.sha.clone(),
            }),
            cancel: AtomicBool::new(false),
            child: Mutex::new(None),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn advance(&self, stage: usize) {
        let mut s = self.state();
        let prev = s.stage;
        s.took[prev] = Some(s.started.elapsed());
        s.stage = stage;
        s.started = Instant::now();
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = c.kill();
        }
    }
    fn bail_if_cancelled(&self) -> Result<()> {
        if self.cancelled() { Err(anyhow!(Cancelled)) } else { Ok(()) }
    }
}

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

fn src_dir() -> PathBuf {
    config::home_dir().join("src")
}

/// Fetch → build → install. Returns the path of the installed binary.
fn work(cfg: &UpdateConfig, sh: &Shared) -> Result<PathBuf> {
    let src = src_dir();
    let url = format!("https://github.com/{}.git", cfg.repo);

    sh.advance(FETCH);
    if src.join(".git").is_dir() {
        let mut fetch = Command::new("git");
        fetch.current_dir(&src).args(["fetch", "--depth", "1", "--no-tags", &url, &cfg.branch]);
        run(sh, fetch, |_| {})?;
        let mut co = Command::new("git");
        co.current_dir(&src).args(["checkout", "--force", "--detach", "FETCH_HEAD"]);
        run(sh, co, |_| {})?;
    } else {
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(config::home_dir())?;
        let mut clone = Command::new("git");
        clone.args(["clone", "--depth", "1", "--branch", &cfg.branch, &url]).arg(&src);
        run(sh, clone, |_| {})?;
    }
    if let Ok(o) = Command::new("git").current_dir(&src).args(["rev-parse", "HEAD"]).output()
        && o.status.success()
    {
        sh.state().target = String::from_utf8_lossy(&o.stdout).trim().to_string();
    }
    sh.bail_if_cancelled()?;

    sh.advance(BUILD);
    let target_dir = src.join("target");
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(&src)
        .args(["build", "--release", "--locked"])
        .env("CARGO_TARGET_DIR", &target_dir)
        // Make cargo print its "Building [===>  ] 57/248: crate" line even
        // though stderr is a pipe; that's what drives the progress bar.
        .env("CARGO_TERM_PROGRESS_WHEN", "always")
        .env("CARGO_TERM_PROGRESS_WIDTH", "200")
        .env("CARGO_TERM_COLOR", "never");
    run(sh, cargo, |line| parse_cargo(line, sh))?;
    sh.bail_if_cancelled()?;

    sh.advance(INSTALL);
    let exe = std::env::current_exe().context("locating the running blush")?;
    let built = target_dir.join("release").join(format!("blush{}", std::env::consts::EXE_SUFFIX));
    install(&built, &exe)?;

    sh.advance(RELAUNCH);
    Ok(exe)
}

fn parse_cargo(line: &str, sh: &Shared) {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix("Compiling ") {
        sh.state().krate = rest.split(' ').next().unwrap_or_default().to_string();
    } else if t.starts_with("Building [")
        && let Some(i) = t.find("] ")
    {
        let after = &t[i + 2..];
        let (nums, names) = after.split_once(':').unwrap_or((after, ""));
        if let Some((n, m)) = nums.trim().split_once('/')
            && let (Ok(n), Ok(m)) = (n.parse(), m.parse())
        {
            let mut s = sh.state();
            s.built = Some((n, m));
            if let Some(k) = names.split(',').next().map(|k| k.split('(').next().unwrap_or(k).trim()).filter(|k| !k.is_empty()) {
                s.krate = k.to_string();
            }
        }
    }
}

/// Run `cmd`, streaming its stderr (split on `\r` and `\n`) to `on_line`.
/// The child is parked in `sh.child` so the UI can kill it on cancel.
fn run(sh: &Shared, mut cmd: Command, mut on_line: impl FnMut(&str)) -> Result<()> {
    sh.bail_if_cancelled()?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).env("GIT_TERMINAL_PROMPT", "0");
    let desc = format!("{cmd:?}");
    let mut child = cmd.spawn().with_context(|| format!("running {desc}"))?;
    let mut err = child.stderr.take().expect("piped stderr");
    *sh.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    if sh.cancelled() {
        sh.request_cancel();
    }

    let mut tail: Vec<String> = Vec::new();
    let mut pending = Vec::<u8>::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match err.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);
        while let Some(i) = pending.iter().position(|&b| b == b'\n' || b == b'\r') {
            let is_newline = pending[i] == b'\n';
            let line = String::from_utf8_lossy(&pending[..i]).into_owned();
            pending.drain(..=i);
            on_line(&line);
            if is_newline && !line.trim().is_empty() {
                tail.push(line);
                if tail.len() > 60 {
                    tail.remove(0);
                }
            }
        }
    }
    if !pending.is_empty() {
        let line = String::from_utf8_lossy(&pending).into_owned();
        on_line(&line);
        tail.push(line);
    }

    let status = sh.child.lock().unwrap_or_else(|e| e.into_inner()).take().map(|mut c| c.wait());
    sh.bail_if_cancelled()?;
    let status = status.ok_or_else(|| anyhow!("lost track of {desc}"))??;
    if status.success() {
        return Ok(());
    }
    log_failure(&desc, &tail);
    let reason = tail
        .iter()
        .find(|l| l.trim_start().starts_with("error") || l.trim_start().starts_with("fatal"))
        .or(tail.last())
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| format!("exited with {status}"));
    bail!("{reason}")
}

fn log_failure(desc: &str, tail: &[String]) {
    let path = config::home_dir().join("logs").join("update.log");
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {desc} failed:", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        for l in tail {
            let _ = writeln!(f, "    {l}");
        }
    }
}

fn old_binary(dest: &Path, n: u32) -> PathBuf {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let file = if n == 0 { format!("{name}.old") } else { format!("{name}.{n}.old") };
    dest.with_file_name(file)
}

/// Swap `new` in over `dest` (the running executable).
fn install(new: &Path, dest: &Path) -> Result<()> {
    if !new.is_file() {
        bail!("build produced no binary at {}", new.display());
    }
    let dir = dest.parent().ok_or_else(|| anyhow!("odd install path {}", dest.display()))?;
    let staged = dir.join(format!(".{}.update", dest.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::copy(new, &staged).with_context(|| format!("can't write to {}", dir.display()))?;

    #[cfg(windows)]
    {
        // A running .exe can't be overwritten, but it can be renamed away.
        // Another blush may still be running an older `.old`; pick a free name.
        let old = (0..32)
            .map(|n| old_binary(dest, n))
            .find(|p| !p.exists() || std::fs::remove_file(p).is_ok())
            .ok_or_else(|| anyhow!("too many old blush binaries in {}", dir.display()))?;
        if let Err(e) = std::fs::rename(dest, &old) {
            let _ = std::fs::remove_file(&staged);
            return Err(e).with_context(|| format!("moving {} aside", dest.display()));
        }
        if let Err(e) = std::fs::rename(&staged, dest) {
            let _ = std::fs::rename(&old, dest);
            let _ = std::fs::remove_file(&staged);
            return Err(e).with_context(|| format!("installing {}", dest.display()));
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        if let Err(e) = std::fs::rename(&staged, dest) {
            let _ = std::fs::remove_file(&staged);
            return Err(e).with_context(|| format!("installing {}", dest.display()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Same as `make install`: keep Gatekeeper from killing the new binary.
        let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(dest).output();
        let _ = Command::new("codesign").args(["--force", "--sign", "-"]).arg(dest).output();
    }
    Ok(())
}

/// Delete binaries left behind by earlier Windows updates (best effort; one
/// still in use by another blush just stays until next time).
fn cleanup_old_binaries() {
    let Ok(exe) = std::env::current_exe() else { return };
    for n in 0..32 {
        let p = old_binary(&exe, n);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Run the freshly installed binary with our arguments in place of this one.
fn relaunch(exe: &Path) -> Option<i32> {
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1)).env(RELAUNCHED_ENV, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec(); // only returns on failure
        eprintln!("blush: couldn't relaunch the updated binary: {e}");
        None
    }
    #[cfg(not(unix))]
    {
        // No exec on Windows: run the child and pass its exit code through.
        // The child handles Ctrl+C itself; make sure it doesn't take us down
        // while it's still running.
        ignore_ctrl_c();
        match cmd.status() {
            Ok(s) => Some(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("blush: couldn't relaunch the updated binary: {e}");
                None
            }
        }
    }
}

#[cfg(windows)]
fn ignore_ctrl_c() {
    // A handler (unlike SetConsoleCtrlHandler(NULL, TRUE)) isn't inherited,
    // so the child still gets normal Ctrl+C behaviour.
    unsafe extern "system" fn swallow(_ctrl: u32) -> i32 {
        1
    }
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    unsafe {
        SetConsoleCtrlHandler(Some(swallow), 1);
    }
}

// ───────────────────────────────────────────────────────────── the show

type Rgb = (u8, u8, u8);
const BG: Rgb = (0x1b, 0x1b, 0x1f);
const PINK: Rgb = (0xf2, 0xb8, 0xcf);
const ROSE: Rgb = (0xc4, 0x6a, 0x92);
const CREAM: Rgb = (0xff, 0xee, 0xf5);
const TEXT: Rgb = (0xe6, 0xe1, 0xe8);
const DIM: Rgb = (0x7a, 0x74, 0x80);
const FAINT: Rgb = (0x3c, 0x3a, 0x44);
const GOOD: Rgb = (0xa8, 0xdc, 0xb5);
const BAD: Rgb = (0xf2, 0x8b, 0x8b);

const LOGO: [&str; 3] = [
    "█▀▀▄ █    █  █ ▄▀▀▀ █  █",
    "█▀▀▄ █    █  █  ▀▀▄ █▀▀█",
    "▀▀▀  ▀▀▀▀  ▀▀  ▀▀▀  ▀  ▀",
];
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    fg: Rgb,
    bold: bool,
}

struct Canvas {
    w: i32,
    h: i32,
    cells: Vec<Cell>,
}

impl Canvas {
    fn new(w: u16, h: u16) -> Canvas {
        let (w, h) = (w as i32, h as i32);
        Canvas { w, h, cells: vec![Cell { ch: ' ', fg: TEXT, bold: false }; (w * h).max(0) as usize] }
    }
    fn set(&mut self, x: i32, y: i32, ch: char, fg: Rgb, bold: bool) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.cells[(y * self.w + x) as usize] = Cell { ch, fg, bold };
        }
    }
    /// Write `s` at (x, y); returns the column after it.
    fn text(&mut self, x: i32, y: i32, s: &str, fg: Rgb, bold: bool) -> i32 {
        let mut x = x;
        for ch in s.chars() {
            self.set(x, y, ch, fg, bold);
            x += 1;
        }
        x
    }
    fn flush(&self, out: &mut impl Write) -> io::Result<()> {
        let mut buf = Vec::with_capacity((self.w * self.h * 4) as usize);
        queue!(buf, BeginSynchronizedUpdate, SetBackgroundColor(rgb(BG)))?;
        let mut fg = None;
        let mut bold = false;
        for y in 0..self.h {
            queue!(buf, cursor::MoveTo(0, y as u16))?;
            for c in &self.cells[(y * self.w) as usize..((y + 1) * self.w) as usize] {
                if fg != Some(c.fg) {
                    queue!(buf, SetForegroundColor(rgb(c.fg)))?;
                    fg = Some(c.fg);
                }
                if bold != c.bold {
                    queue!(buf, SetAttribute(if c.bold { Attribute::Bold } else { Attribute::NormalIntensity }))?;
                    bold = c.bold;
                }
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.ch.encode_utf8(&mut tmp).as_bytes());
            }
        }
        queue!(buf, EndSynchronizedUpdate)?;
        out.write_all(&buf)?;
        out.flush()
    }
}

fn rgb((r, g, b): Rgb) -> Color {
    Color::Rgb { r, g, b }
}

/// Drifting blossom petals in the background.
struct Petals {
    seed: u64,
    items: Vec<Petal>,
}

struct Petal {
    x: f32,
    y: f32,
    vy: f32,
    sway: f32,
    phase: f32,
    glyph: char,
    color: Rgb,
}

impl Petals {
    fn new(w: i32, h: i32) -> Petals {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64) | 1;
        let mut p = Petals { seed, items: Vec::new() };
        let n = ((w * h) / 90).clamp(10, 40);
        for _ in 0..n {
            let mut petal = p.spawn(w);
            petal.y = p.rand() * h as f32;
            p.items.push(petal);
        }
        p
    }
    fn rand(&mut self) -> f32 {
        // xorshift64
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 40) as f32 / (1u64 << 24) as f32
    }
    fn spawn(&mut self, w: i32) -> Petal {
        let glyphs = ['❀', '✿', '·', '·', '•', '˚', '✽'];
        let shades = [(0x5c, 0x3f, 0x4d), (0x75, 0x4f, 0x62), (0x8f, 0x5a, 0x74), (0x4a, 0x38, 0x42)];
        Petal {
            x: self.rand() * w as f32,
            y: -1.0 - self.rand() * 4.0,
            vy: 2.0 + self.rand() * 4.0,
            sway: 0.6 + self.rand() * 1.4,
            phase: self.rand() * std::f32::consts::TAU,
            glyph: glyphs[(self.rand() * glyphs.len() as f32) as usize % glyphs.len()],
            color: shades[(self.rand() * shades.len() as f32) as usize % shades.len()],
        }
    }
    fn step(&mut self, dt: f32, w: i32, h: i32, speed: f32) {
        for i in 0..self.items.len() {
            self.items[i].y += self.items[i].vy * dt * speed;
            if self.items[i].y > h as f32 + 1.0 || self.items[i].x > w as f32 + 3.0 {
                self.items[i] = self.spawn(w);
            }
        }
    }
    fn draw(&self, c: &mut Canvas, t: f32, glow: f32) {
        for p in &self.items {
            let x = p.x + (t * p.sway + p.phase).sin() * 2.0;
            c.set(x.round() as i32, p.y.round() as i32, p.glyph, lerp(p.color, PINK, glow), false);
        }
    }
}

/// Raw mode + alternate screen for the duration of the animation.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        let guard = Screen;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide, terminal::SetTitle("blush · updating"))?;
        Ok(guard)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            crossterm::style::ResetColor,
            terminal::LeaveAlternateScreen,
            cursor::Show,
            terminal::SetTitle("")
        );
        let _ = terminal::disable_raw_mode();
    }
}

enum Outcome {
    Updated(PathBuf),
    Skipped,
    Failed,
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{:.1}s", d.as_secs_f32()) }
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

struct View<'a> {
    rel: &'a Release,
    repo: &'a str,
    st: &'a State,
    /// Seconds since the animation began.
    t: f32,
    /// Eased overall progress, 0..=1.
    shown: f32,
    success: bool,
    failed: bool,
    cancelled: bool,
    cancelling: bool,
    /// Seconds since a successful finish.
    burst: Option<f32>,
    err: Option<&'a str>,
}

impl View<'_> {
    fn result_pending(&self) -> bool {
        !self.success && !self.failed && !self.cancelled
    }
}

/// Draw everything but the petals.
fn paint(c: &mut Canvas, v: &View) {
    let (w, h) = (c.w, c.h);
    let (st, rel, t, shown, burst) = (v.st, v.rel, v.t, v.shown, v.burst);
    let (success, failed, cancelled) = (v.success, v.failed, v.cancelled);

    const BLOCK_W: i32 = 52;
    const BLOCK_H: i32 = 17;
    let left = ((w - BLOCK_W) / 2).max(1);
    let top = ((h - BLOCK_H) / 2).max(0);
    let col = |x: i32| left + x;

    // Logo with a shimmer sweeping across it.
    let logo_x = col((BLOCK_W - 24) / 2);
    let sweep = (t * 16.0) % 64.0 - 16.0;
    for (row, line) in LOGO.iter().enumerate() {
        for (i, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let base = lerp(ROSE, PINK, i as f32 / 20.0);
            let d = (i as f32 - sweep - row as f32 * 1.5).abs();
            let mut fg = if d < 4.0 { lerp(base, CREAM, 1.0 - d / 4.0) } else { base };
            if let Some(b) = burst {
                fg = lerp(fg, CREAM, (1.0 - b * 1.5).clamp(0.0, 0.8));
            }
            c.set(logo_x + i as i32, top + row as i32, ch, fg, true);
        }
    }

    // Subtitle: from → to.
    let target_short = short(&st.target).to_string();
    let mut sub = vec![
        ("updating  ".to_string(), DIM),
        (short(BUILD_SHA).to_string(), TEXT),
        ("  →  ".into(), PINK),
        (target_short.clone(), TEXT),
    ];
    if BUILD_SHA.is_empty() {
        sub.drain(1..3);
    }
    if rel.ahead_by > 0 {
        let s = if rel.ahead_by == 1 { "" } else { "s" };
        sub.push((format!("  ·  {} new commit{s}", rel.ahead_by), DIM));
    }
    let sub_w: usize = sub.iter().map(|(s, _)| s.chars().count()).sum();
    let mut x = col((BLOCK_W - sub_w as i32) / 2);
    for (s, fg) in &sub {
        x = c.text(x, top + 4, s, *fg, false);
    }

    // Stages.
    let labels = ["check github", "fetch source", "compile", "install", "relaunch"];
    for (i, label) in labels.iter().enumerate() {
        let y = top + 6 + i as i32;
        let running = i == st.stage && v.result_pending();
        let (icon, icon_fg, label_fg) = if i < st.stage || v.success {
            ('✓', GOOD, TEXT)
        } else if i == st.stage && failed {
            ('✗', BAD, BAD)
        } else if i == st.stage && cancelled {
            ('–', DIM, DIM)
        } else if running {
            (SPINNER[(t * 12.0) as usize % SPINNER.len()], PINK, CREAM)
        } else {
            ('·', FAINT, DIM)
        };
        c.set(col(4), y, icon, icon_fg, true);
        c.text(col(7), y, label, label_fg, running);
        let detail = match i {
            CHECK => match rel.ahead_by {
                0 => "latest".to_string(),
                1 => "1 new commit".to_string(),
                n => format!("{n} new commits"),
            },
            FETCH if running => format!("github.com/{}", v.repo),
            FETCH if i < st.stage => target_short.clone(),
            BUILD if running => match st.built {
                Some((n, m)) => format!("{n}/{m}  {}", st.krate),
                None if st.krate.is_empty() => "warming up cargo".into(),
                None => st.krate.clone(),
            },
            INSTALL if running => "swapping binaries".into(),
            _ => st.took[i].filter(|_| i < st.stage && i != CHECK).map(fmt_dur).unwrap_or_default(),
        };
        c.text(col(23), y, &ellipsize(&detail, (BLOCK_W - 23) as usize), DIM, false);
    }

    // Progress bar with a travelling glint.
    let bar_w = BLOCK_W - 12;
    let y = top + 12;
    let filled_f = shown * bar_w as f32;
    let filled = filled_f.floor() as i32;
    let glint = (t * 28.0) % (bar_w as f32 + 14.0) - 7.0;
    for i in 0..bar_w {
        let x = col(4 + i);
        if i < filled {
            let base = lerp(ROSE, PINK, i as f32 / bar_w as f32 * 1.3);
            let d = (i as f32 - glint).abs();
            let fg = if d < 3.0 { lerp(base, CREAM, 1.0 - d / 3.0) } else { base };
            c.set(x, y, '━', if failed { lerp(fg, BAD, 0.6) } else { fg }, false);
        } else if i == filled && filled_f.fract() > 0.5 {
            c.set(x, y, '╸', PINK, false);
        } else {
            c.set(x, y, '─', FAINT, false);
        }
    }
    let pct = format!("{:>3}%", (shown * 100.0).round() as i32);
    c.text(col(4 + bar_w + 2), y, &pct, if success { GOOD } else { TEXT }, true);

    // Newest commit subject, or the error.
    let msg_y = top + 14;
    if let Some(e) = v.err {
        let e = ellipsize(e.lines().next().unwrap_or_default(), BLOCK_W as usize - 4);
        c.text(col(4), msg_y, &e, BAD, false);
        c.text(col(4), msg_y + 1, "continuing with the current version", DIM, false);
    } else if success {
        let done = "✿  updated — relaunching";
        c.text(col((BLOCK_W - done.chars().count() as i32) / 2), msg_y, done, PINK, true);
    } else if !rel.message.is_empty() {
        let m = format!("“{}”", ellipsize(&rel.message, BLOCK_W as usize - 6));
        c.text(col((BLOCK_W - m.chars().count() as i32) / 2), msg_y, &m, DIM, false);
    }

    let hint = if failed {
        "press any key"
    } else if !v.result_pending() {
        ""
    } else if v.cancelling {
        "skipping…"
    } else if st.stage >= INSTALL {
        "almost there"
    } else {
        "esc  skip this update"
    };
    c.text((w - hint.chars().count() as i32) / 2, h - 2, hint, FAINT, false);
}

fn animate(cfg: &UpdateConfig, rel: &Release) -> Outcome {
    let sh = Shared::new(rel);
    let mut worker = Some({
        let sh = sh.clone();
        let cfg = cfg.clone();
        thread::spawn(move || work(&cfg, &sh))
    });

    let screen = match Screen::enter() {
        Ok(s) => s,
        Err(_) => {
            // No fancy terminal; just do it quietly.
            sh.request_cancel();
            let _ = worker.take().map(|w| w.join());
            return Outcome::Skipped;
        }
    };

    let begun = Instant::now();
    let mut last = Instant::now();
    let (w0, h0) = terminal::size().unwrap_or((80, 24));
    let mut petals = Petals::new(w0 as i32, h0 as i32);
    let mut shown = 0.0f32;
    let mut result: Option<Result<PathBuf, String>> = None;
    let mut cancelled = false;
    let mut finished_at: Option<Instant> = None;
    let mut out = io::stdout();

    loop {
        if worker.as_ref().is_some_and(|w| w.is_finished()) {
            let r = worker.take().unwrap().join();
            result = Some(match r {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(e)) if e.is::<Cancelled>() => {
                    cancelled = true;
                    Err("skipped".into())
                }
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err("the updater crashed".into()),
            });
            finished_at = Some(Instant::now());
        }
        if let Some(t) = finished_at {
            let linger = match &result {
                Some(Ok(_)) => 1100,
                _ if cancelled => 500,
                _ => 6000,
            };
            if t.elapsed() > Duration::from_millis(linger) {
                break;
            }
        }

        let now = Instant::now();
        let dt = (now - last).as_secs_f32().min(0.1);
        last = now;
        let t = begun.elapsed().as_secs_f32();
        let (w, h) = terminal::size().unwrap_or((80, 24));
        let mut c = Canvas::new(w, h);
        let (w, h) = (w as i32, h as i32);

        let st = sh.state();
        let success = matches!(result, Some(Ok(_)));
        let failed = matches!(result, Some(Err(_))) && !cancelled;

        // Overall progress, eased.
        let stage_t = st.started.elapsed().as_secs_f32();
        let target = if success {
            1.0
        } else {
            match st.stage {
                CHECK => 0.02,
                FETCH => 0.02 + 0.08 * (1.0 - (-stage_t / 2.5).exp()),
                BUILD => match st.built {
                    Some((n, m)) if m > 0 => 0.10 + 0.84 * (n as f32 / m as f32),
                    _ => 0.10 + 0.05 * (1.0 - (-stage_t / 4.0).exp()),
                },
                INSTALL => 0.97,
                _ => 1.0,
            }
        };
        if target > shown || success {
            shown += (target - shown) * (1.0 - (-dt * 6.0).exp());
        }

        let burst = finished_at.filter(|_| success).map(|t| t.elapsed().as_secs_f32());
        petals.step(dt, w, h, if burst.is_some() { 3.5 } else { 1.0 });
        petals.draw(&mut c, t, burst.map_or(0.0, |b| (1.0 - b).clamp(0.0, 0.6)));
        let err = match &result {
            Some(Err(e)) if failed => Some(e.as_str()),
            _ => None,
        };
        paint(
            &mut c,
            &View { rel, repo: &cfg.repo, st: &st, t, shown, success, failed, cancelled, cancelling: sh.cancelled(), burst, err },
        );
        drop(st);

        if c.flush(&mut out).is_err() {
            break;
        }

        if event::poll(Duration::from_millis(33)).unwrap_or(false)
            && let Ok(Event::Key(k)) = event::read()
            && k.kind == KeyEventKind::Press
        {
            let quit_key = matches!(k.code, KeyCode::Esc | KeyCode::Char('q'))
                || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL));
            if failed {
                break;
            } else if result.is_none() && quit_key && sh.state().stage < INSTALL {
                sh.request_cancel();
            }
        }
    }

    // Wait out the worker in case we left the loop early.
    if let Some(w) = worker.take() {
        sh.request_cancel();
        let _ = w.join();
    }
    drop(screen);

    let target = short(&sh.state().target).to_string();
    match result {
        Some(Ok(exe)) => {
            let from = if BUILD_SHA.is_empty() { String::new() } else { format!("{} → ", short(BUILD_SHA)) };
            println!("\x1b[38;2;242;184;207m✿\x1b[0m blush updated {from}{target}");
            Outcome::Updated(exe)
        }
        _ if cancelled => Outcome::Skipped,
        Some(Err(e)) => {
            eprintln!("blush: update failed: {e}");
            eprintln!("       details: {}", config::home_dir().join("logs").join("update.log").display());
            Outcome::Failed
        }
        None => Outcome::Skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Shared> {
        Shared::new(&Release { sha: "abc".into(), ahead_by: 1, message: String::new() })
    }

    #[test]
    fn parses_cargo_progress() {
        let sh = shared();
        parse_cargo("    Building [=======>          ] 57/248: syntect(lib), regex-syntax", &sh);
        assert_eq!(sh.state().built, Some((57, 248)));
        assert_eq!(sh.state().krate, "syntect");
        parse_cargo("   Compiling ratatui v0.29.0", &sh);
        assert_eq!(sh.state().krate, "ratatui");
    }

    fn render(c: &Canvas) -> String {
        c.cells.chunks(c.w as usize).map(|r| r.iter().map(|c| c.ch).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("
")
    }

    #[test]
    fn paints_a_frame() {
        let rel = Release { sha: "e4f5a6b7".into(), ahead_by: 3, message: "feat: pink!".into() };
        let sh = Shared::new(&rel);
        sh.advance(FETCH);
        sh.advance(BUILD);
        parse_cargo("    Building [===>   ] 142/248: syntect(lib)", &sh);
        for (success, shown) in [(false, 0.57), (true, 1.0)] {
            let mut c = Canvas::new(70, 22);
            let st = sh.state();
            let v = View {
                rel: &rel, repo: "thePanda-X/blush", st: &st, t: 1.3, shown, success, failed: false,
                cancelled: false, cancelling: false, burst: success.then_some(0.2), err: None,
            };
            paint(&mut c, &v);
            let out = render(&c);
            println!("{out}
{}", "=".repeat(70));
            assert_eq!(out.contains("142/248  syntect"), !success);
            assert!(out.contains("█▀▀▄ █    █  █ ▄▀▀▀ █  █"));
        }
    }

    #[test]
    fn logo_rows_line_up() {
        assert!(LOGO.iter().all(|r| r.chars().count() == 24));
    }
}

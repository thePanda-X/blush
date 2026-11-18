//! Locates (and, on first run, installs) the Claude Code ACP adapter
//! `@agentclientprotocol/claude-agent-acp`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};

use crate::acp::LaunchSpec;
use crate::config::{Config, home_dir};

pub const ADAPTER_PKG: &str = "@agentclientprotocol/claude-agent-acp";

fn adapter_dir() -> PathBuf {
    home_dir().join("adapter")
}

fn adapter_entry() -> PathBuf {
    adapter_dir()
        .join("node_modules")
        .join("@agentclientprotocol")
        .join("claude-agent-acp")
        .join("dist")
        .join("index.js")
}

pub fn installed_adapter_version() -> Option<String> {
    let pkg = adapter_entry().parent()?.parent()?.join("package.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(pkg).ok()?).ok()?;
    v.get("version").and_then(|v| v.as_str()).map(str::to_owned)
}

pub fn needs_install(config: &Config) -> bool {
    config.agent.command.trim().is_empty()
        && !adapter_entry().exists()
        && which::which("claude-agent-acp").is_err()
}

/// Run `npm install` for the adapter into `~/.blush/adapter`.
pub async fn install_adapter(mut progress: impl FnMut(String)) -> Result<()> {
    let npm = which::which("npm").map_err(|_| {
        anyhow!("`npm` was not found on PATH. Install Node.js 20+ from https://nodejs.org and retry.")
    })?;
    let dir = adapter_dir();
    std::fs::create_dir_all(&dir)?;
    let pkg_json = dir.join("package.json");
    if !pkg_json.exists() {
        std::fs::write(&pkg_json, "{\n  \"name\": \"blush-adapter\",\n  \"private\": true\n}\n")?;
    }
    progress(format!("Installing {ADAPTER_PKG} (one-time)…"));
    let mut cmd = tokio::process::Command::new(&npm);
    cmd.args([
        "install",
        "--no-audit",
        "--no-fund",
        "--loglevel=error",
        &format!("{ADAPTER_PKG}@latest"),
    ])
    .current_dir(&dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output().await.context("running npm install")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("npm install failed:\n{}", err.trim());
    }
    progress(format!(
        "Installed {ADAPTER_PKG} {}",
        installed_adapter_version().unwrap_or_default()
    ));
    Ok(())
}

pub fn launch_spec(config: &Config, cwd: &Path) -> Result<LaunchSpec> {
    let mut env: Vec<(String, String)> =
        config.agent.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    // Stream (summarized) thinking so the scrollback can show "◆ Thought for …" blocks.
    if !env.iter().any(|(k, _)| k == "CLAUDE_CODE_THINKING_DISPLAY")
        && std::env::var_os("CLAUDE_CODE_THINKING_DISPLAY").is_none()
    {
        env.push(("CLAUDE_CODE_THINKING_DISPLAY".into(), "summarized".into()));
    }

    if config.agent.use_system_claude {
        if let Ok(claude) = which::which("claude") {
            env.push(("CLAUDE_CODE_EXECUTABLE".into(), claude.to_string_lossy().into_owned()));
        }
    }

    // 1. Explicit override: any ACP agent.
    if !config.agent.command.trim().is_empty() {
        let program = which::which(config.agent.command.trim())
            .unwrap_or_else(|_| PathBuf::from(config.agent.command.trim()));
        let display = std::iter::once(config.agent.command.clone())
            .chain(config.agent.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        return Ok(LaunchSpec { program, args: config.agent.args.clone(), env, cwd: cwd.to_path_buf(), display });
    }

    // 2. Our managed install, run directly with node (no .cmd shims).
    let entry = adapter_entry();
    if entry.exists() {
        let node = which::which("node").map_err(|_| {
            anyhow!("`node` was not found on PATH. Install Node.js 20+ from https://nodejs.org")
        })?;
        return Ok(LaunchSpec {
            program: node,
            args: vec![entry.to_string_lossy().into_owned()],
            env,
            cwd: cwd.to_path_buf(),
            display: format!("node {}", entry.display()),
        });
    }

    // 3. A global install on PATH.
    if let Ok(bin) = which::which("claude-agent-acp") {
        return Ok(LaunchSpec {
            program: bin,
            args: vec![],
            env,
            cwd: cwd.to_path_buf(),
            display: "claude-agent-acp".into(),
        });
    }

    bail!("Claude Code ACP adapter is not installed")
}

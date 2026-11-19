//! `~/.blush/config.toml`

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub agent: AgentConfig,
    pub features: FeaturesConfig,
    pub update: UpdateConfig,
    /// Set when the file existed but couldn't be parsed (shown in the TUI,
    /// where a plain stderr line would vanish behind the alternate screen).
    #[serde(skip)]
    pub load_warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FeaturesConfig {
    /// Read a short summary of each finished turn aloud (/narrator toggles).
    pub narrator: bool,
    /// Show 5-hour / weekly subscription usage in the footer.
    pub sub_meter: bool,
    /// Show per-turn and session token counts above the prompt.
    pub token_meter: bool,
    /// Snapshot the git working tree after each turn (/timeline, /undo, /redo).
    pub timeline: bool,
    /// Ring the bell when Claude finishes or needs you, plus a desktop
    /// notification while the terminal is in the background.
    pub notify: bool,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        FeaturesConfig { narrator: false, sub_meter: true, token_meter: true, timeline: true, notify: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// On launch, check GitHub for a newer blush and rebuild / swap in place.
    /// (`--no-update` or BLUSH_NO_UPDATE=1 skips it for one run.)
    pub auto: bool,
    /// GitHub repository to update from, as "owner/name".
    pub repo: String,
    /// Branch to track.
    pub branch: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig { auto: true, repo: "thePanda-X/blush".into(), branch: "main".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// blush | night | rosepine | day | terminal
    pub theme: String,
    /// Optional accent override, e.g. "#f2b8cf".
    pub accent: Option<String>,
    /// Optional background override, e.g. "#1b1b1f".
    pub background: Option<String>,
    pub show_timestamps: bool,
    /// Capture the mouse (wheel scrolling, click to select / fold).
    /// Hold Shift while dragging to use your terminal's native selection.
    pub mouse: bool,
    /// Show thinking blocks in the scrollback.
    pub show_thinking: bool,
    /// Expand thinking blocks by default.
    pub expand_thinking: bool,
    pub animations: bool,
    /// Compact layout: runs of tool calls and thinking collapse into one
    /// summary line between responses (Ctrl+G or /compact-mode toggles).
    pub compact: bool,
    /// Timestamps on prompts only, not on replies (`/timestamps minimal`).
    pub timestamps_minimal: bool,
    /// New tool calls start expanded (`/density detailed`).
    pub expand_tools: bool,
    /// Keep edits out of compact groups so their diffs stay visible (`/density diff`).
    pub diff_focus: bool,
    /// Pin Claude's todo list above the prompt while it has open items (Alt+P).
    pub todo_pane: bool,
    /// Show running tools, background commands and queued prompts above the prompt (Ctrl+B).
    pub tasks_pane: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Permission mode a new session starts in. Claude Code modes:
    /// bypassPermissions | default | acceptEdits | plan | auto | dontAsk
    pub default_mode: String,
    /// Model to select at session start (alias like "opus"/"sonnet" or a full id). Empty = Claude Code's default.
    pub model: String,
    /// Reasoning effort to select at session start (low | medium | high | xhigh | max). Empty = default.
    pub effort: String,
    /// Override the ACP agent command entirely (any ACP agent works).
    pub command: String,
    pub args: Vec<String>,
    /// Make the adapter drive the `claude` binary found on PATH instead of
    /// the one bundled with the Claude Agent SDK.
    pub use_system_claude: bool,
    /// Extra environment variables for the agent process.
    pub env: BTreeMap<String, String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: "blush".into(),
            accent: None,
            background: None,
            show_timestamps: true,
            mouse: true,
            show_thinking: true,
            expand_thinking: false,
            animations: true,
            compact: false,
            timestamps_minimal: false,
            expand_tools: false,
            diff_focus: false,
            todo_pane: true,
            tasks_pane: false,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            default_mode: "bypassPermissions".into(),
            model: String::new(),
            effort: String::new(),
            command: String::new(),
            args: vec![],
            use_system_claude: false,
            env: BTreeMap::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config { ui: UiConfig::default(), agent: AgentConfig::default(), features: FeaturesConfig::default(), update: UpdateConfig::default(), load_warning: None }
    }
}

pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("BLUSH_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".blush")
}

pub fn config_path() -> PathBuf {
    home_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Config {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!(
                        "Ignoring {} (using defaults until it's fixed; blush won't overwrite it): {}",
                        path.display(),
                        e.message()
                    );
                    eprintln!("warning: {msg}");
                    Config { load_warning: Some(msg), ..Config::default() }
                }
            },
            Err(_) => {
                let c = Config::default();
                let _ = c.save();
                c
            }
        }
    }

    /// Apply `f` to the config *as stored on disk* and write it back.
    ///
    /// The in-memory config carries one-off CLI overrides (`--theme`, `--mode`,
    /// `--model`, `--no-mouse`), so saving it wholesale would make them sticky.
    /// If the file exists but doesn't parse, it's left alone rather than being
    /// clobbered with defaults.
    pub fn persist(f: impl FnOnce(&mut Config)) {
        let path = config_path();
        let mut disk = match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => c,
                Err(_) => return,
            },
            Err(_) => Config::default(),
        };
        f(&mut disk);
        let _ = disk.save();
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let body = toml::to_string_pretty(self)?;
        let header = "# blush configuration — see README for all options\n\n";
        std::fs::write(&path, format!("{header}{body}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn theme(&self) -> crate::theme::Theme {
        crate::theme::Theme::by_name(&self.ui.theme)
            .unwrap_or_else(crate::theme::Theme::blush)
            .with_overrides(self.ui.accent.as_deref(), self.ui.background.as_deref())
    }
}

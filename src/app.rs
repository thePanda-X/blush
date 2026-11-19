//! Application state, input handling and agent orchestration.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

mod extras;

pub use extras::{Btw, Search, ShellRun};

use crate::acp::{self, AcpClient, Incoming, RpcError};
use crate::attach::{Attached, Attachments};
use crate::config::{Config, home_dir};
use crate::files::{FileIndex, fuzzy_filter, mentioned_paths};
use crate::input::Input;
use crate::launcher;
use crate::model::{Block, Fold, Level, Session, Transcript, s};
use crate::narrator::Narrator;
use crate::theme::{THEME_NAMES, Theme};
use crate::timeline::Timeline;
use crate::usage::{LimitWindow, Tokens};
use crate::{narrator, peek};

pub enum AppMsg {
    Acp(Incoming),
    Status(String),
    Connected(Arc<AcpClient>, Value),
    StartFailed(String),
    SessionReady { result: Value, kind: SessionKind },
    SessionFailed(String),
    PromptDone(Result<Value, RpcError>),
    /// (requested mode, mode to restore on failure, result)
    ModeSet(String, String, Result<Value, RpcError>),
    /// (option id and value to restore on failure, result)
    ConfigSet(Option<(String, String)>, Result<Value, RpcError>),
    Sessions(Result<Value, RpcError>),
    /// Legacy `session/set_model` change made by a per-turn override.
    LegacyModel(String),
    /// The throwaway session a `/usage` fetch runs in was created.
    UsageProbe(String),
    /// A background `/usage` fetch for the subscription meter finished.
    UsageDone(Result<Value, RpcError>),
    /// A Time Machine snapshot or jump finished: (toast/notice, is_error).
    Timeline(Option<(String, bool)>),
    /// A `!` shell command finished: (its run id, result).
    ShellDone(u64, std::io::Result<crate::shell::ShellResult>),
    /// The fork a `/btw` question runs in was created: (run id, fork session id).
    BtwStarted(u64, String),
    /// A `/btw` question finished: (run id, result).
    BtwDone(u64, Result<Value, RpcError>),
    /// `session/set_title` replied.
    TitleSet(Result<Value, RpcError>),
}

/// A prompt on its way to the agent, with its per-turn overrides.
#[derive(Clone, Debug, Default)]
pub struct Outgoing {
    pub text: String,
    /// `_` prefix: run this turn on Haiku.
    pub haiku: bool,
    /// `+` prefix: run this turn at max effort.
    pub max_effort: bool,
    /// What the user typed (placeholders and prefixes included), so a queued
    /// prompt can be pulled back into the editor.
    pub raw: String,
    /// Notes sent ahead of the prompt (shell output from `!` commands).
    pub context: Vec<String>,
    /// Images attached with `[Image #n]` placeholders.
    pub images: Vec<Attached>,
}

impl Outgoing {
    pub fn plain(text: String) -> Outgoing {
        Outgoing { raw: text.clone(), text, ..Outgoing::default() }
    }

    /// Parse leading `_` (Haiku) and `+` (max effort) markers.
    pub fn parse(raw: &str) -> Outgoing {
        let mut out = Outgoing::plain(String::new());
        let mut rest = raw;
        loop {
            if let Some(r) = rest.strip_prefix('_').filter(|r| !r.starts_with('_')) {
                out.haiku = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix('+').filter(|r| !r.starts_with(|c: char| c.is_ascii_digit() || c == '+')) {
                out.max_effort = true;
                rest = r;
            } else {
                break;
            }
        }
        let rest = rest.trim_start();
        if rest.is_empty() {
            return Outgoing::plain(raw.to_string());
        }
        out.text = rest.to_string();
        out.raw = raw.to_string();
        out
    }

    pub fn tag(&self) -> Option<String> {
        match (self.haiku, self.max_effort) {
            (true, true) => Some("haiku · max effort".into()),
            (true, false) => Some("haiku".into()),
            (false, true) => Some("max effort".into()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum SessionKind {
    New,
    Load(String),
    Resume(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Conn {
    Starting(String),
    Ready,
    Failed(String),
    Exited,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Prompt,
    Scrollback,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PickerKind {
    Model,
    Effort,
    Mode,
    Theme,
    Resume,
    Option(String),
    Commit,
    Timeline,
    /// The command palette (Ctrl+X): every command, searchable.
    Palette,
}

#[derive(Clone, Debug)]
pub struct PickItem {
    pub label: String,
    pub detail: String,
    pub value: String,
    pub current: bool,
}

pub struct Picker {
    pub kind: PickerKind,
    pub title: String,
    pub items: Vec<PickItem>,
    pub filter: String,
    pub sel: usize,
    pub original_theme: Option<String>,
}

impl Picker {
    pub fn visible(&self) -> Vec<usize> {
        let labels: Vec<String> = self.items.iter().map(|i| format!("{} {}", i.label, i.value)).collect();
        fuzzy_filter(&self.filter, &labels)
    }
}

pub enum Overlay {
    None,
    Picker(Picker),
    Shortcuts,
    Viewer { title: String, text: String, scroll: usize },
    /// Settings (F2): toggles saved to config.toml.
    Settings { sel: usize },
}

pub struct PermissionCard {
    pub id: Value,
    pub title: String,
    pub detail: Vec<String>,
    /// (optionId, name, kind)
    pub options: Vec<(String, String, String)>,
    pub sel: usize,
    /// When the card reached the front of the queue. Keystrokes in the first
    /// moments are ignored so typing in progress can't approve or reject it.
    pub shown: Instant,
}

#[derive(Clone)]
pub enum FieldKind {
    Single(Vec<(String, String, String)>),
    Multi(Vec<(String, String, String)>, Vec<bool>),
    Text,
    Bool,
}

#[derive(Clone)]
pub struct Field {
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: FieldKind,
}

pub struct QuestionCard {
    pub id: Value,
    pub message: String,
    pub fields: Vec<Field>,
    pub idx: usize,
    pub sel: usize,
    pub text: String,
    pub answers: Map<String, Value>,
}

pub enum Completion {
    Slash { items: Vec<(String, String, bool)>, sel: usize },
    File { items: Vec<String>, sel: usize, start: usize },
}

#[derive(Default, Clone)]
pub struct LayoutInfo {
    pub scroll_area: Rect,
    pub prompt_area: Rect,
    pub prompt_inner_width: u16,
    /// (screen y start, screen y end exclusive, entry index, header rows, first rendered row index)
    pub entry_rows: Vec<(u16, u16, usize, usize, usize)>,
    pub stop_button: Option<Rect>,
    pub narrator_button: Option<Rect>,
    /// Compact mode: (screen row, anchor entry) of expanded groups' header lines.
    pub group_rows: Vec<(u16, usize)>,
    pub total_lines: usize,
    pub view_height: usize,
    pub card_area: Option<Rect>,
    /// Viewer overlay: the largest useful scroll offset and the page height,
    /// both in rendered (wrapped) rows.
    pub viewer_max: usize,
    pub viewer_page: usize,
    /// Shortcuts overlay: the largest useful scroll offset.
    pub shortcuts_max: usize,
    /// Settings overlay: (screen row, item index) of each row, for clicks.
    pub settings_rows: Vec<(u16, usize)>,
}

pub enum External {
    Login,
}

pub struct App {
    pub config: Config,
    pub theme: Theme,
    pub cwd: PathBuf,
    pub files: FileIndex,
    pub tx: mpsc::UnboundedSender<AppMsg>,

    pub client: Option<Arc<AcpClient>>,
    pub conn: Conn,
    pub agent_caps: Value,
    pub agent_name: String,
    pub stderr_tail: VecDeque<String>,

    pub session: Session,
    pub transcript: Transcript,

    pub input: Input,
    pub focus: Focus,
    pub completion: Option<Completion>,
    pub overlay: Overlay,
    pub permissions: VecDeque<PermissionCard>,
    pub question: Option<QuestionCard>,

    pub responding: bool,
    pub cancelling: bool,
    pub turn_started: Option<Instant>,
    pub turn_chars: usize,
    pub last_turn: Option<(Duration, usize)>,
    pub queue: VecDeque<(Outgoing, usize)>,
    /// Override tag of the running turn ("haiku", "max effort").
    pub turn_tag: Option<String>,
    /// Text of the prompt that started the running turn.
    pub turn_prompt: String,

    pub narrator: Narrator,
    /// Whether the narrator was speaking at the last poll.
    pub narrating: bool,
    pub timeline: Arc<std::sync::Mutex<Timeline>>,
    /// A snapshot or jump is running; prompts wait for it.
    pub timeline_busy: bool,
    /// A background `/usage` fetch is running.
    pub usage_inflight: bool,
    /// (throwaway session id, its reply so far) while a fetch runs.
    pub usage_probe: Option<(String, String)>,
    pub usage_fetched: Option<Instant>,
    pub usage_failures: u8,
    pub limits: Vec<LimitWindow>,
    pub turn_tokens: Option<Tokens>,
    pub session_tokens: Tokens,
    pub turns_done: usize,

    pub selected: Option<usize>,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    pub follow: bool,

    pub tick: u64,
    /// `repo:branch` shown in the header when cwd is inside a git repo.
    pub git_label: Option<String>,
    pub quit_armed: Option<Instant>,
    /// Scroll offset of the shortcuts overlay (it's taller than most terminals).
    pub shortcuts_scroll: usize,
    /// A stopped turn left prompts queued: hold them until the user says so.
    pub queue_paused: bool,
    pub esc_armed: Option<Instant>,
    pub toast: Option<(String, Instant)>,
    pub should_quit: bool,
    pub external: Option<External>,
    pub layout: LayoutInfo,
    pub startup: Option<SessionKind>,
    pub terminal_title: String,
    pub dirty: bool,
    pub ensure_selected_visible: bool,
    pub force_redraw: bool,
    pub continue_latest: bool,
    pub pick_session: bool,
    /// An adapter launch is in flight; a second one would orphan the first.
    starting: bool,
    pub pending_prompt: Option<String>,
    /// `session/update`s that arrived for a not-yet-known session while a
    /// `session/new` was in flight; replayed once its id is known.
    pub early_updates: Vec<(String, Value)>,

    /// Whether the terminal has focus; None until it first reports.
    pub focused: Option<bool>,
    /// Images and collapsed pastes in the prompt being edited.
    pub attachments: Attachments,
    /// A prompt set aside with Ctrl+S (and its attachments).
    pub stash: Option<(String, Vec<Attached>)>,
    /// Prompts Ctrl+Enter put at the front of the queue while stopping the
    /// turn; when non-zero, the stop doesn't hold the queue.
    pub send_now: usize,
    /// The `!` command running now.
    pub shell: Option<ShellRun>,
    /// Shell output waiting to go to Claude with the next prompt.
    pub pending_context: Vec<String>,
    /// `/btw` questions in flight.
    pub btw: Vec<Btw>,
    /// Counter for run ids (shell commands, side questions).
    pub run_seq: u64,
    /// Find in scrollback (Ctrl+F or / in the scrollback).
    pub search: Option<Search>,
    /// A title set with /rename (wins over the agent's automatic one).
    pub title_override: Option<String>,
    /// When the last keystroke burst was taken as a paste (its tail may
    /// arrive in the next batch).
    pub last_burst: Option<Instant>,
}

pub const LOCAL_COMMANDS: &[(&str, &str)] = &[
    ("new", "Start a new session"),
    ("clear", "Start a new session"),
    ("resume", "Resume a previous session"),
    ("model", "Choose the model"),
    ("effort", "Set reasoning effort"),
    ("mode", "Choose the permission mode"),
    ("always-approve", "Skip all permission prompts (bypass)"),
    ("ask", "Ask before edits and commands"),
    ("accept-edits", "Auto-accept file edits"),
    ("plan", "Plan mode: research first, then propose"),
    ("fast", "Toggle fast mode (if the model supports it)"),
    ("theme", "Change the color theme"),
    ("copy", "Copy the last response to the clipboard"),
    ("export", "Export the conversation to Markdown"),
    ("commit", "Commit changes as Conventional Commits (runs on Haiku)"),
    ("timeline", "Time Machine: browse and restore file snapshots"),
    ("undo", "Restore files to the previous snapshot"),
    ("redo", "Restore files to the next snapshot"),
    ("after", "Queue a task to run when Claude finishes"),
    ("btw", "Ask a side question without interrupting (answered in a fork)"),
    ("queue", "List queued prompts (↑ on an empty prompt edits the last one)"),
    ("tasks", "List running tools, background commands and queued prompts"),
    ("find", "Search the scrollback"),
    ("rename", "Rename this session"),
    ("stash", "Set the prompt aside, or bring it back"),
    ("density", "Tool-call detail: detailed · balanced · diff · grouped"),
    ("todos", "Show / hide the pinned todo pane"),
    ("tasks-pane", "Show / hide the tasks pane"),
    ("settings", "Open settings"),
    ("narrator", "Read a summary of each turn aloud"),
    ("compact-mode", "Group tool calls and thinking between responses"),
    ("context", "Show context window usage"),
    ("session-info", "Show session details"),
    ("thinking", "Show / hide thinking blocks"),
    ("timestamps", "Timestamps: on · minimal · off"),
    ("restart", "Restart the Claude Code agent"),
    ("login", "Sign in to Claude Code (claude auth login)"),
    ("config", "Open settings (and show the config file)"),
    ("shortcuts", "Keyboard shortcuts"),
    ("help", "Keyboard shortcuts and commands"),
    ("quit", "Exit"),
];

impl App {
    pub fn new(config: Config, cwd: PathBuf, tx: mpsc::UnboundedSender<AppMsg>, startup: SessionKind) -> App {
        let theme = config.theme();
        let mut transcript = Transcript::new();
        transcript.show_thinking = config.ui.show_thinking;
        transcript.expand_thinking = config.ui.expand_thinking;
        transcript.expand_tools = config.ui.expand_tools;
        transcript.diff_focus = config.ui.diff_focus;
        transcript.push(Block::Welcome);
        if let Some(w) = &config.load_warning {
            transcript.notice(Level::Warn, w.clone());
        }
        let mut input = Input::default();
        input.load_history(load_history());
        let git_label = crate::ui::git_label(&cwd);
        App {
            files: FileIndex::spawn(cwd.clone()),
            theme,
            cwd,
            tx,
            client: None,
            conn: Conn::Starting("Starting Claude Code…".into()),
            agent_caps: Value::Null,
            agent_name: "Claude Code".into(),
            stderr_tail: VecDeque::new(),
            session: Session::default(),
            transcript,
            input,
            focus: Focus::Prompt,
            completion: None,
            overlay: Overlay::None,
            permissions: VecDeque::new(),
            question: None,
            responding: false,
            cancelling: false,
            turn_started: None,
            turn_chars: 0,
            last_turn: None,
            queue: VecDeque::new(),
            turn_tag: None,
            turn_prompt: String::new(),
            narrator: Narrator::default(),
            narrating: false,
            timeline: Arc::new(std::sync::Mutex::new(Timeline::default())),
            timeline_busy: false,
            usage_inflight: false,
            usage_probe: None,
            usage_fetched: None,
            usage_failures: 0,
            limits: Vec::new(),
            turn_tokens: None,
            session_tokens: Tokens::default(),
            turns_done: 0,
            selected: None,
            scroll: 0,
            follow: true,
            tick: 0,
            git_label,
            quit_armed: None,
            shortcuts_scroll: 0,
            queue_paused: false,
            esc_armed: None,
            toast: None,
            should_quit: false,
            external: None,
            layout: LayoutInfo::default(),
            startup: Some(startup),
            terminal_title: "blush".to_string(),
            dirty: true,
            ensure_selected_visible: false,
            force_redraw: false,
            continue_latest: false,
            pick_session: false,
            starting: false,
            pending_prompt: None,
            early_updates: Vec::new(),
            focused: None,
            attachments: Attachments::default(),
            stash: None,
            send_now: 0,
            shell: None,
            pending_context: Vec::new(),
            btw: Vec::new(),
            run_seq: 0,
            search: None,
            title_override: None,
            last_burst: None,
            config,
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Track the narrator finishing on its own; true when the footer needs a redraw.
    pub fn poll_narrator(&mut self) -> bool {
        let now = self.narrator.speaking();
        let changed = now != self.narrating;
        self.narrating = now;
        changed
    }

    pub fn animating(&self) -> bool {
        self.responding
            || self.timeline_busy
            || self.shell.is_some()
            || !self.btw.is_empty()
            || matches!(self.conn, Conn::Starting(_))
            || self.transcript.entries.iter().rev().take(30).any(|e| e.is_live())
    }

    // ------------------------------------------------------------------
    // Agent lifecycle
    // ------------------------------------------------------------------

    pub fn start_agent(&mut self) {
        if self.starting {
            // Already launching: the pending `startup` kind is picked up on connect.
            return;
        }
        self.starting = true;
        self.conn = Conn::Starting("Starting Claude Code…".into());
        let tx = self.tx.clone();
        let config = self.config.clone();
        let cwd = self.cwd.clone();
        tokio::spawn(async move {
            if launcher::needs_install(&config) {
                let t2 = tx.clone();
                if let Err(e) = launcher::install_adapter(move |m| {
                    let _ = t2.send(AppMsg::Status(m));
                })
                .await
                {
                    let _ = tx.send(AppMsg::StartFailed(e.to_string()));
                    return;
                }
            }
            let spec = match launcher::launch_spec(&config, &cwd) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(AppMsg::StartFailed(e.to_string()));
                    return;
                }
            };
            // Agent events go straight onto the app channel, so they always
            // arrive before the `PromptDone` for the turn that produced them.
            let etx = tx.clone();
            let client = match AcpClient::spawn(&spec, move |m| {
                let _ = etx.send(AppMsg::Acp(m));
            })
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppMsg::StartFailed(e.to_string()));
                    return;
                }
            };
            let _ = tx.send(AppMsg::Status("Connecting…".into()));
            match tokio::time::timeout(Duration::from_secs(90), client.request("initialize", acp::initialize_params())).await {
                Ok(Ok(init)) => {
                    let _ = tx.send(AppMsg::Connected(client, init));
                }
                Ok(Err(e)) => {
                    let _ = tx.send(AppMsg::StartFailed(format!("initialize failed: {e}")));
                }
                Err(_) => {
                    let _ = tx.send(AppMsg::StartFailed("timed out waiting for the agent to initialize".into()));
                }
            }
        });
    }

    /// Send the startup prompt (if any) and prompts typed while connecting.
    async fn flush_pending(&mut self) {
        if let Some(p) = self.pending_prompt.take() {
            Box::pin(self.submit(p)).await;
        }
        self.pump_queue();
    }

    pub async fn restart_agent(&mut self) {
        if let Some(c) = self.client.take() {
            c.shutdown().await;
        }
        self.responding = false;
        self.permissions.clear();
        self.question = None;
        let id = self.session.id.clone();
        self.session = Session::default();
        // Re-applied from titles.json if the session is reopened.
        self.title_override = None;
        self.startup = Some(match id {
            Some(id) if self.can_load() || self.can_resume() => {
                if self.can_resume() { SessionKind::Resume(id) } else { SessionKind::Load(id) }
            }
            _ => SessionKind::New,
        });
        self.start_agent();
    }

    fn can_load(&self) -> bool {
        self.agent_caps.get("loadSession").and_then(|v| v.as_bool()).unwrap_or(false)
    }

    fn can_resume(&self) -> bool {
        self.agent_caps.get("sessionCapabilities").and_then(|c| c.get("resume")).is_some_and(|v| !v.is_null())
    }

    fn can_list(&self) -> bool {
        self.agent_caps.get("sessionCapabilities").and_then(|c| c.get("list")).is_some_and(|v| !v.is_null())
    }

    fn open_session(&mut self, kind: SessionKind) {
        let Some(client) = self.client.clone() else { return };
        let tx = self.tx.clone();
        let cwd = self.cwd.to_string_lossy().to_string();
        // Updates race the open reply (they arrive on a different task), so
        // switch to the target session now: a load's history replay must not be
        // filtered out as "some other session". For a new session the id isn't
        // known yet — unknown-session updates are buffered until it is.
        self.early_updates.clear();
        self.session.id = match &kind {
            SessionKind::New => None,
            SessionKind::Load(id) | SessionKind::Resume(id) => Some(id.clone()),
        };
        let (method, params) = match &kind {
            SessionKind::New => ("session/new", json!({"cwd": cwd, "mcpServers": []})),
            SessionKind::Load(id) => ("session/load", json!({"sessionId": id, "cwd": cwd, "mcpServers": []})),
            SessionKind::Resume(id) => ("session/resume", json!({"sessionId": id, "cwd": cwd, "mcpServers": []})),
        };
        self.conn = Conn::Starting(match &kind {
            SessionKind::New => "Starting session…".into(),
            _ => "Loading session…".into(),
        });
        tokio::spawn(async move {
            match client.request(method, params).await {
                Ok(result) => {
                    let _ = tx.send(AppMsg::SessionReady { result, kind });
                }
                Err(e) => {
                    let msg = if e.code == -32000 && e.message.to_lowercase().contains("auth") {
                        format!("{e}\nClaude Code isn't signed in. Run /login, then /restart.")
                    } else {
                        e.to_string()
                    };
                    let _ = tx.send(AppMsg::SessionFailed(msg));
                }
            }
        });
    }

    fn set_mode(&mut self, mode: &str) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session.id.clone()) else {
            self.toast("No active session");
            return;
        };
        if !self.session.modes.is_empty() && !self.session.modes.iter().any(|m| m.id == mode) {
            self.transcript.notice(
                Level::Warn,
                format!("Mode `{mode}` is not available in this session (bypass is disabled when running as root unless IS_SANDBOX=1)."),
            );
            return;
        }
        let tx = self.tx.clone();
        let mode = mode.to_string();
        let prev = std::mem::replace(&mut self.session.mode, mode.clone());
        tokio::spawn(async move {
            let r = client.request("session/set_mode", json!({"sessionId": sid, "modeId": mode})).await;
            let _ = tx.send(AppMsg::ModeSet(mode, prev, r));
        });
    }

    fn set_option(&mut self, id: &str, value: &str) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session.id.clone()) else { return };
        let tx = self.tx.clone();
        if id == "__model" {
            // legacy session/set_model
            let value = value.to_string();
            let prev = std::mem::replace(&mut self.session.model, value.clone());
            tokio::spawn(async move {
                let r = client.request("session/set_model", json!({"sessionId": sid, "modelId": value})).await;
                let _ = tx.send(AppMsg::ConfigSet(Some(("__model".into(), prev)), r.map(|_| Value::Null)));
            });
            return;
        }
        let mut undo = None;
        if let Some(o) = self.session.config.iter_mut().find(|o| o.id == id) {
            undo = Some((id.to_string(), std::mem::replace(&mut o.current, value.to_string())));
        }
        let params = json!({"sessionId": sid, "configId": id, "value": value});
        tokio::spawn(async move {
            let r = client.request("session/set_config_option", params).await;
            let _ = tx.send(AppMsg::ConfigSet(undo, r));
        });
    }

    fn cycle_mode(&mut self) {
        if self.session.modes.is_empty() {
            return;
        }
        let order = ["bypassPermissions", "default", "acceptEdits", "plan", "auto", "dontAsk"];
        let mut ids: Vec<String> = order
            .iter()
            .filter(|id| self.session.modes.iter().any(|m| m.id == **id))
            .map(|s| s.to_string())
            .collect();
        for m in &self.session.modes {
            if !ids.contains(&m.id) {
                ids.push(m.id.clone());
            }
        }
        let cur = ids.iter().position(|m| *m == self.session.mode).unwrap_or(ids.len() - 1);
        let next = ids[(cur + 1) % ids.len()].clone();
        self.set_mode(&next);
        let name = self.session.modes.iter().find(|m| m.id == next).map(|m| m.name.clone()).unwrap_or(next);
        self.toast(format!("Mode: {name}"));
    }

    // ------------------------------------------------------------------
    // Messages from background tasks / the agent
    // ------------------------------------------------------------------

    pub async fn on_msg(&mut self, msg: AppMsg) {
        self.dirty = true;
        match msg {
            AppMsg::Status(s) => {
                if s.starts_with("Installed") {
                    self.transcript.notice(Level::Success, s.clone());
                }
                self.conn = Conn::Starting(s);
            }
            AppMsg::StartFailed(e) => {
                self.starting = false;
                self.conn = Conn::Failed(e.clone());
                self.transcript.notice(Level::Error, format!("Couldn't start Claude Code: {e}"));
            }
            AppMsg::Connected(client, init) => {
                self.starting = false;
                self.agent_caps = init.get("agentCapabilities").cloned().unwrap_or(Value::Null);
                if let Some(info) = init.get("agentInfo") {
                    let t = s(info, "title");
                    let n = s(info, "name");
                    let v = s(info, "version");
                    let name = if !t.is_empty() { t } else { n };
                    if !name.is_empty() {
                        self.agent_name = if v.is_empty() { name } else { format!("{name} {v}") };
                    }
                }
                self.client = Some(client);
                let kind = self.startup.take().unwrap_or(SessionKind::New);
                let kind = match kind {
                    SessionKind::Load(id) if !self.can_load() && self.can_resume() => SessionKind::Resume(id),
                    SessionKind::Resume(id) if !self.can_resume() && self.can_load() => SessionKind::Load(id),
                    k => k,
                };
                if matches!(kind, SessionKind::Load(_)) {
                    self.reset_transcript();
                }
                self.open_session(kind);
            }
            AppMsg::SessionReady { result, kind } => {
                self.conn = Conn::Ready;
                let prior_cmds = std::mem::take(&mut self.session.commands);
                self.session.apply_new_session(&result);
                if let SessionKind::Load(id) | SessionKind::Resume(id) = &kind {
                    self.session.id = Some(id.clone());
                }
                for (sid, u) in std::mem::take(&mut self.early_updates) {
                    if Some(sid.as_str()) == self.session.id.as_deref() {
                        self.apply_session_update(&u);
                    }
                }
                if self.session.commands.is_empty() {
                    self.session.commands = prior_cmds;
                }
                match &kind {
                    SessionKind::New => {
                        self.apply_session_defaults();
                    }
                    SessionKind::Load(id) | SessionKind::Resume(id) => {
                        self.session.id = Some(id.clone());
                        self.transcript.end_turn(false);
                        self.transcript.notice(Level::Info, "Session resumed");
                    }
                }
                if let Some(t) = self.session.id.as_deref().and_then(extras::local_title) {
                    self.title_override = Some(t);
                }
                self.update_title();
                self.follow = true;
                if (self.continue_latest || self.pick_session) && matches!(kind, SessionKind::New) && self.can_list() {
                    let client = self.client.clone().unwrap();
                    let tx = self.tx.clone();
                    let cwd = self.cwd.to_string_lossy().to_string();
                    tokio::spawn(async move {
                        let r = client.request("session/list", json!({"cwd": cwd})).await;
                        let _ = tx.send(AppMsg::Sessions(r));
                    });
                    return;
                }
                self.continue_latest = false;
                self.pick_session = false;
                self.flush_pending().await;
                self.maybe_refresh_usage();
            }
            AppMsg::SessionFailed(e) => {
                self.early_updates.clear();
                self.conn = Conn::Failed(e.clone());
                self.transcript.notice(Level::Error, format!("Couldn't open session: {e}"));
            }
            AppMsg::PromptDone(res) => self.on_prompt_done(res),
            AppMsg::ModeSet(mode, prev, res) => {
                if let Err(e) = res {
                    if self.session.mode == mode {
                        self.session.mode = prev;
                    }
                    self.transcript.notice(Level::Error, format!("Couldn't switch to {mode}: {e}"));
                }
            }
            AppMsg::ConfigSet(undo, res) => match res {
                Ok(v) => {
                    if let Some(c) = v.get("configOptions") {
                        self.session.apply_config(c);
                    }
                }
                Err(e) => {
                    match undo {
                        Some((id, prev)) if id == "__model" => self.session.model = prev,
                        Some((id, prev)) => {
                            if let Some(o) = self.session.config.iter_mut().find(|o| o.id == id) {
                                o.current = prev;
                            }
                        }
                        None => {}
                    }
                    self.transcript.notice(Level::Error, format!("Couldn't change setting: {e}"));
                }
            },
            AppMsg::Sessions(res) => match res {
                Ok(v) if self.continue_latest => {
                    self.continue_latest = false;
                    self.pick_session = false;
                    let first = v
                        .get("sessions")
                        .and_then(|s| s.as_array())
                        .and_then(|a| a.iter().find(|x| Some(s(x, "sessionId")) != self.session.id))
                        .map(|x| s(x, "sessionId"));
                    match first {
                        Some(id) => self.resume(id),
                        None => {
                            self.toast("No previous session to continue");
                            self.flush_pending().await;
                        }
                    }
                }
                Ok(v) => {
                    self.pick_session = false;
                    self.open_resume_picker(&v)
                }
                Err(e) => {
                    self.continue_latest = false;
                    self.pick_session = false;
                    self.transcript.notice(Level::Error, format!("Couldn't list sessions: {e}"));
                    self.flush_pending().await;
                }
            },
            AppMsg::LegacyModel(m) => self.session.model = m,
            AppMsg::UsageProbe(sid) => self.usage_probe = Some((sid, String::new())),
            AppMsg::UsageDone(res) => {
                let buf = self.usage_probe.take().map(|(_, b)| b).unwrap_or_default();
                self.usage_inflight = false;
                let limits = crate::usage::parse_limits(&buf);
                if res.is_ok() && !limits.is_empty() {
                    self.limits = limits;
                    self.usage_failures = 0;
                } else {
                    self.usage_failures = self.usage_failures.saturating_add(1);
                }
                self.usage_fetched = Some(Instant::now());
            }
            AppMsg::Timeline(msg) => {
                self.timeline_busy = false;
                match msg {
                    Some((m, true)) => self.transcript.notice(Level::Error, m),
                    Some((m, false)) => self.toast(m),
                    None => {}
                }
                self.after_turn_housekeeping();
            }
            AppMsg::ShellDone(id, r) => self.on_shell_done(id, r),
            AppMsg::BtwStarted(id, sid) => {
                if let Some(b) = self.btw.iter_mut().find(|b| b.id == id) {
                    b.sid = Some(sid);
                }
            }
            AppMsg::BtwDone(id, r) => self.on_btw_done(id, r),
            AppMsg::TitleSet(r) => {
                if let Err(e) = r {
                    self.toast(format!("Renamed here only (the agent didn't save it: {})", e.message));
                }
            }
            AppMsg::Acp(inc) => self.on_incoming(inc).await,
        }
    }

    /// Anything other than a user turn that must finish before the next prompt.
    pub fn busy(&self) -> bool {
        self.responding || self.timeline_busy
    }

    /// After a turn (and its snapshot): refresh the subscription meter if
    /// it's stale, then run the next queued prompt.
    fn after_turn_housekeeping(&mut self) {
        self.maybe_refresh_usage();
        self.pump_queue();
    }

    /// Refresh the subscription meter if it's on and older than 5 minutes
    /// (backing off to 30 minutes after repeated failures).
    pub fn maybe_refresh_usage(&mut self) {
        let every = if self.usage_failures >= 3 { 1800 } else { 300 };
        let stale = self.usage_fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(every));
        if self.config.features.sub_meter && stale && !self.usage_inflight && self.session_ready() {
            self.fetch_usage();
        }
    }

    /// Run Claude Code's `/usage` (a local command: no model call) in a
    /// throwaway session, so the user's own session and history never see it,
    /// then close and delete that session.
    fn fetch_usage(&mut self) {
        let Some(client) = self.client.clone() else { return };
        let caps = self.agent_caps.get("sessionCapabilities").cloned().unwrap_or(Value::Null);
        let (can_close, can_delete) = (caps.get("close").is_some(), caps.get("delete").is_some());
        let dir = home_dir().join("usage-probe");
        let _ = std::fs::create_dir_all(&dir);
        self.usage_inflight = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let cwd = dir.to_string_lossy().to_string();
            let sid = match client.request("session/new", json!({"cwd": cwd, "mcpServers": []})).await {
                Ok(v) => s(&v, "sessionId"),
                Err(e) => {
                    let _ = tx.send(AppMsg::UsageDone(Err(e)));
                    return;
                }
            };
            let _ = tx.send(AppMsg::UsageProbe(sid.clone()));
            let r = client.request("session/prompt", json!({"sessionId": sid, "prompt": [acp::text_block("/usage")]})).await;
            if can_close {
                let _ = client.request("session/close", json!({"sessionId": sid})).await;
            }
            if can_delete {
                let _ = client.request("session/delete", json!({"sessionId": sid})).await;
            }
            let _ = tx.send(AppMsg::UsageDone(r));
        });
    }

    fn apply_session_defaults(&mut self) {
        let want = self.config.agent.default_mode.trim().to_string();
        if !want.is_empty() && self.session.mode != want {
            if self.session.modes.iter().any(|m| m.id == want) {
                self.set_mode(&want);
            } else if !self.session.modes.is_empty() {
                self.transcript.notice(
                    Level::Warn,
                    format!(
                        "Default mode `{want}` isn't offered by the agent; staying in `{}`.{}",
                        self.session.mode,
                        if want == "bypassPermissions" {
                            " Claude Code disables bypass when running as root (set IS_SANDBOX=1) or when disabled by managed settings."
                        } else {
                            ""
                        }
                    ),
                );
            }
        }
        let model = self.config.agent.model.trim().to_string();
        if !model.is_empty() {
            if self.session.option("model").is_some() {
                let id = self.session.option("model").unwrap().id.clone();
                self.set_option(&id, &model);
            } else if !self.session.models.is_empty() {
                self.set_option("__model", &model);
            }
        }
        let effort = self.config.agent.effort.trim().to_string();
        if !effort.is_empty() {
            if let Some(o) = self.session.option("effort") {
                let id = o.id.clone();
                self.set_option(&id, &effort);
            }
        }
    }

    fn on_prompt_done(&mut self, res: Result<Value, RpcError>) {
        if let Ok(v) = &res {
            self.turn_tokens = Tokens::from_prompt_result(v);
            if let Some(t) = &self.turn_tokens {
                self.session_tokens.add(t);
            }
        }
        let cancelled = self.cancelling;
        self.responding = false;
        self.cancelling = false;
        if let Some(t) = self.turn_started.take() {
            self.last_turn = Some((t.elapsed(), self.turn_chars));
        }
        // Any cards still open belong to the finished turn.
        self.permissions.clear();
        self.question = None;
        match res {
            Ok(v) => {
                let reason = s(&v, "stopReason");
                self.transcript.end_turn(reason == "cancelled");
                match reason.as_str() {
                    "cancelled" => {
                        self.transcript.push(Block::TurnEnd { reason: "Interrupted".into() });
                    }
                    "max_tokens" => self.transcript.notice(Level::Warn, "Stopped: hit the output token limit"),
                    "max_turn_requests" => self.transcript.notice(Level::Warn, "Stopped: hit the turn request limit"),
                    "refusal" => self.transcript.notice(Level::Warn, "Claude declined to continue"),
                    _ => {}
                }
            }
            Err(e) => {
                self.transcript.end_turn(true);
                if cancelled {
                    self.transcript.push(Block::TurnEnd { reason: "Interrupted".into() });
                } else {
                    let mut msg = e.to_string();
                    if e.code == -32000 && msg.to_lowercase().contains("auth") {
                        msg.push_str(" — run /login, then /restart");
                    }
                    self.transcript.notice(Level::Error, msg);
                }
            }
        }
        if !cancelled {
            let took = self.last_turn.map(|(d, _)| d).unwrap_or_default();
            self.on_turn_finished_notify(took);
        }
        self.turns_done += 1;
        let send_now = std::mem::take(&mut self.send_now) > 0;
        if cancelled && !self.queue.is_empty() && !send_now {
            self.queue_paused = true;
            let n = self.queue.len();
            self.toast(format!(
                "Stopped · {n} queued prompt{} on hold: Enter sends, Ctrl+C drops",
                if n == 1 { "" } else { "s" }
            ));
        }
        if self.config.features.narrator && !cancelled {
            self.narrate_last_turn();
        }
        if self.config.features.timeline {
            self.take_snapshot();
        } else {
            self.after_turn_housekeeping();
        }
    }

    /// Speak the turn's final message (which usually sums up what was done).
    fn narrate_last_turn(&mut self) {
        let last = self.transcript.entries.iter().rev().take_while(|e| !matches!(e.block, Block::User { .. })).find_map(|e| match &e.block {
            Block::Agent { text } => Some(text.clone()),
            _ => None,
        });
        let Some(last) = last else { return };
        let summary = narrator::summarize(&last);
        if let Err(e) = self.narrator.speak(&summary) {
            self.transcript.notice(Level::Warn, format!("Narrator: {e}"));
        }
    }

    fn take_snapshot(&mut self) {
        let label = crate::text::truncate(self.turn_prompt.lines().next().unwrap_or("turn"), 60);
        self.timeline_busy = true;
        let tl = self.timeline.clone();
        let cwd = self.cwd.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let r = tl.lock().map_err(|_| anyhow::anyhow!("timeline lock poisoned")).and_then(|mut t| t.snapshot(&cwd, &label));
            let msg = match r {
                Ok(_) => None,
                Err(e) => Some((format!("Time Machine snapshot failed: {e}"), true)),
            };
            let _ = tx.send(AppMsg::Timeline(msg));
        });
    }

    fn timeline_jump(&mut self, to: usize) {
        if self.busy() {
            self.toast("Wait for Claude to finish first");
            return;
        }
        self.timeline_busy = true;
        let tl = self.timeline.clone();
        let cwd = self.cwd.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let r = tl.lock().map_err(|_| anyhow::anyhow!("timeline lock poisoned")).and_then(|mut t| {
                let n = t.jump(&cwd, to)?;
                let label = t.snaps[to].label.clone();
                Ok((n, label))
            });
            let msg = match r {
                Ok((n, label)) => Some((format!("Restored “{label}” · {n} file{}", if n == 1 { "" } else { "s" }), false)),
                Err(e) => Some((format!("Time Machine: {e}"), true)),
            };
            let _ = tx.send(AppMsg::Timeline(msg));
        });
    }

    /// Lock the Time Machine without blocking the UI: a snapshot runs git in
    /// the background while holding the lock, which can take a while.
    fn timeline_try_lock_on<'a>(&mut self, tl: &'a std::sync::Mutex<Timeline>) -> Option<std::sync::MutexGuard<'a, Timeline>> {
        match tl.try_lock() {
            Ok(t) => Some(t),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.toast("Time Machine is saving a snapshot, try again in a moment");
                None
            }
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
        }
    }

    fn timeline_step(&mut self, delta: isize) {
        let tl = self.timeline.clone();
        let (cur, len, avail) = match self.timeline_try_lock_on(&tl) {
            Some(t) => (t.cur, t.snaps.len(), t.available()),
            None => return,
        };
        if len == 0 {
            self.toast(if avail || !self.config.features.timeline { "No snapshots yet" } else { "Time Machine needs a git repository" });
            return;
        }
        let to = cur as isize + delta;
        if to < 0 || to as usize >= len {
            self.toast(if delta < 0 { "Already at the oldest snapshot" } else { "Already at the newest snapshot" });
            return;
        }
        self.timeline_jump(to as usize);
    }

    fn open_timeline(&mut self) {
        let tl = self.timeline.clone();
        let Some(t) = self.timeline_try_lock_on(&tl) else { return };
        if t.snaps.is_empty() {
            drop(t);
            self.toast("No snapshots yet — they're taken after each turn that changes files");
            return;
        }
        let items: Vec<PickItem> = t
            .snaps
            .iter()
            .enumerate()
            .rev()
            .map(|(i, sn)| {
                let mark = if i == t.cur { "◉" } else if i > t.cur { "○" } else { "●" };
                let detail = match (i, sn.changed) {
                    (0, _) => format!("{} · start", sn.time.format("%H:%M")),
                    (_, n) => format!("{} · {n} file{}", sn.time.format("%H:%M"), if n == 1 { "" } else { "s" }),
                };
                PickItem { label: format!("{mark} {}", sn.label), detail, value: i.to_string(), current: i == t.cur }
            })
            .collect();
        drop(t);
        self.open_picker(PickerKind::Timeline, "Timeline  ·  Enter restore  ·  → preview", items);
    }

    fn preview_timeline(&mut self, idx: usize) {
        let cwd = self.cwd.clone();
        let tl = self.timeline.clone();
        let Some(mut t) = self.timeline_try_lock_on(&tl) else { return };
        let r = (|| -> anyhow::Result<_> {
            let stat = t.diff_stat(&cwd, idx)?;
            let diff = t.diff(&cwd, idx)?;
            Ok((t.snaps[idx].label.clone(), format!("{stat}\n{diff}")))
        })();
        drop(t);
        match r {
            Ok((label, text)) => self.overlay = Overlay::Viewer { title: format!("Snapshot · {label}"), text, scroll: 0 },
            Err(e) => self.toast(format!("Couldn't diff: {e}")),
        }
    }

    /// `/narrator` and Alt+N: turn the feature on or off (stopping any speech).
    /// The footer button calls this with `click_to_stop`: while speaking, a
    /// click only silences the current summary.
    pub fn toggle_narrator(&mut self, click_to_stop: bool) {
        if self.narrator.speaking() {
            self.narrator.stop();
            self.narrating = false;
            if click_to_stop {
                return;
            }
        }
        self.config.features.narrator = !self.config.features.narrator;
        let on = self.config.features.narrator;
        Config::persist(|c| c.features.narrator = on);
        if !self.config.features.narrator {
            self.narrator.stop();
        }
        self.toast(if self.config.features.narrator { "Narrator on" } else { "Narrator off" });
    }

    fn open_commit_picker(&mut self) {
        let items = vec![
            PickItem { label: "Commit and push".into(), detail: "recommended".into(), value: "push".into(), current: true },
            PickItem { label: "Commit only".into(), detail: "don't push".into(), value: "commit".into(), current: false },
            PickItem { label: "Commit on a new branch".into(), detail: "named after the changes".into(), value: "branch".into(), current: false },
        ];
        self.open_picker(PickerKind::Commit, "Commit", items);
    }

    fn run_commit(&mut self, how: &str) {
        let (label, step) = match how {
            "commit" => ("commit only", "Commit on the current branch. Do not push."),
            "branch" => (
                "new branch",
                "First create and switch to a new branch with a short, descriptive name based on the changes, then commit there. Do not push.",
            ),
            _ => ("commit & push", "Commit on the current branch, then push it (set the upstream if it has none)."),
        };
        let text = format!(
            "Commit my uncommitted changes in this git repository.\n\
             - Read `git status` and `git diff` (staged and unstaged, plus untracked files) and group the changes into logical units.\n\
             - Make one Conventional Commit per group (`type(scope): summary`, imperative mood, under 72 characters; add a body only when it explains why). Stage only that group's files for each commit.\n\
             - Never stage secrets or credentials (.env files, private keys, tokens, *.pem, credential files). If you find any, leave them unstaged and tell me.\n\
             - Do not add Co-Authored-By trailers or any mention of Claude or AI to the commits.\n\
             - {step}\n\
             - Finish with one line per commit: short hash and subject."
        );
        let out = Outgoing { text, haiku: true, raw: format!("/commit {how}"), ..Outgoing::default() };
        self.enqueue(format!("/commit · {label}"), out);
    }

    fn apply_session_update(&mut self, u: &Value) {
        let fx = self.transcript.apply_update(&mut self.session, u);
        if self.responding {
            self.turn_chars += fx.output_chars;
        }
        if fx.title_changed {
            self.update_title();
        }
    }

    async fn on_incoming(&mut self, inc: Incoming) {
        match inc {
            Incoming::Notification { method, params } => {
                if method == "session/update" {
                    // The `/usage` probe's session: capture its reply, ignore the rest.
                    let update_sid = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
                    if !update_sid.is_empty() && Some(update_sid) != self.session.id.as_deref() {
                        let Some(u) = params.get("update") else { return };
                        if self.btw.iter().any(|b| b.sid.as_deref() == Some(update_sid)) {
                            self.on_btw_update(update_sid.to_string(), u);
                            return;
                        }
                        match self.usage_probe.as_mut() {
                            Some((probe, buf)) if probe == update_sid => {
                                if s(u, "sessionUpdate") == "agent_message_chunk" {
                                    if let Some(t) = u.get("content").and_then(crate::model::content_text) {
                                        buf.push_str(&t);
                                    }
                                }
                            }
                            // A `session/new` is in flight and its reply hasn't been
                            // processed yet: this may be the new session's first output.
                            _ if self.session.id.is_none() && self.early_updates.len() < 10_000 => {
                                self.early_updates.push((update_sid.to_string(), u.clone()));
                            }
                            _ => {}
                        }
                        return;
                    }
                    if let Some(u) = params.get("update") {
                        self.apply_session_update(u);
                    }
                }
            }
            Incoming::Request { id, method, params } => self.on_request(id, &method, params),
            Incoming::Stderr(line) => {
                log_agent_line(&line);
                self.stderr_tail.push_back(line);
                while self.stderr_tail.len() > 40 {
                    self.stderr_tail.pop_front();
                }
            }
            Incoming::Exited(code) => {
                self.client = None;
                self.responding = false;
                self.permissions.clear();
                self.question = None;
                self.conn = Conn::Exited;
                let tail: Vec<String> = self
                    .stderr_tail
                    .iter()
                    .rev()
                    .filter(|l| !l.trim().is_empty())
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let mut msg = format!("Claude Code exited (code {}). Use /restart to start it again.", code.map(|c| c.to_string()).unwrap_or("?".into()));
                if !tail.is_empty() {
                    msg.push_str("\n");
                    msg.push_str(&tail.join("\n"));
                }
                self.transcript.notice(Level::Error, msg);
            }
        }
    }

    fn on_request(&mut self, id: Value, method: &str, params: Value) {
        let Some(client) = self.client.clone() else { return };
        // Background sessions (a /btw fork, the /usage probe) never get to
        // change anything: refuse what they ask permission for.
        let req_sid = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        let background = self.btw.iter().any(|b| b.sid.as_deref() == Some(req_sid))
            || self.usage_probe.as_ref().is_some_and(|(p, _)| p == req_sid);
        let foreign = !req_sid.is_empty() && (background || self.session.id.as_deref().is_some_and(|me| me != req_sid));
        if foreign && matches!(method, "session/request_permission" | "elicitation/create") {
            if method == "elicitation/create" {
                client.respond(id, json!({"action": "decline"}));
                return;
            }
            let reject = params
                .get("options")
                .and_then(|o| o.as_array())
                .and_then(|a| a.iter().find(|o| s(o, "kind") == "reject_once").map(|o| s(o, "optionId")));
            let outcome = match reject {
                Some(oid) => json!({"outcome": {"outcome": "selected", "optionId": oid}}),
                None => json!({"outcome": {"outcome": "cancelled"}}),
            };
            client.respond(id, outcome);
            return;
        }
        match method {
            "session/request_permission" => {
                let tc = params.get("toolCall").cloned().unwrap_or(Value::Null);
                let mut title = s(&tc, "title");
                if let Some(tid) = tc.get("toolCallId").and_then(|v| v.as_str()) {
                    if let Some(&idx) = self.transcript.tool_index.get(tid) {
                        if let Block::Tool(t) = &self.transcript.entries[idx].block {
                            if title.is_empty() {
                                title = t.title.clone();
                            }
                        }
                    }
                }
                let mut detail = Vec::new();
                if let Some(raw) = tc.get("rawInput") {
                    if let Some(cmd) = raw.get("command").and_then(|c| c.as_str()) {
                        detail.push(format!("$ {cmd}"));
                    }
                    if let Some(d) = raw.get("description").and_then(|c| c.as_str()) {
                        detail.push(d.to_string());
                    }
                    if let Some(p) = raw.get("file_path").and_then(|c| c.as_str()) {
                        if !title.contains(p) {
                            detail.push(p.to_string());
                        }
                    }
                    if let Some(plan) = raw.get("plan").and_then(|c| c.as_str()) {
                        detail.extend(plan.lines().map(str::to_owned));
                    }
                }
                let options: Vec<(String, String, String)> = params
                    .get("options")
                    .and_then(|o| o.as_array())
                    .map(|a| a.iter().map(|o| (s(o, "optionId"), s(o, "name"), s(o, "kind"))).collect())
                    .unwrap_or_default();
                if title.is_empty() {
                    title = "Permission requested".into();
                }
                self.permissions.push_back(PermissionCard { id, title, detail, options, sel: 0, shown: Instant::now() });
                self.attention();
            }
            "elicitation/create" => {
                if let Some(card) = build_question(id.clone(), &params) {
                    // Only one question card shows at a time; never leave the
                    // agent waiting on one that got replaced.
                    if self.question.is_some() {
                        self.answer_question("cancel");
                    }
                    self.question = Some(card);
                    self.attention();
                } else {
                    client.respond(id, json!({"action": "cancel"}));
                }
            }
            "fs/read_text_file" => {
                let path = s(&params, "path");
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let line = params.get("line").and_then(|l| l.as_u64()).unwrap_or(1).max(1) as usize;
                        let limit = params.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize);
                        let out: String = if line > 1 || limit.is_some() {
                            content
                                .lines()
                                .skip(line - 1)
                                .take(limit.unwrap_or(usize::MAX))
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            content
                        };
                        client.respond(id, json!({"content": out}));
                    }
                    Err(e) => client.respond_error(id, -32002, &format!("{path}: {e}")),
                }
            }
            "fs/write_text_file" => {
                let path = s(&params, "path");
                let content = s(&params, "content");
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&path, content) {
                    Ok(_) => client.respond(id, Value::Null),
                    Err(e) => client.respond_error(id, -32603, &format!("{path}: {e}")),
                }
            }
            _ => client.respond_error(id, -32601, &format!("method not supported by client: {method}")),
        }
    }

    fn answer_permission(&mut self, choice: Option<usize>) {
        let Some(card) = self.permissions.pop_front() else { return };
        if let Some(next) = self.permissions.front_mut() {
            next.shown = Instant::now();
        }
        let Some(client) = self.client.clone() else { return };
        let outcome = match choice.and_then(|i| card.options.get(i)) {
            Some((oid, _, _)) => json!({"outcome": {"outcome": "selected", "optionId": oid}}),
            None => {
                // Esc: prefer an explicit reject option.
                match card.options.iter().find(|o| o.2 == "reject_once") {
                    Some((oid, _, _)) => json!({"outcome": {"outcome": "selected", "optionId": oid}}),
                    None => json!({"outcome": {"outcome": "cancelled"}}),
                }
            }
        };
        client.respond(card.id, outcome);
    }

    fn answer_question(&mut self, action: &str) {
        let Some(card) = self.question.take() else { return };
        let Some(client) = self.client.clone() else { return };
        if action == "accept" {
            client.respond(card.id, json!({"action": "accept", "content": Value::Object(card.answers)}));
        } else {
            client.respond(card.id, json!({"action": action}));
        }
    }

    fn update_title(&mut self) {
        if let Some(t) = &self.title_override {
            self.session.title = Some(t.clone());
        }
        let t = match &self.session.title {
            Some(t) => format!("{t} - blush"),
            None => "blush".to_string(),
        };
        self.terminal_title = t;
    }

    // ------------------------------------------------------------------
    // Prompts
    // ------------------------------------------------------------------

    fn session_ready(&self) -> bool {
        self.conn == Conn::Ready && self.session.id.is_some() && self.client.is_some()
    }

    pub async fn submit(&mut self, raw: String) {
        let text = raw.trim_end().to_string();
        if text.trim().is_empty() && !self.attachments.items.is_empty() {
            self.attachments.clear();
        }
        if text.trim().is_empty() {
            if self.queue_paused {
                self.queue_paused = false;
                self.pump_queue();
            }
            return;
        }
        save_history_line(&text);
        if let Some(cmd) = text.strip_prefix('!') {
            self.attachments.clear();
            self.run_shell(cmd.trim().to_string());
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if LOCAL_COMMANDS.iter().any(|(n, _)| *n == name) || matches!(name, "exit" | "q" | "yolo" | "bypass" | "keys" | "t") {
                self.run_local(name, args.trim()).await;
                return;
            }
        }
        let out = self.outgoing_from(&text);
        self.enqueue(out.text.clone(), out);
    }

    /// Build the prompt for `text`: expand pasted-text placeholders, attach
    /// images it mentions, and pick up pending shell output.
    fn outgoing_from(&mut self, text: &str) -> Outgoing {
        let (expanded, images) = self.attachments.take(text);
        // `raw` keeps image placeholders but has pasted text expanded, so a
        // queued prompt pulled back for editing loses nothing.
        let mut out = Outgoing::parse(&expanded);
        out.images = images;
        out.context = std::mem::take(&mut self.pending_context);
        out
    }

    /// Show `display` as the user's prompt and send `out` now, or queue it
    /// if Claude is busy.
    fn enqueue(&mut self, display: String, out: Outgoing) {
        let tag = out.tag();
        if self.busy() || !self.session_ready() || self.queue_paused {
            let idx = self.transcript.push(Block::User { text: display, queued: true, tag });
            self.queue.push_back((out, idx));
            self.follow = true;
            if self.queue_paused {
                self.queue_paused = false;
                self.pump_queue();
                return;
            }
            if !self.session_ready() && !matches!(self.conn, Conn::Starting(_)) {
                self.toast("Agent isn't running — /restart to start it");
            }
            return;
        }
        let idx = self.transcript.push(Block::User { text: display, queued: false, tag });
        self.send_prompt(out, idx);
    }

    fn pump_queue(&mut self) {
        if self.busy() || !self.session_ready() || self.queue_paused {
            return;
        }
        if let Some((out, idx)) = self.queue.pop_front() {
            // Move the queued entry to the end so the transcript order stays true.
            if idx < self.transcript.entries.len() {
                if let Block::User { queued, .. } = &mut self.transcript.entries[idx].block {
                    *queued = false;
                }
                self.transcript.entries[idx].touch();
                if idx + 1 != self.transcript.entries.len() {
                    let e = self.transcript.entries.remove(idx);
                    self.transcript.entries.push(e);
                    // Fix up indices that shifted.
                    let last = self.transcript.entries.len() - 1;
                    self.selected = self.selected.map(|s| if s == idx { last } else if s > idx { s - 1 } else { s });
                    for v in self.transcript.tool_index.values_mut() {
                        if *v > idx {
                            *v -= 1;
                        }
                    }
                    for q in self.queue.iter_mut() {
                        if q.1 > idx {
                            q.1 -= 1;
                        }
                    }
                    if let Some(p) = self.transcript.plan_entry.as_mut() {
                        if *p > idx {
                            *p -= 1;
                        }
                    }
                }
            }
            let new_idx = self.transcript.entries.len() - 1;
            self.send_prompt(out, new_idx);
        }
    }

    /// Discard prompts held after a stopped turn, and their transcript entries.
    fn drop_queue(&mut self) {
        self.queue_paused = false;
        let mut idxs: Vec<usize> = self.queue.drain(..).map(|(_, i)| i).collect();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        let n = idxs.len();
        for idx in idxs {
            if idx >= self.transcript.entries.len() {
                continue;
            }
            self.transcript.entries.remove(idx);
            let shift = |v: &mut usize| {
                if *v > idx {
                    *v -= 1;
                }
            };
            self.transcript.tool_index.values_mut().for_each(shift);
            if let Some(p) = self.transcript.plan_entry.as_mut() {
                shift(p);
            }
            self.selected = match self.selected {
                Some(s) if s == idx => None,
                Some(mut s) => {
                    shift(&mut s);
                    Some(s)
                }
                None => None,
            };
        }
        self.invalidate_all();
        self.toast(format!("Dropped {n} queued prompt{}", if n == 1 { "" } else { "s" }));
    }

    /// (config id, value for this turn, value to restore afterwards)
    fn turn_overrides(&mut self, out: &Outgoing) -> Vec<(String, String, String)> {
        let mut v = Vec::new();
        if out.haiku {
            let found = if let Some(o) = self.session.option("model") {
                o.options
                    .iter()
                    .find(|(val, name, _)| val.to_lowercase().contains("haiku") || name.to_lowercase().contains("haiku"))
                    .map(|(val, _, _)| (o.id.clone(), val.clone(), o.current.clone()))
            } else {
                self.session
                    .models
                    .iter()
                    .find(|m| m.id.to_lowercase().contains("haiku") || m.name.to_lowercase().contains("haiku"))
                    .map(|m| ("__model".to_string(), m.id.clone(), self.session.model.clone()))
            };
            match found {
                Some(f) if f.1 != f.2 => v.push(f),
                Some(_) => {}
                None => self.toast("Haiku isn't offered here — using the session's model"),
            }
        }
        if out.max_effort {
            match self.session.option("effort") {
                Some(o) => {
                    let max = o.options.iter().find(|(val, _, _)| val == "max").or(o.options.last()).map(|x| x.0.clone());
                    if let Some(max) = max.filter(|m| *m != o.current) {
                        v.push((o.id.clone(), max, o.current.clone()));
                    }
                }
                None => self.toast("Effort isn't configurable for this model"),
            }
        }
        v
    }

    fn send_prompt(&mut self, out: Outgoing, _entry: usize) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session.id.clone()) else { return };
        let overrides = self.turn_overrides(&out);
        self.turn_tag = out.tag();
        self.turn_prompt = out.text.clone();
        let text = out.text.clone();
        self.narrator.stop();
        self.transcript.begin_turn();
        self.responding = true;
        self.cancelling = false;
        self.turn_started = Some(Instant::now());
        self.turn_chars = 0;
        self.follow = true;
        self.scroll = 0;
        self.selected = None;
        let mut blocks: Vec<Value> = out.context.iter().map(|c| acp::text_block(c)).collect();
        blocks.push(acp::text_block(&text));
        blocks.extend(out.images.iter().filter_map(|a| match &a.item {
            crate::attach::Attachment::Image(v) => Some(v.clone()),
            crate::attach::Attachment::Text(_) => None,
        }));
        for p in mentioned_paths(&text, &self.cwd) {
            if p.is_file() {
                blocks.push(acp::resource_link(&p));
            }
        }
        let tx = self.tx.clone();
        let baseline = self.config.features.timeline.then(|| (self.timeline.clone(), self.cwd.clone()));
        tokio::spawn(async move {
            // Time Machine: record the starting state before the first turn touches anything.
            if let Some((tl, cwd)) = baseline {
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mut t) = tl.lock() {
                        if t.snaps.is_empty() {
                            let _ = t.snapshot(&cwd, "start");
                        }
                    }
                })
                .await;
            }
            for (id, value, _) in &overrides {
                let r = set_option_request(&client, &sid, id, value).await;
                let _ = tx.send(if id == "__model" { AppMsg::LegacyModel(value.clone()) } else { AppMsg::ConfigSet(None, r) });
            }
            let r = client.request("session/prompt", json!({"sessionId": sid, "prompt": blocks})).await;
            for (id, _, prev) in overrides.iter().rev() {
                let r2 = set_option_request(&client, &sid, id, prev).await;
                let _ = tx.send(if id == "__model" { AppMsg::LegacyModel(prev.clone()) } else { AppMsg::ConfigSet(None, r2) });
            }
            let _ = tx.send(AppMsg::PromptDone(r));
        });
    }

    fn cancel_turn(&mut self) {
        if !self.responding {
            return;
        }
        if let (Some(client), Some(sid)) = (self.client.clone(), self.session.id.clone()) {
            // Pending permission prompts must be answered `cancelled`.
            while let Some(card) = self.permissions.pop_front() {
                client.respond(card.id, json!({"outcome": {"outcome": "cancelled"}}));
            }
            if let Some(q) = self.question.take() {
                client.respond(q.id, json!({"action": "cancel"}));
            }
            client.notify("session/cancel", json!({"sessionId": sid}));
            self.cancelling = true;
        }
    }

    fn reset_transcript(&mut self) {
        let show = self.transcript.show_thinking;
        let expand = self.transcript.expand_thinking;
        let (tools, diff) = (self.transcript.expand_tools, self.transcript.diff_focus);
        self.transcript = Transcript::new();
        self.transcript.show_thinking = show;
        self.transcript.expand_thinking = expand;
        self.transcript.expand_tools = tools;
        self.transcript.diff_focus = diff;
        self.search = None;
        self.title_override = None;
        // Shell output belongs to the session it was run in.
        self.pending_context.clear();
        self.selected = None;
        self.scroll = 0;
        self.follow = true;
        self.queue.clear();
        self.queue_paused = false;
    }

    async fn run_local(&mut self, name: &str, args: &str) {
        match name {
            "new" | "clear" => {
                if self.responding {
                    self.cancel_turn();
                }
                self.reset_transcript();
                self.transcript.push(Block::Welcome);
                self.session.title = None;
                self.session.used = 0;
                self.session.cost = None;
                self.turn_tokens = None;
                self.session_tokens = Tokens::default();
                self.update_title();
                if self.client.is_some() {
                    self.open_session(SessionKind::New);
                } else {
                    self.startup = Some(SessionKind::New);
                    self.start_agent();
                }
            }
            "resume" => {
                if !args.is_empty() {
                    self.resume(args.to_string());
                    return;
                }
                let Some(client) = self.client.clone() else {
                    self.toast("Agent isn't running");
                    return;
                };
                if !self.can_list() {
                    self.transcript.notice(Level::Warn, "This agent doesn't support listing sessions. Use /resume <session-id>.");
                    return;
                }
                let tx = self.tx.clone();
                let cwd = self.cwd.to_string_lossy().to_string();
                tokio::spawn(async move {
                    let r = client.request("session/list", json!({"cwd": cwd})).await;
                    let _ = tx.send(AppMsg::Sessions(r));
                });
            }
            "model" => {
                if !args.is_empty() {
                    match self.session.option("model").map(|o| o.id.clone()) {
                        Some(id) => self.set_option(&id, args),
                        None => self.set_option("__model", args),
                    }
                    return;
                }
                self.open_option_picker("model", PickerKind::Model);
            }
            "effort" => {
                if !args.is_empty() {
                    if let Some(id) = self.session.option("effort").map(|o| o.id.clone()) {
                        self.set_option(&id, args);
                    } else {
                        self.toast("Effort isn't configurable for this model");
                    }
                    return;
                }
                self.open_option_picker("effort", PickerKind::Effort);
            }
            "mode" => {
                if !args.is_empty() {
                    self.set_mode(args);
                    return;
                }
                let items = self
                    .session
                    .modes
                    .iter()
                    .map(|m| PickItem {
                        label: m.name.clone(),
                        detail: m.description.clone(),
                        value: m.id.clone(),
                        current: m.id == self.session.mode,
                    })
                    .collect();
                self.open_picker(PickerKind::Mode, "Permission mode", items);
            }
            "fast" => match self.session.option("fast").map(|o| (o.id.clone(), o.current.clone())) {
                Some((id, cur)) => {
                    let next = if cur == "on" { "off" } else { "on" };
                    self.set_option(&id, next);
                    self.toast(format!("Fast mode {next}"));
                }
                None => self.toast("Fast mode isn't available for this model"),
            },
            "always-approve" | "yolo" | "bypass" => self.set_mode("bypassPermissions"),
            "ask" => self.set_mode("default"),
            "accept-edits" => self.set_mode("acceptEdits"),
            "plan" => {
                self.set_mode("plan");
                if !args.is_empty() {
                    let a = args.to_string();
                    Box::pin(self.submit(a)).await;
                }
            }
            "theme" | "t" => {
                if !args.is_empty() {
                    self.apply_theme(args, true);
                    return;
                }
                self.open_theme_picker();
            }
            "copy" => {
                // `/copy 2` copies the second-latest response.
                let n = args.parse::<usize>().unwrap_or(1).max(1);
                let nth = self
                    .transcript
                    .entries
                    .iter()
                    .rev()
                    .filter_map(|e| match &e.block {
                        Block::Agent { text } => Some(text.clone()),
                        _ => None,
                    })
                    .nth(n - 1);
                match nth {
                    Some(t) => self.copy(&t),
                    None if n > 1 => self.toast(format!("There aren't {n} responses yet")),
                    None => self.toast("Nothing to copy yet"),
                }
            }
            "export" => self.export(args),
            "commit" => match args {
                "push" | "commit" | "branch" => self.run_commit(args),
                _ => self.open_commit_picker(),
            },
            "timeline" => self.open_timeline(),
            "undo" => self.timeline_step(-1),
            "redo" => self.timeline_step(1),
            "after" => {
                if args.is_empty() {
                    self.toast("Usage: /after <task> — runs once Claude finishes");
                } else {
                    let out = self.outgoing_from(args);
                    self.enqueue(out.text.clone(), out);
                }
            }
            "narrator" => self.toggle_narrator(false),
            "compact-mode" => self.toggle_compact(),
            "context" => {
                let s = &self.session;
                let msg = if s.size > 0 {
                    format!(
                        "Context: {} / {} tokens ({:.0}%){}",
                        crate::text::human_tokens(s.used),
                        crate::text::human_tokens(s.size),
                        s.used as f64 * 100.0 / s.size as f64,
                        s.cost.map(|c| format!(" · session cost ${c:.4}")).unwrap_or_default()
                    )
                } else {
                    "No usage reported yet".into()
                };
                self.transcript.notice(Level::Info, msg);
            }
            "session-info" => {
                let msg = format!(
                    "Session: {}\nAgent: {}\nModel: {}\nMode: {}\nCwd: {}\nConfig: {}\nAdapter: {}",
                    self.session.id.clone().unwrap_or("—".into()),
                    self.agent_name,
                    self.session.model_label(),
                    self.session.mode,
                    self.cwd.display(),
                    crate::config::config_path().display(),
                    launcher::installed_adapter_version().unwrap_or_else(|| "custom / global".into()),
                );
                self.transcript.notice(Level::Info, msg);
            }
            "thinking" => {
                self.transcript.show_thinking = !self.transcript.show_thinking;
                self.config.ui.show_thinking = self.transcript.show_thinking;
                let on = self.config.ui.show_thinking;
                Config::persist(|c| c.ui.show_thinking = on);
                self.toast(if self.transcript.show_thinking { "Thinking shown" } else { "Thinking hidden" });
            }
            "timestamps" => self.set_timestamps(args),
            "restart" => {
                self.transcript.notice(Level::Info, "Restarting Claude Code…");
                self.restart_agent().await;
            }
            "login" => {
                self.external = Some(External::Login);
            }
            "config" | "settings" => self.open_settings(),
            "shortcuts" | "help" | "keys" => {
                self.shortcuts_scroll = 0;
                self.overlay = Overlay::Shortcuts;
            }
            "quit" | "exit" | "q" => self.should_quit = true,
            other => self.run_extra_command(other, args).await,
        }
    }

    pub async fn after_login(&mut self) {
        self.transcript.notice(Level::Info, "Restarting Claude Code after sign-in…");
        self.restart_agent().await;
    }

    fn resume(&mut self, id: String) {
        if self.client.is_none() {
            self.startup = Some(SessionKind::Load(id));
            self.start_agent();
            return;
        }
        if self.responding {
            self.cancel_turn();
        }
        self.reset_transcript();
        let kind = if self.can_load() { SessionKind::Load(id) } else { SessionKind::Resume(id) };
        if matches!(kind, SessionKind::Resume(_)) {
            self.transcript.push(Block::Welcome);
        }
        self.open_session(kind);
    }

    fn open_theme_picker(&mut self) {
        let items = THEME_NAMES
            .iter()
            .map(|(n, d)| PickItem { label: n.to_string(), detail: d.to_string(), value: n.to_string(), current: *n == self.theme.name })
            .collect();
        self.open_picker(PickerKind::Theme, "Theme", items);
        if let Overlay::Picker(p) = &mut self.overlay {
            p.original_theme = Some(self.config.ui.theme.clone());
        }
    }

    fn open_option_picker(&mut self, id: &str, kind: PickerKind) {
        if let Some(o) = self.session.option(id) {
            let items = o
                .options
                .iter()
                .map(|(v, n, d)| PickItem { label: n.clone(), detail: d.clone(), value: v.clone(), current: *v == o.current })
                .collect();
            let title = o.name.clone();
            self.open_picker(kind, &title, items);
        } else if id == "model" && !self.session.models.is_empty() {
            let items = self
                .session
                .models
                .iter()
                .map(|m| PickItem {
                    label: m.name.clone(),
                    detail: m.description.clone(),
                    value: m.id.clone(),
                    current: m.id == self.session.model,
                })
                .collect();
            self.open_picker(PickerKind::Option("__model".into()), "Model", items);
        } else {
            self.toast(format!("No {id} options available yet"));
        }
    }

    fn open_picker(&mut self, kind: PickerKind, title: &str, items: Vec<PickItem>) {
        let sel = items.iter().position(|i| i.current).unwrap_or(0);
        self.overlay = Overlay::Picker(Picker { kind, title: title.into(), items, filter: String::new(), sel, original_theme: None });
    }

    fn open_resume_picker(&mut self, v: &Value) {
        let sessions = v.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        if sessions.is_empty() {
            self.toast("No previous sessions in this directory");
            return;
        }
        let items = sessions
            .iter()
            .map(|sess| {
                let title = extras::local_title(&s(sess, "sessionId")).unwrap_or_else(|| s(sess, "title"));
                let updated = s(sess, "updatedAt");
                let when = chrono::DateTime::parse_from_rfc3339(&updated)
                    .map(|d| relative_time(d.with_timezone(&chrono::Local)))
                    .unwrap_or_default();
                PickItem {
                    label: if title.is_empty() { s(sess, "sessionId") } else { title },
                    detail: when,
                    value: s(sess, "sessionId"),
                    current: Some(s(sess, "sessionId")) == self.session.id,
                }
            })
            .collect();
        self.open_picker(PickerKind::Resume, "Resume session", items);
        if let Overlay::Picker(p) = &mut self.overlay {
            p.sel = 0;
        }
    }

    fn apply_theme(&mut self, name: &str, persist: bool) {
        if Theme::by_name(name).is_none() {
            self.toast(format!("Unknown theme: {name}"));
            return;
        }
        let mut cfg = self.config.clone();
        cfg.ui.theme = name.to_string();
        self.theme = cfg.theme();
        if persist {
            self.config = cfg;
            Config::persist(|c| c.ui.theme = name.to_string());
        }
        self.invalidate_all();
    }

    async fn picker_choose(&mut self) {
        let Overlay::Picker(p) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        let vis = p.visible();
        let Some(item) = vis.get(p.sel).and_then(|i| p.items.get(*i)).cloned() else {
            // Nothing matches the filter: keep the picker open.
            self.overlay = Overlay::Picker(p);
            return;
        };
        match p.kind {
            PickerKind::Model => {
                if let Some(id) = self.session.option("model").map(|o| o.id.clone()) {
                    self.set_option(&id, &item.value);
                }
            }
            PickerKind::Effort => {
                if let Some(id) = self.session.option("effort").map(|o| o.id.clone()) {
                    self.set_option(&id, &item.value);
                }
            }
            PickerKind::Option(id) => self.set_option(&id, &item.value),
            PickerKind::Mode => self.set_mode(&item.value),
            PickerKind::Theme => self.apply_theme(&item.value, true),
            PickerKind::Resume => self.resume(item.value),
            PickerKind::Commit => self.run_commit(&item.value),
            PickerKind::Timeline => {
                if let Ok(i) = item.value.parse() {
                    self.timeline_jump(i);
                }
            }
            PickerKind::Palette => Box::pin(self.palette_run(item.value)).await,
        }
    }

    fn picker_preview(&mut self) {
        if let Overlay::Picker(p) = &self.overlay {
            if p.kind == PickerKind::Theme {
                let vis = p.visible();
                if let Some(item) = vis.get(p.sel).and_then(|i| p.items.get(*i)) {
                    let v = item.value.clone();
                    self.apply_theme(&v, false);
                }
            }
        }
    }

    fn picker_cancel(&mut self) {
        if let Overlay::Picker(p) = std::mem::replace(&mut self.overlay, Overlay::None) {
            if let Some(orig) = p.original_theme {
                self.apply_theme(&orig, false);
            }
        }
    }

    pub fn invalidate_all(&mut self) {
        for e in &mut self.transcript.entries {
            e.touch();
        }
    }

    fn copy(&mut self, text: &str) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
            Ok(_) => self.toast("Copied to clipboard"),
            Err(_) => {
                // OSC 52 fallback (works over SSH in most terminals).
                let b64 = base64_encode(text.as_bytes());
                let _ = std::io::stdout().write_all(format!("\x1b]52;c;{b64}\x07").as_bytes());
                let _ = std::io::stdout().flush();
                self.toast("Copied (OSC 52)");
            }
        }
    }

    fn export(&mut self, args: &str) {
        let mut md = String::new();
        if let Some(t) = &self.session.title {
            md.push_str(&format!("# {t}\n\n"));
        }
        for e in &self.transcript.entries {
            match &e.block {
                Block::User { text, .. } => md.push_str(&format!("## ❯ User — {}\n\n{text}\n\n", e.time.format("%H:%M"))),
                Block::Agent { text } => md.push_str(&format!("{text}\n\n")),
                Block::Tool(t) => md.push_str(&format!("> **{}** `{}`\n\n", t.kind, t.title.replace('`', ""))),
                Block::Plan(_) => md.push_str(&format!("```\n{}\n```\n\n", e.plain_text())),
                Block::Notice { text, .. } => md.push_str(&format!("> {text}\n\n")),
                Block::Aside { question, answer, .. } => md.push_str(&format!("> **btw** {question}\n>\n> {}\n\n", answer.replace('\n', "\n> "))),
                _ => {}
            }
        }
        let name = if args.is_empty() {
            format!("blush-{}.md", chrono::Local::now().format("%Y%m%d-%H%M%S"))
        } else {
            args.to_string()
        };
        let path = self.cwd.join(name);
        match std::fs::write(&path, md) {
            Ok(_) => self.transcript.notice(Level::Success, format!("Exported to {}", path.display())),
            Err(e) => self.transcript.notice(Level::Error, format!("Export failed: {e}")),
        }
    }

    // ------------------------------------------------------------------
    // Completions
    // ------------------------------------------------------------------

    pub fn refresh_completion(&mut self) {
        let Some((start, tok)) = self.input.current_token().map(|(s, t)| (s, t.to_string())) else {
            self.completion = None;
            return;
        };
        if tok.starts_with('/') && start == 0 && !self.input.text.contains('\n') && !self.input.text.contains(' ') {
            let q = &tok[1..];
            let mut all: Vec<(String, String, bool)> =
                LOCAL_COMMANDS.iter().map(|(n, d)| (n.to_string(), d.to_string(), true)).collect();
            for c in &self.session.commands {
                if !all.iter().any(|(n, _, _)| *n == c.name) {
                    let mut d = c.description.clone();
                    if !c.hint.is_empty() {
                        d = format!("{d}  ‹{}›", c.hint);
                    }
                    all.push((c.name.clone(), d, false));
                }
            }
            let names: Vec<String> = all.iter().map(|a| a.0.clone()).collect();
            let mut order = fuzzy_filter(q, &names);
            // An exact name always wins: `/compact` must not resolve to
            // `/compact-mode`, nor `/mode` to `/model`, on a score tie.
            if let Some(p) = order.iter().position(|&i| names[i].eq_ignore_ascii_case(q)) {
                let i = order.remove(p);
                order.insert(0, i);
            }
            let items: Vec<_> = order.into_iter().map(|i| all[i].clone()).collect();
            if items.is_empty() {
                self.completion = None;
            } else {
                let sel = match &self.completion {
                    Some(Completion::Slash { sel, .. }) => (*sel).min(items.len() - 1),
                    _ => 0,
                };
                self.completion = Some(Completion::Slash { items, sel });
            }
            return;
        }
        if let Some(q) = tok.strip_prefix('@') {
            let items = self.files.search(q, 8);
            if items.is_empty() {
                self.completion = None;
            } else {
                let sel = match &self.completion {
                    Some(Completion::File { sel, .. }) => (*sel).min(items.len() - 1),
                    _ => 0,
                };
                self.completion = Some(Completion::File { items, sel, start });
            }
            return;
        }
        self.completion = None;
    }

    /// Accept the highlighted completion. Returns true if it should submit right away.
    fn accept_completion(&mut self) -> bool {
        match self.completion.take() {
            Some(Completion::Slash { items, sel }) => {
                if let Some((name, _, local)) = items.get(sel) {
                    let takes_args = matches!(name.as_str(), "export" | "resume" | "theme" | "model" | "effort" | "mode" | "plan")
                        && !local;
                    self.input.set(&format!("/{name}"));
                    if *local && matches!(name.as_str(), "after" | "btw" | "rename") {
                        // Needs a task to be useful: leave room to type it.
                        self.input.insert_char(' ');
                        return false;
                    }
                    if takes_args || !*local {
                        // Agent commands may take arguments: leave a space.
                        let hint = self.session.commands.iter().find(|c| c.name == *name).map(|c| !c.hint.is_empty()).unwrap_or(false);
                        if hint {
                            self.input.insert_char(' ');
                            return false;
                        }
                    }
                    return true;
                }
                false
            }
            Some(Completion::File { items, sel, start }) => {
                if let Some(path) = items.get(sel) {
                    let end = self.input.cursor;
                    let with = if path.contains(' ') { format!("@\"{path}\" ") } else { format!("@{path} ") };
                    self.input.replace_range(start, end, &with);
                }
                false
            }
            None => false,
        }
    }

    // ------------------------------------------------------------------
    // Input events
    // ------------------------------------------------------------------

    pub async fn on_event(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await,
            Event::Paste(s) => {
                if let Some(q) = self.question.as_mut() {
                    q.text.push_str(&s);
                    return;
                }
                if let Some(search) = self.search.as_mut() {
                    search.query.push_str(s.lines().next().unwrap_or(""));
                    self.search_refresh(true);
                    return;
                }
                if !matches!(self.overlay, Overlay::None) {
                    return;
                }
                self.focus = Focus::Prompt;
                self.on_paste(s);
                self.refresh_completion();
            }
            Event::Mouse(m) => self.on_mouse(m).await,
            Event::FocusGained => self.focused = Some(true),
            Event::FocusLost => self.focused = Some(false),
            _ => {}
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.layout.total_lines.saturating_sub(self.layout.view_height);
        let cur = self.scroll as isize;
        let next = (cur + delta).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.follow = next == 0;
    }

    async fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp => {
                if matches!(self.overlay, Overlay::Shortcuts) {
                    self.shortcuts_scroll = self.shortcuts_scroll.saturating_sub(3);
                } else if let Overlay::Viewer { scroll, .. } = &mut self.overlay {
                    *scroll = scroll.saturating_sub(3);
                } else if let Overlay::Picker(p) = &mut self.overlay {
                    p.sel = p.sel.saturating_sub(1);
                    self.picker_preview();
                } else {
                    self.scroll_by(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if matches!(self.overlay, Overlay::Shortcuts) {
                    self.shortcuts_scroll = (self.shortcuts_scroll + 3).min(self.layout.shortcuts_max);
                } else if let Overlay::Viewer { scroll, .. } = &mut self.overlay {
                    *scroll = (*scroll + 3).min(self.layout.viewer_max);
                } else if let Overlay::Picker(p) = &mut self.overlay {
                    let n = p.visible().len();
                    if p.sel + 1 < n {
                        p.sel += 1;
                    }
                    self.picker_preview();
                } else {
                    self.scroll_by(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if matches!(self.overlay, Overlay::Settings { .. }) {
                    if let Some(&(_, i)) = self.layout.settings_rows.iter().find(|(y, _)| *y == m.row) {
                        self.overlay = Overlay::Settings { sel: i };
                        self.settings_activate(i, 1);
                    }
                    return;
                }
                if !matches!(self.overlay, Overlay::None) {
                    return;
                }
                let (x, y) = (m.column, m.row);
                if let Some(b) = self.layout.stop_button {
                    if b.contains((x, y).into()) {
                        self.cancel_turn();
                        return;
                    }
                }
                if let Some(b) = self.layout.narrator_button {
                    if b.contains((x, y).into()) {
                        self.toggle_narrator(true);
                        return;
                    }
                }
                if self.layout.prompt_area.contains((x, y).into()) {
                    self.focus = Focus::Prompt;
                    return;
                }
                if let Some(&(_, anchor)) = self.layout.group_rows.iter().find(|(gy, _)| *gy == y) {
                    if self.layout.scroll_area.contains((x, y).into()) {
                        self.set_group_open(anchor, false);
                        self.selected = Some(anchor);
                        self.focus = Focus::Scrollback;
                        return;
                    }
                }
                if self.layout.scroll_area.contains((x, y).into()) {
                    let hit = self.layout.entry_rows.iter().find(|(y0, y1, _, _, _)| y >= *y0 && y < *y1).cloned();
                    if let Some((y0, _, idx, header_rows, first_row)) = hit {
                        let already = self.selected == Some(idx);
                        self.selected = Some(idx);
                        self.focus = Focus::Scrollback;
                        let row_in_entry = (y - y0) as usize + first_row;
                        if row_in_entry < header_rows || already {
                            self.toggle_fold(idx);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The compact-mode group `idx` belongs to, if compact mode is on.
    pub fn group_of(&self, idx: usize) -> Option<(usize, usize)> {
        if !self.config.ui.compact {
            return None;
        }
        self.transcript.groups().into_iter().find(|(s, e)| idx >= *s && idx < *e)
    }

    /// `idx` anchors a group that is currently collapsed into one line.
    pub fn is_collapsed_group(&self, idx: usize) -> bool {
        self.group_of(idx).is_some_and(|(s, _)| s == idx && !self.transcript.entries[s].group_open)
    }

    fn set_group_open(&mut self, anchor: usize, open: bool) {
        if let Some(e) = self.transcript.entries.get_mut(anchor) {
            e.group_open = open;
        }
        self.ensure_selected_visible = true;
    }

    pub fn toggle_compact(&mut self) {
        self.config.ui.compact = !self.config.ui.compact;
        let on = self.config.ui.compact;
        Config::persist(|c| c.ui.compact = on);
        // Keep the selection on something that's still shown.
        if let Some(sel) = self.selected {
            if let Some((s, _)) = self.group_of(sel).filter(|(s, _)| !self.transcript.entries[*s].group_open) {
                self.selected = Some(s);
            }
            self.ensure_selected_visible = true;
        }
        self.toast(if self.config.ui.compact { "Compact layout on" } else { "Compact layout off" });
    }

    fn toggle_fold(&mut self, idx: usize) {
        if self.is_collapsed_group(idx) {
            self.set_group_open(idx, true);
            return;
        }
        if let Some(e) = self.transcript.entries.get_mut(idx) {
            if e.foldable() {
                e.fold = if e.fold == Fold::Expanded { Fold::Collapsed } else { Fold::Expanded };
                e.touch();
            }
        }
    }

    fn set_fold(&mut self, idx: usize, fold: Fold) {
        if let Some((s, _)) = self.group_of(idx) {
            let open = self.transcript.entries[s].group_open;
            if fold == Fold::Expanded && !open {
                self.set_group_open(s, true);
                return;
            }
            // ← on a member that has nothing left to fold closes its group.
            let e = &self.transcript.entries[idx];
            if fold == Fold::Collapsed && open && (!e.foldable() || e.fold == Fold::Collapsed) {
                self.set_group_open(s, false);
                self.selected = Some(s);
                return;
            }
        }
        if let Some(e) = self.transcript.entries.get_mut(idx) {
            if e.foldable() && e.fold != fold {
                e.fold = fold;
                e.touch();
            }
        }
    }

    fn visible_entries(&self) -> Vec<usize> {
        let hidden: Vec<(usize, usize)> = if self.config.ui.compact {
            self.transcript.groups().into_iter().filter(|(s, _)| !self.transcript.entries[*s].group_open).collect()
        } else {
            vec![]
        };
        self.transcript
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.transcript.show_thinking || !matches!(e.block, Block::Thought { .. }))
            .filter(|(i, _)| !hidden.iter().any(|(s, e)| *i > *s && *i < *e))
            .map(|(i, _)| i)
            .collect()
    }

    /// Plain text of a selection; a collapsed group yields all its members.
    fn selection_text(&self, idx: usize) -> String {
        match self.group_of(idx).filter(|_| self.is_collapsed_group(idx)) {
            Some(g) => self.transcript.group_members(g).iter().map(|e| e.plain_text()).collect::<Vec<_>>().join("\n\n"),
            None => self.transcript.entries[idx].plain_text(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let vis = self.visible_entries();
        if vis.is_empty() {
            return;
        }
        let pos = match self.selected.and_then(|s| vis.iter().position(|&i| i == s)) {
            Some(p) => (p as isize + delta).clamp(0, vis.len() as isize - 1) as usize,
            None => vis.len() - 1,
        };
        self.selected = Some(vis[pos]);
        self.follow = false;
        self.ensure_selected_visible = true;
    }

    fn jump_turn(&mut self, forward: bool) {
        let vis = self.visible_entries();
        let users: Vec<usize> =
            vis.into_iter().filter(|&i| matches!(self.transcript.entries[i].block, Block::User { .. })).collect();
        if users.is_empty() {
            return;
        }
        let cur = self.selected.unwrap_or(usize::MAX);
        let next = if forward {
            users.iter().find(|&&i| i > cur).or(users.last())
        } else {
            users.iter().rev().find(|&&i| i < cur).or(users.first())
        };
        self.selected = next.copied();
        self.follow = false;
        self.ensure_selected_visible = true;
    }

    async fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        // Global: quit arming
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            if !matches!(self.overlay, Overlay::None) {
                self.picker_cancel();
                self.overlay = Overlay::None;
                return;
            }
            if self.completion.is_some() {
                self.completion = None;
                return;
            }
            if self.search.is_some() {
                self.search = None;
                return;
            }
            // Once a stop has been requested, further presses fall through so
            // a stuck agent can't keep you from clearing the prompt or quitting.
            if self.responding && !self.cancelling {
                self.cancel_turn();
                return;
            }
            if self.shell.is_some() {
                self.kill_shell();
                return;
            }
            if !self.input.is_empty() {
                self.input.clear();
                self.attachments.clear();
                self.completion = None;
                return;
            }
            if self.queue_paused {
                self.drop_queue();
                return;
            }
            if self.quit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                self.should_quit = true;
            } else {
                self.quit_armed = Some(Instant::now());
                self.toast("Press Ctrl+C again to quit");
            }
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('d')) && self.input.is_empty() && self.focus == Focus::Prompt {
            // Quit straight away when idle; mid-turn, ask for a second press.
            let busy = self.responding || !self.queue.is_empty();
            if !busy || self.quit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                self.should_quit = true;
            } else {
                self.quit_armed = Some(Instant::now());
                self.toast("Claude is still working: press Ctrl+D again to quit");
            }
            return;
        }
        if k.code == KeyCode::F(1)
            || (ctrl && matches!(k.code, KeyCode::Char('.') | KeyCode::Char('/') | KeyCode::Char('7') | KeyCode::Char('_')))
        {
            if matches!(self.overlay, Overlay::Shortcuts) {
                self.overlay = Overlay::None;
            } else {
                self.picker_cancel(); // undo a live theme preview
                self.shortcuts_scroll = 0;
                self.overlay = Overlay::Shortcuts;
            }
            return;
        }

        // Overlays
        if let Overlay::Settings { sel } = self.overlay {
            let n = self.settings_items().len().max(1);
            let cur = sel.min(n - 1);
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(2) => self.overlay = Overlay::None,
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    self.overlay = Overlay::Settings { sel: if cur == 0 { n - 1 } else { cur - 1 } }
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.overlay = Overlay::Settings { sel: (cur + 1) % n },
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => self.settings_activate(cur, 1),
                KeyCode::Left | KeyCode::Char('h') => self.settings_activate(cur, -1),
                _ => {}
            }
            return;
        }
        match &mut self.overlay {
            Overlay::Shortcuts => {
                let max = self.layout.shortcuts_max;
                let s = &mut self.shortcuts_scroll;
                match k.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => *s = s.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *s = (*s + 1).min(max),
                    KeyCode::PageUp => *s = s.saturating_sub(10),
                    KeyCode::PageDown | KeyCode::Char(' ') => *s = (*s + 10).min(max),
                    KeyCode::Home | KeyCode::Char('g') => *s = 0,
                    KeyCode::End | KeyCode::Char('G') => *s = max,
                    _ => {}
                }
                return;
            }
            Overlay::Viewer { scroll, text, .. } => {
                let max = self.layout.viewer_max;
                let page = self.layout.viewer_page.saturating_sub(2).max(1);
                *scroll = (*scroll).min(max);
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *scroll = (*scroll + 1).min(max),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(page),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = (*scroll + page).min(max),
                    KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
                    KeyCode::End | KeyCode::Char('G') => *scroll = max,
                    KeyCode::Char('y') => {
                        let t = text.clone();
                        self.copy(&t);
                    }
                    _ => {}
                }
                return;
            }
            Overlay::Picker(p) => {
                match k.code {
                    KeyCode::Esc => self.picker_cancel(),
                    KeyCode::Enter => Box::pin(self.picker_choose()).await,
                    KeyCode::Right if p.kind == PickerKind::Timeline => {
                        let vis = p.visible();
                        if let Some(i) = vis.get(p.sel).and_then(|i| p.items.get(*i)).and_then(|it| it.value.parse().ok()) {
                            self.preview_timeline(i);
                        }
                    }
                    KeyCode::Up => {
                        p.sel = p.sel.saturating_sub(1);
                        self.picker_preview();
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        let n = p.visible().len();
                        if p.sel + 1 < n {
                            p.sel += 1;
                        }
                        self.picker_preview();
                    }
                    KeyCode::Backspace => {
                        p.filter.pop();
                        p.sel = 0;
                        self.picker_preview();
                    }
                    KeyCode::Char(c) if !ctrl => {
                        p.filter.push(c);
                        p.sel = 0;
                        self.picker_preview();
                    }
                    _ => {}
                }
                return;
            }
            Overlay::Settings { .. } | Overlay::None => {}
        }

        // Blocking cards
        if !self.permissions.is_empty() {
            let n = self.permissions[0].options.len();
            let settling = self.permissions[0].shown.elapsed() < Duration::from_millis(600);
            if settling && matches!(k.code, KeyCode::Enter | KeyCode::Char(_)) {
                return;
            }
            match k.code {
                KeyCode::Up | KeyCode::BackTab => {
                    let c = &mut self.permissions[0];
                    c.sel = if c.sel == 0 { n.saturating_sub(1) } else { c.sel - 1 };
                }
                KeyCode::Down | KeyCode::Tab => {
                    let c = &mut self.permissions[0];
                    c.sel = if n == 0 { 0 } else { (c.sel + 1) % n };
                }
                KeyCode::Enter => {
                    let sel = self.permissions[0].sel;
                    self.answer_permission(Some(sel));
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let i = c.to_digit(10).unwrap() as usize;
                    if i >= 1 && i <= n {
                        self.answer_permission(Some(i - 1));
                    }
                }
                KeyCode::Char('y') => {
                    let opts = &self.permissions[0].options;
                    let i = opts.iter().position(|o| o.2 == "allow_once").or_else(|| opts.iter().position(|o| o.2 == "allow_always"));
                    if let Some(i) = i {
                        self.answer_permission(Some(i));
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => self.answer_permission(None),
                KeyCode::PageUp => self.scroll_by(10),
                KeyCode::PageDown => self.scroll_by(-10),
                _ => {}
            }
            return;
        }
        if self.question.is_some() {
            self.on_question_key(k);
            return;
        }
        if self.search.is_some() {
            self.on_search_key(k, ctrl, shift);
            return;
        }
        if self.on_extra_global_key(&k, ctrl, alt).await {
            return;
        }

        if k.code == KeyCode::BackTab {
            self.cycle_mode();
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('e')) {
            let any_collapsed = self
                .transcript
                .entries
                .iter()
                .any(|e| matches!(e.block, Block::Thought { .. }) && e.fold == Fold::Collapsed);
            for e in self.transcript.entries.iter_mut() {
                if matches!(e.block, Block::Thought { .. }) {
                    e.fold = if any_collapsed { Fold::Expanded } else { Fold::Collapsed };
                    e.touch();
                }
            }
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('g')) {
            self.toggle_compact();
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('o')) {
            // expand / collapse all tool output (and, in compact mode, every group)
            let groups = if self.config.ui.compact { self.transcript.groups() } else { vec![] };
            let any_collapsed = self.transcript.entries.iter().any(|e| e.foldable() && e.fold == Fold::Collapsed)
                || groups.iter().any(|(s, _)| !self.transcript.entries[*s].group_open);
            for e in self.transcript.entries.iter_mut() {
                if e.foldable() {
                    e.fold = if any_collapsed { Fold::Expanded } else { Fold::Collapsed };
                    e.touch();
                }
            }
            for (s, _) in groups {
                self.transcript.entries[s].group_open = any_collapsed;
            }
            if let Some(sel) = self.selected.filter(|_| !any_collapsed) {
                if let Some((s, _)) = self.group_of(sel) {
                    self.selected = Some(s);
                }
            }
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Char('l')) {
            self.force_redraw = true;
            return;
        }
        if let Some(cmd) = Self::command_shortcut(&k, ctrl, alt) {
            self.completion = None;
            self.run_local(cmd, "").await;
            return;
        }
        // Turn navigation from anywhere: Alt+↑ / Alt+↓
        if alt && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            if self.focus == Focus::Prompt {
                self.focus = Focus::Scrollback;
            }
            self.jump_turn(k.code == KeyCode::Down);
            return;
        }
        match k.code {
            KeyCode::PageUp => {
                let h = self.layout.view_height.max(4) as isize;
                self.scroll_by(h - 2);
                return;
            }
            KeyCode::PageDown => {
                let h = self.layout.view_height.max(4) as isize;
                self.scroll_by(-(h - 2));
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Scrollback => self.on_scrollback_key(k, ctrl, shift).await,
            Focus::Prompt => self.on_prompt_key(k, ctrl, alt, shift).await,
        }
    }

    /// Global key → local slash command. Kept in sync with the shortcut overlay in ui.rs.
    fn command_shortcut(k: &KeyEvent, ctrl: bool, alt: bool) -> Option<&'static str> {
        let KeyCode::Char(c) = k.code else { return None };
        if ctrl && !alt {
            return match c.to_ascii_lowercase() {
                'n' => Some("new"),
                'r' => Some("resume"),
                'p' => Some("model"),
                't' => Some("theme"),
                'y' => Some("copy"),
                _ => None,
            };
        }
        if alt && !ctrl {
            return match c {
                'm' => Some("mode"),
                'e' => Some("effort"),
                'c' => Some("commit"),
                't' => Some("timeline"),
                'z' => Some("undo"),
                'Z' => Some("redo"),
                'x' => Some("export"),
                'k' => Some("context"),
                'h' => Some("thinking"),
                's' => Some("timestamps"),
                'n' => Some("narrator"),
                _ => None,
            };
        }
        None
    }

    async fn on_scrollback_key(&mut self, k: KeyEvent, ctrl: bool, shift: bool) {
        match k.code {
            KeyCode::Char('u') | KeyCode::Char('d') if ctrl => {
                let half = (self.layout.view_height / 2).max(1) as isize;
                self.scroll_by(if k.code == KeyCode::Char('u') { half } else { -half });
                return;
            }
            KeyCode::Char('/') => {
                self.open_search();
                return;
            }
            KeyCode::Char('?') => {
                self.open_palette();
                return;
            }
            KeyCode::Char('m') => {
                self.toggle_raw();
                return;
            }
            _ => {}
        }
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char(' ') | KeyCode::Char('i') => {
                self.focus = Focus::Prompt;
                self.selected = None;
                self.follow = self.scroll == 0;
            }
            KeyCode::Up | KeyCode::Char('k') if !shift => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') if !shift => {
                let vis = self.visible_entries();
                if self.selected == vis.last().copied() {
                    self.scroll = 0;
                    self.follow = true;
                } else {
                    self.move_selection(1);
                }
            }
            KeyCode::Up | KeyCode::Left if shift => self.jump_turn(false),
            KeyCode::Down | KeyCode::Right if shift => self.jump_turn(true),
            KeyCode::Char('H') => self.jump_turn(false),
            KeyCode::Char('L') => self.jump_turn(true),
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(i) = self.selected {
                    self.set_fold(i, Fold::Collapsed);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(i) = self.selected {
                    self.set_fold(i, Fold::Expanded);
                }
            }
            KeyCode::Char('e') => {
                if let Some(i) = self.selected {
                    self.toggle_fold(i);
                }
            }
            KeyCode::Char('o') | KeyCode::Char('r') => match self.selected_path() {
                Some(p) => {
                    let r = if k.code == KeyCode::Char('o') { peek::open(&p) } else { peek::reveal(&p).map(|_| peek::Opened::Opened) };
                    match r {
                        Ok(peek::Opened::Revealed) => self.toast("Runnable file: revealed it instead of running it"),
                        Ok(peek::Opened::Opened) => {}
                        Err(e) => self.toast(format!("Couldn't open {}: {e}", p.display())),
                    }
                }
                None => self.toast("No file on this entry"),
            },
            KeyCode::Char('E') => {
                let any_collapsed = self.transcript.entries.iter().any(|e| e.foldable() && e.fold == Fold::Collapsed);
                for e in self.transcript.entries.iter_mut() {
                    if e.foldable() {
                        e.fold = if any_collapsed { Fold::Expanded } else { Fold::Collapsed };
                        e.touch();
                    }
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                let vis = self.visible_entries();
                self.selected = vis.first().copied();
                self.follow = false;
                self.ensure_selected_visible = true;
            }
            KeyCode::End | KeyCode::Char('G') => {
                let vis = self.visible_entries();
                self.selected = vis.last().copied();
                self.scroll = 0;
                self.follow = true;
            }
            KeyCode::Char('y') => {
                if let Some(i) = self.selected {
                    let t = self.selection_text(i);
                    self.copy(&t);
                }
            }
            KeyCode::Char('Y') => {
                if let Some(i) = self.selected {
                    if let Block::Tool(t) = &self.transcript.entries[i].block {
                        let meta = t
                            .raw_input
                            .get("command")
                            .and_then(|c| c.as_str())
                            .map(str::to_owned)
                            .or_else(|| t.locations.first().cloned())
                            .unwrap_or_else(|| t.title.clone());
                        self.copy(&meta);
                    }
                }
            }
            KeyCode::Enter if self.selected.is_some_and(|i| self.is_collapsed_group(i)) => {
                let i = self.selected.unwrap();
                self.set_group_open(i, true);
            }
            KeyCode::Enter | KeyCode::Char('f') => self.open_viewer(),
            KeyCode::Char(c) if !ctrl && c.is_alphanumeric() => {
                // Typing jumps back to the prompt.
                self.focus = Focus::Prompt;
                self.selected = None;
                self.input.insert_char(c);
                self.refresh_completion();
            }
            _ => {}
        }
    }

    /// The file a selected tool entry touched, if it exists on disk.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let sel = self.selected?;
        if self.is_collapsed_group(sel) {
            return None;
        }
        let e = self.transcript.entries.get(sel)?;
        let Block::Tool(t) = &e.block else { return None };
        let raw = t.locations.first().cloned().or_else(|| {
            ["file_path", "path", "notebook_path"]
                .iter()
                .find_map(|k| t.raw_input.get(*k).and_then(|v| v.as_str()).map(str::to_owned))
        })?;
        let p = peek::resolve(&self.cwd, &raw);
        p.exists().then_some(p)
    }

    fn open_viewer(&mut self) {
        if let Some(i) = self.selected.filter(|i| self.is_collapsed_group(*i)) {
            let text = self.selection_text(i);
            self.overlay = Overlay::Viewer { title: "Tool calls & thinking".into(), text, scroll: 0 };
            return;
        }
        if let Some(i) = self.selected {
            let e = &self.transcript.entries[i];
            let title = match &e.block {
                Block::User { .. } => "Prompt".to_string(),
                Block::Agent { .. } => "Response".to_string(),
                Block::Thought { .. } => "Thinking".to_string(),
                Block::Tool(t) => t.title.clone(),
                Block::Plan(_) => "Plan".to_string(),
                Block::Aside { .. } => "Side question".to_string(),
                _ => "Entry".to_string(),
            };
            let text = e.plain_text();
            if !text.is_empty() {
                self.overlay = Overlay::Viewer { title, text, scroll: 0 };
            }
        }
    }

    async fn on_prompt_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) {
        let width = self.layout.prompt_inner_width.max(10) as usize;
        // Completion navigation
        if self.completion.is_some() {
            match k.code {
                KeyCode::Up | KeyCode::Down => {
                    let down = k.code == KeyCode::Down;
                    match self.completion.as_mut().unwrap() {
                        Completion::Slash { items, sel } => {
                            *sel = if down { (*sel + 1) % items.len() } else { (*sel + items.len() - 1) % items.len() }
                        }
                        Completion::File { items, sel, .. } => {
                            *sel = if down { (*sel + 1) % items.len() } else { (*sel + items.len() - 1) % items.len() }
                        }
                    }
                    return;
                }
                KeyCode::Tab => {
                    self.accept_completion();
                    self.refresh_completion();
                    if matches!(self.completion, Some(Completion::Slash { .. })) {
                        self.completion = None;
                    }
                    return;
                }
                KeyCode::Enter if !shift && !alt => {
                    let is_slash = matches!(self.completion, Some(Completion::Slash { .. }));
                    let submit = self.accept_completion();
                    if is_slash && submit {
                        let text = self.input.take();
                        self.submit(text).await;
                    }
                    self.refresh_completion();
                    return;
                }
                KeyCode::Esc => {
                    self.completion = None;
                    return;
                }
                _ => {}
            }
        }

        match k.code {
            KeyCode::Enter if ctrl && !shift && !alt && self.responding && !self.input.text.trim().is_empty() => {
                self.send_now().await;
            }
            KeyCode::Enter => {
                let newline = shift || alt || ctrl;
                if newline {
                    self.input.insert_char('\n');
                } else if self.input.text[..self.input.cursor].ends_with('\\') {
                    self.input.backspace();
                    self.input.insert_char('\n');
                } else {
                    let text = self.input.take();
                    self.completion = None;
                    self.submit(text).await;
                }
            }
            KeyCode::Char('j') if ctrl => self.input.insert_char('\n'),
            KeyCode::Tab => {
                if self.input.is_empty() {
                    self.focus = Focus::Scrollback;
                    let vis = self.visible_entries();
                    self.selected = vis.last().copied();
                }
            }
            KeyCode::Esc => {
                if self.responding {
                    // Esc never cancels; hint.
                    self.toast("Ctrl+C to stop the response");
                } else if !self.input.is_empty() {
                    if self.esc_armed.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
                        self.input.clear();
                        self.attachments.clear();
                        self.esc_armed = None;
                    } else {
                        self.esc_armed = Some(Instant::now());
                        self.toast("Esc again to clear");
                    }
                } else if self.has_snapshots() {
                    // Esc Esc on an empty prompt: rewind files with the Time Machine.
                    if self.esc_armed.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
                        self.esc_armed = None;
                        self.open_timeline();
                    } else {
                        self.esc_armed = Some(Instant::now());
                        self.toast("Esc again to rewind files (Time Machine)");
                    }
                }
            }
            KeyCode::Backspace if ctrl || alt => self.input.delete_word_left(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Char('h') if ctrl => self.input.backspace(),
            KeyCode::Char('w') if ctrl => self.input.delete_word_left(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Left if ctrl || alt => self.input.word_left(),
            KeyCode::Right if ctrl || alt => self.input.word_right(),
            KeyCode::Char('b') if alt => self.input.word_left(),
            KeyCode::Char('f') if alt => self.input.word_right(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up if self.input.is_empty() && !self.input.in_history() && !self.queue.is_empty() => {
                self.edit_last_queued();
            }
            KeyCode::Up => {
                if self.input.in_history() || !self.input.up(width) {
                    if !self.input.history_prev() && self.input.is_empty() {
                        self.focus = Focus::Scrollback;
                        let vis = self.visible_entries();
                        self.selected = vis.last().copied();
                    }
                }
            }
            KeyCode::Down => {
                if self.input.in_history() || !self.input.down(width) {
                    self.input.history_next();
                }
            }
            KeyCode::Char(c) if !ctrl || alt => {
                self.input.insert_char(c);
            }
            _ => {}
        }
        self.refresh_completion();
    }

    fn on_question_key(&mut self, k: KeyEvent) {
        let Some(q) = self.question.as_mut() else { return };
        let Some(field) = q.fields.get(q.idx).cloned() else {
            self.answer_question("accept");
            return;
        };
        let n = match &field.kind {
            FieldKind::Single(o) | FieldKind::Multi(o, _) => o.len(),
            FieldKind::Bool => 2,
            FieldKind::Text => 0,
        };
        let advance = |q: &mut QuestionCard| {
            q.idx += 1;
            q.sel = 0;
            q.text.clear();
        };
        match k.code {
            KeyCode::Esc => {
                self.answer_question("decline");
                return;
            }
            KeyCode::Up | KeyCode::BackTab if n > 0 => q.sel = if q.sel == 0 { n - 1 } else { q.sel - 1 },
            KeyCode::Down | KeyCode::Tab if n > 0 => q.sel = (q.sel + 1) % n,
            KeyCode::Char(' ') if matches!(field.kind, FieldKind::Multi(..)) => {
                if let Some(FieldKind::Multi(_, checked)) = q.fields.get_mut(q.idx).map(|f| &mut f.kind) {
                    if let Some(c) = checked.get_mut(q.sel) {
                        *c = !*c;
                    }
                }
            }
            KeyCode::Char(c) if matches!(field.kind, FieldKind::Single(_)) && c.is_ascii_digit() => {
                let i = c.to_digit(10).unwrap() as usize;
                if i >= 1 && i <= n {
                    if let FieldKind::Single(opts) = &field.kind {
                        q.answers.insert(field.key.clone(), Value::String(opts[i - 1].0.clone()));
                    }
                    advance(q);
                }
            }
            KeyCode::Enter => {
                match &field.kind {
                    FieldKind::Single(opts) => {
                        if let Some(o) = opts.get(q.sel) {
                            q.answers.insert(field.key.clone(), Value::String(o.0.clone()));
                        }
                    }
                    FieldKind::Multi(opts, _) => {
                        if let Some(FieldKind::Multi(_, checked)) = q.fields.get(q.idx).map(|f| &f.kind) {
                            let mut vals: Vec<Value> = opts
                                .iter()
                                .zip(checked.iter())
                                .filter(|(_, c)| **c)
                                .map(|(o, _)| Value::String(o.0.clone()))
                                .collect();
                            if vals.is_empty() {
                                if let Some(o) = opts.get(q.sel) {
                                    vals.push(Value::String(o.0.clone()));
                                }
                            }
                            q.answers.insert(field.key.clone(), Value::Array(vals));
                        }
                    }
                    FieldKind::Bool => {
                        q.answers.insert(field.key.clone(), Value::Bool(q.sel == 0));
                    }
                    FieldKind::Text => {
                        if !q.text.trim().is_empty() {
                            q.answers.insert(field.key.clone(), Value::String(q.text.trim().to_string()));
                        }
                    }
                }
                advance(q);
            }
            KeyCode::Backspace if matches!(field.kind, FieldKind::Text) => {
                q.text.pop();
            }
            KeyCode::Char(c) if matches!(field.kind, FieldKind::Text) => q.text.push(c),
            _ => {}
        }
        if self.question.as_ref().is_some_and(|q| q.idx >= q.fields.len()) {
            self.answer_question("accept");
        }
    }
}

impl App {
    pub fn take_force_redraw(&mut self) -> bool {
        std::mem::take(&mut self.force_redraw)
    }
}

fn build_question(id: Value, params: &Value) -> Option<QuestionCard> {
    if params.get("mode").and_then(|m| m.as_str()).unwrap_or("form") != "form" {
        return None;
    }
    let message = s(params, "message");
    let schema = params.get("requestedSchema")?;
    let props = schema.get("properties")?.as_object()?;
    let mut fields = Vec::new();
    for (key, p) in props {
        let title = s(p, "title");
        let description = s(p, "description");
        let ty = s(p, "type");
        let parse_opts = |arr: &Vec<Value>| -> Vec<(String, String, String)> {
            arr.iter()
                .map(|o| {
                    let v = o.get("const").and_then(|c| c.as_str()).unwrap_or_default().to_string();
                    let t = o.get("title").and_then(|c| c.as_str()).map(str::to_owned).unwrap_or_else(|| v.clone());
                    (v, t, s(o, "description"))
                })
                .collect()
        };
        let kind = if let Some(arr) = p.get("oneOf").and_then(|o| o.as_array()) {
            FieldKind::Single(parse_opts(arr))
        } else if let Some(arr) = p.get("enum").and_then(|o| o.as_array()) {
            let names = p.get("enumNames").and_then(|n| n.as_array());
            FieldKind::Single(
                arr.iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let v = v.as_str().unwrap_or_default().to_string();
                        let n = names.and_then(|n| n.get(i)).and_then(|n| n.as_str()).unwrap_or(&v).to_string();
                        (v, n, String::new())
                    })
                    .collect(),
            )
        } else if ty == "array" {
            let items = p.get("items");
            let arr = items.and_then(|i| i.get("anyOf").or(i.get("oneOf"))).and_then(|a| a.as_array());
            match arr {
                Some(a) => {
                    let opts = parse_opts(a);
                    let n = opts.len();
                    FieldKind::Multi(opts, vec![false; n])
                }
                None => FieldKind::Text,
            }
        } else if ty == "boolean" {
            FieldKind::Bool
        } else {
            FieldKind::Text
        };
        fields.push(Field { key: key.clone(), title, description, kind });
    }
    if fields.is_empty() {
        return None;
    }
    Some(QuestionCard { id, message, fields, idx: 0, sel: 0, text: String::new(), answers: Map::new() })
}

fn relative_time(t: chrono::DateTime<chrono::Local>) -> String {
    let d = chrono::Local::now().signed_duration_since(t);
    if d.num_minutes() < 1 {
        "just now".into()
    } else if d.num_hours() < 1 {
        format!("{}m ago", d.num_minutes())
    } else if d.num_days() < 1 {
        format!("{}h ago", d.num_hours())
    } else if d.num_days() < 7 {
        format!("{}d ago", d.num_days())
    } else {
        t.format("%b %d").to_string()
    }
}

fn history_path() -> PathBuf {
    home_dir().join("history")
}

fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_path())
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str::<String>(l).ok())
                .collect::<Vec<_>>()
        })
        .map(|mut v| {
            let n = v.len();
            if n > 500 {
                v.drain(..n - 500);
            }
            v
        })
        .unwrap_or_default()
}

fn save_history_line(s: &str) {
    let p = history_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{}", serde_json::to_string(s).unwrap_or_default());
    }
}

fn log_agent_line(line: &str) {
    let dir = home_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("agent.log")) {
        let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Set a session config option (or the legacy model) and wait for the reply.
async fn set_option_request(client: &AcpClient, sid: &str, id: &str, value: &str) -> Result<Value, RpcError> {
    if id == "__model" {
        client.request("session/set_model", json!({"sessionId": sid, "modelId": value})).await.map(|_| Value::Null)
    } else {
        client.request("session/set_config_option", json!({"sessionId": sid, "configId": id, "value": value})).await
    }
}

#[cfg(test)]
mod outgoing_tests {
    use super::Outgoing;

    #[test]
    fn prefixes() {
        let o = Outgoing::parse("_ fix the test");
        assert!(o.haiku && !o.max_effort);
        assert_eq!(o.text, "fix the test");
        let o = Outgoing::parse("+_refactor this");
        assert!(o.haiku && o.max_effort);
        assert_eq!(o.text, "refactor this");
        // Not markers: dunder names, +1, a bare prefix.
        assert!(!Outgoing::parse("__init__.py is empty").haiku);
        assert!(!Outgoing::parse("+1 to that").max_effort);
        assert_eq!(Outgoing::parse("_").text, "_");
    }
}

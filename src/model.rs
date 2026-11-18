//! Conversation model: scrollback entries and session state, plus the
//! reducer that applies ACP `session/update` notifications.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
pub enum ToolContent {
    Text(String),
    Diff { path: String, old: Option<String>, new: String },
    Terminal(String),
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    pub raw_input: Value,
    pub locations: Vec<String>,
    /// First line number reported by the agent (for diff gutters).
    pub line: Option<u64>,
    pub started: Instant,
    pub ended: Option<Duration>,
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub content: String,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
    Success,
}

#[derive(Clone, Debug)]
pub enum Block {
    Welcome,
    User { text: String, queued: bool },
    Agent { text: String },
    Thought { text: String, started: Instant, ended: Option<Duration> },
    Tool(ToolCall),
    Plan(Vec<PlanItem>),
    Notice { level: Level, text: String },
    /// End-of-turn marker for cancelled / failed turns.
    TurnEnd { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    Collapsed,
    Expanded,
}

pub struct Entry {
    pub block: Block,
    pub time: DateTime<Local>,
    pub fold: Fold,
    /// Bumped on every content change (cache key).
    pub rev: u64,
    pub cache: std::cell::RefCell<Option<RenderCache>>,
}

#[allow(dead_code)]
pub struct RenderCache {
    pub width: u16,
    pub rev: u64,
    pub fold: Fold,
    pub selected: bool,
    pub lines: std::rc::Rc<Vec<ratatui::text::Line<'static>>>,
}

impl Entry {
    pub fn new(block: Block) -> Entry {
        let fold = match &block {
            Block::Thought { .. } => Fold::Collapsed,
            Block::Tool(_) => Fold::Collapsed,
            _ => Fold::Expanded,
        };
        Entry { block, time: Local::now(), fold, rev: 0, cache: Default::default() }
    }

    pub fn touch(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    pub fn foldable(&self) -> bool {
        match &self.block {
            Block::Thought { text, .. } => !text.trim().is_empty(),
            Block::Tool(t) => !t.content.is_empty() || !t.raw_input.is_null(),
            Block::Plan(_) => true,
            _ => false,
        }
    }

    pub fn is_live(&self) -> bool {
        match &self.block {
            Block::Thought { ended, .. } => ended.is_none(),
            Block::Tool(t) => matches!(t.status, ToolStatus::Pending | ToolStatus::Running),
            _ => false,
        }
    }

    /// Plain-text content (for copy / export / viewer).
    pub fn plain_text(&self) -> String {
        match &self.block {
            Block::Welcome => String::new(),
            Block::User { text, .. } => text.clone(),
            Block::Agent { text } => text.clone(),
            Block::Thought { text, .. } => text.clone(),
            Block::Tool(t) => {
                let mut s = t.title.clone();
                for c in &t.content {
                    s.push_str("\n\n");
                    match c {
                        ToolContent::Text(x) | ToolContent::Terminal(x) => s.push_str(x),
                        ToolContent::Diff { path, old, new } => {
                            s.push_str(&unified_diff(path, old.as_deref().unwrap_or(""), new))
                        }
                    }
                }
                s
            }
            Block::Plan(items) => items
                .iter()
                .map(|i| format!("[{}] {}", if i.status == "completed" { "x" } else { " " }, i.content))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Notice { text, .. } => text.clone(),
            Block::TurnEnd { reason } => reason.clone(),
        }
    }
}

pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[derive(Clone, Debug, Default)]
pub struct ModeInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub category: String,
    pub current: String,
    /// (value, name, description)
    pub options: Vec<(String, String, String)>,
}

impl ConfigOption {
    pub fn current_name(&self) -> String {
        self.options
            .iter()
            .find(|o| o.0 == self.current)
            .map(|o| o.1.clone())
            .unwrap_or_else(|| self.current.clone())
    }
}

#[derive(Clone, Debug, Default)]
pub struct AgentCommand {
    pub name: String,
    pub description: String,
    pub hint: String,
}

#[derive(Default)]
pub struct Session {
    pub id: Option<String>,
    pub modes: Vec<ModeInfo>,
    pub mode: String,
    pub config: Vec<ConfigOption>,
    /// Legacy `models` field (session/set_model).
    pub models: Vec<ModeInfo>,
    pub model: String,
    pub commands: Vec<AgentCommand>,
    pub title: Option<String>,
    pub used: u64,
    pub size: u64,
    pub cost: Option<f64>,
}

#[allow(dead_code)]
impl Session {
    pub fn option(&self, id: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|o| o.id == id || o.category == id)
    }

    pub fn mode_name(&self) -> String {
        short_mode(&self.mode).to_string()
    }

    pub fn model_label(&self) -> String {
        let mut model = if let Some(o) = self.option("model") {
            match o.options.iter().find(|x| x.0 == o.current) {
                // The "default" entry names the concrete model in its description.
                Some((v, _, desc)) if v == "default" && !desc.is_empty() && desc.len() < 32 => desc.clone(),
                Some((_, name, _)) => name.clone(),
                None => o.current.clone(),
            }
        } else if let Some(m) = self.models.iter().find(|m| m.id == self.model) {
            m.name.clone()
        } else if !self.model.is_empty() {
            self.model.clone()
        } else {
            "Claude".into()
        };
        if !model.to_lowercase().starts_with("claude") && !model.contains('-') {
            model = format!("Claude {model}");
        }
        if self.option("fast").is_some_and(|f| f.current == "on" || f.current == "true") {
            model.push_str(" ⚡");
        }
        match self.option("effort") {
            Some(e) if !e.current.is_empty() && e.current != "default" => {
                format!("{model} ({})", e.current_name().to_lowercase())
            }
            _ => model,
        }
    }

    pub fn apply_new_session(&mut self, res: &Value) {
        if let Some(id) = res.get("sessionId").and_then(|v| v.as_str()) {
            self.id = Some(id.to_string());
        }
        if let Some(m) = res.get("modes") {
            self.apply_modes(m);
        }
        if let Some(m) = res.get("models") {
            self.models = m
                .get("availableModels")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|m| ModeInfo {
                            id: s(m, "modelId"),
                            name: s(m, "name"),
                            description: s(m, "description"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.model = s(m, "currentModelId");
        }
        if let Some(c) = res.get("configOptions") {
            self.apply_config(c);
        }
    }

    fn apply_modes(&mut self, m: &Value) {
        if let Some(a) = m.get("availableModes").and_then(|v| v.as_array()) {
            self.modes = a
                .iter()
                .map(|m| ModeInfo { id: s(m, "id"), name: s(m, "name"), description: s(m, "description") })
                .collect();
        }
        if let Some(c) = m.get("currentModeId").and_then(|v| v.as_str()) {
            self.mode = c.to_string();
        }
    }

    pub fn apply_config(&mut self, c: &Value) {
        let Some(arr) = c.as_array() else { return };
        self.config = arr
            .iter()
            .map(|o| {
                let mut options = Vec::new();
                if let Some(opts) = o.get("options").and_then(|v| v.as_array()) {
                    for op in opts {
                        // Options may be grouped: {group, name, options: [...]}
                        if let Some(inner) = op.get("options").and_then(|v| v.as_array()) {
                            for i in inner {
                                options.push((s(i, "value"), s(i, "name"), s(i, "description")));
                            }
                        } else {
                            options.push((s(op, "value"), s(op, "name"), s(op, "description")));
                        }
                    }
                }
                let current = match o.get("currentValue") {
                    Some(Value::String(v)) => v.clone(),
                    Some(Value::Bool(b)) => if *b { "on".into() } else { "off".into() },
                    Some(v) if !v.is_null() => v.to_string(),
                    _ => String::new(),
                };
                ConfigOption {
                    id: s(o, "id"),
                    name: s(o, "name"),
                    category: s(o, "category"),
                    current,
                    options,
                }
            })
            .collect();
        if let Some(mode) = self.option("mode").cloned() {
            if !mode.current.is_empty() {
                self.mode = mode.current.clone();
            }
            if self.modes.is_empty() {
                self.modes = mode
                    .options
                    .iter()
                    .map(|(v, n, d)| ModeInfo { id: v.clone(), name: n.clone(), description: d.clone() })
                    .collect();
            }
        }
    }
}

pub fn short_mode(id: &str) -> &str {
    match id {
        "bypassPermissions" => "always-approve",
        "default" => "ask",
        "acceptEdits" => "accept-edits",
        "plan" => "plan",
        "auto" => "auto",
        "dontAsk" => "dont-ask",
        other => other,
    }
}

pub fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

pub fn content_text(c: &Value) -> Option<String> {
    match c.get("type").and_then(|t| t.as_str()) {
        Some("text") => c.get("text").and_then(|t| t.as_str()).map(str::to_owned),
        Some("resource_link") => Some(format!("@{}", s(c, "name"))),
        Some("resource") => c
            .get("resource")
            .and_then(|r| r.get("uri"))
            .and_then(|u| u.as_str())
            .map(|u| format!("@{}", u.rsplit('/').next().unwrap_or(u))),
        Some("image") => Some("[image]".into()),
        _ => None,
    }
}

/// What a reducer step did, so the UI can react (titles, notices...).
#[derive(Debug, Default)]
pub struct Effects {
    pub title_changed: bool,
    pub output_chars: usize,
    pub mode_changed: bool,
}

pub struct Transcript {
    pub entries: Vec<Entry>,
    pub tool_index: HashMap<String, usize>,
    /// Index of the plan entry for the current turn.
    pub plan_entry: Option<usize>,
    /// When the agent last produced something (thought timing).
    pub last_activity: Instant,
    pub show_thinking: bool,
    pub expand_thinking: bool,
}

impl Transcript {
    pub fn new() -> Transcript {
        Transcript {
            entries: vec![],
            tool_index: HashMap::new(),
            plan_entry: None,
            last_activity: Instant::now(),
            show_thinking: true,
            expand_thinking: false,
        }
    }

    pub fn push(&mut self, block: Block) -> usize {
        self.close_thoughts();
        let mut e = Entry::new(block);
        if matches!(e.block, Block::Thought { .. }) && self.expand_thinking {
            e.fold = Fold::Expanded;
        }
        self.entries.push(e);
        self.entries.len() - 1
    }

    pub fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.push(Block::Notice { level, text: text.into() });
    }

    pub fn close_thoughts(&mut self) {
        if let Some(e) = self.entries.last_mut() {
            if let Block::Thought { started, ended, .. } = &mut e.block {
                if ended.is_none() {
                    *ended = Some(started.elapsed());
                    e.touch();
                }
            }
        }
    }

    pub fn begin_turn(&mut self) {
        self.last_activity = Instant::now();
        self.plan_entry = None;
    }

    /// Mark any still-running tools as finished (turn ended / cancelled).
    pub fn end_turn(&mut self, cancelled: bool) {
        self.close_thoughts();
        for e in self.entries.iter_mut() {
            if let Block::Tool(t) = &mut e.block {
                if matches!(t.status, ToolStatus::Pending | ToolStatus::Running) {
                    t.status = if cancelled { ToolStatus::Failed } else { ToolStatus::Done };
                    t.ended.get_or_insert(t.started.elapsed());
                    e.rev += 1;
                }
            }
        }
    }

    fn append_text(&mut self, agent: bool, text: &str) -> usize {
        if let Some(e) = self.entries.last_mut() {
            match (&mut e.block, agent) {
                (Block::Agent { text: t }, true) => {
                    t.push_str(text);
                    e.touch();
                    return text.len();
                }
                (Block::Thought { text: t, ended: None, .. }, false) => {
                    t.push_str(text);
                    e.touch();
                    return text.len();
                }
                _ => {}
            }
        }
        if agent {
            if text.trim().is_empty() {
                return 0;
            }
            self.push(Block::Agent { text: text.trim_start_matches('\n').to_string() });
        } else {
            self.push(Block::Thought { text: text.to_string(), started: self.last_activity, ended: None });
        }
        text.len()
    }

    pub fn apply_update(&mut self, session: &mut Session, u: &Value) -> Effects {
        let mut fx = Effects::default();
        let kind = u.get("sessionUpdate").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(kind, "agent_thought_chunk" | "usage_update" | "available_commands_update" | "session_info_update") {
            self.last_activity = Instant::now();
        }
        match kind {
            "agent_message_chunk" => {
                if let Some(t) = u.get("content").and_then(content_text) {
                    fx.output_chars += self.append_text(true, &t);
                }
            }
            "agent_thought_chunk" => {
                if let Some(t) = u.get("content").and_then(content_text) {
                    fx.output_chars += self.append_text(false, &t);
                }
            }
            "user_message_chunk" => {
                if let Some(t) = u.get("content").and_then(content_text) {
                    if let Some(Entry { block: Block::User { text, .. }, rev, .. }) = self.entries.last_mut() {
                        text.push_str(&t);
                        *rev += 1;
                    } else {
                        self.push(Block::User { text: t, queued: false });
                    }
                }
            }
            "tool_call" => {
                let id = s(u, "toolCallId");
                if let Some(&idx) = self.tool_index.get(&id) {
                    self.merge_tool(idx, u);
                } else {
                    let mut t = ToolCall {
                        id: id.clone(),
                        title: String::new(),
                        kind: "other".into(),
                        status: ToolStatus::Pending,
                        content: vec![],
                        raw_input: Value::Null,
                        locations: vec![],
                        line: None,
                        started: Instant::now(),
                        ended: None,
                    };
                    merge_tool_fields(&mut t, u);
                    let idx = self.push(Block::Tool(t));
                    self.tool_index.insert(id, idx);
                }
            }
            "tool_call_update" => {
                let id = s(u, "toolCallId");
                if let Some(&idx) = self.tool_index.get(&id) {
                    self.merge_tool(idx, u);
                }
            }
            "plan" => {
                let items: Vec<PlanItem> = u
                    .get("entries")
                    .and_then(|e| e.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|i| PlanItem { content: s(i, "content"), status: s(i, "status") })
                            .collect()
                    })
                    .unwrap_or_default();
                match self.plan_entry {
                    Some(idx) if idx < self.entries.len() => {
                        self.entries[idx].block = Block::Plan(items);
                        self.entries[idx].touch();
                    }
                    _ => {
                        let idx = self.push(Block::Plan(items));
                        self.plan_entry = Some(idx);
                    }
                }
            }
            "available_commands_update" => {
                session.commands = u
                    .get("availableCommands")
                    .and_then(|c| c.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|c| AgentCommand {
                                name: s(c, "name"),
                                description: s(c, "description"),
                                hint: c
                                    .get("input")
                                    .and_then(|i| i.get("hint"))
                                    .and_then(|h| h.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }
            "current_mode_update" => {
                let m = s(u, "currentModeId");
                if !m.is_empty() {
                    session.mode = m;
                    fx.mode_changed = true;
                }
            }
            "config_option_update" => {
                if let Some(c) = u.get("configOptions") {
                    let before = session.mode.clone();
                    session.apply_config(c);
                    fx.mode_changed = before != session.mode;
                }
            }
            "usage_update" => {
                if let Some(v) = u.get("used").and_then(|v| v.as_u64()) {
                    session.used = v;
                }
                if let Some(v) = u.get("size").and_then(|v| v.as_u64()) {
                    session.size = v;
                }
                if let Some(c) = u.get("cost").and_then(|c| c.get("amount")).and_then(|a| a.as_f64()) {
                    session.cost = Some(c);
                }
            }
            "session_info_update" => {
                if let Some(t) = u.get("title").and_then(|t| t.as_str()) {
                    session.title = Some(t.to_string());
                    fx.title_changed = true;
                }
            }
            "notice" => {
                let sev = s(u, "severity");
                let title = s(u, "title");
                let desc = s(u, "description");
                let text = match (title.is_empty(), desc.is_empty()) {
                    (false, false) => format!("{title}: {desc}"),
                    (false, true) => title,
                    _ => desc,
                };
                if !text.is_empty() {
                    let level = match sev.as_str() {
                        "error" => Level::Error,
                        "warning" => Level::Warn,
                        _ => Level::Info,
                    };
                    self.notice(level, text);
                }
            }
            "compaction_update" => {
                let state = s(u, "state");
                if state == "started" || state == "in_progress" {
                    self.notice(Level::Info, "Compacting conversation…");
                } else if state == "completed" {
                    self.notice(Level::Success, "Conversation compacted");
                }
            }
            _ => {}
        }
        fx
    }

    fn merge_tool(&mut self, idx: usize, u: &Value) {
        if let Some(e) = self.entries.get_mut(idx) {
            if let Block::Tool(t) = &mut e.block {
                merge_tool_fields(t, u);
                e.touch();
            }
        }
    }
}

fn merge_tool_fields(t: &mut ToolCall, u: &Value) {
    if let Some(v) = u.get("title").and_then(|v| v.as_str()) {
        t.title = v.to_string();
    }
    if let Some(v) = u.get("kind").and_then(|v| v.as_str()) {
        t.kind = v.to_string();
    }
    if let Some(v) = u.get("status").and_then(|v| v.as_str()) {
        t.status = match v {
            "pending" => ToolStatus::Pending,
            "in_progress" => ToolStatus::Running,
            "completed" => ToolStatus::Done,
            "failed" => ToolStatus::Failed,
            _ => t.status,
        };
        if matches!(t.status, ToolStatus::Done | ToolStatus::Failed) && t.ended.is_none() {
            t.ended = Some(t.started.elapsed());
        }
    }
    if let Some(v) = u.get("rawInput") {
        if !v.is_null() {
            t.raw_input = v.clone();
        }
    }
    if let Some(locs) = u.get("locations").and_then(|v| v.as_array()) {
        t.locations = locs.iter().map(|l| s(l, "path")).filter(|p| !p.is_empty()).collect();
        if let Some(l) = locs.first().and_then(|l| l.get("line")).and_then(|l| l.as_u64()) {
            t.line = Some(l);
        }
    }
    if let Some(arr) = u.get("content").and_then(|v| v.as_array()) {
        let mut content = Vec::new();
        for c in arr {
            match c.get("type").and_then(|t| t.as_str()) {
                Some("content") => {
                    if let Some(text) = c.get("content").and_then(content_text) {
                        content.push(ToolContent::Text(text));
                    }
                }
                Some("diff") => content.push(ToolContent::Diff {
                    path: s(c, "path"),
                    old: c.get("oldText").and_then(|v| v.as_str()).map(str::to_owned),
                    new: s(c, "newText"),
                }),
                Some("terminal") => content.push(ToolContent::Terminal(String::new())),
                _ => {}
            }
        }
        t.content = content;
    }
    // Claude adapter: live terminal output in _meta.
    if let Some(meta) = u.get("_meta") {
        let out = meta
            .get("terminal_output")
            .and_then(|o| o.get("data").or(Some(o)))
            .and_then(|d| d.as_str());
        if let Some(o) = out {
            t.content.retain(|c| !matches!(c, ToolContent::Terminal(_)));
            t.content.push(ToolContent::Terminal(o.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chunks_merge_and_tools_update() {
        let mut t = Transcript::new();
        let mut s = Session::default();
        t.apply_update(&mut s, &json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}));
        t.apply_update(&mut s, &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hel"}}));
        t.apply_update(&mut s, &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"lo"}}));
        assert_eq!(t.entries.len(), 2);
        assert!(matches!(&t.entries[0].block, Block::Thought { ended: Some(_), .. }));
        assert!(matches!(&t.entries[1].block, Block::Agent { text } if text == "Hello"));
        t.apply_update(&mut s, &json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"ls","kind":"execute","status":"pending"}));
        t.apply_update(&mut s, &json!({"sessionUpdate":"tool_call_update","toolCallId":"a","status":"completed","content":[{"type":"content","content":{"type":"text","text":"out"}}]}));
        let Block::Tool(tc) = &t.entries[2].block else { panic!() };
        assert_eq!(tc.status, ToolStatus::Done);
        assert_eq!(tc.content.len(), 1);
    }
}

//! Turns an [`App`]'s transcript and session state into JSON for the web
//! view, and works out what changed since the last sync.
//!
//! Entries are identified by [`Entry::uid`] and versioned by [`Entry::rev`].
//! A sync sends the entries whose rev moved (`upsert`; unknown uids append),
//! the full uid order only when entries moved or vanished (`order`), and the
//! session status when it changed (`status`).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::app::{App, Conn, FieldKind, LOCAL_COMMANDS};
use crate::model::{Block, Entry, Level, ToolContent, ToolStatus};

/// Largest text sent for one piece of content; longer text keeps its head and tail.
const MAX_TEXT: usize = 40_000;
/// Largest number of diff lines sent for one edit.
const MAX_DIFF_LINES: usize = 600;

/// Local commands that make sense from a browser (no TUI-only pickers or overlays).
const WEB_LOCAL_COMMANDS: &[&str] = &[
    "new", "always-approve", "ask", "accept-edits", "plan", "fast", "after", "btw", "rename", "undo", "redo",
    "context", "session-info", "restart", "commit",
];

/// Wall-clock milliseconds for an `Instant`, from one fixed anchor so the
/// value never jitters between syncs.
pub fn epoch_ms(t: Instant) -> i64 {
    static ANCHOR: OnceLock<(Instant, i64)> = OnceLock::new();
    let (a, a_ms) = *ANCHOR.get_or_init(|| (Instant::now(), chrono::Utc::now().timestamp_millis()));
    match t.checked_duration_since(a) {
        Some(d) => a_ms + d.as_millis() as i64,
        None => a_ms - a.duration_since(t).as_millis() as i64,
    }
}

fn cap(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_string();
    }
    let mut head = MAX_TEXT * 2 / 3;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - MAX_TEXT / 3;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    let cut = s[head..tail].lines().count();
    format!("{}\n… {cut} lines trimmed …\n{}", &s[..head], &s[tail..])
}

fn level(l: Level) -> &'static str {
    match l {
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
        Level::Success => "success",
    }
}

fn status(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Done => "done",
        ToolStatus::Failed => "failed",
    }
}

/// A diff as lines: `[kind, old line no, new line no, text]`, with `kind`
/// one of ` `, `+`, `-`, or `~` (a gap between hunks).
fn diff_lines(old: &str, new: &str, first_line: Option<u64>) -> Value {
    let base = first_line.unwrap_or(1).saturating_sub(1) as usize;
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for (i, group) in diff.grouped_ops(3).iter().enumerate() {
        if i > 0 {
            out.push(json!(["~", null, null, ""]));
        }
        for op in group {
            for c in diff.iter_changes(op) {
                if out.len() >= MAX_DIFF_LINES {
                    out.push(json!(["~", null, null, "… diff trimmed …"]));
                    return Value::Array(out);
                }
                let kind = match c.tag() {
                    similar::ChangeTag::Equal => " ",
                    similar::ChangeTag::Insert => "+",
                    similar::ChangeTag::Delete => "-",
                };
                let text = c.value().trim_end_matches(['\n', '\r']);
                out.push(json!([
                    kind,
                    c.old_index().map(|n| n + 1 + base),
                    c.new_index().map(|n| n + 1 + base),
                    text,
                ]));
            }
        }
    }
    Value::Array(out)
}

fn input_summary(raw: &Value) -> Value {
    if raw.is_null() {
        return Value::Null;
    }
    let mut full = serde_json::to_string_pretty(raw).unwrap_or_default();
    if full.len() > 4000 {
        let mut end = 4000;
        while !full.is_char_boundary(end) {
            end -= 1;
        }
        full.truncate(end);
        full.push_str("\n…");
    }
    let pick = |k: &str| raw.get(k).and_then(|v| v.as_str()).map(cap);
    json!({
        "command": pick("command"),
        "description": pick("description"),
        "path": pick("file_path").or_else(|| pick("path")),
        "pattern": pick("pattern"),
        "url": pick("url"),
        "query": pick("query"),
        "full": full,
    })
}

pub fn entry_json(e: &Entry) -> Value {
    let at = e.time.timestamp_millis();
    let mut v = match &e.block {
        Block::Welcome => json!({"k": "welcome"}),
        Block::User { text, queued, tag } => json!({"k": "user", "text": cap(text), "queued": queued, "tag": tag}),
        Block::Agent { text } => json!({"k": "agent", "text": cap(text)}),
        Block::Thought { text, started, ended } => json!({
            "k": "thought",
            "text": cap(text),
            "startedAt": epoch_ms(*started),
            "ms": ended.map(|d| d.as_millis() as u64),
        }),
        Block::Tool(t) => {
            let content: Vec<Value> = t
                .content
                .iter()
                .map(|c| match c {
                    ToolContent::Text(x) => json!({"k": "text", "text": cap(x)}),
                    ToolContent::Terminal(x) => json!({"k": "terminal", "text": cap(x)}),
                    ToolContent::Diff { path, old, new } => json!({
                        "k": "diff",
                        "path": path,
                        "created": old.is_none(),
                        "lines": diff_lines(old.as_deref().unwrap_or(""), new, t.line),
                    }),
                })
                .collect();
            json!({
                "k": "tool",
                "id": t.id,
                "title": t.title,
                "kind": t.kind,
                "status": status(t.status),
                "content": content,
                "input": input_summary(&t.raw_input),
                "locations": t.locations,
                "startedAt": epoch_ms(t.started),
                "ms": t.ended.map(|d| d.as_millis() as u64),
            })
        }
        Block::Plan(items) => json!({
            "k": "plan",
            "items": items.iter().map(|i| json!({"content": i.content, "status": i.status})).collect::<Vec<_>>(),
        }),
        Block::Notice { level: l, text } => json!({"k": "notice", "level": level(*l), "text": cap(text)}),
        Block::TurnEnd { reason } => json!({"k": "turnEnd", "reason": reason}),
        Block::Aside { question, answer, started, ended, failed } => json!({
            "k": "aside",
            "question": question,
            "answer": cap(answer),
            "startedAt": epoch_ms(*started),
            "ms": ended.map(|d| d.as_millis() as u64),
            "failed": failed,
        }),
    };
    v["uid"] = json!(e.uid);
    v["at"] = json!(at);
    v
}

pub fn status_json(app: &App) -> Value {
    let s = &app.session;
    let (conn, conn_msg) = match &app.conn {
        Conn::Starting(m) => ("starting", m.clone()),
        Conn::Ready => ("ready", String::new()),
        Conn::Failed(m) => ("failed", m.clone()),
        Conn::Exited => ("exited", String::new()),
    };
    let permissions: Vec<Value> = app
        .permissions
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "detail": p.detail,
                "options": p.options.iter().map(|(id, name, kind)| json!({"id": id, "name": name, "kind": kind})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let question = app.question.as_ref().map(|q| {
        json!({
            "id": q.id,
            "message": q.message,
            "fields": q.fields.iter().map(|f| {
                let (kind, options) = match &f.kind {
                    FieldKind::Single(o) => ("single", o.clone()),
                    FieldKind::Multi(o, _) => ("multi", o.clone()),
                    FieldKind::Text => ("text", vec![]),
                    FieldKind::Bool => ("bool", vec![]),
                };
                json!({
                    "key": f.key,
                    "title": f.title,
                    "description": f.description,
                    "kind": kind,
                    "options": options.iter().map(|(v, n, d)| json!({"value": v, "name": n, "description": d})).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    });
    let options: Vec<Value> = s
        .config
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "name": o.name,
                "category": o.category,
                "current": o.current,
                "options": o.options.iter().map(|(v, n, d)| json!({"value": v, "name": n, "description": d})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut commands: Vec<Value> = WEB_LOCAL_COMMANDS
        .iter()
        .filter_map(|n| LOCAL_COMMANDS.iter().find(|(c, _)| c == n))
        .map(|(n, d)| json!({"name": n, "description": d, "hint": ""}))
        .collect();
    for c in &s.commands {
        if !WEB_LOCAL_COMMANDS.contains(&c.name.as_str()) {
            commands.push(json!({"name": c.name, "description": c.description, "hint": c.hint}));
        }
    }
    let queue: Vec<String> = app.queue.iter().map(|(o, _)| o.raw.clone()).collect();
    json!({
        "sessionId": s.id,
        "title": s.title,
        "cwd": app.cwd.to_string_lossy(),
        "cwdLabel": crate::ui::display_path(&app.cwd),
        "git": app.git_label,
        "conn": conn,
        "connMessage": conn_msg,
        "agent": app.agent_name,
        "responding": app.responding,
        "cancelling": app.cancelling,
        "turnStartedAt": app.turn_started.map(epoch_ms),
        "turnTag": app.turn_tag,
        "mode": s.mode,
        "modes": s.modes.iter().map(|m| json!({"id": m.id, "name": m.name, "description": m.description})).collect::<Vec<_>>(),
        "model": s.model,
        "models": s.models.iter().map(|m| json!({"id": m.id, "name": m.name, "description": m.description})).collect::<Vec<_>>(),
        "modelLabel": s.model_label(),
        "options": options,
        "used": s.used,
        "size": s.size,
        "cost": s.cost,
        "queue": queue,
        "queuePaused": app.queue_paused,
        "permissions": permissions,
        "question": question,
        "commands": commands,
        "busy": app.timeline_busy,
    })
}

fn hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// One host's link to the hub, and what the hub has already been sent.
pub struct WebSync {
    pub out: mpsc::UnboundedSender<String>,
    /// uid -> (rev, hash of the JSON sent)
    sent: HashMap<u64, (u64, u64)>,
    order: Vec<u64>,
    status: u64,
    fresh: bool,
}

impl WebSync {
    pub fn new(out: mpsc::UnboundedSender<String>) -> WebSync {
        WebSync { out, sent: HashMap::new(), order: Vec::new(), status: 0, fresh: true }
    }

    fn send(&self, v: Value) -> bool {
        self.out.send(v.to_string()).is_ok()
    }

    /// Push whatever changed. Returns false once the link is gone.
    pub fn sync(&mut self, app: &App) -> bool {
        let entries = &app.transcript.entries;
        let status = status_json(app);
        let status_s = status.to_string();
        if self.fresh {
            self.fresh = false;
            let all: Vec<Value> = entries.iter().map(entry_json).collect();
            self.sent = entries
                .iter()
                .zip(&all)
                .map(|(e, j)| (e.uid, (e.rev, hash(&j.to_string()))))
                .collect();
            self.order = entries.iter().map(|e| e.uid).collect();
            self.status = hash(&status_s);
            return self.send(json!({"t": "snapshot", "status": status, "entries": all}));
        }

        let order: Vec<u64> = entries.iter().map(|e| e.uid).collect();
        let appended_only = order.len() >= self.order.len() && order[..self.order.len()] == self.order[..];
        if !appended_only {
            self.sent.retain(|uid, _| order.contains(uid));
            if !self.send(json!({"t": "order", "uids": order})) {
                return false;
            }
        }
        // Entries whose rev moved, plus a content check on the live tail in
        // case something changed without bumping rev.
        let tail_from = entries.len().saturating_sub(3);
        let mut changed = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let prev = self.sent.get(&e.uid).copied();
            let recheck = i >= tail_from || e.is_live();
            if prev.is_some_and(|(rev, _)| rev == e.rev) && !recheck {
                continue;
            }
            let j = entry_json(e);
            let h = hash(&j.to_string());
            if prev.is_some_and(|(_, ph)| ph == h) {
                self.sent.insert(e.uid, (e.rev, h));
                continue;
            }
            self.sent.insert(e.uid, (e.rev, h));
            changed.push(j);
        }
        self.order = order;
        if !changed.is_empty() && !self.send(json!({"t": "upsert", "entries": changed})) {
            return false;
        }
        let sh = hash(&status_s);
        if sh != self.status {
            self.status = sh;
            if !self.send(json!({"t": "status", "status": status})) {
                return false;
            }
        }
        true
    }
}

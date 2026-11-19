//! `blush web serve`: the hub. Serves the web app, keeps a cached copy of
//! every host's state, and relays between hosts and the browsers attached to
//! them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::acp::{self, AcpClient};
use crate::config::Config;
use crate::model::s;
use crate::theme::{THEME_NAMES, Theme};

use super::{ServerInfo, assets};

const COOKIE: &str = "blush_token";

struct Host {
    /// None while a session the web opened is still starting.
    tx: Option<mpsc::UnboundedSender<String>>,
    kind: String,
    pid: u32,
    status: Value,
    entries: HashMap<u64, Value>,
    order: Vec<u64>,
    /// What to call it while it starts.
    label: Option<String>,
    cwd_hint: Option<String>,
    /// Last summary broadcast, to skip no-op list updates.
    summary: String,
}

struct Client {
    tx: mpsc::UnboundedSender<String>,
    attached: Option<String>,
}

#[derive(Default)]
struct HubState {
    hosts: HashMap<String, Host>,
    clients: HashMap<u64, Client>,
    next_client: u64,
}

pub struct Hub {
    token: String,
    config: Config,
    state: Mutex<HubState>,
    shutdown: Notify,
    /// An agent process for listing past sessions, started on first use.
    lister: tokio::sync::Mutex<Option<Arc<AcpClient>>>,
}

type Shared = Arc<Hub>;

fn same(a: &str, b: &str) -> bool {
    // Constant-time compare.
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|c| {
        c.split(';').find_map(|kv| kv.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_owned))
    })
}

impl Hub {
    fn authed(&self, headers: &HeaderMap, query: Option<&str>) -> bool {
        let header_tok = headers.get("x-blush-token").and_then(|v| v.to_str().ok()).map(str::to_owned);
        [cookie_token(headers), header_tok, query.map(str::to_owned)]
            .into_iter()
            .flatten()
            .any(|t| same(&t, &self.token))
    }

    fn summary(id: &str, h: &Host) -> Value {
        let st = &h.status;
        let attention = st.get("permissions").and_then(|p| p.as_array()).map_or(0, |a| a.len())
            + usize::from(st.get("question").is_some_and(|q| !q.is_null()));
        json!({
            "hostId": id,
            "kind": h.kind,
            "pid": h.pid,
            "pending": h.tx.is_none(),
            "sessionId": st.get("sessionId"),
            "title": st.get("title").filter(|t| !t.is_null()).cloned().or_else(|| h.label.clone().map(Value::from)),
            "cwd": st.get("cwd").cloned().or_else(|| h.cwd_hint.clone().map(Value::from)),
            "cwdLabel": st.get("cwdLabel").cloned().or_else(|| h.cwd_hint.as_deref().map(|c| Value::from(crate::ui::display_path(std::path::Path::new(c))))),
            "git": st.get("git"),
            "conn": st.get("conn").cloned().unwrap_or_else(|| json!("starting")),
            "responding": st.get("responding").cloned().unwrap_or(json!(false)),
            "attention": attention,
            "modelLabel": st.get("modelLabel"),
            "mode": st.get("mode"),
        })
    }

    fn hosts_msg(st: &HubState) -> String {
        let mut list: Vec<(&String, &Host)> = st.hosts.iter().collect();
        list.sort_by(|a, b| a.1.kind.cmp(&b.1.kind).then(a.0.cmp(b.0)));
        let hosts: Vec<Value> = list.into_iter().map(|(id, h)| Self::summary(id, h)).collect();
        json!({"t": "hosts", "hosts": hosts}).to_string()
    }

    fn broadcast_hosts(st: &mut HubState) {
        let msg = Self::hosts_msg(st);
        for c in st.clients.values() {
            let _ = c.tx.send(msg.clone());
        }
    }

    /// Re-broadcast the host list if this host's summary changed.
    fn maybe_broadcast(st: &mut HubState, id: &str) {
        let Some(h) = st.hosts.get_mut(id) else { return };
        let sum = Self::summary(id, h).to_string();
        if sum != h.summary {
            h.summary = sum;
            Self::broadcast_hosts(st);
        }
    }

    fn to_viewers(st: &HubState, id: &str, msg: &str) {
        for c in st.clients.values() {
            if c.attached.as_deref() == Some(id) {
                let _ = c.tx.send(msg.to_string());
            }
        }
    }

    fn snapshot_msg(id: &str, h: &Host) -> String {
        if h.status.is_null() {
            return json!({"t": "snapshot", "hostId": id, "pending": true, "status": null, "entries": []}).to_string();
        }
        let entries: Vec<&Value> = h.order.iter().filter_map(|u| h.entries.get(u)).collect();
        json!({"t": "snapshot", "hostId": id, "status": h.status, "entries": entries}).to_string()
    }

    fn on_host_msg(&self, id: &str, mut v: Value) {
        let mut st = self.state.lock().unwrap();
        let Some(h) = st.hosts.get_mut(id) else { return };
        let uid = |e: &Value| e.get("uid").and_then(|u| u.as_u64());
        match s(&v, "t").as_str() {
            "snapshot" => {
                h.status = v.get("status").cloned().unwrap_or(Value::Null);
                h.entries.clear();
                h.order.clear();
                for e in v.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
                    if let Some(u) = uid(e) {
                        h.order.push(u);
                        h.entries.insert(u, e.clone());
                    }
                }
            }
            "upsert" => {
                for e in v.get("entries").and_then(|e| e.as_array()).into_iter().flatten() {
                    // An `order` may already have placed a new uid.
                    if let Some(u) = uid(e) {
                        if h.entries.insert(u, e.clone()).is_none() && !h.order.contains(&u) {
                            h.order.push(u);
                        }
                    }
                }
            }
            "order" => {
                h.order = v
                    .get("uids")
                    .and_then(|u| u.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
                    .unwrap_or_default();
                let keep: std::collections::HashSet<u64> = h.order.iter().copied().collect();
                h.entries.retain(|u, _| keep.contains(u));
            }
            "status" => h.status = v.get("status").cloned().unwrap_or(Value::Null),
            _ => return,
        }
        v["hostId"] = json!(id);
        let msg = v.to_string();
        Self::to_viewers(&st, id, &msg);
        Self::maybe_broadcast(&mut st, id);
    }

    fn register_host(&self, id: &str, kind: &str, pid: u32, tx: mpsc::UnboundedSender<String>) {
        let mut st = self.state.lock().unwrap();
        let h = st.hosts.entry(id.to_string()).or_insert_with(|| Host {
            tx: None,
            kind: kind.to_string(),
            pid,
            status: Value::Null,
            entries: HashMap::new(),
            order: vec![],
            label: None,
            cwd_hint: None,
            summary: String::new(),
        });
        h.tx = Some(tx);
        h.kind = kind.to_string();
        h.pid = pid;
        Self::broadcast_hosts(&mut st);
    }

    fn host_gone(&self, id: &str, reason: &str) {
        let mut st = self.state.lock().unwrap();
        if st.hosts.remove(id).is_none() {
            return;
        }
        let msg = json!({"t": "hostGone", "hostId": id, "reason": reason}).to_string();
        Self::to_viewers(&st, id, &msg);
        for c in st.clients.values_mut() {
            if c.attached.as_deref() == Some(id) {
                c.attached = None;
            }
        }
        Self::broadcast_hosts(&mut st);
    }

    /// Open a session in a new headless host. Returns its host id (or the id
    /// of the host already running that session).
    fn spawn_host(self: &Arc<Self>, cwd: &str, resume: Option<&str>, label: Option<String>) -> Result<String, String> {
        if let Some(sid) = resume {
            let st = self.state.lock().unwrap();
            if let Some((id, _)) = st.hosts.iter().find(|(_, h)| h.status.get("sessionId").and_then(|v| v.as_str()) == Some(sid)) {
                return Ok(id.clone());
            }
        }
        let dir = PathBuf::from(cwd.trim());
        if cwd.trim().is_empty() || !dir.is_dir() {
            return Err(format!("No such directory: {cwd}"));
        }
        let id = super::host::new_host_id();
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let log = super::log_path();
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let out = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(["web", "host", "--id", &id, "--cwd"]).arg(&dir);
        if let Some(sid) = resume {
            cmd.args(["--resume", sid]);
        }
        cmd.stdin(std::process::Stdio::null()).stdout(out).stderr(err);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| format!("couldn't start a session: {e}"))?;
        {
            let mut st = self.state.lock().unwrap();
            st.hosts.insert(
                id.clone(),
                Host {
                    tx: None,
                    kind: "web".into(),
                    pid: child.id().unwrap_or(0),
                    status: Value::Null,
                    entries: HashMap::new(),
                    order: vec![],
                    label,
                    cwd_hint: Some(dir.to_string_lossy().to_string()),
                    summary: String::new(),
                },
            );
            Self::broadcast_hosts(&mut st);
        }
        let hub = self.clone();
        let hid = id.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let reason = match status {
                Ok(s) if s.success() => "The session ended".to_string(),
                Ok(s) => format!("The session process exited ({s}); see {}", super::log_path().display()),
                Err(e) => format!("The session process failed: {e}"),
            };
            hub.host_gone(&hid, &reason);
        });
        Ok(id)
    }

    async fn lister(&self) -> Result<Arc<AcpClient>, String> {
        let mut guard = self.lister.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        if crate::launcher::needs_install(&self.config) {
            crate::launcher::install_adapter(|_| {}).await.map_err(|e| e.to_string())?;
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let spec = crate::launcher::launch_spec(&self.config, &home).map_err(|e| e.to_string())?;
        let client = AcpClient::spawn(&spec, |_| {}).await.map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(60), client.request("initialize", acp::initialize_params()))
            .await
            .map_err(|_| "the agent didn't start in time".to_string())?
            .map_err(|e| e.to_string())?;
        *guard = Some(client.clone());
        Ok(client)
    }

    async fn history(&self, cursor: Option<String>) -> Value {
        let mut params = json!({});
        if let Some(c) = cursor.filter(|c| !c.is_empty()) {
            params["cursor"] = json!(c);
        }
        let mut res = Err(String::new());
        for _ in 0..2 {
            let client = match self.lister().await {
                Ok(c) => c,
                Err(e) => {
                    res = Err(e);
                    break;
                }
            };
            match client.request("session/list", params.clone()).await {
                Ok(v) => {
                    res = Ok(v);
                    break;
                }
                Err(e) => {
                    res = Err(e.to_string());
                    // The agent may have died: start a fresh one next time.
                    if let Some(c) = self.lister.lock().await.take() {
                        c.shutdown().await;
                    }
                }
            }
        }
        match res {
            Ok(v) => {
                let sessions: Vec<Value> = v
                    .get("sessions")
                    .and_then(|s| s.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|x| !s(x, "cwd").contains("usage-probe"))
                    .map(|x| {
                        let sid = s(x, "sessionId");
                        let cwd = s(x, "cwd");
                        let title = crate::app::local_title(&sid).unwrap_or_else(|| s(x, "title"));
                        json!({
                            "sessionId": sid,
                            "cwd": cwd,
                            "cwdLabel": crate::ui::display_path(std::path::Path::new(&cwd)),
                            "title": title,
                            "updatedAt": x.get("updatedAt"),
                        })
                    })
                    .collect();
                json!({"t": "history", "sessions": sessions, "nextCursor": v.get("nextCursor")})
            }
            Err(e) => json!({"t": "history", "sessions": [], "error": e}),
        }
    }

    async fn on_client_msg(self: &Arc<Self>, cid: u64, v: Value) {
        let reply = |msg: Value| {
            if let Some(c) = self.state.lock().unwrap().clients.get(&cid) {
                let _ = c.tx.send(msg.to_string());
            }
        };
        let req = v.get("req").cloned().unwrap_or(Value::Null);
        match s(&v, "t").as_str() {
            "attach" => {
                let id = s(&v, "hostId");
                let mut st = self.state.lock().unwrap();
                let msg = match st.hosts.get(&id) {
                    Some(h) => Self::snapshot_msg(&id, h),
                    None => json!({"t": "hostGone", "hostId": id, "reason": "That session isn't running anymore"}).to_string(),
                };
                let exists = st.hosts.contains_key(&id);
                if let Some(c) = st.clients.get_mut(&cid) {
                    c.attached = exists.then_some(id);
                    let _ = c.tx.send(msg);
                }
            }
            "detach" => {
                if let Some(c) = self.state.lock().unwrap().clients.get_mut(&cid) {
                    c.attached = None;
                }
            }
            "cmd" => {
                let id = s(&v, "hostId");
                let cmd = v.get("cmd").cloned().unwrap_or(Value::Null);
                let st = self.state.lock().unwrap();
                match st.hosts.get(&id).and_then(|h| h.tx.as_ref()) {
                    Some(tx) => {
                        let _ = tx.send(cmd.to_string());
                    }
                    None => {
                        if let Some(c) = st.clients.get(&cid) {
                            let _ = c.tx.send(json!({"t": "error", "message": "That session isn't ready yet"}).to_string());
                        }
                    }
                }
            }
            "history" => {
                let hub = self.clone();
                let cursor = v.get("cursor").and_then(|c| c.as_str()).map(str::to_owned);
                tokio::spawn(async move {
                    let mut msg = hub.history(cursor).await;
                    msg["req"] = req;
                    if let Some(c) = hub.state.lock().unwrap().clients.get(&cid) {
                        let _ = c.tx.send(msg.to_string());
                    }
                });
            }
            "open" | "new" => {
                let cwd = s(&v, "cwd");
                let sid = (s(&v, "t") == "open").then(|| s(&v, "sessionId")).filter(|x| !x.is_empty());
                let label = v.get("title").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(str::to_owned);
                match self.spawn_host(&cwd, sid.as_deref(), label) {
                    Ok(id) => reply(json!({"t": "opened", "req": req, "hostId": id})),
                    Err(e) => reply(json!({"t": "error", "req": req, "message": e})),
                }
            }
            "close" => {
                let id = s(&v, "hostId");
                let st = self.state.lock().unwrap();
                match st.hosts.get(&id) {
                    Some(h) if h.kind == "web" => {
                        if let Some(tx) = &h.tx {
                            let _ = tx.send(json!({"t": "quit"}).to_string());
                        }
                    }
                    Some(_) => {
                        if let Some(c) = st.clients.get(&cid) {
                            let _ = c.tx.send(
                                json!({"t": "error", "message": "That session belongs to a terminal; quit it there"}).to_string(),
                            );
                        }
                    }
                    None => {}
                }
            }
            "ping" => reply(json!({"t": "pong"})),
            _ => {}
        }
    }
}

/// Every theme's colors as CSS hex strings (the terminal theme borrows
/// blush's, since a browser can't see the terminal's palette).
fn palettes() -> Value {
    use ratatui::style::Color;
    fn hex(c: Color, fallback: Color) -> String {
        match (c, fallback) {
            (Color::Rgb(r, g, b), _) | (_, Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#888888".into(),
        }
    }
    let base = Theme::blush();
    let mut out = serde_json::Map::new();
    for (name, desc) in THEME_NAMES {
        let Some(t) = Theme::by_name(name) else { continue };
        let b = &base;
        let md = |c: Option<Color>, d: Color, bd: Color| hex(c.unwrap_or(d), bd);
        out.insert(
            name.to_string(),
            json!({
                "description": desc,
                "light": t.is_light,
                "bg": hex(t.bg, b.bg),
                "bgRaised": hex(t.bg_raised, b.bg_raised),
                "bgHover": hex(t.bg_hover, b.bg_hover),
                "text": hex(t.text, b.text),
                "textSecondary": hex(t.text_secondary, b.text_secondary),
                "grayDim": hex(t.gray_dim, b.gray_dim),
                "gray": hex(t.gray, b.gray),
                "grayBright": hex(t.gray_bright, b.gray_bright),
                "accent": hex(t.accent, b.accent),
                "accentThinking": hex(t.accent_thinking, b.accent_thinking),
                "success": hex(t.success, b.success),
                "error": hex(t.error, b.error),
                "warning": hex(t.warning, b.warning),
                "info": hex(t.info, b.info),
                "path": hex(t.path, b.path),
                "command": hex(t.command, b.command),
                "link": hex(t.link, b.link),
                "model": hex(t.model, b.model),
                "plan": hex(t.plan, b.plan),
                "border": hex(t.border, b.border),
                "borderActive": hex(t.border_active, b.border_active),
                "diffInsBg": hex(t.diff_ins_bg, b.diff_ins_bg),
                "diffInsFg": hex(t.diff_ins_fg, b.diff_ins_fg),
                "diffDelBg": hex(t.diff_del_bg, b.diff_del_bg),
                "diffDelFg": hex(t.diff_del_fg, b.diff_del_fg),
                "h1": hex(t.h1, b.h1),
                "h2": hex(t.h2, b.h2),
                "h3": hex(t.h3, b.h3),
                "h4": hex(t.h4, b.h4),
                "codeInline": hex(t.code_inline, b.code_inline),
                "mdText": hex(t.md_text, b.md_text),
                "mdBullet": md(t.md.bullet, t.gray, b.gray),
                "mdStrong": md(t.md.strong, t.text, b.text),
                "mdQuoteBar": md(t.md.quote_bar, t.gray_dim, b.gray_dim),
                "mdQuoteText": md(t.md.quote_text, t.gray_bright, b.gray_bright),
                "mdRule": md(t.md.rule, t.gray_dim, b.gray_dim),
            }),
        );
    }
    Value::Object(out)
}

#[derive(serde::Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "pid": std::process::id(), "version": env!("BLUSH_VERSION")}))
}

async fn status(State(hub): State<Shared>, headers: HeaderMap) -> Response {
    if !hub.authed(&headers, None) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let n = hub.state.lock().unwrap().hosts.len();
    Json(json!({"sessions": n})).into_response()
}

async fn me(State(hub): State<Shared>, headers: HeaderMap) -> Response {
    if hub.authed(&headers, None) {
        Json(json!({"ok": true})).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({"ok": false}))).into_response()
    }
}

fn cookie_header(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000")).unwrap()
}

async fn login(State(hub): State<Shared>, Json(body): Json<Value>) -> Response {
    let t = s(&body, "token");
    let t = t.trim();
    // Accept a pasted link as well as the bare token.
    let t = t.rsplit("token=").next().unwrap_or(t).split('&').next().unwrap_or(t).trim();
    if same(t, &hub.token) {
        let mut res = Json(json!({"ok": true})).into_response();
        res.headers_mut().insert(header::SET_COOKIE, cookie_header(&hub.token));
        res
    } else {
        tokio::time::sleep(Duration::from_millis(400)).await;
        (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "That token doesn't match"}))).into_response()
    }
}

async fn logout() -> Response {
    let mut res = Json(json!({"ok": true})).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_static("blush_token=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"));
    res
}

async fn shutdown(State(hub): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if !is_loopback(&addr) || !hub.authed(&headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    hub.shutdown.notify_one();
    Json(json!({"ok": true})).into_response()
}

async fn host_ws(
    State(hub): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_loopback(&addr) || !hub.authed(&headers, q.token.as_deref()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(256 << 20).on_upgrade(move |socket| host_conn(hub, socket))
}

async fn host_conn(hub: Shared, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    // The first message says who this is.
    let hello = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).unwrap_or(Value::Null),
        _ => return,
    };
    let id = s(&hello, "hostId");
    if s(&hello, "t") != "hello" || id.is_empty() {
        return;
    }
    let kind = match s(&hello, "kind").as_str() {
        "web" => "web",
        _ => "tui",
    };
    let pid = hello.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    hub.register_host(&id, kind, pid, tx);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(t) = msg {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                hub.on_host_msg(&id, v);
            }
        }
    }
    writer.abort();
    hub.host_gone(&id, if kind == "tui" { "The terminal session closed" } else { "The session ended" });
}

async fn browser_ws(State(hub): State<Shared>, Query(q): Query<TokenQuery>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !hub.authed(&headers, q.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Refuse pages from other sites (cross-site WebSocket hijacking).
    if let (Some(origin), Some(host)) = (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        let origin = origin.to_str().unwrap_or("");
        let host = host.to_str().unwrap_or("");
        let bare = origin.split_once("://").map(|(_, rest)| rest).unwrap_or(origin);
        if bare != host {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.max_message_size(16 << 20).on_upgrade(move |socket| browser_conn(hub, socket))
}

async fn browser_conn(hub: Shared, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let cid = {
        let mut st = hub.state.lock().unwrap();
        st.next_client += 1;
        let cid = st.next_client;
        let hello = json!({
            "t": "hello",
            "version": env!("BLUSH_VERSION"),
            "theme": hub.config.ui.theme,
            "themes": palettes(),
            "host": std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default(),
        });
        let _ = tx.send(hello.to_string());
        let _ = tx.send(Hub::hosts_msg(&st));
        st.clients.insert(cid, Client { tx, attached: None });
        cid
    };
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => if sink.send(Message::Text(m.into())).await.is_err() { break },
                    None => break,
                },
                _ = ping.tick() => if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break },
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(t) = msg {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                hub.on_client_msg(cid, v).await;
            }
        }
    }
    writer.abort();
    hub.state.lock().unwrap().clients.remove(&cid);
}

async fn static_file(State(hub): State<Shared>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // `/?token=…`: remember the token in a cookie and drop it from the URL.
    if let Some(q) = uri.query() {
        if let Some(t) = q.split('&').find_map(|kv| kv.strip_prefix("token=")) {
            let mut res = Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(header::LOCATION, uri.path())
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::empty())
                .unwrap();
            if same(t, &hub.token) {
                res.headers_mut().insert(header::SET_COOKIE, cookie_header(&hub.token));
            }
            return res;
        }
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() || (assets::get(path).is_none() && !path.contains('.')) { "index.html" } else { path };
    let Some(plain) = assets::get(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let gzip_ok = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().starts_with("gzip")));
    let gz = gzip_ok.then(|| assets::get(&format!("{path}.gz"))).flatten();
    let a = gz.as_ref().unwrap_or(&plain);
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(a.etag) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut res = Response::builder();
    if gz.is_some() {
        res = res.header(header::CONTENT_ENCODING, "gzip");
    }
    res.header(header::CONTENT_TYPE, plain.mime)
        .header(header::VARY, "accept-encoding")
        .header(header::ETAG, a.etag)
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-content-type-options", "nosniff")
        .header("referrer-policy", "no-referrer")
        .body(Body::from(a.bytes))
        .unwrap()
}

/// Run the hub until `blush web down` (or Ctrl+C).
pub async fn serve(config: Config) -> Result<()> {
    let token = super::token()?;
    let bind = config.web.bind.clone();
    let port = config.web.port;
    let listener = tokio::net::TcpListener::bind((bind.as_str(), port))
        .await
        .with_context(|| format!("couldn't listen on {bind}:{port} (set web.port in config.toml to use another port)"))?;
    let hub: Shared = Arc::new(Hub {
        token,
        config,
        state: Mutex::new(HubState::default()),
        shutdown: Notify::new(),
        lister: tokio::sync::Mutex::new(None),
    });
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/me", get(me))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/shutdown", post(shutdown))
        .route("/ws", any(browser_ws))
        .route("/host", any(host_ws))
        .fallback(static_file)
        .with_state(hub.clone());
    let pid = std::process::id();
    super::write_info(&ServerInfo {
        pid,
        port,
        bind: bind.clone(),
        started: chrono::Utc::now().timestamp(),
        version: env!("BLUSH_VERSION").to_string(),
    })?;
    eprintln!("[{}] blush web listening on {bind}:{port} (pid {pid})", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    let stop = hub.clone();
    let result = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = stop.shutdown.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            // Open connections (WebSockets) may hold up a graceful stop.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                super::clear_info(pid);
                std::process::exit(0);
            });
        })
        .await;
    // Sessions opened from the web end with the hub.
    {
        let st = hub.state.lock().unwrap();
        for h in st.hosts.values().filter(|h| h.kind == "web") {
            if let Some(tx) = &h.tx {
                let _ = tx.send(json!({"t": "quit"}).to_string());
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Some(c) = hub.lister.lock().await.take() {
        c.shutdown().await;
    }
    super::clear_info(pid);
    eprintln!("[{}] blush web stopped", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    result.map_err(Into::into)
}

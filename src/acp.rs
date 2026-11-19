//! Minimal, tolerant Agent Client Protocol (JSON-RPC 2.0 over stdio) client.
//!
//! Messages are kept as `serde_json::Value` so that agent-specific extensions
//! (Claude Code's adapter sends many) never break deserialization.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, mpsc, oneshot};

pub const PROTOCOL_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub display: String,
}

#[derive(Debug)]
pub enum Incoming {
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
    Stderr(String),
    Exited(Option<i32>),
}

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(d) = &self.data {
            if let Some(s) = d.as_str() {
                write!(f, ": {s}")?;
            } else if let Some(s) = d.get("message").and_then(|m| m.as_str()) {
                write!(f, ": {s}")?;
            }
        }
        Ok(())
    }
}
impl std::error::Error for RpcError {}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;

/// Where the client delivers notifications, requests, stderr and exit events.
/// It's called inline by the reader, so a notification is always delivered
/// before the reply to any request that follows it on the wire.
pub type EventSink = Arc<dyn Fn(Incoming) + Send + Sync>;

pub struct AcpClient {
    out: mpsc::UnboundedSender<String>,
    /// Closes the writer (and so the agent's stdin) on shutdown.
    close_stdin: Arc<Notify>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Option<Child>>,
}

impl AcpClient {
    pub async fn spawn(spec: &LaunchSpec, events: impl Fn(Incoming) + Send + Sync + 'static) -> Result<Arc<Self>> {
        let events: EventSink = Arc::new(events);
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: don't flash a console window for node.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to start agent `{}`: {e}", spec.display))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        // Writer
        let close_stdin = Arc::new(Notify::new());
        let close = close_stdin.clone();
        tokio::spawn(async move {
            let mut stdin = stdin;
            loop {
                let line = tokio::select! {
                    l = out_rx.recv() => match l {
                        Some(l) => l,
                        None => break,
                    },
                    _ = close.notified() => break,
                };
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Stderr
        {
            let events = events.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    events(Incoming::Stderr(line));
                }
            });
        }

        // Reader
        {
            let pending = pending.clone();
            let events = events.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(line) else {
                        events(Incoming::Stderr(format!("[non-json stdout] {line}")));
                        continue;
                    };
                    let method = msg.get("method").and_then(|m| m.as_str()).map(str::to_owned);
                    let id = msg.get("id").cloned().filter(|v| !v.is_null());
                    match (method, id) {
                        (Some(method), Some(id)) => {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            events(Incoming::Request { id, method, params });
                        }
                        (Some(method), None) => {
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            events(Incoming::Notification { method, params });
                        }
                        (None, Some(id)) => {
                            let Some(n) = id.as_u64() else { continue };
                            let tx = pending.lock().await.remove(&n);
                            if let Some(tx) = tx {
                                let res = if let Some(err) = msg.get("error") {
                                    Err(RpcError {
                                        code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                                        message: err
                                            .get("message")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("unknown error")
                                            .to_string(),
                                        data: err.get("data").cloned(),
                                    })
                                } else {
                                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                                };
                                let _ = tx.send(res);
                            }
                        }
                        (None, None) => {}
                    }
                }
                // Fail everything still waiting.
                let mut p = pending.lock().await;
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err(RpcError {
                        code: -32000,
                        message: "agent process exited".into(),
                        data: None,
                    }));
                }
            });
        }

        let client = Arc::new(AcpClient {
            out: out_tx,
            close_stdin,
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(Some(child)),
        });

        // Exit watcher
        {
            let client = client.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    let mut guard = client.child.lock().await;
                    let Some(child) = guard.as_mut() else { return };
                    if let Ok(Some(status)) = child.try_wait() {
                        events(Incoming::Exited(status.code()));
                        *guard = None;
                        return;
                    }
                }
            });
        }

        Ok(client)
    }

    pub async fn request(&self, method: &str, params: Value) -> std::result::Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.out.send(msg.to_string()).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(RpcError { code: -32000, message: "agent not running".into(), data: None });
        }
        rx.await.unwrap_or_else(|_| {
            Err(RpcError { code: -32000, message: "agent connection closed".into(), data: None })
        })
    }

    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.out.send(msg.to_string());
    }

    /// A client wired to nothing: what it sends lands in the returned receiver.
    #[cfg(test)]
    pub fn for_test() -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (out, rx) = mpsc::unbounded_channel();
        let client = AcpClient {
            out,
            close_stdin: Arc::new(Notify::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            child: Mutex::new(None),
        };
        (Arc::new(client), rx)
    }

    pub fn respond(&self, id: Value, result: Value) {
        let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
        let _ = self.out.send(msg.to_string());
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        let msg = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
        let _ = self.out.send(msg.to_string());
    }

    pub async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            // Closing stdin asks the adapter to exit gracefully; then make sure.
            // (`notify_one` stores a permit, so this works even mid-write.)
            self.close_stdin.notify_one();
            let waited =
                tokio::time::timeout(std::time::Duration::from_millis(800), child.wait()).await;
            if waited.is_err() {
                kill_tree(&mut child).await;
            }
        }
    }
}

async fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        // node spawns the Claude CLI as a grandchild; take the whole tree down.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill().await;
}

// ---------- request builders ----------

pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "elicitation": { "form": {} }
        },
        "clientInfo": { "name": "blush", "title": "blush", "version": env!("CARGO_PKG_VERSION") }
    })
}

pub fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

pub fn resource_link(path: &std::path::Path) -> Value {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    json!({"type": "resource_link", "uri": path_to_uri(path), "name": name})
}

pub fn path_to_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') { format!("file://{s}") } else { format!("file:///{s}") }
}

//! The App's side of `blush web`: keeping the hub in sync, taking commands
//! from browsers, and the `/web` command.

use serde_json::{Map, Value};

use super::{App, AppMsg};
use crate::model::{Level, s};
use crate::web::sync::WebSync;

impl App {
    /// The hub link came up (or went away).
    pub(super) fn on_web_link(&mut self, out: Option<tokio::sync::mpsc::UnboundedSender<String>>) {
        match out {
            Some(out) => {
                self.web = Some(WebSync::new(out));
                self.web_sync();
                if !self.web_hosted {
                    self.toast("Shared on blush web");
                }
            }
            None => {
                self.web = None;
                if self.web_hosted {
                    // A session opened from the web lives only as long as the hub.
                    self.should_quit = true;
                }
            }
        }
    }

    /// Send the hub whatever changed since the last sync.
    pub fn web_sync(&mut self) {
        if let Some(mut w) = self.web.take() {
            if w.sync(self) {
                self.web = Some(w);
            }
        }
    }

    /// A command from a browser attached to this session.
    pub(super) async fn on_web_cmd(&mut self, v: Value) {
        match s(&v, "t").as_str() {
            "prompt" => {
                let text = s(&v, "text");
                if !text.trim().is_empty() {
                    self.submit(text).await;
                }
            }
            "cancel" => self.cancel_turn(),
            "permission" => {
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let choice = v.get("optionId").and_then(|o| o.as_str()).map(str::to_owned);
                if let Some(idx) = self.permissions.iter().position(|p| p.id == id) {
                    self.answer_permission_at(idx, choice);
                }
            }
            "question" => {
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                if self.question.as_ref().is_some_and(|q| q.id == id) {
                    let action = s(&v, "action");
                    if action == "accept" {
                        let content = v.get("content").and_then(|c| c.as_object()).cloned().unwrap_or_else(Map::new);
                        if let Some(q) = self.question.as_mut() {
                            q.answers = content;
                        }
                        self.answer_question("accept");
                    } else {
                        self.answer_question(if action == "decline" { "decline" } else { "cancel" });
                    }
                }
            }
            "set_mode" => {
                let mode = s(&v, "mode");
                if !mode.is_empty() && mode != self.session.mode {
                    self.set_mode(&mode);
                }
            }
            "set_option" => {
                let (id, value) = (s(&v, "id"), s(&v, "value"));
                if !id.is_empty() {
                    self.set_option(&id, &value);
                }
            }
            "queue" => match s(&v, "action").as_str() {
                "resume" => {
                    self.queue_paused = false;
                    self.pump_queue();
                }
                "drop" if !self.queue.is_empty() => self.drop_queue(),
                _ => {}
            },
            "quit" if self.web_hosted => self.should_quit = true,
            _ => {}
        }
        self.dirty = true;
        self.web_sync();
    }

    /// `/web`, `/web up`, `/web down`.
    pub(super) fn web_command(&mut self, args: &str) {
        let action = args.split_whitespace().next().unwrap_or("status").to_string();
        if !matches!(action.as_str(), "status" | "up" | "start" | "down" | "stop") {
            self.transcript.notice(Level::Warn, "Usage: /web · /web up · /web down");
            return;
        }
        let tx = self.tx.clone();
        self.toast(match action.as_str() {
            "up" | "start" => "Starting blush web…",
            "down" | "stop" => "Stopping blush web…",
            _ => "Checking blush web…",
        });
        tokio::task::spawn_blocking(move || {
            let msg = match action.as_str() {
                "up" | "start" => crate::web::up(true).map_err(|e| format!("Couldn't start blush web: {e:#}")),
                "down" | "stop" => match crate::web::down() {
                    Ok(true) => Ok("blush web stopped.".to_string()),
                    Ok(false) => Ok("blush web wasn't running.".to_string()),
                    Err(e) => Err(format!("Couldn't stop blush web: {e:#}")),
                },
                _ => Ok(crate::web::status_text(true)),
            };
            let _ = tx.send(AppMsg::WebInfo(msg));
        });
    }

    pub(super) fn on_web_info(&mut self, msg: Result<String, String>) {
        match msg {
            Ok(text) => self.transcript.notice(Level::Info, text),
            Err(e) => self.transcript.notice(Level::Error, e),
        }
        self.follow = true;
    }
}

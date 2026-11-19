//! The host side of blush web: the link every session keeps to the hub, and
//! the headless runner for sessions opened from a browser.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::app::{App, AppMsg, SessionKind};
use crate::config::Config;

pub fn new_host_id() -> String {
    let mut b = [0u8; 6];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Keep this session linked to the hub: connect whenever a server is up,
/// forward the app's updates to it and its commands to the app.
///
/// `once`: give up (and report the link gone) after the first connection
/// ends or if none can be made at all; for headless sessions, which only
/// exist for the hub.
pub fn spawn_bridge(tx: mpsc::UnboundedSender<AppMsg>, id: String, kind: &'static str, once: bool) {
    tokio::spawn(async move {
        let mut attempts = 0u32;
        loop {
            if tx.is_closed() {
                return;
            }
            attempts += 1;
            if let (Some(info), Ok(token)) = (super::read_info(), super::token()) {
                let url = format!("ws://127.0.0.1:{}/host?token={token}", info.port);
                if let Ok(Ok((ws, _))) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(url)).await {
                    link(ws, &tx, &id, kind).await;
                    if once {
                        let _ = tx.send(AppMsg::WebUnlinked);
                        return;
                    }
                }
            }
            if once && attempts >= 15 {
                let _ = tx.send(AppMsg::WebUnlinked);
                return;
            }
            tokio::time::sleep(Duration::from_secs(if once { 1 } else { 2 })).await;
        }
    });
}

async fn link<S>(ws: tokio_tungstenite::WebSocketStream<S>, tx: &mpsc::UnboundedSender<AppMsg>, id: &str, kind: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = ws.split();
    let hello = json!({"t": "hello", "hostId": id, "kind": kind, "pid": std::process::id()});
    if sink.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    if tx.send(AppMsg::WebLinked(out_tx)).is_err() {
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            m = out_rx.recv() => match m {
                Some(s) => if sink.send(Message::Text(s.into())).await.is_err() { break },
                // The app dropped the link (it's quitting).
                None => break,
            },
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if tx.send(AppMsg::WebCmd(v)).is_err() { break }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break }
            }
        }
    }
    let _ = tx.send(AppMsg::WebUnlinked);
}

/// `blush web host`: run a session with no terminal, driven from the web.
pub async fn run_headless(mut config: Config, cwd: PathBuf, kind: SessionKind, id: String) -> Result<()> {
    // Nobody is sitting at this machine for it: no chime, no voice, and no
    // extra `/usage` sessions (the TUI already keeps the meter).
    config.features.notify = false;
    config.features.narrator = false;
    config.features.sub_meter = false;
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMsg>();
    let mut app = App::new(config, cwd, tx.clone(), kind);
    app.web_hosted = true;
    spawn_bridge(tx.clone(), id, "web", true);
    app.start_agent();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                app.on_msg(msg).await;
                while let Ok(m) = rx.try_recv() {
                    app.on_msg(m).await;
                }
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(2);
                if app.tick % 40 == 0 {
                    app.git_label = crate::ui::git_label(&app.cwd);
                }
                app.web_sync();
            }
        }
        if app.should_quit {
            break;
        }
    }
    app.web_sync();
    if let Some(c) = app.client.take() {
        c.shutdown().await;
    }
    Ok(())
}

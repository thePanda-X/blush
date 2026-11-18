//! `blush -p "prompt"`: one-shot, non-interactive run. Agent text streams to
//! stdout; tool activity goes to stderr. Permission prompts are auto-approved
//! (the session starts in the configured mode, bypass by default).

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::acp::{self, AcpClient, Incoming};
use crate::config::Config;
use crate::launcher;
use crate::model::{content_text, s};

pub async fn run(config: Config, cwd: PathBuf, prompt: String) -> Result<i32> {
    if launcher::needs_install(&config) {
        launcher::install_adapter(|m| eprintln!("{m}")).await?;
    }
    let spec = launcher::launch_spec(&config, &cwd)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = AcpClient::spawn(&spec, tx).await?;

    client.request("initialize", acp::initialize_params()).await.map_err(|e| anyhow!("initialize: {e}"))?;
    let sess = client
        .request("session/new", json!({"cwd": cwd.to_string_lossy(), "mcpServers": []}))
        .await
        .map_err(|e| anyhow!("session/new: {e}"))?;
    if std::env::var_os("BLUSH_DEBUG").is_some() {
        eprintln!("{}", serde_json::to_string_pretty(&sess).unwrap_or_default());
    }
    let sid = s(&sess, "sessionId");
    let mode = config.agent.default_mode.clone();
    let has_mode = sess
        .get("modes")
        .and_then(|m| m.get("availableModes"))
        .and_then(|a| a.as_array())
        .is_some_and(|a| a.iter().any(|m| s(m, "id") == mode));
    if has_mode {
        let _ = client.request("session/set_mode", json!({"sessionId": sid, "modeId": mode})).await;
    }
    if !config.agent.model.is_empty() {
        let _ = client
            .request("session/set_config_option", json!({"sessionId": sid, "configId": "model", "value": config.agent.model}))
            .await;
    }

    let mut blocks = vec![acp::text_block(&prompt)];
    for p in crate::files::mentioned_paths(&prompt, &cwd) {
        if p.is_file() {
            blocks.push(acp::resource_link(&p));
        }
    }
    let c2 = client.clone();
    let sid2 = sid.clone();
    let mut turn = tokio::spawn(async move {
        c2.request("session/prompt", json!({"sessionId": sid2, "prompt": blocks})).await
    });

    let mut out = std::io::stdout();
    let mut ended_with_newline = true;
    let result = loop {
        tokio::select! {
            r = &mut turn => break r,
            Some(m) = rx.recv() => match m {
                Incoming::Notification { method, params } if method == "session/update" => {
                    let u = params.get("update").cloned().unwrap_or(Value::Null);
                    match s(&u, "sessionUpdate").as_str() {
                        "agent_message_chunk" => {
                            if let Some(t) = u.get("content").and_then(content_text) {
                                print!("{t}");
                                ended_with_newline = t.ends_with('\n');
                                let _ = out.flush();
                            }
                        }
                        "tool_call" => {
                            let title = s(&u, "title");
                            if !title.is_empty() {
                                eprintln!("\x1b[2m◆ {}\x1b[0m", title.lines().next().unwrap_or(""));
                            }
                        }
                        _ => {}
                    }
                }
                Incoming::Request { id, method, params } => {
                    if method == "session/request_permission" {
                        let opts = params.get("options").and_then(|o| o.as_array()).cloned().unwrap_or_default();
                        let pick = opts
                            .iter()
                            .find(|o| s(o, "kind") == "allow_once")
                            .or_else(|| opts.iter().find(|o| s(o, "kind").starts_with("allow")))
                            .map(|o| s(o, "optionId"));
                        match pick {
                            Some(oid) => client.respond(id, json!({"outcome": {"outcome": "selected", "optionId": oid}})),
                            None => client.respond(id, json!({"outcome": {"outcome": "cancelled"}})),
                        }
                    } else if method == "elicitation/create" {
                        client.respond(id, json!({"action": "decline"}));
                    } else {
                        client.respond_error(id, -32601, "not supported in headless mode");
                    }
                }
                Incoming::Exited(code) => {
                    eprintln!("agent exited ({code:?})");
                    break Ok(Err(acp::RpcError { code: -32000, message: "agent exited".into(), data: None }));
                }
                _ => {}
            }
        }
    };
    if !ended_with_newline {
        println!();
    }
    client.shutdown().await;
    match result {
        Ok(Ok(v)) => {
            let reason = s(&v, "stopReason");
            Ok(if reason == "end_turn" || reason.is_empty() { 0 } else { 2 })
        }
        Ok(Err(e)) => {
            eprintln!("error: {e}");
            Ok(1)
        }
        Err(e) => Err(anyhow!(e)),
    }
}

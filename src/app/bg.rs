//! Background tasks: the shells, monitors and workflows Claude runs in the
//! background, as the adapter reports them (`async_task_*` updates). Alt+J or
//! `/bg` opens a viewer that lists them and follows the selected one's output.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

use super::{App, AppMsg, Overlay};
use crate::model::{BgTask, Block, ToolContent};

/// How much of an output file the viewer reads: its last bytes.
const TAIL_BYTES: u64 = 256 * 1024;
/// How often the open viewer re-reads the selected task's output.
const POLL: Duration = Duration::from_millis(400);

/// State of the background tasks viewer.
pub struct BgView {
    /// The selected task (by id, so the selection survives the list changing).
    pub sel: Option<String>,
    /// Lines scrolled up from the bottom of the output; 0 follows new output.
    pub scroll: usize,
    /// The selected task's output, as last read.
    pub output: String,
    /// What `output` was read from: (task id, file length, modified time).
    read_from: Option<(String, u64, Option<SystemTime>)>,
    read_at: Option<Instant>,
}

impl App {
    /// The tasks the viewer lists: running ones first, then finished, newest
    /// first within each. Turns Claude starts on its own have no output, so
    /// they stay out of it (the tasks pane shows them).
    pub fn bg_tasks(&self) -> Vec<&BgTask> {
        let mut v: Vec<&BgTask> = self.transcript.bg.iter().filter(|t| t.kind != "turn").collect();
        v.reverse();
        v.sort_by_key(|t| !t.running());
        v
    }

    pub fn bg_running(&self) -> usize {
        self.transcript.bg.iter().filter(|t| t.kind != "turn" && t.running()).count()
    }

    pub fn open_bg_view(&mut self) {
        if !self.transcript.bg_seen && self.bg_tasks().is_empty() {
            self.toast("No background tasks yet");
            return;
        }
        self.picker_cancel();
        let sel = self.bg_tasks().first().map(|t| t.id.clone());
        self.overlay = Overlay::Tasks(BgView { sel, scroll: 0, output: String::new(), read_from: None, read_at: None });
        self.poll_bg_view();
    }

    /// Index of the selected task in [`App::bg_tasks`].
    fn bg_sel_index(&self) -> Option<usize> {
        let Overlay::Tasks(v) = &self.overlay else { return None };
        let tasks = self.bg_tasks();
        v.sel.as_deref().and_then(|id| tasks.iter().position(|t| t.id == id)).or((!tasks.is_empty()).then_some(0))
    }

    pub(super) fn bg_select(&mut self, delta: isize) {
        let Some(i) = self.bg_sel_index() else { return };
        let ids: Vec<String> = self.bg_tasks().iter().map(|t| t.id.clone()).collect();
        let next = (i as isize + delta).clamp(0, ids.len() as isize - 1) as usize;
        if let Overlay::Tasks(v) = &mut self.overlay {
            if v.sel.as_deref() != Some(ids[next].as_str()) {
                v.sel = Some(ids[next].clone());
                v.scroll = 0;
                v.read_from = None;
                v.read_at = None;
            }
        }
        self.poll_bg_view();
    }

    /// Select the task in row `i` of the list (a mouse click).
    pub(super) fn bg_select_row(&mut self, i: usize) {
        let Some(cur) = self.bg_sel_index() else { return };
        self.bg_select(i as isize - cur as isize);
    }

    /// Re-read the selected task's output when the viewer is open. Called on
    /// every tick; reads at most every [`POLL`]. True when the output changed.
    pub fn poll_bg_view(&mut self) -> bool {
        let Overlay::Tasks(v) = &self.overlay else { return false };
        if v.read_at.is_some_and(|t| t.elapsed() < POLL) {
            return false;
        }
        let task = v.sel.as_deref().and_then(|id| self.transcript.bg.iter().find(|t| t.id == id)).cloned();
        let prev = v.read_from.clone();
        let (text, from) = match &task {
            None => (String::new(), None),
            Some(t) => match t.output_file.as_deref().and_then(|p| file_stamp(p).map(|s| (p, s))) {
                Some((path, (len, mtime))) => {
                    let stamp = Some((t.id.clone(), len, mtime));
                    if stamp == prev {
                        // Unchanged since the last read.
                        let running = self.bg_running() > 0;
                        if let Overlay::Tasks(v) = &mut self.overlay {
                            v.read_at = Some(Instant::now());
                        }
                        return running;
                    }
                    (read_tail(path).unwrap_or_default(), stamp)
                }
                // No output file (yet): show what the tool call returned.
                None => (self.tool_output(t), None),
            },
        };
        // Running tasks' timers tick, so redraw while any runs.
        let running = self.bg_running() > 0;
        let Overlay::Tasks(v) = &mut self.overlay else { return false };
        v.read_at = Some(Instant::now());
        v.read_from = from;
        let changed = v.output != text;
        v.output = text;
        changed || running
    }

    /// The text of the tool call that started a task.
    fn tool_output(&self, task: &BgTask) -> String {
        let Some(idx) = task.tool_call_id.as_deref().and_then(|id| self.transcript.tool_index.get(id)) else {
            return String::new();
        };
        let Some(Block::Tool(t)) = self.transcript.entries.get(*idx).map(|e| &e.block) else { return String::new() };
        t.content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text(s) | ToolContent::Terminal(s) => Some(clean_output(s)),
                ToolContent::Diff { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Stop the selected task through the adapter.
    fn bg_stop_selected(&mut self) {
        let Some(i) = self.bg_sel_index() else { return };
        let task = self.bg_tasks()[i].clone();
        if !task.running() {
            self.toast("That task has already finished");
            return;
        }
        if !task.can_stop {
            self.toast("Claude can't stop this task from here");
            return;
        }
        let (Some(client), Some(sid)) = (self.client.clone(), self.session.id.clone()) else { return };
        let label = self.transcript.bg_label(&task);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = client.request("_session/async_task/stop", json!({"sessionId": sid, "asyncTaskId": task.id})).await;
            let _ = tx.send(AppMsg::BgStopped(label, r.map(|_| ()).map_err(|e| e.to_string())));
        });
        self.toast("Stopping…");
    }

    pub(super) fn on_bg_stopped(&mut self, label: String, r: Result<(), String>) {
        match r {
            Ok(()) => self.toast(format!("Stopped: {}", crate::text::truncate(&label, 60))),
            Err(e) => self.toast(format!("Couldn't stop the task: {e}")),
        }
    }

    /// Keys while the viewer is open.
    pub(super) fn on_bg_key(&mut self, k: &KeyEvent) {
        let page = self.layout.viewer_page.saturating_sub(2).max(1);
        let max = self.layout.viewer_max;
        let scroll = |app: &mut App, f: &dyn Fn(usize) -> usize| {
            if let Overlay::Tasks(v) = &mut app.overlay {
                v.scroll = f(v.scroll.min(max)).min(max);
            }
        };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.bg_select(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.bg_select(1),
            KeyCode::PageUp => scroll(self, &|s| s + page),
            KeyCode::PageDown | KeyCode::Char(' ') => scroll(self, &|s| s.saturating_sub(page)),
            KeyCode::Home | KeyCode::Char('g') => scroll(self, &|_| usize::MAX),
            KeyCode::End | KeyCode::Char('G') => scroll(self, &|_| 0),
            KeyCode::Char('x') | KeyCode::Delete => self.bg_stop_selected(),
            KeyCode::Char('y') => {
                if let Overlay::Tasks(v) = &self.overlay {
                    let t = v.output.clone();
                    self.copy(&t);
                }
            }
            KeyCode::Char('o') => self.bg_reveal(),
            KeyCode::Enter => {
                let Some(i) = self.bg_sel_index() else { return };
                let task = self.bg_tasks()[i].clone();
                let Overlay::Tasks(v) = &self.overlay else { return };
                let mut text = String::new();
                if let Some(cmd) = self.transcript.bg_command(&task) {
                    text.push_str(&format!("```sh\n{cmd}\n```\n\n"));
                }
                text.push_str(&format!("```text\n{}\n```", v.output.trim_end()));
                let title = format!("{} · {}", task.kind, self.transcript.bg_label(&task));
                self.overlay = Overlay::Viewer { title, text, scroll: 0 };
            }
            _ => {}
        }
    }

    /// Show the selected task's output file in the file manager (`.output`
    /// files have no default app to open them with).
    fn bg_reveal(&mut self) {
        let Some(i) = self.bg_sel_index() else { return };
        match self.bg_tasks()[i].output_file.clone() {
            Some(p) if p.exists() => {
                if let Err(e) = crate::peek::reveal(&p) {
                    self.toast(format!("Couldn't reveal {}: {e}", p.display()));
                }
            }
            _ => self.toast("This task has no output file"),
        }
    }
}

fn file_stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

/// The last [`TAIL_BYTES`] of a file, cleaned up for display.
fn read_tail(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // Drop the partial first line.
        if let Some(nl) = text.find('\n') {
            text.drain(..=nl);
        }
    }
    Ok(clean_output(&text))
}

/// Terminal output as plain lines: ANSI escapes removed, and a line that a
/// carriage return rewrote (a progress bar) shows only its last version.
pub fn clean_output(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ params final-byte
            Some('[') => {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(n) = chars.next() {
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out.split('\n')
        .map(|l| {
            let l = l.strip_suffix('\r').unwrap_or(l);
            l.rsplit('\r').next().unwrap_or(l)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SessionKind;
    use crate::config::Config;

    #[test]
    fn cleans_terminal_output() {
        assert_eq!(clean_output("\x1b[32mok\x1b[0m\r\nnext"), "ok\nnext");
        assert_eq!(clean_output("10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(clean_output("\x1b]0;title\x07hi"), "hi");
    }

    #[tokio::test]
    async fn tracks_async_tasks_and_reads_their_output() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Config::default(), std::env::temp_dir(), tx, SessionKind::New);
        let mut s = crate::model::Session::default();
        let dir = std::env::temp_dir().join(format!("blush-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("b1.output");
        std::fs::write(&out, "line 1\nline 2\n").unwrap();
        app.transcript.apply_update(
            &mut s,
            &json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"npm run dev","kind":"execute","status":"completed",
                "rawInput":{"command":"npm run dev","run_in_background":true}}),
        );
        app.transcript.apply_update(
            &mut s,
            &json!({"sessionUpdate":"async_task_spawned","asyncTaskId":"b1","name":"Shell","taskType":"shell",
                "description":"","showInTranscript":true,"canStop":true,"toolCallId":"t1",
                "outputFilePath": out.to_string_lossy()}),
        );
        assert_eq!(app.bg_running(), 1);
        assert_eq!(app.transcript.bg_label(&app.transcript.bg[0]), "npm run dev");
        let rows = app.task_rows();
        assert!(rows.iter().any(|r| r.kind == "background" && r.label == "npm run dev"));

        app.open_bg_view();
        let Overlay::Tasks(v) = &app.overlay else { panic!("viewer not open") };
        assert_eq!(v.output, "line 1\nline 2\n");

        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"async_task_state_update","asyncTaskId":"b1","state":"completed"}));
        assert_eq!(app.bg_running(), 0);
        assert!(!app.task_rows().iter().any(|r| r.kind == "background"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

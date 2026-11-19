//! Grok-Build-style conveniences: command palette, find in scrollback,
//! prompt stash, send-now, queue editing, shell mode, `/btw` side
//! questions, `/rename`, density and timestamp modes, the todo and tasks
//! panes, the settings modal, image and long-paste attachments, and
//! attention notifications.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};

use super::{App, AppMsg, Focus, LOCAL_COMMANDS, Overlay, PickItem, PickerKind};
use crate::acp;
use crate::attach;
use crate::config::{Config, home_dir};
use crate::model::{Block, Fold, Level, ToolCall, ToolContent, ToolStatus, s};
use crate::{notify, shell};

/// A `!` command in flight.
pub struct ShellRun {
    pub id: u64,
    pub cmd: String,
    /// Key of its scrollback entry in `transcript.tool_index`, which keeps
    /// entry indices right when entries move.
    pub key: String,
    pub handle: tokio::task::JoinHandle<()>,
    pub started: Instant,
    /// The shell's process id once it has started (0 until then).
    pub pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// The session it was started in: output goes only to that session.
    pub session: Option<String>,
}

/// A `/btw` side question in flight.
pub struct Btw {
    pub id: u64,
    /// The forked session it runs in, once created.
    pub sid: Option<String>,
    pub key: String,
    pub question: String,
    pub started: Instant,
}

/// Find in scrollback.
#[derive(Default)]
pub struct Search {
    pub query: String,
    /// Matching entry indices, oldest first.
    pub matches: Vec<usize>,
    /// Position in `matches` of the selected match.
    pub cur: usize,
}

/// One row of the settings modal.
pub struct SettingsItem {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub hint: &'static str,
}

/// One row of the tasks pane and `/tasks`.
pub struct TaskRow {
    pub kind: &'static str,
    pub label: String,
    pub since: Option<Duration>,
}

/// Windows terminals deliver a paste as key presses (crossterm reports
/// bracketed paste only on Unix), and every newline as Enter, which would
/// send the prompt. A batch of presses that were all waiting at once is a
/// paste when it's long, has a line break before its end, or continues a
/// paste a moment ago. Returns the pasted text.
pub fn paste_burst(batch: &[Event], continuing: bool) -> Option<String> {
    if !cfg!(windows) && !cfg!(test) {
        return None;
    }
    let mut text = String::new();
    let mut presses = 0;
    for ev in batch {
        let Event::Key(k) = ev else { return None };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        let m = k.modifiers;
        // Plain or shifted characters; Ctrl+Alt is AltGr on Windows.
        let plain = (m - KeyModifiers::SHIFT).is_empty() || m.contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match k.code {
            KeyCode::Char(c) if plain => text.push(c),
            KeyCode::Enter if (m - KeyModifiers::SHIFT).is_empty() => text.push('\n'),
            KeyCode::Tab if m.is_empty() => text.push('\t'),
            KeyCode::Esc if m.is_empty() => text.push('\x1b'),
            _ => return None,
        }
        presses += 1;
    }
    let body = text.trim_end_matches('\n');
    let qualifies = (continuing && presses >= 1) || (presses >= 2 && (body.contains('\n') || presses >= 8));
    if !qualifies {
        return None;
    }
    // Bracketed-paste markers that came through as keys, and stray escapes.
    let text = text.replace("\x1b[200~", "").replace("\x1b[201~", "").replace('\x1b', "");
    (!text.is_empty()).then_some(text)
}

pub const DENSITIES: &[(&str, &str)] = &[
    ("detailed", "every tool call expanded"),
    ("balanced", "tool calls folded to one line (default)"),
    ("diff", "tools and thinking grouped; edits and their diffs stay visible"),
    ("grouped", "tools, thinking and edits grouped between replies"),
];

/// Shortcut shown next to a command in the palette.
fn shortcut_for(name: &str) -> &'static str {
    match name {
        "new" => "Ctrl+N",
        "resume" => "Ctrl+R",
        "model" => "Ctrl+P",
        "theme" => "Ctrl+T",
        "copy" => "Ctrl+Y",
        "mode" => "Alt+M",
        "effort" => "Alt+E",
        "commit" => "Alt+C",
        "timeline" => "Alt+T",
        "undo" => "Alt+Z",
        "redo" => "Alt+Shift+Z",
        "export" => "Alt+X",
        "context" => "Alt+K",
        "thinking" => "Alt+H",
        "timestamps" => "Alt+S",
        "narrator" => "Alt+N",
        "compact-mode" => "Ctrl+G",
        "find" => "Ctrl+F",
        "stash" => "Ctrl+S",
        "todos" => "Alt+P",
        "tasks-pane" => "Ctrl+B",
        "settings" => "F2",
        "shortcuts" => "Ctrl+.",
        _ => "",
    }
}

fn titles_path() -> std::path::PathBuf {
    home_dir().join("titles.json")
}

fn load_titles() -> BTreeMap<String, String> {
    std::fs::read_to_string(titles_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// The title a session was given with /rename.
pub fn local_title(sid: &str) -> Option<String> {
    load_titles().remove(sid)
}

fn save_local_title(sid: &str, title: &str) {
    let mut all = load_titles();
    all.insert(sid.to_string(), title.to_string());
    let p = titles_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, serde_json::to_string_pretty(&all).unwrap_or_default());
}

impl App {
    /// Input events that were all waiting at once (see [`paste_burst`]).
    pub async fn on_events(&mut self, batch: Vec<Event>) {
        let continuing = self.last_burst.is_some_and(|t| t.elapsed() < Duration::from_millis(40));
        if let Some(text) = paste_burst(&batch, continuing) {
            self.last_burst = Some(Instant::now());
            self.on_event(Event::Paste(text)).await;
            return;
        }
        for ev in batch {
            self.on_event(ev).await;
        }
    }

    fn next_run_id(&mut self) -> u64 {
        self.run_seq += 1;
        self.run_seq
    }

    fn entry_for_key(&self, key: &str) -> Option<usize> {
        self.transcript.tool_index.get(key).copied().filter(|&i| i < self.transcript.entries.len())
    }

    // ------------------------------------------------------------ commands

    pub(super) async fn run_extra_command(&mut self, name: &str, args: &str) {
        match name {
            "btw" => {
                if args.is_empty() {
                    self.toast("Usage: /btw <question> — answered on the side, without interrupting");
                } else {
                    // Expand collapsed pastes; images stay with the main prompt.
                    let (q, _) = self.attachments.take(args);
                    self.start_btw(q);
                }
            }
            "queue" => {
                let msg = if self.queue.is_empty() {
                    "No prompts queued".to_string()
                } else {
                    let rows: Vec<String> = self
                        .queue
                        .iter()
                        .enumerate()
                        .map(|(i, (o, _))| format!("{}. {}", i + 1, crate::text::truncate(o.raw.lines().next().unwrap_or(""), 100)))
                        .collect();
                    format!("Queued ({}{}):\n{}\n↑ on an empty prompt edits the last one", self.queue.len(), if self.queue_paused { ", on hold" } else { "" }, rows.join("\n"))
                };
                self.transcript.notice(Level::Info, msg);
            }
            "tasks" => {
                let rows = self.task_rows();
                let msg = if rows.is_empty() {
                    "Nothing running".to_string()
                } else {
                    let body: Vec<String> = rows
                        .iter()
                        .map(|r| format!("{:<10} {}{}", r.kind, r.label, r.since.map(|d| format!("  {}", crate::text::human_duration(d))).unwrap_or_default()))
                        .collect();
                    format!("Tasks:\n{}", body.join("\n"))
                };
                self.transcript.notice(Level::Info, msg);
            }
            "find" => {
                self.open_search();
                if let Some(s) = self.search.as_mut() {
                    s.query = args.to_string();
                }
                if !args.is_empty() {
                    self.search_refresh(true);
                }
            }
            "rename" => self.rename(args),
            "stash" => self.stash_toggle(),
            "density" => {
                if args.is_empty() {
                    let cur = self.density();
                    let items = DENSITIES
                        .iter()
                        .map(|(n, d)| PickItem { label: n.to_string(), detail: d.to_string(), value: format!("run:density {n}"), current: *n == cur })
                        .collect();
                    self.open_picker(PickerKind::Palette, "Density", items);
                } else {
                    self.set_density(args);
                }
            }
            "todos" => self.toggle_pane(true),
            "tasks-pane" => self.toggle_pane(false),
            _ => {}
        }
    }

    // ------------------------------------------------------------ palette

    pub(super) fn open_palette(&mut self) {
        let mut items: Vec<PickItem> = Vec::new();
        for (name, desc) in LOCAL_COMMANDS {
            // Aliases add nothing in a searchable list.
            if matches!(*name, "clear" | "help") {
                continue;
            }
            let key = shortcut_for(name);
            items.push(PickItem {
                label: format!("/{name}"),
                detail: if key.is_empty() { desc.to_string() } else { format!("{desc}  ·  {key}") },
                value: format!("local:{name}"),
                current: false,
            });
        }
        items.push(PickItem {
            label: "Paste image".into(),
            detail: "attach the clipboard's image to the prompt  ·  Alt+V".into(),
            value: "action:paste-image".into(),
            current: false,
        });
        for c in &self.session.commands {
            if LOCAL_COMMANDS.iter().any(|(n, _)| *n == c.name) {
                continue;
            }
            items.push(PickItem { label: format!("/{}", c.name), detail: c.description.clone(), value: format!("agent:{}", c.name), current: false });
        }
        self.completion = None;
        self.open_picker(PickerKind::Palette, "Commands", items);
        if let Overlay::Picker(p) = &mut self.overlay {
            p.sel = 0;
        }
    }

    pub(super) async fn palette_run(&mut self, value: String) {
        let (kind, rest) = value.split_once(':').unwrap_or(("", value.as_str()));
        match kind {
            "local" => {
                // Commands that need an argument wait in the prompt for it.
                if matches!(rest, "after" | "btw" | "rename") {
                    self.focus = Focus::Prompt;
                    self.input.set(&format!("/{rest} "));
                    return;
                }
                self.run_local(rest, "").await;
            }
            "agent" => {
                let hint = self.session.commands.iter().find(|c| c.name == rest).is_some_and(|c| !c.hint.is_empty());
                if hint {
                    self.focus = Focus::Prompt;
                    self.input.set(&format!("/{rest} "));
                } else {
                    self.submit(format!("/{rest}")).await;
                }
            }
            "run" => {
                let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
                self.run_local(name, args).await;
            }
            "action" if rest == "paste-image" => self.paste_image(),
            _ => {}
        }
    }

    // ------------------------------------------------------------ global keys

    /// Keys added on top of blush's own. Returns true when handled.
    pub(super) async fn on_extra_global_key(&mut self, k: &KeyEvent, ctrl: bool, alt: bool) -> bool {
        match k.code {
            KeyCode::F(2) => self.open_settings(),
            KeyCode::Char(',') if ctrl && !alt => self.open_settings(),
            KeyCode::Char('x') | KeyCode::Char('X') if ctrl && !alt => self.open_palette(),
            KeyCode::Char('f') | KeyCode::Char('F') if ctrl && !alt => self.open_search(),
            KeyCode::Char('b') | KeyCode::Char('B') if ctrl && !alt => self.toggle_pane(false),
            KeyCode::Char('s') | KeyCode::Char('S') if ctrl && !alt => self.stash_toggle(),
            KeyCode::Char('v') | KeyCode::Char('V') if ctrl && !alt => self.paste_clipboard(),
            KeyCode::Char('p') if alt && !ctrl => self.toggle_pane(true),
            KeyCode::Char('v') if alt && !ctrl => self.paste_image(),
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------ find

    pub(super) fn open_search(&mut self) {
        self.completion = None;
        if self.search.is_none() {
            self.search = Some(Search::default());
        }
    }

    /// Recompute matches; with `jump`, select the newest one.
    pub(super) fn search_refresh(&mut self, jump: bool) {
        let Some(search) = self.search.as_ref() else { return };
        let q = search.query.to_lowercase();
        let matches: Vec<usize> = if q.is_empty() {
            vec![]
        } else {
            self.transcript
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !matches!(e.block, Block::Welcome))
                .filter(|(_, e)| self.transcript.show_thinking || !matches!(e.block, Block::Thought { .. }))
                .filter(|(_, e)| e.plain_text().to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        let n = matches.len();
        if let Some(search) = self.search.as_mut() {
            search.matches = matches;
            search.cur = if jump { n.saturating_sub(1) } else { search.cur.min(n.saturating_sub(1)) };
        }
        if jump && n > 0 {
            self.search_select();
        }
    }

    fn search_step(&mut self, older: bool) {
        // Entries may have moved (a queued prompt was sent): match afresh.
        self.search_refresh(false);
        let Some(search) = self.search.as_mut() else { return };
        if search.matches.is_empty() {
            return;
        }
        let n = search.matches.len();
        search.cur = if older { (search.cur + n - 1) % n } else { (search.cur + 1) % n };
        self.search_select();
    }

    /// Select the current match, unfolding whatever hides it.
    fn search_select(&mut self) {
        let Some(idx) = self.search.as_ref().and_then(|s| s.matches.get(s.cur).copied()) else { return };
        if idx >= self.transcript.entries.len() {
            return;
        }
        if let Some((start, _)) = self.group_of(idx) {
            if !self.transcript.entries[start].group_open {
                self.transcript.entries[start].group_open = true;
            }
        }
        let e = &mut self.transcript.entries[idx];
        if e.foldable() && e.fold == Fold::Collapsed {
            e.fold = Fold::Expanded;
            e.touch();
        }
        self.selected = Some(idx);
        self.focus = Focus::Scrollback;
        self.follow = false;
        self.ensure_selected_visible = true;
    }

    pub(super) fn on_search_key(&mut self, k: KeyEvent, ctrl: bool, shift: bool) {
        match k.code {
            KeyCode::Esc => self.search = None,
            KeyCode::Enter if shift => self.search_step(false),
            KeyCode::Enter | KeyCode::Up => self.search_step(true),
            KeyCode::Down => self.search_step(false),
            KeyCode::Char('f') if ctrl => self.search_step(true),
            KeyCode::Backspace => {
                if let Some(s) = self.search.as_mut() {
                    s.query.pop();
                }
                self.search_refresh(true);
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(s) = self.search.as_mut() {
                    s.query.push(c);
                }
                self.search_refresh(true);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ prompt helpers

    pub(super) fn stash_toggle(&mut self) {
        let text = self.input.text.clone();
        let atts = self.attachments.items.clone();
        if text.is_empty() {
            match self.stash.take() {
                Some((t, a)) => {
                    self.restore_prompt(&t, a);
                    self.toast("Restored your stashed prompt");
                }
                None => self.toast("Nothing to stash: Ctrl+S sets a prompt aside, and brings it back"),
            }
            return;
        }
        let previous = self.stash.replace((text, atts));
        self.input.clear();
        self.attachments.clear();
        self.completion = None;
        match previous {
            Some((t, a)) => {
                self.restore_prompt(&t, a);
                self.toast("Swapped with your stashed prompt");
            }
            None => self.toast("Prompt stashed · Ctrl+S on an empty prompt brings it back"),
        }
    }

    fn restore_prompt(&mut self, text: &str, atts: Vec<attach::Attached>) {
        self.focus = Focus::Prompt;
        self.input.set(text);
        for a in atts {
            self.attachments.restore(a);
        }
    }

    /// Ctrl+Enter while Claude works: stop the turn and send this prompt next.
    pub(super) async fn send_now(&mut self) {
        let text = self.input.take();
        self.completion = None;
        let trimmed = text.trim_end().to_string();
        if trimmed.starts_with('/') || trimmed.starts_with('!') {
            self.submit(text).await;
            return;
        }
        super::save_history_line(&trimmed);
        let out = self.outgoing_from(&trimmed);
        let idx = self.transcript.push(Block::User { text: out.text.clone(), queued: true, tag: out.tag() });
        // Several Ctrl+Enters before the stop lands go out in the order typed.
        let at = self.send_now.min(self.queue.len());
        self.queue.insert(at, (out, idx));
        self.queue_paused = false;
        self.follow = true;
        self.send_now += 1;
        self.cancel_turn();
        self.toast("Stopping Claude to send this now");
    }

    /// ↑ on an empty prompt with prompts queued: pull the last one back to edit.
    pub(super) fn edit_last_queued(&mut self) {
        let Some((out, idx)) = self.queue.pop_back() else { return };
        self.remove_entry(idx);
        if self.queue.is_empty() {
            self.queue_paused = false;
        }
        let mut ctx = out.context;
        ctx.append(&mut self.pending_context);
        self.pending_context = ctx;
        self.restore_prompt(&out.raw, out.images);
        self.toast("Editing a queued prompt · Enter queues it again, Ctrl+C drops it");
    }

    /// Remove one scrollback entry, keeping every stored index valid.
    fn remove_entry(&mut self, idx: usize) {
        if idx >= self.transcript.entries.len() {
            return;
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
        for q in self.queue.iter_mut() {
            shift(&mut q.1);
        }
        self.selected = match self.selected {
            Some(s) if s == idx => None,
            Some(mut s) => {
                shift(&mut s);
                Some(s)
            }
            None => None,
        };
        self.search = None;
        self.invalidate_all();
    }

    pub(super) fn has_snapshots(&self) -> bool {
        self.config.features.timeline && self.timeline.try_lock().map(|t| !t.snaps.is_empty()).unwrap_or(false)
    }

    /// `m` in the scrollback: a response's markdown source, or rendered.
    pub(super) fn toggle_raw(&mut self) {
        let Some(i) = self.selected else { return };
        let Some(e) = self.transcript.entries.get_mut(i) else { return };
        if !matches!(e.block, Block::Agent { .. }) {
            self.toast("Raw view works on Claude's responses");
            return;
        }
        e.raw = !e.raw;
        e.touch();
        let raw = e.raw;
        self.ensure_selected_visible = true;
        self.toast(if raw { "Showing markdown source (m to render)" } else { "Rendered markdown" });
    }

    // ------------------------------------------------------------ attachments

    fn images_supported(&self) -> bool {
        // Before the agent connects, assume yes; Claude Code accepts images.
        self.agent_caps.is_null()
            || self.agent_caps.get("promptCapabilities").and_then(|p| p.get("image")).and_then(|v| v.as_bool()).unwrap_or(false)
    }

    fn insert_token(&mut self, label: &str) {
        let before = &self.input.text[..self.input.cursor];
        if !before.is_empty() && !before.ends_with(char::is_whitespace) {
            self.input.insert_char(' ');
        }
        self.input.insert_str(label);
        self.input.insert_char(' ');
    }

    pub(super) fn on_paste(&mut self, pasted: String) {
        // With only an image on the clipboard, terminals paste nothing.
        if pasted.is_empty() {
            if self.images_supported() {
                self.paste_image();
            }
            return;
        }
        if self.images_supported() {
            if let Some(path) = attach::pasted_image_path(&pasted, &self.cwd) {
                match attach::image_from_file(&path) {
                    Ok(block) => {
                        let label = self.attachments.add_image(block);
                        self.insert_token(&label);
                        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        self.toast(format!("Attached {name} as {label}"));
                        return;
                    }
                    Err(e) => self.toast(format!("Couldn't attach the image: {e}")),
                }
            }
        }
        if attach::should_collapse(&pasted) && !self.input.text.starts_with('!') {
            let text = pasted.replace("\r\n", "\n").replace('\r', "\n");
            let label = self.attachments.add_text(text);
            self.insert_token(&label);
            return;
        }
        self.input.insert_str(&pasted);
    }

    /// Alt+V: attach the clipboard's image.
    pub(super) fn paste_image(&mut self) {
        if !self.images_supported() {
            self.toast("This agent doesn't accept images");
            return;
        }
        match attach::image_from_clipboard() {
            Ok(block) => {
                self.focus = Focus::Prompt;
                let label = self.attachments.add_image(block);
                self.insert_token(&label);
                self.toast(format!("Image attached as {label}"));
            }
            Err(e) => self.toast(format!("Couldn't paste an image: {e}")),
        }
    }

    /// Ctrl+V when the terminal passes it through: the clipboard's text, or
    /// its image when there's no text (Office copies carry both).
    fn paste_clipboard(&mut self) {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) if !t.is_empty() => {
                self.focus = Focus::Prompt;
                self.on_paste(t);
                self.refresh_completion();
            }
            _ => self.paste_image(),
        }
    }

    // ------------------------------------------------------------ shell mode

    pub(super) fn run_shell(&mut self, cmd: String) {
        if cmd.is_empty() {
            self.toast("Type a command after ! to run it in your shell");
            return;
        }
        if self.shell.is_some() {
            self.toast("A shell command is already running (Ctrl+C stops it)");
            return;
        }
        let id = self.next_run_id();
        let key = format!("blush:shell:{id}");
        let tool = ToolCall {
            id: key.clone(),
            title: cmd.clone(),
            kind: "execute".into(),
            status: ToolStatus::Running,
            content: vec![],
            raw_input: json!({"command": cmd, "description": format!("! {} · the output goes to Claude with your next prompt", shell::name())}),
            locations: vec![],
            line: None,
            started: Instant::now(),
            ended: None,
        };
        let idx = self.transcript.push(Block::Tool(tool));
        self.transcript.entries[idx].fold = Fold::Expanded;
        self.transcript.tool_index.insert(key.clone(), idx);
        self.follow = true;
        let tx = self.tx.clone();
        let cwd = self.cwd.clone();
        let c = cmd.clone();
        let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let p2 = pid.clone();
        let handle = tokio::spawn(async move {
            let r = shell::run(&c, &cwd, p2).await;
            let _ = tx.send(AppMsg::ShellDone(id, r));
        });
        let session = self.session.id.clone();
        self.shell = Some(ShellRun { id, cmd, key, handle, started: Instant::now(), pid, session });
    }

    pub(super) fn kill_shell(&mut self) {
        let Some(run) = self.shell.take() else { return };
        // The whole process tree: PowerShell alone would leave its children running.
        shell::kill_tree(run.pid.load(std::sync::atomic::Ordering::SeqCst));
        run.handle.abort();
        if let Some(i) = self.entry_for_key(&run.key) {
            let e = &mut self.transcript.entries[i];
            if let Block::Tool(t) = &mut e.block {
                t.status = ToolStatus::Failed;
                t.ended = Some(run.started.elapsed());
                t.content = vec![ToolContent::Text("Stopped".into())];
            }
            e.touch();
        }
        self.toast("Shell command stopped");
    }

    pub(super) fn on_shell_done(&mut self, id: u64, r: std::io::Result<shell::ShellResult>) {
        if self.shell.as_ref().map(|s| s.id) != Some(id) {
            return; // stopped already
        }
        let run = self.shell.take().unwrap();
        let (status, content, note) = match r {
            Ok(res) => {
                let ok = res.code == Some(0);
                let text = if res.output.trim().is_empty() { "(no output)".to_string() } else { res.output.clone() };
                (if ok { ToolStatus::Done } else { ToolStatus::Failed }, ToolContent::Terminal(text), Some(shell::context_note(&run.cmd, &res)))
            }
            Err(e) => (ToolStatus::Failed, ToolContent::Text(format!("Couldn't run {}: {e}", shell::name())), None),
        };
        if let Some(i) = self.entry_for_key(&run.key) {
            let e = &mut self.transcript.entries[i];
            if let Block::Tool(t) = &mut e.block {
                t.status = status;
                t.ended = Some(run.started.elapsed());
                t.content = vec![content];
            }
            e.touch();
        }
        match note {
            // Started in a session that's since been left (/new, /resume): keep it there.
            Some(_) if run.session != self.session.id => {}
            Some(n) => {
                self.pending_context.push(n);
                self.toast("Output saved: it goes to Claude with your next prompt");
            }
            None => {}
        }
    }

    // ------------------------------------------------------------ /btw

    fn start_btw(&mut self, question: String) {
        let (Some(client), Some(main)) = (self.client.clone(), self.session.id.clone()) else {
            self.toast("Claude Code isn't connected yet");
            return;
        };
        let caps = self.agent_caps.get("sessionCapabilities").cloned().unwrap_or(Value::Null);
        let (can_fork, can_close, can_delete) = (caps.get("fork").is_some(), caps.get("close").is_some(), caps.get("delete").is_some());
        let id = self.next_run_id();
        let key = format!("blush:btw:{id}");
        let idx = self.transcript.push(Block::Aside {
            question: question.clone(),
            answer: String::new(),
            started: Instant::now(),
            ended: None,
            failed: false,
        });
        self.transcript.tool_index.insert(key.clone(), idx);
        self.btw.push(Btw { id, sid: None, key, question: question.clone(), started: Instant::now() });
        self.follow = true;
        if !can_fork {
            self.toast("This agent can't fork sessions: answering without this conversation's context");
        }
        let prompt = format!(
            "Quick side question while I keep working. Answer briefly and directly from what you already know about \
             this conversation and project. Don't edit files or run commands.\n\n{question}"
        );
        let cwd = self.cwd.to_string_lossy().to_string();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let opened = if can_fork {
                client.request("session/fork", json!({"sessionId": main, "cwd": cwd, "mcpServers": []})).await
            } else {
                client.request("session/new", json!({"cwd": cwd, "mcpServers": []})).await
            };
            let sid = match opened {
                Ok(v) => s(&v, "sessionId"),
                Err(e) => {
                    let _ = tx.send(AppMsg::BtwDone(id, Err(e)));
                    return;
                }
            };
            let _ = tx.send(AppMsg::BtwStarted(id, sid.clone()));
            // Ask mode: anything that needs permission is refused (see on_request).
            let _ = client.request("session/set_mode", json!({"sessionId": sid, "modeId": "default"})).await;
            let r = client.request("session/prompt", json!({"sessionId": sid, "prompt": [acp::text_block(&prompt)]})).await;
            if can_close {
                let _ = client.request("session/close", json!({"sessionId": sid})).await;
            }
            if can_delete {
                let _ = client.request("session/delete", json!({"sessionId": sid})).await;
            }
            let _ = tx.send(AppMsg::BtwDone(id, r));
        });
    }

    pub(super) fn on_btw_update(&mut self, sid: String, u: &Value) {
        if s(u, "sessionUpdate") != "agent_message_chunk" {
            return;
        }
        let Some(text) = u.get("content").and_then(crate::model::content_text) else { return };
        let Some(key) = self.btw.iter().find(|b| b.sid.as_deref() == Some(sid.as_str())).map(|b| b.key.clone()) else { return };
        if let Some(i) = self.entry_for_key(&key) {
            let e = &mut self.transcript.entries[i];
            if let Block::Aside { answer, .. } = &mut e.block {
                answer.push_str(&text);
            }
            e.touch();
        }
    }

    pub(super) fn on_btw_done(&mut self, id: u64, r: Result<Value, crate::acp::RpcError>) {
        let Some(pos) = self.btw.iter().position(|b| b.id == id) else { return };
        let b = self.btw.remove(pos);
        if let Some(i) = self.entry_for_key(&b.key) {
            let e = &mut self.transcript.entries[i];
            if let Block::Aside { answer, ended, failed, started, .. } = &mut e.block {
                *ended = Some(started.elapsed());
                match &r {
                    Err(err) => {
                        *failed = true;
                        if answer.trim().is_empty() {
                            *answer = format!("Couldn't answer: {err}");
                        }
                    }
                    Ok(_) if answer.trim().is_empty() => *answer = "(no answer)".into(),
                    Ok(_) => {}
                }
            }
            e.touch();
        }
    }

    // ------------------------------------------------------------ /rename

    fn rename(&mut self, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            self.toast("Usage: /rename <title>");
            return;
        }
        let Some(sid) = self.session.id.clone() else {
            self.toast("No session to rename yet");
            return;
        };
        self.title_override = Some(title.to_string());
        save_local_title(&sid, title);
        self.update_title();
        let can_title = self.agent_caps.get("sessionCapabilities").and_then(|c| c.get("title")).is_some();
        if let (true, Some(client)) = (can_title, self.client.clone()) {
            let tx = self.tx.clone();
            let t = title.to_string();
            tokio::spawn(async move {
                let r = client.request("session/set_title", json!({"sessionId": sid, "title": t})).await;
                let _ = tx.send(AppMsg::TitleSet(r));
            });
        }
        self.toast(format!("Renamed to “{title}”"));
    }

    // ------------------------------------------------------------ display modes

    pub fn density(&self) -> &'static str {
        let ui = &self.config.ui;
        match (ui.compact, ui.diff_focus, ui.expand_tools) {
            (true, true, _) => "diff",
            (true, false, _) => "grouped",
            (false, _, true) => "detailed",
            _ => "balanced",
        }
    }

    pub(super) fn set_density(&mut self, name: &str) {
        let name = match name.trim().to_lowercase().as_str() {
            "detailed" | "detail" => "detailed",
            "balanced" | "default" => "balanced",
            "diff" | "diff-focus" | "diff focus" => "diff",
            "grouped" | "compact" => "grouped",
            other => {
                self.toast(format!("Unknown density `{other}`: detailed, balanced, diff or grouped"));
                return;
            }
        };
        let (compact, diff, expand) = match name {
            "detailed" => (false, false, true),
            "diff" => (true, true, false),
            "grouped" => (true, false, false),
            _ => (false, false, false),
        };
        self.config.ui.compact = compact;
        self.config.ui.diff_focus = diff;
        self.config.ui.expand_tools = expand;
        self.transcript.diff_focus = diff;
        self.transcript.expand_tools = expand;
        Config::persist(|c| {
            c.ui.compact = compact;
            c.ui.diff_focus = diff;
            c.ui.expand_tools = expand;
        });
        let fold = if expand { Fold::Expanded } else { Fold::Collapsed };
        for e in self.transcript.entries.iter_mut() {
            if matches!(e.block, Block::Tool(_)) && e.fold != fold {
                e.fold = fold;
                e.touch();
            }
        }
        // Keep the selection on something that's still shown.
        if let Some(sel) = self.selected {
            if let Some((s, _)) = self.group_of(sel).filter(|(s, _)| !self.transcript.entries[*s].group_open) {
                self.selected = Some(s);
            }
            self.ensure_selected_visible = true;
        }
        self.invalidate_all();
        self.toast(format!("Density: {name}"));
    }

    pub fn timestamp_mode(&self) -> &'static str {
        match (self.config.ui.show_timestamps, self.config.ui.timestamps_minimal) {
            (false, _) => "off",
            (true, true) => "minimal",
            (true, false) => "on",
        }
    }

    /// `/timestamps [on|minimal|off]`; no argument toggles.
    pub(super) fn set_timestamps(&mut self, arg: &str) {
        let mode = match arg.trim().to_lowercase().as_str() {
            "" => if self.config.ui.show_timestamps { "off" } else { "on" },
            "on" | "full" | "all" => "on",
            "minimal" | "min" | "prompts" => "minimal",
            "off" | "none" => "off",
            other => {
                self.toast(format!("Unknown timestamp mode `{other}`: on, minimal or off"));
                return;
            }
        };
        let (show, minimal) = match mode {
            "on" => (true, false),
            "minimal" => (true, true),
            _ => (false, false),
        };
        self.config.ui.show_timestamps = show;
        self.config.ui.timestamps_minimal = minimal;
        Config::persist(|c| {
            c.ui.show_timestamps = show;
            c.ui.timestamps_minimal = minimal;
        });
        self.invalidate_all();
        self.toast(match mode {
            "on" => "Timestamps on",
            "minimal" => "Timestamps on prompts only",
            _ => "Timestamps off",
        });
    }

    /// Alt+P toggles the todo pane, Ctrl+B the tasks pane.
    pub(super) fn toggle_pane(&mut self, todo: bool) {
        let on = if todo {
            self.config.ui.todo_pane = !self.config.ui.todo_pane;
            self.config.ui.todo_pane
        } else {
            self.config.ui.tasks_pane = !self.config.ui.tasks_pane;
            self.config.ui.tasks_pane
        };
        Config::persist(|c| if todo { c.ui.todo_pane = on } else { c.ui.tasks_pane = on });
        let name = if todo { "Todo pane" } else { "Tasks pane" };
        self.toast(format!("{name} {}", if on { "on" } else { "off" }));
    }

    /// What the tasks pane lists: running tools and subagents, background
    /// commands Claude started, a `!` command, side questions, queued prompts.
    pub fn task_rows(&self) -> Vec<TaskRow> {
        let mut rows = Vec::new();
        let mut background = Vec::new();
        for e in &self.transcript.entries {
            let Block::Tool(t) = &e.block else { continue };
            if t.id.starts_with("blush:") {
                continue;
            }
            let label = || {
                t.raw_input
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.lines().next().unwrap_or("").to_string())
                    .or_else(|| t.raw_input.get("description").and_then(|d| d.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| t.title.clone())
            };
            if e.is_live() {
                let kind = if t.kind == "think" { "subagent" } else { "running" };
                rows.push(TaskRow { kind, label: label(), since: Some(t.started.elapsed()) });
            } else if t.raw_input.get("run_in_background").and_then(|v| v.as_bool()) == Some(true) && t.status == ToolStatus::Done {
                background.push(TaskRow { kind: "background", label: label(), since: Some(t.started.elapsed()) });
            }
        }
        // Claude doesn't report when background commands end: show the latest few.
        let skip = background.len().saturating_sub(3);
        rows.extend(background.into_iter().skip(skip));
        if let Some(run) = &self.shell {
            rows.push(TaskRow { kind: "shell", label: format!("! {}", run.cmd), since: Some(run.started.elapsed()) });
        }
        for b in &self.btw {
            rows.push(TaskRow { kind: "btw", label: b.question.clone(), since: Some(b.started.elapsed()) });
        }
        for (o, _) in &self.queue {
            rows.push(TaskRow {
                kind: if self.queue_paused { "on hold" } else { "queued" },
                label: o.raw.lines().next().unwrap_or("").to_string(),
                since: None,
            });
        }
        rows
    }

    // ------------------------------------------------------------ settings

    pub(super) fn open_settings(&mut self) {
        self.picker_cancel();
        self.completion = None;
        self.overlay = Overlay::Settings { sel: 0 };
    }

    pub fn settings_items(&self) -> Vec<SettingsItem> {
        let onoff = |b: bool| if b { "on" } else { "off" }.to_string();
        let ui = &self.config.ui;
        let f = &self.config.features;
        vec![
            SettingsItem { key: "theme", label: "Theme", value: self.config.ui.theme.clone(), hint: "Enter opens the theme picker" },
            SettingsItem { key: "density", label: "Density", value: self.density().into(), hint: "how much of each tool call shows" },
            SettingsItem { key: "timestamps", label: "Timestamps", value: self.timestamp_mode().into(), hint: "on · minimal (prompts only) · off" },
            SettingsItem { key: "thinking", label: "Show thinking", value: onoff(self.transcript.show_thinking), hint: "Alt+H" },
            SettingsItem { key: "expand_thinking", label: "Expand thinking", value: onoff(ui.expand_thinking), hint: "new thinking blocks start open" },
            SettingsItem { key: "todo_pane", label: "Todo pane", value: onoff(ui.todo_pane), hint: "Alt+P · Claude's todo list, pinned" },
            SettingsItem { key: "tasks_pane", label: "Tasks pane", value: onoff(ui.tasks_pane), hint: "Ctrl+B · running and queued work" },
            SettingsItem { key: "notify", label: "Notifications", value: onoff(f.notify), hint: "chime when Claude needs you or finishes" },
            SettingsItem { key: "narrator", label: "Narrator", value: onoff(f.narrator), hint: "read each turn's summary aloud" },
            SettingsItem { key: "sub_meter", label: "Subscription meter", value: onoff(f.sub_meter), hint: "5-hour and weekly limits" },
            SettingsItem { key: "token_meter", label: "Token meter", value: onoff(f.token_meter), hint: "per-turn and session tokens" },
            SettingsItem { key: "timeline", label: "Time Machine", value: onoff(f.timeline), hint: "snapshot files after each turn" },
            SettingsItem { key: "mouse", label: "Mouse capture", value: onoff(ui.mouse), hint: "takes effect next launch" },
            SettingsItem { key: "auto_update", label: "Auto-update", value: onoff(self.config.update.auto), hint: "check GitHub on launch" },
        ]
    }

    /// Change setting `i`: `dir` is +1 (next / toggle) or -1 (previous).
    pub(super) fn settings_activate(&mut self, i: usize, dir: i32) {
        let items = self.settings_items();
        let Some(item) = items.get(i) else { return };
        let cycle = |list: &[&'static str], cur: &str| -> &'static str {
            let p = list.iter().position(|x| *x == cur).unwrap_or(0) as i32;
            let n = list.len() as i32;
            list[((p + dir).rem_euclid(n)) as usize]
        };
        match item.key {
            "theme" => {
                self.open_theme_picker();
                return;
            }
            "density" => {
                let next = cycle(&["detailed", "balanced", "diff", "grouped"], self.density());
                self.set_density(next);
            }
            "timestamps" => {
                let next = cycle(&["on", "minimal", "off"], self.timestamp_mode());
                self.set_timestamps(next);
            }
            "thinking" => {
                self.transcript.show_thinking = !self.transcript.show_thinking;
                let on = self.transcript.show_thinking;
                self.config.ui.show_thinking = on;
                Config::persist(|c| c.ui.show_thinking = on);
            }
            "expand_thinking" => {
                let on = !self.config.ui.expand_thinking;
                self.config.ui.expand_thinking = on;
                self.transcript.expand_thinking = on;
                Config::persist(|c| c.ui.expand_thinking = on);
            }
            "todo_pane" => self.toggle_pane(true),
            "tasks_pane" => self.toggle_pane(false),
            "notify" => {
                let on = !self.config.features.notify;
                self.config.features.notify = on;
                Config::persist(|c| c.features.notify = on);
            }
            "narrator" => self.toggle_narrator(false),
            "sub_meter" => {
                let on = !self.config.features.sub_meter;
                self.config.features.sub_meter = on;
                Config::persist(|c| c.features.sub_meter = on);
                if on {
                    self.maybe_refresh_usage();
                }
            }
            "token_meter" => {
                let on = !self.config.features.token_meter;
                self.config.features.token_meter = on;
                Config::persist(|c| c.features.token_meter = on);
            }
            "timeline" => {
                let on = !self.config.features.timeline;
                self.config.features.timeline = on;
                Config::persist(|c| c.features.timeline = on);
            }
            "mouse" => {
                // Applies next launch: switching capture mid-session would need a terminal reset.
                let on = !self.config.ui.mouse;
                self.config.ui.mouse = on;
                Config::persist(|c| c.ui.mouse = on);
                self.toast("Mouse capture changes on the next launch");
            }
            "auto_update" => {
                let on = !self.config.update.auto;
                self.config.update.auto = on;
                Config::persist(|c| c.update.auto = on);
            }
            _ => {}
        }
        self.invalidate_all();
    }

    // ------------------------------------------------------------ notifications

    /// Claude needs you (permission, question): play the chime.
    pub(super) fn attention(&mut self) {
        if self.config.features.notify {
            notify::chime();
        }
    }

    /// A turn finished. In the background: play the chime. If the terminal
    /// never reports focus: only after long turns.
    pub(super) fn on_turn_finished_notify(&mut self, took: Duration) {
        if !self.config.features.notify {
            return;
        }
        match self.focused {
            Some(false) => notify::chime(),
            None if took >= Duration::from_secs(20) => notify::chime(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Outgoing, PermissionCard, SessionKind};
    use crossterm::event::{Event, KeyEventKind, KeyEventState, KeyModifiers};

    /// Keep tests away from the real ~/.blush (config, history, titles).
    fn app() -> (App, tokio::sync::mpsc::UnboundedReceiver<AppMsg>) {
        static HOME: std::sync::Once = std::sync::Once::new();
        HOME.call_once(|| {
            let dir = std::env::temp_dir().join(format!("blush-test-home-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            // SAFETY: set once, before any test thread reads it.
            unsafe { std::env::set_var("BLUSH_HOME", &dir) };
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.features.timeline = false;
        cfg.features.notify = false;
        cfg.ui.show_timestamps = false;
        (App::new(cfg, std::env::temp_dir(), tx, SessionKind::New), rx)
    }

    /// Tests that save config.toml take turns: saves read, change and write the file.
    fn config_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE })
    }

    async fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_event(key(KeyCode::Char(c), KeyModifiers::NONE)).await;
        }
    }

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn web_qr_notice_renders_intact() {
        let (mut app, _rx) = app();
        let code = crate::web::qr("http://192.168.1.45:4777/?token=0123456789abcdef0123456789abcdef").unwrap();
        app.on_web_info(Ok(format!("blush web is up

{code}
Stop it with `blush web down`.")));
        let out = screen(&mut app, 120, 60);
        for row in code.lines() {
            assert!(out.contains(row.trim_end()), "QR row mangled: {row:?}
{out}");
        }
    }

    #[tokio::test]
    async fn web_sync_sends_snapshot_then_diffs() {
        let (mut app, _rx) = app();
        let (tx, mut out) = tokio::sync::mpsc::unbounded_channel::<String>();
        app.on_web_link(Some(tx));
        let first: Value = serde_json::from_str(&out.try_recv().unwrap()).unwrap();
        assert_eq!(first["t"], "snapshot");
        // An appended entry goes out as an upsert, with no reorder.
        let idx = app.transcript.push(Block::Agent { text: "hello".into() });
        app.web_sync();
        let m: Value = serde_json::from_str(&out.try_recv().unwrap()).unwrap();
        assert_eq!(m["t"], "upsert");
        assert_eq!(m["entries"][0]["text"], "hello");
        // Growing text re-sends just that entry.
        if let Block::Agent { text } = &mut app.transcript.entries[idx].block {
            text.push_str(" there");
        }
        app.transcript.entries[idx].touch();
        app.web_sync();
        let m: Value = serde_json::from_str(&out.try_recv().unwrap()).unwrap();
        assert_eq!(m["entries"].as_array().unwrap().len(), 1);
        assert_eq!(m["entries"][0]["text"], "hello there");
        // Nothing changed: nothing sent.
        app.web_sync();
        assert!(out.try_recv().is_err());
        // A removal sends the new order.
        app.transcript.entries.remove(idx);
        app.web_sync();
        let m: Value = serde_json::from_str(&out.try_recv().unwrap()).unwrap();
        assert_eq!(m["t"], "order");
        assert_eq!(m["uids"].as_array().unwrap().len(), app.transcript.entries.len());
    }

    #[tokio::test]
    async fn web_commands_answer_permissions_by_id() {
        let (mut app, _rx) = app();
        for n in 0..2 {
            app.permissions.push_back(PermissionCard {
                id: json!(n),
                title: format!("card {n}"),
                detail: vec![],
                options: vec![("ok".into(), "Allow".into(), "allow_once".into())],
                sel: 0,
                shown: Instant::now(),
            });
        }
        // Answering the second card leaves the first waiting.
        app.on_web_cmd(json!({"t": "permission", "id": 1, "optionId": "ok"})).await;
        assert_eq!(app.permissions.len(), 1);
        assert_eq!(app.permissions[0].id, json!(0));
    }

    #[tokio::test]
    async fn stash_sets_aside_and_restores() {
        let (mut app, _rx) = app();
        typ(&mut app, "half a thought").await;
        app.on_event(key(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        assert!(app.input.is_empty());
        assert!(screen(&mut app, 100, 24).contains("stashed"));
        app.on_event(key(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        assert_eq!(app.input.text, "half a thought");
        assert!(app.stash.is_none());
    }

    #[tokio::test]
    async fn find_selects_matches_newest_first() {
        let (mut app, _rx) = app();
        app.transcript.push(Block::Agent { text: "the needle is here".into() });
        app.transcript.push(Block::Agent { text: "nothing".into() });
        let last = app.transcript.push(Block::Agent { text: "another NEEDLE".into() });
        app.on_event(key(KeyCode::Char('f'), KeyModifiers::CONTROL)).await;
        typ(&mut app, "needle").await;
        assert_eq!(app.selected, Some(last));
        let scr = screen(&mut app, 100, 24);
        assert!(scr.contains("Find needle▏  2 of 2"), "{scr}");
        app.on_event(key(KeyCode::Up, KeyModifiers::NONE)).await;
        assert_eq!(app.selected, Some(last - 2));
        app.on_event(key(KeyCode::Esc, KeyModifiers::NONE)).await;
        assert!(app.search.is_none());
        assert_eq!(app.focus, Focus::Scrollback);
    }

    #[tokio::test]
    async fn palette_runs_commands() {
        let _guard = config_lock();
        let (mut app, _rx) = app();
        app.on_event(key(KeyCode::Char('x'), KeyModifiers::CONTROL)).await;
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.kind == PickerKind::Palette));
        typ(&mut app, "density").await;
        app.on_event(key(KeyCode::Enter, KeyModifiers::NONE)).await;
        // /density without arguments opens its own chooser.
        let Overlay::Picker(p) = &app.overlay else { panic!("no density picker") };
        assert_eq!(p.title, "Density");
        typ(&mut app, "detailed").await;
        app.on_event(key(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert_eq!(app.density(), "detailed");
        assert!(app.transcript.expand_tools);
    }

    #[tokio::test]
    async fn queued_prompts_come_back_for_editing() {
        let (mut app, _rx) = app();
        // Not connected: prompts queue.
        app.submit("first".into()).await;
        app.submit("_ second".into()).await;
        assert_eq!(app.queue.len(), 2);
        app.on_event(key(KeyCode::Up, KeyModifiers::NONE)).await;
        assert_eq!(app.input.text, "_ second");
        assert_eq!(app.queue.len(), 1);
        let queued = app.transcript.entries.iter().filter(|e| matches!(e.block, Block::User { queued: true, .. })).count();
        assert_eq!(queued, 1);
        // The remaining queued entry still points at its transcript row.
        let (_, idx) = &app.queue[0];
        assert!(matches!(&app.transcript.entries[*idx].block, Block::User { text, .. } if text == "first"));
    }

    #[tokio::test]
    async fn send_now_jumps_the_queue_without_holding_it() {
        let (mut app, _rx) = app();
        app.submit("later".into()).await;
        app.responding = true;
        typ(&mut app, "urgent").await;
        app.on_event(key(KeyCode::Enter, KeyModifiers::CONTROL)).await;
        assert_eq!(app.queue.front().map(|(o, _)| o.text.as_str()), Some("urgent"));
        assert_eq!(app.send_now, 1);
        typ(&mut app, "also urgent").await;
        app.on_event(key(KeyCode::Enter, KeyModifiers::CONTROL)).await;
        let order: Vec<&str> = app.queue.iter().map(|(o, _)| o.text.as_str()).collect();
        assert_eq!(order, ["urgent", "also urgent", "later"]);
        app.cancelling = true;
        app.on_msg(AppMsg::PromptDone(Ok(json!({"stopReason": "cancelled"})))).await;
        assert!(!app.queue_paused, "a send-now stop must not hold the queue");
        assert_eq!(app.send_now, 0);
    }

    #[tokio::test]
    async fn long_pastes_collapse_and_expand_on_send() {
        let (mut app, _rx) = app();
        let big = (0..30).map(|i| format!("row {i}")).collect::<Vec<_>>().join("\n");
        typ(&mut app, "explain ").await;
        app.on_event(Event::Paste(big.clone())).await;
        assert_eq!(app.input.text, "explain [Pasted text #1 +30 lines] ");
        let text = app.input.text.clone();
        let out = app.outgoing_from(&text);
        assert_eq!(out.text.trim_end(), format!("explain {big}"));
        assert!(app.attachments.items.is_empty());
    }

    #[tokio::test]
    async fn shell_output_goes_with_the_next_prompt() {
        let (mut app, mut rx) = app();
        app.submit("!echo blush-was-here".into()).await;
        assert!(app.shell.is_some());
        let msg = tokio::time::timeout(Duration::from_secs(60), rx.recv()).await.unwrap().unwrap();
        app.on_msg(msg).await;
        assert!(app.shell.is_none());
        let Some(Block::Tool(t)) = app.transcript.entries.last().map(|e| &e.block) else { panic!() };
        assert_eq!(t.status, ToolStatus::Done, "{:?}", t.content);
        assert_eq!(app.pending_context.len(), 1);
        let out: Outgoing = app.outgoing_from("what did that print?");
        assert!(out.context[0].contains("blush-was-here"));
        assert!(app.pending_context.is_empty());
        // A turn ending doesn't touch blush's own shell entries.
        app.transcript.end_turn(true);
        let Some(Block::Tool(t)) = app.transcript.entries.last().map(|e| &e.block) else { panic!() };
        assert_eq!(t.status, ToolStatus::Done);
    }

    #[tokio::test]
    async fn raw_markdown_toggle() {
        let (mut app, _rx) = app();
        let i = app.transcript.push(Block::Agent { text: "**bold** and `code`".into() });
        app.focus = Focus::Scrollback;
        app.selected = Some(i);
        assert!(!screen(&mut app, 100, 24).contains("**bold**"));
        app.on_event(key(KeyCode::Char('m'), KeyModifiers::NONE)).await;
        assert!(screen(&mut app, 100, 24).contains("**bold** and `code`"));
    }

    #[tokio::test]
    async fn todo_and_tasks_panes() {
        let _guard = config_lock();
        let (mut app, _rx) = app();
        let mut s = crate::model::Session::default();
        app.transcript.apply_update(
            &mut s,
            &json!({"sessionUpdate": "plan", "entries": [
                {"content": "Read the parser", "status": "completed"},
                {"content": "Fix the off-by-one", "status": "in_progress"},
                {"content": "Add a test", "status": "pending"}]}),
        );
        let scr = screen(&mut app, 100, 30);
        assert!(scr.contains("Todos  1/3") && scr.contains("◉ Fix the off-by-one"), "{scr}");
        app.on_event(key(KeyCode::Char('b'), KeyModifiers::CONTROL)).await;
        app.submit("queued thing".into()).await;
        let scr = screen(&mut app, 100, 30);
        assert!(scr.contains("Tasks  1") && scr.contains("queued     queued thing"), "{scr}");
        // Tiny terminals drop the panes rather than the scrollback.
        for h in 8u16..14 {
            screen(&mut app, 40, h);
        }
    }

    #[tokio::test]
    async fn settings_modal_toggles_and_saves() {
        let _guard = config_lock();
        let (mut app, _rx) = app();
        app.on_event(key(KeyCode::F(2), KeyModifiers::NONE)).await;
        let scr = screen(&mut app, 120, 34);
        assert!(scr.contains("Settings") && scr.contains("Todo pane"), "{scr}");
        let i = app.settings_items().iter().position(|it| it.key == "todo_pane").unwrap();
        app.overlay = Overlay::Settings { sel: i };
        app.on_event(key(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(!app.config.ui.todo_pane);
        assert!(!Config::load().ui.todo_pane, "saved to the (test) config file");
        app.on_event(key(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(app.config.ui.todo_pane);
        app.on_event(key(KeyCode::Esc, KeyModifiers::NONE)).await;
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[tokio::test]
    async fn rename_survives_automatic_titles() {
        let (mut app, _rx) = app();
        app.session.id = Some("sess-rename-test".into());
        app.submit("/rename Parser cleanup".into()).await;
        assert_eq!(app.session.title.as_deref(), Some("Parser cleanup"));
        assert_eq!(local_title("sess-rename-test").as_deref(), Some("Parser cleanup"));
        // An automatic title from the agent doesn't undo it.
        let mut s = std::mem::take(&mut app.session);
        let fx = app.transcript.apply_update(&mut s, &json!({"sessionUpdate": "session_info_update", "title": "auto"}));
        app.session = s;
        assert!(fx.title_changed);
        app.update_title();
        assert_eq!(app.session.title.as_deref(), Some("Parser cleanup"));
    }

    #[tokio::test]
    async fn btw_answers_stream_into_their_own_block() {
        let (mut app, _rx) = app();
        let idx = app.transcript.push(Block::Aside {
            question: "why?".into(),
            answer: String::new(),
            started: Instant::now(),
            ended: None,
            failed: false,
        });
        app.transcript.tool_index.insert("blush:btw:7".into(), idx);
        app.btw.push(Btw { id: 7, sid: None, key: "blush:btw:7".into(), question: "why?".into(), started: Instant::now() });
        app.on_msg(AppMsg::BtwStarted(7, "fork-1".into())).await;
        let chunk = |t: &str| {
            AppMsg::Acp(crate::acp::Incoming::Notification {
                method: "session/update".into(),
                params: json!({"sessionId": "fork-1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}}),
            })
        };
        app.session.id = Some("main".into());
        app.on_msg(chunk("Because ")).await;
        app.on_msg(chunk("reasons.")).await;
        app.on_msg(AppMsg::BtwDone(7, Ok(json!({})))).await;
        assert!(app.btw.is_empty());
        let Block::Aside { answer, ended, .. } = &app.transcript.entries[idx].block else { panic!() };
        assert_eq!(answer, "Because reasons.");
        assert!(ended.is_some());
        // Nothing leaked into the main thread.
        assert!(!app.transcript.entries.iter().any(|e| matches!(e.block, Block::Agent { .. })));
        let scr = screen(&mut app, 100, 24);
        assert!(scr.contains("btw why?") && scr.contains("Because reasons."), "{scr}");
    }

    #[test]
    fn key_floods_become_pastes() {
        let press = |c: KeyCode| key(c, KeyModifiers::NONE);
        let release = |c: KeyCode| Event::Key(KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Release, state: KeyEventState::NONE });
        let chars = |s: &str| -> Vec<Event> {
            s.chars().flat_map(|c| {
                let code = if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) };
                [press(code), release(code)]
            }).collect()
        };
        // Two lines: a paste, newline kept rather than sending.
        assert_eq!(paste_burst(&chars("fn a()\n}"), false).as_deref(), Some("fn a()\n}"));
        // Typing, even fast, or an IME committing a word: keys.
        assert_eq!(paste_burst(&chars("ok"), false), None);
        assert_eq!(paste_burst(&chars("日本語"), false), None);
        // A typed word and Enter that queued up during a slow frame still sends.
        assert_eq!(paste_burst(&chars("fix it\n"), false), None);
        // Long one-liners and the tail of a paste.
        assert_eq!(paste_burst(&chars("a long pasted line"), false).as_deref(), Some("a long pasted line"));
        assert_eq!(paste_burst(&chars("\n"), true).as_deref(), Some("\n"));
        // Shortcuts are never pastes.
        let mut b = chars("abcdefgh");
        b.push(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(paste_burst(&b, false), None);
        // Bracketed-paste markers that arrive as keys are dropped.
        let mut b = vec![press(KeyCode::Esc)];
        b.extend(chars("[200~two\nlines"));
        b.push(press(KeyCode::Esc));
        b.extend(chars("[201~"));
        assert_eq!(paste_burst(&b, false).as_deref(), Some("two\nlines"));
    }

    #[tokio::test]
    async fn pasted_lines_do_not_send_the_prompt() {
        let (mut app, _rx) = app();
        let batch: Vec<Event> = "line one\nline two".chars().map(|c| key(if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) }, KeyModifiers::NONE)).collect();
        app.on_events(batch).await;
        assert_eq!(app.input.text, "line one\nline two");
        assert!(app.queue.is_empty());
    }

    #[test]
    fn diff_focus_keeps_edits_out_of_groups() {
        let mut t = crate::model::Transcript::new();
        let mut s = crate::model::Session::default();
        let read = |id: &str| json!({"sessionUpdate":"tool_call","toolCallId":id,"title":"Read a","kind":"read","status":"completed"});
        let edit = json!({"sessionUpdate":"tool_call","toolCallId":"e","title":"Edit a","kind":"edit","status":"completed"});
        t.apply_update(&mut s, &read("1")); // 0
        t.apply_update(&mut s, &read("2")); // 1
        t.apply_update(&mut s, &edit); // 2
        t.apply_update(&mut s, &read("3")); // 3
        t.apply_update(&mut s, &read("4")); // 4
        assert_eq!(t.groups(), vec![(0, 5)]);
        t.diff_focus = true;
        assert_eq!(t.groups(), vec![(0, 2), (3, 5)]);
    }
}

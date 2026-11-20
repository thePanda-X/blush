//! Per-entry renderers: turn a scrollback [`Entry`] into styled lines.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::markdown;
use crate::model::{Block, Entry, Fold, Level, PlanItem, ToolCall, ToolContent, ToolStatus};
use crate::text::{hard_wrap, human_duration, sanitize, truncate, wrap_spans};
use crate::theme::{Theme, blend, wave};

pub const DIAMOND: &str = "◆";
pub const DIAMOND_HOLLOW: &str = "◇";
pub const PROMPT_ARROW: &str = "❯ ";
pub const RAIL: &str = "┃";
pub const SELECT_BAR: &str = "▎";
pub const BRAILLE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[allow(dead_code)]
pub struct Ctx<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    pub tick: u64,
    pub selected: bool,
}

/// Rendered entry: lines plus how the frame should decorate it.
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// Paint the whole row (including gutters) with the raised background.
    pub band: bool,
    /// Rows (relative) that react to clicks as a fold toggle.
    pub header_rows: usize,
}

pub fn render(entry: &Entry, ctx: &Ctx) -> Rendered {
    let th = ctx.theme;
    let w = ctx.width.max(10);
    match &entry.block {
        Block::Welcome => Rendered { lines: welcome(th, w), band: false, header_rows: 0 },
        Block::User { text, queued, tag } => {
            let mut lines = vec![Line::default()];
            let arrow_style = if *queued { th.dim() } else { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) };
            let text_style = if *queued { th.muted() } else { Style::default().fg(th.text).add_modifier(Modifier::BOLD) };
            let body = sanitize(text.trim_end());
            let mut wrapped = wrap_spans(
                vec![Span::styled(body, text_style)],
                w.saturating_sub(10),
                vec![Span::styled(PROMPT_ARROW, arrow_style)],
                vec![Span::raw("  ")],
            );
            if let Some(l) = wrapped.last_mut() {
                if let Some(tag) = tag {
                    l.spans.push(Span::styled(format!("  {tag}"), Style::default().fg(blend(th.accent, th.bg, 0.35))));
                }
                if *queued {
                    l.spans.push(Span::styled("  queued", Style::default().fg(th.gray_dim).add_modifier(Modifier::ITALIC)));
                }
            }
            lines.extend(wrapped);
            lines.push(Line::default());
            Rendered { lines, band: true, header_rows: 0 }
        }
        Block::Agent { text } if entry.raw => {
            // `m` in the scrollback: the markdown source, as Claude wrote it.
            let style = Style::default().fg(th.gray_bright);
            let mut lines = Vec::new();
            for l in entry.visible(text).trim_end().lines() {
                lines.extend(hard_wrap(vec![Span::styled(sanitize(l), style)], w.saturating_sub(10), vec![]));
            }
            Rendered { lines, band: false, header_rows: 0 }
        }
        Block::Agent { text } => {
            let lines = markdown::render(entry.visible(text), w.saturating_sub(10), th);
            Rendered { lines, band: false, header_rows: 0 }
        }
        Block::Aside { question, answer, started, ended, failed } => aside(question, answer, *started, *ended, *failed, ctx),
        Block::Thought { text, started, ended } => {
            let mut header = vec![bullet(th, ctx.tick, ended.is_none(), false, th.accent_thinking, entry.settling())];
            match ended {
                Some(d) => {
                    header.push(Span::styled("Thought", th.secondary()));
                    header.push(Span::styled(format!(" for {}", human_duration(*d)), th.muted()));
                }
                None => {
                    header.push(Span::styled("Thinking…", th.secondary()));
                    header.push(Span::styled(format!(" {}", human_duration(started.elapsed())), th.muted()));
                }
            }
            let mut lines = vec![Line::from(header)];
            if entry.fold == Fold::Expanded && !text.trim().is_empty() {
                let style = Style::default().fg(th.gray_bright).add_modifier(Modifier::ITALIC);
                for para in sanitize(entry.visible(text).trim()).split("\n\n") {
                    lines.extend(wrap_spans(
                        vec![Span::styled(para.replace('\n', " "), style)],
                        w,
                        vec![Span::raw("  ")],
                        vec![Span::raw("  ")],
                    ));
                    lines.push(Line::default());
                }
                lines.pop();
            }
            Rendered { lines, band: false, header_rows: 1 }
        }
        Block::Tool(t) => tool(entry, t, ctx),
        Block::Plan(items) => plan(items, th, w, entry.fold),
        Block::Notice { level, text } => {
            let (glyph, color) = match level {
                Level::Info => (DIAMOND_HOLLOW, th.gray),
                Level::Warn => ("⚠", th.warning),
                Level::Error => ("✗", th.error),
                Level::Success => ("✓", th.success),
            };
            let text_style = match level {
                Level::Info => th.muted(),
                _ => Style::default().fg(color),
            };
            let lines = wrap_spans(
                vec![Span::styled(sanitize(text), text_style)],
                w,
                vec![Span::styled(format!("{glyph} "), Style::default().fg(color))],
                vec![Span::raw("  ")],
            );
            Rendered { lines, band: false, header_rows: 0 }
        }
        Block::TurnEnd { reason } => Rendered {
            lines: vec![Line::from(vec![
                Span::styled(format!("{DIAMOND_HOLLOW} "), th.dim()),
                Span::styled(reason.clone(), Style::default().fg(th.gray).add_modifier(Modifier::ITALIC)),
            ])],
            band: false,
            header_rows: 0,
        },
    }
}

/// A `/btw` side question and its answer, set apart from the main thread.
fn aside(
    question: &str,
    answer: &str,
    started: std::time::Instant,
    ended: Option<std::time::Duration>,
    failed: bool,
    ctx: &Ctx,
) -> Rendered {
    let th = ctx.theme;
    let w = ctx.width.max(10);
    let live = ended.is_none();
    let mut header = vec![bullet(th, ctx.tick, live, failed, th.info, None), Span::styled("btw ", Style::default().fg(th.info).add_modifier(Modifier::BOLD))];
    let meta = match ended {
        Some(d) => format!("  {}", human_duration(d)),
        None => format!("  answering… {}", human_duration(started.elapsed())),
    };
    let q = sanitize(question.lines().next().unwrap_or(""));
    header.push(Span::styled(truncate(&q, w.saturating_sub(8 + crate::text::width(&meta))), th.secondary()));
    header.push(Span::styled(meta, th.dim()));
    let mut lines = vec![Line::from(header)];
    if !answer.trim().is_empty() {
        let body = if failed {
            wrap_spans(vec![Span::styled(sanitize(answer.trim()), Style::default().fg(th.error))], w.saturating_sub(2), vec![], vec![])
        } else {
            markdown::render(answer.trim(), w.saturating_sub(12), th)
        };
        let rail = Style::default().fg(blend(th.bg, th.info, 0.6));
        for mut l in body {
            l.spans.insert(0, Span::styled("│ ", rail));
            lines.push(l);
        }
    }
    Rendered { lines, band: false, header_rows: 0 }
}

/// The `◆` before a block: pulsing while live, then a brief flash (success
/// green, or a bright pop into red on failure) that settles to its rest color.
fn bullet(th: &Theme, tick: u64, live: bool, failed: bool, live_color: ratatui::style::Color, settle: Option<f32>) -> Span<'static> {
    let color = if let (false, Some(t)) = (live, settle) {
        let k = crate::anim::ease_in_out(t);
        if failed {
            blend(th.text, th.error, k)
        } else {
            let flash = if t < 0.15 { blend(live_color, th.success, t / 0.15) } else { th.success };
            blend(flash, th.gray, k)
        }
    } else if failed {
        th.error
    } else if live {
        blend(th.bg, live_color, wave(tick, 0))
    } else {
        th.gray
    };
    Span::styled(format!("{DIAMOND} "), Style::default().fg(color))
}

const LOGO: [&str; 8] = [
    " ███████████  ████                     █████     ",
    "▒▒███▒▒▒▒▒███▒▒███                    ▒▒███      ",
    " ▒███    ▒███ ▒███  █████ ████  █████  ▒███████  ",
    " ▒██████████  ▒███ ▒▒███ ▒███  ███▒▒   ▒███▒▒███ ",
    " ▒███▒▒▒▒▒███ ▒███  ▒███ ▒███ ▒▒█████  ▒███ ▒███ ",
    " ▒███    ▒███ ▒███  ▒███ ▒███  ▒▒▒▒███ ▒███ ▒███ ",
    " ███████████  █████ ▒▒████████ ██████  ████ █████",
    "▒▒▒▒▒▒▒▒▒▒▒  ▒▒▒▒▒   ▒▒▒▒▒▒▒▒ ▒▒▒▒▒▒  ▒▒▒▒ ▒▒▒▒▒ ",
];
const LOGO_SMALL: [&str; 2] = ["█▄▄ █   █ █ █▀ █ █", "█▄█ █▄▄ █▄█ ▄█ █▀█"];

/// One logo row, with solid blocks in `solid` and the `▒` shadow in `shade`.
fn logo_line(row: &str, solid: Style, shade: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_shade = false;
    for ch in row.chars() {
        let is_shade = ch == '▒';
        if is_shade != run_shade && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), if run_shade { shade } else { solid }));
        }
        run_shade = is_shade;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_shade { shade } else { solid }));
    }
    Line::from(spans)
}

fn welcome(th: &Theme, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    if w >= crate::text::width(LOGO[0]) {
        let solid = Style::default().fg(th.accent);
        let shade = Style::default().fg(blend(th.accent, th.bg, 0.6));
        lines.extend(LOGO.iter().map(|row| logo_line(row, solid, shade)));
    } else {
        for (i, l) in LOGO_SMALL.iter().enumerate() {
            let c = if i == 0 { th.accent } else { blend(th.accent, th.bg, 0.25) };
            lines.push(Line::from(vec![Span::styled(l.to_string(), Style::default().fg(c).add_modifier(Modifier::BOLD))]));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Claude Code", Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
        Span::styled(" over ACP", th.muted()),
        Span::styled(format!("  ·  v{}", env!("CARGO_PKG_VERSION")), th.dim()),
    ]));
    lines.push(Line::default());
    let tips: [(&str, &str); 4] = [
        ("/", "commands"),
        ("@", "mention files"),
        ("Shift+Tab", "switch permission mode"),
        ("Ctrl+.", "shortcuts"),
    ];
    let mut spans = Vec::new();
    for (i, (k, v)) in tips.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", th.dim()));
        }
        spans.push(Span::styled(k.to_string(), Style::default().fg(th.text_secondary).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {v}"), th.muted()));
    }
    lines.extend(wrap_spans(spans, w, vec![], vec![]));
    lines
}

fn plan(items: &[PlanItem], th: &Theme, w: usize, fold: Fold) -> Rendered {
    let done = items.iter().filter(|i| i.status == "completed").count();
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{DIAMOND} "), Style::default().fg(th.plan)),
        Span::styled("Plan", Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {done}/{}", items.len()), th.muted()),
    ])];
    if fold == Fold::Expanded {
        for item in items {
            let (glyph, gstyle, tstyle) = match item.status.as_str() {
                "completed" => ("✓ ", th.fg(th.success), Style::default().fg(th.gray).add_modifier(Modifier::CROSSED_OUT)),
                "in_progress" => ("◉ ", th.fg(th.accent), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
                _ => ("○ ", th.fg(th.gray), th.secondary()),
            };
            lines.extend(wrap_spans(
                vec![Span::styled(sanitize(&item.content), tstyle)],
                w,
                vec![Span::raw("  "), Span::styled(glyph, gstyle)],
                vec![Span::raw("    ")],
            ));
        }
    }
    Rendered { lines, band: false, header_rows: 1 }
}

const VERBS: &[&str] = &[
    "Read", "Edit", "Write", "Fetch", "Search", "Find", "List", "Grep", "Glob", "Delete", "Move", "Create",
    "Update", "Insert", "Replace", "Open", "View", "Run",
];

fn tool_label(t: &ToolCall, th: &Theme) -> Vec<Span<'static>> {
    let bold = Style::default().fg(th.text).add_modifier(Modifier::BOLD);
    let title = sanitize(t.title.lines().next().unwrap_or("").trim()).replace('`', "");
    if t.kind == "execute" {
        let cmd = t
            .raw_input
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| sanitize(c.lines().next().unwrap_or("")))
            .unwrap_or(title);
        return vec![Span::styled("Run ", bold), Span::styled(cmd, Style::default().fg(th.command))];
    }
    if let Some((verb, rest)) = title.split_once(' ') {
        if VERBS.contains(&verb) {
            let style = if matches!(t.kind.as_str(), "read" | "edit" | "delete" | "move") {
                Style::default().fg(th.path)
            } else {
                th.secondary()
            };
            return vec![Span::styled(format!("{verb} "), bold), Span::styled(rest.to_string(), style)];
        }
    }
    let prefix = match t.kind.as_str() {
        "think" => "Agent ",
        "switch_mode" => "",
        _ => "",
    };
    let mut v = Vec::new();
    if !prefix.is_empty() {
        v.push(Span::styled(prefix, bold));
        v.push(Span::styled(title, th.secondary()));
    } else {
        v.push(Span::styled(if title.is_empty() { "Tool".to_string() } else { title }, bold));
    }
    v
}

/// One summary line for a compact-mode group:
/// `◆ 4 tools · 2 thoughts · 3 edits +12 -3  8s` (plus live activity).
pub fn group_summary(members: &[&Entry], ctx: &Ctx, open: bool) -> Line<'static> {
    let th = ctx.theme;
    let (mut tools, mut thoughts, mut edits, mut failed) = (0usize, 0usize, 0usize, 0usize);
    let (mut ins, mut del) = (0usize, 0usize);
    let mut first: Option<std::time::Instant> = None;
    let mut last_end: Option<std::time::Instant> = None;
    let mut live: Option<Vec<Span<'static>>> = None;
    for e in members {
        let (start, dur) = match &e.block {
            Block::Thought { started, ended, .. } => {
                thoughts += 1;
                if ended.is_none() {
                    live = Some(vec![Span::styled("Thinking…", th.muted())]);
                }
                (*started, *ended)
            }
            Block::Tool(t) => {
                let counts = diff_counts(t);
                if matches!(t.kind.as_str(), "edit" | "delete" | "move") || counts.is_some() {
                    edits += 1;
                } else {
                    tools += 1;
                }
                if let Some((i, d)) = counts {
                    ins += i;
                    del += d;
                }
                if t.status == ToolStatus::Failed {
                    failed += 1;
                }
                if matches!(t.status, ToolStatus::Pending | ToolStatus::Running) {
                    live = Some(tool_label(t, th));
                }
                (t.started, t.ended)
            }
            _ => continue,
        };
        first = Some(first.map_or(start, |f| f.min(start)));
        let end = dur.map(|d| start + d).unwrap_or_else(std::time::Instant::now);
        last_end = Some(last_end.map_or(end, |l| l.max(end)));
    }
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if tools > 0 {
        parts.push(plural(tools, "tool", "tools"));
    }
    if thoughts > 0 {
        parts.push(plural(thoughts, "thought", "thoughts"));
    }
    if edits > 0 {
        parts.push(plural(edits, "edit", "edits"));
    }
    let mut spans = if open {
        vec![Span::styled("▾ ", Style::default().fg(blend(th.bg, th.accent, 0.7)))]
    } else {
        vec![bullet(th, ctx.tick, live.is_some(), false, th.accent, None)]
    };
    // Compact groups are tinted with the accent (pink in the default theme).
    let pink = Style::default().fg(if open { blend(th.bg, th.accent, 0.75) } else { th.accent });
    spans.push(Span::styled(parts.join(" · "), pink));
    if ins + del > 0 {
        spans.push(Span::styled(format!(" +{ins}"), Style::default().fg(th.diff_ins_fg)));
        spans.push(Span::styled(format!(" -{del}"), Style::default().fg(th.diff_del_fg)));
    }
    if let (Some(f), Some(l)) = (first, last_end) {
        let d = l.saturating_duration_since(f);
        if d.as_secs_f64() >= 1.0 {
            spans.push(Span::styled(format!("  {}", human_duration(d)), Style::default().fg(blend(th.bg, th.accent, 0.55))));
        }
    }
    if failed > 0 {
        spans.push(Span::styled(format!("  {failed} failed"), Style::default().fg(th.error)));
    }
    if let Some(activity) = live.filter(|_| !open) {
        let used: usize = spans.iter().map(|s| crate::text::width(&s.content)).sum();
        let budget = ctx.width.saturating_sub(used + 4);
        if budget > 8 {
            spans.push(Span::styled("  ", th.dim()));
            let mut left = budget;
            for mut s in activity {
                let w = crate::text::width(&s.content);
                if w > left {
                    s.content = truncate(&s.content, left).into();
                    spans.push(s);
                    break;
                }
                left -= w;
                spans.push(s);
            }
        }
    }
    Line::from(spans)
}

fn diff_counts(t: &ToolCall) -> Option<(usize, usize)> {
    let mut ins = 0;
    let mut del = 0;
    let mut any = false;
    for c in &t.content {
        if let ToolContent::Diff { old, new, .. } = c {
            any = true;
            let o = old.as_deref().unwrap_or("");
            let o = if o.is_empty() { String::new() } else { format!("{}\n", o.trim_end_matches('\n')) };
            let n = format!("{}\n", new.trim_end_matches('\n'));
            let d = similar::TextDiff::from_lines(o.as_str(), n.as_str());
            for ch in d.iter_all_changes() {
                match ch.tag() {
                    similar::ChangeTag::Insert => ins += 1,
                    similar::ChangeTag::Delete => del += 1,
                    _ => {}
                }
            }
        }
    }
    any.then_some((ins, del))
}

fn tool(entry: &Entry, t: &ToolCall, ctx: &Ctx) -> Rendered {
    let th = ctx.theme;
    let w = ctx.width;
    let live = matches!(t.status, ToolStatus::Pending | ToolStatus::Running);
    let failed = t.status == ToolStatus::Failed;
    let mut header = vec![bullet(th, ctx.tick, live, failed, th.accent, entry.settling())];
    let mut label = tool_label(t, th);
    // meta on the right
    let mut meta: Vec<Span<'static>> = Vec::new();
    if let Some((ins, del)) = diff_counts(t) {
        meta.push(Span::styled(format!(" +{ins}"), Style::default().fg(th.diff_ins_fg)));
        meta.push(Span::styled(format!(" -{del}"), Style::default().fg(th.diff_del_fg)));
    }
    if live {
        meta.push(Span::styled(format!("  {}", human_duration(t.started.elapsed())), th.dim()));
    } else if let Some(d) = t.ended {
        if d.as_secs_f64() >= 1.0 {
            meta.push(Span::styled(format!("  {}", human_duration(d)), th.dim()));
        }
    }
    if failed {
        meta.push(Span::styled("  failed", Style::default().fg(th.error)));
    }
    let meta_w: usize = meta.iter().map(|s| crate::text::width(&s.content)).sum();
    let budget = w.saturating_sub(2 + meta_w + 2);
    // truncate label to one row
    let mut used = 0;
    for span in label.iter_mut() {
        let sw = crate::text::width(&span.content);
        if used + sw > budget {
            span.content = truncate(&span.content, budget.saturating_sub(used)).into();
            used = budget;
        } else {
            used += sw;
        }
    }
    header.extend(label.into_iter().filter(|s| !s.content.is_empty()));
    header.extend(meta);
    let mut lines = vec![Line::from(header)];

    let expanded = entry.fold == Fold::Expanded;
    let body_w = w.saturating_sub(2);
    let indent = || vec![Span::raw("  ")];

    // Description for shell commands (Claude's Bash `description`).
    if t.kind == "execute" {
        if let Some(desc) = t.raw_input.get("description").and_then(|d| d.as_str()) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(truncate(&sanitize(desc), body_w), th.muted()),
            ]));
        }
    }

    let mut has_diff = false;
    for c in &t.content {
        match c {
            ToolContent::Diff { path, old, new } => {
                has_diff = true;
                let max = if expanded { usize::MAX } else { 14 };
                lines.extend(render_diff(path, old.as_deref(), new, t.line, th, w, max));
            }
            ToolContent::Text(text) | ToolContent::Terminal(text) => {
                if !expanded && !(failed && !text.trim().is_empty()) {
                    continue;
                }
                let text = strip_fences(text);
                if text.trim().is_empty() {
                    continue;
                }
                let all: Vec<&str> = text.lines().collect();
                let limit = if expanded { 400 } else { 3 };
                let style = if failed { Style::default().fg(th.error) } else { Style::default().fg(th.gray_bright) };
                for l in all.iter().take(limit) {
                    lines.extend(hard_wrap(vec![Span::styled(sanitize(l), style)], w, indent()));
                }
                if all.len() > limit {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("… {} more lines", all.len() - limit), th.dim()),
                    ]));
                }
            }
        }
    }

    if expanded && t.content.is_empty() && !t.raw_input.is_null() && !has_diff {
        let pretty = pretty_input(&t.raw_input);
        for l in pretty.lines().take(60) {
            lines.extend(hard_wrap(vec![Span::styled(sanitize(l), th.muted())], w, indent()));
        }
    }

    Rendered { lines, band: false, header_rows: 1 }
}

fn pretty_input(v: &Value) -> String {
    match v {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k}: {s}"),
                other => format!("{k}: {other}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn strip_fences(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
        return rest.trim_end().trim_end_matches("```").trim_end().to_string();
    }
    s.to_string()
}

pub fn render_diff(
    path: &str,
    old: Option<&str>,
    new: &str,
    start_line: Option<u64>,
    th: &Theme,
    w: usize,
    max_lines: usize,
) -> Vec<Line<'static>> {
    use similar::{ChangeTag, TextDiff};
    // Normalize trailing newlines so an unchanged last line isn't shown as replaced.
    let old_n = format!("{}\n", old.unwrap_or("").trim_end_matches('\n'));
    let new_n = format!("{}\n", new.trim_end_matches('\n'));
    let old_n = if old.unwrap_or("").is_empty() { String::new() } else { old_n };
    let (old, new) = (old_n.as_str(), new_n.as_str());
    let diff = TextDiff::from_lines(old, new);
    let base = start_line.unwrap_or(1).max(1) as usize;
    let max_no = base + old.lines().count().max(new.lines().count()) + 1;
    let gw = max_no.to_string().len();
    let lang = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    let no_bg = matches!(th.bg, ratatui::style::Color::Reset);
    let mut out = Vec::new();
    let mut count = 0;
    let groups = diff.grouped_ops(2);
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            let prev_end = groups[gi - 1].last().map(|o| o.old_range().end).unwrap_or(0);
            let this_start = group.first().map(|o| o.old_range().start).unwrap_or(0);
            let gap = this_start.saturating_sub(prev_end);
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("… {gap} unchanged lines"), th.dim()),
            ]));
        }
        for op in group {
            for change in diff.iter_changes(op) {
                if count >= max_lines {
                    let remaining: usize = groups
                        .iter()
                        .flat_map(|g| g.iter())
                        .map(|o| diff.iter_changes(o).count())
                        .sum::<usize>()
                        .saturating_sub(count);
                    out.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("… {remaining} more lines (→ to expand)"), th.dim()),
                    ]));
                    return out;
                }
                count += 1;
                let (no, bg, fg, sign) = match change.tag() {
                    ChangeTag::Delete => (change.old_index().map(|i| i + base), Some(th.diff_del_bg), th.diff_del_fg, "-"),
                    ChangeTag::Insert => (change.new_index().map(|i| i + base), Some(th.diff_ins_bg), th.diff_ins_fg, "+"),
                    ChangeTag::Equal => (change.new_index().map(|i| i + base), None, th.gray, " "),
                };
                let text = sanitize(change.value().trim_end_matches('\n'));
                let mut code: Vec<Span<'static>> = if change.tag() == ChangeTag::Equal {
                    vec![Span::styled(text, Style::default().fg(th.gray))]
                } else {
                    crate::markdown::highlight(&text, &lang, th).into_iter().next().unwrap_or_default()
                };
                let row_style = bg.map(|b| Style::default().bg(b)).unwrap_or_default();
                for s in code.iter_mut() {
                    s.style = s.style.patch(row_style);
                }
                let gutter = format!("  {:>gw$}  ", no.map(|n| n.to_string()).unwrap_or_default());
                let mut prefix = vec![Span::styled(gutter, Style::default().fg(if bg.is_some() { fg } else { th.gray_dim }).patch(row_style))];
                if no_bg {
                    prefix.push(Span::styled(sign.to_string(), Style::default().fg(fg)));
                }
                for l in hard_wrap(code, w, prefix) {
                    out.push(if bg.is_some() { crate::text::pad_line(l, w, row_style) } else { l });
                }
            }
        }
    }
    out
}

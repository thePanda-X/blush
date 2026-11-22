//! Frame layout and drawing.

use std::rc::Rc;
use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as WBlock, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, Completion, Conn, FieldKind, Focus, LayoutInfo, Overlay};
use crate::anim::{self, ease_out, fade_rect, progress, tick_ms};
use crate::blocks::{self, BRAILLE, DIAMOND, PROMPT_ARROW, RAIL, SELECT_BAR};
use crate::model::{Block, Entry, Fold, RenderCache, ToolStatus, short_mode};
use crate::text::{human_duration, human_tokens, line_width, sanitize, truncate, truncate_left, wrap_spans};
use crate::usage::until;
use crate::theme::{Theme, blend, wave};


/// How long an unfolding block takes to roll open.
pub const UNFOLD: Duration = Duration::from_millis(180);
/// How long popups, menus and cards take to fade in.
pub const POPUP_FADE: Duration = Duration::from_millis(160);
/// The footer's subscription meter alternates windows this often…
const METER_PERIOD_MS: u64 = 6000;
/// …crossfading over this long at each end.
const METER_FADE_MS: u64 = 260;

/// Whether the footer meter is mid-crossfade (needs redraws).
pub fn meter_fading(tick: u64) -> bool {
    meter_alpha(tick) < 1.0
}

fn meter_alpha(tick: u64) -> f32 {
    let p = tick_ms(tick) % METER_PERIOD_MS;
    let edge = p.min(METER_PERIOD_MS - p);
    ease_out((edge as f32 / METER_FADE_MS as f32).min(1.0))
}

/// Mark which popup is showing, restarting its fade when it changes.
fn track_popup(slot: &mut (u8, std::time::Instant), key: u8) -> f32 {
    if slot.0 != key {
        *slot = (key, std::time::Instant::now());
    }
    ease_out(progress(slot.1, POPUP_FADE))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let th = app.theme.clone();
    let full = f.area();
    f.render_widget(WBlock::default().style(th.base()), full);
    if full.width < 20 || full.height < 8 {
        f.render_widget(Paragraph::new("terminal too small").style(th.muted()), full);
        return;
    }
    let area = Rect { x: full.x + 1, y: full.y, width: full.width - 2, height: full.height };

    // ---- vertical layout ----
    let prompt_inner_w = area.width.saturating_sub(4 + 2) as usize; // borders + padding + arrow
    let footer_h = 1;
    let status_h = 1;
    let header_h = 1;
    let gap = 1;
    let chrome = header_h + gap + status_h + footer_h;
    let card_lines = card_lines(app, &th, area.width.saturating_sub(4) as usize);
    // Everything below must fit in `area.height`: the prompt gives up rows
    // first (it scrolls to the cursor), then the scrollback, and a pending
    // card always keeps at least its frame plus one line.
    let card_min: u16 = if card_lines.is_empty() { 0 } else { 3 };
    let max_input = area.height.saturating_sub(chrome + 2 + card_min + 1).clamp(1, 10) as usize;
    let input_rows = app.input.layout(prompt_inner_w.max(1)).len().clamp(1, max_input) as u16;
    let prompt_h = input_rows + 2;
    let room = area.height.saturating_sub(chrome + prompt_h);
    let card_h = if card_lines.is_empty() {
        0
    } else {
        let want = card_lines.len() as u16 + 2;
        // Leave a few scrollback rows when there's space, but never squeeze
        // the card below a usable size.
        want.min(room.saturating_sub(4).max(want.min(8))).min(room)
    };
    let fixed = header_h + gap + status_h + card_h + prompt_h + footer_h;
    // Pinned panes (todos, tasks) sit above the status row, but never take
    // the scrollback below a handful of rows.
    let pane_lines = pane_lines(app, &th, area.width.saturating_sub(4) as usize);
    let pane_h = (pane_lines.len() as u16).min(area.height.saturating_sub(fixed).saturating_sub(6)).min(10);
    let scroll_h = area.height.saturating_sub(fixed + pane_h);

    let header = Rect { x: area.x, y: area.y, width: area.width, height: header_h };
    let scroll = Rect { x: area.x, y: header.y + header_h + gap, width: area.width, height: scroll_h };
    let pane = Rect { x: area.x, y: scroll.y + scroll_h, width: area.width, height: pane_h };
    let status = Rect { x: area.x, y: pane.y + pane_h, width: area.width, height: status_h };
    let card = Rect { x: area.x, y: status.y + status_h, width: area.width, height: card_h };
    let prompt = Rect { x: area.x, y: card.y + card_h, width: area.width, height: prompt_h };
    let footer = Rect { x: area.x, y: prompt.y + prompt_h, width: area.width, height: footer_h };

    let mut layout = LayoutInfo {
        scroll_area: scroll,
        prompt_area: prompt,
        prompt_inner_width: prompt_inner_w as u16,
        ..Default::default()
    };

    draw_header(f, app, &th, header);
    draw_scrollback(f, app, &th, scroll, &mut layout);
    if pane_h > 0 {
        let lines: Vec<Line> = pane_lines.into_iter().take(pane_h as usize).collect();
        f.render_widget(Paragraph::new(lines), Rect { x: pane.x + 2, width: pane.width.saturating_sub(3), ..pane });
    }
    draw_status(f, app, &th, status, &mut layout);
    if card_h > 0 {
        draw_card(f, app, &th, card, card_lines);
        layout.card_area = Some(card);
    }
    let card_key = if card_h == 0 { 0 } else if !app.permissions.is_empty() { 1 } else { 2 };
    let card_alpha = track_popup(&mut app.popup[1], card_key);
    if card_h > 0 {
        fade_rect(f.buffer_mut(), card, th.bg, card_alpha);
    }
    draw_prompt(f, app, &th, prompt, prompt_inner_w);
    draw_footer(f, app, &th, footer, &mut layout);
    draw_completion(f, app, &th, prompt, &mut layout);

    match &app.overlay {
        Overlay::None => {}
        Overlay::Picker(_) => draw_picker(f, app, &th, full, &mut layout),
        Overlay::Shortcuts => draw_shortcuts(f, app, &th, full, &mut layout),
        Overlay::Viewer { .. } => draw_viewer(f, app, &th, full, &mut layout),
        Overlay::Settings { .. } => draw_settings(f, app, &th, full, &mut layout),
        Overlay::Tasks(_) => draw_bg_tasks(f, app, &th, full, &mut layout),
    }
    let popup_key = match &app.overlay {
        Overlay::None if layout.popup.is_some() => 1,
        Overlay::None => 0,
        Overlay::Picker(_) => 2,
        Overlay::Shortcuts => 3,
        Overlay::Viewer { .. } => 4,
        Overlay::Settings { .. } => 5,
        Overlay::Tasks(_) => 6,
    };
    let alpha = track_popup(&mut app.popup[0], popup_key);
    if let Some(r) = layout.popup {
        fade_rect(f.buffer_mut(), r, th.bg, alpha);
    }
    app.layout = layout;
}

// ---------------------------------------------------------------- header

fn draw_header(f: &mut Frame, app: &App, th: &Theme, r: Rect) {
    let cwd = app.git_label.clone().unwrap_or_else(|| display_path(&app.cwd));
    let used = app.tweens.header_used.get(app.session.used as f64).round() as u64;
    let right = if app.session.size > 0 {
        format!("{} / {}", human_tokens(used), human_tokens(app.session.size))
    } else {
        String::new()
    };
    let left_max = (r.width as usize).saturating_sub(right.len() + 2);
    let mut spans = vec![Span::styled(truncate_left(&cwd, left_max), th.muted())];
    if let Some(t) = &app.session.title {
        let used = crate::text::width(&cwd) + 3;
        if used + 10 < left_max {
            spans.push(Span::styled(" · ", th.dim()));
            spans.push(Span::styled(truncate(t, left_max - used), th.dim()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
    if !right.is_empty() {
        let pct = used as f64 / app.session.size.max(1) as f64;
        let color = if pct > 0.85 { th.error } else if pct > 0.65 { th.warning } else { th.text };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(right, Style::default().fg(color)))).alignment(Alignment::Right),
            r,
        );
    }
}

pub fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy().to_string();
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{}", rest.replace('\\', "/"));
        }
    }
    s.replace('\\', "/")
}

/// `repo:branch` if `cwd` is inside a git repo. Reads `.git/HEAD` directly
/// (no subprocess) so it's cheap enough to poll. Handles worktrees/submodules
/// (`.git` file with `gitdir:`) and detached HEAD (short sha).
pub fn git_label(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let dot = d.join(".git");
        let git_dir = if dot.is_dir() {
            dot
        } else if dot.is_file() {
            let s = std::fs::read_to_string(&dot).ok()?;
            let p = std::path::PathBuf::from(s.trim().strip_prefix("gitdir:")?.trim());
            if p.is_absolute() { p } else { d.join(p) }
        } else {
            dir = d.parent();
            continue;
        };
        let name = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| display_path(d));
        let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
        let head = head.trim();
        let branch = match head.strip_prefix("ref:") {
            Some(r) => r.trim().strip_prefix("refs/heads/").unwrap_or(r.trim()).to_string(),
            None => head.chars().take(7).collect(),
        };
        return Some(format!("{name}:{branch}"));
    }
    None
}

// ---------------------------------------------------------------- scrollback

fn entry_lines(entry: &Entry, width: u16, th: &Theme, tick: u64, show_ts: bool, ts_minimal: bool) -> (Rc<Vec<Line<'static>>>, bool, usize) {
    let live = entry.is_live() || entry.shown.is_some() || entry.settling().is_some();
    if !live {
        if let Some(c) = entry.cache.borrow().as_ref() {
            if c.width == width && c.rev == entry.rev && c.fold == entry.fold {
                let (band, hdr) = meta_for(entry);
                return (c.lines.clone(), band, hdr);
            }
        }
    }
    let ctx = blocks::Ctx { theme: th, width: width as usize, tick, selected: false };
    let mut r = blocks::render(entry, &ctx);
    if show_ts {
        let ts = entry.time.format("%-I:%M %p").to_string();
        let row = match entry.block {
            Block::User { .. } => Some(1),
            Block::Agent { .. } if !ts_minimal => Some(0),
            _ => None,
        };
        if let Some(row) = row {
            if let Some(line) = r.lines.get_mut(row) {
                let w = line_width(line);
                let target = (width as usize).saturating_sub(ts.len());
                if w + 2 <= target {
                    line.spans.push(Span::raw(" ".repeat(target - w)));
                    line.spans.push(Span::styled(ts, th.dim()));
                }
            }
        }
    }
    let lines = Rc::new(r.lines);
    if !live {
        *entry.cache.borrow_mut() =
            Some(RenderCache { width, rev: entry.rev, fold: entry.fold, selected: false, lines: lines.clone() });
    }
    (lines, r.band, r.header_rows)
}

// ---------------------------------------------------------------- panes

/// The todo pane (Claude's latest todo list while it has open items) and
/// the tasks pane (running and queued work), stacked.
fn pane_lines(app: &App, th: &Theme, w: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if app.config.ui.todo_pane {
        if let Some(items) = app.transcript.latest_plan() {
            let open = items.iter().any(|i| i.status != "completed");
            if open || app.responding {
                let done = items.iter().filter(|i| i.status == "completed").count();
                out.push(Line::from(vec![
                    Span::styled(format!("{DIAMOND} "), Style::default().fg(th.plan)),
                    Span::styled("Todos", Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {done}/{}", items.len()), th.muted()),
                    Span::styled("   Alt+P hides", th.dim()),
                ]));
                // A window of five around the item in progress.
                let active = items
                    .iter()
                    .position(|i| i.status == "in_progress")
                    .or_else(|| items.iter().position(|i| i.status != "completed"))
                    .unwrap_or(items.len().saturating_sub(1));
                let start = active.saturating_sub(1).min(items.len().saturating_sub(5));
                for item in items.iter().skip(start).take(5) {
                    let (glyph, gstyle, tstyle) = match item.status.as_str() {
                        "completed" => ("✓ ", th.fg(th.success), Style::default().fg(th.gray).add_modifier(Modifier::CROSSED_OUT)),
                        "in_progress" => ("◉ ", th.fg(th.accent), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
                        _ => ("○ ", th.fg(th.gray), th.secondary()),
                    };
                    out.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(glyph, gstyle),
                        Span::styled(truncate(&sanitize(&item.content), w.saturating_sub(5)), tstyle),
                    ]));
                }
            }
        }
    }
    if app.config.ui.tasks_pane {
        let rows = app.task_rows();
        out.push(Line::from(vec![
            Span::styled(format!("{DIAMOND} "), th.accent()),
            Span::styled("Tasks", Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
            Span::styled(if rows.is_empty() { String::new() } else { format!("  {}", rows.len()) }, th.muted()),
            Span::styled(if app.transcript.bg_seen { "   Ctrl+B hides · Alt+J output" } else { "   Ctrl+B hides" }, th.dim()),
        ]));
        if rows.is_empty() {
            out.push(Line::from(Span::styled("  nothing running", th.dim())));
        }
        for r in rows.iter().take(5) {
            let color = match r.kind {
                "running" | "subagent" => th.accent,
                "shell" => th.command,
                "btw" => th.info,
                "background" | "done" => th.success,
                "working" | "paused" => th.accent,
                "failed" | "stopped" => th.error,
                _ => th.gray,
            };
            let since = r.since.map(|d| format!("  {}", human_duration_short(d))).unwrap_or_default();
            let room = w.saturating_sub(14 + crate::text::width(&since));
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<11}", r.kind), Style::default().fg(color)),
                Span::styled(truncate(&sanitize(&r.label), room), th.secondary()),
                Span::styled(since, th.dim()),
            ]));
        }
        if rows.len() > 5 {
            out.push(Line::from(Span::styled(format!("  +{} more · /tasks lists them all", rows.len() - 5), th.dim())));
        }
    }
    out
}

fn meta_for(entry: &Entry) -> (bool, usize) {
    match entry.block {
        Block::User { .. } => (true, 0),
        Block::Thought { .. } | Block::Tool(_) | Block::Plan(_) => (false, 1),
        _ => (false, 0),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Entry,
    /// A collapsed compact-mode group (one summary line, selectable).
    Group,
    /// The header line above an expanded group (click to collapse).
    GroupHeader,
}

fn compact(b: &Block) -> bool {
    matches!(b, Block::Tool(_) | Block::Thought { .. })
}

fn draw_scrollback(f: &mut Frame, app: &mut App, th: &Theme, r: Rect, layout: &mut LayoutInfo) {
    if r.height == 0 {
        return;
    }
    let content_w = r.width.saturating_sub(4); // rail(1) + pad(1) + ... + pad(1) + scrollbar(1)
    let show_ts = app.config.ui.show_timestamps;
    let ts_minimal = app.config.ui.timestamps_minimal;
    let tick = app.tick;

    // Gather (entry index, gap_before, lines, band, header_rows, kind)
    let mut items: Vec<(usize, usize, Rc<Vec<Line<'static>>>, bool, usize, ItemKind)> = Vec::new();
    let mut prev: Option<&Block> = None;
    let mut total = 0usize;
    let groups = if app.config.ui.compact { app.transcript.groups() } else { vec![] };
    let mut gi = 0;
    let mut skip_until = 0;
    for (i, e) in app.transcript.entries.iter().enumerate() {
        if i < skip_until {
            continue;
        }
        if !app.transcript.show_thinking && matches!(e.block, Block::Thought { .. }) {
            continue;
        }
        while gi < groups.len() && groups[gi].1 <= i {
            gi += 1;
        }
        let mut first_in_open_group = false;
        if let Some(&(s, end)) = groups.get(gi).filter(|g| g.0 == i) {
            let open = e.group_open;
            let members = app.transcript.group_members((s, end));
            let ctx = blocks::Ctx { theme: th, width: content_w as usize, tick, selected: false };
            let lines = Rc::new(vec![blocks::group_summary(&members, &ctx, open)]);
            let gap = if prev.is_none() { 0 } else { 1 };
            total += gap + 1;
            items.push((i, gap, lines, false, 1, if open { ItemKind::GroupHeader } else { ItemKind::Group }));
            prev = Some(&e.block);
            if !open {
                skip_until = end;
                continue;
            }
            first_in_open_group = true;
        }
        let (mut lines, band, hdr) = entry_lines(e, content_w, th, tick, show_ts, ts_minimal);
        if lines.is_empty() {
            continue;
        }
        lines = unroll(e, lines, hdr);
        let gap = match prev {
            None => 0,
            _ if first_in_open_group => 0,
            Some(p) if compact(p) && compact(&e.block) && e.fold == Fold::Collapsed => 0,
            Some(p) if compact(p) && compact(&e.block) => 1,
            _ => 1,
        };
        total += gap + lines.len();
        items.push((i, gap, lines, band, hdr, ItemKind::Entry));
        prev = Some(&e.block);
    }
    let view_h = r.height as usize;
    // Detached from the bottom: `scroll` counts lines up from the bottom, so
    // hold the top row still while the stream (or a resize) changes what's
    // below it. Otherwise new output drags the text you're reading away.
    let (prev_total, prev_h) = (app.layout.total_lines, app.layout.view_height);
    if !app.follow && prev_total > 0 && (prev_total, prev_h) != (total, view_h) {
        let top = prev_total.saturating_sub(prev_h + app.scroll);
        let anchored = total.saturating_sub(view_h + top);
        app.scroll_shown += anchored as f32 - app.scroll as f32;
        app.scroll = anchored;
    }
    layout.total_lines = total;
    layout.view_height = view_h;

    // Keep the selection on screen.
    if app.ensure_selected_visible {
        app.ensure_selected_visible = false;
        if let Some(sel) = app.selected {
            let mut y = 0usize;
            for (i, gap, lines, _, _, kind) in &items {
                y += gap;
                if *i == sel && *kind != ItemKind::GroupHeader {
                    let top = total.saturating_sub(view_h + app.scroll);
                    let bottom = top + view_h;
                    let (s, e) = (y, y + lines.len());
                    if s < top {
                        app.scroll = total.saturating_sub(view_h + s);
                    } else if e > bottom {
                        let new_top = if lines.len() > view_h { s } else { e - view_h };
                        app.scroll = total.saturating_sub(view_h + new_top);
                    }
                    break;
                }
                y += lines.len();
            }
        }
    }
    if app.follow {
        app.scroll = 0;
    }
    let max_scroll = total.saturating_sub(view_h);
    app.scroll = app.scroll.min(max_scroll);
    // Glide toward the scroll target rather than jumping there.
    let now = std::time::Instant::now();
    let dt = now.duration_since(app.scroll_at).as_secs_f32().min(0.05);
    app.scroll_at = now;
    let target = app.scroll as f32;
    app.scroll_shown += (target - app.scroll_shown) * (1.0 - (-dt / 0.06).exp());
    if (target - app.scroll_shown).abs() < 0.5 {
        app.scroll_shown = target;
    }
    app.scroll_shown = app.scroll_shown.clamp(0.0, max_scroll as f32);
    let top = total.saturating_sub(view_h + app.scroll_shown.round() as usize);
    // When content is shorter than the view, pin it to the top.
    let buf = f.buffer_mut();
    let x0 = r.x;
    let cx = r.x + 2;
    let mut y = 0usize;
    for (idx, gap, lines, band, hdr, kind) in &items {
        y += gap;
        let start = y;
        let end = y + lines.len();
        y = end;
        if end <= top || start >= top + view_h {
            continue;
        }
        let entry = &app.transcript.entries[*idx];
        let selected = app.selected == Some(*idx) && app.focus == Focus::Scrollback && *kind != ItemKind::GroupHeader;
        let first_vis = start.max(top);
        let last_vis = end.min(top + view_h);
        let sy0 = r.y + (first_vis - top) as u16;
        let sy1 = r.y + (last_vis - top) as u16;
        if *kind == ItemKind::GroupHeader {
            layout.group_rows.push((sy0, *idx));
            buf.set_line(cx, sy0, &lines[0], content_w);
            continue;
        }
        if *kind == ItemKind::Group {
            layout.entry_rows.push((sy0, sy1, *idx, *hdr, first_vis - start));
            buf.set_line(cx, sy0, &lines[0], content_w);
            if selected {
                buf.set_string(x0, sy0, SELECT_BAR, Style::default().fg(th.accent));
                if let Some(cell) = buf.cell_mut((cx, sy0)) {
                    if cell.symbol() == DIAMOND {
                        cell.set_symbol("›").set_fg(th.accent);
                    }
                }
            }
            continue;
        }
        layout.entry_rows.push((sy0, sy1, *idx, *hdr, first_vis - start));
        let md = match &entry.block {
            Block::Agent { text } if !entry.raw => Some(entry.visible(text)),
            Block::Aside { answer, .. } => Some(answer.trim()),
            _ => None,
        };
        for (li, x0, x1, url) in crate::links::scan(lines, md) {
            let row = start + li;
            if row >= first_vis && row < last_vis && x0 < content_w {
                let sy = r.y + (row - top) as u16;
                layout.links.push((sy, cx + x0, cx + x1.min(content_w), url));
            }
        }
        for row in first_vis..last_vis {
            let sy = r.y + (row - top) as u16;
            let li = row - start;
            if *band {
                let bg = if selected { th.bg_hover } else { th.bg_raised };
                buf.set_style(Rect { x: x0, y: sy, width: r.width - 1, height: 1 }, Style::default().bg(bg));
            }
            let line = &lines[li];
            buf.set_line(cx, sy, line, content_w);
            // Rail
            let rail = rail_for(entry, selected, th, tick, li as u16);
            if let Some((g, st)) = rail {
                buf.set_string(x0, sy, g, st);
            }
        }
        // Fold caret over the bullet on the selected header row.
        if selected && entry.foldable() && start >= top && start < top + view_h {
            let sy = r.y + (start - top) as u16;
            let caret = if entry.fold == Fold::Expanded { "▾" } else { "›" };
            if let Some(cell) = buf.cell_mut((cx, sy)) {
                if cell.symbol() == DIAMOND {
                    cell.set_symbol(caret).set_fg(th.accent);
                }
            }
        }
    }

    // Scrollbar
    if total > view_h {
        let bar_x = r.x + r.width - 1;
        let thumb_h = ((view_h * view_h) / total).max(1);
        let thumb_top = if max_scroll == 0 { 0 } else { (top * (view_h - thumb_h)) / max_scroll };
        for i in 0..view_h {
            let (g, c) = if i >= thumb_top && i < thumb_top + thumb_h { ("┃", th.scrollbar_fg) } else { ("│", th.scrollbar_bg) };
            buf.set_string(bar_x, r.y + i as u16, g, Style::default().fg(c));
        }
        if app.scroll > 0 {
            let label = "▾";
            buf.set_string(r.x + r.width / 2, r.y + r.height - 1, label, Style::default().fg(th.gray));
        }
    }
}

/// A block that just unfolded rolls open over [`UNFOLD`]: its body rows
/// appear top to bottom, pushing what's below down smoothly.
fn unroll(e: &Entry, lines: Rc<Vec<Line<'static>>>, hdr: usize) -> Rc<Vec<Line<'static>>> {
    if e.fold_seen.get() != e.fold {
        e.fold_seen.set(e.fold);
        e.unfolded_at.set((e.fold == Fold::Expanded).then(std::time::Instant::now));
    }
    let Some(at) = e.unfolded_at.get() else { return lines };
    let t = progress(at, UNFOLD);
    if t >= 1.0 {
        e.unfolded_at.set(None);
        return lines;
    }
    let head = hdr.max(1).min(lines.len());
    let body = lines.len() - head;
    let keep = head + (body as f32 * ease_out(t)).ceil() as usize;
    if keep >= lines.len() { lines } else { Rc::new(lines[..keep].to_vec()) }
}

fn rail_for(entry: &Entry, selected: bool, th: &Theme, tick: u64, row: u16) -> Option<(&'static str, Style)> {
    if selected {
        return Some((SELECT_BAR, Style::default().fg(th.accent)));
    }
    let live = entry.is_live();
    let expanded = entry.fold == Fold::Expanded && entry.foldable();
    if live && (expanded || matches!(entry.block, Block::Thought { .. })) {
        let c = blend(th.bg, if matches!(entry.block, Block::Thought { .. }) { th.accent_thinking } else { th.accent }, wave(tick, row));
        return Some((RAIL, Style::default().fg(c)));
    }
    if expanded && !matches!(entry.block, Block::Plan(_)) {
        let failed = matches!(&entry.block, Block::Tool(t) if t.status == ToolStatus::Failed);
        return Some((RAIL, Style::default().fg(if failed { th.error } else { th.gray_dim })));
    }
    None
}

// ---------------------------------------------------------------- status row

fn draw_status(f: &mut Frame, app: &App, th: &Theme, r: Rect, layout: &mut LayoutInfo) {
    let tick = app.tick;
    let mut left: Vec<Span> = Vec::new();
    let mut right: Vec<Span> = Vec::new();
    match &app.conn {
        Conn::Starting(msg) => {
            left.push(Span::styled(format!("{} ", anim::frame(BRAILLE, tick, 80)), th.accent()));
            left.push(Span::styled(msg.clone(), th.secondary()));
        }
        Conn::Failed(e) => {
            left.push(Span::styled("✗ ", Style::default().fg(th.error)));
            left.push(Span::styled(
                truncate(e.lines().next().unwrap_or(""), (r.width as usize).saturating_sub(20)),
                Style::default().fg(th.error),
            ));
            left.push(Span::styled("  /restart", th.muted()));
        }
        Conn::Exited => {
            left.push(Span::styled("✗ Claude Code exited", Style::default().fg(th.error)));
            left.push(Span::styled("  /restart", th.muted()));
        }
        Conn::Ready => {}
    }
    if app.responding {
        let spin = anim::frame(BRAILLE, tick, 80);
        let elapsed = app.turn_started.map(|t| t.elapsed()).unwrap_or_default();
        let activity = if app.cancelling {
            "Cancelling…".to_string()
        } else {
            match app.transcript.entries.iter().rev().find(|e| e.is_live()).map(|e| &e.block) {
                Some(Block::Thought { .. }) => "Thinking…".into(),
                Some(Block::Tool(t)) => {
                    let verb = match t.kind.as_str() {
                        "execute" => "Running",
                        "edit" => "Editing",
                        "read" => "Reading",
                        "search" => "Searching",
                        "fetch" => "Fetching",
                        "think" => "Delegating",
                        _ => "Working",
                    };
                    format!("{verb}…")
                }
                _ => "Responding…".into(),
            }
        };
        left = vec![Span::styled(format!("{spin} "), th.accent())];
        left.extend(anim::shimmer(&activity, tick, th.text_secondary, th.text));
        left.push(Span::styled(format!(" {}", human_duration_short(elapsed)), th.muted()));
        if let Some(tag) = &app.turn_tag {
            left.push(Span::styled(format!(" · {tag}"), th.accent()));
        }
        right.extend(context_meter(app, th));
        let out = app.tweens.out_tokens.get((app.turn_chars / 4) as f64).round() as u64;
        right.push(Span::styled(format!("⇣{} ", human_tokens(out)), th.muted()));
        right.push(Span::styled("[stop]", Style::default().fg(th.gray_bright)));
        let stop_x = r.x + r.width - 6;
        layout.stop_button = Some(Rect { x: stop_x, y: r.y, width: 6, height: 1 });
    } else if app.conn == Conn::Ready {
        if let Some((d, chars)) = app.last_turn {
            right.push(Span::styled(human_duration_short(d), th.dim()));
            if let Some(tag) = &app.turn_tag {
                right.push(Span::styled(format!(" · {tag}"), Style::default().fg(blend(th.accent, th.bg, 0.35))));
            }
            match (&app.turn_tokens, app.config.features.token_meter) {
                (Some(t), true) => {
                    right.push(Span::styled("  in ", th.dim()));
                    right.push(Span::styled(human_tokens(t.input), Style::default().fg(th.info)));
                    right.push(Span::styled(" · out ", th.dim()));
                    right.push(Span::styled(human_tokens(t.output), Style::default().fg(th.success)));
                    if t.cache_read + t.cache_write > 0 {
                        right.push(Span::styled(" · cache ", th.dim()));
                        right.push(Span::styled(
                            format!("{}↺ {}+", human_tokens(t.cache_read), human_tokens(t.cache_write)),
                            th.muted(),
                        ));
                    }
                    let s = &app.session_tokens;
                    right.push(Span::styled(
                        format!("  Σ {} in · {} out", human_tokens(s.input + s.cache_read + s.cache_write), human_tokens(s.output)),
                        th.dim(),
                    ));
                }
                _ => right.push(Span::styled(format!(" ⇣{}", human_tokens((chars / 4) as u64)), th.dim())),
            }
            if let Some(c) = app.session.cost {
                right.push(Span::styled(format!("  ${c:.2}"), th.dim()));
            }
        }
        let ctx = context_meter(app, th);
        if !ctx.is_empty() && !right.is_empty() {
            right.push(Span::styled("  ", th.dim()));
        }
        right.extend(ctx);
    }
    if !app.queue.is_empty() {
        left.push(Span::styled(format!("  · {} queued", app.queue.len()), th.muted()));
    }
    if let Some(s) = &app.search {
        let info = match (s.query.is_empty(), s.matches.len()) {
            (true, _) => "type to search the scrollback".to_string(),
            (false, 0) => "no matches".to_string(),
            (false, n) => format!("{} of {n}", s.cur + 1),
        };
        left = vec![
            Span::styled("Find ", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::styled(s.query.clone(), th.bold()),
            Span::styled("▏", th.accent()),
            Span::styled(format!("  {info}"), if s.matches.is_empty() && !s.query.is_empty() { Style::default().fg(th.warning) } else { th.muted() }),
        ];
    }
    if let Some((msg, at)) = &app.toast {
        let ms = at.elapsed().as_millis() as f32;
        if ms < 2500.0 {
            // Fade in quickly, hold, then fade out over the last 400ms.
            let a = ease_out((ms / 150.0).min(1.0)) * ease_out(((2500.0 - ms) / 400.0).min(1.0));
            if !left.is_empty() {
                left.push(Span::styled("  ·  ", Style::default().fg(blend(th.bg, th.gray_dim, a))));
            }
            left.push(Span::styled(msg.clone(), Style::default().fg(blend(th.bg, th.accent, a))));
        }
    }
    f.render_widget(Paragraph::new(Line::from(left)), Rect { x: r.x + 1, width: r.width.saturating_sub(1), ..r });
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), r);
}

/// "context 23%": dim with room to spare, yellow from 50% (time to /compact),
/// red from 75% (consider a new session).
fn context_meter(app: &App, th: &Theme) -> Vec<Span<'static>> {
    if app.session.size == 0 || app.session.used == 0 {
        return vec![];
    }
    let pct = app.tweens.context.get(app.session.used as f64 * 100.0 / app.session.size as f64);
    let style = if pct >= 75.0 {
        Style::default().fg(th.error)
    } else if pct >= 50.0 {
        Style::default().fg(th.warning)
    } else {
        th.dim()
    };
    vec![Span::styled(format!("context {pct:.0}% "), style)]
}

fn human_duration_short(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { human_duration(d) }
}

// ---------------------------------------------------------------- cards

fn card_lines(app: &App, th: &Theme, w: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(card) = app.permissions.front() {
        out.push(Line::from(vec![
            Span::styled(format!("{DIAMOND} "), Style::default().fg(th.warning)),
            Span::styled("Permission  ", Style::default().fg(th.warning).add_modifier(Modifier::BOLD)),
            Span::styled(truncate(&sanitize(&card.title), w.saturating_sub(14)), th.bold()),
        ]));
        for d in card.detail.iter().take(8) {
            out.extend(wrap_spans(vec![Span::styled(sanitize(d), th.muted())], w, vec![Span::raw("  ")], vec![Span::raw("  ")]));
        }
        out.push(Line::default());
        for (i, (_, name, kind)) in card.options.iter().enumerate() {
            let sel = i == card.sel;
            let color = if kind.starts_with("reject") { th.error } else if kind == "allow_always" { th.success } else { th.text };
            let style = if sel { Style::default().fg(color).bg(th.bg_hover).add_modifier(Modifier::BOLD) } else { Style::default().fg(color) };
            let marker = if sel { PROMPT_ARROW } else { "  " };
            let l = Line::from(vec![
                Span::styled(marker, Style::default().fg(th.accent).bg(if sel { th.bg_hover } else { th.bg })),
                Span::styled(format!("{}. {name}", i + 1), style),
            ]);
            out.push(if sel { crate::text::pad_line(l, w, Style::default().bg(th.bg_hover)) } else { l });
        }
        if app.permissions.len() > 1 {
            out.push(Line::from(Span::styled(format!("  +{} more pending", app.permissions.len() - 1), th.dim())));
        }
        return out;
    }
    if let Some(q) = &app.question {
        let Some(field) = q.fields.get(q.idx) else { return out };
        let header = if !field.title.is_empty() { field.title.clone() } else { "Question".to_string() };
        let mut h = vec![
            Span::styled(format!("{DIAMOND} "), th.accent()),
            Span::styled(header, Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
        ];
        if q.fields.len() > 1 {
            h.push(Span::styled(format!("  {}/{}", q.idx + 1, q.fields.len()), th.muted()));
        }
        out.push(Line::from(h));
        let prompt_text = if !field.description.is_empty() { field.description.clone() } else { q.message.clone() };
        if !prompt_text.is_empty() {
            out.extend(wrap_spans(vec![Span::styled(sanitize(&prompt_text), th.bold())], w, vec![Span::raw("  ")], vec![Span::raw("  ")]));
        }
        out.push(Line::default());
        let opts_lines = |opts: &Vec<(String, String, String)>, checked: Option<&Vec<bool>>, out: &mut Vec<Line<'static>>| {
            for (i, (_, label, desc)) in opts.iter().enumerate() {
                let sel = i == q.sel;
                let mut spans = vec![Span::styled(if sel { PROMPT_ARROW } else { "  " }, th.accent())];
                if let Some(c) = checked {
                    spans.push(Span::styled(if c[i] { "◉ " } else { "○ " }, if c[i] { th.accent() } else { th.muted() }));
                } else {
                    spans.push(Span::styled(format!("{}. ", i + 1), th.muted()));
                }
                spans.push(Span::styled(label.clone(), if sel { th.bold() } else { th.secondary() }));
                if !desc.is_empty() {
                    spans.push(Span::styled(format!("  {}", truncate(&sanitize(desc), w.saturating_sub(label.len() + 10))), th.muted()));
                }
                let l = Line::from(spans);
                out.push(if sel { crate::text::pad_line(l, w, Style::default()) } else { l });
            }
        };
        // The "Other" row after the options, where typing goes.
        let other_line = |out: &mut Vec<Line<'static>>| {
            if field.other.is_none() {
                return;
            }
            let sel = field.on_other(q.sel);
            let mut spans = vec![Span::styled(if sel { PROMPT_ARROW } else { "  " }, th.accent())];
            spans.push(Span::styled(if matches!(field.kind, FieldKind::Multi(..)) { "+ " } else { "   " }, th.muted()));
            if q.text.is_empty() {
                spans.push(Span::styled("Other", if sel { th.bold() } else { th.secondary() }));
                let hint = if !sel || field.required {
                    "  type your own answer"
                } else if matches!(field.kind, FieldKind::Multi(..)) {
                    "  type your own answer, or Enter to submit"
                } else {
                    "  type your own answer, or Enter to skip"
                };
                spans.push(Span::styled(hint, th.dim()));
            } else {
                spans.push(Span::styled(sanitize(&q.text), th.bold()));
            }
            if sel {
                spans.push(Span::styled("▏", th.accent()));
            }
            let l = Line::from(spans);
            out.push(if sel { crate::text::pad_line(l, w, Style::default()) } else { l });
        };
        // Preview of the highlighted option (mockups, snippets), above the
        // options so a clipped card still shows them; kept short.
        let preview_lines = |out: &mut Vec<Line<'static>>| {
            let Some(p) = field.previews.get(q.sel).filter(|p| !p.trim().is_empty()) else { return };
            const MAX: usize = 12;
            let lines: Vec<&str> = p.trim_end().lines().collect();
            for l in lines.iter().take(MAX) {
                out.push(Line::from(vec![
                    Span::styled("  │ ", th.muted()),
                    Span::styled(truncate(&sanitize(l), w.saturating_sub(4)), th.secondary()),
                ]));
            }
            if lines.len() > MAX {
                out.push(Line::from(Span::styled(format!("  │ … {} more lines", lines.len() - MAX), th.dim())));
            }
            out.push(Line::default());
        };
        match &field.kind {
            FieldKind::Single(o) => {
                preview_lines(&mut out);
                opts_lines(o, None, &mut out);
                other_line(&mut out);
            }
            FieldKind::Multi(o, c) => {
                preview_lines(&mut out);
                opts_lines(o, Some(c), &mut out);
                other_line(&mut out);
            }
            FieldKind::Bool => {
                let o = vec![("yes".into(), "Yes".into(), String::new()), ("no".into(), "No".into(), String::new())];
                opts_lines(&o, None, &mut out);
            }
            FieldKind::Text => {
                out.push(Line::from(vec![
                    Span::styled(PROMPT_ARROW, th.accent()),
                    Span::styled(q.text.clone(), th.bold()),
                    Span::styled("▏", th.accent()),
                    Span::styled(if q.text.is_empty() { " type an answer, or Enter to skip" } else { "" }, th.dim()),
                ]));
            }
        }
    }
    out
}

fn draw_card(f: &mut Frame, app: &App, th: &Theme, r: Rect, lines: Vec<Line<'static>>) {
    let color = if app.permissions.is_empty() { th.accent } else { th.warning };
    let block = WBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(blend(th.bg, color, 0.6)))
        .style(th.base());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    // Keep the options (bottom) visible if the card is clipped.
    let h = inner.height as usize;
    let lines: Vec<Line> = if lines.len() <= h {
        lines
    } else if h >= 2 {
        // Keep the title line, then as much of the bottom as fits.
        let mut v = lines[..1].to_vec();
        v.extend(lines[lines.len() - (h - 1)..].iter().cloned());
        v
    } else {
        lines[lines.len() - h..].to_vec()
    };
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------- prompt

fn draw_prompt(f: &mut Frame, app: &App, th: &Theme, r: Rect, inner_w: usize) {
    let focused = app.focus == Focus::Prompt && app.permissions.is_empty() && app.question.is_none() && matches!(app.overlay, Overlay::None);
    let mut border = if focused { th.border_active } else { th.border };
    if app.responding {
        // A slow breath toward the accent while Claude works.
        let ph = tick_ms(app.tick) as f32 / 2400.0 * std::f32::consts::TAU;
        border = blend(border, th.accent, 0.12 + 0.33 * (0.5 - 0.5 * ph.cos()));
    }
    let mode = &app.session.mode;
    let mode_color = match mode.as_str() {
        "plan" => th.plan,
        "bypassPermissions" => th.accent,
        "acceptEdits" => th.success,
        "auto" => th.info,
        _ => th.gray_bright,
    };
    let mut title = vec![Span::styled(" ", th.dim())];
    let shell_mode = app.input.text.starts_with('!');
    if shell_mode {
        title.push(Span::styled(format!("! {} ", crate::shell::name()), Style::default().fg(th.command).add_modifier(Modifier::BOLD)));
        title.push(Span::styled("· ", th.dim()));
    }
    if !app.pending_context.is_empty() {
        let n = app.pending_context.len();
        title.push(Span::styled(format!("+{n} shell output{} ", if n == 1 { "" } else { "s" }), th.accent()));
        title.push(Span::styled("· ", th.dim()));
    }
    if app.stash.is_some() {
        title.push(Span::styled("stashed ", th.muted()));
        title.push(Span::styled("· ", th.dim()));
    }
    if app.conn == Conn::Ready || app.session.id.is_some() {
        title.push(Span::styled(app.session.model_label(), Style::default().fg(th.gray_bright)));
        if !mode.is_empty() {
            title.push(Span::styled(" · ", th.dim()));
            title.push(Span::styled(short_mode(mode).to_string(), Style::default().fg(mode_color)));
        }
    } else {
        title.push(Span::styled("Claude Code", th.muted()));
    }
    title.push(Span::styled(" ", th.dim()));
    let block = WBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title_bottom(Line::from(title).alignment(Alignment::Right))
        .style(th.base());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let text_x = inner.x + 1 + 2;
    let arrow_style = if focused { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { th.dim() };
    f.buffer_mut().set_string(inner.x + 1, inner.y, PROMPT_ARROW, arrow_style);

    if app.input.is_empty() {
        let ph = match &app.conn {
            Conn::Starting(_) => "Claude Code is starting — you can type now",
            Conn::Failed(_) | Conn::Exited => "Agent not running — /restart",
            Conn::Ready if app.responding => "Queue a follow-up…",
            Conn::Ready => "Ask Claude anything  ·  / commands  ·  @ files  ·  ! shell",
        };
        f.buffer_mut().set_string(text_x, inner.y, truncate(ph, inner_w), th.dim());
        if focused {
            f.set_cursor_position((text_x, inner.y));
        }
        return;
    }
    let rows = app.input.layout(inner_w.max(1));
    let (cr, cc) = app.input.cursor_pos(inner_w.max(1));
    let visible = inner.height as usize;
    let first = if cr >= visible { cr + 1 - visible } else { 0 };
    // Attachment placeholders ([Image #1], [Pasted text #2 +40 lines]).
    let tokens: Vec<(usize, usize)> = app
        .attachments
        .items
        .iter()
        .flat_map(|a| app.input.text.match_indices(a.label.as_str()).map(|(i, m)| (i, i + m.len())).collect::<Vec<_>>())
        .collect();
    let buf = f.buffer_mut();
    for (i, (s, e)) in rows.iter().enumerate().skip(first).take(visible) {
        let y = inner.y + (i - first) as u16;
        let seg = &app.input.text[*s..*e];
        let seg = seg.trim_end_matches('\n');
        let style = Style::default().fg(if shell_mode { th.command } else { th.text });
        // Highlight @mentions, attachments and a leading /command or !command.
        let mut x = text_x;
        let mut off = *s;
        for word in split_keep_ws(seg) {
            let (ws, we) = (off, off + word.len());
            off = we;
            let st = if tokens.iter().any(|(a, b)| ws < *b && we > *a) && !word.trim().is_empty() {
                Style::default().fg(th.accent).add_modifier(Modifier::ITALIC)
            } else if shell_mode && i == 0 && x == text_x {
                Style::default().fg(th.command).add_modifier(Modifier::BOLD)
            } else if word.starts_with('@') && word.len() > 1 {
                Style::default().fg(th.accent)
            } else if i == 0 && x == text_x && word.starts_with('/') {
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                style
            };
            buf.set_string(x, y, word, st);
            x += crate::text::width(word) as u16;
        }
    }
    if focused {
        f.set_cursor_position((text_x + cc as u16, inner.y + (cr - first) as u16));
    }
}

fn split_keep_ws(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_ws: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let ws = c.is_whitespace();
        if in_ws.is_some() && in_ws != Some(ws) {
            out.push(&s[start..i]);
            start = i;
        }
        in_ws = Some(ws);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

// ---------------------------------------------------------------- footer

fn hint_line(th: &Theme, hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (k, v)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", th.dim()));
        }
        spans.push(Span::styled(k.to_string(), Style::default().fg(th.text_secondary).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(":{v}"), th.muted()));
    }
    Line::from(spans)
}

fn draw_footer(f: &mut Frame, app: &App, th: &Theme, r: Rect, layout: &mut LayoutInfo) {
    let hints: Vec<(&str, &str)> = if app.search.is_some() {
        vec![("↑ / Enter", "older match"), ("↓", "newer match"), ("Esc", "close")]
    } else if !app.permissions.is_empty() {
        vec![("↑↓", "select"), ("Enter", "confirm"), ("1-9", "pick"), ("Esc", "deny"), ("Ctrl+C", "stop turn")]
    } else if let Some(q) = &app.question {
        let field = q.fields.get(q.idx);
        let mut v = vec![("↑↓", "select")];
        if field.is_some_and(|f| matches!(f.kind, FieldKind::Multi(..))) {
            v.push(("Space", "toggle"));
        }
        if field.is_some_and(|f| f.other.is_some()) {
            v.push(("type", "own answer"));
        }
        v.extend([("Enter", "confirm"), ("Esc", "dismiss")]);
        v
    } else if app.completion.is_some() {
        vec![("↑↓", "select"), ("Tab", "complete"), ("Enter", "accept"), ("Esc", "close")]
    } else if app.focus == Focus::Scrollback && app.selected_path().is_some() {
        vec![("o", "open"), ("r", "reveal"), ("Enter", "view"), ("y", "copy"), ("Tab", "prompt")]
    } else if app.focus == Focus::Scrollback {
        vec![("↑↓", "select"), ("←→", "fold"), ("Enter", "view"), ("y", "copy"), ("/", "find"), ("m", "raw"), ("Tab", "prompt")]
    } else if app.input.text.starts_with('!') {
        vec![("Enter", "run in your shell"), ("Ctrl+C", "clear"), ("Ctrl+.", "shortcuts")]
    } else if app.queue_paused && app.input.is_empty() {
        vec![("Enter", "send queued"), ("Ctrl+C", "drop queued"), ("Ctrl+.", "shortcuts")]
    } else if app.responding {
        vec![("Ctrl+C", "stop"), ("Enter", "queue"), ("Ctrl+Enter", "send now"), ("Shift+Tab", "mode"), ("Ctrl+.", "shortcuts")]
    } else if app.shell.is_some() {
        vec![("Ctrl+C", "stop the shell command"), ("Ctrl+B", "tasks"), ("Ctrl+.", "shortcuts")]
    } else {
        vec![("Shift+Tab", "mode"), ("@", "files"), ("/", "commands"), ("Ctrl+X", "palette"), ("Ctrl+.", "shortcuts")]
    };
    // Right side: background tasks, subscription meter, then the narrator toggle.
    let mut right: Vec<Span> = Vec::new();
    let bg = app.bg_running();
    let bg_chip = (bg > 0).then(|| format!("● {bg} in background"));
    if let Some(chip) = &bg_chip {
        right.push(Span::styled(chip.clone(), Style::default().fg(th.success)));
        right.push(Span::styled("   ", th.dim()));
    }
    let windows = crate::usage::meter_windows(&app.limits);
    if app.config.features.sub_meter && !windows.is_empty() {
        let w = windows[(tick_ms(app.tick) / METER_PERIOD_MS) as usize % windows.len()];
        // Crossfade between windows; the bar slides to each window's fill.
        let a = if windows.len() > 1 { meter_alpha(app.tick) } else { 1.0 };
        let fade = |c: ratatui::style::Color| Style::default().fg(blend(th.bg, c, a));
        let pct = app.tweens.sub_meter.get(w.percent);
        let cells = 10usize;
        let filled = ((pct / 100.0) * cells as f64).round().clamp(0.0, cells as f64) as usize;
        let color = if w.percent >= 90.0 { th.error } else if w.percent >= 70.0 { th.warning } else { th.accent };
        let muted = th.muted().fg.unwrap_or(th.gray);
        let dim = th.dim().fg.unwrap_or(th.gray_dim);
        right.push(Span::styled(format!("{} ", w.label), fade(muted)));
        right.push(Span::styled("▰".repeat(filled), fade(color)));
        right.push(Span::styled("▱".repeat(cells - filled), fade(dim)));
        right.push(Span::styled(format!(" {pct:.0}%"), fade(th.text_secondary)));
        if let Some(t) = w.resets {
            right.push(Span::styled(format!(" · {}", until(t, chrono::Local::now())), fade(dim)));
        }
        right.push(Span::styled("   ", th.dim()));
    }
    let (label, style) = if app.narrating {
        ("⏹ narrating", Style::default().fg(th.accent))
    } else if app.config.features.narrator {
        ("▶ narrator on", th.secondary())
    } else {
        ("⏸ narrator off", th.dim())
    };
    right.push(Span::styled(label, style));
    let mut right_w = right.iter().map(|s| crate::text::width(&s.content)).sum::<usize>() as u16;
    let label_w = crate::text::width(label) as u16;
    if right_w + 10 < r.width {
        layout.narrator_button = Some(Rect { x: r.x + r.width - label_w, y: r.y, width: label_w, height: 1 });
        if let Some(chip) = &bg_chip {
            let w = crate::text::width(chip) as u16;
            layout.bg_button = Some(Rect { x: r.x + r.width - right_w, y: r.y, width: w, height: 1 });
        }
        f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), r);
    } else {
        right_w = 0;
    }

    // Left side: drop whole hints from the end rather than cutting one off mid-word.
    let left_w = r.width.saturating_sub(1 + right_w + 2);
    let peek = if app.focus == Focus::Scrollback { app.selected_path().map(|p| crate::peek::describe(&p)) } else { None };
    let mut hints = hints;
    let left = loop {
        let mut line = hint_line(th, &hints);
        if let Some(d) = &peek {
            line.spans.insert(0, Span::styled(format!("{d}   "), th.secondary()));
        }
        if line_width(&line) <= left_w as usize || hints.len() <= 1 {
            break line;
        }
        hints.pop();
    };
    f.render_widget(Paragraph::new(left), Rect { x: r.x + 1, width: left_w, ..r });
}

// ---------------------------------------------------------------- completion popup

fn draw_completion(f: &mut Frame, app: &App, th: &Theme, prompt: Rect, layout: &mut LayoutInfo) {
    let Some(c) = &app.completion else { return };
    if app.focus != Focus::Prompt {
        return;
    }
    let rows: Vec<Line> = match c {
        Completion::Slash { items, sel } => {
            let start = sel.saturating_sub(7);
            items
                .iter()
                .enumerate()
                .skip(start)
                .take(8)
                .map(|(i, (name, desc, _))| {
                    let s = i == *sel;
                    let name_style = if s { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) };
                    let mut l = Line::from(vec![
                        Span::styled(if s { "▎" } else { " " }, th.accent()),
                        Span::styled(format!("/{name:<18}"), name_style),
                        Span::styled(truncate(desc, prompt.width.saturating_sub(24) as usize), th.muted()),
                    ]);
                    if s {
                        l = crate::text::pad_line(l, prompt.width.saturating_sub(2) as usize, Style::default().bg(th.bg_hover))
                            .style(Style::default().bg(th.bg_hover));
                    }
                    l
                })
                .collect()
        }
        Completion::File { items, sel, .. } => items
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let s = i == *sel;
                let (dir, name) = match p.trim_end_matches('/').rsplit_once('/') {
                    Some((d, n)) => (format!("{d}/"), format!("{n}{}", if p.ends_with('/') { "/" } else { "" })),
                    None => (String::new(), p.clone()),
                };
                // The file name is what matters: shorten the directory from the left.
                let room = (prompt.width as usize).saturating_sub(4 + crate::text::width(&name));
                let dir = if crate::text::width(&dir) > room { crate::text::truncate_left(&dir, room) } else { dir };
                let mut l = Line::from(vec![
                    Span::styled(if s { "▎" } else { " " }, th.accent()),
                    Span::styled("@", th.dim()),
                    Span::styled(dir, th.muted()),
                    Span::styled(name, if s { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) }),
                ]);
                if s {
                    l = crate::text::pad_line(l, prompt.width.saturating_sub(2) as usize, Style::default().bg(th.bg_hover))
                        .style(Style::default().bg(th.bg_hover));
                }
                l
            })
            .collect(),
    };
    let h = rows.len() as u16 + 2;
    let y = prompt.y.saturating_sub(h);
    let r = Rect { x: prompt.x, y, width: prompt.width, height: h };
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let block = WBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.border))
        .style(Style::default().bg(th.bg_raised).fg(th.text));
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(Paragraph::new(rows), inner);
}

// ---------------------------------------------------------------- overlays

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn modal_block<'a>(th: &Theme, title: &str) -> WBlock<'a> {
    WBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.border_active))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(title.to_string(), Style::default().fg(th.accent).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(th.bg_raised).fg(th.text))
}

fn dim_background(buf: &mut Buffer, area: Rect, th: &Theme) {
    if matches!(th.bg, ratatui::style::Color::Reset) {
        return;
    }
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                let fg = cell.fg;
                cell.set_fg(blend(fg, th.bg, 0.55));
            }
        }
    }
}

fn draw_picker(f: &mut Frame, app: &App, th: &Theme, area: Rect, layout: &mut LayoutInfo) {
    let Overlay::Picker(p) = &app.overlay else { return };
    dim_background(f.buffer_mut(), area, th);
    let vis = p.visible();
    let list_h = vis.len().clamp(1, 14) as u16;
    let r = centered(area, 72, list_h + 5);
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let block = modal_block(th, &p.title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let filter = Line::from(vec![
        Span::styled(PROMPT_ARROW, th.accent()),
        Span::styled(p.filter.clone(), th.bold()),
        Span::styled(if p.filter.is_empty() { "type to filter" } else { "" }, th.dim()),
    ]);
    f.render_widget(Paragraph::new(filter), Rect { height: 1, ..inner });
    let list_area = Rect { y: inner.y + 2, height: inner.height.saturating_sub(3), ..inner };
    let n = list_area.height as usize;
    let start = p.sel.saturating_sub(n.saturating_sub(1));
    let w = list_area.width as usize;
    let mut lines = Vec::new();
    for (row, &i) in vis.iter().enumerate().skip(start).take(n) {
        let it = &p.items[i];
        let s = row == p.sel;
        let mark = if it.current { "● " } else { "  " };
        let label_w = (w / 2).max(16).min(w.saturating_sub(4));
        let mut spans = vec![
            Span::styled(if s { "▎" } else { " " }, th.accent()),
            Span::styled(mark, th.accent()),
            Span::styled(
                {
                    // Pad by display width so CJK / emoji titles keep the columns aligned.
                    let t = truncate(&it.label, label_w);
                    let pad = label_w.saturating_sub(crate::text::width(&t));
                    format!("{t}{}", " ".repeat(pad))
                },
                if s { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) },
            ),
        ];
        let rest = w.saturating_sub(label_w + 4);
        if rest > 4 {
            spans.push(Span::styled(truncate(&it.detail, rest), th.muted()));
        }
        let mut l = Line::from(spans);
        if s {
            l = crate::text::pad_line(l, w, Style::default().bg(th.bg_hover)).style(Style::default().bg(th.bg_hover));
        }
        lines.push(l);
    }
    if vis.is_empty() {
        lines.push(Line::from(Span::styled("  no matches", th.muted())));
    }
    f.render_widget(Paragraph::new(lines), list_area);
    let hint = hint_line(th, &[("↑↓", "select"), ("Enter", "choose"), ("Esc", "cancel")]);
    f.render_widget(Paragraph::new(hint), Rect { y: inner.y + inner.height - 1, height: 1, ..inner });
}

fn draw_shortcuts(f: &mut Frame, app: &App, th: &Theme, area: Rect, layout: &mut LayoutInfo) {
    dim_background(f.buffer_mut(), area, th);
    let groups: &[(&str, &[(&str, &str)])] = &[
        ("Prompt", &[
            ("Enter", "send (queues while Claude is responding)"),
            ("Ctrl+Enter", "send now: stop the turn, send this next"),
            ("Shift/Alt+Enter, Ctrl+J, \\⏎", "new line"),
            ("Shift+Tab", "cycle permission mode"),
            ("↑ / ↓", "history"),
            ("↑ on an empty prompt", "edit the last queued prompt"),
            ("Tab", "focus the scrollback"),
            ("Ctrl+C", "stop response · clear · quit (×2)"),
            ("Esc Esc", "clear · rewind files when empty"),
            ("Ctrl+S", "stash the prompt / bring it back"),
            ("/  @", "commands · mention files"),
            ("! command", "run in your shell, output to Claude"),
            ("Alt+V", "attach the clipboard's image"),
            ("_ prompt", "run this turn on Haiku"),
            ("+ prompt", "run this turn at max effort"),
        ]),
        ("Scrollback", &[
            ("↑ / ↓  ·  j / k", "select entry"),
            ("Shift+← / →", "previous / next turn"),
            ("← / →", "collapse / expand"),
            ("Enter", "open in viewer"),
            ("y / Y", "copy content / command"),
            ("o / r", "open / reveal the entry's file"),
            ("m", "markdown source / rendered"),
            ("/", "find in the scrollback"),
            ("?", "command palette"),
            ("g / G", "top / bottom"),
            ("Ctrl+U / Ctrl+D", "half page up / down"),
            ("PgUp / PgDn, wheel", "scroll"),
        ]),
        ("Global", &[
            ("Ctrl+X", "command palette"),
            ("Ctrl+F", "find in the scrollback"),
            ("F2", "settings"),
            ("Alt+P · Ctrl+B", "todo pane · tasks pane"),
            ("Alt+J", "background tasks and their output"),
            ("Ctrl+E", "expand / collapse thinking"),
            ("Ctrl+O", "expand / collapse all blocks"),
            ("Ctrl+G", "compact layout: group tools & thinking"),
            ("Alt+↑ / ↓", "previous / next turn (from anywhere)"),
            ("Ctrl+L", "redraw"),
            ("Ctrl+. / F1", "this help"),
            ("Shift+drag", "native text selection"),
        ]),
        ("Quick commands", &[
            ("Ctrl+N · Ctrl+R", "new session · resume session"),
            ("Ctrl+P · Alt+E · Alt+M", "model · effort · permission mode"),
            ("Ctrl+T", "theme"),
            ("Ctrl+Y · Alt+X", "copy last response · export transcript"),
            ("Alt+C", "commit"),
            ("Alt+T · Alt+Z · Alt+Shift+Z", "timeline · undo · redo"),
            ("Alt+K", "context usage"),
            ("Alt+H · Alt+S · Alt+N", "toggle thinking · timestamps · narrator"),
        ]),
        ("Commands", &[
            ("/commit", "Conventional Commits on Haiku"),
            ("/timeline · /undo · /redo", "Time Machine file snapshots"),
            ("/after <task>", "queue a task for when Claude finishes"),
            ("/btw <question>", "side question, answered in a fork"),
            ("/rename · /queue · /tasks", "name the session · list queue · tasks"),
            ("/bg", "background tasks: output, stop"),
            ("/density · /timestamps", "tool-call detail · clock display"),
            ("/narrator", "read each turn's summary aloud"),
        ]),
    ];
    let key_w = groups.iter().flat_map(|(_, items)| items.iter()).map(|(k, _)| crate::text::width(k)).max().unwrap_or(0) + 2;
    let desc_w = groups.iter().flat_map(|(_, items)| items.iter()).map(|(_, v)| crate::text::width(v)).max().unwrap_or(0);
    let col_w = 2 + key_w + desc_w;
    let render = |gs: &[(&str, &[(&str, &str)])]| {
        let mut lines = Vec::new();
        for (gi, (g, items)) in gs.iter().enumerate() {
            if gi > 0 {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(g.to_string(), Style::default().fg(th.h2).add_modifier(Modifier::BOLD))));
            for (k, v) in *items {
                let pad = key_w.saturating_sub(crate::text::width(k));
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k}{}", " ".repeat(pad)), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
                    Span::styled(v.to_string(), th.muted()),
                ]));
            }
        }
        lines
    };
    // Side by side when there's room, otherwise one scrollable column.
    let two_cols = area.width as usize >= 2 * col_w + 12;
    let columns: Vec<Vec<Line>> = if two_cols { vec![render(&groups[..2]), render(&groups[2..])] } else { vec![render(groups)] };
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    let width = if two_cols { 2 * col_w + 4 + 4 } else { col_w + 4 };
    let r = centered(area, width as u16, rows as u16 + 4);
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let block = modal_block(th, "Shortcuts");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let body = Rect { x: inner.x + 1, y: inner.y + 1, width: inner.width.saturating_sub(2), height: inner.height.saturating_sub(1) };
    let max = rows.saturating_sub(body.height as usize);
    layout.shortcuts_max = max;
    let top = app.shortcuts_scroll.min(max);
    for (ci, col) in columns.into_iter().enumerate() {
        let x = body.x + (ci * (col_w + 4)) as u16;
        let col_area = Rect { x, width: body.width.saturating_sub(x - body.x), ..body };
        let vis: Vec<Line> = col.into_iter().skip(top).take(body.height as usize).collect();
        f.render_widget(Paragraph::new(vis), col_area);
    }
    if max > 0 {
        let more = if top < max { " ↓ more · ↑↓ scroll " } else { " ↑ scroll " };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(more, th.muted())).alignment(Alignment::Right)),
            Rect { x: r.x + 1, y: r.y + r.height - 1, width: r.width.saturating_sub(3), height: 1 },
        );
    }
}

fn draw_settings(f: &mut Frame, app: &App, th: &Theme, area: Rect, layout: &mut LayoutInfo) {
    let Overlay::Settings { sel } = app.overlay else { return };
    dim_background(f.buffer_mut(), area, th);
    let items = app.settings_items();
    let sel = sel.min(items.len().saturating_sub(1));
    let r = centered(area, 84, items.len() as u16 + 6);
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let block = modal_block(th, "Settings");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    // Rows, then a blank line, the key hints and the config file's path.
    let list_h = inner.height.saturating_sub(3) as usize;
    let start = sel.saturating_sub(list_h.saturating_sub(1));
    let w = inner.width as usize;
    for (row, (i, it)) in items.iter().enumerate().skip(start).take(list_h).enumerate() {
        let y = inner.y + row as u16;
        let s = i == sel;
        let label = format!("{:<20}", it.label);
        let value = format!("{:<10}", it.value);
        let mut l = Line::from(vec![
            Span::styled(if s { "▎" } else { " " }, th.accent()),
            Span::styled(format!(" {label}"), if s { Style::default().fg(th.text).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) }),
            Span::styled(value, Style::default().fg(if it.value == "off" { th.gray } else { th.accent }).add_modifier(Modifier::BOLD)),
            Span::styled(truncate(it.hint, w.saturating_sub(34)), th.muted()),
        ]);
        if s {
            l = crate::text::pad_line(l, w, Style::default().bg(th.bg_hover)).style(Style::default().bg(th.bg_hover));
        }
        f.render_widget(Paragraph::new(l), Rect { y, height: 1, ..inner });
        layout.settings_rows.push((y, i));
    }
    let hint = hint_line(th, &[("↑↓", "select"), ("Enter / ←→", "change"), ("Esc", "close")]);
    f.render_widget(Paragraph::new(hint), Rect { y: inner.y + inner.height.saturating_sub(2), height: 1, ..inner });
    let path = format!("Saved to {}", display_path(&crate::config::config_path()));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(truncate(&path, w), th.dim()))),
        Rect { y: inner.y + inner.height.saturating_sub(1), height: 1, ..inner },
    );
}

fn draw_viewer(f: &mut Frame, app: &App, th: &Theme, area: Rect, layout: &mut LayoutInfo) {
    let Overlay::Viewer { title, text, scroll } = &app.overlay else { return };
    let r = Rect { x: area.x + 2, y: area.y + 1, width: area.width.saturating_sub(4), height: area.height.saturating_sub(2) };
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let block = modal_block(th, &truncate(title, (r.width as usize).saturating_sub(6)))
        .title_bottom(hint_line(th, &[("↑↓", "scroll"), ("y", "copy"), ("Esc", "close")]).alignment(Alignment::Right))
        .style(th.base());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let lines = crate::markdown::render(text, inner.width as usize, th);
    let max = lines.len().saturating_sub(inner.height as usize);
    layout.viewer_max = max;
    layout.viewer_page = inner.height as usize;
    let s = (*scroll).min(max);
    let vis: Vec<Line> = lines.into_iter().skip(s).take(inner.height as usize).collect();
    f.render_widget(Paragraph::new(vis), inner);
}

fn draw_bg_tasks(f: &mut Frame, app: &App, th: &Theme, area: Rect, layout: &mut LayoutInfo) {
    let Overlay::Tasks(v) = &app.overlay else { return };
    let r = Rect { x: area.x + 2, y: area.y + 1, width: area.width.saturating_sub(4), height: area.height.saturating_sub(2) };
    f.render_widget(Clear, r);
    layout.popup = Some(r);
    let tasks = app.bg_tasks();
    let running = tasks.iter().filter(|t| t.running()).count();
    let title = format!("Background tasks · {running} running");
    let block = modal_block(th, &title)
        .title_bottom(
            hint_line(th, &[("↑↓", "task"), ("PgUp/PgDn", "scroll"), ("Enter", "full output"), ("x", "stop"), ("y", "copy"), ("o", "reveal"), ("Esc", "close")])
                .alignment(Alignment::Right),
        )
        .style(th.base());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let w = inner.width as usize;
    if tasks.is_empty() {
        f.render_widget(Paragraph::new(Line::from(Span::styled("No background tasks in this session", th.dim()))), inner);
        return;
    }
    let sel = v.sel.as_deref().and_then(|id| tasks.iter().position(|t| t.id == id)).unwrap_or(0);

    // The list: at most a third of the height, scrolled to keep the selection in view.
    let list_h = tasks.len().min((inner.height as usize / 3).max(3)).min(inner.height as usize);
    let start = sel.saturating_sub(list_h.saturating_sub(1));
    for (row, (i, t)) in tasks.iter().enumerate().skip(start).take(list_h).enumerate() {
        let y = inner.y + row as u16;
        let s = i == sel;
        let (glyph, color) = match t.state.as_str() {
            "running" => (anim::frame(BRAILLE, app.tick, 80), th.accent),
            "paused" => ("Ⅱ", th.warning),
            "completed" => ("✓", th.success),
            "stopped" => ("■", th.gray),
            _ => ("✗", th.error),
        };
        let since = human_duration_short(t.elapsed());
        let room = w.saturating_sub(4 + 10 + 9 + crate::text::width(&since) + 2);
        let label = truncate(&sanitize(&app.transcript.bg_label(t)), room);
        let pad = room.saturating_sub(crate::text::width(&label));
        let mut l = Line::from(vec![
            Span::styled(if s { "▎" } else { " " }, th.accent()),
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::styled(format!("{:<10}", t.state), Style::default().fg(color)),
            Span::styled(format!("{:<9}", truncate(&t.kind, 8)), th.muted()),
            Span::styled(
                format!("{label}{}", " ".repeat(pad)),
                if s { Style::default().fg(th.text).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text) },
            ),
            Span::styled(format!("  {since}"), th.dim()),
        ]);
        if s {
            l = crate::text::pad_line(l, w, Style::default().bg(th.bg_hover)).style(Style::default().bg(th.bg_hover));
        }
        f.render_widget(Paragraph::new(l), Rect { y, height: 1, ..inner });
        layout.bg_rows.push((y, i));
    }

    // The selected task: its command, then its output, following the end.
    let t = tasks[sel];
    let bottom = inner.y + inner.height;
    let mut y = inner.y + list_h as u16;
    let mut head: Vec<Line> = vec![Line::from(Span::styled("─".repeat(w), th.dim()))];
    if let Some(cmd) = app.transcript.bg_command(t) {
        head.push(Line::from(vec![
            Span::styled("$ ", th.accent()),
            Span::styled(truncate(&sanitize(cmd.lines().next().unwrap_or("")), w.saturating_sub(2)), th.secondary()),
        ]));
    }
    if let Some(sum) = &t.summary {
        head.push(Line::from(Span::styled(truncate(&sanitize(sum), w), th.muted())));
    }
    for l in head {
        if y >= bottom {
            return;
        }
        f.render_widget(Paragraph::new(l), Rect { y, height: 1, ..inner });
        y += 1;
    }
    let out_area = Rect { y, height: bottom.saturating_sub(y), ..inner };
    let mut lines: Vec<Line> = Vec::new();
    let text = sanitize(&v.output);
    for raw in text.trim_end_matches('\n').split('\n') {
        lines.extend(crate::text::hard_wrap(vec![Span::styled(raw.to_string(), Style::default().fg(th.text_secondary))], w, vec![]));
    }
    if text.trim().is_empty() {
        lines = vec![Line::from(Span::styled(if t.running() { "no output yet" } else { "no output" }, th.dim()))];
    }
    let h = out_area.height as usize;
    let max = lines.len().saturating_sub(h);
    layout.viewer_max = max;
    layout.viewer_page = h;
    let up = v.scroll.min(max);
    let vis: Vec<Line> = lines.into_iter().skip(max - up).take(h).collect();
    f.render_widget(Paragraph::new(vis), out_area);
    if up > 0 && h > 0 {
        let more = format!(" ↓ {up} more lines · End follows ");
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(more, th.muted())).alignment(Alignment::Right)),
            Rect { y: out_area.y + out_area.height - 1, height: 1, ..out_area },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SessionKind;
    use crate::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn background_tasks_viewer_lists_tasks_and_shows_output() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Config::default(), std::env::temp_dir(), tx, SessionKind::New);
        let mut s = crate::model::Session::default();
        let out = std::env::temp_dir().join(format!("blush-ui-bg-{}.output", std::process::id()));
        std::fs::write(&out, "server listening on :3000
").unwrap();
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"dev","kind":"execute",
            "status":"completed","rawInput":{"command":"npm run dev","run_in_background":true}}));
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"async_task_spawned","asyncTaskId":"b1","name":"Shell",
            "taskType":"shell","description":"Start dev server","showInTranscript":true,"canStop":true,"toolCallId":"t1",
            "outputFilePath": out.to_string_lossy()}));
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"async_task_spawned","asyncTaskId":"b2","name":"Shell",
            "taskType":"shell","description":"Run tests","showInTranscript":true,"canStop":true}));
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"async_task_state_update","asyncTaskId":"b2","state":"failed"}));
        let footer = screen(&mut app, 120, 30);
        assert!(footer.contains("1 in background"), "{footer}");
        app.open_bg_view();
        let txt = screen(&mut app, 120, 30);
        let _ = std::fs::remove_file(&out);
        assert!(txt.contains("Background tasks · 1 running"), "{txt}");
        assert!(txt.contains("Start dev server"), "{txt}");
        assert!(txt.contains("Run tests"), "{txt}");
        assert!(txt.contains("$ npm run dev"), "{txt}");
        assert!(txt.contains("server listening on :3000"), "{txt}");
    }

    #[test]
    fn compact_mode_groups_tools_between_responses() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.ui.show_timestamps = false;
        let mut app = App::new(cfg, std::env::temp_dir(), tx, SessionKind::New);
        app.transcript.entries.clear();
        let mut s = crate::model::Session::default();
        let up = |v: serde_json::Value, app: &mut App, s: &mut crate::model::Session| {
            app.transcript.apply_update(s, &v);
        };
        app.transcript.push(Block::User { text: "add div".into(), queued: false, tag: None });
        up(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"think"}}), &mut app, &mut s);
        up(json!({"sessionUpdate":"tool_call","toolCallId":"1","title":"Read calc.py","kind":"read","status":"completed"}), &mut app, &mut s);
        up(json!({"sessionUpdate":"tool_call","toolCallId":"2","title":"Write calc.py","kind":"edit","status":"completed",
            "content":[{"type":"diff","path":"calc.py","oldText":"a\n","newText":"a\nb\nc\n"}]}), &mut app, &mut s);
        up(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I added div."}}), &mut app, &mut s);

        let full = screen(&mut app, 70, 22);
        app.config.ui.compact = true;
        let compact = screen(&mut app, 70, 22);
        println!("--- normal ---\n{full}\n--- compact ---\n{compact}");
        assert!(compact.contains("1 tool · 1 thought · 1 edit +2 -0"), "{compact}");
        assert!(!compact.contains("Read calc.py"));

        app.transcript.entries[1].group_open = true;
        let open = screen(&mut app, 70, 22);
        println!("--- group expanded ---\n{open}");
        assert!(open.contains("▾ 1 tool · 1 thought · 1 edit") && open.contains("Read calc.py"), "{open}");
    }

    #[test]
    fn detached_scrollback_holds_still_while_output_arrives() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.ui.show_timestamps = false;
        let mut app = App::new(cfg, std::env::temp_dir(), tx, SessionKind::New);
        app.transcript.entries.clear();
        for i in 0..40 {
            app.transcript.push(Block::User { text: format!("msg {i:02}"), queued: false, tag: None });
        }
        let first_msg = |s: &str| s.lines().find_map(|l| l.find("msg ").map(|p| l[p..p + 6].to_string()));
        screen(&mut app, 60, 24);
        // Scroll up a little: detached.
        app.follow = false;
        app.scroll = 8;
        app.scroll_shown = 8.0;
        let before = first_msg(&screen(&mut app, 60, 24));
        for i in 40..50 {
            app.transcript.push(Block::User { text: format!("msg {i:02}"), queued: false, tag: None });
        }
        let after = screen(&mut app, 60, 24);
        assert_eq!(first_msg(&after), before, "{after}");
        assert!(!after.contains("msg 49"), "{after}");

        // Following again: the newest output is on screen.
        app.follow = true;
        app.scroll_shown = 0.0; // skip the glide back down
        let attached = screen(&mut app, 60, 24);
        assert!(attached.contains("msg 49"), "{attached}");
    }

    #[test]
    fn tiny_terminals_never_panic() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Config::default(), std::env::temp_dir(), tx, SessionKind::New);
        app.input.set(&(0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("
"));
        app.permissions.push_back(crate::app::PermissionCard {
            id: json!(1),
            title: "Run cargo test".into(),
            detail: vec!["cargo test --all".into(); 4],
            options: vec![
                ("a".into(), "Allow".into(), "allow_once".into()),
                ("r".into(), "Reject".into(), "reject_once".into()),
            ],
            sel: 0,
            shown: std::time::Instant::now(),
        });
        app.conn = crate::app::Conn::Failed("adapter crashed with a long message".into());
        for w in [20u16, 21, 30, 40, 80] {
            for h in 8u16..=24 {
                screen(&mut app, w, h);
            }
        }
        // The card's options stay visible even when it has to be clipped.
        let small = screen(&mut app, 60, 14);
        assert!(small.contains("Reject"), "{small}");
    }

    #[test]
    fn animations_mid_flight() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.ui.show_timestamps = false;
        let mut app = App::new(cfg, std::env::temp_dir(), tx, SessionKind::New);
        app.transcript.entries.clear();
        let mut s = crate::model::Session::default();
        app.responding = true;
        app.turn_started = Some(std::time::Instant::now());
        app.transcript.animate_stream = true;
        app.transcript.push(Block::User { text: "hi".into(), queued: false, tag: None });
        let text = "The quick brown fox jumps over the lazy dog, again and again and again.";
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}));
        app.toast("Copied");

        // Streamed text trickles in rather than landing all at once.
        let first = screen(&mut app, 80, 20);
        assert!(!first.contains("again."), "{first}");
        assert!(first.contains("Responding…"), "{first}");
        assert!(BRAILLE.iter().any(|b| first.contains(&format!("{b} Responding…"))), "{first}");
        while app.reveal_stream() {}
        let done = screen(&mut app, 80, 20);
        assert!(done.contains("again."), "{done}");

        // A tool that just finished flashes, a block that just opened rolls
        // open, and a popup fades in: none of it may panic at any size.
        app.transcript.apply_update(&mut s, &json!({"sessionUpdate":"tool_call","toolCallId":"1","title":"Read a.txt","kind":"read","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"1\n2\n3\n4\n5\n6"}}]}));
        assert!(app.transcript.entries.last().unwrap().settling().is_some());
        app.transcript.entries.last_mut().unwrap().fold = Fold::Expanded;
        app.overlay = Overlay::Shortcuts;
        for (w, h) in [(20u16, 8u16), (40, 12), (80, 20), (120, 40)] {
            screen(&mut app, w, h);
        }
        assert!(app.animating());
    }

    #[test]
    fn shortcuts_overlay_fits_or_scrolls() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Config::default(), std::env::temp_dir(), tx, SessionKind::New);
        app.overlay = Overlay::Shortcuts;
        // Wide: two columns, everything visible at once.
        let wide = screen(&mut app, 180, 40);
        println!("{wide}");
        assert!(wide.contains("Quick commands") && wide.contains("/narrator"), "{wide}");
        assert_eq!(app.layout.shortcuts_max, 0);
        // Narrow and short: one column that scrolls to the end.
        let narrow = screen(&mut app, 100, 30);
        println!("{narrow}");
        assert!(narrow.contains("↓ more"), "{narrow}");
        app.shortcuts_scroll = app.layout.shortcuts_max;
        let end = screen(&mut app, 100, 30);
        assert!(end.contains("/narrator"), "{end}");
    }
}

#[cfg(test)]
mod git_label_tests {
    #[test]
    fn labels_this_repo() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let label = super::git_label(&here).expect("inside a git repo");
        assert!(label.starts_with("blush:"), "{label}");
    }
}

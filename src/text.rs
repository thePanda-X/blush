//! Styled-text wrapping and width helpers.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|s| width(&s.content)).sum()
}

/// Expand tabs and strip control chars that would corrupt the terminal grid.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\r' => {}
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Truncate to `max` display columns, appending `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for g in s.graphemes(true) {
        let gw = width(g);
        if w + gw > max.saturating_sub(1) {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    out
}

/// Truncate keeping the end (for paths).
pub fn truncate_left(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let graphemes: Vec<&str> = s.graphemes(true).collect();
    let mut out: Vec<&str> = Vec::new();
    let mut w = 1;
    for g in graphemes.iter().rev() {
        let gw = width(g);
        if w + gw > max {
            break;
        }
        out.push(g);
        w += gw;
    }
    out.reverse();
    format!("…{}", out.concat())
}

#[derive(Clone)]
struct Piece {
    text: String,
    style: Style,
    ws: bool,
}

fn pieces(spans: &[Span<'static>]) -> Vec<Piece> {
    let mut out = Vec::new();
    for span in spans {
        let mut cur = String::new();
        let mut cur_ws: Option<bool> = None;
        for g in span.content.graphemes(true) {
            let is_ws = g.chars().all(char::is_whitespace);
            if cur_ws.is_some() && cur_ws != Some(is_ws) {
                out.push(Piece { text: std::mem::take(&mut cur), style: span.style, ws: cur_ws.unwrap() });
            }
            cur_ws = Some(is_ws);
            cur.push_str(g);
        }
        if !cur.is_empty() {
            out.push(Piece { text: cur, style: span.style, ws: cur_ws.unwrap_or(false) });
        }
    }
    out
}

/// Greedy word-wrap of styled spans into lines of at most `width` columns.
/// `first` / `rest` are prefixes (bullets, indentation, quote bars).
pub fn wrap_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let pw = |p: &Vec<Span<'static>>| p.iter().map(|s| self::width(&s.content)).sum::<usize>();
    let rest_avail = width.saturating_sub(pw(&rest)).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = first.clone();
    let mut avail = width.saturating_sub(pw(&first)).max(1);
    let mut used = 0usize; // content columns on the current line
    let mut pending_ws: Option<Span<'static>> = None;

    fn flush(lines: &mut Vec<Line<'static>>, cur: &mut Vec<Span<'static>>) {
        lines.push(Line::from(std::mem::take(cur)));
    }

    for p in pieces(&spans) {
        if p.text.contains('\n') {
            for _ in 0..p.text.matches('\n').count() {
                flush(&mut lines, &mut cur);
                cur = rest.clone();
                avail = rest_avail;
                used = 0;
                pending_ws = None;
            }
            continue;
        }
        let w = self::width(&p.text);
        if p.ws {
            if used > 0 {
                pending_ws = Some(Span::styled(p.text, p.style));
            }
            continue;
        }
        let ws_w = pending_ws.as_ref().map(|s| self::width(&s.content)).unwrap_or(0);
        if used + ws_w + w <= avail {
            if let Some(ws) = pending_ws.take() {
                cur.push(ws);
            }
            cur.push(Span::styled(p.text, p.style));
            used += ws_w + w;
            continue;
        }
        pending_ws = None;
        if used > 0 && w <= rest_avail {
            flush(&mut lines, &mut cur);
            cur = rest.clone();
            avail = rest_avail;
            cur.push(Span::styled(p.text, p.style));
            used = w;
            continue;
        }
        // Hard-split a word longer than a line.
        if used > 0 && ws_w > 0 && used + ws_w < avail {
            cur.push(Span::styled(" ".to_string(), p.style));
            used += 1;
        }
        let mut chunk = String::new();
        for g in p.text.graphemes(true) {
            let gw = self::width(g);
            if used + self::width(&chunk) + gw > avail && (used > 0 || !chunk.is_empty()) {
                if !chunk.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut chunk), p.style));
                }
                flush(&mut lines, &mut cur);
                cur = rest.clone();
                avail = rest_avail;
                used = 0;
            }
            chunk.push_str(g);
        }
        if !chunk.is_empty() {
            used += self::width(&chunk);
            cur.push(Span::styled(chunk, p.style));
        }
    }
    if used > 0 || lines.is_empty() {
        flush(&mut lines, &mut cur);
    }
    lines
}

/// Hard-wrap (character level, no word logic) — for code and terminal output.
pub fn hard_wrap(spans: Vec<Span<'static>>, width: usize, prefix: Vec<Span<'static>>) -> Vec<Line<'static>> {
    let pw: usize = prefix.iter().map(|s| self::width(&s.content)).sum();
    let avail = width.saturating_sub(pw).max(1);
    let mut lines = Vec::new();
    let mut cur = prefix.clone();
    let mut w = 0;
    for span in spans {
        let mut chunk = String::new();
        for g in span.content.graphemes(true) {
            let gw = self::width(g);
            if w + gw > avail {
                if !chunk.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                lines.push(Line::from(std::mem::replace(&mut cur, prefix.clone())));
                w = 0;
            }
            chunk.push_str(g);
            w += gw;
        }
        if !chunk.is_empty() {
            cur.push(Span::styled(chunk, span.style));
        }
    }
    lines.push(Line::from(cur));
    lines
}

/// Pad a line with `style` out to `width` columns (for full-width bands).
pub fn pad_line(mut line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let w = line_width(&line);
    if w < width {
        line.spans.push(Span::styled(" ".repeat(width - w), style));
    }
    line
}

pub fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        let v = n as f64 / 1000.0;
        if v >= 100.0 { format!("{:.0}K", v) } else { format!("{:.1}K", v) }
    } else {
        n.to_string()
    }
}

pub fn human_duration(d: std::time::Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1}s")
    } else if s < 60.0 {
        format!("{}s", s as u64)
    } else if s < 3600.0 {
        format!("{}m{:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    } else {
        format!("{}h{:02}m", (s / 3600.0) as u64, ((s % 3600.0) / 60.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn wraps_words() {
        let l = wrap_spans(vec![Span::raw("hello brave new world")], 11, vec![], vec![]);
        assert_eq!(plain(&l), vec!["hello brave", "new world"]);
    }

    #[test]
    fn wraps_with_prefix() {
        let l = wrap_spans(
            vec![Span::raw("aaa bbb ccc ddd")],
            9,
            vec![Span::raw("• ")],
            vec![Span::raw("  ")],
        );
        assert_eq!(plain(&l), vec!["• aaa bbb", "  ccc ddd"]);
    }

    #[test]
    fn splits_long_words() {
        let l = wrap_spans(vec![Span::raw("abcdefghij")], 4, vec![], vec![]);
        assert_eq!(plain(&l), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn newline_breaks() {
        let l = wrap_spans(vec![Span::raw("a\nb")], 10, vec![], vec![]);
        assert_eq!(plain(&l), vec!["a", "b"]);
    }
}

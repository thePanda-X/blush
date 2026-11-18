//! Markdown → ratatui lines, with syntect highlighting for fenced code.

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme as SynTheme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::text::{hard_wrap, pad_line, sanitize, wrap_spans, width};
use crate::theme::Theme;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static T: OnceLock<ThemeSet> = OnceLock::new();
    T.get_or_init(ThemeSet::load_defaults)
}

fn syn_theme(name: &str) -> &'static SynTheme {
    let ts = themes();
    ts.themes.get(name).unwrap_or_else(|| ts.themes.values().next().unwrap())
}

/// Warm the syntax caches off the UI thread.
pub fn preload() {
    std::thread::spawn(|| {
        let _ = syntaxes();
        let _ = themes();
    });
}

/// Highlight `code` (one entry per line) for the language `lang` (name or extension).
pub fn highlight(code: &str, lang: &str, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let ss = syntaxes();
    let lang = lang.trim().split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
    let syntax = (!lang.is_empty())
        .then(|| {
            ss.find_syntax_by_token(lang)
                .or_else(|| ss.find_syntax_by_extension(lang))
                .or_else(|| match lang {
                    "ts" | "tsx" | "typescript" | "jsx" => ss.find_syntax_by_extension("js"),
                    "sh" | "bash" | "zsh" | "shell" | "console" => ss.find_syntax_by_extension("sh"),
                    "toml" | "ini" => ss.find_syntax_by_extension("ini"),
                    "ps1" | "powershell" => ss.find_syntax_by_extension("sh"),
                    _ => None,
                })
        })
        .flatten();
    let plain = |l: &str| vec![Span::styled(sanitize(l), Style::default().fg(theme.text_secondary))];
    let Some(syntax) = syntax else {
        return code.lines().map(plain).collect();
    };
    if matches!(theme.bg, Color::Reset) {
        return code.lines().map(plain).collect();
    }
    let mut h = HighlightLines::new(syntax, syn_theme(theme.syntax_theme));
    let mut out = Vec::new();
    for line in code.split_inclusive('\n') {
        match h.highlight_line(line, ss) {
            Ok(ranges) => {
                let spans = ranges
                    .into_iter()
                    .filter_map(|(st, t)| {
                        let t = sanitize(t.trim_end_matches('\n'));
                        if t.is_empty() {
                            return None;
                        }
                        let mut style =
                            Style::default().fg(Color::Rgb(st.foreground.r, st.foreground.g, st.foreground.b));
                        if st.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if st.font_style.contains(syntect::highlighting::FontStyle::ITALIC) {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        Some(Span::styled(t, style))
                    })
                    .collect();
                out.push(spans);
            }
            Err(_) => out.push(plain(line.trim_end_matches('\n'))),
        }
    }
    out
}

#[derive(Clone)]
enum ListKind {
    Bullet,
    Ordered(u64),
}

struct Renderer<'t> {
    theme: &'t Theme,
    width: usize,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    lists: Vec<ListKind>,
    item_prefix: Option<Vec<Span<'static>>>,
    quote_depth: usize,
    code: Option<(String, String)>,
    heading: Option<HeadingLevel>,
    link: Option<String>,
    table: Option<Table>,
    pending_blank: bool,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<Span<'static>>>>,
    header_rows: usize,
    cur_row: Vec<Vec<Span<'static>>>,
    in_head: bool,
}

impl<'t> Renderer<'t> {
    fn style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default().fg(self.theme.md_text))
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let s = f(self.style());
        self.style_stack.push(s);
    }

    fn indent_prefix(&self) -> Vec<Span<'static>> {
        let mut p = Vec::new();
        for _ in 0..self.quote_depth {
            p.push(Span::styled("┃ ", Style::default().fg(self.theme.gray_dim)));
        }
        let list_indent = self.lists.len().saturating_sub(1) * 2 + if self.lists.is_empty() { 0 } else { 2 };
        if list_indent > 0 {
            p.push(Span::raw(" ".repeat(list_indent)));
        }
        p
    }

    fn blank(&mut self) {
        if !self.out.is_empty() {
            self.pending_blank = true;
        }
    }

    fn emit_blank_if_pending(&mut self) {
        if self.pending_blank {
            self.pending_blank = false;
            let q = self.quote_prefix();
            self.out.push(Line::from(q));
        }
    }

    fn quote_prefix(&self) -> Vec<Span<'static>> {
        (0..self.quote_depth)
            .map(|_| Span::styled("┃ ", Style::default().fg(self.theme.gray_dim)))
            .collect()
    }

    fn flush_paragraph(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        self.emit_blank_if_pending();
        let spans = std::mem::take(&mut self.spans);
        let rest = self.indent_prefix();
        let first = match self.item_prefix.take() {
            Some(item) => {
                let mut f = self.indent_prefix();
                // Replace the trailing list indent with the bullet.
                let item_w: usize = item.iter().map(|s| width(&s.content)).sum();
                if let Some(last) = f.last_mut() {
                    if last.content.trim().is_empty() && width(&last.content) >= item_w {
                        let keep = width(&last.content) - item_w;
                        *last = Span::raw(" ".repeat(keep));
                    }
                }
                f.extend(item);
                f
            }
            None => rest.clone(),
        };
        let lines = wrap_spans(spans, self.width, first, rest);
        self.out.extend(lines);
    }

    fn handle(&mut self, ev: Event<'_>) {
        if let Some(t) = self.table.as_mut() {
            match &ev {
                Event::Text(s) | Event::Code(s) => {
                    let style = if matches!(ev, Event::Code(_)) {
                        Style::default().fg(self.theme.code_inline)
                    } else {
                        *self.style_stack.last().unwrap_or(&Style::default().fg(self.theme.md_text))
                    };
                    self.spans.push(Span::styled(sanitize(s), style));
                    return;
                }
                Event::Start(Tag::TableHead) => {
                    t.in_head = true;
                    return;
                }
                Event::End(TagEnd::TableHead) => {
                    let row = std::mem::take(&mut t.cur_row);
                    t.rows.push(row);
                    t.header_rows = t.rows.len();
                    t.in_head = false;
                    return;
                }
                Event::Start(Tag::TableRow) => return,
                Event::End(TagEnd::TableRow) => {
                    let row = std::mem::take(&mut t.cur_row);
                    t.rows.push(row);
                    return;
                }
                Event::Start(Tag::TableCell) => {
                    self.spans.clear();
                    return;
                }
                Event::End(TagEnd::TableCell) => {
                    let cell = std::mem::take(&mut self.spans);
                    t.cur_row.push(cell);
                    return;
                }
                Event::End(TagEnd::Table) => {
                    let t = self.table.take().unwrap();
                    self.render_table(t);
                    return;
                }
                _ => {}
            }
        }

        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    self.flush_paragraph();
                    self.blank();
                    self.heading = Some(level);
                    let (c, bold) = match level {
                        HeadingLevel::H1 => (self.theme.h1, true),
                        HeadingLevel::H2 => (self.theme.h2, true),
                        HeadingLevel::H3 => (self.theme.h3, true),
                        _ => (self.theme.h4, true),
                    };
                    self.push_style(|s| {
                        let s = s.fg(c);
                        if bold { s.add_modifier(Modifier::BOLD) } else { s }
                    });
                }
                Tag::BlockQuote(_) => {
                    self.flush_paragraph();
                    self.blank();
                    self.quote_depth += 1;
                    self.push_style(|s| s.fg(self.theme.gray_bright).add_modifier(Modifier::ITALIC));
                }
                Tag::CodeBlock(kind) => {
                    self.flush_paragraph();
                    self.blank();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    self.flush_paragraph();
                    if self.lists.is_empty() {
                        self.blank();
                    }
                    self.lists.push(match start {
                        Some(n) => ListKind::Ordered(n),
                        None => ListKind::Bullet,
                    });
                }
                Tag::Item => {
                    self.flush_paragraph();
                    let depth = self.lists.len();
                    let marker = match self.lists.last_mut() {
                        Some(ListKind::Ordered(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => if depth > 1 { "◦ ".to_string() } else { "• ".to_string() },
                    };
                    self.item_prefix = Some(vec![Span::styled(marker, Style::default().fg(self.theme.gray))]);
                }
                Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD).fg(self.theme.text)),
                Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    self.link = Some(dest_url.to_string());
                    self.push_style(|s| s.fg(self.theme.link).add_modifier(Modifier::UNDERLINED));
                }
                Tag::Image { dest_url, .. } => {
                    self.spans.push(Span::styled(
                        format!("[image: {dest_url}]"),
                        Style::default().fg(self.theme.gray),
                    ));
                    self.push_style(|s| s);
                }
                Tag::Table(_) => {
                    self.flush_paragraph();
                    self.blank();
                    self.table = Some(Table::default());
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    self.flush_paragraph();
                    if self.lists.is_empty() {
                        self.blank();
                    }
                }
                TagEnd::Heading(_) => {
                    self.flush_paragraph();
                    self.style_stack.pop();
                    self.heading = None;
                    self.blank();
                }
                TagEnd::BlockQuote(_) => {
                    self.flush_paragraph();
                    self.quote_depth = self.quote_depth.saturating_sub(1);
                    self.style_stack.pop();
                    self.blank();
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = self.code.take() {
                        self.emit_blank_if_pending();
                        self.render_code(&lang, &code);
                        self.blank();
                    }
                }
                TagEnd::List(_) => {
                    self.flush_paragraph();
                    self.lists.pop();
                    if self.lists.is_empty() {
                        self.blank();
                    }
                }
                TagEnd::Item => self.flush_paragraph(),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Image => {
                    self.style_stack.pop();
                }
                TagEnd::Link => {
                    self.style_stack.pop();
                    self.link = None;
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, code)) = self.code.as_mut() {
                    code.push_str(&t);
                } else {
                    let s = self.style();
                    self.spans.push(Span::styled(sanitize(&t), s));
                }
            }
            Event::Code(t) => {
                let s = Style::default().fg(self.theme.code_inline);
                self.spans.push(Span::styled(sanitize(&t), s));
            }
            Event::SoftBreak => {
                let s = self.style();
                self.spans.push(Span::styled(" ", s));
            }
            Event::HardBreak => self.spans.push(Span::raw("\n")),
            Event::Rule => {
                self.flush_paragraph();
                self.emit_blank_if_pending();
                self.out.push(Line::from(Span::styled("───", Style::default().fg(self.theme.gray_dim))));
                self.blank();
            }
            Event::TaskListMarker(done) => {
                let (m, c) = if done { ("✓ ", self.theme.success) } else { ("○ ", self.theme.gray) };
                self.item_prefix = Some(vec![Span::styled(m, Style::default().fg(c))]);
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let s = Style::default().fg(self.theme.gray);
                self.spans.push(Span::styled(sanitize(&h), s));
            }
            Event::FootnoteReference(r) => self.spans.push(Span::raw(format!("[{r}]"))),
            Event::InlineMath(m) | Event::DisplayMath(m) => {
                self.spans.push(Span::styled(sanitize(&m), Style::default().fg(self.theme.code_inline)))
            }
        }
    }

    fn render_code(&mut self, lang: &str, code: &str) {
        let bg = self.theme.bg_raised;
        let fill = Style::default().bg(bg);
        let prefix = self.indent_prefix();
        let pw: usize = prefix.iter().map(|s| width(&s.content)).sum();
        let inner = self.width.saturating_sub(pw);
        let code = code.strip_suffix('\n').unwrap_or(code);
        let highlighted = highlight(code, lang, self.theme);
        let label = lang.split_whitespace().next().unwrap_or("");
        for (i, spans) in highlighted.into_iter().enumerate() {
            let spans: Vec<Span<'static>> =
                spans.into_iter().map(|s| Span::styled(s.content, s.style.bg(bg))).collect();
            let mut lead = vec![Span::styled(" ", fill)];
            lead.splice(0..0, prefix.clone());
            for (j, l) in hard_wrap(spans, self.width.saturating_sub(1), lead.clone()).into_iter().enumerate() {
                let mut l = l;
                // Language label at the right of the first row.
                if i == 0 && j == 0 && !label.is_empty() && inner > label.len() + 8 {
                    let used = crate::text::line_width(&l);
                    let target = self.width.saturating_sub(label.len() + 1);
                    if used < target {
                        l.spans.push(Span::styled(" ".repeat(target - used), fill));
                        l.spans.push(Span::styled(label.to_string(), Style::default().fg(self.theme.gray_dim).bg(bg)));
                    }
                }
                self.out.push(pad_line(l, self.width, fill));
            }
        }
        if code.is_empty() {
            self.out.push(pad_line(Line::from(prefix), self.width, fill));
        }
    }

    fn render_table(&mut self, t: Table) {
        self.emit_blank_if_pending();
        if t.rows.is_empty() {
            return;
        }
        let ncols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; ncols];
        for r in &t.rows {
            for (i, c) in r.iter().enumerate() {
                let w: usize = c.iter().map(|s| width(&s.content)).sum();
                widths[i] = widths[i].max(w);
            }
        }
        let total: usize = widths.iter().sum::<usize>() + ncols.saturating_sub(1) * 3;
        let border = Style::default().fg(self.theme.gray_dim);
        let prefix = self.indent_prefix();
        if total + 2 > self.width {
            // Too wide: one "header: value" line per cell.
            for (ri, r) in t.rows.iter().enumerate().skip(t.header_rows) {
                for (ci, c) in r.iter().enumerate() {
                    let mut spans = Vec::new();
                    if let Some(h) = t.rows.first().and_then(|h| h.get(ci)) {
                        for s in h {
                            spans.push(Span::styled(s.content.clone(), Style::default().fg(self.theme.gray)));
                        }
                        spans.push(Span::styled(": ", Style::default().fg(self.theme.gray)));
                    }
                    spans.extend(c.iter().cloned());
                    let first = if ci == 0 {
                        vec![Span::styled("• ", Style::default().fg(self.theme.gray))]
                    } else {
                        vec![Span::raw("  ")]
                    };
                    self.out.extend(wrap_spans(spans, self.width, first, vec![Span::raw("    ")]));
                }
                let _ = ri;
            }
            self.blank();
            return;
        }
        for (ri, r) in t.rows.iter().enumerate() {
            let mut spans = prefix.clone();
            for ci in 0..ncols {
                let cell = r.get(ci).cloned().unwrap_or_default();
                let w: usize = cell.iter().map(|s| width(&s.content)).sum();
                for s in cell {
                    let st = if ri < t.header_rows { s.style.add_modifier(Modifier::BOLD).fg(self.theme.text) } else { s.style };
                    spans.push(Span::styled(s.content, st));
                }
                spans.push(Span::raw(" ".repeat(widths[ci] - w)));
                if ci + 1 < ncols {
                    spans.push(Span::styled(" │ ", border));
                }
            }
            self.out.push(Line::from(spans));
            if ri + 1 == t.header_rows {
                let mut sep = prefix.clone();
                let parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                sep.push(Span::styled(parts.join("─┼─"), border));
                self.out.push(Line::from(sep));
            }
        }
        self.blank();
    }
}

pub fn render(md: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let mut r = Renderer {
        theme,
        width,
        out: Vec::new(),
        spans: Vec::new(),
        style_stack: vec![Style::default().fg(theme.md_text)],
        lists: Vec::new(),
        item_prefix: None,
        quote_depth: 0,
        code: None,
        heading: None,
        link: None,
        table: None,
        pending_blank: false,
    };
    // Close an unterminated fence while streaming so the partial block still renders as code.
    let fences = md.lines().filter(|l| l.trim_start().starts_with("```")).count();
    let owned;
    let src = if fences % 2 == 1 {
        owned = format!("{md}\n```");
        owned.as_str()
    } else {
        md
    };
    for ev in Parser::new_ext(src, opts) {
        r.handle(ev);
    }
    if let Some(t) = r.table.take() {
        r.render_table(t);
    }
    r.flush_paragraph();
    while r.out.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()) && l.spans.iter().all(|s| s.style.bg.is_none())) {
        r.out.pop();
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn renders_lists_and_headings() {
        let t = Theme::blush();
        let out = plain(&render("# Title\n\nSome **bold** text.\n\n- one\n- two\n", 40, &t));
        assert_eq!(out, vec!["Title", "", "Some bold text.", "", "• one", "• two"]);
    }

    #[test]
    fn renders_code() {
        let t = Theme::blush();
        let out = render("```rust\nfn main() {}\n```", 30, &t);
        assert_eq!(out.len(), 1);
        assert!(plain(&out)[0].contains("fn main() {}"));
    }
}

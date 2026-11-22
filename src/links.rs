//! Clickable links in the scrollback: find where links sit in rendered lines,
//! and hand a clicked one to the system browser.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::text::width;

/// A clickable span within an entry's rendered lines: (line index, start col, end col, url).
pub type Hit = (usize, u16, u16, String);

/// Locate links in `lines`. Markdown links are the underlined runs (only links
/// are underlined) and get their target from `md`, the source the lines were
/// rendered from; bare `http(s)://` URLs anywhere are clickable as written.
pub fn scan(lines: &[Line], md: Option<&str>) -> Vec<Hit> {
    let mut hits = Vec::new();
    // Underlined runs, in reading order: (line, x0, x1, text).
    let mut runs: Vec<(usize, u16, u16, String)> = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let mut x = 0usize;
        for span in &line.spans {
            let w = width(&span.content);
            if span.style.add_modifier.contains(Modifier::UNDERLINED) {
                match runs.last_mut() {
                    Some(r) if r.0 == li && r.2 as usize == x => {
                        r.2 = (x + w) as u16;
                        r.3.push_str(&span.content);
                    }
                    _ => runs.push((li, x as u16, (x + w) as u16, span.content.to_string())),
                }
            } else {
                for (s, e) in find_urls(&span.content) {
                    let url = &span.content[s..e];
                    if url.ends_with('…') {
                        continue; // truncated for display; the real target is unknown
                    }
                    let x0 = x + width(&span.content[..s]);
                    hits.push((li, x0 as u16, (x0 + width(url)) as u16, url.to_string()));
                }
            }
            x += w;
        }
    }
    if runs.is_empty() {
        return hits;
    }
    let targets = md.map(extract).unwrap_or_default();
    let mut next = 0; // links appear in the same order as in the source
    let mut i = 0;
    while i < runs.len() {
        let mut text = squash(&runs[i].3);
        let mut j = i;
        let found = targets[next.min(targets.len())..].iter().position(|(t, _)| !text.is_empty() && t.starts_with(&text));
        let url = match found {
            Some(k) => {
                let (t, url) = &targets[next + k];
                // A link wrapped onto following lines continues in the next run.
                while text != *t && j + 1 < runs.len() && runs[j + 1].0 == runs[j].0 + 1 {
                    let more = format!("{text}{}", squash(&runs[j + 1].3));
                    if !t.starts_with(&more) {
                        break;
                    }
                    text = more;
                    j += 1;
                }
                next += k + 1;
                Some(url.clone())
            }
            None => {
                let t = runs[i].3.trim();
                (t.starts_with("http://") || t.starts_with("https://")).then(|| t.to_string())
            }
        };
        if let Some(url) = url {
            for r in &runs[i..=j] {
                hits.push((r.0, r.1, r.2, url.clone()));
            }
        }
        i = j + 1;
    }
    hits
}

/// (whitespace-free text, url) of each link in `md`, in order.
fn extract(md: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for ev in Parser::new_ext(md, pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH) {
        match ev {
            Event::Start(Tag::Link { dest_url, .. }) => cur = Some((String::new(), dest_url.to_string())),
            Event::End(TagEnd::Link) => {
                if let Some((t, u)) = cur.take() {
                    out.push((squash(&t), u));
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((s, _)) = cur.as_mut() {
                    s.push_str(&t);
                }
            }
            _ => {}
        }
    }
    out
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()
}

/// Byte ranges of bare `http(s)://` URLs in `s`, minus trailing punctuation.
pub fn find_urls(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = s[from..].find("http") {
        let start = from + off;
        let rest = &s[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            from = start + 4;
            continue;
        };
        let boundary = start == 0 || !s[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '\'' | '│' | '┃'))
            .unwrap_or(rest.len());
        let mut url = &rest[..len];
        loop {
            let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*', '_']);
            let trimmed = match trimmed.chars().last() {
                Some(c @ (')' | ']' | '}')) => {
                    let open = match c {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if trimmed.matches(open).count() < trimmed.matches(c).count() { &trimmed[..trimmed.len() - 1] } else { trimmed }
                }
                _ => trimmed,
            };
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        if boundary && url.len() > scheme {
            out.push((start, start + url.len()));
        }
        from = start + len.max(scheme);
    }
    out
}

/// Open `url` in the default browser. Only web and mail links are opened, so a
/// crafted `file:` or custom-scheme link can't launch anything else.
pub fn open(url: &str) -> std::io::Result<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return Err(std::io::Error::other("only http(s) and mailto links can be opened"));
    }
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    let mut cmd = {
        // Unlike `cmd /c start`, this doesn't reinterpret `&` and friends in the URL.
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_urls_drop_trailing_punctuation() {
        let s = "see https://example.com/a_(b). and (http://x.io/y), not xhttps://no";
        let got: Vec<&str> = find_urls(s).into_iter().map(|(a, b)| &s[a..b]).collect();
        assert_eq!(got, vec!["https://example.com/a_(b)", "http://x.io/y"]);
    }

    #[test]
    fn wrapped_markdown_links_resolve_to_their_target() {
        let th = crate::theme::Theme::blush();
        let md = "Read [the very long documentation page title](https://docs.example.com/x) and [here](https://a.b/c), also https://plain.dev/p.";
        let lines = crate::markdown::render(md, 24, &th);
        let hits = scan(&lines, Some(md));
        let urls: Vec<&str> = hits.iter().map(|h| h.3.as_str()).collect();
        assert!(urls.iter().filter(|u| **u == "https://docs.example.com/x").count() >= 2, "{urls:?}");
        assert!(urls.contains(&"https://a.b/c"), "{urls:?}");
        assert!(urls.contains(&"https://plain.dev/p"), "{urls:?}");
    }
}

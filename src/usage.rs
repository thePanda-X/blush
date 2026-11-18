//! Usage meters: subscription limits (parsed from the adapter's `/usage`
//! command, which runs locally without a model call) and per-turn token
//! counts (from the `usage` field of `session/prompt` responses).

use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// "5h", "week", or a model-scoped weekly label.
    pub label: String,
    pub percent: f64,
    pub resets: Option<DateTime<Local>>,
}

/// Pull the limit windows out of `/usage` markdown, e.g.
/// `**5-hour limit** — **34%** · Resets Nov 18, 9:00 PM PST`.
pub fn parse_limits(md: &str) -> Vec<LimitWindow> {
    let mut out = Vec::new();
    for line in md.lines() {
        let line = normalize_spaces(line.trim());
        if let Some(w) = parse_plain_line(&line, Local::now()) {
            out.push(w);
            continue;
        }
        let Some(rest) = line.strip_prefix("**") else { continue };
        let Some((label, rest)) = rest.split_once("**") else { continue };
        let Some(rest) = rest.trim_start().strip_prefix('—').map(str::trim_start) else { continue };
        let Some(rest) = rest.strip_prefix("**") else { continue };
        let Some((pct, rest)) = rest.split_once("%**") else { continue };
        let Ok(percent) = pct.trim().parse::<f64>() else { continue };
        let label = label.replace('\\', "");
        let short = match label.as_str() {
            "5-hour limit" => "5h".to_string(),
            "Weekly · all models" => "week".to_string(),
            l => l.strip_prefix("Weekly · ").map(|m| format!("week {}", m.to_lowercase())).unwrap_or_else(|| l.to_string()),
        };
        let resets = rest.split_once("Resets ").and_then(|(_, r)| parse_reset(r.trim(), Local::now()));
        out.push(LimitWindow { label: short, percent, resets });
    }
    out
}

/// The windows the footer meter alternates between: the 5-hour and the
/// all-models weekly limit, or whatever was reported if neither was.
pub fn meter_windows(all: &[LimitWindow]) -> Vec<&LimitWindow> {
    let main: Vec<&LimitWindow> = all.iter().filter(|w| w.label == "5h" || w.label == "week").collect();
    if main.is_empty() { all.iter().take(1).collect() } else { main }
}

/// Claude Code's own text format, which the adapter falls back to when its
/// structured `/usage` call times out:
/// `Current week (all models): 20% used · resets Nov 23, 4pm (Europe/Paris)`
/// `Current session: 34% used · resets 9pm (Europe/Paris)`
fn parse_plain_line(line: &str, now: DateTime<Local>) -> Option<LimitWindow> {
    let rest = line.strip_prefix("Current ")?;
    let (label, rest) = rest.split_once(':')?;
    let (pct, rest) = rest.trim().split_once('%')?;
    let percent = pct.trim().parse::<f64>().ok()?;
    let label = match label.trim() {
        "session" => "5h".to_string(),
        "week (all models)" => "week".to_string(),
        l => match l.strip_prefix("week (").and_then(|m| m.strip_suffix(')')) {
            Some(m) => format!("week {}", m.to_lowercase()),
            None => l.to_string(),
        },
    };
    let resets = rest.split_once("resets ").and_then(|(_, r)| {
        let r = r.split(" (").next().unwrap_or(r).trim();
        parse_plain_reset(r, now)
    });
    Some(LimitWindow { label, percent, resets })
}

/// `Nov 23, 4pm`, `Nov 23, 4:30pm`, `9pm` (today, or tomorrow if passed).
fn parse_plain_reset(s: &str, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let (date, time) = match s.rsplit_once(", ") {
        Some((d, t)) => (Some(d.trim()), t.trim()),
        None => (None, s.trim()),
    };
    let t = time.to_lowercase();
    let (clock, ampm) = if let Some(c) = t.strip_suffix("am") {
        (c, "AM")
    } else if let Some(c) = t.strip_suffix("pm") {
        (c, "PM")
    } else {
        return None;
    };
    let clock = if clock.contains(':') { clock.trim().to_string() } else { format!("{}:00", clock.trim()) };
    match date {
        Some(d) => parse_reset(&format!("{d}, {clock} {ampm}"), now),
        None => {
            let naive_t = chrono::NaiveTime::parse_from_str(&format!("{clock} {ampm}"), "%I:%M %p").ok()?;
            let today = Local.from_local_datetime(&now.date_naive().and_time(naive_t)).earliest()?;
            Some(if today < now { today + chrono::Duration::days(1) } else { today })
        }
    }
}

fn normalize_spaces(s: &str) -> String {
    s.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect()
}

/// Parse `Nov 18, 9:00 PM PST` (Intl "en" short format, in the machine's
/// local zone, which is also where the adapter runs). The trailing zone name
/// is ignored; the year is inferred as the next occurrence.
fn parse_reset(s: &str, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let mut words: Vec<&str> = s.split(' ').filter(|w| !w.is_empty()).collect();
    // Keep "Mon D, H:MM AM"
    if words.len() > 4 {
        words.truncate(4);
    }
    let text = words.join(" ");
    for year in [now.year(), now.year() + 1] {
        let candidate = format!("{year} {text}");
        if let Ok(naive) = NaiveDateTime::parse_from_str(&candidate, "%Y %b %d, %I:%M %p") {
            if let Some(dt) = Local.from_local_datetime(&naive).earliest() {
                if dt > now - chrono::Duration::days(1) {
                    return Some(dt);
                }
            }
        }
    }
    None
}

/// Compact "time until" like `2h10m` or `3d4h`.
pub fn until(t: DateTime<Local>, now: DateTime<Local>) -> String {
    let secs = (t - now).num_seconds().max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    /// From a `session/prompt` response's `usage` object, if present.
    pub fn from_prompt_result(v: &Value) -> Option<Tokens> {
        let u = v.get("usage")?;
        let n = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let t = Tokens {
            input: n("inputTokens"),
            output: n("outputTokens"),
            cache_read: n("cachedReadTokens"),
            cache_write: n("cachedWriteTokens"),
        };
        (t != Tokens::default()).then_some(t)
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_markdown() {
        let md = "## Usage\n\n### Limits\n\n**5-hour limit** — **34%** · Resets Nov 18, 9:00\u{202f}PM PST\n\n`███░░`\n\n\
                  **Weekly · all models** — **12%** · Resets Nov 24, 10:00 AM PST\n\n**Weekly · Opus** — **3%**\n";
        let now = Local.with_ymd_and_hms(2026, 11, 18, 20, 0, 0).unwrap();
        let v = parse_limits(md);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].label, "5h");
        assert_eq!(v[0].percent, 34.0);
        assert!(v[0].resets.is_some());
        assert_eq!(v[1].label, "week");
        assert_eq!(v[2].label, "week opus");
        assert!(v[2].resets.is_none());
        let r = parse_reset("Nov 18, 9:00 PM PST", now).unwrap();
        assert_eq!(until(r, now), "1h00m");
        // A reset early next year rolls over.
        let dec = Local.with_ymd_and_hms(2026, 12, 30, 12, 0, 0).unwrap();
        assert_eq!(parse_reset("Jan 2, 9:00 AM PST", dec).unwrap().year(), 2027);
    }

    #[test]
    fn parses_real_adapter_output() {
        // Captured from claude-agent-acp 0.108 on a Max plan (no 5-hour window reported).
        let md = "## Usage\n\n> Claude max subscription usage\n\n### Limits\n\n\
                  **Weekly · all models** — **19%** · Resets Nov 23, 4:00 PM GMT+1\n\n`████░░░░░░░░░░░░░░░░`\n\n\
                  **Weekly · Fable** — **0%** · Resets Nov 23, 4:00 PM GMT+1\n\n`░░░░░░░░░░░░░░░░░░░░`\n\n---\n\n### This session\n";
        let v = parse_limits(md);
        assert_eq!(v.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["week", "week fable"]);
        assert_eq!(v[0].percent, 19.0);
        assert!(v[0].resets.is_some());
        assert_eq!(meter_windows(&v).len(), 1, "only the all-models week is shown");
    }

    #[test]
    fn parses_claude_code_plain_text() {
        let txt = "You are currently using your subscription to power your Claude Code usage\n\n\
                   Current session: 34% used · resets 9pm (Europe/Paris)\n\
                   Current week (all models): 20% used · resets Nov 23, 4pm (Europe/Paris)\n\
                   Current week (Fable): 0% used · resets Nov 23, 4:30pm (Europe/Paris)\n\n\
                   Last 24h · 1563 requests · 28 sessions\n  87%  Start fresh when you switch tasks";
        let v = parse_limits(txt);
        assert_eq!(v.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["5h", "week", "week fable"]);
        assert_eq!(v[1].percent, 20.0);
        assert!(v.iter().all(|w| w.resets.is_some()));
        let now = Local.with_ymd_and_hms(2026, 11, 18, 22, 0, 0).unwrap();
        // 9pm already passed today, so it's tomorrow at 9pm.
        assert_eq!(until(parse_plain_reset("9pm", now).unwrap(), now), "23h00m");
        assert_eq!(meter_windows(&v).len(), 2);
    }

    #[test]
    fn reads_prompt_usage() {
        let v = serde_json::json!({"stopReason": "end_turn", "usage": {"inputTokens": 10, "outputTokens": 5, "cachedReadTokens": 100, "cachedWriteTokens": 0, "totalTokens": 115}});
        let t = Tokens::from_prompt_result(&v).unwrap();
        assert_eq!(t.total(), 115);
        assert!(Tokens::from_prompt_result(&serde_json::json!({"stopReason": "end_turn"})).is_none());
    }
}

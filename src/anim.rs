//! Small animation helpers: frame timing, easing, tweens, shimmer and fades.
//!
//! Everything here is driven by the main loop's tick (see [`TICK_MS`]) or by
//! wall-clock `Instant`s, so animations run at the same speed whatever the
//! redraw rate.

use std::cell::Cell;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::theme::blend;

/// Main-loop tick length. ~30 fps while something animates; idle frames are skipped.
pub const TICK_MS: u64 = 33;

/// Milliseconds since startup, measured in ticks.
pub fn tick_ms(tick: u64) -> u64 {
    tick.wrapping_mul(TICK_MS)
}

/// Frame `period_ms` long, picked from `frames` by tick.
pub fn frame<'a>(frames: &[&'a str], tick: u64, period_ms: u64) -> &'a str {
    frames[(tick_ms(tick) / period_ms.max(1)) as usize % frames.len()]
}

pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Progress (0..=1) of an animation started at `at` lasting `dur`.
pub fn progress(at: Instant, dur: Duration) -> f32 {
    (at.elapsed().as_secs_f32() / dur.as_secs_f32().max(0.001)).clamp(0.0, 1.0)
}

/// A number that glides to new targets instead of jumping. Interior
/// mutability so it can be read (and retargeted) from `&App` while drawing.
pub struct Tween {
    from: Cell<f64>,
    to: Cell<f64>,
    at: Cell<Option<Instant>>,
    dur: Duration,
}

impl Tween {
    pub fn new(ms: u64) -> Tween {
        Tween { from: Cell::new(0.0), to: Cell::new(0.0), at: Cell::new(None), dur: Duration::from_millis(ms) }
    }

    /// Current value on the way to `target` (retargets when it changes).
    /// Resets to zero apply instantly; everything else counts.
    pub fn get(&self, target: f64) -> f64 {
        if target != self.to.get() {
            let now = self.value();
            if target == 0.0 {
                self.from.set(target);
                self.at.set(None);
            } else {
                self.from.set(now);
                self.at.set(Some(Instant::now()));
            }
            self.to.set(target);
        }
        self.value()
    }

    fn value(&self) -> f64 {
        match self.at.get() {
            None => self.to.get(),
            Some(at) => {
                let t = progress(at, self.dur);
                if t >= 1.0 {
                    self.at.set(None);
                    return self.to.get();
                }
                let k = ease_out(t) as f64;
                self.from.get() + (self.to.get() - self.from.get()) * k
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.at.get().is_some_and(|at| at.elapsed() < self.dur)
    }
}

/// The numbers on screen that count up rather than jump.
pub struct Tweens {
    /// Streamed output tokens (`⇣36`).
    pub out_tokens: Tween,
    /// Context use, percent (status row).
    pub context: Tween,
    /// Context use, tokens (header).
    pub header_used: Tween,
    /// Subscription meter fill, percent.
    pub sub_meter: Tween,
}

impl Default for Tweens {
    fn default() -> Tweens {
        Tweens { out_tokens: Tween::new(300), context: Tween::new(450), header_used: Tween::new(450), sub_meter: Tween::new(600) }
    }
}

impl Tweens {
    pub fn busy(&self) -> bool {
        self.out_tokens.busy() || self.context.busy() || self.header_used.busy() || self.sub_meter.busy()
    }
}

/// `text` with a soft highlight sweeping across it, left to right.
pub fn shimmer(text: &str, tick: u64, base: Color, hi: Color) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as f32;
    // One sweep per ~1.6s, with a pause off the right edge before the next.
    let span = n + 8.0;
    let pos = (tick_ms(tick) as f32 / 1600.0 * span) % span - 3.0;
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let d = (i as f32 - pos).abs();
            let k = (1.0 - d / 3.0).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            Span::styled(c.to_string(), Style::default().fg(blend(base, hi, k)))
        })
        .collect()
}

/// Fade a drawn region in from `bg`: `alpha` 0 → invisible, 1 → as drawn.
pub fn fade_rect(buf: &mut Buffer, r: Rect, bg: Color, alpha: f32) {
    if alpha >= 1.0 || !matches!(bg, Color::Rgb(..)) {
        return;
    }
    let r = r.intersection(buf.area);
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                let (fg, cbg) = (cell.fg, cell.bg);
                if matches!(fg, Color::Rgb(..)) {
                    cell.set_fg(blend(bg, fg, alpha));
                }
                if matches!(cbg, Color::Rgb(..)) {
                    cell.set_bg(blend(bg, cbg, alpha));
                }
            }
        }
    }
}

/// How many characters of streamed text to reveal this tick: a steady
/// trickle, speeding up so a backlog never falls more than ~200ms behind.
pub fn reveal_step(backlog: usize) -> usize {
    (backlog / 6).max(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tween_starts_at_first_value_and_settles() {
        let t = Tween::new(10);
        assert!(t.get(5.0) < 5.0);
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(t.get(5.0), 5.0);
        let v = t.get(10.0);
        assert!((5.0..=10.0).contains(&v));
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(t.get(10.0), 10.0);
        assert!(!t.busy());
        assert_eq!(t.get(0.0), 0.0);
    }

    #[test]
    fn shimmer_keeps_text() {
        let s: String = shimmer("Responding…", 7, Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255))
            .into_iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(s, "Responding…");
    }

    #[test]
    fn reveal_catches_up() {
        assert_eq!(reveal_step(0), 3);
        assert_eq!(reveal_step(600), 100);
    }
}

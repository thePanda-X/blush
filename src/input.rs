//! Multi-line prompt editor with soft wrapping and history.

use unicode_segmentation::UnicodeSegmentation;

use crate::text::width;

#[derive(Default, Clone)]
pub struct Input {
    pub text: String,
    /// Byte offset of the cursor (always on a grapheme boundary).
    pub cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
    /// Preferred visual column for vertical movement.
    goal_col: Option<usize>,
}

#[allow(dead_code)]
impl Input {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.goal_col = None;
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.len();
        self.goal_col = None;
    }

    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_pos = None;
        self.goal_col = None;
        if !t.trim().is_empty() && self.history.last() != Some(&t) {
            self.history.push(t.clone());
        }
        t
    }

    pub fn load_history(&mut self, items: Vec<String>) {
        self.history = items;
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.goal_col = None;
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.goal_col = None;
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| from + i)
            .unwrap_or(self.text.len())
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let p = self.prev_boundary(self.cursor);
        self.text.replace_range(p..self.cursor, "");
        self.cursor = p;
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let n = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..n, "");
        self.goal_col = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
        self.goal_col = None;
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    pub fn word_left(&mut self) {
        let before: Vec<(usize, char)> = self.text[..self.cursor].char_indices().collect();
        let mut i = before.len();
        while i > 0 && !Self::is_word(before[i - 1].1) {
            i -= 1;
        }
        while i > 0 && Self::is_word(before[i - 1].1) {
            i -= 1;
        }
        self.cursor = before.get(i).map(|(b, _)| *b).unwrap_or(0);
        if i == 0 {
            self.cursor = 0;
        }
        self.goal_col = None;
    }

    pub fn word_right(&mut self) {
        let mut it = self.text[self.cursor..].char_indices().peekable();
        let mut off = self.text.len() - self.cursor;
        let mut seen_word = false;
        while let Some((i, c)) = it.next() {
            if Self::is_word(c) {
                seen_word = true;
            } else if seen_word {
                off = i;
                break;
            }
        }
        self.cursor += off;
        self.goal_col = None;
    }

    pub fn delete_word_left(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    pub fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.text.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
        self.goal_col = None;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
        self.goal_col = None;
    }

    pub fn kill_to_start(&mut self) {
        let s = self.line_start();
        if s == self.cursor && s > 0 {
            // At column 0: join with the previous line.
            self.backspace();
            return;
        }
        self.text.replace_range(s..self.cursor, "");
        self.cursor = s;
    }

    pub fn kill_to_end(&mut self) {
        let e = self.line_end();
        if e == self.cursor && e < self.text.len() {
            self.delete();
            return;
        }
        self.text.replace_range(self.cursor..e, "");
    }

    /// Soft-wrap layout: each visual row is (start_byte, end_byte).
    pub fn layout(&self, width: usize) -> Vec<(usize, usize)> {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in self.text.split('\n') {
            let base = line_start;
            let mut row_start = base;
            let mut w = 0;
            let mut last_space: Option<usize> = None;
            for (i, g) in line.grapheme_indices(true) {
                let gw = width_of(g);
                if w + gw > width {
                    // break at the last space if there is one on this row
                    let brk = match last_space {
                        Some(sp) if sp > row_start => sp,
                        _ => base + i,
                    };
                    rows.push((row_start, brk));
                    row_start = brk;
                    w = width_of(&self.text[row_start..base + i]);
                    last_space = None;
                }
                if g == " " {
                    last_space = Some(base + i + 1);
                }
                w += gw;
            }
            rows.push((row_start, base + line.len()));
            line_start = base + line.len() + 1;
        }
        rows
    }

    /// (row, col) of the cursor within `layout(width)`.
    pub fn cursor_pos(&self, width: usize) -> (usize, usize) {
        let rows = self.layout(width);
        for (r, (s, e)) in rows.iter().enumerate() {
            let last_row_of_line = rows.get(r + 1).is_none_or(|(ns, _)| *ns != *e);
            if self.cursor >= *s && (self.cursor < *e || (self.cursor == *e && last_row_of_line)) {
                return (r, width_of(&self.text[*s..self.cursor]));
            }
        }
        let last = rows.len().saturating_sub(1);
        (last, rows.last().map(|(s, e)| width_of(&self.text[*s..*e])).unwrap_or(0))
    }

    fn byte_at(&self, row: (usize, usize), col: usize) -> usize {
        let (s, e) = row;
        let mut w = 0;
        for (i, g) in self.text[s..e].grapheme_indices(true) {
            let gw = width_of(g);
            if w + gw > col {
                return s + i;
            }
            w += gw;
        }
        e
    }

    /// Move up a visual row. Returns false if already on the first row.
    pub fn up(&mut self, width: usize) -> bool {
        let rows = self.layout(width);
        let (r, c) = self.cursor_pos(width);
        if r == 0 {
            return false;
        }
        let goal = *self.goal_col.get_or_insert(c);
        self.cursor = self.byte_at(rows[r - 1], goal);
        true
    }

    pub fn down(&mut self, width: usize) -> bool {
        let rows = self.layout(width);
        let (r, c) = self.cursor_pos(width);
        if r + 1 >= rows.len() {
            return false;
        }
        let goal = *self.goal_col.get_or_insert(c);
        self.cursor = self.byte_at(rows[r + 1], goal);
        true
    }

    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let pos = match self.history_pos {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        self.set(&self.history[pos].clone());
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(p) = self.history_pos else { return false };
        if p + 1 >= self.history.len() {
            self.history_pos = None;
            let d = std::mem::take(&mut self.draft);
            self.set(&d);
        } else {
            self.history_pos = Some(p + 1);
            self.set(&self.history[p + 1].clone());
        }
        true
    }

    pub fn in_history(&self) -> bool {
        self.history_pos.is_some()
    }

    /// The `@token` or `/token` being typed at the cursor, with its byte start.
    pub fn current_token(&self) -> Option<(usize, &str)> {
        let before = &self.text[..self.cursor];
        let start = before.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
        let tok = &self.text[start..self.cursor];
        if tok.is_empty() { None } else { Some((start, tok)) }
    }

    pub fn replace_range(&mut self, start: usize, end: usize, with: &str) {
        self.text.replace_range(start..end, with);
        self.cursor = start + with.len();
        self.goal_col = None;
    }
}

fn width_of(s: &str) -> usize {
    width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_wraps() {
        let mut i = Input::default();
        i.set("hello world foo");
        let rows = i.layout(8);
        let parts: Vec<&str> = rows.iter().map(|(s, e)| &i.text[*s..*e]).collect();
        assert_eq!(parts, vec!["hello ", "world ", "foo"]);
        assert_eq!(i.cursor_pos(8), (2, 3));
    }

    #[test]
    fn newline_rows() {
        let mut i = Input::default();
        i.set("a\nbc");
        assert_eq!(i.layout(10).len(), 2);
        assert_eq!(i.cursor_pos(10), (1, 2));
        assert!(i.up(10));
        assert_eq!(i.cursor, 1);
    }
}

//! The multiline composer.

use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Default)]
pub struct Composer {
    text: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Position while browsing history; `None` = editing a fresh draft.
    hist_pos: Option<usize>,
    draft: String,
    history_file: Option<PathBuf>,
}

const HISTORY_MAX: usize = 500;

impl Composer {
    pub fn with_history(path: &Path) -> Self {
        let history = std::fs::read_to_string(path)
            .map(|t| t.lines().filter(|l| !l.is_empty()).map(|l| l.replace("\\n", "\n").replace("\\\\", "\\")).collect())
            .unwrap_or_default();
        Self { history, history_file: Some(path.to_path_buf()), ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.hist_pos = None;
    }

    /// Take the text for sending, recording it in history.
    pub fn take(&mut self) -> String {
        let s = self.text();
        if !s.trim().is_empty() && self.history.last() != Some(&s) {
            self.history.push(s.clone());
            if self.history.len() > HISTORY_MAX {
                self.history.remove(0);
            }
            self.persist();
        }
        self.clear();
        s
    }

    fn persist(&self) {
        let Some(p) = &self.history_file else { return };
        let body: Vec<String> = self.history.iter().map(|h| h.replace('\\', "\\\\").replace('\n', "\\n")).collect();
        let _ = std::fs::write(p, body.join("\n") + "\n");
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += 1;
        self.hist_pos = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| *c != '\r') {
            self.insert(if c == '\t' { ' ' } else { c });
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }

    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
    }
    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..].iter().position(|&c| c == '\n').map_or(self.text.len(), |i| pos + i)
    }
    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }
    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }
    pub fn on_first_line(&self) -> bool {
        !self.text[..self.cursor].contains(&'\n')
    }
    pub fn on_last_line(&self) -> bool {
        !self.text[self.cursor..].contains(&'\n')
    }

    /// Move up a line; returns false when already on the first line.
    pub fn up(&mut self) -> bool {
        if self.on_first_line() {
            return false;
        }
        let start = self.line_start(self.cursor);
        let col = self.cursor - start;
        let prev_start = self.line_start(start - 1);
        self.cursor = (prev_start + col).min(start - 1);
        true
    }
    pub fn down(&mut self) -> bool {
        if self.on_last_line() {
            return false;
        }
        let start = self.line_start(self.cursor);
        let col = self.cursor - start;
        let next_start = self.line_end(self.cursor) + 1;
        self.cursor = (next_start + col).min(self.line_end(next_start));
        true
    }

    pub fn word_left(&mut self) {
        while self.cursor > 0 && self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }
    pub fn word_right(&mut self) {
        let n = self.text.len();
        while self.cursor < n && self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < n && !self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }
    pub fn delete_word_back(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.drain(self.cursor..end);
    }
    pub fn kill_to_start(&mut self) {
        let start = self.line_start(self.cursor);
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }
    pub fn kill_to_end(&mut self) {
        let end = self.line_end(self.cursor);
        self.text.drain(self.cursor..end);
    }

    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let pos = match self.hist_pos {
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(p) => p - 1,
        };
        self.hist_pos = Some(pos);
        let h = self.history[pos].clone();
        self.set(&h);
        self.hist_pos = Some(pos);
        true
    }
    pub fn history_next(&mut self) -> bool {
        let Some(pos) = self.hist_pos else { return false };
        if pos + 1 >= self.history.len() {
            let d = std::mem::take(&mut self.draft);
            self.set(&d);
            self.hist_pos = None;
        } else {
            let h = self.history[pos + 1].clone();
            self.set(&h);
            self.hist_pos = Some(pos + 1);
        }
        true
    }

    /// The token under the cursor if it starts with `/` (at line start) or `@`.
    pub fn completion_token(&self) -> Option<(usize, String)> {
        let mut start = self.cursor;
        while start > 0 && !self.text[start - 1].is_whitespace() {
            start -= 1;
        }
        let tok: String = self.text[start..self.cursor].iter().collect();
        if tok.starts_with('@') || (tok.starts_with('/') && start == 0) { Some((start, tok)) } else { None }
    }

    /// Replace the token starting at `start` (up to the cursor) with `with`.
    pub fn replace_token(&mut self, start: usize, with: &str) {
        self.text.drain(start..self.cursor);
        self.cursor = start;
        for c in with.chars() {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    /// Visual layout for rendering: wrapped rows and the cursor (row, col).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut rows = vec![String::new()];
        let mut col = 0;
        let mut cur = (0, 0);
        for (i, &c) in self.text.iter().enumerate() {
            if i == self.cursor {
                cur = (rows.len() - 1, col);
            }
            if c == '\n' {
                rows.push(String::new());
                col = 0;
                continue;
            }
            let w = c.width().unwrap_or(0);
            if col + w > width {
                rows.push(String::new());
                col = 0;
            }
            rows.last_mut().unwrap().push(c);
            col += w;
        }
        if self.cursor == self.text.len() {
            if col >= width {
                rows.push(String::new());
                col = 0;
            }
            cur = (rows.len() - 1, col);
        }
        (rows, cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_and_cursor_movement() {
        let mut c = Composer::default();
        c.insert_str("hello world");
        c.word_left();
        c.insert_str("big ");
        assert_eq!(c.text(), "hello big world");
        c.home();
        c.delete();
        assert_eq!(c.text(), "ello big world");
        c.end();
        c.delete_word_back();
        assert_eq!(c.text(), "ello big ");
        c.kill_to_start();
        assert!(c.is_empty());
    }

    #[test]
    fn multiline_navigation() {
        let mut c = Composer::default();
        c.insert_str("abc\nde\nfghij");
        assert!(c.on_last_line());
        assert!(c.up());
        assert_eq!(c.cursor(), 6); // end of "de" (col clamped)
        assert!(c.up());
        assert_eq!(c.cursor(), 2);
        assert!(!c.up());
        assert!(c.down());
        assert!(c.down());
        assert!(!c.down());
    }

    #[test]
    fn history_roundtrip_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h");
        let mut c = Composer::with_history(&file);
        c.insert_str("first");
        c.take();
        c.insert_str("second\nline");
        c.take();
        c.insert_str("draft");
        assert!(c.history_prev());
        assert_eq!(c.text(), "second\nline");
        assert!(c.history_prev());
        assert_eq!(c.text(), "first");
        assert!(!c.history_prev());
        assert!(c.history_next());
        assert!(c.history_next());
        assert_eq!(c.text(), "draft");
        let c2 = Composer::with_history(&file);
        assert_eq!(c2.history, vec!["first".to_string(), "second\nline".to_string()]);
    }

    #[test]
    fn completion_tokens() {
        let mut c = Composer::default();
        c.insert_str("/the");
        assert_eq!(c.completion_token(), Some((0, "/the".into())));
        c.replace_token(0, "/theme ");
        assert_eq!(c.text(), "/theme ");
        c.clear();
        c.insert_str("look at @src/ma");
        assert_eq!(c.completion_token(), Some((8, "@src/ma".into())));
        c.clear();
        c.insert_str("a /path");
        assert_eq!(c.completion_token(), None, "slash only completes at line start");
    }

    #[test]
    fn layout_wraps_and_tracks_cursor() {
        let mut c = Composer::default();
        c.insert_str("abcdef\ngh");
        let (rows, cur) = c.layout(4);
        assert_eq!(rows, vec!["abcd", "ef", "gh"]);
        assert_eq!(cur, (2, 2));
        c.home();
        assert_eq!(c.layout(4).1, (2, 0));
    }
}

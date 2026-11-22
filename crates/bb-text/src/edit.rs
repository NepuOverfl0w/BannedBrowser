//! Text editing model shared by the browser chrome (URL bar, find bar) and web page form
//! controls (`<input>`, `<textarea>`): editing, selection, undo, IME preedit, and for
//! multi-line fields line-wise caret movement.

#[derive(Debug, Clone, Default)]
pub struct TextField {
    pub text: String,
    /// Caret byte offset.
    pub caret: usize,
    /// Selection anchor (byte offset); selection is anchor..caret.
    pub anchor: Option<usize>,
    /// IME composition text shown at the caret.
    pub preedit: String,
    /// Keep line breaks (`<textarea>`); single-line fields drop them.
    pub multiline: bool,
    /// The most characters the text may hold (`maxlength`); typing beyond it is ignored.
    pub max_chars: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().last().map(|(j, _)| j).unwrap_or(0)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
}

impl TextField {
    pub fn new(text: &str) -> Self {
        TextField { text: text.to_string(), caret: text.len(), ..Default::default() }
    }

    /// A multi-line field (line breaks are kept and normalized to `\n`).
    pub fn new_multiline(text: &str) -> Self {
        let text = normalize_newlines(text);
        TextField { caret: text.len(), text, multiline: true, ..Default::default() }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.len();
        self.anchor = None;
        self.preedit.clear();
    }

    /// `i` clamped to the text and moved back to a character boundary.
    fn boundary(&self, i: usize) -> usize {
        let mut i = i.min(self.text.len());
        while !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.boundary(self.anchor?);
        let caret = self.boundary(self.caret);
        if a == caret {
            None
        } else {
            Some((a.min(caret), a.max(caret)))
        }
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    fn snapshot(&mut self) {
        self.undo.push((self.text.clone(), self.caret));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.text.replace_range(a..b, "");
            self.caret = a;
            self.anchor = None;
            true
        } else {
            false
        }
    }

    pub fn insert(&mut self, s: &str) {
        let s = normalize_newlines(s);
        let mut s: String = s.chars().filter(|&c| !c.is_control() || (self.multiline && c == '\n')).collect();
        if s.is_empty() {
            return;
        }
        if let Some(max) = self.max_chars {
            let selected = self.selection().map_or(0, |(a, b)| self.text[a..b].chars().count());
            let room = (max + selected).saturating_sub(self.text.chars().count());
            if let Some((cut, _)) = s.char_indices().nth(room) {
                s.truncate(cut);
            }
            if s.is_empty() {
                return;
            }
        }
        self.snapshot();
        self.delete_selection();
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
        self.anchor = None;
    }

    pub fn backspace(&mut self) {
        self.snapshot();
        if self.delete_selection() {
            return;
        }
        if self.caret > 0 {
            let p = prev_boundary(&self.text, self.caret);
            self.text.replace_range(p..self.caret, "");
            self.caret = p;
        }
    }

    pub fn delete(&mut self) {
        self.snapshot();
        if self.delete_selection() {
            return;
        }
        if self.caret < self.text.len() {
            let n = next_boundary(&self.text, self.caret);
            self.text.replace_range(self.caret..n, "");
        }
    }

    fn word_left(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 && self.text[..i].ends_with(|c: char| !c.is_alphanumeric()) {
            i = prev_boundary(&self.text, i);
        }
        while i > 0 && self.text[..i].ends_with(|c: char| c.is_alphanumeric()) {
            i = prev_boundary(&self.text, i);
        }
        i
    }

    fn word_right(&self, from: usize) -> usize {
        let mut i = from;
        while i < self.text.len() && self.text[i..].starts_with(|c: char| !c.is_alphanumeric()) {
            i = next_boundary(&self.text, i);
        }
        while i < self.text.len() && self.text[i..].starts_with(|c: char| c.is_alphanumeric()) {
            i = next_boundary(&self.text, i);
        }
        i
    }

    /// Move the caret; `extend` keeps/extends the selection.
    pub fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = self.boundary(to);
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        if !extend {
            if let Some((a, _)) = self.selection() {
                self.move_caret(a, false);
                return;
            }
        }
        let to = if word { self.word_left(self.caret) } else { prev_boundary(&self.text, self.caret) };
        self.move_caret(to, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if !extend {
            if let Some((_, b)) = self.selection() {
                self.move_caret(b, false);
                return;
            }
        }
        let to = if word { self.word_right(self.caret) } else { next_boundary(&self.text, self.caret) };
        self.move_caret(to, extend);
    }

    /// Start of the field, or of the caret's line in a multi-line field.
    pub fn home(&mut self, extend: bool) {
        let to = if self.multiline { self.line_start(self.caret) } else { 0 };
        self.move_caret(to, extend);
    }

    /// End of the field, or of the caret's line in a multi-line field.
    pub fn end(&mut self, extend: bool) {
        let to = if self.multiline { self.line_end(self.caret) } else { self.text.len() };
        self.move_caret(to, extend);
    }

    /// Start or end of the whole text (Ctrl+Home / Ctrl+End).
    pub fn text_start(&mut self, extend: bool) {
        self.move_caret(0, extend);
    }

    pub fn text_end(&mut self, extend: bool) {
        self.move_caret(self.text.len(), extend);
    }

    fn line_start(&self, i: usize) -> usize {
        self.text[..i].rfind('\n').map_or(0, |p| p + 1)
    }

    fn line_end(&self, i: usize) -> usize {
        self.text[i..].find('\n').map_or(self.text.len(), |p| i + p)
    }

    /// Byte offset `col` characters into the line starting at `start` (clamped to its end).
    fn at_column(&self, start: usize, col: usize) -> usize {
        let end = self.line_end(start);
        self.text[start..end].char_indices().nth(col).map_or(end, |(j, _)| start + j)
    }

    /// Up one line, keeping the character column (multi-line fields; single-line: start).
    pub fn up(&mut self, extend: bool) {
        if !self.multiline {
            return self.move_caret(0, extend);
        }
        let start = self.line_start(self.caret);
        if start == 0 {
            return self.move_caret(0, extend);
        }
        let col = self.text[start..self.caret].chars().count();
        let prev = self.line_start(start - 1);
        let to = self.at_column(prev, col);
        self.move_caret(to, extend);
    }

    /// Down one line, keeping the character column (single-line: end).
    pub fn down(&mut self, extend: bool) {
        if !self.multiline {
            return self.move_caret(self.text.len(), extend);
        }
        let start = self.line_start(self.caret);
        let end = self.line_end(self.caret);
        if end == self.text.len() {
            return self.move_caret(end, extend);
        }
        let col = self.text[start..self.caret].chars().count();
        let to = self.at_column(end + 1, col);
        self.move_caret(to, extend);
    }

    /// The caret's (line, byte offset within that line), for drawing multi-line fields.
    pub fn caret_line(&self) -> (usize, usize) {
        let start = self.line_start(self.caret);
        (self.text[..start].matches('\n').count(), self.caret - start)
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo.push((self.text.clone(), self.caret));
            self.text = t;
            self.caret = self.boundary(c);
            self.anchor = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo.push((self.text.clone(), self.caret));
            self.text = t;
            self.caret = self.boundary(c);
            self.anchor = None;
        }
    }

    /// Select the word around byte offset `at` (double-click).
    pub fn select_word_at(&mut self, at: usize) {
        let (a, b) = crate::word_at(&self.text, at.min(self.text.len()));
        self.anchor = Some(a);
        self.caret = b;
    }
}

/// CR LF and lone CR become LF (HTML's newline normalization).
pub fn normalize_newlines(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut t = TextField::new("hello world");
        t.left(true, false);
        assert_eq!(t.caret, 6);
        t.insert("big ");
        assert_eq!(t.text, "hello big world");
        t.select_all();
        t.insert("x");
        assert_eq!(t.text, "x");
        t.undo();
        assert_eq!(t.text, "hello big world");
        t.redo();
        assert_eq!(t.text, "x");
        t.set_text("añb");
        t.left(false, false);
        t.backspace();
        assert_eq!(t.text, "ab");
        t.home(false);
        t.right(false, true);
        assert_eq!(t.selected_text(), Some("a"));
        t.delete();
        assert_eq!(t.text, "b");
    }

    #[test]
    fn multiline_and_maxlength() {
        let mut t = TextField::new_multiline("ab\r\ncdef\rg");
        assert_eq!(t.text, "ab\ncdef\ng");
        assert_eq!(t.caret_line(), (2, 1));
        t.up(false);
        assert_eq!(t.caret, 4); // "c|def": column 1 on line 1
        t.up(false);
        assert_eq!(t.caret, 1);
        t.end(false);
        assert_eq!(t.caret, 2);
        t.down(false);
        t.down(false);
        assert_eq!(t.caret, t.text.len()); // line 2 is "g": clamped
        t.insert("\nh");
        assert_eq!(t.text, "ab\ncdef\ng\nh");
        t.home(false);
        assert_eq!(t.caret_line(), (3, 0));
        t.text_start(true);
        assert_eq!(t.selection(), Some((0, t.text.len() - 1)));
        // Single-line fields drop line breaks.
        let mut s = TextField::new("");
        s.insert("a\r\nb\nc");
        assert_eq!(s.text, "abc");
        // maxlength counts characters; a selection being replaced makes room.
        let mut m = TextField::new("");
        m.max_chars = Some(3);
        m.insert("héllo");
        assert_eq!(m.text, "hél");
        m.insert("x");
        assert_eq!(m.text, "hél");
        m.select_all();
        m.insert("wxyz");
        assert_eq!(m.text, "wxy");
    }
}

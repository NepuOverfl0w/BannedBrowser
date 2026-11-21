//! Single-line text field model: editing, selection, undo, IME preedit.

#[derive(Debug, Clone, Default)]
pub struct TextField {
    pub text: String,
    /// Caret byte offset.
    pub caret: usize,
    /// Selection anchor (byte offset); selection is anchor..caret.
    pub anchor: Option<usize>,
    /// IME composition text shown at the caret.
    pub preedit: String,
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

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.len();
        self.anchor = None;
        self.preedit.clear();
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.caret {
            None
        } else {
            Some((a.min(self.caret), a.max(self.caret)))
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
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() {
            return;
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
        self.caret = to.min(self.text.len());
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

    pub fn home(&mut self, extend: bool) {
        self.move_caret(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_caret(self.text.len(), extend);
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo.push((self.text.clone(), self.caret));
            self.text = t;
            self.caret = c.min(self.text.len());
            self.anchor = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo.push((self.text.clone(), self.caret));
            self.text = t;
            self.caret = c.min(self.text.len());
            self.anchor = None;
        }
    }

    /// Select the word around byte offset `at` (double-click).
    pub fn select_word_at(&mut self, at: usize) {
        let (a, b) = bb_text::word_at(&self.text, at.min(self.text.len()));
        self.anchor = Some(a);
        self.caret = b;
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
}

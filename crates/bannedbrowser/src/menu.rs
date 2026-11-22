//! Popup menus: the ≡ app menu and the context menus (page, link, image,
//! tab, URL bar). Menus are drawn inside the window with `bb-ui`, like the
//! rest of the chrome, and driven by mouse and keyboard.

use bb_paint::{Item, Rect};
use bb_ui::{Painter, Theme};
use bb_url::Url;

/// What a menu item does when chosen.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    NewTab,
    NewWindow,
    NewPrivateWindow,
    FindInPage,
    About,
    Quit,
    Back,
    Forward,
    Reload,
    OpenInNewTab(Url),
    OpenInNewWindow(Url),
    OpenInPrivateWindow(Url),
    CopyText(String),
    TabReload(usize),
    TabDuplicate(usize),
    TabClose(usize),
    TabCloseOthers(usize),
    TabReopen,
    UrlUndo,
    UrlCut,
    UrlCopy,
    UrlPaste,
    UrlPasteAndGo,
    UrlDelete,
    UrlSelectAll,
    /// Pick option `index` (in the list of options) of the open `<select>` drop-down.
    ChooseOption(usize),
}

// Menus hold about ten short-lived entries; boxing the action buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Item { label: String, shortcut: Option<String>, action: Action, enabled: bool },
    Separator,
}

impl Entry {
    pub fn item(label: &str, shortcut: Option<&str>, action: Action, enabled: bool) -> Entry {
        Entry::Item { label: label.to_string(), shortcut: shortcut.map(str::to_string), action, enabled }
    }

    fn height(&self) -> f32 {
        match self {
            Entry::Item { .. } => ITEM_H,
            Entry::Separator => SEP_H,
        }
    }
}

const ITEM_H: f32 = 30.0;
const SEP_H: f32 = 9.0;
const PAD: f32 = 6.0;
const TEXT: f32 = 13.0;

/// An open popup menu.
#[derive(Debug, Clone)]
pub struct Menu {
    /// "app", "page", "link", "image", "tab", "urlbar" (test hook + debugging).
    pub kind: &'static str,
    pub entries: Vec<Entry>,
    /// Popup rect in logical window px.
    pub rect: Rect,
    /// Highlighted entry (always an enabled item).
    pub hover: Option<usize>,
}

/// Platform label for the primary modifier in shortcut hints.
pub fn primary_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

impl Menu {
    /// Lay out a menu at `at`, kept inside `window` (flips up/left when needed).
    pub fn new(kind: &'static str, entries: Vec<Entry>, at: (f32, f32), window: (f32, f32)) -> Menu {
        let mut w: f32 = 220.0;
        for e in &entries {
            if let Entry::Item { label, shortcut, .. } = e {
                let lw = bb_ui::text::measure(label, TEXT, 400);
                let sw = shortcut.as_deref().map_or(0.0, |s| bb_ui::text::measure(s, TEXT, 400) + 32.0);
                w = w.max(lw + sw + 2.0 * PAD + 32.0);
            }
        }
        let h: f32 = entries.iter().map(Entry::height).sum::<f32>() + 2.0 * PAD;
        let mut x = at.0;
        let mut y = at.1;
        if x + w > window.0 - 4.0 {
            x = (window.0 - w - 4.0).max(4.0);
        }
        if y + h > window.1 - 4.0 {
            y = (at.1 - h).max(4.0);
        }
        Menu { kind, entries, rect: Rect::new(x, y, w, h), hover: None }
    }

    fn entry_rects(&self) -> Vec<Rect> {
        let mut y = self.rect.y + PAD;
        self.entries
            .iter()
            .map(|e| {
                let r = Rect::new(self.rect.x + PAD, y, self.rect.w - 2.0 * PAD, e.height());
                y += e.height();
                r
            })
            .collect()
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    /// The enabled item under a point.
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        self.entry_rects().iter().position(|r| r.contains(x, y)).filter(|&i| self.is_enabled(i))
    }

    fn is_enabled(&self, i: usize) -> bool {
        matches!(self.entries.get(i), Some(Entry::Item { enabled: true, .. }))
    }

    /// Move the highlight to the next/previous enabled item (wrapping).
    pub fn step(&mut self, forward: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut i = match (self.hover, forward) {
            (Some(h), true) => (h + 1) % n,
            (Some(h), false) => (h + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        for _ in 0..n {
            if self.is_enabled(i) {
                self.hover = Some(i);
                return;
            }
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
        }
    }

    /// The action of entry `i`, if it is an enabled item.
    pub fn action(&self, i: usize) -> Option<Action> {
        match self.entries.get(i) {
            Some(Entry::Item { action, enabled: true, .. }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Labels of all items (separators as "-"), for the GUI test state.
    pub fn labels(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| match e {
                Entry::Item { label, .. } => label.clone(),
                Entry::Separator => "-".to_string(),
            })
            .collect()
    }

    pub fn paint(&self, p: &mut Painter, th: &Theme) {
        p.shadow(self.rect, 8.0, 12.0, th.shadow.with_alpha(0.35));
        p.rounded(self.rect, 8.0, th.popup);
        p.dl.push(Item::Border {
            rect: self.rect,
            radii: bb_paint::Radii::uniform(8.0),
            sides: [Some(bb_paint::BorderSide {
                width: 1.0,
                color: th.separator,
                style: bb_paint::BorderStyle::Solid,
            }); 4],
        });
        for (i, (e, r)) in self.entries.iter().zip(self.entry_rects()).enumerate() {
            match e {
                Entry::Separator => {
                    p.rect(Rect::new(r.x + 4.0, r.y + r.h / 2.0, r.w - 8.0, 1.0), th.separator);
                }
                Entry::Item { label, shortcut, enabled, .. } => {
                    if self.hover == Some(i) {
                        p.rounded(r, 4.0, th.button_hover);
                    }
                    let c = if *enabled { th.text } else { th.text.with_alpha(0.4) };
                    let base = bb_ui::text::centered_baseline(r.y, r.h, TEXT);
                    p.text(label, r.x + 12.0, base, TEXT, 400, c);
                    if let Some(s) = shortcut {
                        let sw = bb_ui::text::measure(s, TEXT, 400);
                        let sc = if *enabled { th.text_muted } else { th.text_muted.with_alpha(0.4) };
                        p.text(s, r.right() - 12.0 - sw, base, TEXT, 400, sc);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Menu {
        Menu::new(
            "test",
            vec![
                Entry::item("A", None, Action::NewTab, true),
                Entry::Separator,
                Entry::item("B", None, Action::Back, false),
                Entry::item("C", Some("Ctrl+Q"), Action::Quit, true),
            ],
            (10.0, 10.0),
            (800.0, 600.0),
        )
    }

    #[test]
    fn keyboard_skips_separators_and_disabled_items() {
        let mut m = sample();
        m.step(true);
        assert_eq!(m.hover, Some(0));
        m.step(true);
        assert_eq!(m.hover, Some(3), "skips the separator and disabled B");
        m.step(true);
        assert_eq!(m.hover, Some(0), "wraps");
        m.step(false);
        assert_eq!(m.hover, Some(3));
        assert_eq!(m.action(3), Some(Action::Quit));
        assert_eq!(m.action(2), None, "disabled");
        assert_eq!(m.labels(), ["A", "-", "B", "C"]);
    }

    #[test]
    fn stays_inside_the_window_and_hit_tests() {
        let m = Menu::new(
            "t",
            vec![Entry::item("Only", None, Action::NewTab, true)],
            (790.0, 590.0),
            (800.0, 600.0),
        );
        assert!(m.rect.right() <= 796.0 && m.rect.bottom() <= 596.0, "{:?}", m.rect);
        let s = sample();
        let (x, y) = (s.rect.x + 20.0, s.rect.y + PAD + 5.0);
        assert_eq!(s.item_at(x, y), Some(0));
        assert_eq!(s.item_at(x, y + ITEM_H + SEP_H), None, "disabled B is not hit");
        assert!(!s.contains(0.0, 0.0));
    }
}

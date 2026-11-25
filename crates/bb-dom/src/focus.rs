//! Focus (HTML §6.6): the document's focused element, which elements are focusable areas,
//! and shadow hosts' focus delegates. The focused element and the shadow hosts it is inside
//! carry [`NodeFlags::FOCUS`] for CSS `:focus` (CSS Scoping §3.2.3: a host matches while
//! focus is in its shadow tree).

use crate::{Document, InputType, NodeFlags, NodeId};

fn parse_tabindex(s: &str) -> Option<i64> {
    let t = s.trim_matches([' ', '\t', '\n', '\x0C', '\r']);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    let n: i64 = digits[..end].parse().ok()?;
    Some(if neg { -n } else { n })
}

impl Document {
    /// The focused element (None: the viewport).
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Make `n` the focused element (or none), moving the `:focus` flags.
    pub fn set_focused(&mut self, n: Option<NodeId>) {
        if self.focused == n {
            return;
        }
        if let Some(old) = self.focused {
            for x in self.focus_chain(old) {
                self.set_flag(x, NodeFlags::FOCUS, false);
            }
        }
        self.focused = n;
        if let Some(new) = n {
            for x in self.focus_chain(new) {
                self.set_flag(x, NodeFlags::FOCUS, true);
            }
        }
    }

    /// `n` and the shadow hosts it is inside (innermost first).
    fn focus_chain(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = vec![n];
        let mut cur = n;
        while let Some(host) = self.containing_shadow_root(cur).and_then(|r| self.shadow_host(r)) {
            out.push(host);
            cur = host;
        }
        out
    }

    /// The element's `tabindex` value, if it has a valid one (§2.3.4.1 integers).
    pub fn tabindex(&self, n: NodeId) -> Option<i64> {
        self.attr(n, "tabindex").and_then(parse_tabindex)
    }

    /// §6.6.2 whether element `n` is a focusable area: an element with a tabindex value, a link,
    /// an enabled form control (not hidden), an iframe, a details' first summary, an editing
    /// host. (Whether it is being rendered isn't known here.)
    pub fn is_focusable(&self, n: NodeId) -> bool {
        let Some(e) = self.element(n) else { return false };
        if self.is_disabled(n) {
            return false;
        }
        if self.tabindex(n).is_some() {
            return true;
        }
        if e.ns != crate::Namespace::Html {
            return false;
        }
        match e.name.as_str() {
            "a" | "area" => e.has_attr("href"),
            "button" | "select" | "textarea" | "iframe" => true,
            "input" => self.input_type(n) != InputType::Hidden,
            "summary" => self.parent(n).is_some_and(|p| {
                self.is_html(p, "details")
                    && self.element_children(p).find(|&c| self.is_html(c, "summary")) == Some(n)
            }),
            _ => e.attr("contenteditable").is_some_and(|v| {
                v.is_empty() || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("plaintext-only")
            }),
        }
    }

    /// §6.6.3 the focus delegate of shadow host `host` (with `delegatesFocus`): its
    /// shadow tree's first focusable area, in tree order (an element with `autofocus` first);
    /// a nested delegating host delegates further.
    pub fn focus_delegate(&self, host: NodeId) -> Option<NodeId> {
        let root = self.shadow_root(host)?;
        if !self.shadow_info(root).is_some_and(|i| i.delegates_focus) {
            return None;
        }
        let candidates: Vec<NodeId> =
            self.descendants(root).skip(1).filter(|&x| self.element(x).is_some()).collect();
        let pick = |x: NodeId| -> Option<NodeId> {
            if self.is_focusable(x) {
                return Some(x);
            }
            self.focus_delegate(x)
        };
        candidates
            .iter()
            .filter(|&&x| self.attr(x, "autofocus").is_some())
            .find_map(|&x| pick(x))
            .or_else(|| candidates.iter().find_map(|&x| pick(x)))
    }

    /// The element focusing steps (§6.6.4) would focus for `n`: itself if focusable, else
    /// its focus delegate; None if neither (or if focus is already inside a delegating host).
    pub fn focus_target(&self, n: NodeId) -> Option<NodeId> {
        if let Some(root) = self.shadow_root(n) {
            if self.shadow_info(root).is_some_and(|i| i.delegates_focus) {
                // Focus already inside: nothing changes.
                if self.focused.is_some_and(|f| f != n && self.is_shadow_including_inclusive_ancestor(n, f)) {
                    return None;
                }
                return self.focus_delegate(n);
            }
        }
        self.is_focusable(n).then_some(n)
    }
}

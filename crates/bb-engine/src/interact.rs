//! Focus, keyboard and mouse interaction with page content (HTML §6.6 focus, §4.10 forms):
//! focusing and editing text controls, checkboxes and radio buttons, `<select>` drop-downs,
//! buttons, form submission and reset, `<label>` activation, links and `<summary>` from the
//! keyboard, and sequential focus navigation (Tab / Shift+Tab), across frames.
//!
//! Focus lives in the innermost document: each `Page` has a focused element or a focused child
//! frame (`FocusState::frame`), so key events walk down that chain.

use crate::forms::{self, FormSubmission};
use crate::{LinkHit, Page};
use bb_dom::{Document, InputType, NodeId};
use bb_paint::Rect;
use bb_text::edit::TextField;

/// What kind of element has focus; the browser uses it to decide which keys go to the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusKind {
    /// A single-line text control: every editing key goes to the page.
    Text,
    /// A `<textarea>`: editing keys, including Enter and Up/Down.
    TextArea,
    Select,
    Checkbox,
    Radio,
    Button,
    Link,
    Summary,
}

impl FocusKind {
    pub fn name(self) -> &'static str {
        match self {
            FocusKind::Text => "text",
            FocusKind::TextArea => "textarea",
            FocusKind::Select => "select",
            FocusKind::Checkbox => "checkbox",
            FocusKind::Radio => "radio",
            FocusKind::Button => "button",
            FocusKind::Link => "link",
            FocusKind::Summary => "summary",
        }
    }
}

/// Keys the page handles (the browser maps platform keys and shortcuts to these).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKey {
    Tab,
    Enter,
    Space,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// Alt+Down: open a drop-down.
    Open,
    SelectAll,
    Undo,
    Redo,
    Copy,
    Cut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub shift: bool,
    /// Word-wise caret movement (Ctrl, or Alt/Option on macOS).
    pub word: bool,
    /// Ctrl/Cmd+Home/End: to the start/end of the whole text.
    pub primary: bool,
}

/// One row of a `<select>` drop-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupItem {
    pub label: String,
    /// An `<optgroup>` label (not selectable).
    pub group: bool,
    pub enabled: bool,
    pub selected: bool,
    /// Index into the select's list of options (`None` for group labels).
    pub option: Option<usize>,
}

/// A drop-down the browser should show for a `<select>`.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectPopup {
    /// Path of child frame indices down to the select's document.
    pub frame_path: Vec<usize>,
    pub select: NodeId,
    /// The select's border box in top-level page coordinates (CSS px).
    pub rect: Rect,
    pub items: Vec<PopupItem>,
    pub multiple: bool,
}

/// The result of an interaction.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// The page changed and needs a new render.
    pub repaint: bool,
    /// The page used the key (otherwise the browser applies its default, e.g. scrolling).
    pub consumed: bool,
    /// A form was submitted: the request and the frame path of the form's document.
    pub submit: Option<(FormSubmission, Vec<usize>)>,
    /// Enter on a focused link.
    pub follow: Option<LinkHit>,
    pub popup: Option<SelectPopup>,
    /// Text to put on the clipboard (Copy / Cut).
    pub copied: Option<String>,
    /// Tab moved past the last (true) or first (false) focusable element: focus leaves the page.
    pub focus_exit: Option<bool>,
}

impl Outcome {
    fn used() -> Self {
        Outcome { consumed: true, ..Default::default() }
    }

    fn changed() -> Self {
        Outcome { consumed: true, repaint: true, ..Default::default() }
    }
}

/// Per-document focus state.
#[derive(Clone, Debug, Default)]
pub struct FocusState {
    pub node: Option<NodeId>,
    /// The child frame holding focus (focus is then inside it).
    pub frame: Option<usize>,
    /// The editing model of a focused text control.
    pub(crate) editor: Option<TextField>,
    /// The browser window gives the page keyboard focus (the caret shows).
    pub(crate) window_focus: bool,
    /// The focused text control's value when it got focus (a change event fires on blur if
    /// it differs, HTML §4.10.5.5).
    pub(crate) edit_start: Option<String>,
}

/// The focus kind of an element if it is focusable (§6.6.2 "focusable area", for the elements
/// we support; no `tabindex` on other elements yet).
pub fn focus_kind(doc: &Document, n: NodeId) -> Option<FocusKind> {
    let e = doc.element(n)?;
    if e.ns != bb_dom::Namespace::Html {
        return None;
    }
    let kind = match e.name.as_str() {
        "input" => match doc.input_type(n) {
            InputType::Hidden => return None,
            InputType::Checkbox => FocusKind::Checkbox,
            InputType::Radio => FocusKind::Radio,
            InputType::Submit | InputType::Reset | InputType::Button | InputType::Image | InputType::File => {
                FocusKind::Button
            }
            _ => FocusKind::Text,
        },
        "textarea" => FocusKind::TextArea,
        "select" => FocusKind::Select,
        "button" => FocusKind::Button,
        "a" | "area" if e.has_attr("href") => return Some(FocusKind::Link),
        "summary" => {
            let parent = doc.parent(n)?;
            let first = doc.children(parent).find(|&c| doc.is_html(c, "summary"));
            if doc.is_html(parent, "details") && first == Some(n) {
                FocusKind::Summary
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if doc.is_disabled(n) {
        return None;
    }
    Some(kind)
}

/// §4.10.4 the labeled control of a `<label>`: the element its `for` names (if labelable),
/// else its first labelable descendant.
fn labeled_control(doc: &Document, label: NodeId) -> Option<NodeId> {
    doc.labeled_control(label)
}

impl Page {
    /// The focused element of the innermost focused document, its kind, and its rect in this
    /// page's coordinates.
    pub fn focused(&self) -> Option<(FocusKind, Rect)> {
        if let Some(i) = self.focus.frame {
            let f = self.frames.get(i)?;
            let (k, r) = f.page.focused()?;
            return Some((k, Rect::new(r.x + f.rect.x, r.y + f.rect.y - f.scroll_y, r.w, r.h)));
        }
        let n = self.focus.node?;
        let kind = focus_kind(&self.doc, n)?;
        let rects = bb_layout::node_rects(&self.layout, &self.doc, n);
        let r = rects.iter().copied().reduce(|a, b| a.union(&b)).unwrap_or_default();
        Some((kind, r))
    }

    /// Whether the browser window gives the page keyboard focus (hides or shows the caret).
    /// Returns whether a repaint is needed.
    pub fn set_window_focus(&mut self, on: bool) -> bool {
        let mut changed = self.focus.window_focus != on;
        self.focus.window_focus = on;
        for f in &mut self.frames {
            changed |= f.page.set_window_focus(on);
        }
        if changed {
            self.rebuild_display();
        }
        changed
    }

    /// Remove focus from everything (e.g. a click on nothing focusable).
    pub fn blur(&mut self) -> bool {
        let had = self.focus.node.is_some() || self.focus.frame.is_some();
        if let Some(i) = self.focus.frame.take() {
            if let Some(f) = self.frames.get_mut(i) {
                f.page.blur();
            }
        }
        let left = self.focus.node.take();
        self.focus.editor = None;
        if had {
            self.rebuild_display();
        }
        self.committed(left);
        had
    }

    /// A text control lost focus: `change` if its value changed since it got focus.
    fn committed(&mut self, left: Option<NodeId>) {
        let start = self.focus.edit_start.take();
        if let (Some(n), Some(start)) = (left, start) {
            if self.doc.control_value(n) != start {
                self.script_event("change", n, false);
            }
        }
    }

    /// Focus `n` in this document (no frame).
    pub fn focus_node(&mut self, n: Option<NodeId>) {
        if let Some(i) = self.focus.frame.take() {
            if let Some(f) = self.frames.get_mut(i) {
                f.page.blur();
            }
        }
        if self.focus.node != n || n.is_none() {
            self.focus.editor = n.filter(|&n| self.doc.is_text_control(n)).map(|n| {
                let value = self.doc.control_value(n);
                let mut ed = if self.doc.is_html(n, "textarea") {
                    TextField::new_multiline(&value)
                } else {
                    TextField::new(&value)
                };
                ed.max_chars = self.doc.attr(n, "maxlength").and_then(|m| m.trim().parse().ok());
                ed
            });
        }
        let left = self.focus.node.filter(|&o| Some(o) != n);
        let start = if left.is_some() { self.focus.edit_start.take() } else { None };
        if self.focus.node != n {
            self.focus.edit_start =
                n.filter(|&n| self.doc.is_text_control(n)).map(|n| self.doc.control_value(n));
        }
        self.focus.node = n;
        self.rebuild_display();
        if let (Some(o), Some(start)) = (left, start) {
            if self.doc.control_value(o) != start {
                self.script_event("change", o, false);
            }
        }
    }

    /// The edit view (caret, selection) of the focused text control, for layout to paint.
    pub(crate) fn edit_view(&self) -> Option<(NodeId, bb_layout::tree::EditView)> {
        let n = self.focus.node?;
        let ed = self.focus.editor.as_ref()?;
        Some((
            n,
            bb_layout::tree::EditView {
                text: ed.text.clone(),
                caret: ed.caret,
                selection: ed.selection(),
                caret_visible: self.focus.window_focus,
            },
        ))
    }

    /// The focus ring for the display list.
    pub(crate) fn focus_ring(&self) -> Vec<Rect> {
        if !self.focus.window_focus {
            return Vec::new();
        }
        match self.focus.node {
            Some(n) if focus_kind(&self.doc, n).is_some() => {
                bb_layout::node_rects(&self.layout, &self.doc, n)
            }
            _ => Vec::new(),
        }
    }

    /// Store the editor's text as the control's value and repaint the control.
    fn commit_editor(&mut self) {
        let (Some(n), Some(ed)) = (self.focus.node, self.focus.editor.as_ref()) else { return };
        let text = ed.text.clone();
        // The control keeps its size, so only its box is refreshed (in `rebuild_display`).
        self.doc.set_control_value(n, &text);
        // A user edit: maxlength/minlength apply now (§4.10.5.3.3).
        self.doc.mark_user_edited(n, true);
        self.rebuild_display();
    }

    // ----- mouse -----

    /// A primary click at a page point that isn't on a link: focus, activation behaviour
    /// (§6.5 / §4.10), `<summary>` toggling, label activation. `image_point` handling and form
    /// submission are reported in the outcome.
    pub fn click(&mut self, x: f32, y: f32) -> Outcome {
        if let Some((i, lx, ly)) = self.frame_at(x, y) {
            if self.focus.node.take().is_some() {
                self.focus.editor = None;
                self.rebuild_display();
            }
            if self.focus.frame.is_some_and(|f| f != i) {
                if let Some(old) = self.focus.frame.and_then(|f| self.frames.get_mut(f)) {
                    old.page.blur();
                }
            }
            self.focus.frame = Some(i);
            let f = &mut self.frames[i];
            let mut out = f.page.click(lx, ly);
            let (fx, fy, sy) = (f.rect.x, f.rect.y, f.scroll_y);
            if let Some((_, path)) = out.submit.as_mut() {
                path.insert(0, i);
            }
            if let Some(p) = out.popup.as_mut() {
                p.frame_path.insert(0, i);
                p.rect = Rect::new(p.rect.x + fx, p.rect.y + fy - sy, p.rect.w, p.rect.h);
            }
            return out;
        }
        let hit = bb_layout::hit_test(&self.layout, x, y).node;
        // The nearest interactive element: a control, a label, a summary.
        let mut target = None;
        let mut cur = hit;
        while let Some(n) = cur {
            if focus_kind(&self.doc, n).is_some_and(|k| k != FocusKind::Link) || self.doc.is_html(n, "label")
            {
                target = Some(n);
                break;
            }
            // A disabled control swallows the click.
            if self.doc.is_disabled(n) && self.doc.element(n).is_some_and(|e| e.name != "fieldset") {
                return Outcome { repaint: self.blur(), consumed: true, ..Default::default() };
            }
            cur = self.doc.parent(n);
        }
        let Some(mut el) = target else {
            return Outcome { repaint: self.blur(), ..Default::default() };
        };
        let mut from_label = false;
        if self.doc.is_html(el, "label") {
            match labeled_control(&self.doc, el) {
                Some(c) if focus_kind(&self.doc, c).is_some() => {
                    el = c;
                    from_label = true;
                }
                _ => return Outcome { repaint: self.blur(), ..Default::default() },
            }
        }
        let kind = focus_kind(&self.doc, el);
        let was = self.focus.node;
        if was != Some(el) {
            self.focus_node(Some(el));
        }
        let mut out = Outcome::changed();
        match kind {
            Some(FocusKind::Text) | Some(FocusKind::TextArea) => {
                if !from_label {
                    if let Some(off) = bb_layout::paint::text_control_offset_at(&self.layout, el, x, y) {
                        if let Some(ed) = self.focus.editor.as_mut() {
                            ed.move_caret(off, false);
                        }
                    }
                }
                self.rebuild_display();
            }
            Some(FocusKind::Checkbox) => {
                let c = self.doc.checkedness(el);
                self.doc.set_checkedness(el, !c);
                self.relayout_styles_ready();
                self.script_changed(el);
            }
            Some(FocusKind::Radio) => {
                if !self.doc.checkedness(el) {
                    self.doc.set_checkedness(el, true);
                    self.relayout_styles_ready();
                    self.script_changed(el);
                }
            }
            Some(FocusKind::Select) => out.popup = self.select_popup(el),
            Some(FocusKind::Button) => {
                // The click point inside an image button, for its x/y entries.
                let point = if self.doc.input_type(el) == InputType::Image {
                    self.layout.node_box.get(&el).map(|&b| {
                        let (cx, cy, _, _) = self.layout.boxes[b].content_rect();
                        ((x - cx).max(0.0) as i32, (y - cy).max(0.0) as i32)
                    })
                } else {
                    None
                };
                out.submit = self.activate_button(el, point).map(|s| (s, Vec::new()));
            }
            Some(FocusKind::Summary) => {
                self.toggle_details(el);
            }
            _ => {}
        }
        out
    }

    /// The details element of a summary toggles `open`.
    fn toggle_details(&mut self, summary: NodeId) {
        if let Some(details) = self.details_for_summary(summary) {
            let open = self.doc.attr(details, "open").is_some();
            if self.script.is_some() {
                // Through the DOM, so scripts get the toggle event and mutation records.
                self.with_window(|w| w.set_attribute(details, "open", if open { None } else { Some("") }));
            } else if open {
                self.doc.remove_attr(details, "open");
            } else {
                self.doc.set_attr(details, "open", "");
            }
            // `details[open]` selectors and the hidden contents change.
            self.restyle();
        }
    }

    /// A button's activation behaviour: submit or reset its form owner.
    fn activate_button(&mut self, el: NodeId, point: Option<(i32, i32)>) -> Option<FormSubmission> {
        let form = self.doc.form_owner(el)?;
        if forms::is_submit_button(&self.doc, el) {
            return self.submit_form(form, Some(el), point);
        }
        if forms::is_reset_button(&self.doc, el) {
            // HTML §4.10.21.5 "reset": a cancelable reset event first.
            if !self.script_event("reset", form, true) {
                return None;
            }
            if self.script.is_some() {
                self.with_window(|w| w.reset_form(form));
            } else {
                self.doc.reset_form(form);
            }
            if let Some(ed) = self.focus.editor.as_mut() {
                if let Some(n) = self.focus.node {
                    ed.set_text(&self.doc.control_value(n));
                }
            }
            self.relayout_styles_ready();
        }
        None
    }

    /// §4.10.21.3 submit, after "interactively validate the constraints" (§4.10.21.2; unless
    /// `novalidate` / `formnovalidate`): with an invalid control, the first one whose
    /// `invalid` event wasn't canceled gets focus and its message is reported instead.
    fn submit_form(
        &mut self,
        form: NodeId,
        submitter: Option<NodeId>,
        point: Option<(i32, i32)>,
    ) -> Option<FormSubmission> {
        let novalidate = self.doc.attr(form, "novalidate").is_some()
            || submitter.is_some_and(|s| self.doc.attr(s, "formnovalidate").is_some());
        if !novalidate {
            // (invalid control, its `invalid` event wasn't canceled)
            let invalid: Vec<(NodeId, bool)> = if self.script.is_some() {
                let mut out = Vec::new();
                self.with_window(|w| out = w.invalid_controls(form));
                out
            } else {
                self.doc
                    .form_controls(form)
                    .into_iter()
                    .filter(|&c| self.doc.is_validation_candidate(c) && !self.doc.is_valid_control(c))
                    .map(|c| (c, true))
                    .collect()
            };
            if !invalid.is_empty() {
                // If every `invalid` event was canceled nothing is reported, but the form still
                // isn't submitted.
                if let Some(&(bad, _)) = invalid.iter().find(|x| x.1) {
                    self.focus_node(Some(bad));
                    let msg = self.doc.validation_message(bad);
                    if let Some(r) = bb_layout::node_rects(&self.layout, &self.doc, bad)
                        .into_iter()
                        .reduce(|a, b| a.union(&b))
                    {
                        self.validation_report = Some((r, msg));
                    }
                }
                return None;
            }
        }
        // Step 6.3: the submit event, unless the form submits itself.
        if !self.script_submit(form, submitter.filter(|&s| s != form)) {
            return None;
        }
        forms::submission(&self.doc, form, submitter, point, &self.url, &self.base_url, self.encoding)
    }

    /// §4.10.21.2 implicit submission from a text control in `form`.
    fn implicit_submission(&mut self, control: NodeId) -> Option<FormSubmission> {
        let form = self.doc.form_owner(control)?;
        let controls = self.doc.form_controls(form);
        if let Some(&default) = controls.iter().find(|&&c| forms::is_submit_button(&self.doc, c)) {
            if self.doc.is_disabled(default) {
                return None;
            }
            return self.submit_form(form, Some(default), None);
        }
        let blocking = controls
            .iter()
            .filter(|&&c| {
                self.doc.is_html(c, "input")
                    && matches!(
                        self.doc.input_type(c),
                        InputType::Text
                            | InputType::Search
                            | InputType::Url
                            | InputType::Tel
                            | InputType::Email
                            | InputType::Password
                            | InputType::Date
                            | InputType::Month
                            | InputType::Week
                            | InputType::Time
                            | InputType::DateTimeLocal
                            | InputType::Number
                    )
            })
            .count();
        if blocking > 1 {
            return None;
        }
        self.submit_form(form, None, None)
    }

    // ----- selects -----

    fn select_popup(&self, select: NodeId) -> Option<SelectPopup> {
        let doc = &self.doc;
        let options = doc.list_of_options(select);
        let mut items = Vec::new();
        let mut last_group = None;
        for (i, &o) in options.iter().enumerate() {
            let group = doc.parent(o).filter(|&p| doc.is_html(p, "optgroup"));
            if group.is_some() && group != last_group {
                let g = group.unwrap_or(o);
                items.push(PopupItem {
                    label: doc.attr(g, "label").unwrap_or("").to_string(),
                    group: true,
                    enabled: false,
                    selected: false,
                    option: None,
                });
            }
            last_group = group;
            items.push(PopupItem {
                label: forms::option_label(doc, o),
                group: false,
                enabled: !doc.option_disabled(o),
                selected: doc.option_selectedness(o),
                option: Some(i),
            });
        }
        let rect = bb_layout::node_rects(&self.layout, doc, select).first().copied()?;
        Some(SelectPopup {
            frame_path: Vec::new(),
            select,
            rect,
            items,
            multiple: doc.attr(select, "multiple").is_some(),
        })
    }

    /// The user picked option `index` of `select` in the document at `frame_path`.
    pub fn choose_option(&mut self, frame_path: &[usize], select: NodeId, index: usize) -> bool {
        if let Some((&first, rest)) = frame_path.split_first() {
            return self.frames.get_mut(first).is_some_and(|f| f.page.choose_option(rest, select, index));
        }
        let Some(&option) = self.doc.list_of_options(select).get(index) else { return false };
        if self.doc.is_disabled(select) || !self.doc.user_select_option(select, option) {
            return false;
        }
        self.relayout_styles_ready();
        self.script_changed(select);
        true
    }

    /// Up/Down/Home/End on a focused single select change its selection directly.
    fn step_select(&mut self, select: NodeId, key: PageKey) -> bool {
        if self.doc.attr(select, "multiple").is_some() {
            return false;
        }
        let options = self.doc.list_of_options(select);
        let enabled: Vec<usize> =
            (0..options.len()).filter(|&i| !self.doc.option_disabled(options[i])).collect();
        let cur = options.iter().position(|&o| self.doc.option_selectedness(o));
        let next = match key {
            PageKey::Up | PageKey::Left => cur.and_then(|c| enabled.iter().rev().find(|&&i| i < c).copied()),
            PageKey::Down | PageKey::Right => match cur {
                Some(c) => enabled.iter().find(|&&i| i > c).copied(),
                None => enabled.first().copied(),
            },
            PageKey::Home => enabled.first().copied(),
            PageKey::End => enabled.last().copied(),
            _ => None,
        };
        match next {
            Some(i) if Some(i) != cur => self.choose_option(&[], select, i),
            _ => false,
        }
    }

    // ----- keyboard -----

    /// Typed text (an IME commit or a printable key) goes to the focused text control.
    pub fn insert_text(&mut self, text: &str) -> Outcome {
        if let Some(i) = self.focus.frame {
            return self.frames.get_mut(i).map(|f| f.page.insert_text(text)).unwrap_or_default();
        }
        let Some(n) = self.focus.node else { return Outcome::default() };
        if self.focus.editor.is_none() {
            // A space on a button, checkbox etc. is a key, not text.
            return if text == " " {
                self.key(PageKey::Space, KeyMods::default())
            } else {
                Outcome::default()
            };
        }
        if self.doc.attr(n, "readonly").is_some() {
            return Outcome::used();
        }
        if let Some(ed) = self.focus.editor.as_mut() {
            ed.insert(text);
        }
        self.commit_editor();
        Outcome::changed()
    }

    /// A key press while the page has focus.
    pub fn key(&mut self, key: PageKey, m: KeyMods) -> Outcome {
        if let Some(i) = self.focus.frame {
            let mut out = match self.frames.get_mut(i) {
                Some(f) => f.page.key(key, m),
                None => Outcome::default(),
            };
            if let Some((_, path)) = out.submit.as_mut() {
                path.insert(0, i);
            }
            if let Some(h) = out.follow.as_mut() {
                h.frame_path.insert(0, i);
            }
            if let Some(p) = out.popup.as_mut() {
                let f = &self.frames[i];
                p.frame_path.insert(0, i);
                p.rect = Rect::new(p.rect.x + f.rect.x, p.rect.y + f.rect.y - f.scroll_y, p.rect.w, p.rect.h);
            }
            // Tab past the frame's last element continues in this document.
            if key == PageKey::Tab && out.focus_exit.is_some() {
                return self.tab_from_frame(i, !m.shift);
            }
            return out;
        }
        if key == PageKey::Tab {
            return self.tab(!m.shift);
        }
        let Some(n) = self.focus.node else { return Outcome::default() };
        let Some(kind) = focus_kind(&self.doc, n) else { return Outcome::default() };
        match kind {
            FocusKind::Text | FocusKind::TextArea => self.edit_key(n, kind, key, m),
            FocusKind::Select => match key {
                PageKey::Space | PageKey::Enter | PageKey::Open => {
                    Outcome { popup: self.select_popup(n), ..Outcome::used() }
                }
                PageKey::Up
                | PageKey::Down
                | PageKey::Home
                | PageKey::End
                | PageKey::Left
                | PageKey::Right => Outcome { repaint: self.step_select(n, key), ..Outcome::used() },
                _ => Outcome::default(),
            },
            FocusKind::Checkbox if key == PageKey::Space => {
                let c = self.doc.checkedness(n);
                self.doc.set_checkedness(n, !c);
                self.relayout_styles_ready();
                self.script_changed(n);
                Outcome::changed()
            }
            FocusKind::Radio => match key {
                PageKey::Space => {
                    if !self.doc.checkedness(n) {
                        self.doc.set_checkedness(n, true);
                        self.relayout_styles_ready();
                        self.script_changed(n);
                    }
                    Outcome::changed()
                }
                // Arrows move to and check the next/previous radio in the group.
                PageKey::Up | PageKey::Down | PageKey::Left | PageKey::Right => {
                    let group: Vec<NodeId> = self
                        .doc
                        .radio_group(n)
                        .into_iter()
                        .filter(|&r| focus_kind(&self.doc, r).is_some())
                        .collect();
                    let Some(i) = group.iter().position(|&r| r == n) else { return Outcome::used() };
                    let len = group.len();
                    let j = if matches!(key, PageKey::Up | PageKey::Left) {
                        (i + len - 1) % len
                    } else {
                        (i + 1) % len
                    };
                    let r = group[j];
                    self.doc.set_checkedness(r, true);
                    self.focus_node(Some(r));
                    self.relayout_styles_ready();
                    self.script_changed(r);
                    Outcome::changed()
                }
                _ => Outcome::default(),
            },
            FocusKind::Button if matches!(key, PageKey::Space | PageKey::Enter) => {
                let submit = self.activate_button(n, None).map(|s| (s, Vec::new()));
                Outcome { submit, ..Outcome::changed() }
            }
            FocusKind::Link if key == PageKey::Enter => {
                let follow = self.link_hit_for(n);
                Outcome { follow, ..Outcome::used() }
            }
            FocusKind::Summary if matches!(key, PageKey::Space | PageKey::Enter) => {
                self.toggle_details(n);
                Outcome::changed()
            }
            _ => Outcome::default(),
        }
    }

    fn link_hit_for(&self, link: NodeId) -> Option<LinkHit> {
        let href = self.doc.attr(link, "href")?;
        let url = self.base_url.join(href.trim()).ok()?;
        let target = self
            .doc
            .attr(link, "target")
            .or_else(|| self.doc.elements_by_tag("base").into_iter().find_map(|b| self.doc.attr(b, "target")))
            .unwrap_or("")
            .trim()
            .to_string();
        Some(LinkHit { url, target, frame_path: Vec::new(), post: None })
    }

    fn edit_key(&mut self, n: NodeId, kind: FocusKind, key: PageKey, m: KeyMods) -> Outcome {
        let readonly = self.doc.attr(n, "readonly").is_some();
        let multiline = kind == FocusKind::TextArea;
        let Some(ed) = self.focus.editor.as_mut() else { return Outcome::default() };
        let before = (ed.text.clone(), ed.caret, ed.selection());
        let mut out = Outcome::used();
        match key {
            PageKey::Left => ed.left(m.word, m.shift),
            PageKey::Right => ed.right(m.word, m.shift),
            PageKey::Home if m.primary => ed.text_start(m.shift),
            PageKey::End if m.primary => ed.text_end(m.shift),
            PageKey::Home => ed.home(m.shift),
            PageKey::End => ed.end(m.shift),
            PageKey::Up if multiline => ed.up(m.shift),
            PageKey::Down if multiline => ed.down(m.shift),
            PageKey::SelectAll => ed.select_all(),
            PageKey::Copy => out.copied = ed.selected_text().map(str::to_string),
            PageKey::Cut => {
                out.copied = ed.selected_text().map(str::to_string);
                if out.copied.is_some() && !readonly {
                    ed.backspace();
                }
            }
            PageKey::Backspace if !readonly => ed.backspace(),
            PageKey::Delete if !readonly => ed.delete(),
            PageKey::Undo if !readonly => ed.undo(),
            PageKey::Redo if !readonly => ed.redo(),
            PageKey::Enter if multiline => {
                if !readonly {
                    ed.insert("\n");
                }
            }
            PageKey::Enter => {
                out.submit = self.implicit_submission(n).map(|s| (s, Vec::new()));
                out.repaint = true;
                return out;
            }
            // Space arrives as text; Escape, Up/Down in single-line fields etc. aren't used.
            PageKey::Backspace | PageKey::Delete | PageKey::Undo | PageKey::Redo => {}
            _ => return Outcome::default(),
        }
        let after = (ed.text.clone(), ed.caret, ed.selection());
        if after.0 != before.0 {
            self.commit_editor();
            out.repaint = true;
        } else if after != before {
            self.rebuild_display();
            out.repaint = true;
        }
        out
    }

    // ----- sequential focus navigation (§6.6.3) -----

    /// Whether `n` is rendered (no `display: none` on it or an ancestor, and visible).
    fn rendered(&self, n: NodeId) -> bool {
        let Some(s) = self.styles.get(n) else { return false };
        if s.display == bb_style::Display::None || s.visibility != bb_style::Visibility::Visible {
            return false;
        }
        !self
            .doc
            .ancestors(n)
            .any(|a| self.styles.get(a).is_some_and(|s| s.display == bb_style::Display::None))
            && !bb_layout::node_rects(&self.layout, &self.doc, n).is_empty()
    }

    /// This document's sequential focus navigation order: elements with a positive `tabindex`
    /// in ascending order, then the rest in tree order; `tabindex < 0` is skipped. Child frames
    /// appear where their frame element is, as `Err(frame index)`.
    fn focus_order(&self) -> Vec<Result<NodeId, usize>> {
        let mut positive = Vec::new();
        let mut normal = Vec::new();
        for n in self.doc.descendants(Document::ROOT) {
            if let Some(i) = self.frames.iter().position(|f| f.node == n) {
                normal.push(Err(i));
                continue;
            }
            if focus_kind(&self.doc, n).is_none() {
                continue;
            }
            let tabindex = self.doc.attr(n, "tabindex").and_then(|t| t.trim().parse::<i64>().ok());
            if tabindex.is_some_and(|t| t < 0) || !self.rendered(n) {
                continue;
            }
            match tabindex {
                Some(t) if t > 0 => positive.push((t, n)),
                _ => normal.push(Ok(n)),
            }
        }
        positive.sort_by_key(|&(t, _)| t);
        positive.into_iter().map(|(_, n)| Ok(n)).chain(normal).collect()
    }

    /// Move focus to the next (or previous) element, entering child frames.
    fn tab(&mut self, forward: bool) -> Outcome {
        let order = self.focus_order();
        let cur = self.focus.node.and_then(|n| order.iter().position(|e| *e == Ok(n)));
        self.tab_from(order, cur, forward)
    }

    fn tab_from_frame(&mut self, frame: usize, forward: bool) -> Outcome {
        let order = self.focus_order();
        let cur = order.iter().position(|e| *e == Err(frame));
        if let Some(f) = self.frames.get_mut(frame) {
            f.page.blur();
        }
        self.focus.frame = None;
        self.tab_from(order, cur, forward)
    }

    fn tab_from(&mut self, order: Vec<Result<NodeId, usize>>, cur: Option<usize>, forward: bool) -> Outcome {
        let len = order.len();
        let mut idx = match (cur, forward) {
            (Some(c), true) => c + 1,
            (Some(c), false) => c.wrapping_sub(1),
            (None, true) => 0,
            (None, false) => len.wrapping_sub(1),
        };
        while idx < len {
            match order[idx] {
                Ok(n) => {
                    self.focus_node(Some(n));
                    if let Some(ed) = self.focus.editor.as_mut() {
                        ed.select_all();
                    }
                    self.rebuild_display();
                    return Outcome::changed();
                }
                Err(i) => {
                    // Into the frame: its first (or last) element.
                    if self.focus.node.take().is_some() {
                        self.focus.editor = None;
                    }
                    self.focus.frame = Some(i);
                    let out = self.frames[i].page.tab(forward);
                    if out.focus_exit.is_none() {
                        self.rebuild_display();
                        return out;
                    }
                    self.focus.frame = None;
                }
            }
            idx = if forward { idx + 1 } else { idx.wrapping_sub(1) };
        }
        let had = self.blur();
        Outcome { consumed: true, repaint: had, focus_exit: Some(forward), ..Default::default() }
    }
}

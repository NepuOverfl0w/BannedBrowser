//! Form control state (HTML §4.10): the value and checkedness of `<input>` and `<textarea>`
//! elements, radio button groups, form owners, the disabled state, and form reset.
//!
//! Like option selectedness (`select.rs`), a control's current value and checkedness are DOM
//! state, not attributes: until the user (or, later, script) changes them, the *dirty value
//! flag* / *dirty checkedness flag* is false and they follow the `value` / `checked` content
//! attributes (or a textarea's child text).

use crate::{Document, NodeId};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct FormState {
    /// Current values whose dirty value flag is true (inputs: the value; textareas: the raw
    /// value, newlines normalized to LF).
    values: HashMap<NodeId, String>,
    /// Checkedness whose dirty checkedness flag is true.
    checked: HashMap<NodeId, bool>,
    /// Form owners set by the parser's form element pointer (§13.2.6.1).
    parser_owner: HashMap<NodeId, NodeId>,
    /// Form-associated custom elements (§4.13.7) and their submission values.
    face: HashMap<NodeId, FaceSubmission>,
    /// Custom validity error messages (§4.10.21.3 setCustomValidity).
    pub(crate) custom_validity: HashMap<NodeId, String>,
    /// Controls whose value was last changed by a user edit.
    pub(crate) user_edited: std::collections::HashSet<NodeId>,
}

/// A form-associated custom element's submission value (HTML §4.13.7.4 setFormValue).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FaceSubmission {
    /// null: nothing is submitted.
    #[default]
    None,
    /// A string or a File, submitted under the element's `name`.
    Value(crate::form_data::EntryValue),
    /// A FormData's entries, submitted as they are.
    Entries(Vec<(String, crate::form_data::EntryValue)>),
}

/// An `<input>`'s `type` state (§4.10.5.1), keyword lowercased; unknown and missing values are
/// the Text state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputType {
    Hidden,
    Text,
    Search,
    Tel,
    Url,
    Email,
    Password,
    Date,
    Month,
    Week,
    Time,
    DateTimeLocal,
    Number,
    Range,
    Color,
    Checkbox,
    Radio,
    File,
    Submit,
    Image,
    Reset,
    Button,
}

impl InputType {
    pub fn parse(s: Option<&str>) -> InputType {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("hidden") => InputType::Hidden,
            Some("search") => InputType::Search,
            Some("tel") => InputType::Tel,
            Some("url") => InputType::Url,
            Some("email") => InputType::Email,
            Some("password") => InputType::Password,
            Some("date") => InputType::Date,
            Some("month") => InputType::Month,
            Some("week") => InputType::Week,
            Some("time") => InputType::Time,
            Some("datetime-local") => InputType::DateTimeLocal,
            Some("number") => InputType::Number,
            Some("range") => InputType::Range,
            Some("color") => InputType::Color,
            Some("checkbox") => InputType::Checkbox,
            Some("radio") => InputType::Radio,
            Some("file") => InputType::File,
            Some("submit") => InputType::Submit,
            Some("image") => InputType::Image,
            Some("reset") => InputType::Reset,
            Some("button") => InputType::Button,
            _ => InputType::Text,
        }
    }

    /// The type's keyword (the `type` IDL attribute; the form data set's type for `_charset_`).
    pub fn keyword(self) -> &'static str {
        match self {
            InputType::Hidden => "hidden",
            InputType::Text => "text",
            InputType::Search => "search",
            InputType::Tel => "tel",
            InputType::Url => "url",
            InputType::Email => "email",
            InputType::Password => "password",
            InputType::Date => "date",
            InputType::Month => "month",
            InputType::Week => "week",
            InputType::Time => "time",
            InputType::DateTimeLocal => "datetime-local",
            InputType::Number => "number",
            InputType::Range => "range",
            InputType::Color => "color",
            InputType::Checkbox => "checkbox",
            InputType::Radio => "radio",
            InputType::File => "file",
            InputType::Submit => "submit",
            InputType::Image => "image",
            InputType::Reset => "reset",
            InputType::Button => "button",
        }
    }

    /// Types whose value the user edits as text. Date, time, number, range and color pickers
    /// aren't implemented; they are edited as text fields, and their values are sanitized.
    pub fn is_text_entry(self) -> bool {
        matches!(
            self,
            InputType::Text
                | InputType::Search
                | InputType::Tel
                | InputType::Url
                | InputType::Email
                | InputType::Password
                | InputType::Date
                | InputType::Month
                | InputType::Week
                | InputType::Time
                | InputType::DateTimeLocal
                | InputType::Number
                | InputType::Range
                | InputType::Color
        )
    }

    /// The "value" mode (§4.10.5.4): the value is the current value, not the `value` attribute.
    pub fn value_mode_is_value(self) -> bool {
        self.is_text_entry()
    }
}

/// §4.10.2: elements that can be form-associated ("form-associated elements"), besides
/// form-associated custom elements ([`Document::is_form_associated_custom`]).
pub const FORM_ASSOCIATED: &[&str] =
    &["button", "fieldset", "input", "object", "output", "select", "textarea", "img"];

/// §4.10.2 "listed elements": form-associated elements that have a `form` attribute and are in
/// `form.elements`.
pub const LISTED: &[&str] = &["button", "fieldset", "input", "object", "output", "select", "textarea"];

fn is_ascii_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0C' | '\r')
}

fn strip_newlines(s: &str) -> String {
    s.chars().filter(|&c| c != '\n' && c != '\r').collect()
}

/// §2.3.4.3 "valid floating-point number" (a simplified check: optional "-", digits with an
/// optional fraction, optional exponent; no leading "+", no "." without digits after it).
pub(crate) fn is_valid_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == f {
            return false;
        }
    } else if int_digits == 0 {
        return false;
    }
    if int_digits == 0 && b.get(start) != Some(&b'.') {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e {
            return false;
        }
    }
    i == b.len()
}

/// §4.10.5.1 value sanitization algorithms for the types we edit as text.
pub fn sanitize_value(ty: InputType, v: &str) -> String {
    match ty {
        InputType::Text | InputType::Search | InputType::Tel | InputType::Password => strip_newlines(v),
        InputType::Url => strip_newlines(v).trim_matches(is_ascii_ws).to_string(),
        // A single e-mail address (no `multiple` handling yet).
        InputType::Email => strip_newlines(v).trim_matches(is_ascii_ws).to_string(),
        InputType::Number => {
            if is_valid_float(v) {
                v.to_string()
            } else {
                String::new()
            }
        }
        // §4.10.5.1.7–11: a value that isn't a valid date, month, week, time or local
        // date-time string becomes the empty string.
        InputType::Date | InputType::Month | InputType::Week | InputType::Time | InputType::DateTimeLocal => {
            if crate::validity::to_number(ty, v).is_some() {
                v.to_string()
            } else {
                String::new()
            }
        }
        InputType::Color => {
            let ok = v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit());
            if ok {
                v.to_ascii_lowercase()
            } else {
                "#000000".to_string()
            }
        }
        _ => strip_newlines(v),
    }
}

impl Document {
    /// The `type` state of an `<input>` (Text for anything else).
    pub fn input_type(&self, input: NodeId) -> InputType {
        InputType::parse(self.attr(input, "type"))
    }

    /// Whether `el` is an editable text control: a textarea, or an input in a text-entry state.
    pub fn is_text_control(&self, el: NodeId) -> bool {
        self.is_html(el, "textarea") || (self.is_html(el, "input") && self.input_type(el).is_text_entry())
    }

    /// The control's current value (§4.10.5.4 for inputs, §4.10.11 "raw value" for textareas;
    /// LF line breaks). For inputs in the "default" mode it is the `value` attribute (or ""),
    /// in "default/on" (checkbox, radio) the attribute or "on".
    pub fn control_value(&self, el: NodeId) -> String {
        if self.is_html(el, "textarea") {
            return match self.form_state.values.get(&el) {
                Some(v) => v.clone(),
                None => crate::normalize_newlines(&self.child_text(el)),
            };
        }
        // A button's value is its value attribute (HTML §4.10.6).
        if self.is_html(el, "button") {
            return self.attr(el, "value").unwrap_or("").to_string();
        }
        if !self.is_html(el, "input") {
            return String::new();
        }
        let ty = self.input_type(el);
        if ty.value_mode_is_value() {
            if let Some(v) = self.form_state.values.get(&el) {
                return v.clone();
            }
            return sanitize_value(ty, self.attr(el, "value").unwrap_or(""));
        }
        match ty {
            InputType::Checkbox | InputType::Radio => self.attr(el, "value").unwrap_or("on").to_string(),
            InputType::File => String::new(),
            _ => self.attr(el, "value").unwrap_or("").to_string(),
        }
    }

    /// Set the current value as the user would (sets the dirty value flag). Inputs sanitize it.
    pub fn set_control_value(&mut self, el: NodeId, value: &str) {
        let v = if self.is_html(el, "input") {
            sanitize_value(self.input_type(el), value)
        } else {
            crate::normalize_newlines(value)
        };
        if self.form_state.values.get(&el) != Some(&v) {
            self.form_state.values.insert(el, v);
            self.generation += 1;
        }
    }

    /// A textarea's child text content (its default value).
    fn child_text(&self, el: NodeId) -> String {
        let mut s = String::new();
        for c in self.children(el) {
            if let Some(t) = self.node(c).as_text() {
                s.push_str(t);
            }
        }
        s
    }

    /// A checkbox's or radio button's checkedness.
    pub fn checkedness(&self, input: NodeId) -> bool {
        match self.form_state.checked.get(&input) {
            Some(&c) => c,
            None => self.attr(input, "checked").is_some(),
        }
    }

    /// Set checkedness as the user would (dirty checkedness flag). Checking a radio button
    /// unchecks the others in its group (§4.10.5.1.15).
    pub fn set_checkedness(&mut self, input: NodeId, checked: bool) {
        self.form_state.checked.insert(input, checked);
        self.generation += 1;
        if checked && self.input_type(input) == InputType::Radio {
            for other in self.radio_group(input) {
                if other != input && self.checkedness(other) {
                    self.form_state.checked.insert(other, false);
                }
            }
        }
    }

    /// §4.10.5.1.15 the radio button group of `input`: radio buttons with the same non-empty
    /// `name` (compared exactly) and the same form owner, in the same tree; in tree order.
    pub fn radio_group(&self, input: NodeId) -> Vec<NodeId> {
        let Some(name) = self.attr(input, "name").filter(|n| !n.is_empty()) else { return vec![input] };
        let owner = self.form_owner(input);
        let root = self.tree_root(input);
        self.descendants(root)
            .filter(|&n| {
                self.is_html(n, "input")
                    && self.input_type(n) == InputType::Radio
                    && self.attr(n, "name") == Some(name)
                    && self.form_owner(n) == owner
            })
            .collect()
    }

    /// Called by the parser for a form-associated element it inserted while its form element
    /// pointer was set (HTML §13.2.6.1 "create an element for a token").
    pub fn set_parser_form_owner(&mut self, el: NodeId, form: NodeId) {
        self.form_state.parser_owner.insert(el, form);
    }

    /// Make `el` a form-associated custom element (or not): the host does when such a
    /// custom element is defined (HTML §4.13.7).
    pub fn set_form_associated_custom(&mut self, el: NodeId, on: bool) {
        if on {
            self.form_state.face.entry(el).or_default();
        } else {
            self.form_state.face.remove(&el);
        }
        self.generation += 1;
    }

    pub fn is_form_associated_custom(&self, el: NodeId) -> bool {
        self.form_state.face.contains_key(&el)
    }

    /// The form-associated custom elements.
    pub fn form_associated_customs(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.form_state.face.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Set a form-associated custom element's submission value.
    pub fn set_face_submission(&mut self, el: NodeId, value: FaceSubmission) {
        if let Some(s) = self.form_state.face.get_mut(&el) {
            *s = value;
        }
    }

    pub fn face_submission(&self, el: NodeId) -> Option<&FaceSubmission> {
        self.form_state.face.get(&el)
    }

    /// §4.10.17.3 the element's form owner: the element its `form` attribute names (listed
    /// elements), else the form the parser associated it with while both are in the same tree,
    /// else the nearest ancestor `form`.
    pub fn form_owner(&self, el: NodeId) -> Option<NodeId> {
        let face = self.is_form_associated_custom(el);
        let name = self.element(el)?.name.as_str();
        if !face && !FORM_ASSOCIATED.contains(&name) {
            return None;
        }
        if face || LISTED.contains(&name) {
            if let Some(id) = self.attr(el, "form") {
                if !self.is_connected(el) {
                    return None;
                }
                return self.get_element_by_id(id).filter(|&f| self.is_html(f, "form"));
            }
        }
        if let Some(&f) = self.form_state.parser_owner.get(&el) {
            if self.is_html(f, "form") && self.tree_root(f) == self.tree_root(el) {
                return Some(f);
            }
        }
        self.ancestors(el).find(|&a| self.is_html(a, "form"))
    }

    /// The form's listed elements in tree order (the elements of `form.elements`, plus
    /// fieldsets and objects).
    pub fn form_controls(&self, form: NodeId) -> Vec<NodeId> {
        let root = self.tree_root(form);
        self.descendants(root)
            .filter(|&n| {
                self.element(n)
                    .is_some_and(|e| LISTED.contains(&e.name.as_str()) || self.is_form_associated_custom(n))
                    && self.form_owner(n) == Some(form)
            })
            .collect()
    }

    /// §4.10.18.5 "actually disabled": a `disabled` attribute on a button, input, select,
    /// textarea or fieldset, or a descendant of a disabled fieldset that isn't in its first
    /// legend; an option or optgroup with `disabled`.
    pub fn is_disabled(&self, el: NodeId) -> bool {
        let Some(name) = self.element(el).map(|e| e.name.clone()) else { return false };
        if matches!(name.as_str(), "option") {
            return self.option_disabled(el);
        }
        if matches!(name.as_str(), "optgroup") {
            return self.attr(el, "disabled").is_some();
        }
        if !matches!(name.as_str(), "button" | "input" | "select" | "textarea" | "fieldset")
            && !self.is_form_associated_custom(el)
        {
            return false;
        }
        if self.attr(el, "disabled").is_some() {
            return true;
        }
        let mut child = el;
        for a in self.ancestors(el) {
            if self.is_html(a, "fieldset") && self.attr(a, "disabled").is_some() {
                let first_legend = self.element_children(a).find(|&c| self.is_html(c, "legend"));
                if first_legend != Some(child) {
                    return true;
                }
            }
            child = a;
        }
        false
    }

    /// §4.10.21.5 reset a form: every listed element's reset algorithm.
    /// HTML §4.10.4 a label's labeled control: the labelable element its `for` names, else
    /// its first labelable descendant.
    pub fn labeled_control(&self, label: NodeId) -> Option<NodeId> {
        let labelable = |n: NodeId| {
            self.element(n).is_some_and(|e| {
                e.ns == crate::Namespace::Html
                    && (matches!(
                        e.name.as_str(),
                        "button" | "meter" | "output" | "progress" | "select" | "textarea"
                    ) || (e.name == "input" && self.input_type(n) != InputType::Hidden))
                    || self.is_form_associated_custom(n)
            })
        };
        if let Some(id) = self.attr(label, "for") {
            return self.get_element_by_id(id).filter(|&n| labelable(n));
        }
        self.descendants(label).skip(1).find(|&n| labelable(n))
    }

    pub fn reset_form(&mut self, form: NodeId) {
        for el in self.form_controls(form) {
            self.form_state.user_edited.remove(&el);
            self.form_state.values.remove(&el);
            self.form_state.checked.remove(&el);
            if self.is_html(el, "select") {
                self.reset_select(el);
            }
        }
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribute, Namespace};

    fn el(doc: &mut Document, parent: NodeId, name: &str, attrs: &[(&str, &str)]) -> NodeId {
        let a = attrs.iter().map(|(k, v)| Attribute::new(k, v)).collect();
        let e = doc.create_element(Namespace::Html, name, a);
        doc.append_child(parent, e);
        e
    }

    #[test]
    fn values_follow_attributes_until_dirty() {
        let mut d = Document::new();
        let form = el(&mut d, Document::ROOT, "form", &[]);
        let t = el(&mut d, form, "input", &[("value", "a\nb")]);
        assert_eq!(d.control_value(t), "ab");
        d.set_attr(t, "value", "x");
        assert_eq!(d.control_value(t), "x");
        d.set_control_value(t, "typed\r\n");
        d.set_attr(t, "value", "ignored");
        assert_eq!(d.control_value(t), "typed");
        let n = el(&mut d, form, "input", &[("type", "NUMBER"), ("value", "1e3")]);
        assert_eq!(d.control_value(n), "1e3");
        d.set_control_value(n, "12abc");
        assert_eq!(d.control_value(n), "");
        let e = el(&mut d, form, "input", &[("type", "email"), ("value", "  a@b.c ")]);
        assert_eq!(d.control_value(e), "a@b.c");
        let c = el(&mut d, form, "input", &[("type", "checkbox")]);
        assert_eq!(d.control_value(c), "on");
        let ta = el(&mut d, form, "textarea", &[]);
        d.append_text(ta, "l1\r\nl2");
        assert_eq!(d.control_value(ta), "l1\nl2");
        d.reset_form(form);
        assert_eq!(d.control_value(t), "ignored");
    }

    #[test]
    fn radio_groups_and_checkedness() {
        let mut d = Document::new();
        let f1 = el(&mut d, Document::ROOT, "form", &[("id", "f1")]);
        let a = el(&mut d, f1, "input", &[("type", "radio"), ("name", "r"), ("checked", "")]);
        let b = el(&mut d, f1, "input", &[("type", "radio"), ("name", "r")]);
        let other_name = el(&mut d, f1, "input", &[("type", "radio"), ("name", "R"), ("checked", "")]);
        let f2 = el(&mut d, Document::ROOT, "form", &[]);
        let elsewhere = el(&mut d, f2, "input", &[("type", "radio"), ("name", "r"), ("checked", "")]);
        // A `form` attribute moves it into f1's group.
        let by_attr = el(&mut d, f2, "input", &[("type", "radio"), ("name", "r"), ("form", "f1")]);
        assert_eq!(d.radio_group(a), vec![a, b, by_attr]);
        d.set_checkedness(b, true);
        assert!(!d.checkedness(a) && d.checkedness(b));
        assert!(d.checkedness(other_name) && d.checkedness(elsewhere));
        d.set_checkedness(by_attr, true);
        assert!(!d.checkedness(b));
        d.reset_form(f1);
        assert!(d.checkedness(a) && !d.checkedness(b) && !d.checkedness(by_attr));
    }

    #[test]
    fn disabled_fieldsets_and_legends() {
        let mut d = Document::new();
        let fs = el(&mut d, Document::ROOT, "fieldset", &[("disabled", "")]);
        let legend = el(&mut d, fs, "legend", &[]);
        let in_legend = el(&mut d, legend, "input", &[]);
        let second = el(&mut d, fs, "legend", &[]);
        let in_second = el(&mut d, second, "input", &[]);
        let plain = el(&mut d, fs, "button", &[]);
        assert!(!d.is_disabled(in_legend));
        assert!(d.is_disabled(in_second));
        assert!(d.is_disabled(plain));
        assert!(d.is_disabled(fs));
    }

    #[test]
    fn parser_owner_needs_the_same_tree() {
        let mut d = Document::new();
        let table = el(&mut d, Document::ROOT, "table", &[]);
        let form = el(&mut d, table, "form", &[]);
        let td = el(&mut d, table, "td", &[]);
        let input = el(&mut d, td, "input", &[]);
        assert_eq!(d.form_owner(input), None);
        d.set_parser_form_owner(input, form);
        assert_eq!(d.form_owner(input), Some(form));
        d.detach(form);
        assert_eq!(d.form_owner(input), None);
    }

    #[test]
    fn floats() {
        for ok in ["0", "-1", "1.5", ".5", "1e3", "1E-3", "-0.25e+2"] {
            assert!(is_valid_float(ok), "{ok}");
        }
        for bad in ["", "+1", "1.", "e3", "1e", "--1", "1x", "."] {
            assert!(!is_valid_float(bad), "{bad}");
        }
    }
}

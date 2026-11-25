//! Constraint validation (HTML §4.10.21.2): which elements are candidates, their validity
//! states (§4.10.5.3 for inputs, §4.10.7 select, §4.10.11 textarea), custom validity
//! messages, and the browser's validation messages.
//!
//! Two checks need more than the DOM: `pattern` is an ECMAScript regular expression and
//! `type=url` needs the URL parser. The host installs them once ([`set_hooks`]); without
//! them those two constraints don't apply.

use std::sync::OnceLock;

use crate::{Document, InputType, NodeId};

/// Checks the host provides.
pub struct Hooks {
    /// Whether `value` matches `pattern` as HTML's pattern attribute compiles it (`^(?:p)$`
    /// with the `v` flag); None if the pattern isn't a valid regular expression.
    pub pattern_matches: fn(pattern: &str, value: &str) -> Option<bool>,
    /// Whether `s` is a valid absolute URL.
    pub valid_absolute_url: fn(s: &str) -> bool,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Install the host's checks (the first call wins).
pub fn set_hooks(h: Hooks) {
    let _ = HOOKS.set(h);
}

/// ValidityState's flags (§4.10.21.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub too_long: bool,
    pub too_short: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub bad_input: bool,
    pub custom_error: bool,
}

impl Validity {
    pub fn valid(&self) -> bool {
        *self == Validity::default()
    }
}

/// HTML §4.10.5.3.2 "valid e-mail address".
pub fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    let local_ok = !local.is_empty()
        && local.bytes().all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b));
    let label_ok = |l: &str| {
        let b = l.as_bytes();
        !b.is_empty()
            && b.len() <= 63
            && b[0].is_ascii_alphanumeric()
            && b[b.len() - 1].is_ascii_alphanumeric()
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    };
    local_ok && domain.split('.').all(label_ok)
}

// ---- dates and times (HTML §2.3.5) as numbers ----

fn digits(s: &str, n: usize) -> Option<i64> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// §2.3.5.1 a valid month string: (year, month).
fn parse_month(s: &str) -> Option<(i64, i64)> {
    let (y, m) = s.rsplit_once('-')?;
    let y = (y.len() >= 4 && y.bytes().all(|b| b.is_ascii_digit())).then(|| y.parse::<i64>().ok())??;
    let m = digits(m, 2)?;
    (y > 0 && (1..=12).contains(&m)).then_some((y, m))
}

/// §2.3.5.2 a valid date string: days since the epoch.
fn parse_date(s: &str) -> Option<i64> {
    let (ym, d) = s.rsplit_once('-')?;
    let (y, m) = parse_month(ym)?;
    let d = digits(d, 2)?;
    (1..=days_in_month(y, m)).contains(&d).then(|| days_from_civil(y, m, d))
}

/// §2.3.5.4 a valid time string: milliseconds since midnight.
fn parse_time(s: &str) -> Option<f64> {
    let mut parts = s.splitn(3, ':');
    let h = digits(parts.next()?, 2)?;
    let mi = digits(parts.next()?, 2)?;
    let sec = match parts.next() {
        None => 0.0,
        Some(sec) => {
            let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
            let w = digits(whole, 2)?;
            if !(frac.len() <= 3 && frac.bytes().all(|b| b.is_ascii_digit()))
                || (sec.contains('.') && frac.is_empty())
            {
                return None;
            }
            let f = if frac.is_empty() { 0.0 } else { format!("0.{frac}").parse::<f64>().ok()? };
            if w > 59 {
                return None;
            }
            w as f64 + f
        }
    };
    (h <= 23 && mi <= 59).then(|| ((h * 3600 + mi * 60) as f64 + sec) * 1000.0)
}

/// §2.3.5.8 a valid week string: the Monday of that week, in ms since the epoch.
fn parse_week(s: &str) -> Option<f64> {
    let (y, w) = s.split_once("-W")?;
    let y = (y.len() >= 4 && y.bytes().all(|b| b.is_ascii_digit())).then(|| y.parse::<i64>().ok())??;
    let w = digits(w, 2)?;
    // Week 1 holds the year's first Thursday; years starting on Thursday (or Wednesday in
    // leap years) have 53 weeks.
    let jan1 = days_from_civil(y, 1, 1);
    let dow = (jan1 + 3).rem_euclid(7); // 0 = Monday
    let leap = days_in_month(y, 2) == 29;
    let weeks = if dow == 3 || (dow == 2 && leap) { 53 } else { 52 };
    if !(1..=weeks).contains(&w) || y <= 0 {
        return None;
    }
    let week1_monday = if dow <= 3 { jan1 - dow } else { jan1 + 7 - dow };
    Some(((week1_monday + (w - 1) * 7) * 86_400_000) as f64)
}

/// §4.10.5.1 "convert a string to a number" for the input's type.
pub(crate) fn to_number(ty: InputType, s: &str) -> Option<f64> {
    match ty {
        InputType::Number | InputType::Range => {
            crate::forms::is_valid_float(s).then(|| s.parse::<f64>().ok()).flatten().filter(|n| n.is_finite())
        }
        InputType::Date => parse_date(s).map(|d| (d * 86_400_000) as f64),
        InputType::Month => parse_month(s).map(|(y, m)| ((y - 1970) * 12 + m - 1) as f64),
        InputType::Week => parse_week(s),
        InputType::Time => parse_time(s),
        InputType::DateTimeLocal => {
            let (d, t) = s.split_once(['T', ' '])?;
            Some((parse_date(d)? * 86_400_000) as f64 + parse_time(t)?)
        }
        _ => None,
    }
}

/// (default step, step scale factor, default step base) for types with a step (§4.10.5.4).
fn step_params(ty: InputType) -> Option<(f64, f64, f64)> {
    Some(match ty {
        InputType::Date => (1.0, 86_400_000.0, 0.0),
        InputType::Month => (1.0, 1.0, 0.0),
        InputType::Week => (1.0, 604_800_000.0, -259_200_000.0),
        InputType::Time | InputType::DateTimeLocal => (60.0, 1000.0, 0.0),
        InputType::Number | InputType::Range => (1.0, 1.0, 0.0),
        _ => return None,
    })
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Whether `attr` holds a valid non-negative integer (for maxlength/minlength).
fn non_negative(doc: &Document, el: NodeId, attr: &str) -> Option<usize> {
    let v = doc.attr(el, attr)?.trim_matches([' ', '\t', '\n', '\x0C', '\r']);
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then(|| v.parse().ok()).flatten()
}

impl Document {
    /// §4.10.21.2 a candidate for constraint validation: a submittable element (button,
    /// input, select, textarea, form-associated custom element) that isn't barred.
    pub fn is_validation_candidate(&self, el: NodeId) -> bool {
        let Some(e) = self.element(el) else { return false };
        let face = self.is_form_associated_custom(el);
        if !(face
            || matches!(e.name.as_str(), "button" | "input" | "select" | "textarea")
                && e.ns == crate::Namespace::Html)
        {
            return false;
        }
        if self.is_disabled(el) || self.ancestors(el).any(|a| self.is_html(a, "datalist")) {
            return false;
        }
        if face {
            return self.attr(el, "readonly").is_none();
        }
        if self.is_html(el, "button") {
            let ty = self.attr(el, "type").map(|t| t.trim().to_ascii_lowercase());
            return !matches!(ty.as_deref(), Some("reset" | "button"));
        }
        if self.is_html(el, "input") {
            let ty = self.input_type(el);
            if matches!(ty, InputType::Hidden | InputType::Reset | InputType::Button) {
                return false;
            }
            // "If the readonly attribute is specified on an input element, the element is
            // barred from constraint validation."
            if self.attr(el, "readonly").is_some() {
                return false;
            }
        }
        !(self.is_html(el, "textarea") && self.attr(el, "readonly").is_some())
    }

    /// The element's custom validity error message (`setCustomValidity`).
    pub fn custom_validity(&self, el: NodeId) -> &str {
        self.form_state.custom_validity.get(&el).map_or("", String::as_str)
    }

    pub fn set_custom_validity(&mut self, el: NodeId, message: &str) {
        if message.is_empty() {
            self.form_state.custom_validity.remove(&el);
        } else {
            self.form_state.custom_validity.insert(el, message.to_string());
        }
        self.mark_style_dirty(el);
    }

    /// The value was last changed by a user edit (tooLong and tooShort apply only then).
    pub fn mark_user_edited(&mut self, el: NodeId, edited: bool) {
        if edited {
            self.form_state.user_edited.insert(el);
        } else {
            self.form_state.user_edited.remove(&el);
        }
    }

    /// The validity states of a built-in control (custom elements keep theirs in the host).
    pub fn validity(&self, el: NodeId) -> Validity {
        let mut v = Validity { custom_error: !self.custom_validity(el).is_empty(), ..Default::default() };
        let required = self.attr(el, "required").is_some();
        // "Mutable": not disabled and not readonly (inputs and textareas, §4.10.18.3).
        let mutable = !self.is_disabled(el) && self.attr(el, "readonly").is_none();
        if self.is_html(el, "textarea") {
            let value = self.control_value(el);
            v.value_missing = required && mutable && value.is_empty();
            self.length_checks(el, &value, &mut v);
            return v;
        }
        if self.is_html(el, "select") {
            v.value_missing = required && self.select_value_missing(el);
            return v;
        }
        if !self.is_html(el, "input") {
            return v;
        }
        let ty = self.input_type(el);
        let value = self.control_value(el);
        // valueMissing (§4.10.5.3.4 and each type's `required`).
        v.value_missing = match ty {
            InputType::Checkbox => required && !self.checkedness(el),
            InputType::Radio => {
                let group = self.radio_group(el);
                group.iter().any(|&r| self.attr(r, "required").is_some())
                    && !group.iter().any(|&r| self.checkedness(r))
            }
            InputType::File => required,
            InputType::Hidden
            | InputType::Range
            | InputType::Color
            | InputType::Submit
            | InputType::Image
            | InputType::Reset
            | InputType::Button => false,
            _ => required && mutable && value.is_empty(),
        };
        if value.is_empty() {
            return v;
        }
        let multiple = self.attr(el, "multiple").is_some();
        let values: Vec<&str> = if ty == InputType::Email && multiple {
            value.split(',').map(|s| s.trim_matches([' ', '\t', '\n', '\x0C', '\r'])).collect()
        } else {
            vec![value.as_str()]
        };
        // typeMismatch.
        v.type_mismatch = match ty {
            InputType::Email => !values.iter().all(|s| valid_email(s)),
            InputType::Url => HOOKS.get().is_some_and(|h| !(h.valid_absolute_url)(&value)),
            _ => false,
        };
        // patternMismatch (§4.10.5.3.6).
        if matches!(
            ty,
            InputType::Text
                | InputType::Search
                | InputType::Url
                | InputType::Tel
                | InputType::Email
                | InputType::Password
        ) {
            if let (Some(p), Some(h)) = (self.attr(el, "pattern"), HOOKS.get()) {
                v.pattern_mismatch = values.iter().any(|s| (h.pattern_matches)(p, s) == Some(false));
            }
            self.length_checks(el, &value, &mut v);
        }
        // rangeUnderflow, rangeOverflow, stepMismatch (§4.10.5.3.7–8).
        if let (Some(n), Some((def_step, scale, def_base))) = (to_number(ty, &value), step_params(ty)) {
            let min = self.attr(el, "min").and_then(|m| to_number(ty, m));
            let max = self.attr(el, "max").and_then(|m| to_number(ty, m));
            if let (InputType::Time, Some(lo), Some(hi)) = (ty, min, max) {
                if lo > hi {
                    // §4.10.5.1.10 a reversed range: outside it means after max and before min.
                    let out = n > hi && n < lo;
                    v.range_underflow = out;
                    v.range_overflow = out;
                } else {
                    v.range_underflow = n < lo;
                    v.range_overflow = n > hi;
                }
            } else {
                v.range_underflow = min.is_some_and(|m| n < m);
                v.range_overflow = max.is_some_and(|m| n > m);
            }
            let step = match self.attr(el, "step") {
                Some(s) if s.trim().eq_ignore_ascii_case("any") => None,
                Some(s) => match s.trim().parse::<f64>() {
                    Ok(x) if x > 0.0 && crate::forms::is_valid_float(s.trim()) => Some(x * scale),
                    _ => Some(def_step * scale),
                },
                None => Some(def_step * scale),
            };
            if let Some(step) = step {
                let base =
                    min.or_else(|| self.attr(el, "value").and_then(|x| to_number(ty, x))).unwrap_or(def_base);
                let q = (n - base) / step;
                v.step_mismatch = (q - q.round()).abs() > 1e-9 * q.abs().max(1.0);
            }
        }
        v
    }

    fn length_checks(&self, el: NodeId, value: &str, v: &mut Validity) {
        if !self.form_state.user_edited.contains(&el) {
            return;
        }
        let len = utf16_len(value);
        if let Some(max) = non_negative(self, el, "maxlength") {
            v.too_long = len > max;
        }
        if let Some(min) = non_negative(self, el, "minlength") {
            v.too_short = len > 0 && len < min;
        }
    }

    /// §4.10.7 a required select is missing a value: nothing selected, or only its
    /// placeholder label option.
    fn select_value_missing(&self, select: NodeId) -> bool {
        let options = self.list_of_options(select);
        let selected: Vec<NodeId> =
            options.iter().copied().filter(|&o| self.option_selectedness(o)).collect();
        if selected.is_empty() {
            return true;
        }
        let placeholder = self.attr(select, "multiple").is_none()
            && options.first().is_some_and(|&o| {
                self.parent(o) == Some(select) && crate::form_data::option_value(self, o).is_empty()
            });
        placeholder && selected.len() == 1 && Some(&selected[0]) == options.first()
    }

    /// §4.10.21.2: a candidate that suffers from no constraint.
    pub fn is_valid_control(&self, el: NodeId) -> bool {
        self.validity(el).valid()
    }

    /// The browser's validation message for a built-in control (the custom message first).
    pub fn validation_message(&self, el: NodeId) -> String {
        if !self.is_validation_candidate(el) {
            return String::new();
        }
        let custom = self.custom_validity(el);
        if !custom.is_empty() {
            return custom.to_string();
        }
        let v = self.validity(el);
        let ty = self.is_html(el, "input").then(|| self.input_type(el));
        if v.value_missing {
            return match ty {
                Some(InputType::Checkbox) => "Please check this box if you want to proceed.",
                Some(InputType::Radio) => "Please select one of these options.",
                Some(InputType::File) => "Please select a file.",
                _ if self.is_html(el, "select") => "Please select an item in the list.",
                _ => "Please fill out this field.",
            }
            .to_string();
        }
        if v.type_mismatch {
            return if ty == Some(InputType::Url) {
                "Please enter a URL."
            } else {
                "Please enter an email address."
            }
            .to_string();
        }
        if v.pattern_mismatch {
            return match self.attr(el, "title").filter(|t| !t.is_empty()) {
                Some(t) => format!("Please match the requested format: {t}."),
                None => "Please match the requested format.".to_string(),
            };
        }
        let len = utf16_len(&self.control_value(el));
        if v.too_long {
            let max = non_negative(self, el, "maxlength").unwrap_or(0);
            return format!("Please shorten this text to {max} characters or less (you are currently using {len} characters).");
        }
        if v.too_short {
            let min = non_negative(self, el, "minlength").unwrap_or(0);
            return format!(
                "Please use at least {min} characters (you are currently using {len} characters)."
            );
        }
        if v.range_underflow {
            return format!(
                "Please select a value that is no less than {}.",
                self.attr(el, "min").unwrap_or("")
            );
        }
        if v.range_overflow {
            return format!(
                "Please select a value that is no more than {}.",
                self.attr(el, "max").unwrap_or("")
            );
        }
        if v.step_mismatch {
            return "Please select a valid value.".to_string();
        }
        String::new()
    }

    /// Whether an input has range limitations and its value is outside them (CSS
    /// `:in-range`/`:out-of-range`); None if it has none.
    pub fn out_of_range(&self, el: NodeId) -> Option<bool> {
        if !self.is_html(el, "input") {
            return None;
        }
        let ty = self.input_type(el);
        step_params(ty)?;
        let has_limits = self.attr(el, "min").and_then(|m| to_number(ty, m)).is_some()
            || self.attr(el, "max").and_then(|m| to_number(ty, m)).is_some();
        if !has_limits {
            return None;
        }
        let v = self.validity(el);
        Some(self.is_validation_candidate(el) && (v.range_underflow || v.range_overflow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribute, Namespace};

    fn input(d: &mut Document, attrs: &[(&str, &str)]) -> NodeId {
        let attrs = attrs.iter().map(|(k, v)| Attribute::new(k, v)).collect();
        let el = d.create_element(Namespace::Html, "input", attrs);
        d.append_child(Document::ROOT, el);
        el
    }

    #[test]
    fn input_states() {
        let mut d = Document::new();
        let a = input(&mut d, &[("required", "")]);
        assert!(d.validity(a).value_missing);
        assert_eq!(d.validation_message(a), "Please fill out this field.");
        d.set_control_value(a, "x");
        assert!(d.is_valid_control(a));
        let e = input(&mut d, &[("type", "email"), ("value", "nope")]);
        assert!(d.validity(e).type_mismatch);
        d.set_control_value(e, "a@b.example");
        assert!(d.is_valid_control(e));
        let n =
            input(&mut d, &[("type", "number"), ("min", "2"), ("max", "10"), ("step", "2"), ("value", "5")]);
        let v = d.validity(n);
        assert!(v.step_mismatch && !v.range_underflow && !v.range_overflow);
        d.set_control_value(n, "12");
        assert!(d.validity(n).range_overflow);
        let dt = input(&mut d, &[("type", "date"), ("min", "2024-02-01"), ("value", "2024-01-31")]);
        assert!(d.validity(dt).range_underflow);
        let w = input(&mut d, &[("type", "week"), ("value", "2026-W53")]);
        assert!(to_number(InputType::Week, "2026-W53").is_some() && d.validity(w).valid());
        assert!(to_number(InputType::Week, "2025-W53").is_none());
        d.set_custom_validity(a, "nope");
        assert!(d.validity(a).custom_error);
        assert_eq!(d.validation_message(a), "nope");
        let ro = input(&mut d, &[("readonly", ""), ("required", "")]);
        assert!(!d.is_validation_candidate(ro));
    }
}

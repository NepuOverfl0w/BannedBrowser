//! JavaScript string values (ECMA-262 §6.1.4): immutable UTF-16 code units, possibly with
//! lone surrogates.
//!
//! A string is a flat slice or a rope: concatenating long strings makes a node that holds
//! both halves and flattens itself, once, the first time its code units are read. Building a
//! string with `+=` in a loop is then linear instead of quadratic. Reading a rope's length
//! doesn't flatten it. Flattening and dropping walk the tree with explicit stacks, so long
//! chains don't overflow the native stack.

use std::borrow::Borrow;
use std::cell::{OnceCell, RefCell};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

/// Concatenations shorter than this are copied flat (a node isn't worth it).
const ROPE_MIN: usize = 64;

#[derive(Clone)]
pub struct JsString(Rc<Repr>);

enum Repr {
    Flat(Box<[u16]>),
    Rope(Rope),
}

struct Rope {
    len: usize,
    flat: OnceCell<Box<[u16]>>,
    /// The halves, until the flat form exists.
    parts: RefCell<Option<(JsString, JsString)>>,
}

impl JsString {
    /// `a` followed by `b`.
    pub fn concat(a: &JsString, b: &JsString) -> JsString {
        if a.is_empty() {
            return b.clone();
        }
        if b.is_empty() {
            return a.clone();
        }
        let len = a.len() + b.len();
        if len < ROPE_MIN {
            let mut v = Vec::with_capacity(len);
            v.extend_from_slice(a);
            v.extend_from_slice(b);
            return v.into();
        }
        JsString(Rc::new(Repr::Rope(Rope {
            len,
            flat: OnceCell::new(),
            parts: RefCell::new(Some((a.clone(), b.clone()))),
        })))
    }

    /// The number of code units (without flattening).
    pub fn len(&self) -> usize {
        match &*self.0 {
            Repr::Flat(b) => b.len(),
            Repr::Rope(r) => r.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether both are the same string object (not just equal contents).
    pub fn ptr_eq(a: &JsString, b: &JsString) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    fn units(&self) -> &[u16] {
        match &*self.0 {
            Repr::Flat(b) => b,
            Repr::Rope(r) => r.flat.get_or_init(|| r.flatten()),
        }
    }
}

impl Rope {
    fn flatten(&self) -> Box<[u16]> {
        let mut out = Vec::with_capacity(self.len);
        let parts = self.parts.borrow_mut().take();
        let mut stack: Vec<JsString> = Vec::new();
        if let Some((a, b)) = parts {
            stack.push(b);
            stack.push(a);
        }
        while let Some(s) = stack.pop() {
            match &*s.0 {
                Repr::Flat(b) => out.extend_from_slice(b),
                Repr::Rope(r) => match r.flat.get() {
                    Some(f) => out.extend_from_slice(f),
                    None => match &*r.parts.borrow() {
                        Some((a, b)) => {
                            stack.push(b.clone());
                            stack.push(a.clone());
                        }
                        // Taken by a flatten that is filling `flat` right now: impossible
                        // for an acyclic tree, but stay safe.
                        None => out.extend_from_slice(r.flat.get().map(|f| &f[..]).unwrap_or(&[])),
                    },
                },
            }
        }
        out.into_boxed_slice()
    }
}

impl Drop for Rope {
    fn drop(&mut self) {
        // Unlink the tree iteratively: a deep chain would recurse once per node.
        let mut stack: Vec<JsString> = Vec::new();
        if let Some((a, b)) = self.parts.get_mut().take() {
            stack.push(a);
            stack.push(b);
        }
        while let Some(s) = stack.pop() {
            if let Ok(Repr::Rope(mut r)) = Rc::try_unwrap(s.0) {
                if let Some((a, b)) = r.parts.get_mut().take() {
                    stack.push(a);
                    stack.push(b);
                }
            }
        }
    }
}

impl Deref for JsString {
    type Target = [u16];
    fn deref(&self) -> &[u16] {
        self.units()
    }
}

impl AsRef<[u16]> for JsString {
    fn as_ref(&self) -> &[u16] {
        self.units()
    }
}

impl Borrow<[u16]> for JsString {
    fn borrow(&self) -> &[u16] {
        self.units()
    }
}

impl Default for JsString {
    fn default() -> JsString {
        JsString(Rc::new(Repr::Flat(Box::new([]))))
    }
}

impl From<Vec<u16>> for JsString {
    fn from(v: Vec<u16>) -> JsString {
        JsString(Rc::new(Repr::Flat(v.into_boxed_slice())))
    }
}

impl From<&[u16]> for JsString {
    fn from(v: &[u16]) -> JsString {
        JsString(Rc::new(Repr::Flat(v.into())))
    }
}

impl From<&Vec<u16>> for JsString {
    fn from(v: &Vec<u16>) -> JsString {
        v.as_slice().into()
    }
}

impl<const N: usize> From<[u16; N]> for JsString {
    fn from(v: [u16; N]) -> JsString {
        v[..].into()
    }
}

impl PartialEq for JsString {
    fn eq(&self, o: &JsString) -> bool {
        JsString::ptr_eq(self, o) || (self.len() == o.len() && self.units() == o.units())
    }
}

impl Eq for JsString {}

impl PartialOrd for JsString {
    fn partial_cmp(&self, o: &JsString) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for JsString {
    fn cmp(&self, o: &JsString) -> std::cmp::Ordering {
        self.units().cmp(o.units())
    }
}

impl Hash for JsString {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.units().hash(h)
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf16_lossy(self.units()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> JsString {
        x.encode_utf16().collect::<Vec<_>>().into()
    }

    #[test]
    fn ropes_flatten_and_compare() {
        let mut acc = JsString::default();
        let mut want = String::new();
        for i in 0..2000 {
            let piece = format!("{i},");
            acc = JsString::concat(&acc, &s(&piece));
            want.push_str(&piece);
        }
        assert_eq!(acc.len(), want.encode_utf16().count());
        assert_eq!(acc, s(&want));
        assert_eq!(String::from_utf16_lossy(&acc), want);
    }

    #[test]
    fn deep_chains_drop_and_flatten_without_recursion() {
        let one = s(&"x".repeat(ROPE_MIN));
        let mut acc = JsString::default();
        for _ in 0..300_000 {
            acc = JsString::concat(&acc, &one);
        }
        assert_eq!(acc.len(), 300_000 * ROPE_MIN);
        assert!(acc.iter().all(|&u| u == b'x' as u16));
        let mut acc2 = JsString::default();
        for _ in 0..300_000 {
            acc2 = JsString::concat(&one, &acc2);
        }
        drop(acc2);
    }
}

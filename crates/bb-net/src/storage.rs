//! Web storage (HTML §12.2 `localStorage`): per-origin string maps, shared by every page a
//! client loads, like its cookie jar (a private window's client has its own). The profile
//! saves and loads them; `generation` tells it when to.

use std::collections::HashMap;
use std::sync::Mutex;

/// One origin's storage area: (key, value) pairs in insertion order.
pub type Area = Vec<(String, String)>;

#[derive(Default)]
pub struct WebStorage {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    areas: HashMap<String, Area>,
    generation: u64,
}

impl WebStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The area of `origin` (serialized origin).
    pub fn area(&self, origin: &str) -> Area {
        self.lock().areas.get(origin).cloned().unwrap_or_default()
    }

    /// Replace `origin`'s area.
    pub fn set_area(&self, origin: &str, area: Area) {
        let mut g = self.lock();
        if area.is_empty() {
            g.areas.remove(origin);
        } else {
            g.areas.insert(origin.to_string(), area);
        }
        g.generation += 1;
    }

    /// Every (origin, key, value), for saving.
    pub fn all(&self) -> Vec<(String, String, String)> {
        let g = self.lock();
        let mut out = Vec::new();
        for (o, a) in &g.areas {
            for (k, v) in a {
                out.push((o.clone(), k.clone(), v.clone()));
            }
        }
        out
    }

    /// Add saved entries (at startup).
    pub fn load(&self, entries: Vec<(String, String, String)>) {
        let mut g = self.lock();
        for (o, k, v) in entries {
            let a = g.areas.entry(o).or_default();
            match a.iter_mut().find(|x| x.0 == k) {
                Some(x) => x.1 = v,
                None => a.push((k, v)),
            }
        }
    }

    /// Changes so far (the profile saves when this moves).
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }
}

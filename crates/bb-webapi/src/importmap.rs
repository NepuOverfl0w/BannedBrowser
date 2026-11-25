//! Import maps (HTML §8.1.5 "Module specifier resolution"): `<script type=importmap>`
//! parses its JSON into an import map (§8.1.5.3 "parse an import map string"), merged into
//! the window's (§8.1.5.3 "merge existing and new import maps"), and module specifiers
//! resolve through it (§8.1.5.1 "resolve a module specifier"). Workers' import maps stay
//! empty. The integrity section is parsed and kept, but nothing checks module integrity yet
//! (Subresource Integrity isn't implemented).

use bb_js::vm::heap::Kind;
use bb_js::vm::value::PropKey;
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

/// A specifier map: (specifier key, address), sorted by key in descending code unit order.
/// A None address is a blocked entry (a value that didn't resolve).
pub type SpecifierMap = Vec<(String, Option<String>)>;

#[derive(Debug, Default, Clone)]
pub struct ImportMap {
    pub imports: SpecifierMap,
    /// (scope prefix, its specifier map), sorted like a specifier map.
    pub scopes: Vec<(String, SpecifierMap)>,
    /// (module URL, integrity metadata).
    pub integrity: Vec<(String, String)>,
}

/// A resolved module set record (§8.1.5.1): what an earlier resolution depended on, so that
/// later import maps can't change it.
#[derive(Debug, Clone)]
struct Resolved {
    base: String,
    specifier: String,
    /// Whether the specifier was a URL with a special scheme, or not a URL at all.
    special_or_bare: bool,
}

/// The window's import map and resolved module set.
#[derive(Debug, Default)]
pub struct State {
    pub map: ImportMap,
    resolved: Vec<Resolved>,
}

/// A JSON value as Infra sees it, after JSON.parse.
enum J {
    Map(Vec<(String, J)>),
    Str(String),
    Other,
}

fn to_infra(vm: &mut Vm, v: &Value, depth: usize) -> JsResult<J> {
    match v {
        Value::String(s) => Ok(J::Str(bb_js::vm::value::to_rust(s))),
        Value::Object(o) if depth < 3 && !matches!(vm.heap.obj(*o).kind, Kind::Array) => {
            let mut out = Vec::new();
            for k in vm.own_property_keys(*o)? {
                if matches!(k, PropKey::Sym(_)) {
                    continue;
                }
                let v = vm.get(*o, &k)?;
                let j = to_infra(vm, &v, depth + 1)?;
                out.push((k.display(), j));
            }
            Ok(J::Map(out))
        }
        // Deeper than an import map goes: only its kind matters (not a string).
        Value::Object(o) if !matches!(vm.heap.obj(*o).kind, Kind::Array) => Ok(J::Map(Vec::new())),
        _ => Ok(J::Other),
    }
}

fn warn(msg: &str) {
    log::warn!("import map: {msg}");
}

/// §8.1.5.1 "resolve a URL-like module specifier".
pub fn resolve_url_like(specifier: &str, base: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return Url::parse_with_base(specifier, Some(base)).ok();
    }
    Url::parse(specifier).ok()
}

/// §8.1.5.3 "parse an import map string": a SyntaxError (from JSON.parse) or TypeError for
/// bad input, thrown in `vm`'s realm.
pub fn parse(vm: &mut Vm, input: &str, base: &Url) -> JsResult<ImportMap> {
    let g = vm.realm.global;
    let json = vm.get(g, &PropKey::lit("JSON"))?;
    let Value::Object(j) = json else { return vm.type_error("no JSON") };
    let parse = vm.get(j, &PropKey::lit("parse"))?;
    let parsed = vm.call(&parse, json, vec![crate::host::js(input)])?;
    let J::Map(top) = to_infra(vm, &parsed, 0)? else {
        return vm.type_error("An import map must be a JSON object");
    };
    let mut map = ImportMap::default();
    for (k, v) in top {
        match k.as_str() {
            "imports" => {
                let J::Map(m) = v else {
                    return vm.type_error("An import map's \"imports\" must be a JSON object");
                };
                map.imports = sort_and_normalize_specifiers(m, base);
            }
            "scopes" => {
                let J::Map(m) = v else {
                    return vm.type_error("An import map's \"scopes\" must be a JSON object");
                };
                let mut scopes = Vec::new();
                for (prefix, specifiers) in m {
                    let J::Map(specifiers) = specifiers else {
                        return vm.type_error(&format!("The scope \"{prefix}\" must be a JSON object"));
                    };
                    let Ok(url) = Url::parse_with_base(&prefix, Some(base)) else {
                        warn(&format!("the scope prefix \"{prefix}\" isn't a URL"));
                        continue;
                    };
                    let normalized = sort_and_normalize_specifiers(specifiers, base);
                    let key = url.href();
                    scopes.retain(|(p, _): &(String, SpecifierMap)| *p != key);
                    scopes.push((key, normalized));
                }
                sort_desc(&mut scopes);
                map.scopes = scopes;
            }
            "integrity" => {
                let J::Map(m) = v else {
                    return vm.type_error("An import map's \"integrity\" must be a JSON object");
                };
                for (referrer, value) in m {
                    let Some(url) = resolve_url_like(&referrer, base) else {
                        warn(&format!("the integrity key \"{referrer}\" isn't a URL"));
                        continue;
                    };
                    let J::Str(value) = value else {
                        warn(&format!("the integrity of \"{referrer}\" isn't a string"));
                        continue;
                    };
                    let key = url.href();
                    map.integrity.retain(|(u, _)| *u != key);
                    map.integrity.push((key, value));
                }
            }
            other => warn(&format!("unknown top-level key \"{other}\"")),
        }
    }
    Ok(map)
}

/// Descending code unit order of the keys (so a longer key sorts before its prefixes).
fn sort_desc<T>(v: &mut [(String, T)]) {
    v.sort_by(|a, b| {
        let (a, b): (Vec<u16>, Vec<u16>) = (a.0.encode_utf16().collect(), b.0.encode_utf16().collect());
        b.cmp(&a)
    });
}

/// §8.1.5.3 "normalize a specifier key".
fn normalize_key(key: &str, base: &Url) -> Option<String> {
    if key.is_empty() {
        warn("a specifier key is empty");
        return None;
    }
    Some(resolve_url_like(key, base).map(|u| u.href()).unwrap_or_else(|| key.to_string()))
}

/// §8.1.5.3 "sort and normalize a specifier map".
fn sort_and_normalize_specifiers(original: Vec<(String, J)>, base: &Url) -> SpecifierMap {
    let mut out: SpecifierMap = Vec::new();
    for (key, value) in original {
        let Some(nkey) = normalize_key(&key, base) else { continue };
        let address = match value {
            J::Str(v) => match resolve_url_like(&v, base) {
                Some(u) if key.ends_with('/') && !u.href().ends_with('/') => {
                    warn(&format!("\"{key}\" ends with '/' but its address \"{v}\" doesn't"));
                    None
                }
                Some(u) => Some(u.href()),
                None => {
                    warn(&format!("the address \"{v}\" of \"{key}\" isn't a URL"));
                    None
                }
            },
            _ => {
                warn(&format!("the address of \"{key}\" isn't a string"));
                None
            }
        };
        out.retain(|(k, _)| *k != nkey);
        out.push((nkey, address));
    }
    sort_desc(&mut out);
    out
}

fn is_special_or_bare(as_url: Option<&Url>) -> bool {
    as_url.is_none_or(Url::is_special)
}

/// §8.1.5.1 "resolve an imports match": Ok(None) when no entry applies.
fn resolve_imports_match(
    normalized: &str,
    as_url: Option<&Url>,
    map: &SpecifierMap,
) -> Result<Option<Url>, String> {
    for (key, result) in map {
        if key == normalized {
            let Some(r) = result else { return Err(format!("the import map blocks \"{key}\"")) };
            return Url::parse(r).map(Some).map_err(|_| format!("bad address for \"{key}\""));
        }
        if key.ends_with('/') && normalized.starts_with(key.as_str()) && is_special_or_bare(as_url) {
            let Some(r) = result else { return Err(format!("the import map blocks \"{key}\"")) };
            let after = &normalized[key.len()..];
            let base = Url::parse(r).map_err(|_| format!("bad address for \"{key}\""))?;
            let Ok(url) = Url::parse_with_base(after, Some(&base)) else {
                return Err(format!("\"{normalized}\" doesn't resolve against \"{r}\""));
            };
            if !url.href().starts_with(&base.href()) {
                return Err(format!("\"{normalized}\" backtracks above its prefix \"{key}\""));
            }
            return Ok(Some(url));
        }
    }
    Ok(None)
}

fn scope_applies(prefix: &str, base: &str) -> bool {
    prefix == base || (prefix.ends_with('/') && base.starts_with(prefix))
}

impl State {
    /// §8.1.5.1 "resolve a module specifier" against `base` (the referring script's base URL,
    /// or the document's), adding to the resolved module set.
    pub fn resolve(&mut self, specifier: &str, base: &Url) -> Result<Url, String> {
        let serialized_base = base.href();
        let as_url = resolve_url_like(specifier, base);
        let normalized = as_url.as_ref().map(Url::href).unwrap_or_else(|| specifier.to_string());
        let mut result = None;
        for (prefix, imports) in &self.map.scopes {
            if scope_applies(prefix, &serialized_base) {
                if let Some(u) = resolve_imports_match(&normalized, as_url.as_ref(), imports)? {
                    result = Some(u);
                    break;
                }
            }
        }
        if result.is_none() {
            result = resolve_imports_match(&normalized, as_url.as_ref(), &self.map.imports)?;
        }
        let Some(url) = result.or_else(|| as_url.clone()) else {
            return Err(format!(
                "the bare specifier \"{specifier}\" isn't mapped (relative specifiers start with '/', './' or '../')"
            ));
        };
        // (A repeated record would change nothing the merge does.)
        if !self.resolved.iter().any(|r| r.base == serialized_base && r.specifier == normalized) {
            self.resolved.push(Resolved {
                base: serialized_base,
                specifier: normalized,
                special_or_bare: is_special_or_bare(as_url.as_ref()),
            });
        }
        Ok(url)
    }

    /// §8.1.5.3 "merge existing and new import maps": entries the resolved module set
    /// already depended on, or that the existing map has, are dropped.
    pub fn merge(&mut self, new: ImportMap) {
        let ImportMap { imports: mut new_imports, scopes: new_scopes, integrity } = new;
        for (prefix, mut scope_imports) in new_scopes {
            for r in &self.resolved {
                if scope_applies(&prefix, &r.base) {
                    scope_imports.retain(|(key, _)| {
                        let hit = *key == r.specifier
                            || (key.ends_with('/')
                                && r.specifier.starts_with(key.as_str())
                                && r.special_or_bare);
                        if hit {
                            warn(&format!("\"{key}\" in scope \"{prefix}\" was already resolved; ignored"));
                        }
                        !hit
                    });
                }
            }
            match self.map.scopes.iter_mut().find(|(p, _)| *p == prefix) {
                Some((_, old)) => merge_specifier_maps(scope_imports, old),
                None => self.map.scopes.push((prefix, scope_imports)),
            }
        }
        sort_desc(&mut self.map.scopes);
        for (url, value) in integrity {
            if self.map.integrity.iter().any(|(u, _)| *u == url) {
                warn(&format!("the integrity of \"{url}\" is already set; ignored"));
                continue;
            }
            self.map.integrity.push((url, value));
        }
        // The spec's step drops a key that starts with a resolved specifier; a prefix key
        // that the resolved specifier starts with would change it too (as WPT's
        // not-overridden/prefix.html checks), so that is dropped as in the scopes step.
        for r in &self.resolved {
            new_imports.retain(|(key, _)| {
                let hit = key.starts_with(r.specifier.as_str())
                    || (key.ends_with('/') && r.specifier.starts_with(key.as_str()) && r.special_or_bare);
                if hit {
                    warn(&format!("\"{key}\" was already resolved; ignored"));
                }
                !hit
            });
        }
        merge_specifier_maps(new_imports, &mut self.map.imports);
    }
}

/// §8.1.5.3 "merge module specifier maps": the old map's entries win.
fn merge_specifier_maps(new: SpecifierMap, old: &mut SpecifierMap) {
    for (key, address) in new {
        if old.iter().any(|(k, _)| *k == key) {
            warn(&format!("\"{key}\" is already mapped; ignored"));
            continue;
        }
        old.push((key, address));
    }
    sort_desc(old);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(imports: &[(&str, Option<&str>)], scopes: &[(&str, &[(&str, Option<&str>)])]) -> State {
        let sm = |v: &[(&str, Option<&str>)]| {
            let mut m: SpecifierMap = v.iter().map(|(k, a)| (k.to_string(), a.map(str::to_string))).collect();
            sort_desc(&mut m);
            m
        };
        let mut s = State::default();
        s.map.imports = sm(imports);
        s.map.scopes = scopes.iter().map(|(p, m)| (p.to_string(), sm(m))).collect();
        sort_desc(&mut s.map.scopes);
        s
    }

    #[test]
    fn resolves_exact_prefix_and_scoped_entries() {
        let base = Url::parse("https://example.com/app/index.html").unwrap();
        let mut s = state(
            &[
                ("moment", Some("https://cdn.example/moment.js")),
                ("lodash/", Some("https://cdn.example/lodash/")),
                ("blocked", None),
                ("https://example.com/app/a.js", Some("https://example.com/app/b.js")),
            ],
            &[("https://example.com/app/", &[("moment", Some("https://example.com/app/m.js"))])],
        );
        let r = |s: &mut State, spec: &str, base: &Url| s.resolve(spec, base).map(|u| u.href());
        // The scope wins over the top-level imports for scripts under it.
        assert_eq!(r(&mut s, "moment", &base).unwrap(), "https://example.com/app/m.js");
        let other = Url::parse("https://example.com/other.js").unwrap();
        assert_eq!(r(&mut s, "moment", &other).unwrap(), "https://cdn.example/moment.js");
        assert_eq!(r(&mut s, "lodash/fp.js", &base).unwrap(), "https://cdn.example/lodash/fp.js");
        // URL-like specifiers are normalized before matching.
        assert_eq!(r(&mut s, "./a.js", &base).unwrap(), "https://example.com/app/b.js");
        assert!(r(&mut s, "blocked", &base).is_err());
        assert!(r(&mut s, "unmapped", &base).is_err());
        assert!(r(&mut s, "lodash/../../x.js", &base).is_err());
        assert_eq!(r(&mut s, "./c.js", &base).unwrap(), "https://example.com/app/c.js");
    }

    #[test]
    fn merging_keeps_earlier_resolutions() {
        let base = Url::parse("https://example.com/").unwrap();
        let mut s = state(&[("a", Some("https://example.com/a1.js"))], &[]);
        assert!(s.resolve("a", &base).is_ok());
        assert!(s.resolve("b", &base).is_err());
        s.merge(ImportMap {
            imports: vec![
                ("c".into(), Some("https://example.com/c.js".into())),
                ("b".into(), Some("https://example.com/b.js".into())),
                ("a".into(), Some("https://example.com/a2.js".into())),
            ],
            ..Default::default()
        });
        assert_eq!(s.resolve("a", &base).unwrap().href(), "https://example.com/a1.js");
        // "b" failed to resolve before, so it isn't in the resolved module set: it maps now.
        assert_eq!(s.resolve("b", &base).unwrap().href(), "https://example.com/b.js");
        assert_eq!(s.resolve("c", &base).unwrap().href(), "https://example.com/c.js");
    }
}

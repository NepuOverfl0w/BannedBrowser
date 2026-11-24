//! XML serialization (DOM Parsing and Serialization, "XML serialization" of a node): the
//! algorithm behind `XMLSerializer.serializeToString` and markup getters in XML documents.
//! Namespaces are kept by a namespace prefix map, declaring prefixes (`ns1`, `ns2`, …) where
//! the tree needs them.

use bb_dom::{Attribute, Document, Namespace, NodeData, NodeId};

use crate::{is_char, is_name, XMLNS_NS, XML_NS};

/// The node can't be serialized as well-formed XML ("require well-formed" was set).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotWellFormed(pub String);

type R<T> = Result<T, NotWellFormed>;

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";

fn bad<T>(msg: &str) -> R<T> {
    Err(NotWellFormed(msg.to_string()))
}

/// A namespace prefix map: namespace (None for the null namespace) → its prefixes.
#[derive(Clone, Default)]
struct PrefixMap(Vec<(Option<String>, Vec<String>)>);

impl PrefixMap {
    fn get(&self, ns: Option<&str>) -> Option<&Vec<String>> {
        self.0.iter().find(|(k, _)| k.as_deref() == ns).map(|(_, v)| v)
    }

    fn add(&mut self, ns: Option<&str>, prefix: &str) {
        match self.0.iter_mut().find(|(k, _)| k.as_deref() == ns) {
            Some((_, v)) => v.push(prefix.to_string()),
            None => self.0.push((ns.map(str::to_string), vec![prefix.to_string()])),
        }
    }

    /// "retrieve a preferred prefix string".
    fn preferred(&self, preferred: Option<&str>, ns: Option<&str>) -> Option<String> {
        let list = self.get(ns)?;
        if let Some(p) = preferred {
            if list.iter().any(|x| x == p) {
                return Some(p.to_string());
            }
        }
        list.last().cloned()
    }

    /// "found": whether `prefix` is among `ns`'s prefixes.
    fn found(&self, prefix: &str, ns: Option<&str>) -> bool {
        self.get(ns).is_some_and(|l| l.iter().any(|x| x == prefix))
    }
}

struct Ser<'a> {
    d: &'a Document,
    wf: bool,
    prefix_index: u32,
    out: String,
}

/// "produce an XML serialization" of node `id` of `d`.
pub fn serialize(d: &Document, id: NodeId, require_well_formed: bool) -> Result<String, NotWellFormed> {
    let mut s = Ser { d, wf: require_well_formed, prefix_index: 1, out: String::new() };
    let mut map = PrefixMap::default();
    map.add(Some(XML_NS), "xml");
    s.node(id, None, &mut map)?;
    Ok(s.out)
}

/// The children of `id`, serialized one after the other (the inner part of the fragment
/// serializing algorithm for XML documents).
pub fn serialize_children(
    d: &Document,
    id: NodeId,
    require_well_formed: bool,
) -> Result<String, NotWellFormed> {
    let mut s = Ser { d, wf: require_well_formed, prefix_index: 1, out: String::new() };
    let mut map = PrefixMap::default();
    map.add(Some(XML_NS), "xml");
    let target = d.template_of(id).unwrap_or(id);
    let kids: Vec<NodeId> = d.children(target).collect();
    for k in kids {
        s.node(k, None, &mut map)?;
    }
    Ok(s.out)
}

fn ns_str(ns: Namespace) -> Option<&'static str> {
    (ns != Namespace::None).then(|| ns.url())
}

/// "serializing an attribute value".
fn attr_value(v: &str, wf: bool) -> R<String> {
    if wf && v.chars().any(|c| !is_char(c)) {
        return bad("an attribute value has a character XML doesn't allow");
    }
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
    Ok(out)
}

fn is_void(local: &str) -> bool {
    matches!(
        local,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

impl Ser<'_> {
    fn node(&mut self, id: NodeId, ctx: Option<&str>, map: &mut PrefixMap) -> R<()> {
        let d = self.d;
        match &d.node(id).data {
            NodeData::Element(_) => self.element(id, ctx, map),
            NodeData::Document => {
                if self.wf && d.document_element().is_none() {
                    return bad("a document without a document element");
                }
                let kids: Vec<NodeId> = d.children(id).collect();
                for k in kids {
                    self.node(k, ctx, map)?;
                }
                Ok(())
            }
            NodeData::DocumentFragment => {
                let kids: Vec<NodeId> = d.children(id).collect();
                for k in kids {
                    self.node(k, ctx, map)?;
                }
                Ok(())
            }
            NodeData::Text(t) => {
                if self.wf && t.chars().any(|c| !is_char(c)) {
                    return bad("text has a character XML doesn't allow");
                }
                if d.is_cdata(id) {
                    if self.wf && t.contains("]]>") {
                        return bad("a CDATA section contains ']]>'");
                    }
                    self.out.push_str("<![CDATA[");
                    self.out.push_str(t);
                    self.out.push_str("]]>");
                    return Ok(());
                }
                for c in t.chars() {
                    match c {
                        '&' => self.out.push_str("&amp;"),
                        '<' => self.out.push_str("&lt;"),
                        '>' => self.out.push_str("&gt;"),
                        c => self.out.push(c),
                    }
                }
                Ok(())
            }
            NodeData::Comment(t) => {
                if self.wf && (t.chars().any(|c| !is_char(c)) || t.contains("--") || t.ends_with('-')) {
                    return bad("a comment can't be serialized as XML");
                }
                self.out.push_str("<!--");
                self.out.push_str(t);
                self.out.push_str("-->");
                Ok(())
            }
            NodeData::ProcessingInstruction { target, data } => {
                if self.wf
                    && (target.contains(':')
                        || target.eq_ignore_ascii_case("xml")
                        || data.chars().any(|c| !is_char(c))
                        || data.contains("?>"))
                {
                    return bad("a processing instruction can't be serialized as XML");
                }
                self.out.push_str("<?");
                self.out.push_str(target);
                self.out.push(' ');
                self.out.push_str(data);
                self.out.push_str("?>");
                Ok(())
            }
            NodeData::Doctype { name, public_id, system_id } => {
                if self.wf {
                    let pubid_ok = public_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || " \n\r-'()+,./:=?;!*#@$_%".contains(c));
                    if !pubid_ok {
                        return bad("a doctype's public identifier has characters XML doesn't allow");
                    }
                    if system_id.chars().any(|c| !is_char(c))
                        || (system_id.contains('"') && system_id.contains('\''))
                    {
                        return bad("a doctype's system identifier can't be serialized");
                    }
                }
                self.out.push_str("<!DOCTYPE ");
                self.out.push_str(name);
                if !public_id.is_empty() {
                    self.out.push_str(" PUBLIC \"");
                    self.out.push_str(public_id);
                    self.out.push('"');
                }
                if !system_id.is_empty() && public_id.is_empty() {
                    self.out.push_str(" SYSTEM");
                }
                if !system_id.is_empty() {
                    self.out.push_str(" \"");
                    self.out.push_str(system_id);
                    self.out.push('"');
                }
                self.out.push('>');
                Ok(())
            }
        }
    }

    /// "generate a prefix".
    fn generate(&mut self, map: &mut PrefixMap, ns: Option<&str>) -> String {
        let p = format!("ns{}", self.prefix_index);
        self.prefix_index += 1;
        map.add(ns, &p);
        p
    }

    /// "XML serializing an Element node".
    fn element(&mut self, id: NodeId, ctx: Option<&str>, outer: &mut PrefixMap) -> R<()> {
        let d = self.d;
        let Some(e) = d.element(id) else { return Ok(()) };
        // Step 1.
        if self.wf && (e.name.contains(':') || !is_name(&e.name)) {
            return bad("an element's local name can't be serialized as XML");
        }
        // Steps 2–9.
        let mut map = outer.clone();
        let mut local_prefixes: Vec<(String, String)> = Vec::new();
        let local_default = record_namespaces(&e.attrs, &mut map, &mut local_prefixes);
        let mut inherited: Option<String> = ctx.map(str::to_string);
        let mut ignore_ns_def = false;
        let ns = ns_str(e.ns);
        let local = e.name.as_str();
        let qualified: String;
        let mut markup = String::from("<");
        if inherited.as_deref() == ns {
            // Step 11: the element's namespace is the context's, so its own default
            // namespace declaration is redundant or inconsistent and dropped.
            if local_default.is_some() {
                ignore_ns_def = true;
            }
            qualified = if ns == Some(XML_NS) { format!("xml:{local}") } else { local.to_string() };
            markup.push_str(&qualified);
        } else {
            // Step 12.
            let mut prefix = e.prefix.clone();
            let mut candidate = map.preferred(prefix.as_deref(), ns);
            if prefix.as_deref() == Some("xmlns") {
                if self.wf {
                    return bad("an element can't have the prefix 'xmlns'");
                }
                candidate = prefix.clone();
            }
            if let Some(c) = candidate {
                qualified = format!("{c}:{local}");
                if let Some(ld) = &local_default {
                    if ld != XML_NS {
                        inherited = (!ld.is_empty()).then(|| ld.clone());
                    }
                }
                markup.push_str(&qualified);
            } else if let Some(p) = prefix.take() {
                let p = if local_prefixes.iter().any(|(k, _)| *k == p) {
                    self.generate(&mut map, ns)
                } else {
                    map.add(ns, &p);
                    p
                };
                qualified = format!("{p}:{local}");
                markup.push_str(&qualified);
                markup.push_str(" xmlns:");
                markup.push_str(&p);
                markup.push_str("=\"");
                markup.push_str(&attr_value(ns.unwrap_or(""), self.wf)?);
                markup.push('"');
                if let Some(ld) = &local_default {
                    inherited = (!ld.is_empty()).then(|| ld.clone());
                }
            } else if local_default.is_none() || local_default.as_deref() != Some(ns.unwrap_or("")) {
                ignore_ns_def = true;
                qualified = local.to_string();
                inherited = ns.map(str::to_string);
                markup.push_str(&qualified);
                markup.push_str(" xmlns=\"");
                markup.push_str(&attr_value(ns.unwrap_or(""), self.wf)?);
                markup.push('"');
            } else {
                qualified = local.to_string();
                inherited = ns.map(str::to_string);
                markup.push_str(&qualified);
            }
        }
        // Step 13.
        markup.push_str(&self.attributes(&e.attrs, &mut map, &local_prefixes, ignore_ns_def)?);
        // Steps 14–17.
        let target = d.template_of(id).unwrap_or(id);
        let no_children = d.first_child(target).is_none();
        let html = ns == Some(HTML_NS);
        if no_children && html && is_void(local) {
            markup.push_str(" />");
            self.out.push_str(&markup);
            return Ok(());
        }
        if no_children && !html {
            markup.push_str("/>");
            self.out.push_str(&markup);
            return Ok(());
        }
        markup.push('>');
        self.out.push_str(&markup);
        // Steps 18–19.
        let kids: Vec<NodeId> = d.children(target).collect();
        for k in kids {
            self.node(k, inherited.as_deref(), &mut map)?;
        }
        // Step 20.
        self.out.push_str("</");
        self.out.push_str(&qualified);
        self.out.push('>');
        Ok(())
    }

    /// "XML serialization of the attributes".
    fn attributes(
        &mut self,
        attrs: &[Attribute],
        map: &mut PrefixMap,
        local_prefixes: &[(String, String)],
        ignore_ns_def: bool,
    ) -> R<String> {
        let mut out = String::new();
        let mut seen: Vec<(Namespace, &str)> = Vec::new();
        for a in attrs {
            if self.wf && seen.iter().any(|(n, l)| *n == a.ns && *l == a.name) {
                return bad("an element has the same attribute twice");
            }
            seen.push((a.ns, &a.name));
            let ans = ns_str(a.ns);
            let mut candidate: Option<String> = None;
            // A null-namespace attribute named xmlns is a default namespace declaration too.
            if ans.is_none() && a.prefix.is_none() && a.name == "xmlns" && ignore_ns_def {
                continue;
            }
            if let Some(ans) = ans {
                candidate = map.preferred(a.prefix.as_deref(), Some(ans));
                if ans == XMLNS_NS {
                    let dup = match &a.prefix {
                        None => ignore_ns_def,
                        Some(_) => !local_prefixes.iter().any(|(k, v)| *k == a.name && *v == a.value),
                    };
                    if a.value == XML_NS || dup {
                        continue;
                    }
                    if self.wf && a.value == XMLNS_NS {
                        return bad("the XMLNS namespace can't be declared");
                    }
                    if self.wf && a.prefix.is_some() && a.value.is_empty() {
                        return bad("a namespace prefix can't be undeclared");
                    }
                    if a.prefix.as_deref() == Some("xmlns") {
                        candidate = Some("xmlns".to_string());
                    }
                } else if candidate.is_none() {
                    let p = self.generate(map, Some(ans));
                    out.push_str(" xmlns:");
                    out.push_str(&p);
                    out.push_str("=\"");
                    out.push_str(&attr_value(ans, self.wf)?);
                    out.push('"');
                    candidate = Some(p);
                }
            }
            out.push(' ');
            if let Some(c) = candidate {
                out.push_str(&c);
                out.push(':');
            }
            if self.wf && (a.name.contains(':') || !is_name(&a.name) || (a.name == "xmlns" && ans.is_none()))
            {
                return bad("an attribute's name can't be serialized as XML");
            }
            out.push_str(&a.name);
            out.push_str("=\"");
            out.push_str(&attr_value(&a.value, self.wf)?);
            out.push('"');
        }
        Ok(out)
    }
}

/// "recording the namespace information" for an element's attributes; returns the default
/// namespace its `xmlns` attribute declares, if any.
fn record_namespaces(
    attrs: &[Attribute],
    map: &mut PrefixMap,
    local: &mut Vec<(String, String)>,
) -> Option<String> {
    let mut default = None;
    for a in attrs {
        if a.ns == Namespace::None && a.prefix.is_none() && a.name == "xmlns" {
            default = Some(a.value.clone());
            continue;
        }
        if ns_str(a.ns) != Some(XMLNS_NS) {
            continue;
        }
        if a.prefix.is_none() {
            default = Some(a.value.clone());
            continue;
        }
        let prefix = a.name.as_str();
        let def = a.value.as_str();
        if def == XML_NS {
            continue;
        }
        let def_ns = (!def.is_empty()).then_some(def);
        if map.found(prefix, def_ns) {
            continue;
        }
        map.add(def_ns, prefix);
        local.push((prefix.to_string(), def.to_string()));
    }
    default
}

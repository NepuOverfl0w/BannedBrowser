//! DOM §4.2.3 mutation algorithms: pre-insert, insert, replace, remove, and adopting nodes
//! into another document (DOM §4.5 "adopt").

use bb_dom::{Document, NodeData, NodeId};
use bb_js::{JsResult, Value, Vm};

use crate::exception::throw;
use crate::host::{host, with_doc, with_doc_mut, Host, NodeRef};

fn is_parent_kind(d: &NodeData) -> bool {
    matches!(d, NodeData::Document | NodeData::DocumentFragment | NodeData::Element(_))
}

fn is_doctype(doc: &Document, id: NodeId) -> bool {
    matches!(doc.node(id).data, NodeData::Doctype { .. })
}

fn is_element(doc: &Document, id: NodeId) -> bool {
    doc.node(id).is_element()
}

/// Whether `a` is a host-including inclusive ancestor of `b` (DOM §4.2.2.1: a shadow root,
/// and a template's contents, count as children of their host).
pub fn is_inclusive_ancestor(doc: &Document, a: NodeId, b: NodeId) -> bool {
    let mut cur = Some(b);
    while let Some(c) = cur {
        if c == a {
            return true;
        }
        cur = doc.parent_or_host(c).or_else(|| template_host(doc, c));
    }
    false
}

/// The template whose contents fragment `frag` is.
fn template_host(doc: &Document, frag: NodeId) -> Option<NodeId> {
    if !matches!(doc.node(frag).data, NodeData::DocumentFragment) {
        return None;
    }
    // Templates are rare; a scan is fine.
    (0..doc.len() as u32).map(NodeId).find(|&t| doc.template_of(t) == Some(frag))
}

/// DOM §4.2.3 "ensure pre-insertion validity" (for insertion, `replace` is None) or the
/// replace checks (DOM §4.2.3 "replace" steps 1–6): the DOMException name on failure.
fn validity(
    docs: &[Document],
    node: NodeRef,
    parent: NodeRef,
    child: Option<NodeId>,
    replace: Option<NodeId>,
) -> Result<(), &'static str> {
    let pd = &docs[parent.doc as usize];
    let nd = &docs[node.doc as usize];
    if !is_parent_kind(&pd.node(parent.id).data) {
        return Err("HierarchyRequestError");
    }
    if node.doc == parent.doc && is_inclusive_ancestor(pd, node.id, parent.id) {
        return Err("HierarchyRequestError");
    }
    let reference = replace.or(child);
    if let Some(c) = reference {
        if pd.parent(c) != Some(parent.id) {
            return Err("NotFoundError");
        }
    }
    let ndata = &nd.node(node.id).data;
    if !matches!(
        ndata,
        NodeData::DocumentFragment
            | NodeData::Doctype { .. }
            | NodeData::Element(_)
            | NodeData::Text(_)
            | NodeData::Comment(_)
            | NodeData::ProcessingInstruction { .. }
    ) {
        return Err("HierarchyRequestError");
    }
    let parent_is_doc = matches!(pd.node(parent.id).data, NodeData::Document);
    if (matches!(ndata, NodeData::Text(_)) && parent_is_doc)
        || (matches!(ndata, NodeData::Doctype { .. }) && !parent_is_doc)
    {
        return Err("HierarchyRequestError");
    }
    if !parent_is_doc {
        return Ok(());
    }
    // The parent is a document: at most one element and one doctype, doctype first.
    let kids: Vec<NodeId> = pd.children(parent.id).collect();
    let others = |f: &dyn Fn(NodeId) -> bool| kids.iter().any(|&k| Some(k) != replace && f(k));
    let doctype_after = |c: NodeId| {
        let mut cur = pd.next_sibling(c);
        while let Some(x) = cur {
            if is_doctype(pd, x) {
                return true;
            }
            cur = pd.next_sibling(x);
        }
        false
    };
    let element_before = |c: NodeId| {
        let mut cur = pd.prev_sibling(c);
        while let Some(x) = cur {
            if is_element(pd, x) {
                return true;
            }
            cur = pd.prev_sibling(x);
        }
        false
    };
    match ndata {
        NodeData::DocumentFragment => {
            let fk: Vec<NodeId> = nd.children(node.id).collect();
            let elems = fk.iter().filter(|&&k| is_element(nd, k)).count();
            if elems > 1 || fk.iter().any(|&k| matches!(nd.node(k).data, NodeData::Text(_))) {
                return Err("HierarchyRequestError");
            }
            if elems == 1 {
                let bad = match replace {
                    Some(r) => others(&|k| is_element(pd, k)) || doctype_after(r),
                    None => {
                        kids.iter().any(|&k| is_element(pd, k))
                            || child.is_some_and(|c| is_doctype(pd, c) || doctype_after(c))
                    }
                };
                if bad {
                    return Err("HierarchyRequestError");
                }
            }
        }
        NodeData::Element(_) => {
            let bad = match replace {
                Some(r) => others(&|k| is_element(pd, k)) || doctype_after(r),
                None => {
                    kids.iter().any(|&k| is_element(pd, k))
                        || child.is_some_and(|c| is_doctype(pd, c) || doctype_after(c))
                }
            };
            if bad {
                return Err("HierarchyRequestError");
            }
        }
        NodeData::Doctype { .. } => {
            let bad = match replace {
                Some(r) => others(&|k| is_doctype(pd, k)) || element_before(r),
                None => {
                    kids.iter().any(|&k| is_doctype(pd, k))
                        || match child {
                            Some(c) => element_before(c),
                            None => kids.iter().any(|&k| is_element(pd, k)),
                        }
                }
            };
            if bad {
                return Err("HierarchyRequestError");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Throw unless `node` may be inserted into `parent` before `child`.
pub fn check_pre_insert(
    vm: &mut Vm,
    h: &Host,
    node: NodeRef,
    parent: NodeRef,
    child: Option<NodeRef>,
) -> JsResult<()> {
    let r = {
        let docs = match h.docs.try_borrow() {
            Ok(d) => d,
            Err(_) => return throw(vm, "InvalidStateError", "the document is busy"),
        };
        validity(&docs, node, parent, child.map(|c| c.id), None)
    };
    match r {
        Ok(()) => Ok(()),
        Err(name) => throw(vm, name, "the node can't be inserted here"),
    }
}

/// DOM §4.2.3 "pre-insert" `node` into `parent` before `child`; returns the node.
pub fn pre_insert(
    vm: &mut Vm,
    h: &Host,
    node: NodeRef,
    parent: NodeRef,
    child: Option<NodeRef>,
) -> JsResult<NodeRef> {
    if let Some(c) = child {
        if c.doc != parent.doc {
            return throw(vm, "NotFoundError", "the reference node is not a child of this node");
        }
    }
    let r = {
        let docs = match h.docs.try_borrow() {
            Ok(d) => d,
            Err(_) => return throw(vm, "InvalidStateError", "the document is busy"),
        };
        validity(&docs, node, parent, child.map(|c| c.id), None)
    };
    if let Err(name) = r {
        return throw(vm, name, "the node can't be inserted here");
    }
    let mut reference = child.map(|c| c.id);
    if reference == Some(node.id) && node.doc == parent.doc {
        reference = with_doc(vm, h, parent.doc, |d| d.next_sibling(node.id))?;
    }
    insert(vm, h, node, parent, reference, false)?;
    Ok(node)
}

/// DOM §4.2.3 "insert" `node` into `parent` before `child` (validity already checked);
/// returns the inserted nodes (where they are now).
pub fn insert(
    vm: &mut Vm,
    h: &Host,
    node: NodeRef,
    parent: NodeRef,
    child: Option<NodeId>,
    suppress: bool,
) -> JsResult<Vec<NodeRef>> {
    crate::ce::enter(h);
    let r = insert_steps(vm, h, node, parent, child, suppress);
    if let Ok(nodes) = &r {
        if parent.doc == 0 {
            crate::ce::inserted(vm, h, nodes);
        }
        // §4.10.17.3 form owners of form-associated custom elements (after connectedCallback
        // is queued).
        crate::internals::forms_changed(vm, h, parent.doc);
    }
    crate::ce::leave(vm, h);
    r
}

fn insert_steps(
    vm: &mut Vm,
    h: &Host,
    node: NodeRef,
    parent: NodeRef,
    child: Option<NodeId>,
    suppress: bool,
) -> JsResult<Vec<NodeRef>> {
    // Steps 1–3.
    let is_frag = with_doc(vm, h, node.doc, |d| matches!(d.node(node.id).data, NodeData::DocumentFragment))?;
    let nodes: Vec<NodeRef> = if is_frag {
        let kids = with_doc(vm, h, node.doc, |d| d.children(node.id).collect::<Vec<_>>())?;
        kids.into_iter().map(|k| node.with(k)).collect()
    } else {
        vec![node]
    };
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    // Step 4.
    if is_frag {
        for k in &nodes {
            remove(vm, h, *k, true)?;
        }
        crate::mutation::tree_record(vm, h, node, Vec::new(), nodes.clone(), None, None)?;
    }
    // Steps 5–6.
    let prev = with_doc(vm, h, parent.doc, |d| match child {
        Some(c) => {
            crate::range::inserting(h, parent, crate::range::index(d, c), nodes.len() as u32);
            d.prev_sibling(c)
        }
        None => d.last_child(parent.id),
    })?;
    // Step 7.
    let mut inserted = Vec::with_capacity(nodes.len());
    for n in nodes {
        let n = adopt(vm, h, n, parent.doc)?;
        with_doc_mut(vm, h, parent.doc, |d| d.insert_before(parent.id, n.id, child))?;
        inserted.push(n);
    }
    // Steps 7.4–7.5 and 7.6: slots of the trees that changed are reassigned.
    crate::shadow::tree_changed(vm, h, parent);
    // HTML §4.11.1 details insertion steps.
    crate::details::inserted(vm, h, &inserted)?;
    // Step 8.
    if !suppress {
        crate::mutation::tree_record(
            vm,
            h,
            parent,
            inserted.clone(),
            Vec::new(),
            prev.map(|p| parent.with(p)),
            child.map(|c| parent.with(c)),
        )?;
    }
    // HTML §4.12.1 "node insertion steps": connected script elements are prepared (in tree
    // order); a script whose text was added to a connected script element is too.
    if parent.doc == 0 {
        let scripts: Vec<NodeRef> = with_doc(vm, h, 0, |d| {
            let connected = |x: NodeId| d.is_connected(x);
            let mut out: Vec<NodeRef> = Vec::new();
            for n in &inserted {
                if !connected(n.id) {
                    continue;
                }
                for x in d.shadow_including_descendants(n.id) {
                    if d.is_html(x, "script") {
                        out.push(n.with(x));
                    }
                }
            }
            if d.is_html(parent.id, "script") && connected(parent.id) {
                out.push(parent);
            }
            out
        })?;
        for s in scripts {
            crate::loader::script_inserted(vm, h, s);
        }
        // HTML §4.8.5: connected iframes and frames get their child navigables.
        if h.frames.borrow().nested {
            let hr = host(vm)?;
            if std::ptr::eq(&*hr, h) {
                let roots: Vec<NodeId> = inserted.iter().map(|n| n.id).collect();
                crate::frames::nodes_inserted(vm, &hr, &roots)?;
            }
        }
    }
    Ok(inserted)
}

/// DOM §4.2.3 "append".
pub fn append(vm: &mut Vm, h: &Host, node: NodeRef, parent: NodeRef) -> JsResult<NodeRef> {
    pre_insert(vm, h, node, parent, None)
}

/// DOM §4.2.3 "replace" `child` with `node` in `parent`.
pub fn replace(vm: &mut Vm, h: &Host, child: NodeRef, node: NodeRef, parent: NodeRef) -> JsResult<NodeRef> {
    if child.doc != parent.doc {
        return throw(vm, "NotFoundError", "the node to replace is not a child of this node");
    }
    let r = {
        let docs = match h.docs.try_borrow() {
            Ok(d) => d,
            Err(_) => return throw(vm, "InvalidStateError", "the document is busy"),
        };
        validity(&docs, node, parent, None, Some(child.id))
    };
    if let Err(name) = r {
        return throw(vm, name, "the node can't replace that child");
    }
    // Steps 7–9.
    let (mut reference, mut prev) =
        with_doc(vm, h, parent.doc, |d| (d.next_sibling(child.id), d.prev_sibling(child.id)))?;
    if reference == Some(node.id) && node.doc == parent.doc {
        reference = with_doc(vm, h, parent.doc, |d| d.next_sibling(node.id))?;
    }
    if prev == Some(node.id) && node.doc == parent.doc {
        prev = with_doc(vm, h, parent.doc, |d| d.prev_sibling(node.id))?;
    }
    // Step 11.
    let node = adopt(vm, h, node, parent.doc)?;
    // Steps 10–11.
    let mut removed = Vec::new();
    if with_doc(vm, h, child.doc, |d| d.parent(child.id).is_some())? {
        removed.push(child);
        remove(vm, h, child, true)?;
    }
    // Steps 12–14.
    let nodes = insert(vm, h, node, parent, reference, true)?;
    crate::mutation::tree_record(
        vm,
        h,
        parent,
        nodes,
        removed,
        prev.map(|p| parent.with(p)),
        reference.map(|r| parent.with(r)),
    )?;
    Ok(child)
}

/// DOM §4.2.3 "pre-remove".
pub fn pre_remove(vm: &mut Vm, h: &Host, child: NodeRef, parent: NodeRef) -> JsResult<NodeRef> {
    let ok =
        child.doc == parent.doc && with_doc(vm, h, parent.doc, |d| d.parent(child.id) == Some(parent.id))?;
    if !ok {
        return throw(vm, "NotFoundError", "the node is not a child of this node");
    }
    remove(vm, h, child, false)?;
    Ok(child)
}

/// DOM §4.2.3 "remove" `node` from its parent (if it has one).
pub fn remove(vm: &mut Vm, h: &Host, node: NodeRef, suppress: bool) -> JsResult<()> {
    crate::ce::enter(h);
    // Step 15 (before the node leaves): its custom elements are disconnected.
    crate::ce::removing(vm, h, node);
    let r = remove_steps(vm, h, node, suppress);
    crate::ce::leave(vm, h);
    r
}

fn remove_steps(vm: &mut Vm, h: &Host, node: NodeRef, suppress: bool) -> JsResult<()> {
    let info = with_doc(vm, h, node.doc, |d| {
        let p = d.parent(node.id)?;
        let i = crate::range::index(d, node.id);
        // Steps 4–7: live ranges and node iterators.
        crate::range::removing(h, d, node, node.with(p), i);
        crate::traversal::pre_removing(h, d, node);
        Some((node.with(p), d.prev_sibling(node.id), d.next_sibling(node.id)))
    })?;
    let Some((parent, prev, next)) = info else { return Ok(()) };
    // Step 10.
    with_doc_mut(vm, h, node.doc, |d| d.detach(node.id))?;
    // Steps 12–13: slot reassignment and fallback content changes.
    crate::shadow::tree_changed(vm, h, parent);
    crate::internals::forms_changed(vm, h, node.doc);
    // HTML §4.8.5 iframe "removing steps": frames whose containers left go away.
    if node.doc == 0 {
        crate::frames::nodes_removed(vm, h);
    }
    // Steps 14–15.
    crate::mutation::add_transient_observers(vm, h, parent, node)?;
    if !suppress {
        crate::mutation::tree_record(
            vm,
            h,
            parent,
            Vec::new(),
            vec![node],
            prev.map(|p| node.with(p)),
            next.map(|n| node.with(n)),
        )?;
    }
    Ok(())
}

/// Replace all children of `parent` with `node` (DOM §4.2.3 "replace all").
pub fn replace_all(vm: &mut Vm, h: &Host, node: Option<NodeRef>, parent: NodeRef) -> JsResult<()> {
    crate::ce::enter(h);
    let r = replace_all_steps(vm, h, node, parent);
    crate::ce::leave(vm, h);
    r
}

fn replace_all_steps(vm: &mut Vm, h: &Host, node: Option<NodeRef>, parent: NodeRef) -> JsResult<()> {
    let kids = with_doc(vm, h, parent.doc, |d| d.children(parent.id).collect::<Vec<_>>())?;
    let removed: Vec<NodeRef> = kids.into_iter().map(|k| parent.with(k)).collect();
    for k in &removed {
        remove(vm, h, *k, true)?;
    }
    let added = match node {
        Some(n) => insert(vm, h, n, parent, None, true)?,
        None => Vec::new(),
    };
    crate::mutation::tree_record(vm, h, parent, added, removed, None, None)
}

/// DOM §4.5 "adopt" `node` into document `doc`: it is removed from its parent; nodes live in
/// their document's arena, so a node from another document is copied over, and its wrappers
/// (and those of its descendants, and what observes them) now point at the copies. Returns
/// where the node is now.
pub fn adopt(vm: &mut Vm, h: &Host, node: NodeRef, doc: u32) -> JsResult<NodeRef> {
    crate::ce::enter(h);
    let r = adopt_steps(vm, h, node, doc);
    crate::ce::leave(vm, h);
    r
}

fn adopt_steps(vm: &mut Vm, h: &Host, node: NodeRef, doc: u32) -> JsResult<NodeRef> {
    remove(vm, h, node, false)?;
    if node.doc == doc {
        return Ok(node);
    }
    let mut docs = match h.docs.try_borrow_mut() {
        Ok(d) => d,
        Err(_) => return throw(vm, "InvalidStateError", "the document is busy"),
    };
    if doc as usize >= docs.len() || node.doc as usize >= docs.len() {
        drop(docs);
        return vm.type_error("no such document");
    }
    let (copy, pairs) = {
        let (src, dst) = two_docs(&mut docs, node.doc, doc);
        dst.import_node(src, node.id, true)
    };
    drop(docs);
    h.mark_dirty();
    let packed: Vec<(u64, u64)> =
        pairs.iter().map(|&(old, new)| (node.with(old).pack(), NodeRef { doc, id: new }.pack())).collect();
    // Move wrappers to the copies, keeping identity (and anything stored on them).
    {
        let mut wrappers = h.wrappers.borrow_mut();
        for &(ok, nk) in &packed {
            if let Some(o) = wrappers.remove(&ok) {
                if let Some(ho) = vm.host_obj_mut(o) {
                    ho.id = nk;
                }
                wrappers.insert(nk, o);
            }
        }
    }
    h.mo.borrow_mut().remap(&packed);
    {
        let mut started = h.started_scripts.borrow_mut();
        for &(ok, nk) in &packed {
            if started.remove(&ok) {
                started.insert(nk);
            }
        }
    }
    crate::range::remap(h, &packed);
    crate::traversal::remap(h, &packed);
    crate::ce::adopted(vm, h, &packed, node.doc, doc);
    crate::shadow::adopted(vm, h, &packed, node.doc, doc);
    h.internals.borrow_mut().remap(vm, &packed);
    // Static node lists (mutation records' addedNodes, …) follow too.
    let map: std::collections::HashMap<u64, u64> = packed.iter().copied().collect();
    for c in h.collections.borrow_mut().iter_mut() {
        if let crate::host::CollKind::Static(list) = c {
            for n in list.iter_mut() {
                if let Some(x) = map.get(&n.pack()) {
                    *n = NodeRef::unpack(*x);
                }
            }
        }
    }
    Ok(NodeRef { doc, id: copy })
}

fn two_docs(docs: &mut [Document], a: u32, b: u32) -> (&Document, &mut Document) {
    let (a, b) = (a as usize, b as usize);
    if a < b {
        let (l, r) = docs.split_at_mut(b);
        (&l[a], &mut r[0])
    } else {
        let (l, r) = docs.split_at_mut(a);
        (&r[0], &mut l[b])
    }
}

/// Convert a `(Node or DOMString)...` argument list into one node (DOM §4.2.6 "convert
/// nodes into a node"): strings become Text nodes, several nodes a fragment.
pub fn nodes_into_node(vm: &mut Vm, h: &Host, doc: u32, args: &[Value]) -> JsResult<NodeRef> {
    let mut nodes = Vec::with_capacity(args.len());
    for a in args {
        match crate::host::node_of(vm, a) {
            Some(n) => nodes.push(n),
            None => {
                let s = crate::host::rstr(vm, a)?;
                let id = with_doc_mut(vm, h, doc, |d| d.create_text(&s))?;
                nodes.push(NodeRef { doc, id });
            }
        }
    }
    if nodes.len() == 1 {
        return Ok(nodes[0]);
    }
    let frag = with_doc_mut(vm, h, doc, |d| d.create_fragment())?;
    let frag = NodeRef { doc, id: frag };
    for n in nodes {
        append(vm, h, n, frag)?;
    }
    Ok(frag)
}

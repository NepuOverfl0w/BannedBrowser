//! The interfaces (WebIDL §3.7): their inheritance, interface objects and prototypes.
//!
//! A platform object's `class` in bb-js is a [`C`]; `implements` follows the parent chain.

use bb_dom::{Document, Namespace, NodeData, NodeId};
use bb_js::vm::heap::{NativeCall, NativeFn, ObjRef, Prop};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::host::Host;

macro_rules! parent {
    (None) => {
        None
    };
    ($p:ident) => {
        Some(C::$p)
    };
}

macro_rules! interfaces {
    ($($c:ident : $parent:tt;)*) => {
        /// Interfaces, parents before children.
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        #[repr(u32)]
        pub enum C { $($c),* }
        pub const ALL: &[C] = &[$(C::$c),*];
        const PARENTS: &[Option<C>] = &[$(parent!($parent)),*];
        const NAMES: &[&str] = &[$(stringify!($c)),*];
    };
}

interfaces! {
    EventTarget: None;
    Node: EventTarget;
    Document: Node;
    HTMLDocument: Document;
    XMLDocument: Document;
    DocumentType: Node;
    DocumentFragment: Node;
    ShadowRoot: DocumentFragment;
    CharacterData: Node;
    Text: CharacterData;
    CDATASection: Text;
    Comment: CharacterData;
    ProcessingInstruction: CharacterData;
    Attr: Node;
    Element: Node;
    HTMLElement: Element;
    HTMLUnknownElement: HTMLElement;
    HTMLHtmlElement: HTMLElement;
    HTMLHeadElement: HTMLElement;
    HTMLTitleElement: HTMLElement;
    HTMLBaseElement: HTMLElement;
    HTMLLinkElement: HTMLElement;
    HTMLMetaElement: HTMLElement;
    HTMLStyleElement: HTMLElement;
    HTMLBodyElement: HTMLElement;
    HTMLHeadingElement: HTMLElement;
    HTMLParagraphElement: HTMLElement;
    HTMLHRElement: HTMLElement;
    HTMLPreElement: HTMLElement;
    HTMLQuoteElement: HTMLElement;
    HTMLOListElement: HTMLElement;
    HTMLUListElement: HTMLElement;
    HTMLMenuElement: HTMLElement;
    HTMLLIElement: HTMLElement;
    HTMLDListElement: HTMLElement;
    HTMLDivElement: HTMLElement;
    HTMLAnchorElement: HTMLElement;
    HTMLDataElement: HTMLElement;
    HTMLTimeElement: HTMLElement;
    HTMLSpanElement: HTMLElement;
    HTMLBRElement: HTMLElement;
    HTMLModElement: HTMLElement;
    HTMLPictureElement: HTMLElement;
    HTMLSourceElement: HTMLElement;
    HTMLImageElement: HTMLElement;
    HTMLIFrameElement: HTMLElement;
    HTMLEmbedElement: HTMLElement;
    HTMLObjectElement: HTMLElement;
    HTMLMediaElement: HTMLElement;
    HTMLVideoElement: HTMLMediaElement;
    HTMLAudioElement: HTMLMediaElement;
    HTMLTrackElement: HTMLElement;
    HTMLMapElement: HTMLElement;
    HTMLAreaElement: HTMLElement;
    HTMLTableElement: HTMLElement;
    HTMLTableCaptionElement: HTMLElement;
    HTMLTableColElement: HTMLElement;
    HTMLTableSectionElement: HTMLElement;
    HTMLTableRowElement: HTMLElement;
    HTMLTableCellElement: HTMLElement;
    HTMLFormElement: HTMLElement;
    HTMLLabelElement: HTMLElement;
    HTMLInputElement: HTMLElement;
    HTMLButtonElement: HTMLElement;
    HTMLSelectElement: HTMLElement;
    HTMLDataListElement: HTMLElement;
    HTMLOptGroupElement: HTMLElement;
    HTMLOptionElement: HTMLElement;
    HTMLTextAreaElement: HTMLElement;
    HTMLOutputElement: HTMLElement;
    HTMLProgressElement: HTMLElement;
    HTMLMeterElement: HTMLElement;
    HTMLFieldSetElement: HTMLElement;
    HTMLLegendElement: HTMLElement;
    HTMLDetailsElement: HTMLElement;
    HTMLDialogElement: HTMLElement;
    HTMLScriptElement: HTMLElement;
    HTMLTemplateElement: HTMLElement;
    HTMLSlotElement: HTMLElement;
    HTMLCanvasElement: HTMLElement;
    HTMLFrameSetElement: HTMLElement;
    HTMLFrameElement: HTMLElement;
    HTMLDirectoryElement: HTMLElement;
    HTMLFontElement: HTMLElement;
    HTMLMarqueeElement: HTMLElement;
    HTMLParamElement: HTMLElement;
    SVGElement: Element;
    MathMLElement: Element;
    NodeList: None;
    HTMLCollection: None;
    DOMTokenList: None;
    NamedNodeMap: None;
    DOMStringMap: None;
    DOMImplementation: None;
    DOMException: None;
    Event: None;
    CustomEvent: Event;
    UIEvent: Event;
    MouseEvent: UIEvent;
    KeyboardEvent: UIEvent;
    FocusEvent: UIEvent;
    ErrorEvent: Event;
    Window: EventTarget;
    CSSStyleDeclaration: None;
    DOMRectReadOnly: None;
    DOMRect: DOMRectReadOnly;
    DOMRectList: None;
    DOMPointReadOnly: None;
    DOMPoint: DOMPointReadOnly;
    DOMQuad: None;
    DOMMatrixReadOnly: None;
    DOMMatrix: DOMMatrixReadOnly;
    URL: None;
    URLSearchParams: None;
    Headers: None;
    Request: None;
    Response: None;
    AbortController: None;
    AbortSignal: EventTarget;
    XMLHttpRequestEventTarget: EventTarget;
    XMLHttpRequest: XMLHttpRequestEventTarget;
    XMLHttpRequestUpload: XMLHttpRequestEventTarget;
    ProgressEvent: Event;
    TextEncoder: None;
    TextDecoder: None;
    Storage: None;
    MutationObserver: None;
    MutationRecord: None;
    AbstractRange: None;
    StaticRange: AbstractRange;
    Range: AbstractRange;
    NodeIterator: None;
    TreeWalker: None;
    Selection: None;
    DOMParser: None;
    XMLSerializer: None;
    Blob: None;
    File: Blob;
    FileReader: EventTarget;
    FormData: None;
    FormDataEvent: Event;
    SubmitEvent: Event;
    ToggleEvent: Event;
    CustomElementRegistry: None;
    ElementInternals: None;
    ValidityState: None;
    CustomStateSet: None;
    Crypto: None;
    MessageEvent: Event;
    Worker: EventTarget;
    WorkerGlobalScope: EventTarget;
    DedicatedWorkerGlobalScope: WorkerGlobalScope;
    WorkerLocation: None;
    WorkerNavigator: None;
    MessageChannel: None;
    MessagePort: EventTarget;
    BroadcastChannel: EventTarget;
    CanvasRenderingContext2D: None;
    CanvasGradient: None;
    CanvasPattern: None;
    ImageData: None;
    TextMetrics: None;
    Path2D: None;
    ImageBitmap: None;
}

pub fn name(c: C) -> &'static str {
    NAMES[c as usize]
}

pub fn parent(c: C) -> Option<C> {
    PARENTS[c as usize]
}

pub fn from_u32(c: u32) -> Option<C> {
    ALL.get(c as usize).copied()
}

/// Whether an object of class `c` implements interface `iface`.
pub fn implements(c: u32, iface: C) -> bool {
    let mut cur = from_u32(c);
    while let Some(x) = cur {
        if x == iface {
            return true;
        }
        cur = parent(x);
    }
    false
}

/// The interface of an HTML element (HTML §3.2.2 "element interface").
pub fn html_class(name: &str) -> C {
    match name {
        "html" => C::HTMLHtmlElement,
        "head" => C::HTMLHeadElement,
        "title" => C::HTMLTitleElement,
        "base" => C::HTMLBaseElement,
        "link" => C::HTMLLinkElement,
        "meta" => C::HTMLMetaElement,
        "style" => C::HTMLStyleElement,
        "body" => C::HTMLBodyElement,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => C::HTMLHeadingElement,
        "p" => C::HTMLParagraphElement,
        "hr" => C::HTMLHRElement,
        "pre" | "listing" | "xmp" => C::HTMLPreElement,
        "blockquote" | "q" => C::HTMLQuoteElement,
        "ol" => C::HTMLOListElement,
        "ul" => C::HTMLUListElement,
        "menu" => C::HTMLMenuElement,
        "li" => C::HTMLLIElement,
        "dl" => C::HTMLDListElement,
        "div" => C::HTMLDivElement,
        "a" => C::HTMLAnchorElement,
        "data" => C::HTMLDataElement,
        "time" => C::HTMLTimeElement,
        "span" => C::HTMLSpanElement,
        "br" => C::HTMLBRElement,
        "ins" | "del" => C::HTMLModElement,
        "picture" => C::HTMLPictureElement,
        "source" => C::HTMLSourceElement,
        "img" => C::HTMLImageElement,
        "iframe" => C::HTMLIFrameElement,
        "embed" => C::HTMLEmbedElement,
        "object" => C::HTMLObjectElement,
        "video" => C::HTMLVideoElement,
        "audio" => C::HTMLAudioElement,
        "track" => C::HTMLTrackElement,
        "map" => C::HTMLMapElement,
        "area" => C::HTMLAreaElement,
        "table" => C::HTMLTableElement,
        "caption" => C::HTMLTableCaptionElement,
        "colgroup" | "col" => C::HTMLTableColElement,
        "tbody" | "thead" | "tfoot" => C::HTMLTableSectionElement,
        "tr" => C::HTMLTableRowElement,
        "td" | "th" => C::HTMLTableCellElement,
        "form" => C::HTMLFormElement,
        "label" => C::HTMLLabelElement,
        "input" => C::HTMLInputElement,
        "button" => C::HTMLButtonElement,
        "select" => C::HTMLSelectElement,
        "datalist" => C::HTMLDataListElement,
        "optgroup" => C::HTMLOptGroupElement,
        "option" => C::HTMLOptionElement,
        "textarea" => C::HTMLTextAreaElement,
        "output" => C::HTMLOutputElement,
        "progress" => C::HTMLProgressElement,
        "meter" => C::HTMLMeterElement,
        "fieldset" => C::HTMLFieldSetElement,
        "legend" => C::HTMLLegendElement,
        "details" => C::HTMLDetailsElement,
        "dialog" => C::HTMLDialogElement,
        "script" => C::HTMLScriptElement,
        "template" => C::HTMLTemplateElement,
        "slot" => C::HTMLSlotElement,
        "canvas" => C::HTMLCanvasElement,
        "frameset" => C::HTMLFrameSetElement,
        "frame" => C::HTMLFrameElement,
        "dir" => C::HTMLDirectoryElement,
        "font" => C::HTMLFontElement,
        "marquee" => C::HTMLMarqueeElement,
        "param" => C::HTMLParamElement,
        // Elements of HTML's index whose interface is HTMLElement itself.
        "article" | "section" | "nav" | "aside" | "hgroup" | "header" | "footer" | "address" | "dt"
        | "dd" | "figure" | "figcaption" | "main" | "search" | "em" | "strong" | "small" | "s" | "cite"
        | "dfn" | "abbr" | "ruby" | "rt" | "rp" | "code" | "var" | "samp" | "kbd" | "sub" | "sup" | "i"
        | "b" | "u" | "mark" | "bdi" | "bdo" | "wbr" | "summary" | "noscript" | "acronym" | "basefont"
        | "big" | "center" | "nobr" | "noembed" | "noframes" | "plaintext" | "rb" | "rtc" | "strike"
        | "tt" => C::HTMLElement,
        n if is_valid_custom_element_name(n) => C::HTMLElement,
        _ => C::HTMLUnknownElement,
    }
}

/// HTML §4.13.2 valid custom element name (the parts that matter for the interface).
pub fn is_valid_custom_element_name(n: &str) -> bool {
    let mut cs = n.chars();
    matches!(cs.next(), Some('a'..='z'))
        && n.contains('-')
        && !n.chars().any(|c| c.is_ascii_uppercase())
        && !matches!(
            n,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
}

/// The interface of a node.
pub fn node_class(doc: &Document, id: NodeId) -> C {
    match &doc.node(id).data {
        NodeData::Document => C::Document,
        NodeData::DocumentFragment if doc.is_shadow_root(id) => C::ShadowRoot,
        NodeData::DocumentFragment => C::DocumentFragment,
        NodeData::Doctype { .. } => C::DocumentType,
        NodeData::Element(e) => match e.ns {
            Namespace::Html => html_class(&e.name),
            Namespace::Svg => C::SVGElement,
            Namespace::MathMl => C::MathMLElement,
            _ => C::Element,
        },
        NodeData::Text(_) if doc.is_cdata(id) => C::CDATASection,
        NodeData::Text(_) => C::Text,
        NodeData::Comment(_) => C::Comment,
        NodeData::ProcessingInstruction { .. } => C::ProcessingInstruction,
    }
}

fn illegal(vm: &mut Vm, _c: NativeCall) -> JsResult<Value> {
    vm.type_error("Illegal constructor")
}

/// Constructors of the interfaces that have one.
fn constructor(c: C) -> Option<(NativeFn, u32)> {
    Some(match c {
        C::EventTarget => (crate::events::event_target_ctor, 0),
        C::Event => (crate::events::event_ctor, 1),
        C::CustomEvent => (crate::events::custom_event_ctor, 1),
        C::UIEvent | C::MouseEvent | C::KeyboardEvent | C::FocusEvent | C::ErrorEvent => {
            (crate::events::event_ctor, 1)
        }
        C::Text => (crate::dom::text_ctor, 0),
        C::Comment => (crate::dom::comment_ctor, 0),
        C::DocumentFragment => (crate::dom::fragment_ctor, 0),
        C::Document => (crate::dom::document_ctor, 0),
        C::DOMException => (crate::exception::ctor, 0),
        C::DOMRect | C::DOMRectReadOnly => (crate::css::rect_ctor, 0),
        C::DOMPoint | C::DOMPointReadOnly => (crate::geometry::point_ctor, 0),
        C::DOMQuad => (crate::geometry::quad_ctor, 0),
        C::DOMMatrix | C::DOMMatrixReadOnly => (crate::geometry::matrix_ctor, 0),
        C::URL => (crate::url::url_ctor, 1),
        C::Headers => (crate::net::headers_ctor, 0),
        C::Request => (crate::net::request_ctor, 1),
        C::Response => (crate::net::response_ctor, 0),
        C::AbortController => (crate::net::abort_controller_ctor, 0),
        C::XMLHttpRequest => (crate::net::xhr_ctor, 0),
        C::ProgressEvent => (crate::events::event_ctor, 1),
        C::TextEncoder => (crate::net::text_encoder_ctor, 0),
        C::TextDecoder => (crate::net::text_decoder_ctor, 0),
        C::URLSearchParams => (crate::url::params_ctor, 0),
        C::MutationObserver => (crate::mutation::observer_ctor, 1),
        C::Range => (crate::range::range_ctor, 0),
        C::DOMParser => (crate::xml::dom_parser_ctor, 0),
        C::Blob => (crate::blob::blob_ctor, 0),
        C::File => (crate::blob::file_ctor, 2),
        C::FileReader => (crate::blob::file_reader_ctor, 0),
        C::FormData => (crate::blob::form_data_ctor, 0),
        C::FormDataEvent => (crate::events::event_ctor, 2),
        C::SubmitEvent => (crate::events::event_ctor, 1),
        C::ToggleEvent => (crate::events::event_ctor, 1),
        C::MessageEvent => (crate::events::event_ctor, 1),
        C::Worker => (crate::worker::worker_ctor, 1),
        C::MessageChannel => (crate::port::message_channel_ctor, 0),
        C::BroadcastChannel => (crate::port::broadcast_channel_ctor, 1),
        C::ImageData => (crate::canvas::image_data_ctor, 2),
        C::Path2D => (crate::canvas::path2d_ctor, 0),
        C::XMLSerializer => (crate::xml::xml_serializer_ctor, 0),
        C::StaticRange => (crate::range::static_range_ctor, 1),
        // HTML element interfaces have [HTMLConstructor] (custom elements).
        c if c != C::HTMLUnknownElement && implements(c as u32, C::HTMLElement) => (crate::ce::html_ctor, 0),
        _ => return None,
    })
}

/// Interfaces exposed in dedicated workers ([Exposed=DedicatedWorker] and wider).
fn exposed_in_worker(c: C) -> bool {
    matches!(
        c,
        C::EventTarget
            | C::Event
            | C::CustomEvent
            | C::ErrorEvent
            | C::MessageEvent
            | C::ProgressEvent
            | C::DOMException
            | C::AbortController
            | C::AbortSignal
            | C::URL
            | C::URLSearchParams
            | C::Headers
            | C::Request
            | C::Response
            | C::XMLHttpRequestEventTarget
            | C::XMLHttpRequest
            | C::XMLHttpRequestUpload
            | C::TextEncoder
            | C::TextDecoder
            | C::Blob
            | C::File
            | C::FileReader
            | C::FormData
            | C::Crypto
            | C::Worker
            | C::WorkerGlobalScope
            | C::DedicatedWorkerGlobalScope
            | C::WorkerLocation
            | C::WorkerNavigator
            | C::MessageChannel
            | C::MessagePort
            | C::BroadcastChannel
            | C::ImageData
            | C::Path2D
            | C::ImageBitmap
            | C::DOMRectReadOnly
            | C::DOMRect
            | C::DOMPointReadOnly
            | C::DOMPoint
            | C::DOMQuad
            | C::DOMMatrixReadOnly
            | C::DOMMatrix
    )
}

/// Interfaces exposed on a Window (all but the worker-only ones).
fn exposed_in_window(c: C) -> bool {
    !matches!(
        c,
        C::WorkerGlobalScope | C::DedicatedWorkerGlobalScope | C::WorkerLocation | C::WorkerNavigator
    )
}

/// Create every interface object and prototype (WebIDL §3.7.1–3.7.3) and expose them on
/// the global object, which becomes a Window.
pub fn install(vm: &mut Vm, h: &Host) {
    install_scope(vm, h, false);
}

/// As [`install`], for a Window or (`worker`) a DedicatedWorkerGlobalScope.
pub fn install_scope(vm: &mut Vm, h: &Host, worker: bool) {
    let g = vm.realm.global;
    let mut protos: Vec<ObjRef> = Vec::with_capacity(ALL.len());
    let mut ctors: Vec<ObjRef> = Vec::with_capacity(ALL.len());
    for &c in ALL {
        let parent_proto = match parent(c) {
            Some(p) => protos[p as usize],
            None if c == C::DOMException => vm.realm.error_proto(bb_js::vm::ErrorKind::Error),
            None => vm.realm.object_proto,
        };
        let proto = vm.new_object_with_proto(Some(parent_proto));
        let (f, len) = constructor(c).unwrap_or((illegal, 0));
        let ctor = realm::native_with(vm, name(c), len, f, true, Vec::new());
        if let Some(p) = parent(c) {
            vm.heap.obj_mut(ctor).proto = Some(ctors[p as usize]);
        }
        vm.heap
            .obj_mut(ctor)
            .props
            .insert(PropKey::lit("prototype"), Prop::data(Value::Object(proto), false, false, false));
        vm.heap.obj_mut(proto).props.insert(PropKey::lit("constructor"), Prop::method(Value::Object(ctor)));
        realm::to_string_tag(vm, proto, name(c));
        let exposed = if worker { exposed_in_worker(c) } else { exposed_in_window(c) };
        if exposed {
            realm::value(vm, g, name(c), Value::Object(ctor));
        }
        protos.push(proto);
        ctors.push(ctor);
    }
    *h.protos.borrow_mut() = protos.clone();
    *h.ctors.borrow_mut() = ctors.clone();
    for &c in ALL {
        let (p, k) = (protos[c as usize], ctors[c as usize]);
        // The Window's setup puts its members on the global object itself.
        if worker && c == C::Window {
            continue;
        }
        setup(vm, c, p, k, worker);
    }
    if worker {
        vm.heap.obj_mut(g).proto = Some(protos[C::DedicatedWorkerGlobalScope as usize]);
        crate::worker::setup_worker_global(vm, g);
        return;
    }
    crate::traversal::install_node_filter(vm);
    crate::window::install_window_properties(vm, h);
    // The global object is the Window (HTML §7.2); its prototype chain is Window's.
    vm.heap.obj_mut(g).proto = Some(protos[C::Window as usize]);
}

fn setup(vm: &mut Vm, c: C, proto: ObjRef, ctor: ObjRef, worker: bool) {
    match c {
        C::EventTarget => crate::events::setup_event_target(vm, proto),
        C::Node => crate::dom::setup_node(vm, proto, ctor),
        C::Document => {
            crate::dom::setup_document(vm, proto);
            crate::xml::setup_document_statics(vm, ctor);
        }
        // HTML §4.12.1 `HTMLScriptElement.supports(type)`: the script types this engine runs.
        C::HTMLScriptElement => {
            crate::idl::op(vm, ctor, "supports", 1, |vm, c| {
                let ty = crate::host::rstr(vm, &c.arg(0))?;
                Ok(Value::Bool(matches!(ty.as_str(), "classic" | "module" | "importmap")))
            });
            crate::html::setup_element_interface(vm, c, proto);
        }
        C::DocumentType => crate::dom::setup_doctype(vm, proto),
        C::DocumentFragment => crate::dom::setup_fragment(vm, proto),
        C::CharacterData => crate::dom::setup_chardata(vm, proto),
        C::Text => {
            crate::dom::setup_text(vm, proto);
            crate::shadow::setup_text(vm, proto);
        }
        C::ShadowRoot => crate::shadow::setup_shadow_root(vm, proto),
        C::HTMLSlotElement => crate::shadow::setup_slot(vm, proto),
        C::ProcessingInstruction => crate::dom::setup_pi(vm, proto),
        C::Attr => crate::attr::setup_attr(vm, proto),
        C::Element => {
            crate::dom::setup_element(vm, proto);
            crate::shadow::setup_element(vm, proto);
        }
        C::HTMLElement => {
            crate::html::setup_html_element(vm, proto);
            crate::internals::setup_html_element(vm, proto);
        }
        C::SVGElement | C::MathMLElement => {
            crate::focus::setup_element(vm, proto);
            crate::css::setup_inline_style(vm, proto);
            crate::events::setup_event_handlers(vm, proto);
        }
        C::NodeList => crate::collections::setup_node_list(vm, proto),
        C::HTMLCollection => crate::collections::setup_html_collection(vm, proto),
        C::DOMTokenList => crate::collections::setup_token_list(vm, proto),
        C::NamedNodeMap => crate::attr::setup_named_node_map(vm, proto),
        C::DOMImplementation => crate::dom::setup_implementation(vm, proto),
        C::DOMException => crate::exception::setup(vm, proto, ctor),
        C::Event => crate::events::setup_event(vm, proto, ctor),
        C::CustomEvent => crate::events::setup_custom_event(vm, proto),
        C::UIEvent => crate::events::setup_ui_event(vm, proto),
        C::SubmitEvent => crate::events::setup_submit_event(vm, proto),
        C::ToggleEvent => crate::events::setup_toggle_event(vm, proto),
        C::CustomElementRegistry => crate::ce::setup_registry(vm, proto),
        C::ElementInternals => crate::internals::setup_internals(vm, proto),
        C::ValidityState => crate::internals::setup_validity_state(vm, proto),
        C::CustomStateSet => crate::internals::setup_custom_state_set(vm, proto),
        C::MouseEvent => crate::events::setup_mouse_event(vm, proto),
        C::FocusEvent => crate::events::setup_focus_event(vm, proto),
        C::KeyboardEvent => crate::events::setup_keyboard_event(vm, proto),
        C::ErrorEvent => crate::events::setup_error_event(vm, proto),
        C::Window => {
            crate::window::setup_window(vm, proto);
            crate::selection::setup_window(vm, proto);
        }
        C::CSSStyleDeclaration => crate::css::setup_style_declaration(vm, proto),
        C::DOMRectReadOnly => {
            crate::css::setup_rect_readonly(vm, proto);
            crate::geometry::setup_rect_statics(vm, ctor, c);
        }
        C::DOMRect => {
            crate::css::setup_rect(vm, proto);
            crate::geometry::setup_rect_statics(vm, ctor, c);
        }
        C::DOMPointReadOnly => crate::geometry::setup_point_readonly(vm, proto, ctor),
        C::DOMPoint => crate::geometry::setup_point(vm, proto, ctor),
        C::DOMQuad => crate::geometry::setup_quad(vm, proto, ctor),
        C::DOMMatrixReadOnly => crate::geometry::setup_matrix_readonly(vm, proto, ctor, worker),
        C::DOMMatrix => crate::geometry::setup_matrix(vm, proto, ctor, worker),
        C::DOMRectList => crate::css::setup_rect_list(vm, proto),
        C::URL => {
            crate::url::setup_url(vm, proto, ctor);
            crate::blob::setup_url_statics(vm, ctor);
        }
        C::URLSearchParams => crate::url::setup_params(vm, proto),
        C::Headers => crate::net::setup_headers(vm, proto),
        C::Request => crate::net::setup_request(vm, proto),
        C::Response => crate::net::setup_response(vm, proto, ctor),
        C::AbortController => crate::net::setup_abort_controller(vm, proto),
        C::AbortSignal => crate::net::setup_abort_signal(vm, proto, ctor),
        C::XMLHttpRequestEventTarget => crate::net::setup_xhr_target(vm, proto),
        C::XMLHttpRequest => crate::net::setup_xhr(vm, proto, ctor),
        C::ProgressEvent => crate::net::setup_progress_event(vm, proto),
        C::TextEncoder => crate::net::setup_text_encoder(vm, proto),
        C::TextDecoder => crate::net::setup_text_decoder(vm, proto),
        C::Storage => crate::storage::setup_storage(vm, proto),
        C::MutationObserver => crate::mutation::setup_observer(vm, proto),
        C::MutationRecord => crate::mutation::setup_record(vm, proto),
        C::AbstractRange => crate::range::setup_abstract_range(vm, proto),
        C::StaticRange => {}
        C::Range => crate::range::setup_range(vm, proto, ctor),
        C::NodeIterator => crate::traversal::setup_node_iterator(vm, proto),
        C::TreeWalker => crate::traversal::setup_tree_walker(vm, proto),
        C::Selection => crate::selection::setup_selection(vm, proto),
        C::DOMParser => crate::xml::setup_dom_parser(vm, proto),
        C::Blob => crate::blob::setup_blob(vm, proto),
        C::File => crate::blob::setup_file(vm, proto),
        C::FileReader => crate::blob::setup_file_reader(vm, proto, ctor),
        C::FormData => crate::blob::setup_form_data(vm, proto),
        C::FormDataEvent => crate::blob::setup_form_data_event(vm, proto),
        C::Crypto => crate::blob::setup_crypto(vm, proto),
        C::MessageEvent => crate::worker::setup_message_event(vm, proto),
        C::Worker => crate::worker::setup_worker(vm, proto),
        C::MessageChannel => crate::port::setup_message_channel(vm, proto),
        C::MessagePort => crate::port::setup_message_port(vm, proto),
        C::BroadcastChannel => crate::port::setup_broadcast_channel(vm, proto),
        C::CanvasRenderingContext2D => crate::canvas::setup_context(vm, proto),
        C::CanvasGradient => crate::canvas::setup_gradient(vm, proto),
        C::CanvasPattern => crate::canvas::setup_pattern(vm, proto),
        C::ImageData => crate::canvas::setup_image_data(vm, proto),
        C::TextMetrics => crate::canvas::setup_text_metrics(vm, proto),
        C::Path2D => crate::canvas::setup_path2d(vm, proto),
        C::ImageBitmap => crate::canvas::setup_image_bitmap(vm, proto),
        C::WorkerGlobalScope | C::DedicatedWorkerGlobalScope | C::WorkerLocation | C::WorkerNavigator => {}
        C::XMLSerializer => crate::xml::setup_xml_serializer(vm, proto),
        _ => crate::html::setup_element_interface(vm, c, proto),
    }
}

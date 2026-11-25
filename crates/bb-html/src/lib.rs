//! `bb-html` — the WHATWG HTML parser.
//!
//! * [`tokenizer`]: the full HTML §13.2.5 tokenizer state machine, with the
//!   complete named character reference table.
//! * [`tree_builder`]: HTML §13.2.6 tree construction into a [`bb_dom::Document`].
//!
//! ```
//! let doc = bb_html::parse_document("<p>Hello <b>world");
//! assert_eq!(doc.title(), "");
//! assert_eq!(doc.text_content(doc.body().unwrap()), "Hello world");
//! ```

mod entities;
mod foreign;
mod quirks;
pub mod tokenizer;
pub mod tree_builder;

use bb_dom::{Document, Namespace, NodeId};
pub use tree_builder::TreeBuilder;

/// Parse a complete HTML document.
pub fn parse_document(input: &str) -> Document {
    TreeBuilder::new(input).run()
}

/// Parse a document loaded by navigation: declarative shadow roots are allowed.
pub fn parse_document_with_shadow_roots(input: &str) -> Document {
    let mut tb = TreeBuilder::new(input);
    tb.set_allow_declarative_shadow_roots(true);
    tb.run()
}

/// Parse an HTML fragment with declarative shadow roots allowed (`setHTMLUnsafe`).
pub fn parse_fragment_with_shadow_roots(
    input: &str,
    context_ns: Namespace,
    context_name: &str,
) -> (Document, NodeId) {
    TreeBuilder::parse_fragment_with(input, context_ns, context_name, true)
}

/// Parse an HTML fragment in the context of an element (HTML §13.4).
/// Returns the document and the node whose children are the parsed nodes.
pub fn parse_fragment(input: &str, context_ns: Namespace, context_name: &str) -> (Document, NodeId) {
    TreeBuilder::parse_fragment(input, context_ns, context_name)
}

/// Dump a document in html5lib-tests tree format (used by `--dump-dom`).
pub fn dump(doc: &Document) -> String {
    bb_dom::dump_html5lib(doc, Document::ROOT)
}

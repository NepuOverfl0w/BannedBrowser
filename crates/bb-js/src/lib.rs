//! `bb-js` — BannedBrowser's JavaScript engine (milestone D, in progress).
//!
//! Done so far: the lexer (`lexer.rs`), the parser with its early errors (`parser.rs`) into an
//! AST (`ast.rs`), and regular expression syntax checks (`regexp.rs`), measured against test262.
//! Not yet: bytecode, the interpreter, the garbage collector and the built-ins; until then
//! `<script>` elements are ignored by the engine and `<noscript>` content is rendered.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod regexp;
mod regexp_props;
mod unicode_ids;

pub use parser::{parse_module, parse_script, ParseError};

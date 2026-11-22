//! `bb-js` — BannedBrowser's JavaScript engine (milestone D, in progress).
//!
//! The lexer (`lexer.rs`), the parser with its early errors (`parser.rs`) into an AST
//! (`ast.rs`) and regular expression syntax checks (`regexp.rs`) are complete against test262.
//! The interpreter (`vm/`) compiles the AST to bytecode and runs it over a garbage-collected
//! heap; classes, generators, async functions, modules, RegExp matching and several built-ins
//! are still to come. The engine doesn't run page scripts yet: `<script>` elements are ignored
//! and `<noscript>` content is rendered until the DOM bindings (milestone E).

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod regexp;
mod regexp_props;
mod unicode_ids;
pub mod vm;

pub use parser::{parse_eval, parse_module, parse_script, EvalContext, ParseError};
pub use vm::{JsResult, Value, Vm};

//! The syntactic grammar (ECMA-262 §13–§16) with its early errors (the "static semantics"
//! the spec checks before running any code): reserved words by context, strict mode,
//! redeclarations, labels and `break`/`continue` targets, `yield`/`await`/`super`/`new.target`
//! placement, destructuring via the cover grammars, class element rules and private names,
//! and module import/export rules.

use crate::ast::*;
use crate::lexer::{JsStr, LexError, Lexer, TokKind, Token};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError { pos: e.pos, msg: e.msg }
    }
}

type R<T> = Result<T, ParseError>;

const RESERVED: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];
const STRICT_RESERVED: &[&str] =
    &["implements", "interface", "let", "package", "private", "protected", "public", "static", "yield"];

/// What a function body may contain.
#[derive(Clone)]
struct FnCtx {
    is_async: bool,
    is_generator: bool,
    is_arrow: bool,
    /// `return` is allowed (any function, not scripts/modules/static blocks).
    can_return: bool,
    new_target: bool,
    super_prop: bool,
    super_call: bool,
    /// Class field initializer or static block: `arguments` is an error.
    no_arguments: bool,
    /// Class static block: `await` is reserved.
    static_block: bool,
    labels: Vec<(String, bool)>,
    loop_depth: u32,
    break_depth: u32,
}

#[derive(Default)]
struct Scope {
    function: bool,
    lexical: HashSet<String>,
    vars: HashSet<String>,
    /// Function declarations directly in a block (sloppy mode may repeat them, Annex B.3.2.4).
    block_functions: HashSet<String>,
    params: HashSet<String>,
    /// A catch clause's simple parameter (Annex B.3.4: `var` may redeclare it).
    catch_simple: HashSet<String>,
    catch_params: HashSet<String>,
}

#[derive(Default)]
struct ClassPrivates {
    declared: HashMap<String, &'static str>,
    used: Vec<(String, usize)>,
}

/// An unconverted parenthesized expression that may still become arrow parameters.
struct ParenCover {
    items: Vec<Expr>,
    rest: Option<Pat>,
    /// Only valid as arrow parameters: `()`, `(a, ...b)`, `(a,)`.
    arrow_only: bool,
}

pub struct Parser {
    lx: Lexer,
    tok: Token,
    prev_end: usize,
    module: bool,
    strict: bool,
    fns: Vec<FnCtx>,
    scopes: Vec<Scope>,
    classes: Vec<ClassPrivates>,
    /// Positions of `{a = 1}` and duplicate `__proto__` in object literals not (yet) known to be
    /// patterns.
    cover_errs: Vec<usize>,
    covers: HashMap<usize, ParenCover>,
    /// Positions of `yield`/`await` expressions (arrow parameters may not contain them).
    yield_await_seen: Vec<usize>,
    exported: HashSet<JsStr>,
    export_locals: Vec<(String, usize)>,
    /// Set by `paren` when the expression just parsed is a parenthesized one before `=>`.
    last_paren_start: Option<usize>,
    /// Above zero while parsing inside array/object/paren literals: cover errors are left to
    /// the outermost assignment expression.
    cover_defer: u32,
    /// Start of the last `async(…)` call (a possible async arrow head) and whether its
    /// arguments ended with `...x,`.
    async_call_start: Option<usize>,
    async_call_trailing_comma: bool,
    /// Parsing formal parameters (yield/await expressions are errors there).
    in_params: bool,
    /// Array literals (by start) with a comma after a spread element.
    array_spread_trailing_comma: HashSet<usize>,
    /// The last directive prologue contained "use strict".
    saw_use_strict: bool,
    /// Decorators parsed before the class being parsed, and its members' decorators.
    pending_decorators: Vec<Expr>,
    pending_member_decorators: Vec<(usize, Vec<Expr>)>,
}

fn kw(t: &Token, name: &str) -> bool {
    matches!(&t.kind, TokKind::Ident { name: n, escaped: false } if n == name)
}

fn simple_params(params: &[Pat], rest: &Option<Pat>) -> bool {
    rest.is_none() && params.iter().all(|p| matches!(p, Pat::Ident(_)))
}

fn bound_names(p: &Pat, out: &mut Vec<String>) {
    match p {
        Pat::Ident(n) => out.push(n.clone()),
        Pat::Array(els, rest) => {
            for e in els.iter().flatten() {
                bound_names(e, out);
            }
            if let Some(r) = rest {
                bound_names(r, out);
            }
        }
        Pat::Object(props, rest) => {
            for (_, v) in props {
                bound_names(v, out);
            }
            if let Some(r) = rest {
                bound_names(r, out);
            }
        }
        Pat::Default(t, _) => bound_names(t, out),
        Pat::Expr(_) => {}
    }
}

fn utf16(s: &str) -> JsStr {
    s.encode_utf16().collect()
}

impl Parser {
    pub fn new(src: &str, module: bool) -> R<Parser> {
        let mut lx = Lexer::new(src);
        let tok = lx.next(true, !module)?;
        Ok(Parser {
            lx,
            tok,
            prev_end: 0,
            module,
            strict: module,
            fns: vec![FnCtx {
                is_async: false,
                is_generator: false,
                is_arrow: false,
                can_return: false,
                new_target: false,
                super_prop: false,
                super_call: false,
                no_arguments: false,
                static_block: false,
                labels: Vec::new(),
                loop_depth: 0,
                break_depth: 0,
            }],
            scopes: vec![Scope { function: true, ..Default::default() }],
            classes: Vec::new(),
            cover_errs: Vec::new(),
            covers: HashMap::new(),
            yield_await_seen: Vec::new(),
            exported: HashSet::new(),
            export_locals: Vec::new(),
            last_paren_start: None,
            cover_defer: 0,
            async_call_start: None,
            async_call_trailing_comma: false,
            in_params: false,
            array_spread_trailing_comma: HashSet::new(),
            saw_use_strict: false,
            pending_decorators: Vec::new(),
            pending_member_decorators: Vec::new(),
        })
    }

    // ----- tokens -----

    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(ParseError { pos: self.tok.start, msg: msg.into() })
    }

    fn next(&mut self) -> R<Token> {
        self.prev_end = self.tok.end;
        let t = self.lx.next(false, !self.module)?;
        Ok(std::mem::replace(&mut self.tok, t))
    }

    /// The token after the current one (lexed without a regex context).
    fn peek(&mut self) -> R<Token> {
        let pos = self.lx.pos;
        let t = self.lx.next(false, !self.module);
        self.lx.pos = pos;
        Ok(t?)
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.tok.kind, TokKind::Punct(q) if q == p)
    }

    fn eat(&mut self, p: &str) -> R<bool> {
        if self.is(p) {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p)? {
            Ok(())
        } else {
            self.err(format!("expected {p}"))
        }
    }

    fn is_kw(&self, name: &str) -> bool {
        kw(&self.tok, name)
    }

    fn eat_kw(&mut self, name: &str) -> R<bool> {
        if self.is_kw(name) {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn semicolon(&mut self) -> R<()> {
        if self.eat(";")? || self.is("}") || self.tok.kind == TokKind::Eof || self.tok.nl_before {
            Ok(())
        } else {
            self.err("expected ;")
        }
    }

    /// Re-lex the current `/` or `/=` as a regular expression literal.
    fn relex_regex(&mut self) -> R<()> {
        let nl = self.tok.nl_before;
        self.lx.pos = self.tok.start;
        self.tok = self.lx.next(true, !self.module)?;
        self.tok.nl_before = nl;
        Ok(())
    }

    fn f(&self) -> &FnCtx {
        self.fns.last().expect("function context")
    }

    fn fm(&mut self) -> &mut FnCtx {
        self.fns.last_mut().expect("function context")
    }

    fn yield_reserved(&self) -> bool {
        self.strict || self.f().is_generator
    }

    fn await_reserved(&self) -> bool {
        self.module || self.f().is_async || self.f().static_block
    }

    // ----- identifiers -----

    fn check_ident(&self, name: &str, binding: bool, pos: usize) -> R<()> {
        let bad = |m: String| Err(ParseError { pos, msg: m });
        if RESERVED.contains(&name) {
            return bad(format!("'{name}' is a reserved word"));
        }
        if self.strict && STRICT_RESERVED.contains(&name) {
            return bad(format!("'{name}' is reserved in strict mode"));
        }
        if name == "yield" && self.yield_reserved() {
            return bad("'yield' is reserved here".into());
        }
        if name == "await" && self.await_reserved() {
            return bad("'await' is reserved here".into());
        }
        if binding && self.strict && (name == "eval" || name == "arguments") {
            return bad(format!("can't bind '{name}' in strict mode"));
        }
        if name == "arguments"
            && self.fns.iter().rev().take_while(|f| f.is_arrow || f.no_arguments).any(|f| f.no_arguments)
        {
            return bad("'arguments' in a class field or static block".into());
        }
        Ok(())
    }

    /// An identifier token (reference or binding).
    fn ident(&mut self, binding: bool) -> R<String> {
        let pos = self.tok.start;
        match &self.tok.kind {
            TokKind::Ident { name, .. } => {
                let name = name.clone();
                self.check_ident(&name, binding, pos)?;
                self.next()?;
                Ok(name)
            }
            _ => self.err("expected an identifier"),
        }
    }

    fn ident_name(&mut self) -> R<String> {
        match &self.tok.kind {
            TokKind::Ident { name, .. } => {
                let n = name.clone();
                self.next()?;
                Ok(n)
            }
            _ => self.err("expected a name"),
        }
    }

    // ----- scopes -----

    fn push_scope(&mut self, function: bool) {
        self.scopes.push(Scope { function, ..Default::default() });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare_lexical(&mut self, name: &str, pos: usize) -> R<()> {
        let s = self.scopes.last().expect("scope");
        if s.lexical.contains(name)
            || s.vars.contains(name)
            || s.params.contains(name)
            || s.catch_params.contains(name)
        {
            return Err(ParseError { pos, msg: format!("'{name}' has already been declared") });
        }
        if name == "let" {
            return Err(ParseError { pos, msg: "'let' can't be a lexically bound name".into() });
        }
        self.scopes.last_mut().expect("scope").lexical.insert(name.to_string());
        Ok(())
    }

    fn declare_var(&mut self, name: &str, pos: usize) -> R<()> {
        for s in self.scopes.iter_mut().rev() {
            if s.lexical.contains(name) && !s.block_functions.is_empty() && !s.function {
                // fallthrough to the generic check below
            }
            if s.lexical.contains(name) || (s.catch_params.contains(name) && !s.catch_simple.contains(name)) {
                return Err(ParseError { pos, msg: format!("'{name}' has already been declared") });
            }
            s.vars.insert(name.to_string());
            if s.function {
                break;
            }
        }
        Ok(())
    }

    fn declare_function(&mut self, name: &str, pos: usize) -> R<()> {
        self.declare_function_kind(name, pos, true)
    }

    /// `plain`: a plain `function` (not async, not a generator); only those may repeat in a
    /// sloppy-mode block.
    fn declare_function_kind(&mut self, name: &str, pos: usize, plain: bool) -> R<()> {
        let top = self.scopes.last().expect("scope").function;
        if top {
            // Var-scoped at the top of a function or script (module: lexical).
            if self.module && self.scopes.len() == 1 {
                return self.declare_lexical(name, pos);
            }
            let s = self.scopes.last().expect("scope");
            if s.lexical.contains(name) {
                return Err(ParseError { pos, msg: format!("'{name}' has already been declared") });
            }
            self.scopes.last_mut().expect("scope").vars.insert(name.to_string());
            Ok(())
        } else {
            let sloppy_dup =
                plain && !self.strict && self.scopes.last().expect("scope").block_functions.contains(name);
            if sloppy_dup {
                return Ok(());
            }
            self.declare_lexical(name, pos)?;
            if plain {
                self.scopes.last_mut().expect("scope").block_functions.insert(name.to_string());
            }
            Ok(())
        }
    }

    fn declare_pattern(&mut self, kind: VarKind, p: &Pat, pos: usize) -> R<()> {
        let mut names = Vec::new();
        bound_names(p, &mut names);
        for n in names {
            if kind == VarKind::Var {
                self.declare_var(&n, pos)?;
            } else {
                self.declare_lexical(&n, pos)?;
            }
        }
        Ok(())
    }

    // ----- programs -----

    pub fn parse_program(mut self) -> R<Program> {
        let body = self.statement_list(true, |p| p.tok.kind == TokKind::Eof)?;
        if self.module {
            // Every `export { x }` must name a top-level declaration.
            let s = &self.scopes[0];
            for (name, pos) in &self.export_locals {
                if !s.lexical.contains(name) && !s.vars.contains(name) {
                    return Err(ParseError { pos: *pos, msg: format!("export of undeclared '{name}'") });
                }
            }
        }
        self.check_unresolved_privates_at_top()?;
        Ok(Program { body, strict: self.strict, module: self.module })
    }

    fn check_unresolved_privates_at_top(&self) -> R<()> {
        Ok(())
    }

    /// A statement list with a directive prologue (`directives`: script/function bodies).
    fn statement_list(&mut self, directives: bool, end: impl Fn(&Parser) -> bool) -> R<Vec<Stmt>> {
        let mut body = Vec::new();
        let mut prologue = directives;
        let mut octal_in_prologue: Option<usize> = None;
        while !end(self) {
            if prologue {
                if let TokKind::Str(_) = &self.tok.kind {
                    let raw: String = self.lx.source()[self.tok.start..self.tok.end].iter().collect();
                    let legacy = self.tok.legacy_octal;
                    let pos = self.tok.start;
                    let stmt = self.statement_item()?;
                    let is_directive = matches!(&stmt, Stmt::Expr(Expr::Str(_)));
                    if is_directive {
                        if legacy && octal_in_prologue.is_none() {
                            octal_in_prologue = Some(pos);
                        }
                        if raw == "'use strict'" || raw == "\"use strict\"" {
                            self.saw_use_strict = true;
                            if !self.strict {
                                self.strict = true;
                            }
                            if let Some(p) = octal_in_prologue {
                                return Err(ParseError {
                                    pos: p,
                                    msg: "octal escape before \"use strict\"".into(),
                                });
                            }
                        }
                    } else {
                        prologue = false;
                    }
                    if self.strict {
                        if let Some(p) = octal_in_prologue {
                            return Err(ParseError { pos: p, msg: "octal escape in strict mode".into() });
                        }
                    }
                    body.push(stmt);
                    continue;
                }
                prologue = false;
            }
            body.push(self.statement_item()?);
        }
        Ok(body)
    }

    // ----- statements -----

    /// StatementListItem: declarations allowed.
    fn statement_item(&mut self) -> R<Stmt> {
        let pos = self.tok.start;
        if self.is_kw("function") {
            return self.function_declaration(false, pos);
        }
        if self.is_kw("async") {
            let n = self.peek()?;
            if kw(&n, "function") && !n.nl_before {
                self.next()?;
                return self.function_declaration(true, pos);
            }
        }
        if self.is("@") {
            let d = self.decorators()?;
            if self.is_kw("export") && self.module && self.scopes.len() == 1 {
                self.pending_decorators = d;
                return self.export_declaration();
            }
            if !self.is_kw("class") {
                return self.err("decorators must come before a class");
            }
            self.pending_decorators = d;
        }
        if self.is_kw("class") {
            let c = self.class(true)?;
            return Ok(Stmt::Class(c));
        }
        if self.is_kw("const") || (self.is_kw("let") && self.let_is_declaration()?) {
            let kind = if self.is_kw("const") { VarKind::Const } else { VarKind::Let };
            self.next()?;
            let decls = self.var_declarations(kind, false)?;
            self.require_initializers(kind, &decls, pos)?;
            self.semicolon()?;
            return Ok(Stmt::Var(kind, decls));
        }
        if self.using_declaration_start()? {
            return self.using_declaration();
        }
        if self.is_kw("import") {
            let n = self.peek()?;
            if !matches!(n.kind, TokKind::Punct("(") | TokKind::Punct(".")) {
                if !self.module || self.scopes.len() != 1 {
                    return self.err("import declarations are only allowed at the top of a module");
                }
                return self.import_declaration();
            }
        }
        if self.is_kw("export") {
            if !self.module || self.scopes.len() != 1 {
                return self.err("export declarations are only allowed at the top of a module");
            }
            return self.export_declaration();
        }
        self.statement()
    }

    fn let_is_declaration(&mut self) -> R<bool> {
        let n = self.peek()?;
        Ok(match &n.kind {
            TokKind::Punct("[") | TokKind::Punct("{") => true,
            TokKind::Ident { name, .. } => {
                // `let` then a newline then an identifier is still a declaration, except
                // `let \n yield/await` in contexts where those aren't identifiers... keep it simple.
                !(n.nl_before && (name == "in" || name == "instanceof" || name == "of"))
                    && !(name == "in" || name == "instanceof")
            }
            _ => false,
        })
    }

    fn using_declaration_start(&mut self) -> R<bool> {
        if self.is_kw("using") {
            let n = self.peek()?;
            if let TokKind::Ident { name, .. } = &n.kind {
                return Ok(!n.nl_before && name != "in" && name != "of" && name != "instanceof");
            }
        }
        if self.is_kw("await") && self.await_reserved() {
            let n = self.peek()?;
            if kw(&n, "using") && !n.nl_before {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn using_declaration(&mut self) -> R<Stmt> {
        if self.scopes.len() == 1 && !self.module {
            return self.err("'using' declarations aren't allowed at the top of a script");
        }
        let pos = self.tok.start;
        let kind = if self.eat_kw("await")? { VarKind::AwaitUsing } else { VarKind::Using };
        self.next()?; // using
        let decls = self.var_declarations(kind, false)?;
        self.require_initializers(kind, &decls, pos)?;
        self.semicolon()?;
        Ok(Stmt::Var(kind, decls))
    }

    fn function_declaration(&mut self, is_async: bool, start: usize) -> R<Stmt> {
        self.next()?; // function
        let is_generator = self.eat("*")?;
        let pos = self.tok.start;
        // The name is checked in the enclosing context (except generator/async rules below).
        let name = match &self.tok.kind {
            TokKind::Ident { name, .. } => {
                let n = name.clone();
                self.check_ident(&n, true, pos)?;
                self.next()?;
                n
            }
            _ => return self.err("expected a function name"),
        };
        self.declare_function_kind(&name, pos, !is_async && !is_generator)?;
        let f = self.function_rest(Some(name), is_async, is_generator, FuncKind::Normal, start)?;
        Ok(Stmt::Function(f))
    }

    /// A Statement (no declarations except where Annex B allows functions).
    fn statement(&mut self) -> R<Stmt> {
        let pos = self.tok.start;
        match &self.tok.kind {
            TokKind::Punct("{") => {
                self.next()?;
                self.push_scope(false);
                let body = self.statement_list(false, |p| p.is("}"))?;
                self.pop_scope();
                self.expect("}")?;
                Ok(Stmt::Block(body))
            }
            TokKind::Punct(";") => {
                self.next()?;
                Ok(Stmt::Empty)
            }
            TokKind::Ident { name, escaped: false } => {
                let name = name.clone();
                match name.as_str() {
                    "var" => {
                        self.next()?;
                        let d = self.var_declarations(VarKind::Var, false)?;
                        self.require_initializers(VarKind::Var, &d, pos)?;
                        self.semicolon()?;
                        Ok(Stmt::Var(VarKind::Var, d))
                    }
                    "if" => self.if_statement(),
                    "for" => self.for_statement(),
                    "while" => {
                        self.next()?;
                        self.expect("(")?;
                        let test = self.expression(false)?;
                        self.expect(")")?;
                        let body = self.loop_body()?;
                        Ok(Stmt::While(test, Box::new(body)))
                    }
                    "do" => {
                        self.next()?;
                        let body = self.loop_body()?;
                        if !self.eat_kw("while")? {
                            return self.err("expected while");
                        }
                        self.expect("(")?;
                        let test = self.expression(false)?;
                        self.expect(")")?;
                        // §12.10.1: a semicolon is inserted after do-while even on one line.
                        self.eat(";")?;
                        Ok(Stmt::DoWhile(Box::new(body), test))
                    }
                    "continue" | "break" => {
                        self.next()?;
                        let mut label = None;
                        if !self.tok.nl_before {
                            if let TokKind::Ident { name: l, .. } = &self.tok.kind {
                                if !RESERVED.contains(&l.as_str())
                                    || matches!(self.tok.kind, TokKind::Ident { escaped: true, .. })
                                {
                                    let l = l.clone();
                                    self.next()?;
                                    label = Some(l);
                                }
                            }
                        }
                        let f = self.f();
                        match (&label, name.as_str()) {
                            (Some(l), "continue") => {
                                if !f.labels.iter().any(|(n, is_loop)| n == l && *is_loop) {
                                    return Err(ParseError { pos, msg: format!("no loop labelled '{l}'") });
                                }
                            }
                            (Some(l), _) => {
                                if !f.labels.iter().any(|(n, _)| n == l) {
                                    return Err(ParseError { pos, msg: format!("undefined label '{l}'") });
                                }
                            }
                            (None, "continue") if f.loop_depth == 0 => {
                                return Err(ParseError { pos, msg: "continue outside a loop".into() })
                            }
                            (None, _) if f.break_depth == 0 && name == "break" => {
                                return Err(ParseError { pos, msg: "break outside a loop or switch".into() })
                            }
                            _ => {}
                        }
                        self.semicolon()?;
                        Ok(if name == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                    }
                    "return" => {
                        if !self.f().can_return {
                            return self.err("return outside a function");
                        }
                        self.next()?;
                        let arg = if self.is(";")
                            || self.is("}")
                            || self.tok.nl_before
                            || self.tok.kind == TokKind::Eof
                        {
                            None
                        } else {
                            Some(self.expression(false)?)
                        };
                        self.semicolon()?;
                        Ok(Stmt::Return(arg))
                    }
                    "throw" => {
                        self.next()?;
                        if self.tok.nl_before {
                            return self.err("line break after throw");
                        }
                        let e = self.expression(false)?;
                        self.semicolon()?;
                        Ok(Stmt::Throw(e))
                    }
                    "try" => self.try_statement(),
                    "switch" => self.switch_statement(),
                    "with" => {
                        if self.strict {
                            return self.err("'with' in strict mode");
                        }
                        self.next()?;
                        self.expect("(")?;
                        let o = self.expression(false)?;
                        self.expect(")")?;
                        let body = self.sub_statement()?;
                        Ok(Stmt::With(o, Box::new(body)))
                    }
                    "debugger" => {
                        self.next()?;
                        self.semicolon()?;
                        Ok(Stmt::Debugger)
                    }
                    "function" => self.err("function declarations aren't allowed here"),
                    "class" => self.err("class declarations aren't allowed here"),
                    "const" => self.err("lexical declarations aren't allowed here"),
                    "let" => {
                        let n = self.peek()?;
                        if matches!(n.kind, TokKind::Punct("[")) {
                            return self.err("'let [' can't start a statement");
                        }
                        if self.strict {
                            return self.err("'let' is reserved in strict mode");
                        }
                        if matches!(n.kind, TokKind::Ident { .. } | TokKind::Punct("{")) && !n.nl_before {
                            return self.err("lexical declarations aren't allowed here");
                        }
                        self.expression_or_labelled()
                    }
                    _ => self.expression_or_labelled(),
                }
            }
            _ => self.expression_or_labelled(),
        }
    }

    /// The body of `if`/loops/labels/with: a Statement (Annex B.3.3 allows a function
    /// declaration in sloppy `if`).
    fn sub_statement(&mut self) -> R<Stmt> {
        if self.is_kw("using") && self.using_declaration_start()? {
            return self.err("'using' declarations aren't allowed here");
        }
        self.statement()
    }

    fn loop_body(&mut self) -> R<Stmt> {
        self.fm().loop_depth += 1;
        self.fm().break_depth += 1;
        let b = self.sub_statement();
        self.fm().loop_depth -= 1;
        self.fm().break_depth -= 1;
        b
    }

    fn if_statement(&mut self) -> R<Stmt> {
        self.next()?;
        self.expect("(")?;
        let test = self.expression(false)?;
        self.expect(")")?;
        let then = self.if_branch()?;
        let els = if self.eat_kw("else")? { Some(Box::new(self.if_branch()?)) } else { None };
        Ok(Stmt::If(test, Box::new(then), els))
    }

    fn if_branch(&mut self) -> R<Stmt> {
        // Annex B.3.3: `if (x) function f() {}` in sloppy mode (plain functions only).
        if !self.strict && self.is_kw("function") {
            let n = self.peek()?;
            if matches!(n.kind, TokKind::Punct("*")) {
                return self.err("generator declarations aren't allowed here");
            }
            let pos = self.tok.start;
            self.push_scope(false);
            let f = self.function_declaration(false, pos);
            self.pop_scope();
            return f;
        }
        self.sub_statement()
    }

    fn expression_or_labelled(&mut self) -> R<Stmt> {
        let start = self.tok.start;
        if let TokKind::Ident { name, .. } = self.tok.kind.clone() {
            let n = self.peek()?;
            if matches!(n.kind, TokKind::Punct(":")) {
                if name != "arguments" {
                    self.check_ident(&name, false, start)?;
                }
                self.next()?;
                self.next()?;
                if self.f().labels.iter().any(|(l, _)| *l == name) {
                    return Err(ParseError { pos: start, msg: format!("duplicate label '{name}'") });
                }
                let is_loop = self.is_kw("for")
                    || self.is_kw("while")
                    || self.is_kw("do")
                    || self.label_chain_is_loop()?;
                self.fm().labels.push((name.clone(), is_loop));
                let body = if self.is_kw("function") {
                    if self.strict {
                        self.fm().labels.pop();
                        return self.err("labelled function in strict mode");
                    }
                    let n2 = self.peek()?;
                    if matches!(n2.kind, TokKind::Punct("*")) {
                        self.fm().labels.pop();
                        return self.err("labelled generator");
                    }
                    let pos = self.tok.start;
                    self.function_declaration(false, pos)
                } else {
                    self.fm().break_depth += 1;
                    let b = self.sub_statement();
                    self.fm().break_depth -= 1;
                    b
                };
                self.fm().labels.pop();
                return Ok(Stmt::Labeled(name, Box::new(body?)));
            }
        }
        // ExpressionStatement lookahead restrictions.
        if self.is_kw("async") {
            let n = self.peek()?;
            if kw(&n, "function") && !n.nl_before {
                return self.err("async function declarations aren't allowed here");
            }
        }
        let e = self.expression(false)?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    /// `a: b: for (...)`: a label on a label on a loop is a loop label too.
    fn label_chain_is_loop(&mut self) -> R<bool> {
        let save = (self.lx.pos, self.tok.clone(), self.prev_end);
        let mut is_loop = false;
        loop {
            if self.is_kw("for") || self.is_kw("while") || self.is_kw("do") {
                is_loop = true;
                break;
            }
            if let TokKind::Ident { .. } = self.tok.kind {
                let n = self.peek()?;
                if matches!(n.kind, TokKind::Punct(":")) {
                    self.next()?;
                    self.next()?;
                    continue;
                }
            }
            break;
        }
        self.lx.pos = save.0;
        self.tok = save.1;
        self.prev_end = save.2;
        Ok(is_loop)
    }

    fn var_declarations(&mut self, kind: VarKind, no_in: bool) -> R<Vec<(Pat, Option<Expr>)>> {
        let mut out = Vec::new();
        loop {
            let pos = self.tok.start;
            let target = self.binding_target()?;
            if matches!(kind, VarKind::Using | VarKind::AwaitUsing) && !matches!(target, Pat::Ident(_)) {
                return Err(ParseError { pos, msg: "'using' needs a plain name".into() });
            }
            self.declare_pattern(kind, &target, pos)?;
            let init = if self.eat("=")? { Some(self.assign(no_in)?) } else { None };
            out.push((target, init));
            if !self.eat(",")? {
                break;
            }
        }
        Ok(out)
    }

    /// Checks a declaration's initializers once we know it isn't a for-in/of head.
    fn require_initializers(&self, kind: VarKind, decls: &[(Pat, Option<Expr>)], pos: usize) -> R<()> {
        for (p, init) in decls {
            let needs = !matches!(p, Pat::Ident(_))
                || matches!(kind, VarKind::Const | VarKind::Using | VarKind::AwaitUsing);
            if needs && init.is_none() {
                return Err(ParseError { pos, msg: "missing initializer".into() });
            }
        }
        Ok(())
    }

    fn binding_target(&mut self) -> R<Pat> {
        match self.tok.kind {
            TokKind::Punct("[") | TokKind::Punct("{") => {
                let start = self.tok.start;
                let before = self.cover_errs.len();
                let e = self.primary()?;
                self.cover_errs.truncate(before);
                self.reinterpret_as_pattern(e, true, start)
            }
            _ => Ok(Pat::Ident(self.ident(true)?)),
        }
    }

    fn for_statement(&mut self) -> R<Stmt> {
        self.next()?;
        let mut is_await = false;
        if self.is_kw("await") {
            if !self.await_reserved() || self.f().static_block {
                return self.err("for await outside an async function");
            }
            self.next()?;
            is_await = true;
        }
        self.expect("(")?;
        self.push_scope(false);
        let r = self.for_rest(is_await);
        self.pop_scope();
        r
    }

    fn for_rest(&mut self, is_await: bool) -> R<Stmt> {
        let pos = self.tok.start;
        let mut init: Option<ForInit> = None;
        if self.is(";") {
            if is_await {
                return self.err("for await needs of");
            }
        } else {
            let decl_kind = if self.is_kw("var") {
                Some(VarKind::Var)
            } else if self.is_kw("const") {
                Some(VarKind::Const)
            } else if self.is_kw("let") && self.let_is_declaration()? {
                Some(VarKind::Let)
            } else if self.using_declaration_start()? && !self.is_kw("await") {
                Some(VarKind::Using)
            } else if self.is_kw("await") && self.using_declaration_start()? {
                self.next()?;
                Some(VarKind::AwaitUsing)
            } else {
                None
            };
            if let Some(kind) = decl_kind {
                self.next()?;
                let decls = self.var_declarations(kind, true)?;
                if (self.is_kw("of") || self.is_kw("in")) && decls.len() == 1 {
                    let of = self.is_kw("of");
                    if !of && is_await {
                        return self.err("for await needs of");
                    }
                    if !of && matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
                        return self.err("'using' in for-in");
                    }
                    let (p, initv) = decls.into_iter().next().expect("one declaration");
                    if initv.is_some() {
                        // Annex B.3.5: `for (var a = 1 in o)` in sloppy mode, simple names only.
                        if of || self.strict || kind != VarKind::Var || !matches!(p, Pat::Ident(_)) {
                            return Err(ParseError {
                                pos,
                                msg: "for-in/of declarations can't have initializers".into(),
                            });
                        }
                    }
                    self.next()?;
                    let right = if of { self.assign(false)? } else { self.expression(false)? };
                    self.expect(")")?;
                    let body = self.loop_body()?;
                    self.check_for_body_redeclarations(&p, kind, pos)?;
                    let head = ForHead::Var(kind, p);
                    return Ok(if of {
                        Stmt::ForOf { head, right, body: Box::new(body), is_await }
                    } else {
                        Stmt::ForIn { head, right, body: Box::new(body) }
                    });
                }
                self.require_initializers(kind, &decls, pos)?;
                init = Some(ForInit::Var(kind, decls));
            } else {
                let starts_let = self.is_kw("let");
                let starts_async = self.is_kw("async") && !matches!(self.peek()?.kind, TokKind::Punct("=>"));
                let before = self.cover_errs.len();
                let start = self.tok.start;
                self.cover_defer += 1;
                let e = self.lhs_or_expression(true);
                self.cover_defer -= 1;
                let e = e?;
                if self.is_kw("of") || self.is_kw("in") {
                    let of = self.is_kw("of");
                    if of && starts_let {
                        return self.err("'for (let of' is ambiguous");
                    }
                    if of && starts_async && matches!(e, Expr::Ident(ref n) if n == "async") && !is_await {
                        return self.err("'for (async of' is ambiguous");
                    }
                    if !of && is_await {
                        return self.err("for await needs of");
                    }
                    self.cover_errs.truncate(before);
                    let p = self.reinterpret_as_pattern(e, false, start)?;
                    self.next()?;
                    let right = if of { self.assign(false)? } else { self.expression(false)? };
                    self.expect(")")?;
                    let body = self.loop_body()?;
                    let head = ForHead::Pat(p);
                    return Ok(if of {
                        Stmt::ForOf { head, right, body: Box::new(body), is_await }
                    } else {
                        Stmt::ForIn { head, right, body: Box::new(body) }
                    });
                }
                if self.cover_errs.len() > before {
                    return Err(ParseError {
                        pos: self.cover_errs[before],
                        msg: "invalid shorthand property initializer".into(),
                    });
                }
                init = Some(ForInit::Expr(e));
            }
            if is_await {
                return self.err("for await needs of");
            }
        }
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression(false)?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression(false)?) };
        self.expect(")")?;
        let body = self.loop_body()?;
        Ok(Stmt::For { init, test, update, body: Box::new(body) })
    }

    /// §14.7.5.1: a for-in/of body can't `var`-declare a name the lexical head binds (the
    /// scope stack already reported lexical conflicts).
    fn check_for_body_redeclarations(&self, _p: &Pat, _kind: VarKind, _pos: usize) -> R<()> {
        Ok(())
    }

    /// A for-in/of head expression or a whole for-init expression (with `in` disallowed).
    fn lhs_or_expression(&mut self, no_in: bool) -> R<Expr> {
        let e = self.assign(no_in)?;
        if self.is(",") {
            let mut items = vec![e];
            while self.eat(",")? {
                items.push(self.assign(no_in)?);
            }
            return Ok(Expr::Seq(items));
        }
        Ok(e)
    }

    fn try_statement(&mut self) -> R<Stmt> {
        self.next()?;
        let block = self.block_body()?;
        let mut param = None;
        let mut handler = None;
        if self.eat_kw("catch")? {
            self.push_scope(false);
            if self.eat("(")? {
                let pos = self.tok.start;
                let p = self.binding_target()?;
                let mut names = Vec::new();
                bound_names(&p, &mut names);
                let mut seen = HashSet::new();
                for n in &names {
                    if !seen.insert(n.clone()) {
                        return Err(ParseError { pos, msg: format!("duplicate catch parameter '{n}'") });
                    }
                }
                let simple = matches!(p, Pat::Ident(_));
                {
                    let s = self.scopes.last_mut().expect("scope");
                    for n in names {
                        s.catch_params.insert(n.clone());
                        if simple {
                            s.catch_simple.insert(n);
                        }
                    }
                }
                param = Some(p);
                self.expect(")")?;
            }
            // The catch block shares the parameter's scope for redeclaration checks.
            self.expect("{")?;
            let body = self.statement_list(false, |p| p.is("}"))?;
            self.expect("}")?;
            self.pop_scope();
            handler = Some(body);
        }
        let finalizer = if self.eat_kw("finally")? { Some(self.block_body()?) } else { None };
        if handler.is_none() && finalizer.is_none() {
            return self.err("try without catch or finally");
        }
        Ok(Stmt::Try { block, param, handler, finalizer })
    }

    fn block_body(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        self.push_scope(false);
        let b = self.statement_list(false, |p| p.is("}"));
        self.pop_scope();
        let b = b?;
        self.expect("}")?;
        Ok(b)
    }

    fn switch_statement(&mut self) -> R<Stmt> {
        self.next()?;
        self.expect("(")?;
        let d = self.expression(false)?;
        self.expect(")")?;
        self.expect("{")?;
        self.push_scope(false);
        self.fm().break_depth += 1;
        let mut cases = Vec::new();
        let mut default_seen = false;
        let r: R<()> = (|| {
            while !self.is("}") {
                let test = if self.eat_kw("case")? {
                    Some(self.expression(false)?)
                } else if self.eat_kw("default")? {
                    if default_seen {
                        return self.err("more than one default clause");
                    }
                    default_seen = true;
                    None
                } else {
                    return self.err("expected case or default");
                };
                self.expect(":")?;
                let body =
                    self.statement_list(false, |p| p.is("}") || p.is_kw("case") || p.is_kw("default"))?;
                cases.push(Case { test, body });
            }
            Ok(())
        })();
        self.fm().break_depth -= 1;
        self.pop_scope();
        r?;
        self.expect("}")?;
        Ok(Stmt::Switch(d, cases))
    }

    // ----- modules -----

    fn module_specifier_string(&mut self) -> R<JsStr> {
        match &self.tok.kind {
            TokKind::Str(s) => {
                let s = s.clone();
                self.next()?;
                Ok(s)
            }
            _ => self.err("expected a module specifier"),
        }
    }

    /// `with { type: "json" }` import attributes.
    fn import_attributes(&mut self) -> R<()> {
        if self.is_kw("with") && !self.tok.nl_before || self.is_kw("with") {
            self.next()?;
            self.expect("{")?;
            let mut keys = HashSet::new();
            while !self.is("}") {
                let key: JsStr = match &self.tok.kind {
                    TokKind::Ident { name, .. } => utf16(name),
                    TokKind::Str(s) => s.clone(),
                    _ => return self.err("bad import attribute"),
                };
                self.next()?;
                if !keys.insert(key) {
                    return self.err("duplicate import attribute");
                }
                self.expect(":")?;
                if !matches!(self.tok.kind, TokKind::Str(_)) {
                    return self.err("import attribute values are strings");
                }
                self.next()?;
                if !self.eat(",")? {
                    break;
                }
            }
            self.expect("}")?;
        }
        Ok(())
    }

    /// A ModuleExportName: an identifier name or a string (well-formed, no lone surrogates).
    fn export_name(&mut self) -> R<(JsStr, bool)> {
        match &self.tok.kind {
            TokKind::Str(s) => {
                let s = s.clone();
                if String::from_utf16(&s).is_err() {
                    return self.err("module export names must be well-formed");
                }
                self.next()?;
                Ok((s, true))
            }
            TokKind::Ident { name, .. } => {
                let n = utf16(name);
                self.next()?;
                Ok((n, false))
            }
            _ => self.err("expected a name"),
        }
    }

    fn import_declaration(&mut self) -> R<Stmt> {
        self.next()?;
        let mut specs = Vec::new();
        if let TokKind::Str(_) = self.tok.kind {
            let source = self.module_specifier_string()?;
            self.import_attributes()?;
            self.semicolon()?;
            return Ok(Stmt::Import { specs, source });
        }
        let bind = |p: &mut Parser, name: String, pos: usize| -> R<String> {
            p.check_ident(&name, true, pos)?;
            p.declare_lexical(&name, pos)?;
            Ok(name)
        };
        // Phases: `import source x from "m"`, `import defer * as ns from "m"`.
        if self.is_kw("defer") && matches!(self.peek()?.kind, TokKind::Punct("*")) {
            self.next()?;
            self.next()?;
            if !self.eat_kw("as")? {
                return self.err("expected as");
            }
            let pos = self.tok.start;
            let n = self.ident_name()?;
            specs.push(ImportSpec::DeferNamespace(bind(self, n, pos)?));
            return self.import_from(specs);
        }
        if self.is_kw("source") {
            let n = self.peek()?;
            let is_phase = match &n.kind {
                TokKind::Ident { name, .. } if name != "from" => true,
                // `import source from from "m"` binds `from`.
                TokKind::Ident { .. } => {
                    let save = (self.lx.pos, self.tok.clone(), self.prev_end);
                    self.next()?;
                    self.next()?;
                    let r = kw(&self.tok, "from");
                    self.lx.pos = save.0;
                    self.tok = save.1;
                    self.prev_end = save.2;
                    r
                }
                _ => false,
            };
            if is_phase {
                self.next()?;
                let pos = self.tok.start;
                let n = self.ident_name()?;
                specs.push(ImportSpec::Source(bind(self, n, pos)?));
                return self.import_from(specs);
            }
        }
        if let TokKind::Ident { .. } = self.tok.kind {
            if !self.is_kw("from")
                || matches!(self.peek()?.kind, TokKind::Punct(","))
                || kw(&self.peek()?, "from")
            {
                let pos = self.tok.start;
                let n = self.ident_name()?;
                specs.push(ImportSpec::Default(bind(self, n, pos)?));
                if !self.eat(",")? {
                    return self.import_from(specs);
                }
            }
        }
        if self.eat("*")? {
            if !self.eat_kw("as")? {
                return self.err("expected as");
            }
            let pos = self.tok.start;
            let n = self.ident_name()?;
            specs.push(ImportSpec::Namespace(bind(self, n, pos)?));
        } else if self.eat("{")? {
            while !self.is("}") {
                let pos = self.tok.start;
                let (imported, is_str) = self.export_name()?;
                let local = if self.eat_kw("as")? {
                    let p2 = self.tok.start;
                    let n = self.ident_name()?;
                    bind(self, n, p2)?
                } else {
                    if is_str {
                        return self.err("a string import name needs 'as'");
                    }
                    let n = String::from_utf16_lossy(&imported);
                    bind(self, n, pos)?
                };
                specs.push(ImportSpec::Named { imported, local });
                if !self.eat(",")? {
                    break;
                }
            }
            self.expect("}")?;
        } else if specs.is_empty() {
            return self.err("bad import");
        }
        self.import_from(specs)
    }

    fn import_from(&mut self, specs: Vec<ImportSpec>) -> R<Stmt> {
        if !self.eat_kw("from")? {
            return self.err("expected from");
        }
        let source = self.module_specifier_string()?;
        self.import_attributes()?;
        self.semicolon()?;
        Ok(Stmt::Import { specs, source })
    }

    fn add_export(&mut self, name: JsStr, pos: usize) -> R<()> {
        if !self.exported.insert(name.clone()) {
            return Err(ParseError {
                pos,
                msg: format!("duplicate export '{}'", String::from_utf16_lossy(&name)),
            });
        }
        Ok(())
    }

    fn export_declaration(&mut self) -> R<Stmt> {
        let start = self.tok.start;
        self.next()?;
        if self.eat("*")? {
            let mut specs = Vec::new();
            if self.eat_kw("as")? {
                let pos = self.tok.start;
                let (n, _) = self.export_name()?;
                self.add_export(n.clone(), pos)?;
                specs.push((n, utf16("*")));
            }
            if !self.eat_kw("from")? {
                return self.err("expected from");
            }
            let source = self.module_specifier_string()?;
            self.import_attributes()?;
            self.semicolon()?;
            return Ok(Stmt::ExportNamed { specs, source: Some(source), star: true });
        }
        if self.eat("{")? {
            let mut specs = Vec::new();
            let mut locals = Vec::new();
            while !self.is("}") {
                let pos = self.tok.start;
                let (local, is_str) = self.export_name()?;
                let exported = if self.eat_kw("as")? { self.export_name()?.0 } else { local.clone() };
                self.add_export(exported.clone(), pos)?;
                locals.push((local.clone(), is_str, pos));
                specs.push((exported, local));
                if !self.eat(",")? {
                    break;
                }
            }
            self.expect("}")?;
            let source = if self.eat_kw("from")? {
                let s = self.module_specifier_string()?;
                self.import_attributes()?;
                Some(s)
            } else {
                for (l, is_str, pos) in locals {
                    if is_str {
                        return Err(ParseError { pos, msg: "a string can't name a local binding".into() });
                    }
                    let name = String::from_utf16_lossy(&l);
                    if RESERVED.contains(&name.as_str())
                        || STRICT_RESERVED.contains(&name.as_str())
                        || name == "await"
                    {
                        return Err(ParseError { pos, msg: format!("'{name}' isn't a valid local name") });
                    }
                    self.export_locals.push((name, pos));
                }
                None
            };
            self.semicolon()?;
            return Ok(Stmt::ExportNamed { specs, source, star: false });
        }
        if self.is("@") {
            let d = self.decorators()?;
            if !self.is_kw("class") && !self.is_kw("default") {
                return self.err("decorators must come before a class");
            }
            self.pending_decorators.extend(d);
        }
        if self.eat_kw("default")? {
            self.add_export(utf16("default"), start)?;
            if self.is("@") {
                let d = self.decorators()?;
                self.pending_decorators.extend(d);
            }
            let pos = self.tok.start;
            if self.is_kw("function")
                || (self.is_kw("async") && kw(&self.peek()?, "function") && !self.peek()?.nl_before)
            {
                let is_async = self.eat_kw("async")?;
                self.next()?;
                let is_gen = self.eat("*")?;
                let name = if let TokKind::Ident { .. } = self.tok.kind {
                    let p = self.tok.start;
                    let n = self.ident(true)?;
                    self.declare_function(&n, p)?;
                    Some(n)
                } else {
                    None
                };
                let f = self.function_rest(name, is_async, is_gen, FuncKind::Normal, pos)?;
                return Ok(Stmt::ExportDefault(Box::new(Stmt::Function(f))));
            }
            if self.is_kw("class") {
                let c = self.class(false)?;
                if let Some(n) = &c.name {
                    self.declare_lexical(&n.clone(), pos)?;
                }
                return Ok(Stmt::ExportDefault(Box::new(Stmt::Class(c))));
            }
            let e = self.assign(false)?;
            self.semicolon()?;
            return Ok(Stmt::ExportDefaultExpr(e));
        }
        let pos = self.tok.start;
        let decl = if self.is_kw("var") {
            self.next()?;
            let d = self.var_declarations(VarKind::Var, false)?;
            self.semicolon()?;
            Stmt::Var(VarKind::Var, d)
        } else if self.is_kw("let") || self.is_kw("const") {
            let kind = if self.is_kw("let") { VarKind::Let } else { VarKind::Const };
            self.next()?;
            let d = self.var_declarations(kind, false)?;
            self.require_initializers(kind, &d, pos)?;
            self.semicolon()?;
            Stmt::Var(kind, d)
        } else if self.is_kw("function") || self.is_kw("async") || self.is_kw("class") {
            self.statement_item()?
        } else {
            return self.err("bad export");
        };
        let mut names = Vec::new();
        match &decl {
            Stmt::Var(_, ds) => {
                for (p, _) in ds {
                    bound_names(p, &mut names);
                }
            }
            Stmt::Function(f) => names.extend(f.name.clone()),
            Stmt::Class(c) => names.extend(c.name.clone()),
            _ => {}
        }
        for n in names {
            self.add_export(utf16(&n), pos)?;
        }
        Ok(Stmt::ExportDecl(Box::new(decl)))
    }

    // ----- functions -----

    /// Parameters and body after the name. `start`: source start (for toString).
    fn function_rest(
        &mut self,
        name: Option<String>,
        is_async: bool,
        is_generator: bool,
        kind: FuncKind,
        start: usize,
    ) -> R<Rc<Function>> {
        let outer = self.f().clone();
        let method_like = !matches!(kind, FuncKind::Normal);
        self.fns.push(FnCtx {
            is_async,
            is_generator,
            is_arrow: false,
            can_return: true,
            new_target: true,
            super_prop: method_like,
            super_call: kind == FuncKind::DerivedConstructor,
            no_arguments: false,
            static_block: false,
            labels: Vec::new(),
            loop_depth: 0,
            break_depth: 0,
        });
        let _ = outer;
        let r = self.function_params_and_body(name, is_async, is_generator, kind, start);
        self.fns.pop();
        r
    }

    fn function_params_and_body(
        &mut self,
        name: Option<String>,
        is_async: bool,
        is_generator: bool,
        kind: FuncKind,
        start: usize,
    ) -> R<Rc<Function>> {
        let outer_strict = self.strict;
        self.push_scope(true);
        self.expect("(")?;
        let params_pos = self.tok.start;
        let (params, rest) = self.formal_params()?;
        self.expect(")")?;
        match kind {
            FuncKind::Getter if !params.is_empty() || rest.is_some() => {
                return Err(ParseError { pos: params_pos, msg: "getters take no parameters".into() })
            }
            FuncKind::Setter if params.len() != 1 || rest.is_some() => {
                return Err(ParseError { pos: params_pos, msg: "setters take exactly one parameter".into() })
            }
            _ => {}
        }
        let body = self.function_body(&params, &rest, params_pos, kind != FuncKind::Normal)?;
        let strict = self.strict;
        if strict && !outer_strict {
            // The name and parameters must also obey strict mode (§15.2.1).
            if let Some(n) = &name {
                if n == "eval" || n == "arguments" || STRICT_RESERVED.contains(&n.as_str()) {
                    return Err(ParseError {
                        pos: start,
                        msg: format!("'{n}' can't name a strict function"),
                    });
                }
            }
        }
        self.strict = outer_strict;
        self.pop_scope();
        Ok(Rc::new(Function {
            name,
            params,
            rest,
            body,
            expression_body: false,
            kind,
            is_async,
            is_generator,
            strict,
            span: (start, self.prev_end),
        }))
    }

    fn formal_params(&mut self) -> R<(Vec<Pat>, Option<Pat>)> {
        let saved = self.in_params;
        self.in_params = true;
        let r = self.formal_params_inner();
        self.in_params = saved;
        r
    }

    fn formal_params_inner(&mut self) -> R<(Vec<Pat>, Option<Pat>)> {
        let mut params = Vec::new();
        let mut rest = None;
        while !self.is(")") {
            if self.eat("...")? {
                rest = Some(self.binding_target()?);
                if self.is(",") {
                    return self.err("a rest parameter must be last");
                }
                break;
            }
            let mut p = self.binding_target()?;
            if self.eat("=")? {
                let d = self.assign(false)?;
                p = Pat::Default(Box::new(p), Box::new(d));
            }
            params.push(p);
            if !self.eat(",")? {
                break;
            }
        }
        // yield/await expressions in parameters.
        Ok((params, rest))
    }

    /// `{ body }` with the parameter checks that depend on its strictness.
    fn function_body(
        &mut self,
        params: &[Pat],
        rest: &Option<Pat>,
        params_pos: usize,
        unique: bool,
    ) -> R<Vec<Stmt>> {
        self.expect("{")?;
        let was_strict = self.strict;
        let mut names = Vec::new();
        for p in params {
            bound_names(p, &mut names);
        }
        if let Some(r) = rest {
            bound_names(r, &mut names);
        }
        {
            let s = self.scopes.last_mut().expect("scope");
            s.params = names.iter().cloned().collect();
        }
        self.saw_use_strict = false;
        let body = self.statement_list(true, |p| p.is("}"))?;
        self.expect("}")?;
        let simple = simple_params(params, rest);
        let _ = was_strict;
        if self.saw_use_strict && !simple {
            return Err(ParseError {
                pos: params_pos,
                msg: "\"use strict\" in a function with non-simple parameters".into(),
            });
        }
        self.check_params(&names, simple, unique, params_pos)?;
        Ok(body)
    }

    fn check_params(&self, names: &[String], simple: bool, unique: bool, pos: usize) -> R<()> {
        let mut seen = HashSet::new();
        for n in names {
            if self.strict {
                if n == "eval" || n == "arguments" {
                    return Err(ParseError {
                        pos,
                        msg: format!("'{n}' can't be a parameter in strict mode"),
                    });
                }
                if STRICT_RESERVED.contains(&n.as_str()) {
                    return Err(ParseError { pos, msg: format!("'{n}' is reserved in strict mode") });
                }
            }
            if !seen.insert(n) && (self.strict || !simple || unique) {
                return Err(ParseError { pos, msg: format!("duplicate parameter '{n}'") });
            }
        }
        Ok(())
    }

    /// An arrow function from its parameter list.
    fn arrow(
        &mut self,
        params: Vec<Pat>,
        rest: Option<Pat>,
        is_async: bool,
        start: usize,
        params_pos: usize,
    ) -> R<Expr> {
        if self.tok.nl_before {
            return self.err("line break before =>");
        }
        self.expect("=>")?;
        let outer = self.f().clone();
        self.fns.push(FnCtx {
            is_async,
            is_generator: false,
            is_arrow: true,
            can_return: true,
            new_target: outer.new_target,
            super_prop: outer.super_prop,
            super_call: outer.super_call,
            no_arguments: false,
            static_block: false,
            labels: Vec::new(),
            loop_depth: 0,
            break_depth: 0,
        });
        let outer_strict = self.strict;
        self.push_scope(true);
        let r: R<(Vec<Stmt>, bool)> = (|| {
            let mut names = Vec::new();
            for p in &params {
                bound_names(p, &mut names);
            }
            if let Some(r) = &rest {
                bound_names(r, &mut names);
            }
            for n in &names {
                self.check_ident(n, true, params_pos)?;
                if n == "await" && (is_async || self.module) {
                    return Err(ParseError {
                        pos: params_pos,
                        msg: "'await' as an async arrow parameter".into(),
                    });
                }
            }
            if self.is("{") {
                let body = self.function_body(&params, &rest, params_pos, true)?;
                Ok((body, false))
            } else {
                self.check_params(&names, simple_params(&params, &rest), true, params_pos)?;
                let e = self.assign(false)?;
                Ok((vec![Stmt::Return(Some(e))], true))
            }
        })();
        let strict = self.strict;
        self.strict = outer_strict;
        self.pop_scope();
        self.fns.pop();
        let (body, expression_body) = r?;
        Ok(Expr::Arrow(Rc::new(Function {
            name: None,
            params,
            rest,
            body,
            expression_body,
            kind: FuncKind::Arrow,
            is_async,
            is_generator: false,
            strict,
            span: (start, self.prev_end),
        })))
    }

    // ----- classes -----

    fn class(&mut self, declaration: bool) -> R<Rc<Class>> {
        let start = self.tok.start;
        self.next()?; // class
        let outer_strict = self.strict;
        self.strict = true;
        let decorators = std::mem::take(&mut self.pending_decorators);
        let outer_members = std::mem::take(&mut self.pending_member_decorators);
        self.pending_decorators = decorators;
        let r = self.class_rest(declaration, start);
        self.pending_member_decorators = outer_members;
        self.strict = outer_strict;
        r
    }

    /// Stage 3 decorators: `@a.b`, `@a.b(args)`, `@(expr)`.
    fn decorators(&mut self) -> R<Vec<Expr>> {
        let mut out = Vec::new();
        while self.eat("@")? {
            let saved_strict = self.strict;
            let e = if self.eat("(")? {
                let e = self.expression(false)?;
                self.expect(")")?;
                e
            } else {
                let pos = self.tok.start;
                let name = match &self.tok.kind {
                    TokKind::Ident { name, .. } => name.clone(),
                    _ => return self.err("expected a decorator"),
                };
                self.check_ident(&name, false, pos)?;
                self.next()?;
                let mut e = Expr::Ident(name);
                while self.eat(".")? {
                    e = self.member_dot(e, false)?;
                }
                if self.eat("(")? {
                    let args = self.arguments()?;
                    e = Expr::Call { callee: Box::new(e), args, optional: false };
                }
                e
            };
            self.strict = saved_strict;
            out.push(e);
        }
        Ok(out)
    }

    fn class_rest(&mut self, declaration: bool, start: usize) -> R<Rc<Class>> {
        let name = if matches!(self.tok.kind, TokKind::Ident { .. }) && !self.is_kw("extends") {
            let pos = self.tok.start;
            let n = self.ident(true)?;
            if declaration {
                self.declare_lexical(&n, pos)?;
            }
            Some(n)
        } else if declaration {
            return self.err("a class declaration needs a name");
        } else {
            None
        };
        let extends = if self.eat_kw("extends")? { Some(Box::new(self.lhs_expression()?)) } else { None };
        self.expect("{")?;
        self.classes.push(ClassPrivates::default());
        let r = self.class_body(extends.is_some());
        let privs = self.classes.pop().unwrap_or_default();
        let (constructor, members) = r?;
        // Private names used here but not declared: the enclosing class may declare them.
        for (n, pos) in privs.used {
            if !privs.declared.contains_key(&n) {
                match self.classes.last_mut() {
                    Some(outer) => outer.used.push((n, pos)),
                    None => return Err(ParseError { pos, msg: format!("undeclared private name #{n}") }),
                }
            }
        }
        self.expect("}")?;
        Ok(Rc::new(Class {
            name,
            extends,
            constructor,
            members,
            decorators: std::mem::take(&mut self.pending_decorators),
            member_decorators: std::mem::take(&mut self.pending_member_decorators),
            span: (start, self.prev_end),
        }))
    }

    fn class_body(&mut self, derived: bool) -> R<(Option<Rc<Function>>, Vec<ClassMember>)> {
        let mut members = Vec::new();
        let mut constructor = None;
        while !self.is("}") {
            if self.eat(";")? {
                continue;
            }
            let member_decorators = self.decorators()?;
            if !member_decorators.is_empty() {
                self.pending_member_decorators.push((members.len(), member_decorators));
            }
            let start = self.tok.start;
            let mut is_static = false;
            if self.is_kw("static") {
                let n = self.peek()?;
                if !matches!(
                    n.kind,
                    TokKind::Punct("(") | TokKind::Punct("=") | TokKind::Punct(";") | TokKind::Punct("}")
                ) && !(n.nl_before && false)
                {
                    self.next()?;
                    is_static = true;
                    if self.is("{") {
                        members.push(ClassMember::StaticBlock(self.static_block(start)?));
                        continue;
                    }
                }
            }
            let (mut is_async, mut is_gen, mut accessor) = (false, false, None::<FuncKind>);
            if self.is_kw("async") {
                let n = self.peek()?;
                if !n.nl_before
                    && !matches!(
                        n.kind,
                        TokKind::Punct("(") | TokKind::Punct("=") | TokKind::Punct(";") | TokKind::Punct("}")
                    )
                {
                    self.next()?;
                    is_async = true;
                }
            }
            if self.eat("*")? {
                is_gen = true;
            }
            if !is_async && !is_gen && (self.is_kw("get") || self.is_kw("set")) {
                let n = self.peek()?;
                if !matches!(
                    n.kind,
                    TokKind::Punct("(") | TokKind::Punct("=") | TokKind::Punct(";") | TokKind::Punct("}")
                ) {
                    accessor = Some(if self.is_kw("get") { FuncKind::Getter } else { FuncKind::Setter });
                    self.next()?;
                }
            }
            let key_pos = self.tok.start;
            let key = self.property_key(true)?;
            let key_name = match &key {
                PropKey::Ident(n) => Some(n.clone()),
                PropKey::Str(s) => Some(String::from_utf16_lossy(s)),
                _ => None,
            };
            if let PropKey::Private(n) = &key {
                if n == "constructor" {
                    return Err(ParseError { pos: key_pos, msg: "#constructor isn't allowed".into() });
                }
                let kind = match accessor {
                    Some(FuncKind::Getter) => "get",
                    Some(FuncKind::Setter) => "set",
                    _ => "other",
                };
                let tag: &'static str = match (kind, is_static) {
                    ("get", false) => "get",
                    ("set", false) => "set",
                    ("get", true) => "sget",
                    ("set", true) => "sset",
                    _ => "other",
                };
                let privs = self.classes.last_mut().expect("class");
                match privs.declared.get(n).copied() {
                    None => {
                        privs.declared.insert(n.clone(), tag);
                    }
                    Some(prev)
                        if (prev == "get" && tag == "set")
                            || (prev == "set" && tag == "get")
                            || (prev == "sget" && tag == "sset")
                            || (prev == "sset" && tag == "sget") =>
                    {
                        privs.declared.insert(n.clone(), "pair");
                    }
                    Some(_) => {
                        return Err(ParseError { pos: key_pos, msg: format!("duplicate private name #{n}") })
                    }
                }
            }
            if self.is("(") {
                let is_ctor = !is_static
                    && key_name.as_deref() == Some("constructor")
                    && !matches!(key, PropKey::Computed(_));
                if is_ctor {
                    if is_async || is_gen || accessor.is_some() {
                        return Err(ParseError {
                            pos: key_pos,
                            msg: "the constructor can't be a generator, async or an accessor".into(),
                        });
                    }
                    if constructor.is_some() {
                        return Err(ParseError { pos: key_pos, msg: "duplicate constructor".into() });
                    }
                    let kind =
                        if derived { FuncKind::DerivedConstructor } else { FuncKind::ClassConstructor };
                    constructor = Some(self.function_rest(None, false, false, kind, start)?);
                    continue;
                }
                if is_static
                    && key_name.as_deref() == Some("prototype")
                    && !matches!(key, PropKey::Computed(_))
                {
                    return Err(ParseError { pos: key_pos, msg: "static 'prototype' method".into() });
                }
                let kind = accessor.unwrap_or(FuncKind::Method);
                let func = self.function_rest(None, is_async, is_gen, kind, start)?;
                members.push(ClassMember::Method { key, is_static, kind, func });
                continue;
            }
            if is_async || is_gen || accessor.is_some() {
                return self.err("expected (");
            }
            // A field.
            if !matches!(key, PropKey::Computed(_) | PropKey::Private(_)) {
                if key_name.as_deref() == Some("constructor")
                    || (is_static && key_name.as_deref() == Some("prototype"))
                {
                    return Err(ParseError { pos: key_pos, msg: "bad field name".into() });
                }
            }
            let value = if self.eat("=")? { Some(self.field_initializer(start)?) } else { None };
            self.semicolon()?;
            members.push(ClassMember::Field { key, is_static, value });
        }
        Ok((constructor, members))
    }

    fn field_initializer(&mut self, start: usize) -> R<Rc<Function>> {
        self.fns.push(FnCtx {
            is_async: false,
            is_generator: false,
            is_arrow: false,
            can_return: false,
            new_target: true,
            super_prop: true,
            super_call: false,
            no_arguments: true,
            static_block: false,
            labels: Vec::new(),
            loop_depth: 0,
            break_depth: 0,
        });
        self.push_scope(true);
        let e = self.assign(false);
        self.pop_scope();
        self.fns.pop();
        let e = e?;
        Ok(Rc::new(Function {
            name: None,
            params: Vec::new(),
            rest: None,
            body: vec![Stmt::Return(Some(e))],
            expression_body: true,
            kind: FuncKind::ClassField,
            is_async: false,
            is_generator: false,
            strict: true,
            span: (start, self.prev_end),
        }))
    }

    fn static_block(&mut self, start: usize) -> R<Rc<Function>> {
        self.fns.push(FnCtx {
            is_async: false,
            is_generator: false,
            is_arrow: false,
            can_return: false,
            new_target: true,
            super_prop: true,
            super_call: false,
            no_arguments: true,
            static_block: true,
            labels: Vec::new(),
            loop_depth: 0,
            break_depth: 0,
        });
        self.push_scope(true);
        let r: R<Vec<Stmt>> = (|| {
            self.expect("{")?;
            let b = self.statement_list(false, |p| p.is("}"))?;
            self.expect("}")?;
            Ok(b)
        })();
        self.pop_scope();
        self.fns.pop();
        Ok(Rc::new(Function {
            name: None,
            params: Vec::new(),
            rest: None,
            body: r?,
            expression_body: false,
            kind: FuncKind::Method,
            is_async: false,
            is_generator: false,
            strict: true,
            span: (start, self.prev_end),
        }))
    }

    /// A property name (§13.2.5 PropertyName), or a private name in classes.
    fn property_key(&mut self, allow_private: bool) -> R<PropKey> {
        let k = match &self.tok.kind {
            TokKind::Ident { name, .. } => PropKey::Ident(name.clone()),
            TokKind::Str(s) => {
                if self.strict && self.tok.legacy_octal {
                    return self.err("octal escape in strict mode");
                }
                PropKey::Str(s.clone())
            }
            TokKind::Num(n) => {
                if self.strict && self.tok.legacy_octal {
                    return self.err("legacy octal literal in strict mode");
                }
                PropKey::Num(*n)
            }
            TokKind::BigInt(b) => PropKey::BigInt(b.clone()),
            TokKind::Private(n) if allow_private => PropKey::Private(n.clone()),
            TokKind::Punct("[") => {
                self.next()?;
                let e = self.assign(false)?;
                self.expect("]")?;
                return Ok(PropKey::Computed(Box::new(e)));
            }
            _ => return self.err("expected a property name"),
        };
        self.next()?;
        Ok(k)
    }

    // ----- expressions -----

    pub fn expression(&mut self, no_in: bool) -> R<Expr> {
        let e = self.assign(no_in)?;
        if self.is(",") {
            let mut items = vec![e];
            while self.eat(",")? {
                items.push(self.assign(no_in)?);
            }
            return Ok(Expr::Seq(items));
        }
        Ok(e)
    }

    /// AssignmentExpression, with the cover grammars for arrows and destructuring.
    pub fn assign(&mut self, no_in: bool) -> R<Expr> {
        let start = self.tok.start;
        let before = self.cover_errs.len();
        // yield
        if self.is_kw("yield") && self.f().is_generator {
            return self.yield_expr(no_in);
        }
        // async arrows: `async x => …`, `async (…) => …`
        if self.is_kw("async") {
            let n = self.peek()?;
            if !n.nl_before {
                if let TokKind::Ident { name, .. } = &n.kind {
                    if name != "function" {
                        self.next()?;
                        let ppos = self.tok.start;
                        let p = self.ident(true)?;
                        if p == "await" {
                            return Err(ParseError {
                                pos: ppos,
                                msg: "'await' as an async arrow parameter".into(),
                            });
                        }
                        if !self.is("=>") {
                            return self.err("expected =>");
                        }
                        return self.arrow(vec![Pat::Ident(p)], None, true, start, ppos);
                    }
                }
            }
        }
        let ya_before = self.yield_await_seen.len();
        self.last_paren_start = None;
        let lhs = self.conditional(no_in)?;
        if self.is("=>") {
            let ppos = start;
            // Arrow parameters can't contain yield/await expressions.
            if self.yield_await_seen.len() > ya_before {
                return self.err("yield/await in arrow parameters");
            }
            match lhs {
                Expr::Ident(name) => {
                    self.cover_errs.truncate(before);
                    return self.arrow(vec![Pat::Ident(name)], None, false, start, ppos);
                }
                Expr::Paren(_) if self.last_paren_start == Some(start) => {
                    let cover = self.covers.remove(&start).expect("paren cover");
                    self.cover_errs.truncate(before);
                    let mut params = Vec::new();
                    for item in cover.items {
                        params.push(self.reinterpret_as_pattern(item, true, start)?);
                    }
                    return self.arrow(params, cover.rest, false, start, ppos);
                }
                Expr::Call { callee, args, optional: false }
                    if matches!(*callee, Expr::Ident(ref n) if n == "async")
                        && self.async_call_start == Some(start) =>
                {
                    self.cover_errs.truncate(before);
                    let mut params = Vec::new();
                    let mut rest = None;
                    let n = args.len();
                    for (i, a) in args.into_iter().enumerate() {
                        if let Expr::Spread(inner) = a {
                            if i + 1 != n {
                                return self.err("rest parameter must be last");
                            }
                            rest = Some(self.reinterpret_as_pattern(*inner, true, start)?);
                        } else {
                            params.push(self.reinterpret_as_pattern(a, true, start)?);
                        }
                    }
                    if self.async_call_trailing_comma && rest.is_some() {
                        return self.err("trailing comma after rest parameter");
                    }
                    return self.arrow(params, rest, true, start, ppos);
                }
                _ => return self.err("invalid arrow parameters"),
            }
        }
        if let Some(c) = self.covers.remove(&start) {
            if c.arrow_only {
                return Err(ParseError { pos: start, msg: "expected =>".into() });
            }
        }
        if let TokKind::Punct(op) = self.tok.kind {
            if matches!(
                op,
                "=" | "+="
                    | "-="
                    | "*="
                    | "/="
                    | "%="
                    | "**="
                    | "<<="
                    | ">>="
                    | ">>>="
                    | "&="
                    | "|="
                    | "^="
                    | "&&="
                    | "||="
                    | "??="
            ) {
                let target = if op == "=" && matches!(lhs, Expr::Object(_) | Expr::Array(_)) {
                    self.cover_errs.truncate(before);
                    self.reinterpret_as_pattern(lhs, false, start)?
                } else {
                    self.simple_target(lhs, start, op == "=")?
                };
                self.next()?;
                let value = self.assign(no_in)?;
                return Ok(Expr::Assign { op, target: Box::new(target), value: Box::new(value) });
            }
        }
        if self.owned_cover(before) && self.cover_errs.len() > before {
            return Err(ParseError {
                pos: self.cover_errs[before],
                msg: "invalid shorthand property initializer".into(),
            });
        }
        Ok(lhs)
    }

    /// Whether the cover errors recorded after `before` belong to this assignment expression
    /// (nested array/object/paren parsing defers them to the outermost one).
    fn owned_cover(&self, _before: usize) -> bool {
        self.cover_defer == 0
    }

    fn yield_expr(&mut self, no_in: bool) -> R<Expr> {
        let pos = self.tok.start;
        if self.in_params {
            return self.err("yield in parameters");
        }
        self.next()?;
        self.yield_await_seen.push(pos);
        let delegate = !self.tok.nl_before && self.eat("*")?;
        let starts_expr = !self.tok.nl_before
            && !matches!(
                self.tok.kind,
                TokKind::Punct(")")
                    | TokKind::Punct("]")
                    | TokKind::Punct("}")
                    | TokKind::Punct(",")
                    | TokKind::Punct(";")
                    | TokKind::Punct(":")
                    | TokKind::Eof
            )
            && !(no_in && self.is_kw("in"))
            && !self.is("=>");
        let arg = if delegate || starts_expr { Some(Box::new(self.assign(no_in)?)) } else { None };
        Ok(Expr::Yield { arg, delegate })
    }

    /// A simple assignment target (§13.15.1): an identifier (not eval/arguments in strict) or a
    /// member expression; parentheses around those are allowed.
    fn simple_target(&mut self, e: Expr, pos: usize, _plain: bool) -> R<Pat> {
        match e {
            Expr::Ident(n) => {
                if self.strict && (n == "eval" || n == "arguments") {
                    return Err(ParseError { pos, msg: format!("can't assign to '{n}' in strict mode") });
                }
                Ok(Pat::Ident(n))
            }
            Expr::Member { optional: false, .. }
            | Expr::PrivateMember { optional: false, .. }
            | Expr::SuperMember { .. } => Ok(Pat::Expr(Box::new(e))),
            Expr::Paren(inner) => match *inner {
                Expr::Object(_) | Expr::Array(_) => {
                    Err(ParseError { pos, msg: "invalid assignment target".into() })
                }
                other => self.simple_target(other, pos, _plain),
            },
            // Annex B: `f() = 1` is a runtime ReferenceError in sloppy scripts, but an early
            // error everywhere since ES2024 for logical assignment; we reject it uniformly.
            _ => Err(ParseError { pos, msg: "invalid assignment target".into() }),
        }
    }

    /// Reinterpret an expression as a destructuring pattern (§13.15.5, §14.3.3).
    fn reinterpret_as_pattern(&mut self, e: Expr, binding: bool, pos: usize) -> R<Pat> {
        let bad = || Err(ParseError { pos, msg: "invalid destructuring target".into() });
        match e {
            Expr::Ident(n) => {
                self.check_ident(&n, binding, pos)?;
                Ok(Pat::Ident(n))
            }
            Expr::Assign { op: "=", target, value } => {
                let t = match *target {
                    Pat::Expr(inner) if binding => {
                        return self
                            .reinterpret_as_pattern(
                                Expr::Assign { op: "=", target: Box::new(Pat::Expr(inner)), value },
                                false,
                                pos,
                            )
                            .and(bad())
                    }
                    other => {
                        if binding {
                            self.pattern_as_binding(other, pos)?
                        } else {
                            other
                        }
                    }
                };
                Ok(Pat::Default(Box::new(t), value))
            }
            Expr::Array(items) => {
                let mut els = Vec::new();
                let mut rest = None;
                let n = items.len();
                for (i, it) in items.into_iter().enumerate() {
                    match it {
                        None => els.push(None),
                        Some(Expr::Spread(inner)) => {
                            if i + 1 != n {
                                return bad();
                            }
                            let p = self.reinterpret_as_pattern(*inner, binding, pos)?;
                            if matches!(p, Pat::Default(..)) {
                                return bad();
                            }
                            rest = Some(Box::new(p));
                        }
                        Some(x) => els.push(Some(self.reinterpret_as_pattern(x, binding, pos)?)),
                    }
                }
                if rest.is_some() && self.array_spread_trailing_comma.contains(&pos) {
                    return bad();
                }
                Ok(Pat::Array(els, rest))
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                let n = props.len();
                for (i, p) in props.into_iter().enumerate() {
                    match p {
                        Prop::KeyValue(k, v) => out.push((k, self.reinterpret_as_pattern(v, binding, pos)?)),
                        Prop::Shorthand(name, init) => {
                            self.check_ident(&name, binding, pos)?;
                            let t = Pat::Ident(name.clone());
                            let t = match init {
                                Some(d) => Pat::Default(Box::new(t), d),
                                None => t,
                            };
                            out.push((PropKey::Ident(name), t));
                        }
                        Prop::Spread(inner) => {
                            if i + 1 != n {
                                return bad();
                            }
                            let p = self.reinterpret_as_pattern(inner, binding, pos)?;
                            if !matches!(p, Pat::Ident(_) | Pat::Expr(_)) {
                                return bad();
                            }
                            rest = Some(Box::new(p));
                        }
                        _ => return bad(),
                    }
                }
                Ok(Pat::Object(out, rest))
            }
            Expr::Member { optional: false, .. }
            | Expr::PrivateMember { optional: false, .. }
            | Expr::SuperMember { .. }
                if !binding =>
            {
                Ok(Pat::Expr(Box::new(e)))
            }
            Expr::Paren(inner) if !binding => match *inner {
                Expr::Ident(_)
                | Expr::Member { .. }
                | Expr::PrivateMember { .. }
                | Expr::SuperMember { .. } => self.reinterpret_as_pattern(*inner, false, pos),
                _ => bad(),
            },
            _ => bad(),
        }
    }

    fn pattern_as_binding(&mut self, p: Pat, pos: usize) -> R<Pat> {
        match p {
            Pat::Ident(n) => {
                self.check_ident(&n, true, pos)?;
                Ok(Pat::Ident(n))
            }
            Pat::Expr(_) => Err(ParseError { pos, msg: "invalid binding".into() }),
            other => Ok(other),
        }
    }

    fn conditional(&mut self, no_in: bool) -> R<Expr> {
        let start = self.tok.start;
        let test = self.binary(0, no_in)?;
        if self.eat("?")? {
            let a = self.assign(false)?;
            self.expect(":")?;
            let b = self.assign(no_in)?;
            let _ = start;
            return Ok(Expr::Cond(Box::new(test), Box::new(a), Box::new(b)));
        }
        Ok(test)
    }

    fn binary_prec(&self, no_in: bool) -> Option<(&'static str, u8)> {
        let op = match &self.tok.kind {
            TokKind::Punct(p) => *p,
            TokKind::Ident { name, escaped: false } if name == "instanceof" => "instanceof",
            TokKind::Ident { name, escaped: false } if name == "in" && !no_in => "in",
            _ => return None,
        };
        let p = match op {
            "??" => 1,
            "||" => 2,
            "&&" => 3,
            "|" => 4,
            "^" => 5,
            "&" => 6,
            "==" | "!=" | "===" | "!==" => 7,
            "<" | ">" | "<=" | ">=" | "instanceof" | "in" => 8,
            "<<" | ">>" | ">>>" => 9,
            "+" | "-" => 10,
            "*" | "/" | "%" => 11,
            "**" => 12,
            _ => return None,
        };
        Some((op, p))
    }

    fn binary(&mut self, min: u8, no_in: bool) -> R<Expr> {
        // `#x in obj`
        let mut left = if let TokKind::Private(name) = &self.tok.kind {
            let name = name.clone();
            let pos = self.tok.start;
            self.next()?;
            if !self.is_kw("in") || no_in || min > 8 {
                return Err(ParseError { pos, msg: "unexpected private name".into() });
            }
            self.use_private(&name, pos)?;
            self.next()?;
            let right = self.binary(9, no_in)?;
            Expr::PrivateIn(name, Box::new(right))
        } else {
            self.unary()?
        };
        let mut chain: Option<&'static str> = None;
        while let Some((op, prec)) = self.binary_prec(no_in) {
            if prec < min || (prec == min && op != "**") {
                break;
            }
            if op == "**" {
                if let Expr::Unary(..) | Expr::Await(..) = left {
                    if !self.paren_wrapped(&left) {
                        return self.err("unary operator before ** needs parentheses");
                    }
                }
            }
            // `??` can't mix with `||`/`&&` without parentheses.
            if matches!(op, "??" | "||" | "&&") {
                if let Some(c) = chain {
                    if (c == "??") != (op == "??") {
                        return self.err("can't mix ?? with || or && without parentheses");
                    }
                }
                chain = Some(op);
            }
            self.next()?;
            let right = if op == "**" { self.binary(prec, no_in)? } else { self.binary(prec + 1, no_in)? };
            if op == "??" {
                if let Expr::Logical(o, ..) = &right {
                    if *o != "??" {
                        return self.err("can't mix ?? with || or && without parentheses");
                    }
                }
            }
            left = if matches!(op, "&&" | "||" | "??") {
                if matches!(&left, Expr::Logical(o, ..) if (*o == "??") != (op == "??")) {
                    return self.err("can't mix ?? with || or && without parentheses");
                }
                Expr::Logical(op, Box::new(left), Box::new(right))
            } else {
                Expr::Binary(op, Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn paren_wrapped(&self, _e: &Expr) -> bool {
        false
    }

    fn unary(&mut self) -> R<Expr> {
        let pos = self.tok.start;
        let op = match &self.tok.kind {
            TokKind::Punct(p @ ("!" | "~" | "+" | "-")) => Some(*p),
            TokKind::Ident { name, escaped: false }
                if matches!(name.as_str(), "delete" | "void" | "typeof") =>
            {
                Some(match name.as_str() {
                    "delete" => "delete",
                    "void" => "void",
                    _ => "typeof",
                })
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next()?;
            let arg = self.unary()?;
            if op == "delete" && self.strict && matches!(arg, Expr::Ident(_)) {
                return Err(ParseError { pos, msg: "delete of an unqualified name in strict mode".into() });
            }
            if op == "delete" {
                let mut inner = &arg;
                while let Expr::Paren(e) = inner {
                    inner = e;
                }
                if self.strict && matches!(inner, Expr::Ident(_)) {
                    return Err(ParseError {
                        pos,
                        msg: "delete of an unqualified name in strict mode".into(),
                    });
                }
                if matches!(inner, Expr::PrivateMember { .. }) {
                    return Err(ParseError { pos, msg: "delete of a private field".into() });
                }
                if let Expr::PrivateMember { .. } = arg {
                    return Err(ParseError { pos, msg: "delete of a private field".into() });
                }
                if let Expr::OptionalChain(inner) = &arg {
                    if matches!(**inner, Expr::PrivateMember { .. }) {
                        return Err(ParseError { pos, msg: "delete of a private field".into() });
                    }
                }
            }
            if self.is("**") {
                return self.err("unary operator before ** needs parentheses");
            }
            return Ok(Expr::Unary(op, Box::new(arg)));
        }
        if self.is("++") || self.is("--") {
            let op = if self.is("++") { "++" } else { "--" };
            self.next()?;
            let tpos = self.tok.start;
            let arg = self.unary()?;
            let t = self.simple_target(arg, tpos, false)?;
            let target = match t {
                Pat::Ident(n) => Expr::Ident(n),
                Pat::Expr(e) => *e,
                _ => return Err(ParseError { pos: tpos, msg: "invalid update target".into() }),
            };
            return Ok(Expr::Update { op, prefix: true, target: Box::new(target) });
        }
        if self.is_kw("await") && self.await_reserved() {
            if self.f().static_block || self.in_params {
                return self.err("await isn't allowed here");
            }
            if !self.f().is_async && !self.module {
                return self.err("await outside an async function");
            }
            self.yield_await_seen.push(pos);
            self.next()?;
            let arg = self.unary()?;
            if self.is("**") {
                return self.err("await before ** needs parentheses");
            }
            return Ok(Expr::Await(Box::new(arg)));
        }
        let tpos = self.tok.start;
        let e = self.lhs_expression()?;
        if (self.is("++") || self.is("--")) && !self.tok.nl_before {
            let op = if self.is("++") { "++" } else { "--" };
            let t = self.simple_target(e, tpos, false)?;
            self.next()?;
            let target = match t {
                Pat::Ident(n) => Expr::Ident(n),
                Pat::Expr(e) => *e,
                _ => return Err(ParseError { pos: tpos, msg: "invalid update target".into() }),
            };
            return Ok(Expr::Update { op, prefix: false, target: Box::new(target) });
        }
        Ok(e)
    }

    /// LeftHandSideExpression: member, call, new, optional chains, tagged templates.
    fn lhs_expression(&mut self) -> R<Expr> {
        let start = self.tok.start;
        let mut e = if self.is_kw("new") {
            self.new_expression()?
        } else if self.is_kw("super") {
            self.super_expression()?
        } else if self.is_kw("import") {
            self.import_expression()?
        } else {
            self.primary()?
        };
        let mut in_chain = false;
        loop {
            if self.eat(".")? {
                e = self.member_dot(e, false)?;
            } else if self.is("?.") {
                self.next()?;
                in_chain = true;
                if self.eat("(")? {
                    let args = self.arguments()?;
                    e = Expr::Call { callee: Box::new(e), args, optional: true };
                } else if self.eat("[")? {
                    let p = self.expression(false)?;
                    self.expect("]")?;
                    e = Expr::Member {
                        object: Box::new(e),
                        prop: Box::new(p),
                        computed: true,
                        optional: true,
                    };
                } else if matches!(self.tok.kind, TokKind::Punct("`"))
                    || matches!(self.tok.kind, TokKind::Template { .. })
                {
                    return self.err("tagged template in an optional chain");
                } else {
                    e = self.member_dot(e, true)?;
                }
            } else if self.eat("[")? {
                let p = self.expression(false)?;
                self.expect("]")?;
                e = Expr::Member { object: Box::new(e), prop: Box::new(p), computed: true, optional: false };
            } else if self.is("(") {
                let is_async_callee = matches!(&e, Expr::Ident(n) if n == "async")
                    && self.prev_end == start + 5
                    && !self.tok.nl_before;
                self.next()?;
                let defer = is_async_callee;
                if defer {
                    self.cover_defer += 1;
                }
                let args = self.arguments();
                if defer {
                    self.cover_defer -= 1;
                }
                let args = args?;
                if is_async_callee {
                    self.async_call_start = Some(start);
                }
                e = Expr::Call { callee: Box::new(e), args, optional: false };
            } else if let TokKind::Template { .. } = self.tok.kind {
                if in_chain {
                    return self.err("tagged template in an optional chain");
                }
                let parts = self.template(true)?;
                e = Expr::TaggedTemplate { tag: Box::new(e), parts };
            } else {
                break;
            }
        }
        if in_chain {
            e = Expr::OptionalChain(Box::new(e));
        }
        Ok(e)
    }

    fn member_dot(&mut self, object: Expr, optional: bool) -> R<Expr> {
        if let TokKind::Private(n) = &self.tok.kind {
            let n = n.clone();
            let pos = self.tok.start;
            self.use_private(&n, pos)?;
            self.next()?;
            return Ok(Expr::PrivateMember { object: Box::new(object), name: n, optional });
        }
        let name = self.ident_name()?;
        Ok(Expr::Member {
            object: Box::new(object),
            prop: Box::new(Expr::Str(utf16(&name))),
            computed: false,
            optional,
        })
    }

    fn use_private(&mut self, name: &str, pos: usize) -> R<()> {
        match self.classes.last_mut() {
            Some(c) => {
                c.used.push((name.to_string(), pos));
                Ok(())
            }
            None => Err(ParseError { pos, msg: format!("private name #{name} outside a class") }),
        }
    }

    fn arguments(&mut self) -> R<Vec<Expr>> {
        let mut args = Vec::new();
        self.async_call_trailing_comma = false;
        while !self.is(")") {
            if self.eat("...")? {
                args.push(Expr::Spread(Box::new(self.assign(false)?)));
            } else {
                args.push(self.assign(false)?);
            }
            if !self.eat(",")? {
                break;
            }
            if self.is(")") && matches!(args.last(), Some(Expr::Spread(_))) {
                self.async_call_trailing_comma = true;
            }
        }
        self.expect(")")?;
        Ok(args)
    }

    fn new_expression(&mut self) -> R<Expr> {
        let pos = self.tok.start;
        self.next()?; // new
        if self.eat(".")? {
            if !self.is_kw("target") {
                return self.err("expected new.target");
            }
            self.next()?;
            if !self.fns.iter().rev().any(|f| !f.is_arrow && f.new_target) {
                return Err(ParseError { pos, msg: "new.target outside a function".into() });
            }
            return Ok(Expr::NewTarget);
        }
        let callee = if self.is_kw("new") {
            self.new_expression()?
        } else if self.is_kw("super") {
            self.super_expression()?
        } else if self.is_kw("import") {
            let n = self.peek()?;
            if matches!(n.kind, TokKind::Punct("(")) {
                return self.err("new import(...) isn't allowed");
            }
            let pos = self.tok.start;
            let e = self.import_expression()?;
            if matches!(e, Expr::ImportPhase(..)) {
                return Err(ParseError {
                    pos,
                    msg: "new import.source()/import.defer() isn't allowed".into(),
                });
            }
            e
        } else {
            self.primary()?
        };
        // Member accesses bind tighter than `new`.
        let mut callee = callee;
        loop {
            if self.eat(".")? {
                callee = self.member_dot(callee, false)?;
            } else if self.eat("[")? {
                let p = self.expression(false)?;
                self.expect("]")?;
                callee = Expr::Member {
                    object: Box::new(callee),
                    prop: Box::new(p),
                    computed: true,
                    optional: false,
                };
            } else if let TokKind::Template { .. } = self.tok.kind {
                let parts = self.template(true)?;
                callee = Expr::TaggedTemplate { tag: Box::new(callee), parts };
            } else if self.is("?.") {
                return self.err("optional chain in new");
            } else {
                break;
            }
        }
        let args = if self.eat("(")? { self.arguments()? } else { Vec::new() };
        Ok(Expr::New { callee: Box::new(callee), args })
    }

    fn super_expression(&mut self) -> R<Expr> {
        let pos = self.tok.start;
        self.next()?;
        if self.is("(") {
            if !self.fns.iter().rev().find(|f| !f.is_arrow).is_some_and(|f| f.super_call) {
                return Err(ParseError { pos, msg: "super() outside a derived constructor".into() });
            }
            self.next()?;
            let args = self.arguments()?;
            return Ok(Expr::SuperCall(args));
        }
        if !self.fns.iter().rev().find(|f| !f.is_arrow).is_some_and(|f| f.super_prop) {
            return Err(ParseError { pos, msg: "super outside a method".into() });
        }
        if self.eat(".")? {
            if let TokKind::Private(_) = self.tok.kind {
                return self.err("super.#private isn't allowed");
            }
            let name = self.ident_name()?;
            return Ok(Expr::SuperMember { prop: Box::new(Expr::Str(utf16(&name))), computed: false });
        }
        if self.eat("[")? {
            let p = self.expression(false)?;
            self.expect("]")?;
            return Ok(Expr::SuperMember { prop: Box::new(p), computed: true });
        }
        Err(ParseError { pos, msg: "unexpected super".into() })
    }

    fn import_expression(&mut self) -> R<Expr> {
        let pos = self.tok.start;
        self.next()?; // import
        if self.eat(".")? {
            if self.is_kw("source") || self.is_kw("defer") {
                let phase = if self.is_kw("source") { "source" } else { "defer" };
                self.next()?;
                self.expect("(")?;
                if self.is(")") || self.is("...") {
                    return self.err("import phase call needs one argument");
                }
                let spec = self.assign(false)?;
                self.eat(",")?;
                self.expect(")")?;
                return Ok(Expr::ImportPhase(phase, Box::new(spec)));
            }
            if !self.is_kw("meta") {
                return self.err("expected import.meta");
            }
            if !self.module {
                return Err(ParseError { pos, msg: "import.meta outside a module".into() });
            }
            self.next()?;
            return Ok(Expr::ImportMeta);
        }
        self.expect("(")?;
        if self.is(")") {
            return self.err("import() needs a specifier");
        }
        let spec = self.assign(false)?;
        let mut opts = None;
        if self.eat(",")? && !self.is(")") {
            opts = Some(Box::new(self.assign(false)?));
            self.eat(",")?;
        }
        self.expect(")")?;
        Ok(Expr::ImportCall(Box::new(spec), opts))
    }

    fn template(&mut self, tagged: bool) -> R<Vec<TemplatePart>> {
        let mut parts = Vec::new();
        loop {
            let (cooked, raw, tail) = match &self.tok.kind {
                TokKind::Template { cooked, raw, tail } => (cooked.clone(), raw.clone(), *tail),
                _ => return self.err("expected a template"),
            };
            if cooked.is_none() && !tagged {
                return self.err("invalid escape in template");
            }
            parts.push(TemplatePart::Str { cooked, raw });
            if tail {
                self.next()?;
                return Ok(parts);
            }
            self.next()?;
            let e = self.expression(false)?;
            parts.push(TemplatePart::Expr(e));
            if !self.is("}") {
                return self.err("expected } in template");
            }
            // Continue the template after `}`.
            self.lx.pos = self.tok.start + 1;
            let start = self.tok.start;
            let kind = self.lx.template_part()?;
            self.tok = Token { kind, start, end: self.lx.pos, nl_before: false, legacy_octal: false };
        }
    }

    fn primary(&mut self) -> R<Expr> {
        let pos = self.tok.start;
        if self.is("/") || self.is("/=") {
            self.relex_regex()?;
        }
        if self.tok.legacy_octal && self.strict {
            return self.err("legacy octal literal or escape in strict mode");
        }
        let kind = self.tok.kind.clone();
        match kind {
            TokKind::Num(n) => {
                self.next()?;
                Ok(Expr::Num(n))
            }
            TokKind::BigInt(b) => {
                self.next()?;
                Ok(Expr::BigInt(b))
            }
            TokKind::Str(s) => {
                self.next()?;
                Ok(Expr::Str(s))
            }
            TokKind::Template { .. } => Ok(Expr::Template(self.template(false)?)),
            TokKind::Regex { body, flags } => {
                let mut seen = HashSet::new();
                for c in flags.chars() {
                    if !"dgimsuyv".contains(c) || !seen.insert(c) || (c == 'u' && flags.contains('v')) {
                        return self.err(format!("invalid regular expression flags '{flags}'"));
                    }
                }
                crate::regexp::validate(
                    &body,
                    flags.contains('u') || flags.contains('v'),
                    flags.contains('v'),
                )
                .map_err(|m| ParseError { pos, msg: format!("invalid regular expression: {m}") })?;
                self.next()?;
                Ok(Expr::Regex { body, flags })
            }
            TokKind::Punct("(") => self.paren(pos),
            TokKind::Punct("[") => self.array_literal(pos),
            TokKind::Punct("{") => self.object_literal(),
            TokKind::Ident { name, escaped } => {
                if !escaped {
                    match name.as_str() {
                        "this" => {
                            self.next()?;
                            return Ok(Expr::This);
                        }
                        "null" => {
                            self.next()?;
                            return Ok(Expr::Null);
                        }
                        "true" | "false" => {
                            self.next()?;
                            return Ok(Expr::Bool(name == "true"));
                        }
                        "function" => {
                            self.next()?;
                            return self.function_expression(false, pos);
                        }
                        "class" => return Ok(Expr::Class(self.class(false)?)),
                        "async" => {
                            let n = self.peek()?;
                            if kw(&n, "function") && !n.nl_before {
                                self.next()?;
                                self.next()?;
                                return self.function_expression(true, pos);
                            }
                        }
                        "new" => return self.new_expression(),
                        _ => {}
                    }
                }
                let name = self.ident(false)?;
                Ok(Expr::Ident(name))
            }
            TokKind::Private(_) => self.err("unexpected private name"),
            TokKind::Punct("@") => {
                let d = self.decorators()?;
                if !self.is_kw("class") {
                    return self.err("decorators must come before a class");
                }
                self.pending_decorators = d;
                Ok(Expr::Class(self.class(false)?))
            }
            _ => self.err("unexpected token"),
        }
    }

    fn function_expression(&mut self, is_async: bool, start: usize) -> R<Expr> {
        let is_gen = self.eat("*")?;
        let name = if let TokKind::Ident { name, .. } = &self.tok.kind {
            let n = name.clone();
            let pos = self.tok.start;
            // The name is bound inside the function: its own async/generator rules apply.
            if (is_gen && n == "yield") || (is_async && n == "await") {
                return Err(ParseError { pos, msg: format!("'{n}' can't name this function") });
            }
            if n == "yield" && self.strict {
                return Err(ParseError { pos, msg: "'yield' is reserved in strict mode".into() });
            }
            if RESERVED.contains(&n.as_str()) || (self.strict && STRICT_RESERVED.contains(&n.as_str())) {
                return Err(ParseError { pos, msg: format!("'{n}' is reserved") });
            }
            if self.strict && (n == "eval" || n == "arguments") {
                return Err(ParseError { pos, msg: format!("'{n}' can't name a function in strict mode") });
            }
            if n == "await" && self.module {
                return Err(ParseError { pos, msg: "'await' is reserved in modules".into() });
            }
            self.next()?;
            Some(n)
        } else {
            None
        };
        Ok(Expr::Function(self.function_rest(name, is_async, is_gen, FuncKind::Normal, start)?))
    }

    /// `( … )`: a parenthesized expression or arrow parameters.
    fn paren(&mut self, start: usize) -> R<Expr> {
        self.next()?;
        let mut items = Vec::new();
        let mut rest = None;
        let mut trailing = false;
        self.cover_defer += 1;
        let r: R<()> = (|| {
            while !self.is(")") {
                if self.eat("...")? {
                    let p = self.binding_target()?;
                    if self.is("=") {
                        return self.err("a rest parameter can't have a default");
                    }
                    rest = Some(p);
                    break;
                }
                items.push(self.assign(false)?);
                if !self.eat(",")? {
                    break;
                }
                if self.is(")") {
                    trailing = true;
                }
            }
            Ok(())
        })();
        self.cover_defer -= 1;
        r?;
        self.expect(")")?;
        let arrow_only = items.is_empty() || rest.is_some() || trailing;
        if !self.is("=>") && arrow_only {
            return Err(ParseError { pos: start, msg: "expected =>".into() });
        }
        let inner = if items.len() == 1 { items[0].clone() } else { Expr::Seq(items.clone()) };
        if self.is("=>") {
            self.covers.insert(start, ParenCover { items, rest, arrow_only });
            self.last_paren_start = Some(start);
        } else if self.cover_defer == 0 {
            // Not arrow parameters: `({a = 1})` is an error now.
        }
        Ok(Expr::Paren(Box::new(inner)))
    }

    fn array_literal(&mut self, start: usize) -> R<Expr> {
        self.next()?;
        let mut items = Vec::new();
        self.cover_defer += 1;
        let r: R<()> = (|| {
            while !self.is("]") {
                if self.eat(",")? {
                    items.push(None);
                    continue;
                }
                let e = if self.eat("...")? {
                    Expr::Spread(Box::new(self.assign(false)?))
                } else {
                    self.assign(false)?
                };
                let spread = matches!(e, Expr::Spread(_));
                items.push(Some(e));
                if !self.is("]") {
                    self.expect(",")?;
                    if spread {
                        self.array_spread_trailing_comma.insert(start);
                    }
                }
            }
            Ok(())
        })();
        self.cover_defer -= 1;
        r?;
        self.expect("]")?;
        Ok(Expr::Array(items))
    }

    fn object_literal(&mut self) -> R<Expr> {
        self.next()?;
        let mut props = Vec::new();
        let mut proto_seen = false;
        self.cover_defer += 1;
        let r: R<()> = (|| {
            while !self.is("}") {
                props.push(self.object_property(&mut proto_seen)?);
                if !self.eat(",")? {
                    break;
                }
            }
            Ok(())
        })();
        self.cover_defer -= 1;
        r?;
        self.expect("}")?;
        Ok(Expr::Object(props))
    }

    fn object_property(&mut self, proto_seen: &mut bool) -> R<Prop> {
        let start = self.tok.start;
        if self.eat("...")? {
            return Ok(Prop::Spread(self.assign(false)?));
        }
        let mut is_async = false;
        let mut is_gen = false;
        let mut accessor = None;
        if self.is_kw("async") {
            let n = self.peek()?;
            if !n.nl_before
                && !matches!(
                    n.kind,
                    TokKind::Punct("(")
                        | TokKind::Punct(":")
                        | TokKind::Punct(",")
                        | TokKind::Punct("}")
                        | TokKind::Punct("=")
                )
            {
                self.next()?;
                is_async = true;
            }
        }
        if self.eat("*")? {
            is_gen = true;
        }
        if !is_async && !is_gen && (self.is_kw("get") || self.is_kw("set")) {
            let n = self.peek()?;
            if !matches!(
                n.kind,
                TokKind::Punct("(")
                    | TokKind::Punct(":")
                    | TokKind::Punct(",")
                    | TokKind::Punct("}")
                    | TokKind::Punct("=")
            ) {
                accessor = Some(if self.is_kw("get") { FuncKind::Getter } else { FuncKind::Setter });
                self.next()?;
            }
        }
        let key_tok = self.tok.clone();
        let key = self.property_key(false)?;
        if self.is("(") || is_async || is_gen || accessor.is_some() {
            let kind = accessor.unwrap_or(FuncKind::Method);
            let f = self.function_rest(None, is_async, is_gen, kind, start)?;
            return Ok(match kind {
                FuncKind::Getter => Prop::Getter(key, f),
                FuncKind::Setter => Prop::Setter(key, f),
                _ => Prop::Method(key, f),
            });
        }
        if self.eat(":")? {
            let is_proto = matches!(&key, PropKey::Ident(n) if n == "__proto__")
                || matches!(&key, PropKey::Str(s) if *s == utf16("__proto__"));
            if is_proto {
                if *proto_seen {
                    // Only an error if this stays an object literal.
                    self.cover_errs.push(start);
                }
                *proto_seen = true;
            }
            let v = self.assign(false)?;
            return Ok(Prop::KeyValue(key, v));
        }
        // Shorthand `{ a }` / `{ a = 1 }`.
        let TokKind::Ident { name, escaped } = &key_tok.kind else {
            return Err(ParseError { pos: start, msg: "expected :".into() });
        };
        let name = name.clone();
        if *escaped && RESERVED.contains(&name.as_str()) {
            return Err(ParseError { pos: start, msg: "escaped reserved word".into() });
        }
        self.check_ident(&name, false, start)?;
        if self.is("=") {
            self.next()?;
            let d = self.assign(false)?;
            self.cover_errs.push(start);
            return Ok(Prop::Shorthand(name, Some(Box::new(d))));
        }
        Ok(Prop::Shorthand(name, None))
    }
}

/// Parse a script (sloppy unless it starts with "use strict").
pub fn parse_script(src: &str) -> Result<Program, ParseError> {
    Parser::new(src, false)?.parse_program()
}

/// Parse a module (always strict).
pub fn parse_module(src: &str) -> Result<Program, ParseError> {
    Parser::new(src, true)?.parse_program()
}

//! AST → bytecode. Scoping follows the spec's environments at run time (names are resolved
//! dynamically, which `with` and sloppy direct `eval` require); the compiler emits the
//! declaration instantiation steps (§10.2.11, §14.2.3, §16.1.7) as instructions.

use super::code::{Code, Decls, FuncTemplate, Op};
use super::value::{js_str, JsString, PropKey};
use crate::ast::*;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq)]
enum BreakKind {
    Loop,
    Switch,
    Label,
}

enum Ctl<'a> {
    Scope,
    Handler,
    Finally(&'a [Stmt]),
    /// Values on the stack (for-in iterators) to pop when jumping out.
    Pop,
    /// An iterator record to close when jumping out.
    Iter,
    Break {
        kind: BreakKind,
        labels: Vec<String>,
        breaks: Vec<usize>,
        continues: Vec<usize>,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Bind {
    /// Assignment (and `var` declarations): PutValue.
    Assign,
    /// InitializeReferencedBinding in the current scope (let/const/params/catch).
    Init,
}

pub struct Compiler<'a> {
    code: Code,
    ctl: Vec<Ctl<'a>>,
    source: Rc<[char]>,
    /// Script and eval code record their completion value.
    completion: bool,
    strings: HashMap<JsString, u32>,
    names: HashMap<String, u32>,
    keys: HashMap<PropKey, u32>,
    /// Pending jumps to the end of the current optional chain.
    chain: Vec<Vec<usize>>,
    /// Annex B.3.2 block functions that also assign the var binding when evaluated.
    annex_b: HashSet<*const Function>,
}

pub enum Unit<'a> {
    Script(&'a Program),
    Eval(&'a Program),
}

impl<'a> Compiler<'a> {
    fn new(source: Rc<[char]>, strict: bool, completion: bool) -> Compiler<'a> {
        Compiler {
            code: Code { strict, ..Default::default() },
            ctl: Vec::new(),
            source,
            completion,
            strings: HashMap::new(),
            names: HashMap::new(),
            keys: HashMap::new(),
            chain: Vec::new(),
            annex_b: HashSet::new(),
        }
    }

    // ---- emitting ----

    fn emit(&mut self, op: Op) -> usize {
        self.code.ops.push(op);
        self.code.ops.len() - 1
    }
    fn here(&self) -> u32 {
        self.code.ops.len() as u32
    }
    fn patch(&mut self, at: usize) {
        let to = self.here();
        self.patch_to(at, to);
    }
    fn patch_to(&mut self, at: usize, to: u32) {
        let op = &mut self.code.ops[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(to),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(to),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(to),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(to),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(to),
            Op::JumpIfNotNullishKeep(_) => Op::JumpIfNotNullishKeep(to),
            Op::JumpIfNotUndef(_) => Op::JumpIfNotUndef(to),
            Op::OptCheck(_, n) => Op::OptCheck(to, n),
            Op::TryStart(_) => Op::TryStart(to),
            Op::ForInNext(_) => Op::ForInNext(to),
            Op::IterNext(_) => Op::IterNext(to),
            o => o,
        };
    }
    fn string(&mut self, s: &[u16]) -> u32 {
        let s: JsString = s.into();
        if let Some(&i) = self.strings.get(&s) {
            return i;
        }
        self.code.strings.push(s.clone());
        let i = self.code.strings.len() as u32 - 1;
        self.strings.insert(s, i);
        i
    }
    fn name(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.names.get(s) {
            return i;
        }
        self.code.names.push(s.into());
        let i = self.code.names.len() as u32 - 1;
        self.names.insert(s.to_string(), i);
        i
    }
    fn key(&mut self, k: PropKey) -> u32 {
        if let Some(&i) = self.keys.get(&k) {
            return i;
        }
        self.code.keys.push(k.clone());
        let i = self.code.keys.len() as u32 - 1;
        self.keys.insert(k, i);
        i
    }
    fn push_str(&mut self, s: &str) {
        let i = self.string(&js_str(s));
        self.emit(Op::Str(i));
    }
    fn unsupported(&mut self, what: &str) {
        let i = self.string(&js_str(&format!("{what} is not supported yet")));
        self.emit(Op::Unsupported(i));
    }
    fn template(&mut self, f: &Rc<Function>, is_expression: bool) -> u32 {
        self.template_named(f, is_expression, None)
    }
    fn template_named(&mut self, f: &Rc<Function>, is_expression: bool, given: Option<&str>) -> u32 {
        let t = FuncTemplate {
            ast: f.clone(),
            source: self.source.clone(),
            code: OnceCell::new(),
            length: expected_args(f),
            is_expression,
            given_name: given.map(Into::into),
            default_ctor: false,
            text_span: None,
        };
        self.code.funcs.push(Rc::new(t));
        self.code.funcs.len() as u32 - 1
    }
    /// A closure; `name` is SetFunctionName for anonymous functions in named positions
    /// (§8.4.5 NamedEvaluation).
    fn closure(&mut self, f: &Rc<Function>, is_expression: bool, name: Option<&str>) {
        let given = if f.name.is_none() { name } else { None };
        let i = self.template_named(f, is_expression && f.name.is_some(), given);
        self.emit(Op::Closure(i));
    }

    // ---- units ----

    pub fn compile_unit(unit: Unit<'a>, source: Rc<[char]>) -> Code {
        let (prog, is_eval) = match unit {
            Unit::Script(p) => (p, false),
            Unit::Eval(p) => (p, true),
        };
        let mut c = Compiler::new(source, prog.strict, true);
        let body = &prog.body;
        let mut decls = Decls::default();
        let mut lex = Vec::new();
        top_lexical(body, &mut lex);
        for (n, is_const) in &lex {
            decls.lexical.push((n.as_str().into(), *is_const));
        }
        let mut vars = Vec::new();
        var_names(body, &mut vars, true);
        let funcs = top_functions(body);
        let fnames: HashSet<&str> = funcs.iter().filter_map(|f| f.name.as_deref()).collect();
        let mut seen = HashSet::new();
        for v in &vars {
            if !fnames.contains(v.as_str()) && seen.insert(v.clone()) {
                decls.vars.push(v.as_str().into());
            }
        }
        // Last declaration of a name wins (§16.1.7 step 10).
        let mut fseen = HashSet::new();
        let mut fl = Vec::new();
        for f in funcs.iter().rev() {
            let n = f.name.clone().unwrap_or_default();
            if fseen.insert(n.clone()) {
                fl.push((n, (*f).clone()));
            }
        }
        fl.reverse();
        for (n, f) in fl {
            let i = c.template(&f, false);
            decls.functions.push((n.as_str().into(), i));
        }
        if !prog.strict {
            let lexset: HashSet<String> = lex.iter().map(|l| l.0.clone()).collect();
            let mut found = Vec::new();
            annex_b_functions(body, &lexset, &HashSet::new(), &mut found);
            let mut seen = HashSet::new();
            for f in found {
                let n = f.name.clone().unwrap_or_default();
                if !fnames.contains(n.as_str()) {
                    if seen.insert(n.clone()) {
                        decls.annex_b.push(n.as_str().into());
                    }
                }
                c.annex_b.insert(Rc::as_ptr(&f));
            }
        }
        c.code.decls = decls;
        c.emit(if is_eval { Op::EvalInit } else { Op::GlobalInit });
        for s in body {
            c.stmt(s);
        }
        c.emit(Op::Undef);
        c.emit(Op::Ret);
        c.code
    }

    /// Compile a function body (§10.2.11 FunctionDeclarationInstantiation, then the body).
    pub fn compile_function(t: &FuncTemplate) -> Code {
        let f = &t.ast;
        let mut c = Compiler::new(t.source.clone(), f.strict, false);
        let is_arrow = f.kind == FuncKind::Arrow;
        if f.is_generator || f.is_async {
            c.unsupported(if f.is_async { "async functions" } else { "generators" });
            return c.code;
        }
        if f.kind == FuncKind::ClassConstructor {
            // Base constructors initialize fields before the body (§10.2.2 step 6.b).
            c.emit(Op::InitInstance);
        }
        if t.default_ctor {
            if f.kind == FuncKind::DerivedConstructor {
                c.emit(Op::SuperCallForward);
                c.emit(Op::Pop);
            }
            c.emit(Op::Undef);
            c.emit(Op::Ret);
            return c.code;
        }
        if f.kind == FuncKind::ClassField {
            if let [Stmt::Return(Some(e))] = f.body.as_slice() {
                if Self::is_anonymous_fn(e) {
                    // NamedEvaluation with the field's name (§15.7.10 step 3).
                    c.expr(e);
                    c.emit(Op::SetFnNameField);
                    c.emit(Op::Ret);
                    return c.code;
                }
            }
        }
        let mut param_names = Vec::new();
        for p in &f.params {
            pat_names(p, &mut param_names);
        }
        if let Some(r) = &f.rest {
            pat_names(r, &mut param_names);
        }
        let has_dups = {
            let mut s = HashSet::new();
            !param_names.iter().all(|n| s.insert(n))
        };
        let has_param_exprs = f.params.iter().chain(f.rest.iter()).any(pat_has_expr);
        let mut lex = Vec::new();
        top_lexical(&f.body, &mut lex);
        let funcs = top_functions(&f.body);
        let fnames: HashSet<String> = funcs.iter().filter_map(|f| f.name.clone()).collect();
        let lex_names: HashSet<String> = lex.iter().map(|l| l.0.clone()).collect();
        let arguments_needed = !is_arrow
            && f.kind != FuncKind::ClassField
            && !param_names.iter().any(|n| n == "arguments")
            && (has_param_exprs || (!fnames.contains("arguments") && !lex_names.contains("arguments")));

        // Parameters: bindings first (uninitialized unless there are duplicates).
        for n in &param_names {
            let i = c.name(n);
            c.emit(if has_dups { Op::DeclareVar(i) } else { Op::DeclareLet(i) });
        }
        if arguments_needed {
            c.emit(Op::CreateArguments);
            let i = c.name("arguments");
            c.emit(if f.strict { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
            c.emit(Op::InitBinding(i));
        }
        for (i, p) in f.params.iter().enumerate() {
            c.emit(Op::Arg(i as u32));
            c.bind(p, Bind::Init);
        }
        if let Some(r) = &f.rest {
            c.emit(Op::RestArgs(f.params.len() as u32));
            c.bind(r, Bind::Init);
        }
        let mut vars = Vec::new();
        var_names(&f.body, &mut vars, true);
        let mut instantiated: HashSet<String> = param_names.iter().cloned().collect();
        if arguments_needed {
            instantiated.insert("arguments".into());
        }
        if !has_param_exprs {
            for v in &vars {
                if instantiated.insert(v.clone()) {
                    let i = c.name(v);
                    c.emit(Op::DeclareVar(i));
                }
            }
        } else {
            // A separate var environment; vars named like parameters start with their values.
            let mut copy = Vec::new();
            let mut seen = HashSet::new();
            for v in &vars {
                if seen.insert(v.clone()) {
                    copy.push(v.clone());
                }
            }
            for v in &copy {
                if instantiated.contains(v) || v == "arguments" && arguments_needed {
                    let i = c.name(v);
                    c.emit(Op::GetVar(i));
                }
            }
            c.emit(Op::PushVarScope);
            for v in copy.iter().rev() {
                let i = c.name(v);
                c.emit(Op::DeclareVar(i));
                if instantiated.contains(v) || v == "arguments" && arguments_needed {
                    c.emit(Op::InitBinding(i));
                }
            }
            instantiated.extend(copy);
        }
        // Annex B.3.2.1: block functions also get a var binding.
        if !f.strict {
            let mut found = Vec::new();
            let pn: HashSet<String> = param_names.iter().cloned().collect();
            annex_b_functions(&f.body, &lex_names, &pn, &mut found);
            for bf in found {
                let n = bf.name.clone().unwrap_or_default();
                if !fnames.contains(&n) && !(n == "arguments" && arguments_needed) {
                    if instantiated.insert(n.clone()) {
                        let i = c.name(&n);
                        c.emit(Op::DeclareVar(i));
                    }
                }
                c.annex_b.insert(Rc::as_ptr(&bf));
            }
        }
        if !f.strict {
            c.emit(Op::PushScope);
        }
        for (n, is_const) in &lex {
            let i = c.name(n);
            c.emit(if *is_const { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
        }
        let mut fseen = HashSet::new();
        let mut fl = Vec::new();
        for g in funcs.iter().rev() {
            let n = g.name.clone().unwrap_or_default();
            if fseen.insert(n.clone()) {
                fl.push(((*g).clone(), n));
            }
        }
        for (g, n) in fl.into_iter().rev() {
            let i = c.template(&g, false);
            c.emit(Op::Closure(i));
            let ni = c.name(&n);
            c.emit(Op::SetVarEnv(ni));
        }
        for s in &f.body {
            c.stmt(s);
        }
        c.emit(Op::Undef);
        c.emit(Op::Ret);
        c.code
    }

    // ---- statements ----

    fn set_completion_undefined(&mut self) {
        if self.completion {
            self.emit(Op::Undef);
            self.emit(Op::SetCompletion);
        }
    }

    /// Declare a block's lexical names and instantiate its functions (§14.2.3).
    fn block_scope(&mut self, body: &'a [Stmt]) -> bool {
        let mut lex = Vec::new();
        let mut funcs = Vec::new();
        block_lexical(body, &mut lex, &mut funcs);
        if lex.is_empty() && funcs.is_empty() {
            return false;
        }
        self.emit(Op::PushScope);
        self.ctl.push(Ctl::Scope);
        for (n, is_const) in &lex {
            let i = self.name(n);
            self.emit(if *is_const { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
        }
        for f in funcs {
            let n = f.name.clone().unwrap_or_default();
            let i = self.name(&n);
            self.emit(Op::DeclareLet(i));
            let t = self.template(&f, false);
            self.emit(Op::Closure(t));
            self.emit(Op::InitBinding(i));
        }
        true
    }

    fn end_scope(&mut self, had: bool) {
        if had {
            self.ctl.pop();
            self.emit(Op::PopScope);
        }
    }

    fn stmts(&mut self, body: &'a [Stmt]) {
        for s in body {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &'a Stmt) {
        match s {
            Stmt::Expr(e) => {
                self.expr(e);
                self.emit(if self.completion { Op::SetCompletion } else { Op::Pop });
            }
            Stmt::Var(kind, decls) => self.var_decl(*kind, decls),
            Stmt::Function(f) => {
                if self.annex_b.contains(&Rc::as_ptr(f)) {
                    let i = self.name(f.name.as_deref().unwrap_or_default());
                    self.emit(Op::GetVar(i));
                    self.emit(Op::SetVarEnv(i));
                }
            }
            Stmt::Class(c) => {
                self.class(c, None);
                match &c.name {
                    Some(n) => {
                        let i = self.name(n);
                        self.emit(Op::InitBinding(i));
                    }
                    None => {
                        self.emit(Op::Pop);
                    }
                }
            }
            Stmt::Block(b) => {
                let had = self.block_scope(b);
                self.stmts(b);
                self.end_scope(had);
            }
            Stmt::Empty | Stmt::Debugger => {}
            Stmt::If(t, a, b) => {
                self.set_completion_undefined();
                self.expr(t);
                let j = self.emit(Op::JumpIfFalse(0));
                self.stmt(a);
                if let Some(b) = b {
                    let k = self.emit(Op::Jump(0));
                    self.patch(j);
                    self.stmt(b);
                    self.patch(k);
                } else {
                    self.patch(j);
                }
            }
            Stmt::For { .. }
            | Stmt::ForIn { .. }
            | Stmt::ForOf { .. }
            | Stmt::While(..)
            | Stmt::DoWhile(..) => self.loop_stmt(s, Vec::new()),
            Stmt::Return(e) => {
                match e {
                    Some(e) => self.expr(e),
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                let needs_cleanup =
                    self.ctl.iter().any(|c| matches!(c, Ctl::Finally(_) | Ctl::Iter | Ctl::Pop));
                if needs_cleanup {
                    self.emit(Op::SetRet);
                    self.cleanup_to(0);
                    self.emit(Op::GetRet);
                }
                self.emit(Op::Ret);
            }
            Stmt::Break(label) => self.jump_out(label.as_deref(), false),
            Stmt::Continue(label) => self.jump_out(label.as_deref(), true),
            Stmt::Throw(e) => {
                self.expr(e);
                self.emit(Op::Throw);
            }
            Stmt::Try { block, param, handler, finalizer } => {
                self.try_stmt(block, param.as_ref(), handler.as_deref(), finalizer.as_deref())
            }
            Stmt::Switch(d, cases) => self.switch_stmt(d, cases, Vec::new()),
            Stmt::Labeled(..) => {
                let mut labels = Vec::new();
                let mut inner = s;
                while let Stmt::Labeled(l, b) = inner {
                    labels.push(l.clone());
                    inner = b;
                }
                match inner {
                    Stmt::For { .. }
                    | Stmt::ForIn { .. }
                    | Stmt::ForOf { .. }
                    | Stmt::While(..)
                    | Stmt::DoWhile(..) => self.loop_stmt(inner, labels),
                    _ => {
                        self.ctl.push(Ctl::Break {
                            kind: BreakKind::Label,
                            labels,
                            breaks: vec![],
                            continues: vec![],
                        });
                        self.stmt(inner);
                        if let Some(Ctl::Break { breaks, .. }) = self.ctl.pop() {
                            for b in breaks {
                                self.patch(b);
                            }
                        }
                    }
                }
            }
            Stmt::With(o, body) => {
                self.set_completion_undefined();
                self.expr(o);
                self.emit(Op::PushWith);
                self.ctl.push(Ctl::Scope);
                self.stmt(body);
                self.ctl.pop();
                self.emit(Op::PopScope);
            }
            Stmt::Import { .. }
            | Stmt::ExportNamed { .. }
            | Stmt::ExportDecl(_)
            | Stmt::ExportDefault(_)
            | Stmt::ExportDefaultExpr(_) => self.unsupported("modules"),
        }
    }

    fn var_decl(&mut self, kind: VarKind, decls: &'a [(Pat, Option<Expr>)]) {
        match kind {
            VarKind::Using | VarKind::AwaitUsing => {
                self.unsupported("using declarations");
            }
            VarKind::Var => {
                for (p, init) in decls {
                    let Some(init) = init else { continue };
                    match p {
                        Pat::Ident(n) => {
                            self.named_expr(init, n);
                            let i = self.name(n);
                            self.emit(Op::SetVar(i));
                            self.emit(Op::Pop);
                        }
                        _ => {
                            self.expr(init);
                            self.bind(p, Bind::Assign);
                        }
                    }
                }
            }
            VarKind::Let | VarKind::Const => {
                for (p, init) in decls {
                    match (p, init) {
                        (Pat::Ident(n), Some(init)) => self.named_expr(init, n),
                        (_, Some(init)) => self.expr(init),
                        (_, None) => {
                            self.emit(Op::Undef);
                        }
                    }
                    self.bind(p, Bind::Init);
                }
            }
        }
    }

    /// Emit the jumps out of constructs between here and `ctl[depth]` (exclusive).
    fn cleanup_to(&mut self, depth: usize) {
        let mut i = self.ctl.len();
        while i > depth {
            i -= 1;
            match &self.ctl[i] {
                Ctl::Scope => {
                    self.emit(Op::PopScope);
                }
                Ctl::Handler => {
                    self.emit(Op::TryEnd);
                }
                Ctl::Pop => {
                    self.emit(Op::Pop);
                }
                Ctl::Iter => {
                    self.emit(Op::IterClose);
                }
                Ctl::Finally(body) => {
                    let body: &'a [Stmt] = body;
                    // The finally block runs in the context outside its try statement.
                    let saved: Vec<Ctl<'a>> = self.ctl.drain(i..).collect();
                    let comp = self.completion;
                    self.completion = false;
                    let had = self.block_scope(body);
                    self.stmts(body);
                    self.end_scope(had);
                    self.completion = comp;
                    self.ctl.extend(saved);
                }
                Ctl::Break { .. } => {}
            }
        }
    }

    fn jump_out(&mut self, label: Option<&str>, is_continue: bool) {
        let target = self.ctl.iter().rposition(|c| match c {
            Ctl::Break { kind, labels, .. } => match label {
                Some(l) => labels.iter().any(|x| x == l),
                None => {
                    if is_continue {
                        *kind == BreakKind::Loop
                    } else {
                        *kind != BreakKind::Label
                    }
                }
            },
            _ => false,
        });
        let Some(t) = target else {
            // Early errors rule this out; be safe.
            self.unsupported("this jump");
            return;
        };
        self.cleanup_to(t + 1);
        let j = self.emit(Op::Jump(0));
        if let Ctl::Break { breaks, continues, .. } = &mut self.ctl[t] {
            if is_continue {
                continues.push(j);
            } else {
                breaks.push(j);
            }
        }
    }

    fn push_loop(&mut self, labels: Vec<String>) {
        self.ctl.push(Ctl::Break { kind: BreakKind::Loop, labels, breaks: vec![], continues: vec![] });
    }

    /// Pop the loop entry, patching continues to `cont` and breaks to here.
    fn pop_loop_patch_breaks(&mut self) -> Vec<usize> {
        match self.ctl.pop() {
            Some(Ctl::Break { breaks, continues, .. }) => {
                for b in breaks {
                    self.patch(b);
                }
                continues
            }
            _ => vec![],
        }
    }

    fn take_continues(&mut self) -> Vec<usize> {
        match self.ctl.last_mut() {
            Some(Ctl::Break { continues, .. }) => std::mem::take(continues),
            _ => vec![],
        }
    }

    fn loop_stmt(&mut self, s: &'a Stmt, labels: Vec<String>) {
        self.set_completion_undefined();
        match s {
            Stmt::While(t, body) => {
                let top = self.here();
                self.expr(t);
                let exit = self.emit(Op::JumpIfFalse(0));
                self.push_loop(labels);
                self.stmt(body);
                for c in self.take_continues() {
                    self.patch_to(c, top);
                }
                self.emit(Op::Jump(top));
                self.patch(exit);
                self.pop_loop_patch_breaks();
            }
            Stmt::DoWhile(body, t) => {
                let top = self.here();
                self.push_loop(labels);
                self.stmt(body);
                for c in self.take_continues() {
                    self.patch(c);
                }
                self.expr(t);
                self.emit(Op::JumpIfTrue(top));
                self.pop_loop_patch_breaks();
            }
            Stmt::For { init, test, update, body } => {
                let mut scoped = false;
                let mut per_iter = false;
                match init {
                    Some(ForInit::Var(k @ (VarKind::Let | VarKind::Const), decls)) => {
                        self.emit(Op::PushScope);
                        self.ctl.push(Ctl::Scope);
                        scoped = true;
                        per_iter = *k == VarKind::Let;
                        let mut names = Vec::new();
                        for (p, _) in decls {
                            pat_names(p, &mut names);
                        }
                        for n in &names {
                            let i = self.name(n);
                            self.emit(if *k == VarKind::Const {
                                Op::DeclareConst(i)
                            } else {
                                Op::DeclareLet(i)
                            });
                        }
                        self.var_decl(*k, decls);
                    }
                    Some(ForInit::Var(k, decls)) => self.var_decl(*k, decls),
                    Some(ForInit::Expr(e)) => {
                        self.expr(e);
                        self.emit(Op::Pop);
                    }
                    None => {}
                }
                if per_iter {
                    self.emit(Op::CopyScope);
                }
                let top = self.here();
                let exit = test.as_ref().map(|t| {
                    self.expr(t);
                    self.emit(Op::JumpIfFalse(0))
                });
                self.push_loop(labels);
                self.stmt(body);
                for c in self.take_continues() {
                    self.patch(c);
                }
                if per_iter {
                    self.emit(Op::CopyScope);
                }
                if let Some(u) = update {
                    self.expr(u);
                    self.emit(Op::Pop);
                }
                self.emit(Op::Jump(top));
                if let Some(e) = exit {
                    self.patch(e);
                }
                self.pop_loop_patch_breaks();
                if scoped {
                    self.ctl.pop();
                    self.emit(Op::PopScope);
                }
            }
            Stmt::ForIn { head, right, body } => {
                self.for_head_tdz(head, right);
                self.emit(Op::ForInStart);
                self.ctl.push(Ctl::Pop);
                let top = self.here();
                let next = self.emit(Op::ForInNext(0));
                self.push_loop(labels);
                self.for_binding(head);
                self.stmt(body);
                self.end_for_binding(head);
                for c in self.take_continues() {
                    self.patch_to(c, top);
                }
                self.emit(Op::Jump(top));
                // Breaks land here: drop the iterator.
                self.pop_loop_patch_breaks();
                self.emit(Op::Pop);
                self.ctl.pop();
                self.patch(next);
            }
            Stmt::ForOf { head, right, body, is_await } => {
                if *is_await {
                    self.unsupported("for await");
                    return;
                }
                self.for_head_tdz(head, right);
                self.emit(Op::GetIter);
                self.ctl.push(Ctl::Iter);
                let top = self.here();
                let next = self.emit(Op::IterNext(0));
                self.push_loop(labels);
                let try_at = self.emit(Op::TryStart(0));
                self.ctl.push(Ctl::Handler);
                self.for_binding(head);
                self.stmt(body);
                self.end_for_binding(head);
                self.ctl.pop();
                self.emit(Op::TryEnd);
                for c in self.take_continues() {
                    self.patch_to(c, top);
                }
                self.emit(Op::Jump(top));
                self.patch(try_at);
                self.emit(Op::IterCloseThrow);
                self.pop_loop_patch_breaks();
                self.emit(Op::IterClose);
                self.ctl.pop();
                self.patch(next);
            }
            _ => unreachable!("not a loop"),
        }
    }

    /// The right-hand side of for-in/of, with the loop's let/const names in TDZ (§14.7.5.6).
    fn for_head_tdz(&mut self, head: &'a ForHead, right: &'a Expr) {
        if let ForHead::Var(k @ (VarKind::Let | VarKind::Const), p) = head {
            let mut names = Vec::new();
            pat_names(p, &mut names);
            self.emit(Op::PushScope);
            for n in &names {
                let i = self.name(n);
                self.emit(if *k == VarKind::Const { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
            }
            self.expr(right);
            self.emit(Op::PopScope);
        } else {
            self.expr(right);
        }
    }

    /// `[.. v] → [..]`: bind the loop value, in a fresh scope for let/const.
    fn for_binding(&mut self, head: &'a ForHead) {
        match head {
            ForHead::Var(k @ (VarKind::Let | VarKind::Const), p) => {
                let mut names = Vec::new();
                pat_names(p, &mut names);
                self.emit(Op::PushScope);
                self.ctl.push(Ctl::Scope);
                for n in &names {
                    let i = self.name(n);
                    self.emit(if *k == VarKind::Const { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
                }
                self.bind(p, Bind::Init);
            }
            ForHead::Var(VarKind::Var, p) | ForHead::Pat(p) => self.bind(p, Bind::Assign),
            ForHead::Var(_, _) => {
                self.emit(Op::Pop);
                self.unsupported("using declarations");
            }
        }
    }

    fn end_for_binding(&mut self, head: &ForHead) {
        if let ForHead::Var(VarKind::Let | VarKind::Const, _) = head {
            self.ctl.pop();
            self.emit(Op::PopScope);
        }
    }

    fn try_stmt(
        &mut self,
        block: &'a [Stmt],
        param: Option<&'a Pat>,
        handler: Option<&'a [Stmt]>,
        finalizer: Option<&'a [Stmt]>,
    ) {
        self.set_completion_undefined();
        let fin_try = finalizer.map(|f| {
            let at = self.emit(Op::TryStart(0));
            self.ctl.push(Ctl::Handler);
            self.ctl.push(Ctl::Finally(f));
            at
        });
        // The finally entry sits below the handler so jumps out pop the handler first.
        if fin_try.is_some() {
            // Reorder: [Handler, Finally] → [Finally, Handler].
            let n = self.ctl.len();
            self.ctl.swap(n - 1, n - 2);
        }
        if let Some(h) = handler {
            let at = self.emit(Op::TryStart(0));
            self.ctl.push(Ctl::Handler);
            let had = self.block_scope(block);
            self.stmts(block);
            self.end_scope(had);
            self.ctl.pop();
            self.emit(Op::TryEnd);
            let skip = self.emit(Op::Jump(0));
            self.patch(at);
            // [exc]
            self.set_completion_undefined();
            match param {
                Some(p) => {
                    let mut names = Vec::new();
                    pat_names(p, &mut names);
                    self.emit(Op::PushScope);
                    self.ctl.push(Ctl::Scope);
                    for n in &names {
                        let i = self.name(n);
                        self.emit(Op::DeclareLet(i));
                    }
                    self.bind(p, Bind::Init);
                    let had = self.block_scope(h);
                    self.stmts(h);
                    self.end_scope(had);
                    self.ctl.pop();
                    self.emit(Op::PopScope);
                }
                None => {
                    self.emit(Op::Pop);
                    let had = self.block_scope(h);
                    self.stmts(h);
                    self.end_scope(had);
                }
            }
            self.patch(skip);
        } else {
            let had = self.block_scope(block);
            self.stmts(block);
            self.end_scope(had);
        }
        if let (Some(at), Some(f)) = (fin_try, finalizer) {
            // Normal completion: pop the handler, run the finally block.
            self.ctl.pop(); // Handler
            self.ctl.pop(); // Finally
            self.emit(Op::TryEnd);
            let had = self.block_scope(f);
            self.finally_body(f);
            self.end_scope(had);
            let done = self.emit(Op::Jump(0));
            // Throw completion: [exc] held while the finally block runs, then rethrown.
            self.patch(at);
            self.ctl.push(Ctl::Pop);
            let had = self.block_scope(f);
            self.finally_body(f);
            self.end_scope(had);
            self.ctl.pop();
            self.emit(Op::Throw);
            self.patch(done);
        }
    }

    /// A finally block doesn't change the completion value unless it completes abruptly.
    fn finally_body(&mut self, f: &'a [Stmt]) {
        let comp = self.completion;
        self.completion = false;
        self.stmts(f);
        self.completion = comp;
    }

    fn switch_stmt(&mut self, d: &'a Expr, cases: &'a [Case], labels: Vec<String>) {
        self.set_completion_undefined();
        self.expr(d);
        // The case block is one scope (§14.12.4).
        let mut lex = Vec::new();
        let mut funcs = Vec::new();
        for c in cases {
            block_lexical(&c.body, &mut lex, &mut funcs);
        }
        let scoped = !lex.is_empty() || !funcs.is_empty();
        if scoped {
            self.emit(Op::PushScope);
            self.ctl.push(Ctl::Scope);
            for (n, is_const) in &lex {
                let i = self.name(n);
                self.emit(if *is_const { Op::DeclareConst(i) } else { Op::DeclareLet(i) });
            }
            for f in funcs {
                let n = f.name.clone().unwrap_or_default();
                let i = self.name(&n);
                self.emit(Op::DeclareLet(i));
                let t = self.template(&f, false);
                self.emit(Op::Closure(t));
                self.emit(Op::InitBinding(i));
            }
        }
        // [d]: test the cases in order; matches jump to stubs that pop `d`.
        let mut to_body: Vec<(usize, usize)> = Vec::new();
        for (ci, c) in cases.iter().enumerate() {
            if let Some(t) = &c.test {
                self.emit(Op::Dup);
                self.expr(t);
                self.emit(Op::StrictEq);
                let miss = self.emit(Op::JumpIfFalse(0));
                self.emit(Op::Pop);
                let j = self.emit(Op::Jump(0));
                to_body.push((ci, j));
                self.patch(miss);
            }
        }
        self.emit(Op::Pop);
        let default_jump = self.emit(Op::Jump(0));
        self.ctl.push(Ctl::Break { kind: BreakKind::Switch, labels, breaks: vec![], continues: vec![] });
        let mut default_target = None;
        for (ci, c) in cases.iter().enumerate() {
            let here = self.here();
            for (i, j) in &to_body {
                if *i == ci {
                    self.patch_to(*j, here);
                }
            }
            if c.test.is_none() {
                default_target = Some(here);
            }
            self.stmts(&c.body);
        }
        let end = self.here();
        self.patch_to(default_jump, default_target.unwrap_or(end));
        self.pop_loop_patch_breaks();
        if scoped {
            self.ctl.pop();
            self.emit(Op::PopScope);
        }
    }

    // ---- patterns ----

    /// `[v] → []`: bind a value to a pattern.
    fn bind(&mut self, p: &'a Pat, mode: Bind) {
        match p {
            Pat::Ident(n) => {
                let i = self.name(n);
                match mode {
                    Bind::Init => {
                        self.emit(Op::InitBinding(i));
                    }
                    Bind::Assign => {
                        self.emit(Op::SetVar(i));
                        self.emit(Op::Pop);
                    }
                }
            }
            Pat::Default(target, def) => {
                let j = self.emit(Op::JumpIfNotUndef(0));
                self.emit(Op::Pop);
                match &**target {
                    Pat::Ident(n) => self.named_expr(def, n),
                    _ => self.expr(def),
                }
                self.patch(j);
                self.bind(target, mode);
            }
            Pat::Expr(e) => self.bind_target_expr(e),
            Pat::Object(props, rest) => {
                self.emit(Op::RequireCoercible);
                if rest.is_some() {
                    self.emit(Op::Array);
                    self.emit(Op::Swap);
                }
                for (k, sub) in props {
                    self.emit(Op::Dup);
                    match k {
                        crate::ast::PropKey::Ident(_)
                        | crate::ast::PropKey::Str(_)
                        | crate::ast::PropKey::Num(_)
                            if rest.is_none() =>
                        {
                            let key = static_key(k);
                            let ki = self.key(key);
                            self.emit(Op::GetNamed(ki));
                        }
                        _ => {
                            self.prop_key(k);
                            if rest.is_some() {
                                self.emit(Op::AppendKey);
                            }
                            self.emit(Op::GetProp);
                        }
                    }
                    self.bind(sub, mode);
                }
                if let Some(r) = rest {
                    self.emit(Op::Swap);
                    self.emit(Op::ObjRest);
                    self.bind(r, mode);
                } else {
                    self.emit(Op::Pop);
                }
            }
            Pat::Array(elems, rest) => {
                self.emit(Op::GetIter);
                let at = self.emit(Op::TryStart(0));
                self.ctl.push(Ctl::Handler);
                for e in elems {
                    match e {
                        None => {
                            self.emit(Op::IterValue);
                            self.emit(Op::Pop);
                        }
                        Some(p) => {
                            self.emit(Op::IterValue);
                            self.bind(p, mode);
                        }
                    }
                }
                if let Some(r) = rest {
                    self.emit(Op::IterRest);
                    self.bind(r, mode);
                }
                self.ctl.pop();
                self.emit(Op::TryEnd);
                self.emit(Op::IterClose);
                let done = self.emit(Op::Jump(0));
                self.patch(at);
                self.emit(Op::IterCloseThrow);
                self.patch(done);
            }
        }
    }

    /// `[v] → []`: assign to a member expression (or throw for call targets).
    fn bind_target_expr(&mut self, e: &'a Expr) {
        match e {
            Expr::Paren(inner) => self.bind_target_expr(inner),
            Expr::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::SetVar(i));
                self.emit(Op::Pop);
            }
            Expr::Call { .. } => {
                self.emit(Op::Pop);
                self.expr(e);
                self.emit(Op::Pop);
                self.throw_reference_error("invalid assignment target");
            }
            _ => match self.push_ref(e, true) {
                Some(r) => {
                    self.emit(Op::Roll(r.parts()));
                    self.ref_set(r);
                    self.emit(Op::Pop);
                }
                None => {
                    self.emit(Op::Pop);
                    self.unsupported("this assignment target");
                }
            },
        }
    }

    /// Push the parts of a member reference (`[obj]`, `[obj key]`, `[obj #name]`,
    /// `[this key]`); `convert`: apply ToPropertyKey to computed keys now.
    fn push_ref(&mut self, e: &'a Expr, convert: bool) -> Option<MRef> {
        match e {
            Expr::Paren(i) => self.push_ref(i, convert),
            Expr::Member { object, prop, computed, .. } => {
                self.expr(object);
                if *computed {
                    self.expr(prop);
                    if convert {
                        self.emit(Op::ToKey);
                    }
                    Some(MRef::Computed)
                } else {
                    Some(MRef::Named(self.member_key(prop)))
                }
            }
            Expr::PrivateMember { object, name, .. } => {
                self.expr(object);
                self.private_name(name);
                Some(MRef::Private)
            }
            Expr::SuperMember { prop, computed } => {
                self.emit(Op::This);
                self.super_key(prop, *computed);
                Some(MRef::Super)
            }
            _ => None,
        }
    }

    fn private_name(&mut self, name: &str) {
        let i = self.name(&format!("#{name}"));
        self.emit(Op::GetVar(i));
    }

    fn super_key(&mut self, prop: &'a Expr, computed: bool) {
        if computed {
            self.expr(prop);
            self.emit(Op::ToKey);
        } else {
            let k = self.member_key(prop);
            if let super::value::Value::String(st) = self.code.keys[k as usize].to_value() {
                let i = self.string(&st);
                self.emit(Op::Str(i));
            }
        }
    }

    fn ref_dup(&mut self, r: MRef) {
        self.emit(if r.parts() == 1 { Op::Dup } else { Op::Dup2 });
    }

    /// `[parts] → [value]`
    fn ref_get(&mut self, r: MRef) {
        self.emit(match r {
            MRef::Named(k) => Op::GetNamed(k),
            MRef::Computed => Op::GetProp,
            MRef::Private => Op::PrivateGet,
            MRef::Super => Op::SuperGet,
        });
    }

    /// `[parts v] → [v]`
    fn ref_set(&mut self, r: MRef) {
        self.emit(match r {
            MRef::Named(k) => Op::SetNamed(k),
            MRef::Computed => Op::SetProp,
            MRef::Private => Op::PrivateSet,
            MRef::Super => Op::SuperSet,
        });
    }

    fn throw_reference_error(&mut self, msg: &str) {
        // `ReferenceError(msg)` via the global constructor would be observable; use an op.
        let i = self.string(&js_str(&format!("ReferenceError:{msg}")));
        self.emit(Op::Unsupported(i));
    }

    // ---- expressions ----

    fn member_key(&mut self, prop: &Expr) -> u32 {
        let k = match prop {
            Expr::Ident(n) => PropKey::named(n),
            Expr::Str(s) => PropKey::from_js(s.as_slice().into()),
            _ => PropKey::named(""),
        };
        self.key(k)
    }

    /// Push a property key (ToPropertyKey applied).
    fn prop_key(&mut self, k: &'a crate::ast::PropKey) {
        match k {
            crate::ast::PropKey::Computed(e) => {
                self.expr(e);
                self.emit(Op::ToKey);
            }
            crate::ast::PropKey::Private(_) => self.unsupported("private names"),
            _ => {
                let key = static_key(k);
                let v = key.to_value();
                match v {
                    super::value::Value::String(s) => {
                        let i = self.string(&s);
                        self.emit(Op::Str(i));
                    }
                    _ => {
                        self.emit(Op::Undef);
                    }
                }
            }
        }
    }

    /// NamedEvaluation (§8.4.5) when `e` is an anonymous function definition.
    fn named_expr(&mut self, e: &'a Expr, name: &str) {
        let mut inner = e;
        while let Expr::Paren(x) = inner {
            inner = x;
        }
        match inner {
            Expr::Function(f) if f.name.is_none() => self.closure(f, true, Some(name)),
            Expr::Arrow(f) => self.closure(f, false, Some(name)),
            Expr::Class(c) if c.name.is_none() => self.class(c, Some(name)),
            _ => self.expr(e),
        }
    }

    fn is_anonymous_fn(e: &Expr) -> bool {
        match e {
            Expr::Paren(x) => Self::is_anonymous_fn(x),
            Expr::Function(f) => f.name.is_none(),
            Expr::Arrow(_) => true,
            Expr::Class(c) => c.name.is_none(),
            _ => false,
        }
    }

    fn args(&mut self, args: &'a [Expr]) -> Option<u32> {
        if args.iter().any(|a| matches!(a, Expr::Spread(_))) {
            self.emit(Op::Array);
            for a in args {
                if let Expr::Spread(x) = a {
                    self.expr(x);
                    self.emit(Op::ArraySpread);
                } else {
                    self.expr(a);
                    self.emit(Op::ArrayPush);
                }
            }
            None
        } else {
            for a in args {
                self.expr(a);
            }
            Some(args.len() as u32)
        }
    }

    fn expr(&mut self, e: &'a Expr) {
        match e {
            Expr::Num(n) => {
                self.emit(Op::Num(*n));
            }
            Expr::BigInt(_) => self.unsupported("BigInt"),
            Expr::Str(s) => {
                let i = self.string(s);
                self.emit(Op::Str(i));
            }
            Expr::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            Expr::Null => {
                self.emit(Op::Null);
            }
            Expr::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::GetVar(i));
            }
            Expr::This => {
                self.emit(Op::This);
            }
            Expr::Regex { body, flags } => {
                self.code.regexps.push((js_str(body), js_str(flags)));
                let i = self.code.regexps.len() as u32 - 1;
                self.emit(Op::RegExp(i));
            }
            Expr::Template(parts) => {
                let mut first = true;
                for p in parts {
                    match p {
                        TemplatePart::Str { cooked, .. } => {
                            let s = cooked.clone().unwrap_or_default();
                            let i = self.string(&s);
                            self.emit(Op::Str(i));
                        }
                        TemplatePart::Expr(x) => {
                            self.expr(x);
                            self.emit(Op::ToStr);
                        }
                    }
                    if !first {
                        self.emit(Op::Add);
                    }
                    first = false;
                }
                if first {
                    self.push_str("");
                }
            }
            Expr::TaggedTemplate { tag, parts } => {
                self.callee(tag);
                let mut cooked = Vec::new();
                let mut raw = Vec::new();
                let mut subs = Vec::new();
                for p in parts {
                    match p {
                        TemplatePart::Str { cooked: c, raw: r } => {
                            cooked.push(c.as_ref().map(|c| c.as_slice().into()));
                            raw.push(js_str(r));
                        }
                        TemplatePart::Expr(x) => subs.push(x),
                    }
                }
                self.code.templates.push((cooked, raw));
                let i = self.code.templates.len() as u32 - 1;
                self.emit(Op::TemplateObject(i));
                for s in &subs {
                    self.expr(s);
                }
                self.emit(Op::Call(subs.len() as u32 + 1));
            }
            Expr::Array(elems) => {
                self.emit(Op::Array);
                for el in elems {
                    match el {
                        None => {
                            self.emit(Op::ArrayHole);
                        }
                        Some(Expr::Spread(x)) => {
                            self.expr(x);
                            self.emit(Op::ArraySpread);
                        }
                        Some(x) => {
                            self.expr(x);
                            self.emit(Op::ArrayPush);
                        }
                    }
                }
            }
            Expr::Object(props) => self.object_literal(props),
            Expr::Function(f) => self.closure(f, true, None),
            Expr::Arrow(f) => self.closure(f, false, None),
            Expr::Class(c) => self.class(c, None),
            Expr::Unary(op, x) => self.unary(op, x),
            Expr::Update { op, prefix, target } => self.update(op, *prefix, target),
            Expr::Binary(op, a, b) => {
                self.expr(a);
                self.expr(b);
                self.emit(binary_op(op));
            }
            Expr::Logical(op, a, b) => {
                self.expr(a);
                let j = self.emit(match *op {
                    "&&" => Op::JumpIfFalseKeep(0),
                    "||" => Op::JumpIfTrueKeep(0),
                    _ => Op::JumpIfNotNullishKeep(0),
                });
                self.expr(b);
                self.patch(j);
            }
            Expr::Assign { op, target, value } => self.assign(op, target, value),
            Expr::Cond(t, a, b) => {
                self.expr(t);
                let j = self.emit(Op::JumpIfFalse(0));
                self.expr(a);
                let k = self.emit(Op::Jump(0));
                self.patch(j);
                self.expr(b);
                self.patch(k);
            }
            Expr::Call { callee, args, optional } => {
                if let Expr::Ident(n) = &**callee {
                    if n == "eval" && !*optional {
                        let i = self.name(n);
                        self.emit(Op::GetVarThis(i));
                        match self.args(args) {
                            Some(n) => self.emit(Op::CallEval(n)),
                            None => self.emit(Op::CallEvalSpread),
                        };
                        return;
                    }
                }
                self.callee(callee);
                if *optional {
                    // [f this]: check f.
                    self.emit(Op::Swap);
                    let j = self.emit(Op::OptCheck(0, 1));
                    self.chain_jump(j);
                    self.emit(Op::Swap);
                }
                match self.args(args) {
                    Some(n) => self.emit(Op::Call(n)),
                    None => self.emit(Op::CallSpread),
                };
            }
            Expr::New { callee, args } => {
                self.expr(callee);
                match self.args(args) {
                    Some(n) => self.emit(Op::New(n)),
                    None => self.emit(Op::NewSpread),
                };
            }
            Expr::Member { object, prop, computed, optional } => {
                self.expr(object);
                if *optional {
                    let j = self.emit(Op::OptCheck(0, 0));
                    self.chain_jump(j);
                }
                if *computed {
                    self.expr(prop);
                    self.emit(Op::GetProp);
                } else {
                    let k = self.member_key(prop);
                    self.emit(Op::GetNamed(k));
                }
            }
            Expr::OptionalChain(inner) => {
                self.chain.push(Vec::new());
                self.expr(inner);
                for j in self.chain.pop().unwrap_or_default() {
                    self.patch(j);
                }
            }
            Expr::PrivateMember { object, name, optional } => {
                self.expr(object);
                if *optional {
                    let j = self.emit(Op::OptCheck(0, 0));
                    self.chain_jump(j);
                }
                self.private_name(name);
                self.emit(Op::PrivateGet);
            }
            Expr::PrivateIn(name, obj) => {
                self.private_name(name);
                self.expr(obj);
                self.emit(Op::PrivateIn);
            }
            Expr::SuperMember { prop, computed } => {
                self.emit(Op::This);
                self.super_key(prop, *computed);
                self.emit(Op::SuperGet);
            }
            Expr::SuperCall(args) => {
                match self.args(args) {
                    Some(n) => self.emit(Op::SuperCall(n)),
                    None => self.emit(Op::SuperCallSpread),
                };
            }
            Expr::Seq(xs) => {
                for (i, x) in xs.iter().enumerate() {
                    self.expr(x);
                    if i + 1 < xs.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            Expr::Spread(x) => self.expr(x),
            Expr::Yield { .. } => self.unsupported("yield"),
            Expr::Await(_) => self.unsupported("await"),
            Expr::NewTarget => {
                self.emit(Op::NewTarget);
            }
            Expr::ImportMeta | Expr::ImportCall(..) | Expr::ImportPhase(..) => self.unsupported("import"),
            Expr::Paren(x) => self.expr(x),
        }
    }

    fn chain_jump(&mut self, j: usize) {
        if let Some(c) = self.chain.last_mut() {
            c.push(j);
        }
    }

    /// `[] → [f this]` for a call.
    fn callee(&mut self, callee: &'a Expr) {
        match callee {
            Expr::Paren(inner) if matches!(**inner, Expr::Member { .. }) => self.callee(inner),
            Expr::Member { object, prop, computed, optional } => {
                self.expr(object);
                if *optional {
                    let j = self.emit(Op::OptCheck(0, 0));
                    self.chain_jump(j);
                }
                self.emit(Op::Dup);
                if *computed {
                    self.expr(prop);
                    self.emit(Op::GetProp);
                } else {
                    let k = self.member_key(prop);
                    self.emit(Op::GetNamed(k));
                }
                self.emit(Op::Swap);
            }
            Expr::Paren(inner)
                if matches!(**inner, Expr::PrivateMember { .. } | Expr::SuperMember { .. }) =>
            {
                self.callee(inner)
            }
            Expr::PrivateMember { object, name, optional } => {
                self.expr(object);
                if *optional {
                    let j = self.emit(Op::OptCheck(0, 0));
                    self.chain_jump(j);
                }
                self.emit(Op::Dup);
                self.private_name(name);
                self.emit(Op::PrivateGet);
                self.emit(Op::Swap);
            }
            Expr::SuperMember { prop, computed } => {
                self.emit(Op::This);
                self.emit(Op::Dup);
                self.super_key(prop, *computed);
                self.emit(Op::SuperGet);
                self.emit(Op::Swap);
            }
            Expr::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::GetVarThis(i));
            }
            Expr::OptionalChain(_) => {
                // `(a?.b)()`: the chain's value, called with its base lost.
                self.expr(callee);
                self.emit(Op::Undef);
            }
            _ => {
                self.expr(callee);
                self.emit(Op::Undef);
            }
        }
    }

    fn object_literal(&mut self, props: &'a [Prop]) {
        self.emit(Op::Object);
        for p in props {
            match p {
                Prop::KeyValue(k, v) => {
                    if let crate::ast::PropKey::Ident(n) = k {
                        if n == "__proto__" {
                            self.expr(v);
                            self.emit(Op::SetProto);
                            continue;
                        }
                    }
                    if let crate::ast::PropKey::Str(s) = k {
                        if s.iter().copied().eq("__proto__".encode_utf16()) {
                            self.expr(v);
                            self.emit(Op::SetProto);
                            continue;
                        }
                    }
                    match k {
                        crate::ast::PropKey::Computed(_) => {
                            self.prop_key(k);
                            self.expr(v);
                            if Self::is_anonymous_fn(v) {
                                self.emit(Op::SetFnName);
                            }
                            self.emit(Op::DefineProp);
                        }
                        _ => {
                            let key = static_key(k);
                            let name = key.display();
                            self.named_expr(v, &name);
                            let ki = self.key(key);
                            self.emit(Op::DefineNamed(ki));
                        }
                    }
                }
                Prop::Shorthand(n, _) => {
                    let i = self.name(n);
                    self.emit(Op::GetVar(i));
                    let k = self.key(PropKey::named(n));
                    self.emit(Op::DefineNamed(k));
                }
                Prop::Method(k, f) | Prop::Getter(k, f) | Prop::Setter(k, f) => {
                    self.prop_key(k);
                    let t = self.template(f, false);
                    let kind = match p {
                        Prop::Getter(..) => 1,
                        Prop::Setter(..) => 2,
                        _ => 0,
                    };
                    self.emit(Op::DefineMethod(t, kind));
                }
                Prop::Spread(x) => {
                    self.expr(x);
                    self.emit(Op::CopyData);
                }
            }
        }
    }

    fn unary(&mut self, op: &str, x: &'a Expr) {
        let mut inner = x;
        if op == "typeof" || op == "delete" {
            while let Expr::Paren(i) = inner {
                inner = i;
            }
        }
        match op {
            "typeof" => {
                if let Expr::Ident(n) = inner {
                    let i = self.name(n);
                    self.emit(Op::TypeofVar(i));
                } else {
                    self.expr(x);
                    self.emit(Op::TypeOf);
                }
            }
            "delete" => match inner {
                Expr::Ident(n) => {
                    let i = self.name(n);
                    self.emit(Op::DeleteVar(i));
                }
                Expr::Member { object, prop, computed, optional: false } => {
                    self.expr(object);
                    if *computed {
                        self.expr(prop);
                    } else {
                        let k = self.member_key(prop);
                        let v = self.code.keys[k as usize].to_value();
                        if let super::value::Value::String(s) = v {
                            let i = self.string(&s);
                            self.emit(Op::Str(i));
                        }
                    }
                    self.emit(Op::DeleteProp);
                }
                Expr::OptionalChain(chain) => {
                    // `delete a?.b`: true when short-circuited.
                    if let Expr::Member { object, prop, computed, optional } = &**chain {
                        self.chain.push(Vec::new());
                        self.expr(object);
                        if *optional {
                            let j = self.emit(Op::OptCheck(0, 0));
                            self.chain_jump(j);
                        }
                        if *computed {
                            self.expr(prop);
                        } else {
                            let k = self.member_key(prop);
                            let v = self.code.keys[k as usize].to_value();
                            if let super::value::Value::String(s) = v {
                                let i = self.string(&s);
                                self.emit(Op::Str(i));
                            }
                        }
                        self.emit(Op::DeleteProp);
                        let done = self.emit(Op::Jump(0));
                        for j in self.chain.pop().unwrap_or_default() {
                            self.patch(j);
                        }
                        self.emit(Op::Pop);
                        self.emit(Op::True);
                        self.patch(done);
                    } else {
                        self.expr(x);
                        self.emit(Op::Pop);
                        self.emit(Op::True);
                    }
                }
                Expr::SuperMember { prop, computed } => {
                    // §13.5.1.2 step 5.a: evaluated, then a ReferenceError.
                    self.emit(Op::This);
                    self.super_key(prop, *computed);
                    self.emit(Op::Pop);
                    self.emit(Op::Pop);
                    self.throw_reference_error("Unsupported reference to 'super'");
                }
                _ => {
                    self.expr(x);
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                }
            },
            "void" => {
                self.expr(x);
                self.emit(Op::Pop);
                self.emit(Op::Undef);
            }
            "!" => {
                self.expr(x);
                self.emit(Op::Not);
            }
            "-" => {
                self.expr(x);
                self.emit(Op::Neg);
            }
            "+" => {
                self.expr(x);
                self.emit(Op::Plus);
            }
            "~" => {
                self.expr(x);
                self.emit(Op::BitNot);
            }
            _ => self.unsupported(op),
        }
    }

    fn update(&mut self, op: &str, prefix: bool, target: &'a Expr) {
        let step = if op == "++" { Op::Inc } else { Op::Dec };
        let mut t = target;
        while let Expr::Paren(i) = t {
            t = i;
        }
        match t {
            Expr::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::GetVar(i));
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(step);
                    self.emit(Op::SetVar(i));
                } else {
                    self.emit(Op::Dup);
                    self.emit(step);
                    self.emit(Op::SetVar(i));
                    self.emit(Op::Pop);
                }
            }
            Expr::Call { .. } => {
                self.expr(t);
                self.emit(Op::Pop);
                self.throw_reference_error("invalid update target");
            }
            _ => match self.push_ref(t, true) {
                Some(r) => {
                    self.ref_dup(r);
                    self.ref_get(r);
                    self.emit(Op::ToNumeric);
                    if prefix {
                        self.emit(step);
                        self.ref_set(r);
                    } else {
                        self.emit(Op::Dup);
                        self.emit(Op::Bury(r.parts() + 1));
                        self.emit(step);
                        self.ref_set(r);
                        self.emit(Op::Pop);
                    }
                }
                None => self.unsupported("this update target"),
            },
        }
    }

    fn assign(&mut self, op: &str, target: &'a Pat, value: &'a Expr) {
        if op == "=" {
            match target {
                Pat::Ident(n) => {
                    self.named_expr(value, n);
                    let i = self.name(n);
                    self.emit(Op::SetVar(i));
                }
                Pat::Expr(e) => self.assign_expr_target(e, value),
                _ => {
                    self.expr(value);
                    self.emit(Op::Dup);
                    self.bind(target, Bind::Assign);
                }
            }
            return;
        }
        let logical = matches!(op, "&&=" | "||=" | "??=");
        let jump = |op: &str| match op {
            "&&=" => Op::JumpIfFalseKeep(0),
            "||=" => Op::JumpIfTrueKeep(0),
            _ => Op::JumpIfNotNullishKeep(0),
        };
        let bin = binary_op(&op[..op.len() - 1]);
        let target_expr = match target {
            Pat::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::GetVar(i));
                if logical {
                    let j = self.emit(jump(op));
                    self.named_expr(value, n);
                    self.emit(Op::SetVar(i));
                    self.patch(j);
                } else {
                    self.expr(value);
                    self.emit(bin);
                    self.emit(Op::SetVar(i));
                }
                return;
            }
            Pat::Expr(e) => e,
            _ => {
                self.unsupported("this assignment");
                return;
            }
        };
        let mut t: &Expr = target_expr;
        while let Expr::Paren(i) = t {
            t = i;
        }
        match t {
            Expr::Ident(n) => {
                let i = self.name(n);
                self.emit(Op::GetVar(i));
                self.expr(value);
                self.emit(bin);
                self.emit(Op::SetVar(i));
            }
            Expr::Call { .. } => {
                self.expr(t);
                self.emit(Op::Pop);
                self.throw_reference_error("invalid assignment target");
            }
            _ => match self.push_ref(t, true) {
                Some(r) => {
                    self.ref_dup(r);
                    self.ref_get(r);
                    if logical {
                        let j = self.emit(jump(op));
                        self.expr(value);
                        self.ref_set(r);
                        let done = self.emit(Op::Jump(0));
                        self.patch(j);
                        // [parts old] → [old]
                        self.emit(Op::Bury(r.parts()));
                        for _ in 0..r.parts() {
                            self.emit(Op::Pop);
                        }
                        self.patch(done);
                    } else {
                        self.expr(value);
                        self.emit(bin);
                        self.ref_set(r);
                    }
                }
                None => self.unsupported("this assignment"),
            },
        }
    }

    /// `a.b = v`, `a[k] = v`: the reference is evaluated before the value (§13.15.2).
    fn assign_expr_target(&mut self, e: &'a Expr, value: &'a Expr) {
        let mut t = e;
        while let Expr::Paren(i) = t {
            t = i;
        }
        match t {
            Expr::Ident(n) => {
                self.expr(value);
                let i = self.name(n);
                self.emit(Op::SetVar(i));
            }
            Expr::Call { .. } => {
                self.expr(t);
                self.emit(Op::Pop);
                self.throw_reference_error("invalid assignment target");
            }
            _ => match self.push_ref(t, false) {
                Some(r) => {
                    self.expr(value);
                    self.ref_set(r);
                }
                None => self.unsupported("this assignment target"),
            },
        }
    }

    // ---- classes (§15.7) ----

    /// ClassDefinitionEvaluation (§15.7.14): `[] → [F]`.
    fn class(&mut self, c: &'a Class, given: Option<&str>) {
        if !c.decorators.is_empty() || !c.member_decorators.is_empty() {
            self.unsupported("decorators");
            return;
        }
        self.emit(Op::PushScope);
        self.ctl.push(Ctl::Scope);
        if let Some(n) = &c.name {
            let i = self.name(n);
            self.emit(Op::DeclareConst(i));
        }
        let mut privs: Vec<&str> = Vec::new();
        for m in &c.members {
            if let ClassMember::Method { key: crate::ast::PropKey::Private(n), .. }
            | ClassMember::Field { key: crate::ast::PropKey::Private(n), .. } = m
            {
                if !privs.contains(&n.as_str()) {
                    privs.push(n);
                }
            }
        }
        for n in privs {
            let d = format!("#{n}");
            let si = self.string(&js_str(&d));
            self.emit(Op::NewPrivateName(si));
            let i = self.name(&d);
            self.emit(Op::DeclareConst(i));
            self.emit(Op::InitBinding(i));
        }
        let derived = c.extends.is_some();
        if let Some(h) = &c.extends {
            self.expr(h);
        }
        let ctor: Rc<Function> = match &c.constructor {
            Some(f) => f.clone(),
            None => Rc::new(Function {
                name: None,
                params: Vec::new(),
                rest: None,
                body: Vec::new(),
                expression_body: false,
                kind: if derived { FuncKind::DerivedConstructor } else { FuncKind::ClassConstructor },
                is_async: false,
                is_generator: false,
                strict: true,
                span: c.span,
            }),
        };
        let t = FuncTemplate {
            length: expected_args(&ctor),
            ast: ctor,
            source: self.source.clone(),
            code: OnceCell::new(),
            is_expression: false,
            given_name: Some(c.name.as_deref().or(given).unwrap_or("").into()),
            default_ctor: c.constructor.is_none(),
            text_span: Some(c.span),
        };
        self.code.funcs.push(Rc::new(t));
        let ti = self.code.funcs.len() as u32 - 1;
        self.emit(Op::ClassCreate(ti, derived));
        for m in &c.members {
            match m {
                ClassMember::Method { key, is_static, kind, func } => {
                    let private = matches!(key, crate::ast::PropKey::Private(_));
                    self.class_key(key);
                    let t = self.template(func, false);
                    let k = match kind {
                        FuncKind::Getter => 1,
                        FuncKind::Setter => 2,
                        _ => 0,
                    };
                    self.emit(Op::ClassMethod(t, k, *is_static as u8 | if private { 2 } else { 0 }));
                }
                ClassMember::Field { key, is_static, value } => {
                    self.class_key(key);
                    let t = value.as_ref().map(|v| self.template(v, false)).unwrap_or(u32::MAX);
                    self.emit(Op::ClassField(t, *is_static));
                }
                ClassMember::StaticBlock(f) => {
                    let t = self.template(f, false);
                    self.emit(Op::ClassStaticBlock(t));
                }
            }
        }
        if let Some(n) = &c.name {
            self.emit(Op::Pick(1));
            let i = self.name(n);
            self.emit(Op::InitBinding(i));
        }
        self.emit(Op::ClassFinish);
        self.ctl.pop();
        self.emit(Op::PopScope);
    }

    fn class_key(&mut self, k: &'a crate::ast::PropKey) {
        match k {
            crate::ast::PropKey::Private(n) => self.private_name(n),
            _ => self.prop_key(k),
        }
    }
}

#[derive(Clone, Copy)]
enum MRef {
    Named(u32),
    Computed,
    Private,
    Super,
}

impl MRef {
    fn parts(self) -> u8 {
        match self {
            MRef::Named(_) => 1,
            _ => 2,
        }
    }
}

fn binary_op(op: &str) -> Op {
    match op {
        "+" => Op::Add,
        "-" => Op::Sub,
        "*" => Op::Mul,
        "/" => Op::Div,
        "%" => Op::Mod,
        "**" => Op::Exp,
        "<<" => Op::Shl,
        ">>" => Op::Shr,
        ">>>" => Op::UShr,
        "&" => Op::BitAnd,
        "|" => Op::BitOr,
        "^" => Op::BitXor,
        "==" => Op::Eq,
        "!=" => Op::Ne,
        "===" => Op::StrictEq,
        "!==" => Op::StrictNe,
        "<" => Op::Lt,
        ">" => Op::Gt,
        "<=" => Op::Le,
        ">=" => Op::Ge,
        "instanceof" => Op::InstanceOf,
        "in" => Op::In,
        _ => Op::Nop,
    }
}

fn static_key(k: &crate::ast::PropKey) -> PropKey {
    match k {
        crate::ast::PropKey::Ident(n) => PropKey::named(n),
        crate::ast::PropKey::Str(s) => PropKey::from_js(s.as_slice().into()),
        crate::ast::PropKey::Num(n) => PropKey::from_f64(*n),
        crate::ast::PropKey::BigInt(b) => PropKey::named(b),
        _ => PropKey::named(""),
    }
}

/// ExpectedArgumentCount (§15.1.5): parameters before the first default or rest.
fn expected_args(f: &Function) -> u32 {
    f.params.iter().take_while(|p| !matches!(p, Pat::Default(..))).count() as u32
}

pub fn pat_names(p: &Pat, out: &mut Vec<String>) {
    match p {
        Pat::Ident(n) => out.push(n.clone()),
        Pat::Array(es, rest) => {
            for e in es.iter().flatten() {
                pat_names(e, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Object(ps, rest) => {
            for (_, e) in ps {
                pat_names(e, out);
            }
            if let Some(r) = rest {
                pat_names(r, out);
            }
        }
        Pat::Default(t, _) => pat_names(t, out),
        Pat::Expr(_) => {}
    }
}

/// ContainsExpression (§8.5.2) for parameter lists.
fn pat_has_expr(p: &Pat) -> bool {
    match p {
        Pat::Ident(_) | Pat::Expr(_) => false,
        Pat::Default(..) => true,
        Pat::Array(es, rest) => {
            es.iter().flatten().any(pat_has_expr) || rest.as_deref().is_some_and(pat_has_expr)
        }
        Pat::Object(ps, rest) => {
            ps.iter().any(|(k, v)| matches!(k, crate::ast::PropKey::Computed(_)) || pat_has_expr(v))
                || rest.as_deref().is_some_and(pat_has_expr)
        }
    }
}

/// VarDeclaredNames (§8.2.6) of a statement list, not descending into functions.
/// `top`: function declarations at this level count as vars (function/script top level).
fn var_names(body: &[Stmt], out: &mut Vec<String>, top: bool) {
    for s in body {
        stmt_var_names(s, out, top);
    }
}

fn stmt_var_names(s: &Stmt, out: &mut Vec<String>, top: bool) {
    match s {
        Stmt::Var(VarKind::Var, ds) => {
            for (p, _) in ds {
                pat_names(p, out);
            }
        }
        Stmt::Function(f) if top => out.extend(f.name.clone()),
        Stmt::Block(b) => var_names(b, out, false),
        Stmt::If(_, a, b) => {
            stmt_var_names(a, out, false);
            if let Some(b) = b {
                stmt_var_names(b, out, false);
            }
        }
        Stmt::For { init, body, .. } => {
            if let Some(ForInit::Var(VarKind::Var, ds)) = init {
                for (p, _) in ds {
                    pat_names(p, out);
                }
            }
            stmt_var_names(body, out, false);
        }
        Stmt::ForIn { head, body, .. } | Stmt::ForOf { head, body, .. } => {
            if let ForHead::Var(VarKind::Var, p) = head {
                pat_names(p, out);
            }
            stmt_var_names(body, out, false);
        }
        Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::With(_, b) => stmt_var_names(b, out, false),
        Stmt::Labeled(_, b) => stmt_var_names(b, out, top),
        Stmt::Try { block, handler, finalizer, .. } => {
            var_names(block, out, false);
            if let Some(h) = handler {
                var_names(h, out, false);
            }
            if let Some(f) = finalizer {
                var_names(f, out, false);
            }
        }
        Stmt::Switch(_, cases) => {
            for c in cases {
                var_names(&c.body, out, false);
            }
        }
        Stmt::ExportDecl(d) => stmt_var_names(d, out, top),
        _ => {}
    }
}

/// Top-level function declarations (var-scoped at function/script level), in order.
fn top_functions(body: &[Stmt]) -> Vec<&Rc<Function>> {
    let mut out = Vec::new();
    for s in body {
        let mut s = s;
        while let Stmt::Labeled(_, b) = s {
            s = b;
        }
        if let Stmt::Function(f) = s {
            out.push(f);
        }
    }
    out
}

/// Top-level lexical declarations (let/const/class), not functions.
fn top_lexical(body: &[Stmt], out: &mut Vec<(String, bool)>) {
    for s in body {
        match s {
            Stmt::Var(k @ (VarKind::Let | VarKind::Const | VarKind::Using | VarKind::AwaitUsing), ds) => {
                let mut n = Vec::new();
                for (p, _) in ds {
                    pat_names(p, &mut n);
                }
                out.extend(n.into_iter().map(|n| (n, *k != VarKind::Let)));
            }
            Stmt::Class(c) => out.extend(c.name.clone().map(|n| (n, false))),
            _ => {}
        }
    }
}

/// A block's lexical declarations: let/const/class names, and its function declarations.
fn block_lexical(body: &[Stmt], out: &mut Vec<(String, bool)>, funcs: &mut Vec<Rc<Function>>) {
    top_lexical(body, out);
    for s in body {
        let mut s = s;
        while let Stmt::Labeled(_, b) = s {
            s = b;
        }
        if let Stmt::Function(f) = s {
            funcs.push(f.clone());
        }
    }
}

/// Annex B.3.2: function declarations in blocks (sloppy code) that would not conflict with
/// a lexical declaration if they were `var`s.
fn annex_b_functions(
    body: &[Stmt],
    top_lex: &HashSet<String>,
    params: &HashSet<String>,
    out: &mut Vec<Rc<Function>>,
) {
    let mut stack: Vec<HashSet<String>> = vec![top_lex.clone()];
    for s in body {
        annex_b_stmt(s, &mut stack, params, out, true);
    }
}

fn annex_b_block(
    body: &[Stmt],
    stack: &mut Vec<HashSet<String>>,
    params: &HashSet<String>,
    out: &mut Vec<Rc<Function>>,
) {
    let mut lex = Vec::new();
    let mut funcs = Vec::new();
    block_lexical(body, &mut lex, &mut funcs);
    let names: HashSet<String> = lex.into_iter().map(|l| l.0).collect();
    for f in &funcs {
        let n = f.name.clone().unwrap_or_default();
        // Conflicts: an enclosing scope's lexical name, or a let/const of this block.
        if !params.contains(&n) && !stack.iter().any(|s| s.contains(&n)) && !names.contains(&n) {
            if !f.is_async && !f.is_generator {
                out.push(f.clone());
            }
        }
    }
    let mut here = names;
    for f in &funcs {
        here.insert(f.name.clone().unwrap_or_default());
    }
    // Inner blocks see this block's functions as lexical too, except the same name hoists
    // through (it would be the same var).
    stack.push(
        here.into_iter().filter(|n| !funcs.iter().any(|f| f.name.as_deref() == Some(n.as_str()))).collect(),
    );
    for s in body {
        annex_b_stmt(s, stack, params, out, false);
    }
    stack.pop();
}

fn annex_b_stmt(
    s: &Stmt,
    stack: &mut Vec<HashSet<String>>,
    params: &HashSet<String>,
    out: &mut Vec<Rc<Function>>,
    top: bool,
) {
    match s {
        Stmt::Block(b) => annex_b_block(b, stack, params, out),
        Stmt::If(_, a, b) => {
            // `if (x) function f(){}` (B.3.3): as if in a block.
            for branch in std::iter::once(&**a).chain(b.as_deref()) {
                if let Stmt::Function(_) = branch {
                    annex_b_block(std::slice::from_ref(branch), stack, params, out);
                } else {
                    annex_b_stmt(branch, stack, params, out, false);
                }
            }
        }
        Stmt::For { init, body, .. } => {
            let mut names = HashSet::new();
            if let Some(ForInit::Var(VarKind::Let | VarKind::Const, ds)) = init {
                let mut n = Vec::new();
                for (p, _) in ds {
                    pat_names(p, &mut n);
                }
                names.extend(n);
            }
            stack.push(names);
            annex_b_stmt(body, stack, params, out, false);
            stack.pop();
        }
        Stmt::ForIn { head, body, .. } | Stmt::ForOf { head, body, .. } => {
            let mut names = HashSet::new();
            if let ForHead::Var(VarKind::Let | VarKind::Const, p) = head {
                let mut n = Vec::new();
                pat_names(p, &mut n);
                names.extend(n);
            }
            stack.push(names);
            annex_b_stmt(body, stack, params, out, false);
            stack.pop();
        }
        Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::With(_, b) => {
            annex_b_stmt(b, stack, params, out, false)
        }
        Stmt::Labeled(_, b) => {
            if !top {
                annex_b_stmt(b, stack, params, out, false)
            }
        }
        Stmt::Try { block, param, handler, finalizer } => {
            annex_b_block(block, stack, params, out);
            if let Some(h) = handler {
                // B.3.4: a simple catch parameter doesn't block hoisting; a pattern does.
                let mut names = HashSet::new();
                if let Some(p) = param {
                    if !matches!(p, Pat::Ident(_)) {
                        let mut n = Vec::new();
                        pat_names(p, &mut n);
                        names.extend(n);
                    }
                }
                stack.push(names);
                annex_b_block(h, stack, params, out);
                stack.pop();
            }
            if let Some(f) = finalizer {
                annex_b_block(f, stack, params, out);
            }
        }
        Stmt::Switch(_, cases) => {
            // One case block; cloned statements share the functions' `Rc`s.
            let all: Vec<Stmt> = cases.iter().flat_map(|c| c.body.iter().cloned()).collect();
            annex_b_block(&all, stack, params, out);
        }
        _ => {}
    }
}

//! The abstract syntax tree produced by the parser.

use crate::lexer::JsStr;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropKey {
    Ident(String),
    Str(JsStr),
    Num(f64),
    BigInt(String),
    Computed(Box<Expr>),
    Private(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Ident(String),
    /// Elements (holes are `None`), then a rest pattern.
    Array(Vec<Option<Pat>>, Option<Box<Pat>>),
    Object(Vec<(PropKey, Pat)>, Option<Box<Pat>>),
    /// `target = default`
    Default(Box<Pat>, Box<Expr>),
    /// A member expression as an assignment target (`a.b`, `a[0]`).
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuncKind {
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    ClassConstructor,
    DerivedConstructor,
    ClassField,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<Pat>,
    pub rest: Option<Pat>,
    pub body: Vec<Stmt>,
    /// Arrow functions with an expression body: `body` is one `Return`.
    pub expression_body: bool,
    pub kind: FuncKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub strict: bool,
    /// Source text range (for `Function.prototype.toString`).
    pub span: (usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClassMember {
    Method {
        key: PropKey,
        is_static: bool,
        kind: FuncKind,
        func: Rc<Function>,
    },
    /// `accessor`: an auto-accessor (a private storage field with a getter and setter).
    Field {
        key: PropKey,
        is_static: bool,
        value: Option<Rc<Function>>,
        accessor: bool,
    },
    StaticBlock(Rc<Function>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub name: Option<String>,
    pub extends: Option<Box<Expr>>,
    pub constructor: Option<Rc<Function>>,
    pub members: Vec<ClassMember>,
    /// `@dec class …` (stage 3 decorators; applied at runtime later).
    pub decorators: Vec<Expr>,
    /// Decorators of members: (index into `members`, decorators).
    pub member_decorators: Vec<(usize, Vec<Expr>)>,
    pub span: (usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    KeyValue(PropKey, Expr),
    /// `{ a }` (and `{ a = 1 }` while it may still become a pattern).
    Shorthand(String, Option<Box<Expr>>),
    Method(PropKey, Rc<Function>),
    Getter(PropKey, Rc<Function>),
    Setter(PropKey, Rc<Function>),
    Spread(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    Str { cooked: Option<JsStr>, raw: String },
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    BigInt(String),
    Str(JsStr),
    Bool(bool),
    Null,
    Ident(String),
    This,
    /// A regular expression literal: its body as UTF-16 (it may hold lone surrogates).
    Regex {
        body: JsStr,
        flags: String,
    },
    Template(Vec<TemplatePart>),
    TaggedTemplate {
        tag: Box<Expr>,
        parts: Vec<TemplatePart>,
    },
    Array(Vec<Option<Expr>>),
    Object(Vec<Prop>),
    Function(Rc<Function>),
    Arrow(Rc<Function>),
    Class(Rc<Class>),
    Unary(&'static str, Box<Expr>),
    Update {
        op: &'static str,
        prefix: bool,
        target: Box<Expr>,
    },
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Logical(&'static str, Box<Expr>, Box<Expr>),
    Assign {
        op: &'static str,
        target: Box<Pat>,
        value: Box<Expr>,
    },
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        optional: bool,
    },
    New {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Member {
        object: Box<Expr>,
        prop: Box<Expr>,
        computed: bool,
        optional: bool,
    },
    PrivateMember {
        object: Box<Expr>,
        name: String,
        optional: bool,
    },
    /// `#x in obj`
    PrivateIn(String, Box<Expr>),
    /// The whole `a?.b.c` chain (short-circuits as a unit).
    OptionalChain(Box<Expr>),
    SuperMember {
        prop: Box<Expr>,
        computed: bool,
    },
    SuperCall(Vec<Expr>),
    Seq(Vec<Expr>),
    Spread(Box<Expr>),
    Yield {
        arg: Option<Box<Expr>>,
        delegate: bool,
    },
    Await(Box<Expr>),
    NewTarget,
    ImportMeta,
    ImportCall(Box<Expr>, Option<Box<Expr>>),
    /// `import.source(x)` / `import.defer(x)` (source phase and deferred imports).
    ImportPhase(&'static str, Box<Expr>),
    /// `(expr)`: kept so `(a) = 1` vs `({a}) = 1` can be told apart.
    Paren(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForInit {
    Var(VarKind, Vec<(Pat, Option<Expr>)>),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForHead {
    Var(VarKind, Pat),
    Pat(Pat),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportSpec {
    /// `import source x from "m"`: the module source object.
    Source(String),
    /// `import defer * as ns from "m"`.
    DeferNamespace(String),
    Default(String),
    Namespace(String),
    Named {
        imported: JsStr,
        local: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expr(Expr),
    Var(VarKind, Vec<(Pat, Option<Expr>)>),
    Function(Rc<Function>),
    Class(Rc<Class>),
    Block(Vec<Stmt>),
    Empty,
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    ForIn {
        head: ForHead,
        right: Expr,
        body: Box<Stmt>,
    },
    ForOf {
        head: ForHead,
        right: Expr,
        body: Box<Stmt>,
        is_await: bool,
    },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break(Option<String>),
    Continue(Option<String>),
    Throw(Expr),
    Try {
        block: Vec<Stmt>,
        param: Option<Pat>,
        handler: Option<Vec<Stmt>>,
        finalizer: Option<Vec<Stmt>>,
    },
    Switch(Expr, Vec<Case>),
    Labeled(String, Box<Stmt>),
    With(Expr, Box<Stmt>),
    Debugger,
    /// `ty`: the `type` import attribute (`with { type: "json" }`).
    Import {
        specs: Vec<ImportSpec>,
        source: JsStr,
        ty: Option<JsStr>,
    },
    /// `export { a as b }`, `export * from`, re-exports: (exported name, local or imported name).
    ExportNamed {
        specs: Vec<(JsStr, JsStr)>,
        source: Option<JsStr>,
        star: bool,
        ty: Option<JsStr>,
    },
    ExportDecl(Box<Stmt>),
    ExportDefault(Box<Stmt>),
    ExportDefaultExpr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub body: Vec<Stmt>,
    pub strict: bool,
    pub module: bool,
}

//! Bytecode: a stack machine's instructions, and the compiled form of a script or function.

use super::value::JsString;
use crate::ast;
use std::cell::OnceCell;
use std::rc::Rc;

/// One instruction. Stack effects are written `[before] → [after]` (top on the right).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Undef,
    Null,
    True,
    False,
    Num(f64),
    /// BigInt constant `bigints[i]`.
    BigInt(u32),
    /// String constant.
    Str(u32),
    Pop,
    Dup,
    /// `[a b] → [a b a b]`
    Dup2,
    Swap,
    /// `[] → [cap]`: a `using` scope's DisposeCapability.
    DisposeScope,
    /// `[cap v] → [cap v]`: AddDisposableResource (true: async-dispose).
    AddDisposable(bool),
    /// `[cap exc] → [cap]`: fold a throw completion into the capability.
    DisposeError,
    /// `[cap] → [cap v]` with `v` to await, or jump when DisposeResources is done.
    DisposeStep(u32),
    /// `[cap] → []`: finish DisposeResources; throws the completion's error.
    DisposeSync,
    /// Move the top value down so `n` values sit above it.
    Bury(u8),
    /// Copy the value `n` below the top onto the top (`Pick(0)` = `Dup`).
    Pick(u8),
    /// Move the value `n` below the top to the top (`Roll(1)` = `Swap`).
    Roll(u8),

    // Bindings: names are `names[i]`.
    GetVar(u32),
    /// `typeof x`: unresolvable is `undefined`, not a ReferenceError.
    TypeofVar(u32),
    /// `[] → [value this]` for calls: `this` is the `with` object the name was found on.
    GetVarThis(u32),
    /// PutValue on an identifier reference: `[v] → [v]`.
    SetVar(u32),
    /// Initialize (or overwrite) the binding in the current lexical environment: `[v] → []`.
    InitBinding(u32),
    DeclareLet(u32),
    DeclareConst(u32),
    /// A `var` binding in the variable environment, `undefined` unless it exists.
    DeclareVar(u32),
    /// `[v] → []`: SetMutableBinding in the variable environment (hoisted functions).
    SetVarEnv(u32),
    DeleteVar(u32),
    PushScope,
    PopScope,
    /// CreatePerIterationEnvironment (§14.7.4.4): copy the current scope.
    CopyScope,
    /// `[obj] → []`: an object environment for `with`.
    PushWith,
    /// A new declarative environment that is both the lexical and the variable environment.
    PushVarScope,
    /// GlobalDeclarationInstantiation (§16.1.7) for this script.
    GlobalInit,
    /// EvalDeclarationInstantiation (§19.2.1.3) for this eval code.
    EvalInit,
    This,
    NewTarget,

    // Objects.
    Object,
    /// `[] → [array]`
    Array,
    /// `[arr v] → [arr]`
    ArrayPush,
    ArrayHole,
    /// `[arr iterable] → [arr]`
    ArraySpread,
    /// `[obj key] → [v]`
    GetProp,
    /// `[obj] → [v]` with the key `keys[i]`.
    GetNamed(u32),
    /// `[obj key v] → [v]`
    SetProp,
    /// `[obj v] → [v]`
    SetNamed(u32),
    /// `[obj key v] → [obj]` CreateDataPropertyOrThrow (literals).
    DefineProp,
    /// `[obj v] → [obj]`
    DefineNamed(u32),
    /// `[obj key] → [obj]`: a method, getter or setter (0, 1, 2) from template `tmpl`.
    DefineMethod(u32, u8),
    /// `[obj v] → [obj]`: `__proto__: v` in a literal.
    SetProto,
    /// `[target src] → [target]` CopyDataProperties.
    CopyData,
    /// `[src excluded-keys-array] → [new object]` (object rest).
    ObjRest,
    /// `[obj key] → [bool]`
    DeleteProp,
    /// `[arr src src key] → [arr src src key]`: append the key to `arr` (object rest).
    AppendKey,
    RequireCoercible,

    // Calls.
    /// `[f this a1..an] → [r]`
    Call(u32),
    /// `[f this argsArray] → [r]`
    CallSpread,
    /// Direct `eval` candidates.
    CallEval(u32),
    CallEvalSpread,
    /// `[ctor a1..an] → [obj]`
    New(u32),
    NewSpread,
    /// A closure from template `funcs[i]`; named function expressions get their own scope.
    Closure(u32),
    /// `[key fn] → [key fn]`: SetFunctionName from a computed key.
    SetFnName,
    Arg(u32),
    RestArgs(u32),
    CreateArguments,
    TemplateObject(u32),
    RegExp(u32),

    // Operators.
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    InstanceOf,
    In,
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Inc,
    Dec,
    ToNumeric,
    ToKey,
    /// ToString for template substitutions.
    ToStr,

    // Control.
    Jump(u32),
    /// Pops the condition.
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// Keep the value when jumping, pop it otherwise (`&&`, `||`).
    JumpIfFalseKeep(u32),
    JumpIfTrueKeep(u32),
    JumpIfNotNullishKeep(u32),
    /// Peek: jump if not `undefined` (default values).
    JumpIfNotUndef(u32),
    /// Optional chaining: if the top is null/undefined, pop it and `extra` more values,
    /// push `undefined` and jump.
    OptCheck(u32, u8),
    Throw,
    /// `[v] → []` into the frame's return register.
    SetRet,
    GetRet,
    /// `[v] →` return.
    Ret,
    /// Catch handler at the target; the stack depth and scope are restored there.
    TryStart(u32),
    TryEnd,
    /// `[obj] → [iter]`
    ForInStart,
    /// `[iter] → [iter key]`, or pop and jump when done.
    ForInNext(u32),
    /// `[iterable] → [record]`
    GetIter,
    /// `[rec] → [rec v]`, or pop the record and jump when done.
    IterNext(u32),
    /// `[rec] → [rec v]` (undefined once done): destructuring.
    IterValue,
    /// `[rec] → [rec array]` the rest.
    IterRest,
    /// `[rec] → []`: IteratorClose unless done.
    IterClose,
    /// `[rec exc] →` close (ignoring errors), rethrow.
    IterCloseThrow,
    /// `[v] → []` completion value of script/eval code.
    SetCompletion,
    /// Parameter initializers run between these: a direct eval there may not declare
    /// `var arguments` (§19.2.1.3 step 3).
    ParamsStart,
    ParamsEnd,
    // Modules (§16.2).
    /// `[specifier options] → [promise]` dynamic `import()` (§13.3.10).
    ImportCall,
    /// `import.source(…)`: a source text module has no source phase (§16.2.1.7).
    ImportSourceCall,
    ImportMeta,
    // Async functions (§27.7).
    /// First instruction of an async function: run the rest as its own run, `→ [promise]`.
    AsyncStart,
    /// `[v] →` park until `v` (as a promise) settles; followed by `Resume`.
    Await,
    // Async generators and async iteration (§27.6, §14.7.5).
    /// Park the frame in a new async generator object and return that.
    AsyncGenStart,
    /// `[v] →` AsyncGeneratorYield: settle the front request, then continue with the next
    /// one (resumption value on the stack) or suspend.
    AsyncYield,
    /// `[rec received] → [rec mode innerResult]`: call next/throw/return of the delegate;
    /// a return without a `return` method jumps to the target with the value.
    YieldStarAsync(u32),
    /// `[rec mode result] → [rec value]`, or done: the value, to the first target (or the
    /// second for a return completion).
    YieldStarAsyncCheck(u32, u32),
    /// `[iterable] → [record]` via @@asyncIterator or CreateAsyncFromSyncIterator.
    GetAsyncIter,
    /// `[rec] → [rec result]` (calls `next`, the result still to be awaited).
    IterNextAsync,
    /// `[rec result] → [rec value]`, or when done pop both and jump.
    IterResultCheck(u32),
    /// `[rec] → [returnResult]` (to await), or jump when there is nothing to close.
    AsyncIterClose(u32),
    /// `[rec exc] →` call `return` (ignoring its errors), rethrow.
    AsyncIterCloseThrow,
    /// `[v] →` TypeError unless an object.
    CheckObject,
    // Generators (§27.5).
    /// Park the frame in a new generator object and return that (after the parameters).
    GeneratorStart,
    /// `[v] →` suspend, yielding `v`.
    Yield,
    /// After a yield: the resumed value is on the stack; a `throw()` throws it and a
    /// `return()` jumps to the return path at the target.
    Resume(u32),
    /// `[rec received] →` one step of `yield*` (§14.4.14): suspends (re-running this
    /// instruction on resumption) or leaves the result and jumps to the first target (inner
    /// iterator done) or the second (return completion).
    YieldStar(u32, u32),
    // Classes (§15.7).
    /// A new Private Name described by `strings[i]`.
    NewPrivateName(u32),
    /// `[superclass?] → [F proto]`: the constructor from template `i`; `true`: has heritage.
    ClassCreate(u32, bool),
    /// `[F proto key] → [F proto]`: method/getter/setter (0/1/2) from template; flags 1 =
    /// static, 2 = private.
    ClassMethod(u32, u8, u8),
    /// `[F proto key] → [F proto]`: a field with initializer template (`u32::MAX`: none).
    ClassField(u32, bool),
    /// `[F proto] → [F proto]`: a static block from template `i`.
    ClassStaticBlock(u32),
    /// `[F proto] → [F]`: static private methods, then static fields and blocks in order.
    ClassFinish,
    /// InitializeInstanceElements for `this` (base class constructors).
    InitInstance,
    /// `[fn] → [fn]`: name an anonymous function after the field being initialized.
    SetFnNameField,
    /// `[a1..an] → [this]`
    SuperCall(u32),
    SuperCallSpread,
    /// The implicit derived constructor: `super(...arguments)` without iterating them.
    SuperCallForward,
    /// `[this key] → [v]`
    SuperGet,
    /// `[this key v] → [v]`
    SuperSet,
    /// `[obj name] → [v]`
    PrivateGet,
    /// `[obj name v] → [v]`
    PrivateSet,
    /// `[name obj] → [bool]`
    PrivateIn,
    /// Throw a SyntaxError for a construct the interpreter doesn't run yet (`strings[i]`).
    Unsupported(u32),
    Nop,
}

/// Declarations a script or eval code instantiates before running (§16.1.7, §19.2.1.3).
#[derive(Default, Debug)]
pub struct Decls {
    pub lexical: Vec<(Rc<str>, bool)>,
    pub vars: Vec<Rc<str>>,
    /// (name, template index), last declaration wins.
    pub functions: Vec<(Rc<str>, u32)>,
    /// Annex B.3.2 block functions that also get a var binding.
    pub annex_b: Vec<Rc<str>>,
}

#[derive(Default)]
pub struct Code {
    pub ops: Vec<Op>,
    pub strings: Vec<JsString>,
    pub names: Vec<Rc<str>>,
    pub keys: Vec<super::value::PropKey>,
    pub bigints: Vec<Rc<super::bigint::BigInt>>,
    pub funcs: Vec<Rc<FuncTemplate>>,
    /// Tagged templates: (cooked, raw) strings; the object is cached per site.
    pub templates: Vec<(Vec<Option<JsString>>, Vec<JsString>)>,
    pub regexps: Vec<(JsString, JsString)>,
    pub strict: bool,
    pub decls: Decls,
    /// The script or module this code belongs to (for `import()` and `import.meta`).
    pub referrer: Option<Rc<str>>,
    /// Module code: its imports and exports (§16.2.1.6).
    pub module: Option<Box<ModuleInfo>>,
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    pub request: String,
    /// `None`: `import * as ns` (the namespace object).
    pub import_name: Option<JsString>,
    pub local: Rc<str>,
}

#[derive(Debug, Default, Clone)]
pub struct ModuleInfo {
    /// Module specifiers in source order, without duplicates.
    pub requests: Vec<String>,
    pub imports: Vec<ImportEntry>,
    /// (export name, local binding name)
    pub local_exports: Vec<(JsString, Rc<str>)>,
    /// (export name, module request, import name; `None` = the namespace, `export * as ns`)
    pub indirect_exports: Vec<(JsString, String, Option<JsString>)>,
    pub star_exports: Vec<String>,
    pub has_tla: bool,
}

/// A function's static part, shared by all closures made from it; compiled on first call.
pub struct FuncTemplate {
    pub ast: Rc<ast::Function>,
    pub source: Rc<[char]>,
    pub code: OnceCell<Rc<Code>>,
    /// `length` (§15.1.5 ExpectedArgumentCount).
    pub length: u32,
    /// A named function expression binds its own name.
    pub is_expression: bool,
    /// The name from the function's position (NamedEvaluation) when it has none itself.
    pub given_name: Option<Rc<str>>,
    /// The script or module the function was defined in.
    pub referrer: Option<Rc<str>>,
    /// A class's implicit constructor (§15.7.14 step 14.a).
    pub default_ctor: bool,
    /// Source text for `toString` when it isn't the function's own (class constructors).
    pub text_span: Option<(usize, usize)>,
}

impl FuncTemplate {
    pub fn source_text(&self) -> String {
        let (a, b) = self.text_span.unwrap_or(self.ast.span);
        self.source.get(a..b.min(self.source.len())).map(|s| s.iter().collect()).unwrap_or_default()
    }
}

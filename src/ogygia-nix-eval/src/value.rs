//! Runtime values.
//!
//! Every value lives in the evaluation session's arena, so a [`Value`] is a
//! small `Copy` handle and nothing is freed until the session ends.

use std::cell::Cell;
use std::fmt;

use crate::eval::Eval;
use crate::ir::ExprRef;
use crate::ir::LambdaDef;
use crate::ir::Pos;
use crate::symbol::Sym;

#[derive(Clone, Copy)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a NixStr<'a>),
    Path(&'a PathV<'a>),
    Attrs(&'a Attrs<'a>),
    List(&'a List<'a>),
    Lambda(&'a Closure<'a>),
    PrimOp(&'static PrimOp),
    PrimOpApp(&'a PrimOpApp<'a>),
    Thunk(&'a Thunk<'a>),
}

/// An element of a string's context: a store path the string refers to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Ctx<'a> {
    /// A plain store path.
    Opaque(&'a str),
    /// A derivation and its whole closure (`=` prefix).
    DrvDeep(&'a str),
    /// An output of a derivation (`!out!` prefix).
    Built { drv: &'a str, output: &'a str },
}

pub struct NixStr<'a> {
    pub s: &'a [u8],
    /// Sorted and deduplicated.
    pub ctx: &'a [Ctx<'a>],
}

impl NixStr<'_> {
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.s)
    }
}

pub struct PathV<'a>(pub &'a str);

#[derive(Clone, Copy)]
pub struct Entry<'a> {
    pub name: Sym,
    pub value: Value<'a>,
    pub pos: Option<Pos<'a>>,
}

/// An attribute set; `entries` is sorted by symbol.
pub struct Attrs<'a> {
    pub entries: &'a [Entry<'a>],
}

impl<'a> Attrs<'a> {
    pub fn get(&self, name: Sym) -> Option<Value<'a>> {
        self.entry(name).map(|e| e.value)
    }

    pub fn entry(&self, name: Sym) -> Option<&Entry<'a>> {
        self.entries
            .binary_search_by_key(&name, |e| e.name)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in lexicographic name order, the order Nix exposes.
    pub fn sorted(&self, ctx: &crate::Context) -> Vec<Entry<'a>> {
        let mut v = self.entries.to_vec();
        v.sort_by(|a, b| ctx.name(a.name).cmp(ctx.name(b.name)));
        v
    }
}

pub struct List<'a> {
    pub items: &'a [Value<'a>],
}

pub struct Closure<'a> {
    pub def: &'a LambdaDef<'a>,
    pub env: &'a Env<'a>,
}

pub type PrimOpFn = for<'a> fn(&Eval<'a>, &[Value<'a>]) -> R<'a>;

pub struct PrimOp {
    pub name: &'static str,
    pub arity: usize,
    pub f: PrimOpFn,
}

pub struct PrimOpApp<'a> {
    pub op: &'static PrimOp,
    pub args: &'a [Value<'a>],
}

pub type NativeFn<'a> = &'a dyn Fn(&Eval<'a>) -> R<'a>;

#[derive(Clone, Copy)]
pub enum ThunkState<'a> {
    Expr(ExprRef<'a>, &'a Env<'a>),
    /// A lazy function application.
    App(Value<'a>, Value<'a>),
    /// A lazy attribute selection (`inherit (from) name`).
    Select(Value<'a>, Sym, Pos<'a>),
    Native(NativeFn<'a>),
    Blackhole,
    Done(Value<'a>),
}

pub struct Thunk<'a>(pub Cell<ThunkState<'a>>);

pub struct Env<'a> {
    pub parent: Option<&'a Env<'a>>,
    pub slots: &'a [Cell<Value<'a>>],
}

impl<'a> Env<'a> {
    pub fn up(&'a self, depth: u32) -> &'a Env<'a> {
        let mut env = self;
        for _ in 0..depth {
            env = env
                .parent
                .expect("variable depth exceeds environment chain");
        }
        env
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `throw`; caught by `tryEval`.
    Throw,
    /// A failed `assert`; caught by `tryEval`.
    Assert,
    /// `abort`.
    Abort,
    InfiniteRecursion,
    /// Any other evaluation error.
    Eval,
}

#[derive(Debug)]
pub struct EvalError {
    pub kind: ErrorKind,
    pub msg: String,
    pub trace: Vec<String>,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The outermost frames are rarely useful; keep the innermost.
        for t in self.trace.iter().take(30).rev() {
            writeln!(f, "… {t}")?;
        }
        write!(f, "error: {}", self.msg)
    }
}

impl std::error::Error for EvalError {}

pub type R<'a, T = Value<'a>> = Result<T, Box<EvalError>>;

pub fn error(kind: ErrorKind, msg: impl Into<String>) -> Box<EvalError> {
    Box::new(EvalError {
        kind,
        msg: msg.into(),
        trace: Vec::new(),
    })
}

pub fn eval_err<T>(msg: impl Into<String>) -> Result<T, Box<EvalError>> {
    Err(error(ErrorKind::Eval, msg))
}

impl<'a> Value<'a> {
    /// The type name as returned by `builtins.typeOf`.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Path(_) => "path",
            Value::Attrs(_) => "set",
            Value::List(_) => "list",
            Value::Lambda(_) | Value::PrimOp(_) | Value::PrimOpApp(_) => "lambda",
            Value::Thunk(_) => "thunk",
        }
    }

    /// The type described with an article, as in Nix's error messages.
    pub fn show_type(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a Boolean",
            Value::Int(_) => "an integer",
            Value::Float(_) => "a float",
            Value::Str(_) => "a string",
            Value::Path(_) => "a path",
            Value::Attrs(_) => "a set",
            Value::List(_) => "a list",
            Value::Lambda(_) => "a function",
            Value::PrimOp(_) => "a built-in function",
            Value::PrimOpApp(_) => "a partially applied built-in function",
            Value::Thunk(_) => "a thunk",
        }
    }

    pub fn is_function(&self) -> bool {
        matches!(
            self,
            Value::Lambda(_) | Value::PrimOp(_) | Value::PrimOpApp(_)
        )
    }

    /// Identity of the heap object behind this value, if any.
    pub fn ptr(&self) -> Option<*const ()> {
        Some(match self {
            Value::Str(s) => *s as *const _ as *const (),
            Value::Path(p) => *p as *const _ as *const (),
            Value::Attrs(a) => *a as *const _ as *const (),
            Value::List(l) => *l as *const _ as *const (),
            Value::Lambda(c) => *c as *const _ as *const (),
            Value::PrimOp(p) => *p as *const _ as *const (),
            Value::PrimOpApp(p) => *p as *const _ as *const (),
            Value::Thunk(t) => *t as *const _ as *const (),
            _ => return None,
        })
    }
}

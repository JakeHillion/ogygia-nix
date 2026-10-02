//! The evaluator: forcing thunks, evaluating expressions, calling functions.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;

use bumpalo::Bump;

use crate::builtins;
use crate::context::Context;
use crate::ir::AttrKey;
use crate::ir::AttrValue;
use crate::ir::AttrsDef;
use crate::ir::BinOp;
use crate::ir::Expr;
use crate::ir::ExprRef;
use crate::ir::Param;
use crate::ir::Pos;
use crate::ir::SlotInit;
use crate::path::canon_path;
use crate::symbol::Sym;
use crate::value::Attrs;
use crate::value::Closure;
use crate::value::Ctx;
use crate::value::Entry;
use crate::value::Env;
use crate::value::ErrorKind;
use crate::value::List;
use crate::value::NixStr;
use crate::value::PathV;
use crate::value::PrimOpApp;
use crate::value::R;
use crate::value::Thunk;
use crate::value::ThunkState;
use crate::value::Value;
use crate::value::error;
use crate::value::eval_err;

/// Nix's default `max-call-depth`.
const MAX_CALL_DEPTH: usize = 10_000;

/// Evaluation settings that are fixed for a session.
#[derive(Clone, Debug)]
pub struct Settings {
    /// `builtins.currentSystem`; `None` in pure evaluation.
    pub current_system: Option<String>,
    /// Search path entries for `<...>` lookups, as `(prefix, path)`.
    pub nix_path: Vec<(String, String)>,
    pub pure: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            current_system: Some(crate::CURRENT_SYSTEM.to_owned()),
            nix_path: Vec::new(),
            pure: false,
        }
    }
}

/// One evaluation session. Values are allocated in `bump` and live until the
/// session's arena is dropped.
pub struct Eval<'a> {
    pub ctx: &'a Context,
    pub bump: &'a Bump,
    pub settings: Settings,
    globals: RefCell<HashMap<Sym, Value<'a>>>,
    import_cache: RefCell<HashMap<String, Value<'a>>>,
    depth: Cell<usize>,
    /// Derivations created in this session, with their modulo hashes, by
    /// `.drv` path.
    pub(crate) drvs: RefCell<HashMap<String, (crate::derivation::Derivation, [u8; 32])>>,
    /// Flakes opened in this session.
    pub(crate) flakes: RefCell<Vec<crate::flake::LockedFlake>>,
    /// Flake values by (flake, lock node).
    pub(crate) flake_cache: RefCell<HashMap<(usize, String), Value<'a>>>,
    pub(crate) empty_attrs: &'a Attrs<'a>,
    pub(crate) empty_list: &'a List<'a>,
}

/// Whether string coercion copies paths to the store, and whether it accepts
/// the extra types `toString` does.
#[derive(Clone, Copy)]
pub struct Coerce {
    pub more: bool,
    pub copy: bool,
}

impl Coerce {
    pub const INTERP: Coerce = Coerce {
        more: false,
        copy: true,
    };
    pub const PLAIN: Coerce = Coerce {
        more: false,
        copy: false,
    };
}

impl<'a> Eval<'a> {
    pub fn new(ctx: &'a Context, bump: &'a Bump, settings: Settings) -> Eval<'a> {
        let ev = Eval {
            ctx,
            bump,
            settings,
            globals: RefCell::new(HashMap::new()),
            import_cache: RefCell::new(HashMap::new()),
            depth: Cell::new(0),
            drvs: RefCell::new(HashMap::new()),
            flakes: RefCell::new(Vec::new()),
            flake_cache: RefCell::new(HashMap::new()),
            empty_attrs: bump.alloc(Attrs { entries: &[] }),
            empty_list: bump.alloc(List { items: &[] }),
        };
        let globals = builtins::make_globals(&ev);
        *ev.globals.borrow_mut() = globals;
        ev
    }

    pub fn sym(&self, name: &str) -> Sym {
        self.ctx.intern(name)
    }

    pub fn name(&self, sym: Sym) -> &'a str {
        self.ctx.name(sym)
    }

    // ---- allocation -------------------------------------------------------

    pub fn alloc<T>(&self, t: T) -> &'a T {
        self.bump.alloc(t)
    }

    pub fn str_val(&self, s: &[u8], ctx: &[Ctx<'a>]) -> Value<'a> {
        let s = self.bump.alloc_slice_copy(s);
        let ctx: &'a [Ctx<'a>] = if ctx.is_empty() {
            &[]
        } else {
            self.bump.alloc_slice_copy(ctx)
        };
        Value::Str(self.bump.alloc(NixStr { s, ctx }))
    }

    pub fn string(&self, s: &str) -> Value<'a> {
        self.str_val(s.as_bytes(), &[])
    }

    pub fn path_val(&self, p: &str) -> Value<'a> {
        Value::Path(self.bump.alloc(PathV(self.bump.alloc_str(p))))
    }

    pub fn list(&self, items: &[Value<'a>]) -> Value<'a> {
        if items.is_empty() {
            return Value::List(self.empty_list);
        }
        Value::List(self.bump.alloc(List {
            items: self.bump.alloc_slice_copy(items),
        }))
    }

    /// An attribute set from entries in any order; later duplicates win.
    pub fn attrs(&self, mut entries: Vec<Entry<'a>>) -> Value<'a> {
        if entries.is_empty() {
            return Value::Attrs(self.empty_attrs);
        }
        // Stable sort keeps duplicates in insertion order; keep the last.
        entries.sort_by_key(|e| e.name);
        let mut out: Vec<Entry<'a>> = Vec::with_capacity(entries.len());
        for e in entries {
            match out.last_mut() {
                Some(last) if last.name == e.name => *last = e,
                _ => out.push(e),
            }
        }
        self.attrs_sorted(&out)
    }

    /// An attribute set from entries already sorted by symbol, without
    /// duplicates.
    pub fn attrs_sorted(&self, entries: &[Entry<'a>]) -> Value<'a> {
        if entries.is_empty() {
            return Value::Attrs(self.empty_attrs);
        }
        Value::Attrs(self.bump.alloc(Attrs {
            entries: self.bump.alloc_slice_copy(entries),
        }))
    }

    pub fn entry(&self, name: &str, value: Value<'a>) -> Entry<'a> {
        Entry {
            name: self.sym(name),
            value,
            pos: None,
        }
    }

    pub fn thunk(&self, state: ThunkState<'a>) -> Value<'a> {
        Value::Thunk(self.bump.alloc(Thunk(Cell::new(state))))
    }

    /// A lazy application `f x`.
    pub fn lazy_app(&self, f: Value<'a>, x: Value<'a>) -> Value<'a> {
        self.thunk(ThunkState::App(f, x))
    }

    /// A lazy native computation. The closure lives in the arena, which never
    /// runs destructors, so it may only capture `Copy` data.
    pub fn lazy_native(&self, f: impl Fn(&Eval<'a>) -> R<'a> + Copy + 'a) -> Value<'a> {
        let f: &'a dyn Fn(&Eval<'a>) -> R<'a> = self.bump.alloc(f);
        self.thunk(ThunkState::Native(f))
    }

    pub fn global(&self, name: &str) -> Option<Value<'a>> {
        self.globals.borrow().get(&self.sym(name)).copied()
    }

    // ---- forcing ----------------------------------------------------------

    pub fn force(&self, v: Value<'a>) -> R<'a> {
        match v {
            Value::Thunk(t) => self.force_thunk(t),
            v => Ok(v),
        }
    }

    fn force_thunk(&self, t: &'a Thunk<'a>) -> R<'a> {
        let state = t.0.get();
        let result = match state {
            ThunkState::Done(v) => return Ok(v),
            ThunkState::Blackhole => {
                return Err(error(
                    ErrorKind::InfiniteRecursion,
                    "infinite recursion encountered",
                ));
            }
            ThunkState::Expr(e, env) => {
                t.0.set(ThunkState::Blackhole);
                self.eval(e, env)
            }
            ThunkState::App(f, x) => {
                t.0.set(ThunkState::Blackhole);
                self.call(f, x)
            }
            ThunkState::Select(v, name, pos) => {
                t.0.set(ThunkState::Blackhole);
                self.select_one(v, name, pos)
            }
            ThunkState::Native(f) => {
                t.0.set(ThunkState::Blackhole);
                f(self)
            }
        };
        match result {
            Ok(v) => {
                t.0.set(ThunkState::Done(v));
                Ok(v)
            }
            Err(e) => {
                t.0.set(state);
                Err(e)
            }
        }
    }

    fn select_one(&self, v: Value<'a>, name: Sym, pos: Pos<'a>) -> R<'a> {
        let attrs = self.force_attrs(v)?;
        match attrs.get(name) {
            Some(x) => self.force(x),
            None => eval_err(format!("attribute '{}' missing at {pos}", self.name(name))),
        }
    }

    /// Force `v` and everything reachable from it.
    pub fn deep_force(&self, v: Value<'a>) -> R<'a> {
        let mut seen = HashSet::new();
        self.deep_force_inner(v, &mut seen)
    }

    fn deep_force_inner(&self, v: Value<'a>, seen: &mut HashSet<*const ()>) -> R<'a> {
        let v = self.force(v)?;
        match v {
            Value::Attrs(a) => {
                if seen.insert(a as *const _ as *const ()) {
                    for e in a.entries {
                        self.deep_force_inner(e.value, seen).map_err(|mut err| {
                            err.trace.push(format!(
                                "while evaluating the attribute '{}'",
                                self.name(e.name)
                            ));
                            err
                        })?;
                    }
                }
            }
            Value::List(l) if seen.insert(l as *const _ as *const ()) => {
                for item in l.items {
                    self.deep_force_inner(*item, seen)?;
                }
            }
            _ => {}
        }
        Ok(v)
    }

    pub fn type_error<T>(&self, expected: &str, got: Value<'a>) -> R<'a, T> {
        eval_err(format!(
            "expected {expected} but found {}",
            self.describe(got)
        ))
    }

    pub fn force_attrs(&self, v: Value<'a>) -> R<'a, &'a Attrs<'a>> {
        match self.force(v)? {
            Value::Attrs(a) => Ok(a),
            other => self.type_error("a set", other),
        }
    }

    pub fn force_list(&self, v: Value<'a>) -> R<'a, &'a [Value<'a>]> {
        match self.force(v)? {
            Value::List(l) => Ok(l.items),
            other => self.type_error("a list", other),
        }
    }

    pub fn force_int(&self, v: Value<'a>) -> R<'a, i64> {
        match self.force(v)? {
            Value::Int(i) => Ok(i),
            other => self.type_error("an integer", other),
        }
    }

    pub fn force_bool(&self, v: Value<'a>) -> R<'a, bool> {
        match self.force(v)? {
            Value::Bool(b) => Ok(b),
            other => self.type_error("a Boolean", other),
        }
    }

    pub fn force_function(&self, v: Value<'a>) -> R<'a> {
        let v = self.force(v)?;
        if v.is_function() {
            Ok(v)
        } else {
            self.type_error("a function", v)
        }
    }

    /// A string value without coercion.
    pub fn force_str(&self, v: Value<'a>) -> R<'a, &'a NixStr<'a>> {
        match self.force(v)? {
            Value::Str(s) => Ok(s),
            other => self.type_error("a string", other),
        }
    }

    /// A string without context, without coercion.
    pub fn force_str_no_ctx(&self, v: Value<'a>) -> R<'a, &'a [u8]> {
        let s = self.force_str(v)?;
        if !s.ctx.is_empty() {
            return eval_err(format!(
                "the string '{}' is not allowed to refer to a store path",
                s.as_str_lossy()
            ));
        }
        Ok(s.s)
    }

    pub fn describe(&self, v: Value<'a>) -> String {
        format!("{}: {}", v.show_type(), crate::print::short(self, v))
    }

    // ---- evaluation -------------------------------------------------------

    /// A value for `e` in `env` without evaluating it, if that is cheap.
    pub fn lazy(&self, e: ExprRef<'a>, env: &'a Env<'a>) -> Value<'a> {
        match e {
            Expr::Null => Value::Null,
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Int(i) => Value::Int(*i),
            Expr::Float(f) => Value::Float(*f),
            Expr::Str(s) => Value::Str(s),
            Expr::Path(p) => Value::Path(p),
            Expr::Var(d, i) => env.up(*d).slots[*i as usize].get(),
            Expr::Lambda(def) => Value::Lambda(self.bump.alloc(Closure { def, env })),
            Expr::Global(name) => match self.globals.borrow().get(name) {
                Some(v) => *v,
                None => self.thunk(ThunkState::Expr(e, env)),
            },
            _ => self.thunk(ThunkState::Expr(e, env)),
        }
    }

    pub fn eval(&self, e: ExprRef<'a>, env: &'a Env<'a>) -> R<'a> {
        match e {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Int(i) => Ok(Value::Int(*i)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Str(s) => Ok(Value::Str(s)),
            Expr::Path(p) => Ok(Value::Path(p)),
            Expr::Var(d, i) => self.force(env.up(*d).slots[*i as usize].get()),
            Expr::Global(name) => {
                let v = self.globals.borrow().get(name).copied();
                match v {
                    Some(v) => self.force(v),
                    None => eval_err(format!("undefined variable '{}'", self.name(*name))),
                }
            }
            Expr::WithVar {
                name,
                withs,
                global,
                pos,
            } => {
                for &depth in withs.iter() {
                    let ns = env.up(depth).slots[0].get();
                    let attrs = self.force_attrs(ns)?;
                    if let Some(v) = attrs.get(*name) {
                        return self.force(v);
                    }
                }
                if *global {
                    let v = self.globals.borrow()[name];
                    return self.force(v);
                }
                eval_err(format!(
                    "undefined variable '{}' at {pos}",
                    self.name(*name)
                ))
            }
            Expr::Lambda(def) => Ok(Value::Lambda(self.bump.alloc(Closure { def, env }))),
            Expr::List(items) => {
                if items.is_empty() {
                    return Ok(Value::List(self.empty_list));
                }
                let items = self
                    .bump
                    .alloc_slice_fill_iter(items.iter().map(|i| self.lazy(i, env)));
                Ok(Value::List(self.bump.alloc(List { items })))
            }
            Expr::Attrs(def) => self.eval_attrs(def, env),
            Expr::Select {
                expr,
                path,
                default,
                pos,
            } => {
                let mut v = self.eval(expr, env)?;
                for key in path.iter() {
                    let name = self.attr_key(key, env)?;
                    let found = match v {
                        Value::Attrs(a) => a.get(name),
                        _ => None,
                    };
                    match found {
                        Some(x) => v = self.force(x)?,
                        None => {
                            if let Some(d) = default {
                                return self.eval(d, env);
                            }
                            return match v {
                                Value::Attrs(_) => eval_err(format!(
                                    "attribute '{}' missing at {pos}",
                                    self.name(name)
                                )),
                                other => eval_err(format!(
                                    "expected a set but found {} while selecting attribute '{}' at {pos}",
                                    self.describe(other),
                                    self.name(name)
                                )),
                            };
                        }
                    }
                }
                Ok(v)
            }
            Expr::HasAttr { expr, path } => {
                // Only the sets along the path are forced, never the value of
                // the last attribute.
                let mut v = self.lazy(expr, env);
                for key in path.iter() {
                    let name = self.attr_key(key, env)?;
                    match self.force(v)? {
                        Value::Attrs(a) => match a.get(name) {
                            Some(x) => v = x,
                            None => return Ok(Value::Bool(false)),
                        },
                        _ => return Ok(Value::Bool(false)),
                    }
                }
                Ok(Value::Bool(true))
            }
            Expr::Apply { func, args, pos } => {
                let mut f = self.eval(func, env)?;
                for arg in args.iter() {
                    let a = self.lazy(arg, env);
                    f = self.call_at(f, a, *pos)?;
                }
                Ok(f)
            }
            Expr::Let(def) => {
                let env2 = self.new_frame(def.slots, env);
                self.eval(def.body, env2)
            }
            Expr::With { namespace, body } => {
                let ns = self.lazy(namespace, env);
                let slots = self.bump.alloc_slice_fill_with(1, |_| Cell::new(ns));
                let env2 = self.bump.alloc(Env {
                    parent: Some(env),
                    slots,
                });
                self.eval(body, env2)
            }
            Expr::If(c, t, f) => match self.eval(c, env)? {
                Value::Bool(true) => self.eval(t, env),
                Value::Bool(false) => self.eval(f, env),
                other => self.type_error("a Boolean", other),
            },
            Expr::Assert { cond, body, pos } => match self.eval(cond, env)? {
                Value::Bool(true) => self.eval(body, env),
                Value::Bool(false) => Err(error(
                    ErrorKind::Assert,
                    format!("assertion failed at {pos}"),
                )),
                other => self.type_error("a Boolean", other),
            },
            Expr::Not(e, _) => match self.eval(e, env)? {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => self.type_error("a Boolean", other),
            },
            Expr::Neg(e, pos) => {
                let v = self.eval(e, env)?;
                self.arith(BinOp::Sub, Value::Int(0), v, *pos)
            }
            Expr::Bin(op, l, r, pos) => self.eval_bin(*op, l, r, env, *pos),
            Expr::InterpStr(parts, _) => {
                let mut buf = Vec::new();
                let mut ctx = Vec::new();
                for p in parts.iter() {
                    let v = self.eval(p, env)?;
                    self.coerce_into(v, Coerce::INTERP, &mut buf, &mut ctx)?;
                }
                Ok(self.str_with_ctx(&buf, ctx))
            }
            Expr::InterpPath(parts, _) => {
                let mut buf = Vec::new();
                let mut ctx = Vec::new();
                for (i, p) in parts.iter().enumerate() {
                    let v = self.eval(p, env)?;
                    match (i, v) {
                        (0, Value::Path(p)) => buf.extend_from_slice(p.0.as_bytes()),
                        _ => self.coerce_into(v, Coerce::PLAIN, &mut buf, &mut ctx)?,
                    }
                }
                if !ctx.is_empty() {
                    return eval_err(
                        "a string that refers to a store path cannot be appended to a path",
                    );
                }
                Ok(self.path_val(&canon_path(&String::from_utf8_lossy(&buf))))
            }
            Expr::CurPos(pos) => Ok(self.pos_value(*pos)),
        }
    }

    /// `{ file, line, column }` for `pos`, or null for positions not in a file.
    pub fn pos_value(&self, pos: Pos<'a>) -> Value<'a> {
        let Some(path) = pos.source.path else {
            return Value::Null;
        };
        let (line, col) = pos.line_col();
        let s = &self.ctx.syms;
        self.attrs(vec![
            Entry {
                name: s.file,
                value: self.string(path),
                pos: None,
            },
            Entry {
                name: s.line,
                value: Value::Int(line as i64),
                pos: None,
            },
            Entry {
                name: s.column,
                value: Value::Int(col as i64),
                pos: None,
            },
        ])
    }

    pub fn str_with_ctx(&self, buf: &[u8], mut ctx: Vec<Ctx<'a>>) -> Value<'a> {
        ctx.sort();
        ctx.dedup();
        self.str_val(buf, &ctx)
    }

    fn attr_key(&self, key: &'a AttrKey<'a>, env: &'a Env<'a>) -> R<'a, Sym> {
        match key {
            AttrKey::Static(s) => Ok(*s),
            AttrKey::Dynamic(e) => {
                let v = self.eval(e, env)?;
                let s = self.force_str(v)?;
                Ok(self.ctx.interner.intern_bytes(s.s))
            }
        }
    }

    pub(crate) fn new_frame(&self, inits: &'a [SlotInit<'a>], parent: &'a Env<'a>) -> &'a Env<'a> {
        let slots = self
            .bump
            .alloc_slice_fill_with(inits.len(), |_| Cell::new(Value::Null));
        let env: &'a Env<'a> = self.bump.alloc(Env {
            parent: Some(parent),
            slots,
        });
        for (slot, init) in slots.iter().zip(inits) {
            match init {
                // A slot aliasing another slot of this frame may refer to one
                // that is not filled in yet.
                SlotInit::Expr(e @ Expr::Var(0, _)) => {
                    slot.set(self.thunk(ThunkState::Expr(e, env)))
                }
                SlotInit::Expr(e) => slot.set(self.lazy(e, env)),
                SlotInit::Select(..) => {}
            }
        }
        // Inherit-from sources are slots of their own, so fill those first.
        for (slot, init) in slots.iter().zip(inits) {
            if let SlotInit::Select(from, name, pos) = init {
                let src = slots[*from as usize].get();
                slot.set(self.thunk(ThunkState::Select(src, *name, *pos)));
            }
        }
        env
    }

    fn eval_attrs(&self, def: &'a AttrsDef<'a>, env: &'a Env<'a>) -> R<'a> {
        let env2 = match def.frame {
            Some(inits) => self.new_frame(inits, env),
            None => env,
        };
        let mut entries: Vec<Entry<'a>> = def
            .statics
            .iter()
            .map(|a| Entry {
                name: a.name,
                pos: Some(a.pos),
                value: match &a.value {
                    AttrValue::Expr(e) => self.lazy(e, env2),
                    AttrValue::Slot(i) => env2.slots[*i as usize].get(),
                },
            })
            .collect();
        if def.dynamics.is_empty() {
            return Ok(self.attrs_sorted(&entries));
        }
        for d in def.dynamics {
            let name = match self.eval(d.name, env2)? {
                Value::Null => continue,
                Value::Str(s) => self.ctx.interner.intern_bytes(s.s),
                other => return self.type_error("a string", other),
            };
            if entries.iter().any(|e| e.name == name) {
                return eval_err(format!(
                    "dynamic attribute '{}' already defined at {}",
                    self.name(name),
                    d.pos
                ));
            }
            entries.push(Entry {
                name,
                value: self.lazy(d.value, env2),
                pos: Some(d.pos),
            });
        }
        entries.sort_by_key(|e| e.name);
        Ok(self.attrs_sorted(&entries))
    }

    // ---- function calls ---------------------------------------------------

    pub fn call(&self, f: Value<'a>, arg: Value<'a>) -> R<'a> {
        self.call_inner(f, arg, None)
    }

    pub fn call_at(&self, f: Value<'a>, arg: Value<'a>, pos: Pos<'a>) -> R<'a> {
        self.call_inner(f, arg, Some(pos))
    }

    /// Apply `f` to several arguments in turn.
    pub fn call_n(&self, f: Value<'a>, args: &[Value<'a>]) -> R<'a> {
        let mut f = f;
        for a in args {
            f = self.call(f, *a)?;
        }
        Ok(f)
    }

    fn call_inner(&self, f: Value<'a>, arg: Value<'a>, pos: Option<Pos<'a>>) -> R<'a> {
        let f = self.force(f)?;
        let depth = self.depth.get();
        if depth >= MAX_CALL_DEPTH {
            return eval_err("stack overflow; max-call-depth exceeded");
        }
        self.depth.set(depth + 1);
        let r = self.call_forced(f, arg, pos);
        self.depth.set(depth);
        r
    }

    fn call_forced(&self, f: Value<'a>, arg: Value<'a>, pos: Option<Pos<'a>>) -> R<'a> {
        match f {
            Value::Lambda(c) => self.call_lambda(c, arg),
            Value::PrimOp(op) => {
                if op.arity == 1 {
                    let r = (op.f)(self, &[arg])?;
                    self.force(r)
                } else {
                    Ok(Value::PrimOpApp(self.bump.alloc(PrimOpApp {
                        op,
                        args: self.bump.alloc_slice_copy(&[arg]),
                    })))
                }
            }
            Value::PrimOpApp(app) => {
                let mut args: Vec<Value<'a>> = Vec::with_capacity(app.args.len() + 1);
                args.extend_from_slice(app.args);
                args.push(arg);
                if args.len() == app.op.arity {
                    let r = (app.op.f)(self, &args)?;
                    self.force(r)
                } else {
                    Ok(Value::PrimOpApp(self.bump.alloc(PrimOpApp {
                        op: app.op,
                        args: self.bump.alloc_slice_copy(&args),
                    })))
                }
            }
            Value::Attrs(a) if a.get(self.ctx.syms.functor).is_some() => {
                let functor = a.get(self.ctx.syms.functor).unwrap();
                let g = self.call(functor, f)?;
                self.call_inner(g, arg, pos)
            }
            other => eval_err(format!(
                "attempt to call something which is not a function but {}{}",
                self.describe(other),
                pos.map(|p| format!(" at {p}")).unwrap_or_default()
            )),
        }
    }

    fn call_lambda(&self, c: &'a Closure<'a>, arg: Value<'a>) -> R<'a> {
        let def = c.def;
        let slots = self
            .bump
            .alloc_slice_fill_with(def.nslots(), |_| Cell::new(Value::Null));
        let env: &'a Env<'a> = self.bump.alloc(Env {
            parent: Some(c.env),
            slots,
        });
        match &def.param {
            Param::Ident(_) => slots[0].set(arg),
            Param::Pattern {
                formals,
                ellipsis,
                at,
            } => {
                let attrs = match self.force(arg)? {
                    Value::Attrs(a) => a,
                    other => {
                        return eval_err(format!(
                            "expected a set but found {} while evaluating the value passed for the lambda argument at {}",
                            self.describe(other),
                            def.pos
                        ));
                    }
                };
                let mut used = 0;
                for (slot, formal) in slots.iter().zip(formals.iter()) {
                    match attrs.get(formal.name) {
                        Some(v) => {
                            used += 1;
                            slot.set(v);
                        }
                        None => match formal.default {
                            // A default naming another formal (or the `@` binding)
                            // may refer to a slot that is not filled in yet.
                            Some(d @ Expr::Var(0, _)) => {
                                slot.set(self.thunk(ThunkState::Expr(d, env)))
                            }
                            Some(d) => slot.set(self.lazy(d, env)),
                            None => {
                                return eval_err(format!(
                                    "function '{}' called without required argument '{}' at {}",
                                    self.lambda_name(def.name),
                                    self.name(formal.name),
                                    def.pos
                                ));
                            }
                        },
                    }
                }
                if !ellipsis && used != attrs.len() {
                    let bad = attrs
                        .sorted(self.ctx)
                        .into_iter()
                        .find(|e| !formals.iter().any(|f| f.name == e.name));
                    if let Some(bad) = bad {
                        return eval_err(format!(
                            "function '{}' called with unexpected argument '{}' at {}",
                            self.lambda_name(def.name),
                            self.name(bad.name),
                            def.pos
                        ));
                    }
                }
                if at.is_some() {
                    slots[formals.len()].set(arg);
                }
            }
        }
        self.eval(def.body, env).map_err(|mut e| {
            e.trace.push(format!(
                "while calling '{}' defined at {}",
                self.lambda_name(def.name),
                def.pos
            ));
            e
        })
    }

    fn lambda_name(&self, name: Option<Sym>) -> &'a str {
        match name {
            Some(n) => self.name(n),
            None => "anonymous lambda",
        }
    }

    // ---- operators --------------------------------------------------------

    fn eval_bin(
        &self,
        op: BinOp,
        l: ExprRef<'a>,
        r: ExprRef<'a>,
        env: &'a Env<'a>,
        pos: Pos<'a>,
    ) -> R<'a> {
        let operands =
            || -> R<'a, (Value<'a>, Value<'a>)> { Ok((self.eval(l, env)?, self.eval(r, env)?)) };
        match op {
            BinOp::And => Ok(Value::Bool(
                self.bool_operand(l, env)? && self.bool_operand(r, env)?,
            )),
            BinOp::Or => Ok(Value::Bool(
                self.bool_operand(l, env)? || self.bool_operand(r, env)?,
            )),
            BinOp::Impl => Ok(Value::Bool(
                !self.bool_operand(l, env)? || self.bool_operand(r, env)?,
            )),
            BinOp::Eq => {
                let (a, b) = operands()?;
                Ok(Value::Bool(self.eq(a, b)?))
            }
            BinOp::Neq => {
                let (a, b) = operands()?;
                Ok(Value::Bool(!self.eq(a, b)?))
            }
            BinOp::Lt => {
                let (a, b) = operands()?;
                Ok(Value::Bool(self.less_than(a, b)?))
            }
            BinOp::Gt => {
                let (a, b) = operands()?;
                Ok(Value::Bool(self.less_than(b, a)?))
            }
            BinOp::Le => {
                let (a, b) = operands()?;
                Ok(Value::Bool(!self.less_than(b, a)?))
            }
            BinOp::Ge => {
                let (a, b) = operands()?;
                Ok(Value::Bool(!self.less_than(a, b)?))
            }
            BinOp::Concat => {
                let (a, b) = operands()?;
                let a = self.force_list(a)?;
                let b = self.force_list(b)?;
                if a.is_empty() {
                    return Ok(self.list(b));
                }
                if b.is_empty() {
                    return Ok(self.list(a));
                }
                let mut v = Vec::with_capacity(a.len() + b.len());
                v.extend_from_slice(a);
                v.extend_from_slice(b);
                Ok(self.list(&v))
            }
            BinOp::Update => {
                let (a, b) = operands()?;
                let a = self.force_attrs(a)?;
                let b = self.force_attrs(b)?;
                Ok(self.update(a, b))
            }
            BinOp::Add => {
                let (a, b) = operands()?;
                self.add(a, b, pos)
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let (a, b) = operands()?;
                self.arith(op, a, b, pos)
            }
        }
    }

    fn bool_operand(&self, e: ExprRef<'a>, env: &'a Env<'a>) -> R<'a, bool> {
        match self.eval(e, env)? {
            Value::Bool(b) => Ok(b),
            other => self.type_error("a Boolean", other),
        }
    }

    /// `a // b`.
    pub fn update(&self, a: &'a Attrs<'a>, b: &'a Attrs<'a>) -> Value<'a> {
        if a.is_empty() {
            return Value::Attrs(b);
        }
        if b.is_empty() {
            return Value::Attrs(a);
        }
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        let (ae, be) = (a.entries, b.entries);
        while i < ae.len() && j < be.len() {
            match ae[i].name.cmp(&be[j].name) {
                std::cmp::Ordering::Less => {
                    out.push(ae[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(be[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    out.push(be[j]);
                    i += 1;
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&ae[i..]);
        out.extend_from_slice(&be[j..]);
        self.attrs_sorted(&out)
    }

    pub fn add(&self, a: Value<'a>, b: Value<'a>, pos: Pos<'a>) -> R<'a> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        match a {
            Value::Int(_) | Value::Float(_) => self.arith(BinOp::Add, a, b, pos),
            Value::Path(p) => {
                let mut buf = p.0.as_bytes().to_vec();
                let mut ctx = Vec::new();
                self.coerce_into(b, Coerce::PLAIN, &mut buf, &mut ctx)?;
                if !ctx.is_empty() {
                    return eval_err(
                        "a string that refers to a store path cannot be appended to a path",
                    );
                }
                Ok(self.path_val(&canon_path(&String::from_utf8_lossy(&buf))))
            }
            _ => {
                let mut buf = Vec::new();
                let mut ctx = Vec::new();
                self.coerce_into(a, Coerce::INTERP, &mut buf, &mut ctx)?;
                self.coerce_into(b, Coerce::INTERP, &mut buf, &mut ctx)?;
                Ok(self.str_with_ctx(&buf, ctx))
            }
        }
    }

    /// Arithmetic from a builtin, where there is no source position.
    pub fn arith_nopos(&self, op: BinOp, a: Value<'a>, b: Value<'a>) -> R<'a> {
        self.arith_inner(op, a, b, String::new())
    }

    pub fn arith(&self, op: BinOp, a: Value<'a>, b: Value<'a>, pos: Pos<'a>) -> R<'a> {
        self.arith_inner(op, a, b, format!(" at {pos}"))
    }

    fn arith_inner(&self, op: BinOp, a: Value<'a>, b: Value<'a>, at: String) -> R<'a> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => {
                let (r, verb, sym) = match op {
                    BinOp::Add => (x.checked_add(y), "adding", "+"),
                    BinOp::Sub => (x.checked_sub(y), "subtracting", "-"),
                    BinOp::Mul => (x.checked_mul(y), "multiplying", "*"),
                    _ => {
                        if y == 0 {
                            return eval_err(format!("division by zero{at}"));
                        }
                        (x.checked_div(y), "dividing", "/")
                    }
                };
                match r {
                    Some(r) => Ok(Value::Int(r)),
                    None => eval_err(format!("integer overflow in {verb} {x} {sym} {y}{at}")),
                }
            }
            (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
                let (x, y) = (as_f64(a), as_f64(b));
                Ok(Value::Float(match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    _ => {
                        if y == 0.0 {
                            return eval_err(format!("division by zero{at}"));
                        }
                        x / y
                    }
                }))
            }
            (Value::Int(_) | Value::Float(_), other) | (other, _) => {
                self.type_error("an integer", other)
            }
        }
    }

    /// Structural equality, as `==`.
    pub fn eq(&self, a: Value<'a>, b: Value<'a>) -> R<'a, bool> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        Ok(match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
                as_f64(a) == as_f64(b)
            }
            (Value::Str(x), Value::Str(y)) => x.s == y.s,
            (Value::Path(x), Value::Path(y)) => x.0 == y.0,
            (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::List(x), Value::List(y)) => {
                if x.items.len() != y.items.len() {
                    return Ok(false);
                }
                for (p, q) in x.items.iter().zip(y.items) {
                    if !self.eq_elem(*p, *q)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Attrs(x), Value::Attrs(y)) => {
                if self.is_derivation(x)? && self.is_derivation(y)? {
                    let out_path = self.ctx.syms.out_path;
                    if let (Some(p), Some(q)) = (x.get(out_path), y.get(out_path)) {
                        return self.eq_elem(p, q);
                    }
                }
                if x.len() != y.len() {
                    return Ok(false);
                }
                if x.entries
                    .iter()
                    .zip(y.entries)
                    .any(|(p, q)| p.name != q.name)
                {
                    return Ok(false);
                }
                for (p, q) in x.entries.iter().zip(y.entries) {
                    if !self.eq_elem(p.value, q.value)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => false,
        })
    }

    /// Equality of values stored in a list or set: a stored value equals
    /// itself without being forced, even if it is a function.
    fn eq_elem(&self, a: Value<'a>, b: Value<'a>) -> R<'a, bool> {
        if let (Some(p), Some(q)) = (a.ptr(), b.ptr())
            && p == q
            && std::mem::discriminant(&a) == std::mem::discriminant(&b)
        {
            return Ok(true);
        }
        self.eq(a, b)
    }

    pub fn is_derivation(&self, a: &'a Attrs<'a>) -> R<'a, bool> {
        match a.get(self.ctx.syms.type_) {
            Some(t) => match self.force(t)? {
                Value::Str(s) => Ok(s.s == b"derivation"),
                _ => Ok(false),
            },
            None => Ok(false),
        }
    }

    pub fn less_than(&self, a: Value<'a>, b: Value<'a>) -> R<'a, bool> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(x < y),
            (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
                Ok(as_f64(a) < as_f64(b))
            }
            (Value::Str(x), Value::Str(y)) => Ok(x.s < y.s),
            (Value::Path(x), Value::Path(y)) => Ok(x.0 < y.0),
            (Value::List(x), Value::List(y)) => {
                for (p, q) in x.items.iter().zip(y.items) {
                    if !self.eq(*p, *q)? {
                        return self.less_than(*p, *q);
                    }
                }
                Ok(x.items.len() < y.items.len())
            }
            _ => eval_err(format!(
                "cannot compare {} with {}",
                a.show_type(),
                b.show_type()
            )),
        }
    }

    // ---- string coercion --------------------------------------------------

    pub fn coerce_into(
        &self,
        v: Value<'a>,
        c: Coerce,
        buf: &mut Vec<u8>,
        ctx: &mut Vec<Ctx<'a>>,
    ) -> R<'a, ()> {
        let v = self.force(v)?;
        match v {
            Value::Str(s) => {
                buf.extend_from_slice(s.s);
                ctx.extend_from_slice(s.ctx);
            }
            Value::Path(p) => {
                if c.copy {
                    let sp = self.copy_path_to_store(p.0)?;
                    buf.extend_from_slice(sp.as_bytes());
                    ctx.push(Ctx::Opaque(sp));
                } else {
                    buf.extend_from_slice(p.0.as_bytes());
                }
            }
            Value::Attrs(a) => {
                let s = &self.ctx.syms;
                if let Some(f) = a.get(s.to_string) {
                    let r = self.call(f, v)?;
                    return self.coerce_into(r, c, buf, ctx);
                }
                if let Some(out) = a.get(s.out_path) {
                    return self.coerce_into(out, c, buf, ctx);
                }
                return eval_err(format!(
                    "cannot coerce a set to a string: {}",
                    crate::print::short(self, v)
                ));
            }
            Value::Null | Value::Bool(false) if c.more => {}
            Value::Bool(true) if c.more => buf.push(b'1'),
            Value::Int(i) if c.more => buf.extend_from_slice(i.to_string().as_bytes()),
            Value::Float(f) if c.more => {
                buf.extend_from_slice(crate::print::float_to_string(f).as_bytes())
            }
            Value::List(l) if c.more => {
                for (i, item) in l.items.iter().enumerate() {
                    if i > 0 {
                        buf.push(b' ');
                    }
                    self.coerce_into(*item, c, buf, ctx)?;
                }
            }
            other => {
                return eval_err(format!(
                    "cannot coerce {} to a string: {}",
                    other.show_type(),
                    crate::print::short(self, other)
                ));
            }
        }
        Ok(())
    }

    /// Coerce to a string with context.
    pub fn coerce_to_string(&self, v: Value<'a>, c: Coerce) -> R<'a, (Vec<u8>, Vec<Ctx<'a>>)> {
        let mut buf = Vec::new();
        let mut ctx = Vec::new();
        self.coerce_into(v, c, &mut buf, &mut ctx)?;
        Ok((buf, ctx))
    }

    /// Coerce to a filesystem path: a path value, or a string holding an
    /// absolute path.
    pub fn coerce_to_path(&self, v: Value<'a>) -> R<'a, String> {
        let v = self.force(v)?;
        if let Value::Path(p) = v {
            return Ok(p.0.to_owned());
        }
        let (buf, _) = self.coerce_to_string(v, Coerce::PLAIN)?;
        let s = String::from_utf8_lossy(&buf).into_owned();
        if !s.starts_with('/') {
            return eval_err(format!("string '{s}' doesn't represent an absolute path"));
        }
        Ok(canon_path(&s))
    }

    pub fn copy_path_to_store(&self, p: &str) -> R<'a, &'a str> {
        let sp = self
            .ctx
            .io
            .add_path_to_store(p)
            .map_err(|e| error(ErrorKind::Eval, format!("{e:#}")))?;
        Ok(self.bump.alloc_str(&sp))
    }

    // ---- import -----------------------------------------------------------

    /// Evaluate the file at `path` (a directory means its `default.nix`),
    /// sharing the result with every other import of it in this session.
    pub fn import(&self, path: &str) -> R<'a> {
        let path = self
            .ctx
            .io
            .resolve_import(path)
            .map_err(|e| error(ErrorKind::Eval, format!("{e:#}")))?;
        let cached = self.import_cache.borrow().get(&path).copied();
        if let Some(v) = cached {
            return self.force(v);
        }
        let expr = self
            .ctx
            .compile_file(&path)
            .map_err(|e| error(ErrorKind::Eval, format!("{e:#}")))?;
        let v = self.thunk(ThunkState::Expr(expr, self.root_env()));
        self.import_cache.borrow_mut().insert(path.clone(), v);
        self.force(v).map_err(|mut e| {
            e.trace.push(format!("while importing '{path}'"));
            e
        })
    }

    pub fn root_env(&self) -> &'a Env<'a> {
        self.bump.alloc(Env {
            parent: None,
            slots: &[],
        })
    }

    /// Parse and evaluate `text`, resolving relative paths against `base_dir`.
    pub fn eval_string(&self, text: &str, base_dir: &str) -> R<'a> {
        let expr = self
            .ctx
            .compile_str(text, base_dir)
            .map_err(|e| error(ErrorKind::Eval, e.msg))?;
        self.eval(expr, self.root_env())
    }
}

pub fn as_f64(v: Value<'_>) -> f64 {
    match v {
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
        _ => 0.0,
    }
}

impl Eval<'_> {
    /// Write every derivation created in this session to `dir` as a `.drv`
    /// file named after its store path, for comparison with Nix's.
    pub fn dump_derivations(&self, dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for (path, (drv, _)) in self.drvs.borrow().iter() {
            let name = path.rsplit('/').next().unwrap_or(path);
            std::fs::write(dir.join(name), drv.unparse(&drv.input_drvs))?;
        }
        Ok(())
    }
}

//! The long-lived state shared by evaluation sessions: interned names,
//! parsed sources and their compiled expressions.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use bumpalo::Bump;

use crate::builtins;
use crate::compile;
use crate::compile::CompileError;
use crate::derivation::Derivation;
use crate::io::Io;
use crate::ir::ExprRef;
use crate::ir::Source;
use crate::symbol;
use crate::symbol::Interner;
use crate::symbol::Sym;
use crate::symbol::Syms;

/// Owns everything that outlives a single evaluation: compiled files are
/// cached here so that later sessions reuse them, and all of it is freed when
/// the context is dropped.
pub struct Context {
    arena: Bump,
    pub interner: Interner,
    pub syms: Syms,
    pub io: Io,
    /// Compiled files by path. The `'static` is a lie told to the type
    /// system: the expressions live in `arena` and are only handed out with
    /// the lifetime of a borrow of `self`.
    files: RefCell<HashMap<String, ExprRef<'static>>>,
    pub(crate) regexes: builtins::regex::Cache,
    /// Derivations written to the store, with their modulo hashes, by `.drv`
    /// path.
    pub(crate) drvs: RefCell<HashMap<String, (Derivation, [u8; 32])>>,
}

impl Context {
    pub fn new(io: Io) -> Context {
        let interner = Interner::default();
        for name in symbol::NIX_STARTUP {
            if !(io.pure && symbol::NIX_STARTUP_IMPURE.contains(name)) {
                interner.intern(name);
            }
        }
        let syms = Syms::new(&interner);
        Context {
            arena: Bump::new(),
            interner,
            syms,
            io,
            files: RefCell::new(HashMap::new()),
            regexes: Default::default(),
            drvs: RefCell::new(HashMap::new()),
        }
    }

    pub fn intern(&self, name: &str) -> Sym {
        self.interner.intern(name)
    }

    pub fn name(&self, sym: Sym) -> &str {
        self.interner.resolve(sym)
    }

    /// Allocate an IR node. IR types hold only arena references, so not
    /// running their destructors leaks nothing.
    pub(crate) fn alloc<T>(&self, v: T) -> &T {
        self.arena.alloc(v)
    }

    pub(crate) fn alloc_slice<T>(&self, v: Vec<T>) -> &[T] {
        self.arena.alloc_slice_fill_iter(v)
    }

    pub(crate) fn alloc_str(&self, s: &str) -> &str {
        self.arena.alloc_str(s)
    }

    pub(crate) fn alloc_bytes(&self, b: &[u8]) -> &[u8] {
        self.arena.alloc_slice_copy(b)
    }

    pub(crate) fn source(&self, path: Option<&str>, text: &str) -> &Source<'_> {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        self.alloc(Source {
            path: path.map(|p| self.alloc_str(p)),
            text: self.alloc_str(text),
            line_starts: self.arena.alloc_slice_copy(&line_starts),
        })
    }

    /// Compile an expression that did not come from a file. In pure evaluation
    /// it rejects what Nix rejects while parsing in that mode.
    pub fn compile_str(&self, text: &str, base_dir: &str) -> Result<ExprRef<'_>, CompileError> {
        compile::compile(self, self.source(None, text), base_dir, self.io.pure, None)
    }

    /// Compile the file at the logical path `path`, which must be a file, not
    /// a directory, as [`compile_str`](Self::compile_str) does. Repeated calls
    /// return the same expression.
    pub fn compile_file(&self, path: &str) -> Result<ExprRef<'_>> {
        self.cached(path, || self.compile_path(path, None))
    }

    /// Compile Nix source that ships with the evaluator, once per context.
    pub fn compile_internal(&self, name: &str, text: &str) -> Result<ExprRef<'_>, CompileError> {
        let key = format!("<nix/{name}>");
        self.cached(&key, || {
            compile::compile(self, self.source(Some(&key), text), "/", true, None)
        })
    }

    /// Compile a file with extra names in scope (`scopedImport`). Not cached:
    /// the names differ per call.
    pub fn compile_file_scoped(&self, path: &str, names: &[Sym]) -> Result<ExprRef<'_>> {
        self.compile_path(path, Some(names))
    }

    fn compile_path(&self, path: &str, extra_scope: Option<&[Sym]>) -> Result<ExprRef<'_>> {
        let text = self.io.read_to_string(path)?;
        compile::compile(
            self,
            self.source(Some(path), &text),
            crate::path::dir_of(path),
            self.io.pure,
            extra_scope,
        )
        .map_err(|e| anyhow::anyhow!("{}", e.msg))
    }

    /// Return the expression cached under `key`, running `compile` and
    /// caching its result on a miss.
    fn cached<'c, E>(
        &'c self,
        key: &str,
        compile: impl FnOnce() -> Result<ExprRef<'c>, E>,
    ) -> Result<ExprRef<'c>, E> {
        if let Some(e) = self.files.borrow().get(key) {
            return Ok(e);
        }
        let expr = compile()?;
        // SAFETY: `expr` lives in `self.arena`, which is neither reset nor
        // dropped while `self` is alive, and the cache only returns it with the
        // lifetime of a borrow of `self`.
        let stored: ExprRef<'static> = unsafe { std::mem::transmute(expr) };
        self.files.borrow_mut().insert(key.to_owned(), stored);
        Ok(expr)
    }
}

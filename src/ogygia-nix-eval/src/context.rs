//! The long-lived state shared by evaluation sessions: interned names,
//! parsed sources and their compiled expressions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use anyhow::Result;
use bumpalo::Bump;

use crate::builtins;
use crate::compile;
use crate::compile::CompileError;
use crate::io::Io;
use crate::ir::ExprRef;
use crate::ir::Source;
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
    globals: HashSet<Sym>,
    /// Compiled files by path. The `'static` is a lie told to the type
    /// system: the expressions live in `arena` and are only handed out with
    /// the lifetime of a borrow of `self`.
    files: RefCell<HashMap<String, ExprRef<'static>>>,
    /// Compiled `builtins.match` patterns by source.
    pub(crate) match_regexes: RefCell<HashMap<Vec<u8>, Rc<regex::bytes::Regex>>>,
    /// Compiled `builtins.split` patterns by source.
    pub(crate) split_regexes: RefCell<HashMap<Vec<u8>, Rc<builtins::strings::SplitRegex>>>,
}

impl Context {
    pub fn new(io: Io) -> Context {
        let interner = Interner::default();
        let syms = Syms::new(&interner);
        let globals = builtins::global_names()
            .map(|n| interner.intern(&n))
            .collect();
        Context {
            arena: Bump::new(),
            interner,
            syms,
            io,
            globals,
            files: RefCell::new(HashMap::new()),
            match_regexes: RefCell::new(HashMap::new()),
            split_regexes: RefCell::new(HashMap::new()),
        }
    }

    pub fn intern(&self, name: &str) -> Sym {
        self.interner.intern(name)
    }

    pub fn name(&self, sym: Sym) -> &str {
        self.interner.resolve(sym)
    }

    pub fn is_global(&self, sym: Sym) -> bool {
        self.globals.contains(&sym)
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

    /// Compile an expression that did not come from a file.
    pub fn compile_str(&self, text: &str, base_dir: &str) -> Result<ExprRef<'_>, CompileError> {
        compile::compile(self, self.source(None, text), base_dir, None)
    }

    /// Compile the file at the logical path `path`, which must be a file, not
    /// a directory. Repeated calls return the same expression.
    pub fn compile_file(&self, path: &str) -> Result<ExprRef<'_>> {
        if let Some(e) = self.files.borrow().get(path) {
            return Ok(e);
        }
        let text = self.io.read_to_string(path)?;
        let dir = crate::path::dir_of(path);
        let expr = compile::compile(self, self.source(Some(path), &text), dir, None)
            .map_err(|e| anyhow::anyhow!("{}", e.msg))?;
        // SAFETY: `expr` lives in `self.arena`, which is neither reset nor
        // dropped while `self` is alive, and the cache only returns it with the
        // lifetime of a borrow of `self`.
        let stored: ExprRef<'static> = unsafe { std::mem::transmute(expr) };
        self.files.borrow_mut().insert(path.to_owned(), stored);
        Ok(expr)
    }

    /// Compile Nix source that ships with the evaluator, once per context.
    pub fn compile_internal(&self, name: &str, text: &str) -> Result<ExprRef<'_>, CompileError> {
        let key = format!("<nix/{name}>");
        if let Some(e) = self.files.borrow().get(&key) {
            return Ok(e);
        }
        let expr = compile::compile(self, self.source(Some(&key), text), "/", None)?;
        // SAFETY: as in `compile_file`.
        let stored: ExprRef<'static> = unsafe { std::mem::transmute(expr) };
        self.files.borrow_mut().insert(key, stored);
        Ok(expr)
    }

    /// Compile a file with extra names in scope (`scopedImport`). Not cached:
    /// the names differ per call.
    pub fn compile_file_scoped(&self, path: &str, names: &[Sym]) -> Result<ExprRef<'_>> {
        let text = self.io.read_to_string(path)?;
        let dir = crate::path::dir_of(path);
        compile::compile(self, self.source(Some(path), &text), dir, Some(names))
            .map_err(|e| anyhow::anyhow!("{}", e.msg))
    }
}

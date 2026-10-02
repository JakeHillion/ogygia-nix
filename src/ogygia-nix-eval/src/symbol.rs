//! Interned attribute and variable names.

use lasso::Spur;
use lasso::ThreadedRodeo;

/// An interned name. Symbols compare by identity; their textual order is
/// only available through the [`Interner`] that created them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Sym(Spur);

#[derive(Default)]
pub struct Interner(ThreadedRodeo);

impl Interner {
    pub fn intern(&self, name: &str) -> Sym {
        Sym(self.0.get_or_intern(name))
    }

    /// Intern raw string bytes. Names are UTF-8 in practice; invalid bytes are
    /// replaced so that every symbol has a printable form.
    pub fn intern_bytes(&self, name: &[u8]) -> Sym {
        match std::str::from_utf8(name) {
            Ok(s) => self.intern(s),
            Err(_) => self.intern(&String::from_utf8_lossy(name)),
        }
    }

    /// The symbol for `name` if it has been interned.
    pub fn get(&self, name: &str) -> Option<Sym> {
        self.0.get(name).map(Sym)
    }

    pub fn resolve(&self, sym: Sym) -> &str {
        self.0.resolve(&sym.0)
    }
}

macro_rules! well_known {
    ($($field:ident = $name:literal,)*) => {
        /// Symbols the evaluator refers to by name.
        pub struct Syms {
            $(pub $field: Sym,)*
        }

        impl Syms {
            pub fn new(interner: &Interner) -> Syms {
                Syms {
                    $($field: interner.intern($name),)*
                }
            }
        }
    };
}

well_known! {
    functor = "__functor",
    out_path = "outPath",
    type_ = "type",
    to_string = "__toString",
    name = "name",
    value = "value",
    success = "success",
    file = "file",
    line = "line",
    column = "column",
    outputs = "outputs",
    out = "out",
    drv_path = "drvPath",
    output_name = "outputName",
    ignore_nulls = "__ignoreNulls",
    structured_attrs = "__structuredAttrs",
    path = "path",
    prefix = "prefix",
    key = "key",
    start_set = "startSet",
    operator = "operator",
    right = "right",
    wrong = "wrong",
    system = "system",
    builder = "builder",
    args = "args",
    all_outputs = "allOutputs",
    body = "body",
    find_file = "__findFile",
    nix_path = "__nixPath",
    cur_pos = "__curPos",
    true_ = "true",
    false_ = "false",
    null = "null",
}

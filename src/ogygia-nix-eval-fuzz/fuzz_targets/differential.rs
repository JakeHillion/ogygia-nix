//! Differential fuzzing against `nix-instantiate`.
//!
//! Each input drives a generator of pure Nix expressions. The expression is
//! deeply evaluated by ogygia-nix-eval and by `nix-instantiate --eval
//! --strict`; the input is a crash when exactly one of them fails, or both
//! succeed with different output. Error messages are not compared.
//!
//! `nix-instantiate` is `$OGYGIA_NIX_EVAL_NIX_INSTANTIATE`, else the path
//! baked in at build time from `$OGYGIA_NIX_INSTANTIATE_BIN`, else found on
//! `PATH`.

#![no_main]

use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use arbitrary::Result;
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

const DEEP_COPY: &str = "let dc = v: if builtins.isAttrs v then builtins.mapAttrs (_: dc) v \
                         else if builtins.isList v then map dc v else v; in dc";

/// How long Nix may spend on one expression before the input is discarded.
const NIX_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_DEPTH: u32 = 6;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Any,
    Int,
    Float,
    Bool,
    Str,
    List,
    Attrs,
    Fn1,
    Fn2,
}

/// Pure builtins and the kinds of their arguments.
const BUILTINS: &[(&str, &[Kind])] = {
    use Kind::*;
    &[
        ("add", &[Int, Int]),
        ("add", &[Float, Int]),
        ("all", &[Fn1, List]),
        ("any", &[Fn1, List]),
        ("attrNames", &[Attrs]),
        ("attrValues", &[Attrs]),
        ("baseNameOf", &[Str]),
        ("bitAnd", &[Int, Int]),
        ("bitOr", &[Int, Int]),
        ("bitXor", &[Int, Int]),
        ("catAttrs", &[Str, List]),
        ("ceil", &[Float]),
        ("compareVersions", &[Str, Str]),
        ("concatLists", &[List]),
        ("concatMap", &[Fn1, List]),
        ("concatStringsSep", &[Str, List]),
        ("deepSeq", &[Any, Any]),
        ("dirOf", &[Str]),
        ("div", &[Int, Int]),
        ("div", &[Float, Float]),
        ("elem", &[Any, List]),
        ("elemAt", &[List, Int]),
        ("filter", &[Fn1, List]),
        ("floor", &[Float]),
        ("foldl'", &[Fn2, Any, List]),
        ("fromJSON", &[Str]),
        ("fromTOML", &[Str]),
        ("functionArgs", &[Fn1]),
        ("genList", &[Fn1, Int]),
        ("getAttr", &[Str, Attrs]),
        ("groupBy", &[Fn1, List]),
        ("hasAttr", &[Str, Attrs]),
        ("hashString", &[Str, Str]),
        ("head", &[List]),
        ("intersectAttrs", &[Attrs, Attrs]),
        ("isAttrs", &[Any]),
        ("isBool", &[Any]),
        ("isFloat", &[Any]),
        ("isFunction", &[Any]),
        ("isInt", &[Any]),
        ("isList", &[Any]),
        ("isNull", &[Any]),
        ("isString", &[Any]),
        ("length", &[List]),
        ("lessThan", &[Any, Any]),
        ("listToAttrs", &[List]),
        ("map", &[Fn1, List]),
        ("mapAttrs", &[Fn2, Attrs]),
        ("match", &[Str, Str]),
        ("mul", &[Int, Int]),
        ("mul", &[Float, Float]),
        ("parseDrvName", &[Str]),
        ("partition", &[Fn1, List]),
        ("removeAttrs", &[Attrs, List]),
        ("replaceStrings", &[List, List, Str]),
        ("seq", &[Any, Any]),
        ("sort", &[Fn2, List]),
        ("split", &[Str, Str]),
        ("splitVersion", &[Str]),
        ("stringLength", &[Str]),
        ("sub", &[Int, Int]),
        ("substring", &[Int, Int, Str]),
        ("tail", &[List]),
        ("toJSON", &[Any]),
        ("toString", &[Any]),
        ("toXML", &[Any]),
        ("tryEval", &[Any]),
        ("typeOf", &[Any]),
        ("zipAttrsWith", &[Fn2, List]),
        ("unsafeDiscardStringContext", &[Str]),
        ("getContext", &[Str]),
        ("hasContext", &[Str]),
        ("convertHash", &[Attrs]),
        ("lessThan", &[Str, Str]),
        ("trace", &[Any, Any]),
    ]
};

const NAMES: &[&str] = &["a", "b", "c", "x", "y", "name", "value", "or"];

const INTS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "-1",
    "7",
    "10",
    "64",
    "255",
    "4294967296",
    "9223372036854775807",
    "(-9223372036854775807 - 1)",
    "00012",
];

const FLOATS: &[&str] = &[
    "0.0",
    "1.5",
    "-2.5",
    ".5",
    "1e3",
    "1.0e-5",
    "3.14159",
    "1e21",
    "1e-7",
    "123456.789",
    "0.1",
    "2.0",
    "1e16",
    "100000.0",
    "1E5",
    "5.e2",
    "1.7976931348623157e308",
    "4.9e-324",
];

/// String fragments, already escaped for a double-quoted string.
const STR_PIECES: &[&str] = &[
    "a",
    "b",
    "abc",
    " ",
    "\\n",
    "\\t",
    "\\r",
    "\\\"",
    "\\\\",
    "\\$",
    "$",
    "$$",
    "\\${",
    "é",
    "ß",
    "😀",
    "1.2.3",
    "1.2pre3",
    "-",
    "foo-1.0",
    "/",
    "/a/b",
    "a/b/",
    ".",
    "..",
    "x",
    "aa",
    "A",
    "_",
    ",",
    "0",
    "\\x",
    "{",
    "}",
    "[",
    "]",
    "(",
    ")",
    "*",
    "+",
    "?",
    "|",
    "^",
    "[[:alpha:]]",
    "[[:space:]]",
    "\\\\d",
    ".*",
    "(a|b)",
    "md5",
    "sha1",
    "sha256",
    "sha512",
    "true",
    "null",
    "1e5",
    "\\u00e9",
    "\\0",
];

const JSON: &[&str] = &[
    r#"{"a": 1, "b": [true, null, 1.5]}"#,
    "[]",
    "{}",
    "1e400",
    "-0",
    "0.1",
    "123456789012345678901234567890",
    r#""é😀""#,
    r#""\u0000""#,
    "  [1,2 ]  ",
    r#"{"a":1,"a":2}"#,
    "18446744073709551615",
    "-9223372036854775808",
    "1E2",
    "true",
    "nul",
    r#""\/""#,
];

const TOML: &[&str] = &[
    "a = 1",
    "a = 1.5\nb = \"x\"",
    "[t]\nx = [1, 2]",
    "a.b.c = true",
    "d = 1979-05-27T07:32:00Z",
    "x = 0x1f",
    "x = inf",
    "x = 1_000",
    "[[arr]]\na = 1\n[[arr]]\nb = 2",
    "s = '''\nline'''",
    "x = { y = 1 }",
];

struct Gen<'u, 'd> {
    u: &'u mut Unstructured<'d>,
    out: String,
    scope: Vec<&'static str>,
}

impl Gen<'_, '_> {
    fn pick<T: Copy>(&mut self, xs: &[T]) -> Result<T> {
        Ok(*self.u.choose(xs)?)
    }

    fn name(&mut self) -> Result<&'static str> {
        self.pick(NAMES)
    }

    fn attr_name(&mut self) -> Result<()> {
        match self.u.int_in_range(0..=5)? {
            0 => {
                let s = self.pick(STR_PIECES)?;
                write!(self.out, "\"{s}\"").unwrap();
            }
            1 => {
                self.out.push_str("${");
                self.expr(Kind::Str, MAX_DEPTH)?;
                self.out.push('}');
            }
            _ => {
                let n = self.name()?;
                self.out.push_str(n);
            }
        }
        Ok(())
    }

    fn string(&mut self, depth: u32) -> Result<()> {
        if self.u.ratio(1, 4)? {
            return self.indented(depth);
        }
        if self.u.ratio(1, 8)? {
            let s = if self.u.arbitrary()? {
                self.pick(JSON)?
            } else {
                self.pick(TOML)?
            };
            self.out.push_str(&nix_string(s));
            return Ok(());
        }
        self.out.push('"');
        for _ in 0..self.u.int_in_range(0..=4)? {
            if depth > 0 && self.u.ratio(1, 6)? {
                self.out.push_str("${");
                self.expr(Kind::Str, depth - 1)?;
                self.out.push('}');
            } else {
                let s = self.pick(STR_PIECES)?;
                self.out.push_str(s);
            }
        }
        self.out.push('"');
        Ok(())
    }

    fn indented(&mut self, depth: u32) -> Result<()> {
        const PIECES: &[&str] = &[
            "a", " ", "  ", "\t", "\n", "\n  ", "\n    ", "''$", "'''", "''\\n", "''\\t", "$",
            "$$", "\\", "\"", "'", "x y", "''\\", "\r\n",
        ];
        self.out.push_str("''");
        for _ in 0..self.u.int_in_range(0..=6)? {
            if depth > 0 && self.u.ratio(1, 6)? {
                self.out.push_str("${");
                self.expr(Kind::Str, depth - 1)?;
                self.out.push('}');
            } else {
                let s = self.pick(PIECES)?;
                self.out.push_str(s);
            }
        }
        self.out.push_str("''");
        Ok(())
    }

    fn func(&mut self, arity: u32, depth: u32) -> Result<()> {
        self.out.push('(');
        let mut bound = Vec::new();
        for _ in 0..arity {
            if self.u.ratio(1, 4)? {
                self.out.push('{');
                for _ in 0..self.u.int_in_range(0..=2)? {
                    let n = self.name()?;
                    write!(self.out, "{n}").unwrap();
                    if self.u.arbitrary()? {
                        self.out.push_str(" ? ");
                        self.expr(Kind::Any, depth)?;
                    }
                    self.out.push_str(", ");
                    bound.push(n);
                }
                if self.u.arbitrary()? {
                    self.out.push_str("...");
                }
                self.out.push('}');
                if self.u.arbitrary()? {
                    let n = self.name()?;
                    write!(self.out, "@{n}").unwrap();
                    bound.push(n);
                }
                self.out.push_str(": ");
            } else {
                let n = self.name()?;
                write!(self.out, "{n}: ").unwrap();
                bound.push(n);
            }
        }
        let len = self.scope.len();
        self.scope.extend(bound);
        self.expr(Kind::Any, depth)?;
        self.scope.truncate(len);
        self.out.push(')');
        Ok(())
    }

    fn binding(&mut self, depth: u32) -> Result<&'static str> {
        let n = self.name()?;
        if self.u.ratio(1, 6)? {
            write!(
                self.out,
                "inherit ({}) {n}; ",
                self.scope.first().copied().unwrap_or("{}")
            )
            .unwrap();
            return Ok(n);
        }
        if !self.scope.is_empty() && self.u.ratio(1, 6)? {
            let s = self.pick(&self.scope.clone())?;
            write!(self.out, "inherit {s}; ").unwrap();
            return Ok(s);
        }
        self.out.push_str(n);
        if self.u.ratio(1, 5)? {
            self.out.push('.');
            self.attr_name()?;
        }
        self.out.push_str(" = ");
        self.expr(Kind::Any, depth)?;
        self.out.push_str("; ");
        Ok(n)
    }

    fn attrs(&mut self, depth: u32) -> Result<()> {
        let rec = self.u.ratio(1, 4)?;
        if rec {
            self.out.push_str("rec ");
        }
        self.out.push_str("{ ");
        let len = self.scope.len();
        for _ in 0..self.u.int_in_range(0..=4)? {
            if self.u.ratio(1, 5)? {
                self.attr_name()?;
                self.out.push_str(" = ");
                self.expr(Kind::Any, depth)?;
                self.out.push_str("; ");
            } else {
                let n = self.binding(depth)?;
                if rec {
                    self.scope.push(n);
                }
            }
        }
        self.scope.truncate(len);
        self.out.push('}');
        Ok(())
    }

    fn literal(&mut self, kind: Kind, depth: u32) -> Result<()> {
        match kind {
            Kind::Int => {
                let s = self.pick(INTS)?;
                self.out.push_str(s);
            }
            Kind::Float => {
                let s = self.pick(FLOATS)?;
                self.out.push_str(s);
            }
            Kind::Bool => {
                let s = self.pick(&["true", "false", "null"])?;
                self.out.push_str(s);
            }
            Kind::Str => self.string(depth)?,
            Kind::List => {
                self.out.push('[');
                for _ in 0..self.u.int_in_range(0..=4)? {
                    self.out.push(' ');
                    self.atom(Kind::Any, depth)?;
                }
                self.out.push_str(" ]");
            }
            Kind::Attrs => self.attrs(depth)?,
            Kind::Fn1 => self.func(1, depth)?,
            Kind::Fn2 => self.func(2, depth)?,
            Kind::Any => {
                let k = self.pick(&[
                    Kind::Int,
                    Kind::Float,
                    Kind::Bool,
                    Kind::Str,
                    Kind::Str,
                    Kind::List,
                    Kind::Attrs,
                    Kind::Fn1,
                ])?;
                self.literal(k, depth)?;
            }
        }
        Ok(())
    }

    /// An expression that needs no parentheses as a list element or argument.
    fn atom(&mut self, kind: Kind, depth: u32) -> Result<()> {
        self.out.push('(');
        self.expr(kind, depth)?;
        self.out.push(')');
        Ok(())
    }

    fn expr(&mut self, kind: Kind, depth: u32) -> Result<()> {
        if depth == 0 || self.u.is_empty() {
            return self.literal(kind, 0);
        }
        let d = depth - 1;
        match self.u.int_in_range(0..=15)? {
            0..=3 => self.literal(kind, d)?,
            4 if !self.scope.is_empty() => {
                let n = self.pick(&self.scope.clone())?;
                self.out.push_str(n);
            }
            5 | 6 => {
                let (name, args) = self.pick(BUILTINS)?;
                write!(self.out, "builtins.{name}").unwrap();
                for &k in args {
                    self.out.push(' ');
                    self.atom(k, d)?;
                }
            }
            7 => {
                const OPS: &[&str] = &[
                    "+", "-", "*", "/", "++", "//", "==", "!=", "<", "<=", ">", ">=", "&&", "||",
                    "->",
                ];
                let op = self.pick(OPS)?;
                let k = match op {
                    "++" => Kind::List,
                    "//" => Kind::Attrs,
                    "&&" | "||" | "->" => Kind::Bool,
                    _ => self.pick(&[Kind::Int, Kind::Float, Kind::Str, Kind::Any])?,
                };
                let paren = self.u.arbitrary()?;
                if paren {
                    self.atom(k, d)?;
                } else {
                    self.expr(k, d)?;
                }
                write!(self.out, " {op} ").unwrap();
                if paren {
                    self.atom(k, d)?;
                } else {
                    self.expr(k, d)?;
                }
            }
            8 => {
                let op = self.pick(&["!", "-"])?;
                self.out.push_str(op);
                self.atom(kind, d)?;
            }
            9 => {
                self.out.push_str("if ");
                self.expr(Kind::Bool, d)?;
                self.out.push_str(" then ");
                self.expr(kind, d)?;
                self.out.push_str(" else ");
                self.expr(kind, d)?;
            }
            10 => {
                self.out.push_str("let ");
                let len = self.scope.len();
                for _ in 0..self.u.int_in_range(1..=3)? {
                    let n = self.binding(d)?;
                    self.scope.push(n);
                }
                self.out.push_str("in ");
                self.expr(kind, d)?;
                self.scope.truncate(len);
            }
            11 => {
                self.out.push_str("with ");
                self.atom(Kind::Attrs, d)?;
                self.out.push_str("; ");
                self.expr(kind, d)?;
            }
            12 => {
                self.atom(Kind::Attrs, d)?;
                self.out.push('.');
                self.attr_name()?;
                if self.u.arbitrary()? {
                    self.out.push_str(" or ");
                    self.atom(kind, d)?;
                }
            }
            13 => {
                self.atom(Kind::Attrs, d)?;
                self.out.push_str(" ? ");
                self.attr_name()?;
            }
            14 => {
                self.atom(Kind::Fn1, d)?;
                self.out.push(' ');
                self.atom(Kind::Any, d)?;
            }
            _ => {
                self.out.push_str("assert ");
                self.expr(Kind::Bool, d)?;
                self.out.push_str("; ");
                self.expr(kind, d)?;
            }
        }
        Ok(())
    }
}

fn nix_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn generate(data: &[u8]) -> Option<String> {
    let mut u = Unstructured::new(data);
    let mut g = Gen {
        u: &mut u,
        out: String::new(),
        scope: Vec::new(),
    };
    g.expr(Kind::Any, MAX_DEPTH).ok()?;
    Some(g.out)
}

fn scratch() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ogygia-nix-eval-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    })
}

fn nix_instantiate() -> String {
    std::env::var("OGYGIA_NIX_EVAL_NIX_INSTANTIATE")
        .ok()
        .or_else(|| option_env!("OGYGIA_NIX_INSTANTIATE_BIN").map(str::to_owned))
        .unwrap_or_else(|| "nix-instantiate".to_owned())
}

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        r.read_to_end(&mut buf)
            .expect("reading nix-instantiate output");
        buf
    })
}

/// `None` when Nix ran out of time or stack, which says nothing about
/// equivalence.
fn run_nix(expr: &str) -> Option<Result<String, String>> {
    let s = scratch();
    let mut child = Command::new(nix_instantiate())
        .args([
            "--eval",
            "--strict",
            "--readonly-mode",
            "--store",
            "dummy://",
            "--expr",
        ])
        .arg(expr)
        .env("HOME", s)
        .env("NIX_STATE_DIR", s.join("state"))
        .env("NIX_CONF_DIR", s.join("conf"))
        .env("NIX_LOG_DIR", s.join("log"))
        .env("XDG_CACHE_HOME", s.join("cache"))
        .env_remove("NIX_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("running nix-instantiate");
    let stdout = read_all(child.stdout.take().unwrap());
    let stderr = read_all(child.stderr.take().unwrap());
    let deadline = Instant::now() + NIX_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting for nix-instantiate") {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().expect("killing nix-instantiate");
            child.wait().expect("waiting for nix-instantiate");
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap()).into_owned();
    let status = status?;
    if stderr.contains("stack overflow") || stderr.contains("max-call-depth") {
        return None;
    }
    Some(if status.success() {
        Ok(String::from_utf8_lossy(&stdout).trim_end().to_owned())
    } else {
        Err(stderr)
    })
}

fn show(r: &Result<String, String>) -> String {
    match r {
        Ok(s) => s.clone(),
        Err(e) => format!("error: {}", e.trim_end()),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(expr) = generate(data) else {
        return;
    };
    let expr = format!("{DEEP_COPY} ({expr})");
    let ours = ogygia_nix_eval::eval_to_string(&expr, "/", ogygia_nix_eval::Settings::default());
    let Some(theirs) = run_nix(&expr) else {
        return;
    };
    match (&theirs, &ours) {
        (Ok(a), Ok(b)) if a == b => {}
        (Err(_), Err(_)) => {}
        _ => panic!(
            "nix and ogygia-nix-eval disagree on\n{expr}\n--- nix:\n{}\n--- ours:\n{}",
            show(&theirs),
            show(&ours)
        ),
    }
});

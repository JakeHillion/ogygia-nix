//! Lowering of the rnix syntax tree into [`crate::ir`].

use std::borrow::Cow;
use std::collections::HashMap;

use num_bigint::BigUint;
use rnix::ast;
use rnix::ast::AstToken;
use rnix::ast::HasEntry;
use rnix::ast::InterpolPart;
use rowan::ast::AstNode;

use crate::context::Context;
use crate::ir::AttrKey;
use crate::ir::AttrValue;
use crate::ir::AttrsDef;
use crate::ir::BinOp;
use crate::ir::DynamicAttr;
use crate::ir::Expr;
use crate::ir::ExprRef;
use crate::ir::Formal;
use crate::ir::LambdaDef;
use crate::ir::LetDef;
use crate::ir::Param;
use crate::ir::Pos;
use crate::ir::SlotInit;
use crate::ir::Source;
use crate::ir::StaticAttr;
use crate::path::canon_path;
use crate::symbol::Sym;
use crate::value::NixStr;
use crate::value::PathV;

/// A parse or scoping error.
#[derive(Debug)]
pub struct CompileError {
    pub msg: String,
}

type CResult<T> = Result<T, CompileError>;

fn err<T>(msg: impl Into<String>) -> CResult<T> {
    Err(CompileError { msg: msg.into() })
}

enum Scope {
    Names(HashMap<Sym, u32>),
    With,
}

struct Compiler<'a> {
    ctx: &'a Context,
    source: &'a Source<'a>,
    /// Offsets of the bytes [`match_nix_tokens`] inserted.
    inserted: Vec<u32>,
    /// Directory relative path literals are resolved against.
    base_dir: &'a str,
    pure: bool,
    scopes: Vec<Scope>,
}

/// Parse and compile `source`. `extra_scope` names are bound in an outermost
/// frame (`scopedImport`). In `pure` mode, `~/` paths are a compile error, as
/// Nix rejects them while parsing.
pub fn compile<'a>(
    ctx: &'a Context,
    source: &'a Source<'a>,
    base_dir: &str,
    pure: bool,
    extra_scope: Option<&[Sym]>,
) -> CResult<ExprRef<'a>> {
    let text = end_comments_at_cr(source.text);
    let (text, inserted) = match_nix_tokens(&text);
    let pos = |offset: rnix::TextSize| Pos {
        source,
        offset: source_offset(&inserted, u32::from(offset)),
    };
    if let Some(at) = invalid_whitespace(&text) {
        return err(format!(
            "syntax error: unexpected invalid token at {}",
            pos(at.into())
        ));
    }
    let parse = rnix::Root::parse(&text);
    if let Some(e) = parse.errors().first() {
        let range = match e {
            rnix::ParseError::Unexpected(r)
            | rnix::ParseError::UnexpectedExtra(r)
            | rnix::ParseError::UnexpectedWanted(_, r, _)
            | rnix::ParseError::UnexpectedDoubleBind(r)
            | rnix::ParseError::DuplicatedArgs(r, _) => Some(*r),
            _ => None,
        };
        let at = range
            .map(|r| format!(" at {}", pos(r.start())))
            .unwrap_or_default();
        return err(format!("syntax error: {e}{at}"));
    }
    // rnix closes a parenthesised expression with whatever token follows
    // it, without reporting an error when that is not `)`.
    if let Some(t) = parse
        .syntax()
        .descendants()
        .filter(|n| n.kind() == rnix::SyntaxKind::NODE_PAREN)
        .filter_map(|n| n.last_token())
        .find(|t| t.kind() != rnix::SyntaxKind::TOKEN_R_PAREN)
    {
        return err(format!(
            "syntax error: unexpected {}, expecting ')' at {}",
            t.text(),
            pos(t.text_range().start())
        ));
    }
    // rnix parses a function wherever it parses an operand, but Nix only
    // allows one unparenthesised outside operators, lists and selects.
    if let Some(n) = parse.syntax().descendants().find(|n| {
        n.kind() == rnix::SyntaxKind::NODE_LAMBDA
            && n.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    rnix::SyntaxKind::NODE_LIST
                        | rnix::SyntaxKind::NODE_BIN_OP
                        | rnix::SyntaxKind::NODE_UNARY_OP
                        | rnix::SyntaxKind::NODE_SELECT
                        | rnix::SyntaxKind::NODE_HAS_ATTR
                        | rnix::SyntaxKind::NODE_APPLY
                )
            })
    }) {
        return err(format!(
            "syntax error: unexpected function at {}",
            pos(n.text_range().start())
        ));
    }
    let root = parse.tree();
    let expr = child(root.expr())?;
    let mut c = Compiler {
        ctx,
        source,
        inserted,
        base_dir: ctx.alloc_str(base_dir),
        pure,
        scopes: Vec::new(),
    };
    if let Some(names) = extra_scope {
        c.scopes.push(scope_of(names));
    }
    c.expr(&expr)
}

/// rnix lexes any Unicode whitespace between tokens as whitespace, but Nix
/// only space, tab, `\r` and `\n`, and any other character there is an invalid
/// token. Returns the offset of the first other whitespace character.
fn invalid_whitespace(text: &str) -> Option<u32> {
    let mut start = 0;
    rnix::tokenize(text).find_map(|(kind, s)| {
        let at = start;
        start += s.len();
        if kind != rnix::SyntaxKind::TOKEN_WHITESPACE {
            return None;
        }
        s.find(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
            .map(|i| (at + i) as u32)
    })
}

/// Nix ends a `#` comment at `\r` as well as `\n`, but rnix only at `\n`.
/// Rewrites each `\r` that ends a comment to `\n`, which keeps offsets and
/// changes nothing else, then retokenises as the text after it is now code.
fn end_comments_at_cr(text: &str) -> Cow<'_, str> {
    let mut text = Cow::Borrowed(text);
    if !text.contains('\r') {
        return text;
    }
    loop {
        let mut start = 0;
        let cr = rnix::tokenize(&text).find_map(|(kind, s)| {
            let at = start;
            start += s.len();
            if kind == rnix::SyntaxKind::TOKEN_COMMENT && s.starts_with('#') {
                s.find('\r').map(|i| at + i)
            } else {
                None
            }
        });
        match cr {
            Some(i) => text.to_mut().replace_range(i..=i, "\n"),
            None => return text,
        }
    }
}

/// Inserts text wherever rnix lexes the source differently from Nix: a space
/// where rnix lexes one token but Nix lexes several, and `./` before an ellipsis
/// Nix lexes as the start of a path. Retokenises after each as the text after
/// it changes, and returns the offsets of the inserted bytes in the result.
fn match_nix_tokens(text: &str) -> (Cow<'_, str>, Vec<u32>) {
    let mut text = Cow::Borrowed(text);
    let mut inserted = Vec::new();
    if !text.contains(['/', '.', '<']) {
        return (text, inserted);
    }
    loop {
        let mut start = 0;
        let mut prev = None;
        let insert = rnix::tokenize(&text).find_map(|(kind, s)| {
            let at = start;
            start += s.len();
            let after_interpol = prev == Some(rnix::SyntaxKind::TOKEN_INTERPOL_END);
            prev = Some(kind);
            split_division(kind, s, after_interpol)
                .or_else(|| split_number(kind, s))
                .or_else(|| split_less(kind, s))
                .map(|i| (at + i, " "))
                .or_else(|| ellipsis_path(kind, &text[at..]).then_some((at, "./")))
        });
        match insert {
            Some((i, s)) => {
                text.to_mut().insert_str(i, s);
                inserted.extend((i..i + s.len()).map(|i| i as u32));
            }
            None => return (text, inserted),
        }
    }
}

/// Nix lexes `...` followed by path characters and a `/` that continues the
/// path, as in `.../a`, as a relative path, but rnix lexes `...` as an
/// ellipsis. Returns whether `rest` starts with such an ellipsis token, which
/// `./` before it makes rnix lex as the same relative path.
fn ellipsis_path(kind: rnix::SyntaxKind, rest: &str) -> bool {
    let path_char = |c: &u8| c.is_ascii_alphanumeric() || b"._-+".contains(c);
    let b = rest.as_bytes();
    kind == rnix::SyntaxKind::TOKEN_ELLIPSIS
        && b.iter().position(|c| !path_char(c)).is_some_and(|i| {
            b[i] == b'/' && (b.get(i + 1).is_some_and(path_char) || b[i + 1..].starts_with(b"${"))
        })
}

/// rnix lexes `a/` followed by a character that cannot continue a path, as in
/// `a/(b)` or `a/"b"`, as a path with a trailing slash, but Nix only lexes a
/// path when a path character or `${` follows the `/`, so this is `a / ...`.
/// Returns the offset of that `/` in the token.
fn split_division(kind: rnix::SyntaxKind, s: &str, after_interpol: bool) -> Option<usize> {
    // A path continued after an interpolation, as in `./a${b}c/`, has a
    // trailing slash in Nix too.
    (kind == rnix::SyntaxKind::TOKEN_ERROR
        && !after_interpol
        && !s.starts_with('~')
        && s.find('/') == Some(s.len() - 1))
    .then_some(s.len() - 1)
}

/// Nix lexes `<` as the start of a search path only when path segments
/// separated by single slashes and then `>` follow it, and otherwise as less
/// than. rnix also lexes a search path with an empty segment, as in `<a//b>`,
/// and an error token when another `<` follows, as in `<a/b<c>`, which Nix
/// lexes as `< a/b <c>`. Returns the offset after such a `<`.
fn split_less(kind: rnix::SyntaxKind, s: &str) -> Option<usize> {
    let search_path = s
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .is_some_and(|s| {
            s.split('/').all(|seg| {
                !seg.is_empty()
                    && seg
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c))
            })
        });
    (matches!(
        kind,
        rnix::SyntaxKind::TOKEN_PATH_SEARCH | rnix::SyntaxKind::TOKEN_ERROR
    ) && s.starts_with('<')
        && !search_path)
        .then_some(1)
}

/// Nix lexes a float as `(([1-9][0-9]*\.[0-9]*)|(0?\.[0-9]+))([Ee][+-]?[0-9]+)?`
/// and an integer as `[0-9]+`, but rnix lexes `0.` followed by a non-digit as
/// a float and an exponent marker without digits, as in `1.5e`, as an error,
/// where Nix lexes `0 .x` and `1.5 e`. Returns where Nix's number ends in a
/// number token rnix lexed longer.
fn split_number(kind: rnix::SyntaxKind, s: &str) -> Option<usize> {
    let numeric = match kind {
        rnix::SyntaxKind::TOKEN_FLOAT => true,
        rnix::SyntaxKind::TOKEN_ERROR => {
            s.starts_with(|c: char| c.is_ascii_digit() || c == '.')
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || b".eE+-".contains(&c))
        }
        _ => false,
    };
    if !numeric {
        return None;
    }
    let b = s.as_bytes();
    let digits = |from: usize| from + b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int = digits(0);
    let mantissa = if b.first().is_some_and(|c| (b'1'..=b'9').contains(c)) {
        (b.get(int) == Some(&b'.')).then(|| digits(int + 1))
    } else {
        let dot = usize::from(b.first() == Some(&b'0'));
        (b.get(dot) == Some(&b'.') && b.get(dot + 1).is_some_and(u8::is_ascii_digit))
            .then(|| digits(dot + 1))
    };
    let len = match mantissa {
        Some(m) if matches!(b.get(m), Some(b'e' | b'E')) => {
            let sign = m + 1 + usize::from(matches!(b.get(m + 1), Some(b'+' | b'-')));
            match digits(sign) {
                end if end > sign => end,
                _ => m,
            }
        }
        Some(m) => m,
        None => int,
    };
    (0 < len && len < s.len()).then_some(len)
}

/// Maps an offset in the text [`match_nix_tokens`] returned to the source.
fn source_offset(inserted: &[u32], offset: u32) -> u32 {
    offset - inserted.partition_point(|&s| s < offset) as u32
}

/// The parts of an attribute set or `let` before compilation, with nested
/// attribute paths merged.
#[derive(Default)]
struct PreSet {
    statics: Vec<(Sym, u32, PreAttr)>,
    dynamics: Vec<(ast::Expr, u32, PreAttr)>,
    /// `inherit (from) names;` sources.
    froms: Vec<ast::Expr>,
}

enum PreAttr {
    Expr(ast::Expr),
    Nested(PreSet),
    /// `inherit name;`, resolved in the enclosing scope.
    Inherit(Sym, u32),
    /// `inherit (froms[i]) name;`
    InheritFrom(usize, Sym, u32),
}

enum AttrName {
    Static(Sym),
    Dynamic(ast::Expr),
}

fn offset(node: &impl AstNode<Language = rnix::NixLanguage>) -> u32 {
    u32::from(node.syntax().text_range().start())
}

impl<'a> Compiler<'a> {
    fn at(&self, offset: u32) -> Pos<'a> {
        Pos {
            source: self.source,
            offset: source_offset(&self.inserted, offset),
        }
    }

    fn pos(&self, node: &impl AstNode<Language = rnix::NixLanguage>) -> Pos<'a> {
        self.at(offset(node))
    }

    fn leak(&self, e: Expr<'a>) -> ExprRef<'a> {
        self.ctx.alloc(e)
    }

    fn lit_str(&self, b: &[u8]) -> &'a NixStr<'a> {
        self.ctx.alloc(NixStr {
            s: self.ctx.alloc_bytes(b),
            ctx: &[],
        })
    }

    fn name(&self, s: Sym) -> &'a str {
        self.ctx.name(s)
    }

    fn resolve(&self, name: Sym, skip: usize, pos: Pos<'a>) -> CResult<Expr<'a>> {
        let n = self.scopes.len();
        let mut withs = Vec::new();
        for (depth, scope) in self.scopes[..n - skip.min(n)].iter().rev().enumerate() {
            let depth = depth + skip;
            match scope {
                Scope::Names(names) => {
                    if let Some(&idx) = names.get(&name) {
                        return Ok(Expr::Var(depth as u32, idx));
                    }
                }
                Scope::With => withs.push(depth as u32),
            }
        }
        // The global scope is static, so `with` cannot shadow it.
        let s = &self.ctx.syms;
        if name == s.true_ {
            return Ok(Expr::Bool(true));
        } else if name == s.false_ {
            return Ok(Expr::Bool(false));
        } else if name == s.null {
            return Ok(Expr::Null);
        }
        if self.ctx.is_global(name) {
            return Ok(Expr::Global(name));
        }
        if withs.is_empty() {
            return err(format!("undefined variable '{}' at {pos}", self.name(name)));
        }
        Ok(Expr::WithVar {
            name,
            withs: self.ctx.alloc_slice(withs),
            global: false,
            pos,
        })
    }

    fn expr(&mut self, e: &ast::Expr) -> CResult<ExprRef<'a>> {
        let e = self.expr_inner(e)?;
        Ok(self.leak(e))
    }

    fn exprs(&mut self, es: &[ast::Expr]) -> CResult<&'a [ExprRef<'a>]> {
        let v = es
            .iter()
            .map(|e| self.expr(e))
            .collect::<CResult<Vec<_>>>()?;
        Ok(self.ctx.alloc_slice(v))
    }

    fn expr_inner(&mut self, e: &ast::Expr) -> CResult<Expr<'a>> {
        let pos = self.pos(e);
        Ok(match e {
            ast::Expr::Paren(p) => return self.expr_inner(&child(p.expr())?),
            ast::Expr::Root(r) => return self.expr_inner(&child(r.expr())?),
            ast::Expr::Error(_) => return err(format!("syntax error at {pos}")),
            ast::Expr::Literal(l) => match l.kind() {
                ast::LiteralKind::Integer(i) => match i.value() {
                    Ok(v) => Expr::Int(v),
                    Err(_) => {
                        return err(format!("invalid integer '{}' at {pos}", i.syntax().text()));
                    }
                },
                ast::LiteralKind::Float(f) => match parse_float(f.syntax().text()) {
                    Some(v) => Expr::Float(v),
                    None => {
                        return err(format!("invalid float '{}' at {pos}", f.syntax().text()));
                    }
                },
                ast::LiteralKind::Uri(u) => Expr::Str(self.lit_str(u.syntax().text().as_bytes())),
            },
            ast::Expr::Ident(i) => {
                let name = self.ident(i)?;
                if name == self.ctx.syms.cur_pos {
                    return Ok(Expr::CurPos(pos));
                }
                self.resolve(name, 0, pos)?
            }
            ast::Expr::CurPos(_) => Expr::CurPos(pos),
            ast::Expr::Str(s) => self.string(s, pos)?,
            ast::Expr::PathAbs(_) | ast::Expr::PathRel(_) | ast::Expr::PathHome(_) => {
                let p = ast::Path::cast(e.syntax().clone()).unwrap();
                self.path(&p, pos)?
            }
            ast::Expr::PathSearch(p) => {
                // `<name>` is `__findFile __nixPath "name"`, resolved lexically.
                let text = p.content().map(|c| c.text().to_owned()).unwrap_or_default();
                let name = text.trim_start_matches('<').trim_end_matches('>');
                let s = &self.ctx.syms;
                let find_file = self.resolve(s.find_file, 0, pos)?;
                let nix_path = self.resolve(s.nix_path, 0, pos)?;
                let args = vec![
                    self.leak(nix_path),
                    self.leak(Expr::Str(self.lit_str(name.as_bytes()))),
                ];
                Expr::Apply {
                    func: self.leak(find_file),
                    args: self.ctx.alloc_slice(args),
                    pos,
                }
            }
            ast::Expr::List(l) => {
                let items: Vec<ast::Expr> = l.items().collect();
                Expr::List(self.exprs(&items)?)
            }
            ast::Expr::Apply(a) => {
                // Flatten `f a b c` into one application node.
                let mut args = vec![child(a.argument())?];
                let mut func = child(a.lambda())?;
                while let ast::Expr::Apply(inner) = &func {
                    args.push(child(inner.argument())?);
                    func = child(inner.lambda())?;
                }
                args.reverse();
                Expr::Apply {
                    func: self.expr(&func)?,
                    args: self.exprs(&args)?,
                    pos,
                }
            }
            ast::Expr::Select(s) => {
                let expr = self.expr(&child(s.expr())?)?;
                let attrpath = child(s.attrpath())?;
                let path = self.attrpath(&attrpath)?;
                let default = match s.default_expr() {
                    Some(d) => Some(self.expr(&d)?),
                    None => None,
                };
                Expr::Select {
                    expr,
                    path,
                    default,
                    pos: self.pos(&attrpath),
                }
            }
            ast::Expr::HasAttr(h) => Expr::HasAttr {
                expr: self.expr(&child(h.expr())?)?,
                path: self.attrpath(&child(h.attrpath())?)?,
            },
            ast::Expr::IfElse(i) => Expr::If(
                self.expr(&child(i.condition())?)?,
                self.expr(&child(i.body())?)?,
                self.expr(&child(i.else_body())?)?,
            ),
            ast::Expr::Assert(a) => Expr::Assert {
                cond: self.expr(&child(a.condition())?)?,
                body: self.expr(&child(a.body())?)?,
                pos,
            },
            ast::Expr::With(w) => {
                let namespace = self.expr(&child(w.namespace())?)?;
                self.scopes.push(Scope::With);
                let body = child(w.body()).and_then(|b| self.expr(&b));
                self.scopes.pop();
                Expr::With {
                    namespace,
                    body: body?,
                }
            }
            ast::Expr::UnaryOp(u) => {
                let inner = self.expr(&child(u.expr())?)?;
                match u.operator() {
                    Some(ast::UnaryOpKind::Invert) => Expr::Not(inner, pos),
                    Some(ast::UnaryOpKind::Negate) => Expr::Neg(inner, pos),
                    None => return err(format!("syntax error: unknown unary operator at {pos}")),
                }
            }
            ast::Expr::BinOp(b) => self.binop(b, pos)?,
            ast::Expr::Lambda(l) => Expr::Lambda(self.lambda(l, None)?),
            ast::Expr::AttrSet(a) => {
                let set = self.preset(a.entries())?;
                if a.rec_token().is_some() {
                    self.rec_attrs(set)?
                } else {
                    self.attrs(set)?
                }
            }
            ast::Expr::LetIn(l) => {
                let set = self.preset(l.entries())?;
                if !set.dynamics.is_empty() {
                    return err(format!("dynamic attributes not allowed in let at {pos}"));
                }
                let ((slots, body), _) = self.frame(set, |c| c.expr(&child(l.body())?))?;
                Expr::Let(self.ctx.alloc(LetDef { slots, body }))
            }
            ast::Expr::LegacyLet(l) => {
                // `let { ...; body = e; }` is `rec { ...; body = e; }.body`.
                let set = self.preset(l.entries())?;
                let attrs = self.rec_attrs(set)?;
                let body = self.ctx.syms.body;
                Expr::Select {
                    expr: self.leak(attrs),
                    path: self.ctx.alloc_slice(vec![AttrKey::Static(body)]),
                    default: None,
                    pos,
                }
            }
        })
    }

    fn binop(&mut self, b: &ast::BinOp, pos: Pos<'a>) -> CResult<Expr<'a>> {
        let Some(op) = b.operator() else {
            return err(format!("syntax error: unknown operator at {pos}"));
        };
        let lhs = self.expr(&child(b.lhs())?)?;
        let rhs = self.expr(&child(b.rhs())?)?;
        let op_pos = b
            .syntax()
            .children_with_tokens()
            .filter_map(|t| t.into_token())
            .find(|t| ast::BinOpKind::from_kind(t.kind()).is_some())
            .map(|t| self.at(u32::from(t.text_range().start())))
            .unwrap_or(pos);
        let op = match op {
            ast::BinOpKind::Concat => BinOp::Concat,
            ast::BinOpKind::Update => BinOp::Update,
            ast::BinOpKind::Add => BinOp::Add,
            ast::BinOpKind::Sub => BinOp::Sub,
            ast::BinOpKind::Mul => BinOp::Mul,
            ast::BinOpKind::Div => BinOp::Div,
            ast::BinOpKind::And => BinOp::And,
            ast::BinOpKind::Equal => BinOp::Eq,
            ast::BinOpKind::Implication => BinOp::Impl,
            ast::BinOpKind::Less => BinOp::Lt,
            ast::BinOpKind::LessOrEq => BinOp::Le,
            ast::BinOpKind::More => BinOp::Gt,
            ast::BinOpKind::MoreOrEq => BinOp::Ge,
            ast::BinOpKind::NotEqual => BinOp::Neq,
            ast::BinOpKind::Or => BinOp::Or,
            ast::BinOpKind::PipeRight | ast::BinOpKind::PipeLeft => {
                return err(format!(
                    "experimental Nix feature 'pipe-operators' is disabled at {op_pos}"
                ));
            }
        };
        Ok(Expr::Bin(op, lhs, rhs, op_pos))
    }

    fn lambda(&mut self, l: &ast::Lambda, name: Option<Sym>) -> CResult<&'a LambdaDef<'a>> {
        let pos = self.pos(l);
        let (param, names) = match child(l.param())? {
            ast::Param::IdentParam(i) => {
                let n = self.ident(&child(i.ident())?)?;
                (Param::Ident(n), vec![n])
            }
            ast::Param::Pattern(p) => {
                let mut names = Vec::new();
                let mut entries = Vec::new();
                for entry in p.pat_entries() {
                    let n = self.ident(&child(entry.ident())?)?;
                    if names.contains(&n) {
                        return err(format!(
                            "duplicate formal function argument '{}' at {}",
                            self.name(n),
                            self.pos(&entry)
                        ));
                    }
                    names.push(n);
                    entries.push(entry);
                }
                let at = match p.pat_bind() {
                    Some(b) => {
                        let n = self.ident(&child(b.ident())?)?;
                        if names.contains(&n) {
                            return err(format!(
                                "duplicate formal function argument '{}' at {}",
                                self.name(n),
                                self.pos(&b)
                            ));
                        }
                        Some(n)
                    }
                    None => None,
                };
                let mut all = names.clone();
                all.extend(at);
                // Defaults see every formal and the `@` binding.
                self.scopes.push(scope_of(&all));
                let formals = entries
                    .iter()
                    .zip(&names)
                    .map(|(entry, &name)| {
                        Ok(Formal {
                            name,
                            default: match entry.default() {
                                Some(d) => Some(self.expr(&d)?),
                                None => None,
                            },
                        })
                    })
                    .collect::<CResult<Vec<_>>>();
                self.scopes.pop();
                (
                    Param::Pattern {
                        formals: self.ctx.alloc_slice(formals?),
                        ellipsis: p.ellipsis_token().is_some(),
                        at,
                    },
                    all,
                )
            }
        };
        self.scopes.push(scope_of(&names));
        let body = child(l.body()).and_then(|b| self.expr(&b));
        self.scopes.pop();
        Ok(self.ctx.alloc(LambdaDef {
            param,
            body: body?,
            pos,
            name,
        }))
    }

    fn string(&mut self, s: &ast::Str, pos: Pos<'a>) -> CResult<Expr<'a>> {
        let mut out = Vec::new();
        for part in str_parts(s) {
            match part {
                InterpolPart::Literal(l) => {
                    if !l.is_empty() {
                        out.push(self.leak(Expr::Str(self.lit_str(l.as_bytes()))));
                    }
                }
                InterpolPart::Interpolation(i) => out.push(self.expr(&child(i.expr())?)?),
            }
        }
        Ok(match out.as_slice() {
            [] => Expr::Str(self.lit_str(b"")),
            [Expr::Str(s)] => Expr::Str(s),
            _ => Expr::InterpStr(self.ctx.alloc_slice(out), pos),
        })
    }

    fn path(&mut self, p: &ast::Path, pos: Pos<'a>) -> CResult<Expr<'a>> {
        let mut out: Vec<ExprRef<'a>> = Vec::new();
        for (i, part) in p.parts().into_iter().enumerate() {
            match part {
                InterpolPart::Literal(l) => {
                    let text = l.text();
                    if i == 0 {
                        if l.is_home() && self.pure {
                            return err(format!(
                                "the path '{text}' can not be resolved in pure mode at {pos}"
                            ));
                        }
                        let resolved = self.resolve_path_literal(text, l.is_home());
                        let path = self.ctx.alloc(PathV(self.ctx.alloc_str(&resolved)));
                        out.push(self.leak(Expr::Path(path)));
                    } else {
                        out.push(self.leak(Expr::Str(self.lit_str(text.as_bytes()))));
                    }
                }
                InterpolPart::Interpolation(i) => out.push(self.expr(&child(i.expr())?)?),
            }
        }
        Ok(match out.as_slice() {
            [Expr::Path(p)] => Expr::Path(p),
            _ => Expr::InterpPath(self.ctx.alloc_slice(out), pos),
        })
    }

    fn resolve_path_literal(&self, text: &str, home: bool) -> String {
        // A literal prefix followed by interpolation keeps its trailing
        // slash, which canonicalisation must not drop: `./a/${x}`.
        let trailing = text.ends_with('/');
        let p = if home {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}{}", &text[1..])
        } else if text.starts_with('/') {
            text.to_owned()
        } else {
            format!("{}/{}", self.base_dir, text)
        };
        let mut p = canon_path(&p);
        if trailing && !p.ends_with('/') {
            p.push('/');
        }
        p
    }

    fn attrpath(&mut self, p: &ast::Attrpath) -> CResult<&'a [AttrKey<'a>]> {
        let keys = p
            .attrs()
            .map(|a| {
                Ok(match self.attr_name(&a)? {
                    AttrName::Static(s) => AttrKey::Static(s),
                    AttrName::Dynamic(e) => AttrKey::Dynamic(self.expr(&e)?),
                })
            })
            .collect::<CResult<Vec<_>>>()?;
        Ok(self.ctx.alloc_slice(keys))
    }

    fn attr_name(&self, a: &ast::Attr) -> CResult<AttrName> {
        Ok(match a {
            ast::Attr::Ident(i) => AttrName::Static(self.ident(i)?),
            // Nix's grammar only allows `"` strings as attribute names.
            ast::Attr::Str(s) if s.syntax().first_token().is_some_and(|t| t.text() == "''") => {
                return err(format!(
                    "syntax error: unexpected start of an indented string at {}",
                    self.pos(s)
                ));
            }
            ast::Attr::Str(s) => match static_str(s) {
                Some(lit) => AttrName::Static(self.ctx.intern(&lit)),
                None => AttrName::Dynamic(ast::Expr::Str(s.clone())),
            },
            ast::Attr::Dynamic(d) => {
                let inner = child(d.expr())?;
                // `${"lit"}` is a static name.
                if let ast::Expr::Str(s) = &inner
                    && let Some(lit) = static_str(s)
                {
                    return Ok(AttrName::Static(self.ctx.intern(&lit)));
                }
                AttrName::Dynamic(inner)
            }
        })
    }

    fn ident(&self, i: &ast::Ident) -> CResult<Sym> {
        let Some(tok) = i.syntax().first_token() else {
            return err("syntax error: empty identifier");
        };
        Ok(self.ctx.intern(tok.text()))
    }

    fn preset(&mut self, entries: impl Iterator<Item = ast::Entry>) -> CResult<PreSet> {
        let mut set = PreSet::default();
        for entry in entries {
            self.merge_entry(&mut set, entry)?;
        }
        Ok(set)
    }

    fn merge_entry(&self, set: &mut PreSet, entry: ast::Entry) -> CResult<()> {
        match entry {
            ast::Entry::AttrpathValue(av) => {
                let path = child(av.attrpath())?;
                let attrs: Vec<ast::Attr> = path.attrs().collect();
                self.insert_path(set, &attrs, child(av.value())?)
            }
            ast::Entry::Inherit(inh) => {
                let from = match inh.from() {
                    Some(f) => {
                        set.froms.push(child(f.expr())?);
                        Some(set.froms.len() - 1)
                    }
                    None => None,
                };
                for attr in inh.attrs() {
                    let off = offset(&attr);
                    let AttrName::Static(name) = self.attr_name(&attr)? else {
                        return err(format!(
                            "dynamic attributes not allowed in inherit at {}",
                            self.at(off)
                        ));
                    };
                    if set.statics.iter().any(|(n, _, _)| *n == name) {
                        return err(format!(
                            "attribute '{}' already defined at {}",
                            self.name(name),
                            self.at(off)
                        ));
                    }
                    let value = match from {
                        Some(i) => PreAttr::InheritFrom(i, name, off),
                        None => PreAttr::Inherit(name, off),
                    };
                    set.statics.push((name, off, value));
                }
                Ok(())
            }
        }
    }

    fn insert_path(&self, set: &mut PreSet, path: &[ast::Attr], value: ast::Expr) -> CResult<()> {
        let off = offset(&path[0]);
        let rest = &path[1..];
        let leaf = |c: &Self, value: ast::Expr| -> CResult<PreAttr> {
            if rest.is_empty() {
                Ok(PreAttr::Expr(value))
            } else {
                let mut nested = PreSet::default();
                c.insert_path(&mut nested, rest, value)?;
                Ok(PreAttr::Nested(nested))
            }
        };
        let name = match self.attr_name(&path[0])? {
            AttrName::Dynamic(e) => {
                let attr = leaf(self, value)?;
                set.dynamics.push((e, off, attr));
                return Ok(());
            }
            AttrName::Static(name) => name,
        };
        let Some((_, _, existing)) = set.statics.iter_mut().find(|(n, _, _)| *n == name) else {
            let attr = leaf(self, value)?;
            set.statics.push((name, off, attr));
            return Ok(());
        };
        let already = || {
            err(format!(
                "attribute '{}' already defined at {}",
                self.name(name),
                self.at(off)
            ))
        };
        // Two definitions merge only when both are plain attribute sets:
        // `a.b = 1; a.c = 2;` or `a = { b = 1; }; a.c = 2;`.
        if let PreAttr::Expr(ast::Expr::AttrSet(lit)) = existing
            && lit.rec_token().is_none()
        {
            let mut nested = PreSet::default();
            for entry in lit.entries() {
                self.merge_entry(&mut nested, entry)?;
            }
            *existing = PreAttr::Nested(nested);
        }
        let PreAttr::Nested(nested) = existing else {
            return already();
        };
        if !rest.is_empty() {
            return self.insert_path(nested, rest, value);
        }
        match value {
            ast::Expr::AttrSet(lit) if lit.rec_token().is_none() => {
                for entry in lit.entries() {
                    self.merge_entry(nested, entry)?;
                }
                Ok(())
            }
            _ => already(),
        }
    }

    /// Compile a value of a set. `from_base` is the frame slot of the set's
    /// first inherit-from source.
    fn pre_attr(
        &mut self,
        attr: PreAttr,
        name: Option<Sym>,
        from_base: Option<u32>,
    ) -> CResult<ExprRef<'a>> {
        Ok(match attr {
            PreAttr::Expr(ast::Expr::Lambda(l)) => {
                let def = self.lambda(&l, name)?;
                self.leak(Expr::Lambda(def))
            }
            PreAttr::Expr(e) => self.expr(&e)?,
            PreAttr::Nested(set) => {
                let e = self.attrs(set)?;
                self.leak(e)
            }
            PreAttr::Inherit(n, off) => {
                let e = self.resolve(n, 0, self.at(off))?;
                self.leak(e)
            }
            PreAttr::InheritFrom(i, n, off) => {
                let base = from_base.expect("inherit-from requires a frame");
                let var = self.leak(Expr::Var(0, base + i as u32));
                self.leak(Expr::Select {
                    expr: var,
                    path: self.ctx.alloc_slice(vec![AttrKey::Static(n)]),
                    default: None,
                    pos: self.at(off),
                })
            }
        })
    }

    /// A non-recursive attribute set.
    fn attrs(&mut self, set: PreSet) -> CResult<Expr<'a>> {
        let has_frame = !set.froms.is_empty();
        if has_frame {
            // A frame with no visible names holds the inherit-from sources so
            // that each is evaluated once.
            self.scopes.push(Scope::Names(HashMap::new()));
        }
        let result = self.attrs_body(set, has_frame);
        if has_frame {
            self.scopes.pop();
        }
        result
    }

    fn attrs_body(&mut self, set: PreSet, has_frame: bool) -> CResult<Expr<'a>> {
        let frame = if has_frame {
            let froms = set
                .froms
                .iter()
                .map(|f| Ok(SlotInit::Expr(self.expr(f)?)))
                .collect::<CResult<Vec<_>>>()?;
            Some(self.ctx.alloc_slice(froms))
        } else {
            None
        };
        let base = has_frame.then_some(0);
        let mut statics = Vec::new();
        for (name, off, attr) in set.statics {
            let value = AttrValue::Expr(self.pre_attr(attr, Some(name), base)?);
            statics.push(StaticAttr {
                name,
                pos: self.at(off),
                value,
            });
        }
        let mut dynamics = Vec::new();
        for (name, off, attr) in set.dynamics {
            dynamics.push(DynamicAttr {
                name: self.expr(&name)?,
                value: self.pre_attr(attr, None, base)?,
                pos: self.at(off),
            });
        }
        statics.sort_by_key(|s| s.name);
        Ok(Expr::Attrs(self.ctx.alloc(AttrsDef {
            frame,
            statics: self.ctx.alloc_slice(statics),
            dynamics: self.ctx.alloc_slice(dynamics),
        })))
    }

    /// Compile the bindings of a `rec` set or `let` into a frame: the static
    /// names in order, then the inherit-from sources. `body` is compiled with
    /// the frame in scope.
    #[allow(clippy::type_complexity)]
    fn frame<T>(
        &mut self,
        set: PreSet,
        body: impl FnOnce(&mut Self) -> CResult<T>,
    ) -> CResult<((&'a [SlotInit<'a>], T), Vec<(Sym, u32, u32)>)> {
        let names: Vec<Sym> = set.statics.iter().map(|(n, _, _)| *n).collect();
        self.scopes.push(scope_of(&names));
        let result = self.frame_body(set, body);
        self.scopes.pop();
        result
    }

    #[allow(clippy::type_complexity)]
    fn frame_body<T>(
        &mut self,
        set: PreSet,
        body: impl FnOnce(&mut Self) -> CResult<T>,
    ) -> CResult<((&'a [SlotInit<'a>], T), Vec<(Sym, u32, u32)>)> {
        let nstatic = set.statics.len() as u32;
        let mut slots = Vec::new();
        let mut slot_names = Vec::new();
        for (i, (name, off, attr)) in set.statics.into_iter().enumerate() {
            let init = match attr {
                // `inherit x;` refers to the enclosing scope, not the frame.
                PreAttr::Inherit(n, off) => {
                    let e = self.resolve(n, 1, self.at(off))?;
                    SlotInit::Expr(self.leak(e))
                }
                PreAttr::InheritFrom(f, n, off) => {
                    SlotInit::Select(nstatic + f as u32, n, self.at(off))
                }
                other => SlotInit::Expr(self.pre_attr(other, Some(name), Some(nstatic))?),
            };
            slots.push(init);
            slot_names.push((name, off, i as u32));
        }
        for from in &set.froms {
            slots.push(SlotInit::Expr(self.expr(from)?));
        }
        let b = body(self)?;
        Ok(((self.ctx.alloc_slice(slots), b), slot_names))
    }

    fn rec_attrs(&mut self, mut set: PreSet) -> CResult<Expr<'a>> {
        let dynamics = std::mem::take(&mut set.dynamics);
        let ((slots, dynamics), names) = self.frame(set, |c| {
            dynamics
                .into_iter()
                .map(|(name, off, attr)| {
                    Ok(DynamicAttr {
                        name: c.expr(&name)?,
                        value: c.pre_attr(attr, None, None)?,
                        pos: c.at(off),
                    })
                })
                .collect::<CResult<Vec<_>>>()
        })?;
        let mut statics: Vec<_> = names
            .into_iter()
            .map(|(name, off, slot)| StaticAttr {
                name,
                pos: self.at(off),
                value: AttrValue::Slot(slot),
            })
            .collect();
        statics.sort_by_key(|s| s.name);
        Ok(Expr::Attrs(self.ctx.alloc(AttrsDef {
            frame: Some(slots),
            statics: self.ctx.alloc_slice(statics),
            dynamics: self.ctx.alloc_slice(dynamics),
        })))
    }
}

fn scope_of(names: &[Sym]) -> Scope {
    Scope::Names(
        names
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, i as u32))
            .collect(),
    )
}

/// The literal text and interpolations of a string, with escapes resolved.
/// rnix keeps a raw `\r\n` or `\r` in a `"` string, but Nix reads each as
/// `\n`; an escaped `\r` is kept, as are both in an indented string.
fn str_parts(s: &ast::Str) -> Vec<InterpolPart<String>> {
    let indented = s.syntax().first_token().is_some_and(|t| t.text() == "''");
    if indented {
        return s.normalized_parts();
    }
    s.parts()
        .map(|part| match part {
            InterpolPart::Literal(l) => InterpolPart::Literal(unescape_str(l.syntax().text())),
            InterpolPart::Interpolation(i) => InterpolPart::Interpolation(i),
        })
        .collect()
}

/// Resolves the escapes in the text of a `"` string literal.
fn unescape_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(c) => out.push(c),
                None => {}
            },
            '\r' => {
                out.push('\n');
                if chars.as_str().starts_with('\n') {
                    chars.next();
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The text of a string literal with no interpolation.
fn static_str(s: &ast::Str) -> Option<String> {
    let mut lit = String::new();
    for part in str_parts(s) {
        match part {
            InterpolPart::Literal(l) => lit.push_str(&l),
            InterpolPart::Interpolation(_) => return None,
        }
    }
    Some(lit)
}

fn child<T>(o: Option<T>) -> CResult<T> {
    o.ok_or_else(|| CompileError {
        msg: "syntax error: incomplete expression".into(),
    })
}

/// Parses a float literal as Nix does with glibc's `strtod`, rejecting a
/// literal whose value overflows to infinity or underflows.
fn parse_float(text: &str) -> Option<f64> {
    let v: f64 = text.parse().ok()?;
    if v.is_infinite() || (v <= f64::MIN_POSITIVE && underflows(text)) {
        return None;
    }
    Some(v)
}

// Whether glibc detects tininess after rounding on this architecture, as
// its sysdeps/*/tininess.h say; on the others it detects it before.
const TININESS_AFTER_ROUNDING: bool = cfg!(any(
    target_arch = "x86",
    target_arch = "x86_64",
    target_arch = "riscv32",
    target_arch = "riscv64",
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "loongarch64",
    target_arch = "csky",
));

/// Whether glibc's `strtod` reports a range error for the literal `text`:
/// its value is tiny (below the smallest normal double) and not exactly
/// representable. Where tininess is detected after rounding, a value that
/// rounds to the smallest normal double at full precision is not tiny.
fn underflows(text: &str) -> bool {
    let Some((digits, scale)) = decimal(text) else {
        return true;
    };
    if digits.is_empty() {
        return false;
    }
    // Below 10^-330 the value is under half the smallest subnormal.
    if digits.len() as i64 + scale < -330 {
        return true;
    }
    if scale >= 0 {
        return false;
    }
    // The value is d / 10^e; compare it against powers of two exactly.
    let d: BigUint = digits.parse().expect("decimal digits");
    let ten_e = BigUint::from(10u32).pow((-scale) as u32);
    if (&d << 1022u32) >= ten_e {
        return false;
    }
    if (&d << 1074u32) % &ten_e == BigUint::ZERO {
        return false;
    }
    // 2^-1022 - 2^-1076 is halfway to the next double below at full
    // precision, and rounds to even, up to 2^-1022.
    !(TININESS_AFTER_ROUNDING && (&d << 1076u32) >= BigUint::from((1u64 << 54) - 1) * &ten_e)
}

/// A decimal literal as significant digits and the power of ten they are
/// scaled by, `None` for an exponent out of range.
fn decimal(text: &str) -> Option<(String, i64)> {
    let (mantissa, exp) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let significant = digits.trim_end_matches('0');
    if significant.is_empty() {
        return Some((String::new(), 0));
    }
    let scale =
        exp.parse::<i64>().ok()? - frac.len() as i64 + (digits.len() - significant.len()) as i64;
    Some((significant.to_owned(), scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_float_smallest_normal() {
        assert_eq!(
            parse_float("2.2250738585072014e-308"),
            Some(f64::MIN_POSITIVE)
        );
        // Tiny, and rounds to the smallest normal only at subnormal precision.
        assert_eq!(parse_float("2.2250738585072012e-308"), None);
        // Tiny before rounding, but rounds to the smallest normal at full
        // precision; 2^-1022 - 2^-1076 is the boundary.
        let after = TININESS_AFTER_ROUNDING.then_some(f64::MIN_POSITIVE);
        assert_eq!(parse_float("2.2250738585072013e-308"), after);
        assert_eq!(parse_float("2.225073858507201259574e-308"), after);
        assert_eq!(parse_float("2.225073858507201259573e-308"), None);
    }

    #[test]
    fn parse_float_exact_subnormal() {
        assert_eq!(parse_float("0.0e-999"), Some(0.0));
        assert_eq!(parse_float("1.0e-320"), None);
        let min = format!("{:.1100e}", f64::from_bits(1));
        assert_eq!(parse_float(&min), Some(f64::from_bits(1)));
    }
}

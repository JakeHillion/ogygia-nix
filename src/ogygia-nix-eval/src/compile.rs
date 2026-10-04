//! Lowering of the rnix syntax tree into [`crate::ir`].

use std::collections::HashMap;

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
    let parse = rnix::Root::parse(source.text);
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
            .map(|r| {
                format!(
                    " at {}",
                    Pos {
                        source,
                        offset: u32::from(r.start()),
                    }
                )
            })
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
            Pos {
                source,
                offset: u32::from(t.text_range().start()),
            }
        ));
    }
    let root = parse.tree();
    let expr = child(root.expr())?;
    let mut c = Compiler {
        ctx,
        source,
        base_dir: ctx.alloc_str(base_dir),
        pure,
        scopes: Vec::new(),
    };
    if let Some(names) = extra_scope {
        c.scopes.push(scope_of(names));
    }
    c.expr(&expr)
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
            offset,
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
                ast::LiteralKind::Float(f) => match f.value() {
                    Ok(v) => Expr::Float(v),
                    Err(_) => return err(format!("invalid float at {pos}")),
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
            ast::BinOpKind::PipeRight => {
                return Ok(Expr::Apply {
                    func: rhs,
                    args: self.ctx.alloc_slice(vec![lhs]),
                    pos: op_pos,
                });
            }
            ast::BinOpKind::PipeLeft => {
                return Ok(Expr::Apply {
                    func: lhs,
                    args: self.ctx.alloc_slice(vec![rhs]),
                    pos: op_pos,
                });
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
        for part in s.normalized_parts() {
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

/// The text of a string literal with no interpolation.
fn static_str(s: &ast::Str) -> Option<String> {
    let mut lit = String::new();
    for part in s.normalized_parts() {
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

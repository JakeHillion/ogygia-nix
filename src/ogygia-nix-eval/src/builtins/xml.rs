//! `toXML`.

use std::collections::HashSet;

use crate::eval::Eval;
use crate::ir::Param;
use crate::print::float_g;
use crate::value::Attrs;
use crate::value::Ctx;
use crate::value::R;
use crate::value::Value;

pub fn to_xml<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let mut w = Writer {
        out: b"<?xml version='1.0' encoding='utf-8'?>\n".to_vec(),
        depth: 0,
        ctx: Vec::new(),
        drvs_seen: HashSet::new(),
    };
    w.open("expr", &[]);
    w.value(ev, args[0])?;
    w.close("expr");
    Ok(ev.str_with_ctx(&w.out, w.ctx))
}

struct Writer<'a> {
    out: Vec<u8>,
    depth: usize,
    ctx: Vec<Ctx<'a>>,
    drvs_seen: HashSet<&'a [u8]>,
}

impl<'a> Writer<'a> {
    fn start(&mut self, name: &str, attrs: &[(&str, &[u8])]) {
        self.out.resize(self.out.len() + self.depth * 2, b' ');
        self.out.push(b'<');
        self.out.extend_from_slice(name.as_bytes());
        for (k, v) in attrs {
            self.out.push(b' ');
            self.out.extend_from_slice(k.as_bytes());
            self.out.extend_from_slice(b"=\"");
            for &c in *v {
                match c {
                    b'"' => self.out.extend_from_slice(b"&quot;"),
                    b'<' => self.out.extend_from_slice(b"&lt;"),
                    b'>' => self.out.extend_from_slice(b"&gt;"),
                    b'&' => self.out.extend_from_slice(b"&amp;"),
                    b'\n' => self.out.extend_from_slice(b"&#xA;"),
                    c => self.out.push(c),
                }
            }
            self.out.push(b'"');
        }
    }

    /// `attrs` must be sorted by name.
    fn open(&mut self, name: &str, attrs: &[(&str, &[u8])]) {
        self.start(name, attrs);
        self.out.extend_from_slice(b">\n");
        self.depth += 1;
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.out.resize(self.out.len() + self.depth * 2, b' ');
        self.out.extend_from_slice(b"</");
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(b">\n");
    }

    /// `attrs` must be sorted by name.
    fn empty(&mut self, name: &str, attrs: &[(&str, &[u8])]) {
        self.start(name, attrs);
        self.out.extend_from_slice(b" />\n");
    }

    fn value(&mut self, ev: &Eval<'a>, v: Value<'a>) -> R<'a, ()> {
        match ev.force(v)? {
            Value::Int(i) => self.empty("int", &[("value", i.to_string().as_bytes())]),
            Value::Bool(b) => self.empty("bool", &[("value", if b { b"true" } else { b"false" })]),
            Value::Str(s) => {
                self.ctx.extend_from_slice(s.ctx);
                self.empty("string", &[("value", s.s)]);
            }
            Value::Path(p) => self.empty("path", &[("value", p.0.as_bytes())]),
            Value::Null => self.empty("null", &[]),
            Value::Float(f) => self.empty("float", &[("value", float_g(f).as_bytes())]),
            Value::Attrs(a) if ev.is_derivation(a)? => {
                let syms = &ev.ctx.syms;
                let mut drv_path: &[u8] = b"";
                let mut attrs = Vec::new();
                for (key, sym) in [("drvPath", syms.drv_path), ("outPath", syms.out_path)] {
                    if let Some(p) = a.get(sym)
                        && let Value::Str(s) = ev.force(p)?
                    {
                        attrs.push((key, s.s));
                        if key == "drvPath" {
                            drv_path = s.s;
                        }
                    }
                }
                self.open("derivation", &attrs);
                if !drv_path.is_empty() && self.drvs_seen.insert(drv_path) {
                    self.attrs(ev, a)?;
                } else {
                    self.empty("repeated", &[]);
                }
                self.close("derivation");
            }
            Value::Attrs(a) => {
                self.open("attrs", &[]);
                self.attrs(ev, a)?;
                self.close("attrs");
            }
            Value::List(l) => {
                self.open("list", &[]);
                for &item in l.items {
                    self.value(ev, item)?;
                }
                self.close("list");
            }
            Value::Lambda(c) => {
                self.open("function", &[]);
                match &c.def.param {
                    Param::Ident(x) => self.empty("varpat", &[("name", ev.name(*x).as_bytes())]),
                    Param::Pattern {
                        formals,
                        ellipsis,
                        at,
                    } => {
                        let mut attrs: Vec<(&str, &[u8])> = Vec::new();
                        if *ellipsis {
                            attrs.push(("ellipsis", b"1"));
                        }
                        if let Some(at) = at {
                            attrs.push(("name", ev.name(*at).as_bytes()));
                        }
                        self.open("attrspat", &attrs);
                        let mut names: Vec<&str> =
                            formals.iter().map(|f| ev.name(f.name)).collect();
                        names.sort_unstable();
                        for name in names {
                            self.empty("attr", &[("name", name.as_bytes())]);
                        }
                        self.close("attrspat");
                    }
                }
                self.close("function");
            }
            Value::PrimOp(_) | Value::PrimOpApp(_) => self.empty("unevaluated", &[]),
            Value::Thunk(_) => unreachable!("forced"),
        }
        Ok(())
    }

    fn attrs(&mut self, ev: &Eval<'a>, a: &'a Attrs<'a>) -> R<'a, ()> {
        for e in a.sorted(ev.ctx) {
            self.open("attr", &[("name", ev.name(e.name).as_bytes())]);
            self.value(ev, e.value)?;
            self.close("attr");
        }
        Ok(())
    }
}

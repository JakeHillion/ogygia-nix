//! `toJSON` and `fromJSON`.

use crate::eval::Coerce;
use crate::eval::Eval;
use crate::value::Ctx;
use crate::value::Entry;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

pub fn to_json<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let mut out = Vec::new();
    let mut ctx = Vec::new();
    write_json(ev, args[0], &mut out, &mut ctx)?;
    Ok(ev.str_with_ctx(&out, ctx))
}

pub fn json_string(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &c in s {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            c if c < 0x20 => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
            c => out.push(c),
        }
    }
    out.push(b'"');
}

/// A float the way Nix's JSON writer prints it: the shortest representation
/// that round-trips, always with a fraction or exponent.
pub fn json_float(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_owned();
    }
    let s = format!("{f:?}");
    // Rust prints `1e20` as `1e20`; JSON writers print `1e+20`.
    if let Some((m, e)) = s.split_once('e') {
        let (sign, digits) = match e.strip_prefix('-') {
            Some(d) => ('-', d),
            None => ('+', e),
        };
        let m = m.strip_suffix(".0").unwrap_or(m);
        return format!("{m}e{sign}{digits:0>2}");
    }
    s
}

pub fn write_json<'a>(
    ev: &Eval<'a>,
    v: Value<'a>,
    out: &mut Vec<u8>,
    ctx: &mut Vec<Ctx<'a>>,
) -> R<'a, ()> {
    let v = ev.force(v)?;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Float(f) => out.extend_from_slice(json_float(f).as_bytes()),
        Value::Str(s) => {
            json_string(out, s.s);
            ctx.extend_from_slice(s.ctx);
        }
        Value::Path(_) => {
            let (s, c) = ev.coerce_to_string(v, Coerce::INTERP)?;
            json_string(out, &s);
            ctx.extend(c);
        }
        Value::Attrs(a) => {
            let s = &ev.ctx.syms;
            if a.get(s.to_string).is_some() {
                let (st, c) = ev.coerce_to_string(v, Coerce::INTERP)?;
                json_string(out, &st);
                ctx.extend(c);
                return Ok(());
            }
            if let Some(p) = a.get(s.out_path) {
                return write_json(ev, p, out, ctx);
            }
            out.push(b'{');
            for (i, e) in a.sorted(ev.ctx).iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                json_string(out, ev.name(e.name).as_bytes());
                out.push(b':');
                write_json(ev, e.value, out, ctx)?;
            }
            out.push(b'}');
        }
        Value::List(l) => {
            out.push(b'[');
            for (i, item) in l.items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_json(ev, *item, out, ctx)?;
            }
            out.push(b']');
        }
        other => {
            return eval_err(format!("cannot convert {} to JSON", other.show_type()));
        }
    }
    Ok(())
}

pub fn from_json<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str(args[0])?;
    let parsed: serde_json::Value = match serde_json::from_slice(s.s) {
        Ok(v) => v,
        Err(e) => return eval_err(format!("JSON parse error: {e}")),
    };
    json_to_value(ev, &parsed)
}

pub fn json_to_value<'a>(ev: &Eval<'a>, j: &serde_json::Value) -> R<'a> {
    Ok(match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => Value::Int(i),
            (None, Some(u)) => {
                return eval_err(format!(
                    "unsigned json number {u} outside of Nix integer range"
                ));
            }
            (None, None) => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(s) => ev.string(s),
        serde_json::Value::Array(items) => {
            let items = items
                .iter()
                .map(|i| json_to_value(ev, i))
                .collect::<R<'a, Vec<_>>>()?;
            ev.list(&items)
        }
        serde_json::Value::Object(map) => ev.attrs(
            map.iter()
                .map(|(k, v)| {
                    Ok(Entry {
                        name: ev.sym(k),
                        value: json_to_value(ev, v)?,
                        pos: None,
                    })
                })
                .collect::<R<'a, Vec<_>>>()?,
        ),
    })
}

pub fn from_toml<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str(args[0])?;
    let text = String::from_utf8_lossy(s.s);
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => return eval_err(format!("while parsing TOML: {e}")),
    };
    toml_to_value(ev, &toml::Value::Table(table))
}

fn toml_to_value<'a>(ev: &Eval<'a>, t: &toml::Value) -> R<'a> {
    Ok(match t {
        toml::Value::String(s) => ev.string(s),
        toml::Value::Integer(i) => Value::Int(*i),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(_) => {
            return eval_err("while parsing TOML: dates and times are not supported");
        }
        toml::Value::Array(items) => {
            let items = items
                .iter()
                .map(|i| toml_to_value(ev, i))
                .collect::<R<'a, Vec<_>>>()?;
            ev.list(&items)
        }
        toml::Value::Table(map) => ev.attrs(
            map.iter()
                .map(|(k, v)| {
                    Ok(Entry {
                        name: ev.sym(k),
                        value: toml_to_value(ev, v)?,
                        pos: None,
                    })
                })
                .collect::<R<'a, Vec<_>>>()?,
        ),
    })
}

//! Rendering values the way `nix-instantiate --eval --strict` does.

use std::fmt::Write;

use crate::eval::Eval;
use crate::value::R;
use crate::value::Value;

/// C's `%g`: six significant digits, trailing zeros removed.
pub fn float_g(f: f64) -> String {
    if f.is_nan() {
        return if f.is_sign_negative() { "-nan" } else { "nan" }.to_owned();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    const P: i32 = 6;
    let sci = format!("{:.*e}", (P - 1) as usize, f);
    let (mantissa, exp) = sci.split_once('e').expect("exponent format");
    let exp: i32 = exp.parse().expect("exponent is an integer");
    if !(-4..P).contains(&exp) {
        let mantissa = strip_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (P - 1 - exp) as usize;
        strip_zeros(&format!("{f:.decimals$}")).to_owned()
    }
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// C's `%f`, used by `toString` on floats.
pub fn float_to_string(f: f64) -> String {
    if f.is_nan() {
        return if f.is_sign_negative() { "-nan" } else { "nan" }.to_owned();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    format!("{f:.6}")
}

const KEYWORDS: &[&str] = &[
    "if", "then", "else", "assert", "with", "let", "in", "rec", "inherit",
];

/// Whether `name` can be written as a bare attribute name.
pub fn is_plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || "_'-".contains(c))
        && !KEYWORDS.contains(&name)
}

pub fn escape_string(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'$' if s.get(i + 1) == Some(&b'{') => out.extend_from_slice(b"\\$"),
            c => out.push(c),
        }
        i += 1;
    }
    out.push(b'"');
}

fn attr_name(out: &mut Vec<u8>, name: &str) {
    if is_plain_ident(name) {
        out.extend_from_slice(name.as_bytes());
    } else {
        escape_string(out, name.as_bytes());
    }
}

/// Deeply force and print `v`.
pub fn print_strict<'a>(ev: &Eval<'a>, v: Value<'a>) -> R<'a, Vec<u8>> {
    let mut out = Vec::new();
    print_value(ev, v, &mut out)?;
    Ok(out)
}

fn print_value<'a>(ev: &Eval<'a>, v: Value<'a>, out: &mut Vec<u8>) -> R<'a, ()> {
    let v = ev.force(v)?;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Float(f) => out.extend_from_slice(float_g(f).as_bytes()),
        Value::Str(s) => escape_string(out, s.s),
        Value::Path(p) => out.extend_from_slice(p.0.as_bytes()),
        Value::Attrs(a) => {
            out.extend_from_slice(b"{ ");
            for e in a.sorted(ev.ctx) {
                attr_name(out, ev.name(e.name));
                out.extend_from_slice(b" = ");
                print_value(ev, e.value, out)?;
                out.extend_from_slice(b"; ");
            }
            out.push(b'}');
        }
        Value::List(l) => {
            out.extend_from_slice(b"[ ");
            for item in l.items {
                print_value(ev, *item, out)?;
                out.push(b' ');
            }
            out.push(b']');
        }
        Value::Lambda(_) => out.extend_from_slice(b"<LAMBDA>"),
        Value::PrimOp(_) => out.extend_from_slice(b"<PRIMOP>"),
        Value::PrimOpApp(_) => out.extend_from_slice(b"<PRIMOP-APP>"),
        Value::Thunk(_) => unreachable!("forced above"),
    }
    Ok(())
}

/// A short rendering for error messages; never forces anything.
pub fn short<'a>(ev: &Eval<'a>, v: Value<'a>) -> String {
    let mut s = String::new();
    short_into(ev, v, &mut s, 2);
    s
}

fn short_into<'a>(ev: &Eval<'a>, v: Value<'a>, s: &mut String, depth: usize) {
    match v {
        Value::Null => s.push_str("null"),
        Value::Bool(b) => s.push_str(if b { "true" } else { "false" }),
        Value::Int(i) => write!(s, "{i}").unwrap(),
        Value::Float(f) => s.push_str(&float_g(f)),
        Value::Str(st) => {
            let mut b = Vec::new();
            let text = st.s;
            escape_string(&mut b, &text[..text.len().min(100)]);
            s.push_str(&String::from_utf8_lossy(&b));
        }
        Value::Path(p) => s.push_str(p.0),
        Value::Attrs(a) => {
            if depth == 0 {
                s.push_str("{ ... }");
                return;
            }
            s.push_str("{ ");
            for e in a.sorted(ev.ctx).iter().take(10) {
                s.push_str(ev.name(e.name));
                s.push_str(" = ");
                short_into(ev, e.value, s, depth - 1);
                s.push_str("; ");
            }
            if a.len() > 10 {
                s.push_str("... ");
            }
            s.push('}');
        }
        Value::List(l) => {
            if depth == 0 {
                s.push_str("[ ... ]");
                return;
            }
            s.push_str("[ ");
            for item in l.items.iter().take(10) {
                short_into(ev, *item, s, depth - 1);
                s.push(' ');
            }
            if l.items.len() > 10 {
                s.push_str("... ");
            }
            s.push(']');
        }
        Value::Lambda(_) => s.push_str("«lambda»"),
        Value::PrimOp(p) => write!(s, "«primop {}»", p.name).unwrap(),
        Value::PrimOpApp(p) => write!(s, "«partially applied primop {}»", p.op.name).unwrap(),
        Value::Thunk(t) => match t.0.get() {
            crate::value::ThunkState::Done(v) => short_into(ev, v, s, depth),
            _ => s.push_str("«thunk»"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_format() {
        assert_eq!(float_g(1.0), "1");
        assert_eq!(float_g(0.1), "0.1");
        assert_eq!(float_g(1e20), "1e+20");
        assert_eq!(float_g(123456789.123), "1.23457e+08");
        assert_eq!(float_g(1e-5), "1e-05");
        assert_eq!(float_g(0.0001), "0.0001");
        assert_eq!(float_g(1234567.0), "1.23457e+06");
        assert_eq!(float_g(2.5), "2.5");
    }
}

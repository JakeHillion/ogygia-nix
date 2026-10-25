//! String builtins.

use std::cmp::Ordering;

use crate::eval::Coerce;
use crate::eval::Eval;
use crate::store::HashAlgo;
use crate::value::R;
use crate::value::Value;
use crate::value::anyhow_err;
use crate::value::eval_err;

pub fn to_string<'a>(ev: &Eval<'a>, v: Value<'a>) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(
        v,
        Coerce {
            more: true,
            copy: false,
        },
    )?;
    Ok(ev.str_with_ctx(&s, ctx))
}

pub fn string_length<'a>(ev: &Eval<'a>, s: Value<'a>) -> R<'a> {
    let (s, _) = ev.coerce_to_string(s, Coerce::INTERP)?;
    Ok(Value::Int(s.len() as i64))
}

pub fn substring<'a>(ev: &Eval<'a>, start: Value<'a>, len: Value<'a>, s: Value<'a>) -> R<'a> {
    let start = ev.force_int(start)?;
    if start < 0 {
        return eval_err("negative start position in 'substring'");
    }
    let len = ev.force_int(len)?;
    let (s, ctx) = ev.coerce_to_string(s, Coerce::INTERP)?;
    let start = (start as usize).min(s.len());
    let end = if len < 0 {
        s.len()
    } else {
        start.saturating_add(len as usize).min(s.len())
    };
    Ok(ev.str_with_ctx(&s[start..end], ctx))
}

pub fn concat_strings_sep<'a>(ev: &Eval<'a>, sep: Value<'a>, list: Value<'a>) -> R<'a> {
    let sep = ev.force_str(sep)?;
    let list = ev.force_list(list)?;
    let mut buf = Vec::new();
    let mut ctx = sep.ctx.to_vec();
    for (i, item) in list.iter().enumerate() {
        if i > 0 {
            buf.extend_from_slice(sep.s);
        }
        ev.coerce_into(*item, Coerce::INTERP, &mut buf, &mut ctx)?;
    }
    Ok(ev.str_with_ctx(&buf, ctx))
}

pub fn base_name_of<'a>(ev: &Eval<'a>, s: Value<'a>) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(s, Coerce::PLAIN)?;
    Ok(ev.str_with_ctx(crate::path::base_name_of(&s), ctx))
}

pub fn dir_of<'a>(ev: &Eval<'a>, v: Value<'a>) -> R<'a> {
    match ev.force(v)? {
        Value::Path(p) => Ok(ev.path_val(crate::path::dir_of(p.0))),
        v => {
            let (s, ctx) = ev.coerce_to_string(v, Coerce::PLAIN)?;
            let dir: &[u8] = match s.iter().rposition(|&b| b == b'/') {
                Some(0) => b"/",
                Some(i) => &s[..i],
                None => b".",
            };
            Ok(ev.str_with_ctx(dir, ctx))
        }
    }
}

/// The hash algorithm named `algo`.
pub fn hash_algo<'a>(algo: &[u8]) -> R<'a, HashAlgo> {
    String::from_utf8_lossy(algo).parse().map_err(anyhow_err)
}

pub fn hash_string<'a>(ev: &Eval<'a>, algo: Value<'a>, s: Value<'a>) -> R<'a> {
    let algo = hash_algo(ev.force_str_no_ctx(algo)?)?;
    let s = ev.force_str(s)?;
    Ok(ev.string(&hex::encode(algo.digest(s.s))))
}

pub fn replace_strings<'a>(ev: &Eval<'a>, from: Value<'a>, to: Value<'a>, s: Value<'a>) -> R<'a> {
    let from = ev.force_list(from)?;
    let to = ev.force_list(to)?;
    if from.len() != to.len() {
        return eval_err(
            "'from' and 'to' arguments passed to builtins.replaceStrings have different lengths",
        );
    }
    let from: Vec<&[u8]> = from
        .iter()
        .map(|f| Ok(ev.force_str(*f)?.s))
        .collect::<R<'a, _>>()?;
    let mut to_cache: Vec<Option<(&'a [u8], &'a [crate::value::Ctx<'a>])>> = vec![None; to.len()];
    let s = ev.force_str(s)?;
    let mut out = Vec::with_capacity(s.s.len());
    let mut ctx = s.ctx.to_vec();
    let mut i = 0;
    while i <= s.s.len() {
        let mut matched = false;
        for (j, f) in from.iter().enumerate() {
            if s.s[i..].starts_with(f) {
                let (rep, rep_ctx) = match to_cache[j] {
                    Some(r) => r,
                    None => {
                        let t = ev.force_str(to[j])?;
                        to_cache[j] = Some((t.s, t.ctx));
                        (t.s, t.ctx)
                    }
                };
                out.extend_from_slice(rep);
                ctx.extend_from_slice(rep_ctx);
                matched = true;
                if f.is_empty() {
                    if i < s.s.len() {
                        out.push(s.s[i]);
                    }
                    i += 1;
                } else {
                    i += f.len();
                }
                break;
            }
        }
        if !matched {
            if i < s.s.len() {
                out.push(s.s[i]);
            }
            i += 1;
        }
    }
    Ok(ev.str_with_ctx(&out, ctx))
}

/// Components of a version string: runs of digits or of other characters,
/// separated by `.` and `-`.
fn version_components(v: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i] == b'.' || v[i] == b'-' {
            i += 1;
            continue;
        }
        let start = i;
        if v[i].is_ascii_digit() {
            while i < v.len() && v[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            while i < v.len() && !v[i].is_ascii_digit() && v[i] != b'.' && v[i] != b'-' {
                i += 1;
            }
        }
        out.push(&v[start..i]);
    }
    out
}

fn component_lt(a: &[u8], b: &[u8]) -> bool {
    // Nix only treats a component as a number if it fits a C++ `int`; a
    // longer run of digits compares as a non-numeric component.
    let num = |c: &[u8]| -> Option<i32> {
        if !c.is_empty() && c.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(c).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (an, bn) = (num(a), num(b));
    match (an, bn) {
        (Some(x), Some(y)) => x < y,
        _ if a.is_empty() && bn.is_some() => true,
        _ if a == b"pre" && b != b"pre" => true,
        _ if b == b"pre" => false,
        (_, Some(_)) => true,
        (Some(_), _) => false,
        _ => a < b,
    }
}

pub fn compare_version_strings(a: &[u8], b: &[u8]) -> Ordering {
    let ca = version_components(a);
    let cb = version_components(b);
    for i in 0..ca.len().max(cb.len()) {
        let x = ca.get(i).copied().unwrap_or(b"");
        let y = cb.get(i).copied().unwrap_or(b"");
        if component_lt(x, y) {
            return Ordering::Less;
        }
        if component_lt(y, x) {
            return Ordering::Greater;
        }
    }
    Ordering::Equal
}

pub fn compare_versions<'a>(ev: &Eval<'a>, a: Value<'a>, b: Value<'a>) -> R<'a> {
    let a = ev.force_str_no_ctx(a)?;
    let b = ev.force_str_no_ctx(b)?;
    Ok(Value::Int(match compare_version_strings(a, b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

pub fn split_version<'a>(ev: &Eval<'a>, s: Value<'a>) -> R<'a> {
    let s = ev.force_str_no_ctx(s)?;
    let parts: Vec<Value<'a>> = version_components(s)
        .into_iter()
        .map(|c| ev.str_val(c, &[]))
        .collect();
    Ok(ev.list(&parts))
}

pub fn parse_drv_name<'a>(ev: &Eval<'a>, s: Value<'a>) -> R<'a> {
    let s = ev.force_str_no_ctx(s)?;
    let split = (0..s.len())
        .find(|&i| s[i] == b'-' && s.get(i + 1).is_some_and(|c| !c.is_ascii_alphabetic()));
    let (name, version): (&[u8], &[u8]) = match split {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, b""),
    };
    Ok(ev.attrs(vec![
        ev.entry("name", ev.str_val(name, &[])),
        ev.entry("version", ev.str_val(version, &[])),
    ]))
}

pub fn convert_hash<'a>(ev: &Eval<'a>, set: Value<'a>) -> R<'a> {
    let set = ev.force_attrs(set)?;
    let get = |n: &str| -> R<'a, Option<String>> {
        match set.get(ev.sym(n)) {
            Some(v) => Ok(Some(
                String::from_utf8_lossy(ev.force_str_no_ctx(v)?).into_owned(),
            )),
            None => Ok(None),
        }
    };
    let Some(hash) = get("hash")? else {
        return eval_err("attribute 'hash' missing in a call to 'convertHash'");
    };
    let algo = get("hashAlgo")?
        .map(|a| hash_algo(a.as_bytes()))
        .transpose()?;
    let Some(format) = get("toHashFormat")? else {
        return eval_err("attribute 'toHashFormat' missing in a call to 'convertHash'");
    };
    let result = format.parse().and_then(|format| {
        let (algo, bytes) = crate::store::parse_hash(&hash, algo)?;
        Ok(crate::store::format_hash(algo, &bytes, format))
    });
    result.map(|s| ev.string(&s)).map_err(anyhow_err)
}

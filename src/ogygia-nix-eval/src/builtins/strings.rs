//! String builtins.

use std::cmp::Ordering;
use std::rc::Rc;

use md5::Md5;
use regex_automata::Anchored;
use regex_automata::Input;
use regex_automata::MatchKind;
use regex_automata::meta;
use regex_automata::util::syntax;
use sha1::Sha1;
use sha2::Digest;
use sha2::Sha256;
use sha2::Sha512;

use crate::eval::Coerce;
use crate::eval::Eval;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

pub fn to_string<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(
        args[0],
        Coerce {
            more: true,
            copy: false,
        },
    )?;
    Ok(ev.str_with_ctx(&s, ctx))
}

pub fn string_length<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, _) = ev.coerce_to_string(args[0], Coerce::INTERP)?;
    Ok(Value::Int(s.len() as i64))
}

pub fn substring<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let start = ev.force_int(args[0])?;
    let len = ev.force_int(args[1])?;
    let (s, ctx) = ev.coerce_to_string(args[2], Coerce::INTERP)?;
    if start < 0 {
        return eval_err("negative start position in 'substring'");
    }
    let start = (start as usize).min(s.len());
    let end = if len < 0 {
        s.len()
    } else {
        start.saturating_add(len as usize).min(s.len())
    };
    Ok(ev.str_with_ctx(&s[start..end], ctx))
}

pub fn concat_strings_sep<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let sep = ev.force_str(args[0])?;
    let list = ev.force_list(args[1])?;
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

pub fn base_name_of<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(args[0], Coerce::PLAIN)?;
    Ok(ev.str_with_ctx(crate::path::base_name_of(&s), ctx))
}

pub fn dir_of<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    match ev.force(args[0])? {
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

pub fn hash_bytes(algo: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    Some(match algo {
        b"md5" => Md5::digest(data).to_vec(),
        b"sha1" => Sha1::digest(data).to_vec(),
        b"sha256" => Sha256::digest(data).to_vec(),
        b"sha512" => Sha512::digest(data).to_vec(),
        _ => return None,
    })
}

pub fn hash_string<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let algo = ev.force_str_no_ctx(args[0])?;
    let s = ev.force_str(args[1])?;
    match hash_bytes(algo, s.s) {
        Some(h) => Ok(ev.string(&hex::encode(h))),
        None => eval_err(format!(
            "unknown hash algorithm '{}'",
            String::from_utf8_lossy(algo)
        )),
    }
}

pub fn replace_strings<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let from = ev.force_list(args[0])?;
    let to = ev.force_list(args[1])?;
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
    let s = ev.force_str(args[2])?;
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

/// Translate a POSIX extended regular expression to `regex` syntax, writing
/// the end anchor `$` as `end`.
fn translate_regex(re: &[u8], end: &str) -> String {
    let re = String::from_utf8_lossy(re);
    let chars: Vec<char> = re.chars().collect();
    let mut out = String::with_capacity(re.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' => {
                // Copy a bracket expression, where backslash is literal and
                // `]` first is a member.
                out.push('[');
                i += 1;
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '[' && matches!(chars.get(i + 1), Some(':' | '.' | '=')) {
                        let close = chars.get(i + 1).copied().unwrap();
                        let start = i;
                        i += 2;
                        while i + 1 < chars.len() && !(chars[i] == close && chars[i + 1] == ']') {
                            i += 1;
                        }
                        i += 2;
                        out.extend(&chars[start..i.min(chars.len())]);
                        continue;
                    }
                    match chars[i] {
                        '\\' | '[' | '&' | '~' => {
                            out.push('\\');
                            out.push(chars[i]);
                        }
                        '-' if chars.get(i + 1) == Some(&'-') => out.push_str("\\-"),
                        ch => out.push(ch),
                    }
                    i += 1;
                }
                out.push(']');
                i += 1;
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    if n.is_ascii_alphanumeric()
                        && !matches!(n, 'w' | 'W' | 's' | 'S' | 'd' | 'D' | 'b' | 'B')
                    {
                        out.push(n);
                    } else {
                        out.push('\\');
                        out.push(n);
                    }
                    i += 2;
                } else {
                    out.push_str("\\\\");
                    i += 1;
                }
            }
            '$' => {
                out.push_str(end);
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn regex_error(re: &[u8], e: impl std::fmt::Display) -> Box<crate::value::EvalError> {
    crate::value::error(
        crate::value::ErrorKind::Eval,
        format!(
            "invalid regular expression '{}': {e}",
            String::from_utf8_lossy(re)
        ),
    )
}

fn compile_match_regex<'a>(ev: &Eval<'a>, re: &[u8]) -> R<'a, Rc<regex::bytes::Regex>> {
    if let Some(r) = ev.ctx.match_regexes.borrow().get(re) {
        return Ok(r.clone());
    }
    let pattern = format!("^(?:{})$", translate_regex(re, "$"));
    let r = regex::bytes::RegexBuilder::new(&pattern)
        .dot_matches_new_line(true)
        .build()
        .map_err(|e| regex_error(re, e))?;
    let r = Rc::new(r);
    ev.ctx
        .match_regexes
        .borrow_mut()
        .insert(re.to_vec(), r.clone());
    Ok(r)
}

fn captures_list<'a>(ev: &Eval<'a>, caps: &regex::bytes::Captures) -> Value<'a> {
    let groups: Vec<Value<'a>> = (1..caps.len())
        .map(|i| match caps.get(i) {
            Some(m) => ev.str_val(m.as_bytes(), &[]),
            None => Value::Null,
        })
        .collect();
    ev.list(&groups)
}

pub fn match_<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let re = ev.force_str_no_ctx(args[0])?;
    let s = ev.force_str(args[1])?;
    let r = compile_match_regex(ev, re)?;
    match r.captures(s.s) {
        Some(caps) => Ok(captures_list(ev, &caps)),
        None => Ok(Value::Null),
    }
}

/// A `builtins.split` pattern, searched for as Nix's POSIX `std::regex` does:
/// each match starts as early as possible and is then as long as possible,
/// with the groups of the first such match in order of alternatives and
/// repetitions.
pub(crate) struct SplitRegex {
    /// Finds where the next match starts.
    start: meta::Regex,
    /// Finds where the longest match from a start ends.
    longest: meta::Regex,
    /// Captures a match that ends at the end of the haystack.
    to_end: meta::Regex,
    /// Captures a match that ends at the end of a haystack cut short, where
    /// `$` cannot match.
    to_cut: meta::Regex,
}

fn compile_split_regex<'a>(ev: &Eval<'a>, re: &[u8]) -> R<'a, Rc<SplitRegex>> {
    if let Some(r) = ev.ctx.split_regexes.borrow().get(re) {
        return Ok(r.clone());
    }
    let build = |pattern: &str, kind| {
        meta::Builder::new()
            .configure(meta::Config::new().match_kind(kind).utf8_empty(false))
            .syntax(syntax::Config::new().utf8(false).dot_matches_new_line(true))
            .build(pattern)
            .map_err(|e| regex_error(re, e))
    };
    let translated = translate_regex(re, "$");
    let r = Rc::new(SplitRegex {
        start: build(&translated, MatchKind::LeftmostFirst)?,
        longest: build(&translated, MatchKind::All)?,
        to_end: build(&format!("(?:{translated})\\z"), MatchKind::LeftmostFirst)?,
        to_cut: build(
            &format!("(?:{})\\z", translate_regex(re, "[a&&b]")),
            MatchKind::LeftmostFirst,
        )?,
    });
    ev.ctx
        .split_regexes
        .borrow_mut()
        .insert(re.to_vec(), r.clone());
    Ok(r)
}

pub fn split<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let re = ev.force_str_no_ctx(args[0])?;
    let s = ev.force_str(args[1])?;
    let r = compile_split_regex(ev, re)?;
    let hay = s.s;
    let mut caps = r.to_end.create_captures();
    let mut out = Vec::new();
    let mut last = 0;
    let mut at = 0;
    while at <= hay.len() {
        let Some(m) = r.start.find(Input::new(hay).range(at..)) else {
            break;
        };
        let start = m.start();
        let end = r
            .longest
            .find(Input::new(hay).range(start..).anchored(Anchored::Yes))
            .expect("a match starts here")
            .end();
        // Cutting the haystack at the end of the match makes `\z` find the
        // groups of a match that ends there, but would let `$` match early.
        let to = if end == hay.len() {
            &r.to_end
        } else {
            &r.to_cut
        };
        to.captures(
            Input::new(&hay[..end])
                .range(start..)
                .anchored(Anchored::Yes),
            &mut caps,
        );
        let groups: Vec<Value<'a>> = (1..caps.group_len())
            .map(|i| match caps.get_group(i) {
                Some(g) => ev.str_val(&hay[g.range()], &[]),
                None => Value::Null,
            })
            .collect();
        out.push(ev.str_val(&hay[last..start], &[]));
        out.push(ev.list(&groups));
        last = end;
        // As with `std::regex_iterator`, an empty match may directly follow
        // another match, but the next search after one starts a byte later.
        at = if start == end { end + 1 } else { end };
    }
    out.push(ev.str_val(&hay[last..], &[]));
    Ok(ev.list(&out))
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
    let num = |c: &[u8]| -> Option<u128> {
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

pub fn compare_versions<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let a = ev.force_str_no_ctx(args[0])?;
    let b = ev.force_str_no_ctx(args[1])?;
    Ok(Value::Int(match compare_version_strings(a, b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

pub fn split_version<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str_no_ctx(args[0])?;
    let parts: Vec<Value<'a>> = version_components(s)
        .into_iter()
        .map(|c| ev.str_val(c, &[]))
        .collect();
    Ok(ev.list(&parts))
}

pub fn parse_drv_name<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str_no_ctx(args[0])?;
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

pub fn convert_hash<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
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
    let Some(format) = get("toHashFormat")? else {
        return eval_err("attribute 'toHashFormat' missing in a call to 'convertHash'");
    };
    let algo = get("hashAlgo")?;
    let result = crate::store::parse_hash(&hash, algo.as_deref())
        .and_then(|(algo, bytes)| crate::store::format_hash(&algo, &bytes, &format));
    match result {
        Ok(s) => Ok(ev.string(&s)),
        Err(e) => eval_err(format!("{e:#}")),
    }
}

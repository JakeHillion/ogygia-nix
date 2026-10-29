//! The flake builtins, which Nix provides but refuses to run while the
//! `flakes` experimental feature is disabled, as it is here, except for the
//! references of input types that do not need it.

use std::collections::BTreeMap;

use super::url;
use super::url::split_once;
use crate::eval::Eval;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

const FLAKES_DISABLED: &str = "experimental Nix feature 'flakes' is disabled; add '--extra-experimental-features flakes' to enable it";

fn flakes_disabled<'a>() -> R<'a> {
    eval_err(FLAKES_DISABLED)
}

pub fn get_flake<'a>(ev: &Eval<'a>, flake_ref: Value<'a>) -> R<'a> {
    ev.force_str_no_ctx(flake_ref)?;
    flakes_disabled()
}

pub fn parse_flake_ref<'a>(ev: &Eval<'a>, flake_ref: Value<'a>) -> R<'a> {
    let attrs = url_ref(ev.force_str_no_ctx(flake_ref)?).or_else(eval_err)?;
    Ok(ev.attrs(
        attrs
            .into_iter()
            .map(|(name, attr)| {
                let value = match attr {
                    Attr::Str(s) => ev.str_val(&s, &[]),
                    Attr::Int(i) => Value::Int(i),
                    Attr::Bool(b) => Value::Bool(b),
                };
                ev.entry(name, value)
            })
            .collect(),
    ))
}

enum Attr {
    Str(Vec<u8>),
    Int(i64),
    Bool(bool),
}

/// The attributes of the flake reference `r`, which must be the URL of a
/// Git, Mercurial, tarball or file input.
fn url_ref(r: &[u8]) -> Result<Vec<(&'static str, Attr)>, String> {
    let shown = String::from_utf8_lossy(r);
    let (r, fragment) = split_once(r, b'#').unwrap_or((r, b""));
    if !fragment.is_empty() {
        return Err(format!(
            "unexpected fragment '{}' in flake reference '{shown}'",
            String::from_utf8_lossy(fragment)
        ));
    }
    let not_absolute = || format!("flake reference '{shown}' is not an absolute path");
    let (base, query) = split_once(r, b'?').unwrap_or((r, b""));
    // Nix reads what is not a URL as a path or a registry name, which need
    // the flakes feature, and rejects a relative path first.
    let Ok(uri) = url::parse(base) else {
        return Err(if r.starts_with(b".") || r.contains(&b':') {
            not_absolute()
        } else {
            FLAKES_DISABLED.to_owned()
        });
    };
    if !url::chars(query, |c| url::pchar(c) || b"/? \"[]".contains(&c)) {
        return Err(format!(
            "invalid URI query '{}'",
            String::from_utf8_lossy(query)
        ));
    }
    let mut params = BTreeMap::new();
    for (k, v) in query
        .split(|&c| c == b'&')
        .filter_map(|p| split_once(p, b'='))
    {
        params
            .entry(url::decode(k))
            .or_insert_with(|| url::decode(v));
    }

    let (ty, scheme) = match uri.scheme {
        b"path" | b"github" | b"gitlab" | b"sourcehut" | b"flake" => {
            return Err(FLAKES_DISABLED.to_owned());
        }
        s @ b"git" => ("git", s),
        s @ (b"http" | b"https" | b"file") => ("tarball", s),
        s => match split_once(s, b'+') {
            Some((b"git", s)) => ("git", s),
            Some((b"hg", s @ (b"http" | b"https" | b"ssh" | b"file"))) => ("hg", s),
            Some((b"tarball", s @ (b"http" | b"https" | b"file"))) => ("tarball", s),
            Some((b"file", s @ (b"http" | b"https" | b"file"))) => ("file", s),
            _ => return Err(format!("input '{shown}' is unsupported")),
        },
    };
    if scheme == b"file" && uri.authority.as_ref().is_some_and(|a| !a.host.is_empty()) {
        return Err(not_absolute());
    }

    let mut attrs = vec![("type", Attr::Str(ty.into()))];
    if let Some(dir) = params.remove(&b"dir"[..]).filter(|d| !d.is_empty()) {
        attrs.push(("dir", Attr::Str(dir)));
    }
    let mut str_attr = |params: &mut BTreeMap<Vec<u8>, Vec<u8>>, name: &'static str| {
        if let Some(v) = params.remove(name.as_bytes()) {
            attrs.push((name, Attr::Str(v)));
        }
    };
    match ty {
        "git" | "hg" => {
            if let Some(r) = params.get(&b"ref"[..]) {
                let (valid, kind) = match ty {
                    "git" => (git_ref(r), "Git"),
                    _ => (hg_ref(r), "Mercurial"),
                };
                if !valid {
                    return Err(format!(
                        "invalid {kind} branch/tag name '{}'",
                        String::from_utf8_lossy(r)
                    ));
                }
            }
            str_attr(&mut params, "ref");
            str_attr(&mut params, "rev");
            if ty == "git" {
                const VERIFIED: [&str; 4] = ["verifyCommit", "keytype", "publicKey", "publicKeys"];
                if VERIFIED.iter().any(|n| params.contains_key(n.as_bytes())) {
                    return Err("experimental Nix feature 'verified-fetches' is disabled; add '--extra-experimental-features verified-fetches' to enable it".to_owned());
                }
                for name in ["shallow", "submodules", "lfs", "exportIgnore", "allRefs"] {
                    if let Some(v) = params.remove(name.as_bytes()) {
                        attrs.push((name, Attr::Bool(v == b"1")));
                    }
                }
            }
        }
        _ => {
            str_attr(&mut params, "narHash");
            str_attr(&mut params, "rev");
            for name in ["revCount", "lastModified"] {
                // An integer that does not parse is dropped, and one that
                // overflows a signed integer wraps.
                let int = params
                    .remove(name.as_bytes())
                    .and_then(|v| String::from_utf8(v).ok()?.parse::<u64>().ok());
                if let Some(i) = int {
                    attrs.push((name, Attr::Int(i as i64)));
                }
            }
            for name in ["unpack", "name", "url"] {
                params.remove(name.as_bytes());
            }
        }
    }

    let mut out = [scheme, &b":"[..]].concat();
    if let Some(a) = &uri.authority {
        out.extend(b"//");
        authority(&mut out, a);
    }
    if uri.path.is_empty() && scheme == b"file" {
        out.push(b'/');
    } else {
        let segments: Vec<Vec<u8>> = uri
            .path
            .split(|&c| c == b'/')
            .map(|s| url::encode(&url::decode(s), b":@"))
            .collect();
        out.extend(segments.join(&b'/'));
    }
    if !params.is_empty() {
        let query: Vec<Vec<u8>> = params
            .iter()
            .map(|(k, v)| [url::encode(k, b":@/?"), url::encode(v, b":@/?")].join(&b'='))
            .collect();
        out.push(b'?');
        out.extend(query.join(&b'&'));
    }
    attrs.push(("url", Attr::Str(out)));
    Ok(attrs)
}

/// Write `a` with each part percent-encoded afresh, and the port without
/// leading zeros.
fn authority(out: &mut Vec<u8>, a: &url::Authority<'_>) {
    let fresh = |s: &[u8]| url::encode(&url::decode(s), b"");
    if let Some(userinfo) = a.userinfo {
        match split_once(userinfo, b':') {
            Some((user, password)) => {
                out.extend(fresh(user));
                out.push(b':');
                out.extend(fresh(password));
            }
            None => out.extend(fresh(userinfo)),
        }
        out.push(b'@');
    }
    match a.host.strip_prefix(b"[").and_then(|h| h.strip_suffix(b"]")) {
        Some(future) if future.starts_with(b"v") => {
            out.push(b'[');
            out.extend(url::encode(future, b":"));
            out.push(b']');
        }
        Some(ip) => {
            out.push(b'[');
            match ip.windows(3).position(|w| w == b"%25") {
                Some(i) => {
                    out.extend(&ip[..i + 3]);
                    out.extend(fresh(&ip[i + 3..]));
                }
                None => out.extend(ip),
            }
            out.push(b']');
        }
        None => out.extend(fresh(a.host)),
    }
    if let Some(port) = a.port {
        out.extend(format!(":{port}").bytes());
    }
}

/// Whether `r` is a valid Git branch or tag name. Nix stops reading it at
/// the first NUL byte.
fn git_ref(r: &[u8]) -> bool {
    let r = &r[..r.iter().position(|&c| c == 0).unwrap_or(r.len())];
    let has = |s: &[u8]| r.windows(s.len()).any(|w| w == s);
    !r.is_empty()
        && r != b"@"
        && !r.starts_with(b"-")
        && !r.starts_with(b"/")
        && !r.ends_with(b"/")
        && !r.ends_with(b".")
        && !has(b"..")
        && !has(b"//")
        && !has(b"@{")
        && r.iter()
            .all(|&c| c >= 0x20 && c != 0x7f && !b" ~^:?*[\\".contains(&c))
        && r.split(|&c| c == b'/')
            .all(|c| !c.starts_with(b".") && !c.ends_with(b".lock"))
}

/// Whether `r` is a valid Mercurial branch or tag name.
fn hg_ref(r: &[u8]) -> bool {
    r.first()
        .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'@')
        && r.iter()
            .all(|&c| c.is_ascii_alphanumeric() || b"_./@+-".contains(&c))
}

pub fn flake_ref_to_string<'a>(ev: &Eval<'a>, set: Value<'a>) -> R<'a> {
    let set = ev.force_attrs(set)?;
    for e in set.entries {
        match ev.force(e.value)? {
            Value::Str(_) | Value::Int(_) | Value::Bool(_) => {}
            v => {
                return eval_err(format!(
                    "flake reference attribute sets may only contain integers, Booleans, and strings, but attribute '{}' is {}",
                    ev.ctx.name(e.name),
                    v.show_type()
                ));
            }
        }
    }
    match set
        .get(ev.ctx.syms.type_)
        .map(|v| ev.force(v))
        .transpose()?
    {
        None => eval_err("'type' attribute to specify input scheme is required but not provided"),
        Some(Value::Str(_)) => flakes_disabled(),
        Some(_) => eval_err("input attribute 'type' is not a string"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects() {
        for r in [
            "https://:0",
            "https://:00",
            "https://:65536",
            "https://:-1",
            "https://h:1:2",
            "https://u@h@h",
            "https://a b",
            "https://h/a b",
            "https://h/é",
            "https://h/a%zz",
            "https://[zz]",
            "https://[V1.x]",
            "https://h#f",
            "https://h?a=%zz",
            "https://h?a=é",
            "https://h?a=b^c",
            "HTTPS://h",
            "ftp://h",
            "hg://h/x",
            "hg+git://h/x",
            "tarball+ftp://h/x",
            "file+git://h/x",
            "file://h/x",
            "git+file://localhost/x",
            "path:/x",
            "github:o/r",
            "flake:nixpkgs",
            "nixpkgs",
            "/x",
            "./x",
            "git+https://h?verifyCommit=0",
            "git+https://h?ref=",
            "git+https://h?ref=%00a",
            "git+https://h?ref=-a",
            "git+https://h?ref=@",
            "git+https://h?ref=a..b",
            "git+https://h?ref=a/.b",
            "git+https://h?ref=a.lock/b",
            "git+https://h?ref=a/",
            "git+https://h?ref=a.",
            "git+https://h?ref=a@%7Bb",
            "git+https://h?ref=a%5Cb",
            "git+https://h?ref=a%01",
            "hg+https://h?ref=",
            "hg+https://h?ref=-a",
            "hg+https://h?ref=a%00",
            "hg+https://h?ref=a%C3%A9",
        ] {
            assert!(url_ref(r.as_bytes()).is_err(), "{r}");
        }
    }

    #[test]
    fn git_ref_stops_at_nul() {
        let attrs = url_ref(b"git+https://h?ref=a%00..&ref=b").unwrap();
        assert!(
            attrs
                .iter()
                .any(|(n, a)| *n == "ref" && matches!(a, Attr::Str(s) if s == b"a\0.."))
        );
    }
}

//! Flake references: the URLs of flake inputs and installables, and the
//! attribute sets lock files record them as.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use anyhow::bail;

/// The value of an attribute of an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attr {
    Str(String),
    Int(u64),
    Bool(bool),
}

/// An input as a set of attributes, as a lock file's `original` and
/// `locked` record it. `dir`, the flake's directory within the input, is
/// one of them.
pub type Attrs = BTreeMap<String, Attr>;

pub fn get_str<'a>(attrs: &'a Attrs, name: &str) -> Option<&'a str> {
    match attrs.get(name) {
        Some(Attr::Str(s)) => Some(s),
        _ => None,
    }
}

pub fn get_int(attrs: &Attrs, name: &str) -> Option<u64> {
    match attrs.get(name) {
        Some(Attr::Int(i)) => Some(*i),
        _ => None,
    }
}

/// Attributes whose values in a URL's query are integers.
const INT_ATTRS: &[&str] = &["lastModified", "revCount"];

/// Attributes whose values in a URL's query are Booleans.
const BOOL_ATTRS: &[&str] = &["submodules", "shallow", "lfs", "exportIgnore", "allRefs"];

/// Archive extensions that make an `http`, `https` or `file` URL a tarball.
const ARCHIVES: &[&str] = &[
    ".zip", ".tar", ".tgz", ".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst",
];

fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(b) = hex else {
                bail!("invalid URI parameter '{s}'");
            };
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow::anyhow!("invalid URI parameter '{s}'"))
}

/// Add the parameters of the query string `query` to `attrs`.
fn add_query(attrs: &mut Attrs, query: &str) -> Result<()> {
    for param in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = param.split_once('=').unwrap_or((param, ""));
        let name = percent_decode(name)?;
        let value = percent_decode(value)?;
        let attr = if INT_ATTRS.contains(&name.as_str()) {
            match value.parse() {
                Ok(i) => Attr::Int(i),
                Err(_) => bail!("invalid value '{value}' for attribute '{name}'"),
            }
        } else if BOOL_ATTRS.contains(&name.as_str()) {
            Attr::Bool(value == "1" || value == "true")
        } else {
            Attr::Str(value)
        };
        attrs.insert(name, attr);
    }
    Ok(())
}

fn str_attr(attrs: &mut Attrs, name: &str, value: impl Into<String>) {
    attrs.insert(name.to_owned(), Attr::Str(value.into()));
}

fn is_hex_rev(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The URL scheme `url` starts with, if any.
fn scheme(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
        .then_some((scheme, rest))
}

/// Parse a flake reference as written in a flake's inputs or on the command
/// line. Relative paths are kept as written when `cwd` is `None`, and
/// otherwise made absolute against it.
pub fn parse(url: &str, cwd: Option<&Path>) -> Result<Attrs> {
    let mut attrs = Attrs::new();
    let (url_part, query) = url.split_once('?').unwrap_or((url, ""));
    match scheme(url_part) {
        Some(("path", path)) => {
            str_attr(&mut attrs, "type", "path");
            str_attr(&mut attrs, "path", resolve(path, cwd));
        }
        Some(("flake", id)) => indirect(&mut attrs, id, url)?,
        Some((s @ ("github" | "gitlab" | "sourcehut"), rest)) => {
            let mut parts = rest.split('/');
            let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
                bail!("'{url}' is not a valid URL");
            };
            str_attr(&mut attrs, "type", s);
            str_attr(&mut attrs, "owner", owner);
            str_attr(&mut attrs, "repo", repo);
            let rest: Vec<&str> = parts.collect();
            if !rest.is_empty() {
                let r = rest.join("/");
                str_attr(&mut attrs, if is_hex_rev(&r) { "rev" } else { "ref" }, r);
            }
        }
        Some((s, rest)) if s.starts_with("git+") => {
            str_attr(&mut attrs, "type", "git");
            str_attr(&mut attrs, "url", format!("{}:{rest}", &s[4..]));
        }
        Some((s, rest)) if s.starts_with("tarball+") || s.starts_with("file+") => {
            let (kind, inner) = s.split_once('+').expect("checked above");
            str_attr(
                &mut attrs,
                "type",
                if kind == "file" { "file" } else { "tarball" },
            );
            str_attr(&mut attrs, "url", format!("{inner}:{rest}"));
        }
        Some((s @ ("http" | "https" | "file"), rest)) => {
            let archive = ARCHIVES.iter().any(|e| url_part.ends_with(e));
            str_attr(&mut attrs, "type", if archive { "tarball" } else { "file" });
            str_attr(&mut attrs, "url", format!("{s}:{rest}"));
        }
        Some((s, _)) => bail!("input type '{s}' is unsupported"),
        None if url_part.starts_with('/') || url_part.starts_with('.') => {
            str_attr(&mut attrs, "type", "path");
            str_attr(&mut attrs, "path", resolve(url_part, cwd));
        }
        None => indirect(&mut attrs, url_part, url)?,
    }
    add_query(&mut attrs, query)?;
    Ok(attrs)
}

fn indirect(attrs: &mut Attrs, id: &str, url: &str) -> Result<()> {
    let (id, rest) = id.split_once('/').unwrap_or((id, ""));
    if id.is_empty()
        || !id.starts_with(|c: char| c.is_ascii_alphabetic())
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("'{url}' is not a valid URL");
    }
    str_attr(attrs, "type", "indirect");
    str_attr(attrs, "id", id);
    if !rest.is_empty() {
        str_attr(attrs, if is_hex_rev(rest) { "rev" } else { "ref" }, rest);
    }
    Ok(())
}

/// `path` against `cwd`, if given and `path` is relative, with `.` and `..`
/// components resolved.
fn resolve(path: &str, cwd: Option<&Path>) -> String {
    match cwd {
        Some(cwd) if !path.starts_with('/') => {
            crate::path::canon_path(&format!("{}/{path}", cwd.display()))
        }
        _ => path.to_owned(),
    }
}

/// The reference `attrs` describes, as Nix shows it in messages.
pub fn to_string(attrs: &Attrs) -> String {
    let ty = get_str(attrs, "type").unwrap_or("");
    let mut skip = vec!["type"];
    let mut out = match ty {
        "path" => {
            skip.push("path");
            format!("path:{}", get_str(attrs, "path").unwrap_or(""))
        }
        "git" => {
            skip.push("url");
            format!("git+{}", get_str(attrs, "url").unwrap_or(""))
        }
        "tarball" | "file" => {
            skip.push("url");
            get_str(attrs, "url").unwrap_or("").to_owned()
        }
        "indirect" => {
            skip.push("id");
            format!("flake:{}", get_str(attrs, "id").unwrap_or(""))
        }
        _ => {
            skip.extend(["owner", "repo"]);
            format!(
                "{ty}:{}/{}",
                get_str(attrs, "owner").unwrap_or(""),
                get_str(attrs, "repo").unwrap_or("")
            )
        }
    };
    let query: Vec<String> = attrs
        .iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .map(|(k, v)| match v {
            Attr::Str(s) => format!("{k}={s}"),
            Attr::Int(i) => format!("{k}={i}"),
            Attr::Bool(b) => format!("{k}={}", u8::from(*b)),
        })
        .collect();
    if !query.is_empty() {
        out.push('?');
        out.push_str(&query.join("&"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(attrs: &[(&str, &str)]) -> Attrs {
        attrs
            .iter()
            .map(|(k, v)| (k.to_string(), Attr::Str(v.to_string())))
            .collect()
    }

    #[test]
    fn parses_input_urls() {
        assert_eq!(
            parse("github:o/r", None).unwrap(),
            s(&[("type", "github"), ("owner", "o"), ("repo", "r")])
        );
        assert_eq!(
            parse("github:o/r/nixos-unstable", None).unwrap(),
            s(&[
                ("type", "github"),
                ("owner", "o"),
                ("repo", "r"),
                ("ref", "nixos-unstable")
            ])
        );
        assert_eq!(
            parse("path:./sub", None).unwrap(),
            s(&[("type", "path"), ("path", "./sub")])
        );
        assert_eq!(
            parse("git+file:///r?dir=d", None).unwrap(),
            s(&[("type", "git"), ("url", "file:///r"), ("dir", "d")])
        );
        assert_eq!(
            parse("https://x/n.tar.xz", None).unwrap(),
            s(&[("type", "tarball"), ("url", "https://x/n.tar.xz")])
        );
        assert_eq!(
            parse("nixpkgs", None).unwrap(),
            s(&[("type", "indirect"), ("id", "nixpkgs")])
        );
    }

    #[test]
    fn resolves_relative_paths_against_cwd() {
        assert_eq!(
            parse("./a/../b", Some(Path::new("/c"))).unwrap(),
            s(&[("type", "path"), ("path", "/c/b")])
        );
    }
}

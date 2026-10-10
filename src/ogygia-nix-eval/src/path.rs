//! Lexical path manipulation, matching Nix's handling of path values.

use std::borrow::Cow;

/// Prefix of the logical paths of the files Nix ships with itself, which
/// `<nix/…>` finds when no search path entry has it. The rest of such a
/// logical path is the absolute path within those files.
pub const COREPKGS: &str = "<nix>";

/// The absolute path within the filesystem `p` belongs to, as `toString`
/// gives it.
pub fn abs(p: &str) -> &str {
    p.strip_prefix(COREPKGS).unwrap_or(p)
}

/// `p` as Nix prints it.
pub fn show(p: &str) -> Cow<'_, str> {
    match p.strip_prefix(COREPKGS) {
        Some(rest) => Cow::Owned(format!("<nix{rest}>")),
        None => Cow::Borrowed(p),
    }
}

/// Make `p` absolute-looking and remove `.`, `..` and repeated slashes
/// without consulting the filesystem.
pub fn canon_path(p: &str) -> String {
    if let Some(rest) = p.strip_prefix(COREPKGS) {
        return format!("{COREPKGS}{}", canon_path(rest));
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return "/".to_owned();
    }
    let mut out = String::with_capacity(p.len());
    for c in parts {
        out.push('/');
        out.push_str(c);
    }
    out
}

/// `dirOf` for a path: everything before the last slash, or `/`.
pub fn dir_of(p: &str) -> &str {
    let root = p.len() - abs(p).len();
    match p.rfind('/') {
        Some(i) if i == root => &p[..=i],
        Some(i) => &p[..i],
        None => ".",
    }
}

/// `baseNameOf`: the last component, ignoring one trailing slash.
pub fn base_name_of(p: &[u8]) -> &[u8] {
    let p = p.strip_suffix(b"/").unwrap_or(p);
    match p.iter().rposition(|&b| b == b'/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// The name of the store path that copying the path `p` gives.
pub fn store_name(p: &str) -> String {
    match base_name_of(abs(p).as_bytes()) {
        b"" => "source".to_owned(),
        name => String::from_utf8_lossy(name).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon() {
        assert_eq!(canon_path("/a/b/../c"), "/a/c");
        assert_eq!(canon_path("/a//b/./c/"), "/a/b/c");
        assert_eq!(canon_path("/.."), "/");
        assert_eq!(canon_path("/"), "/");
        assert_eq!(canon_path("<nix>/a/../.."), "<nix>/");
    }

    #[test]
    fn dir() {
        assert_eq!(dir_of("/a/b"), "/a");
        assert_eq!(dir_of("/a"), "/");
        assert_eq!(dir_of("<nix>/a"), "<nix>/");
        assert_eq!(dir_of("<nix>/"), "<nix>/");
    }
}

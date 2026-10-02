//! Lexical path manipulation, matching Nix's handling of path values.

/// Make `p` absolute-looking and remove `.`, `..` and repeated slashes
/// without consulting the filesystem.
pub fn canon_path(p: &str) -> String {
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
    match p.rfind('/') {
        Some(0) => "/",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon() {
        assert_eq!(canon_path("/a/b/../c"), "/a/c");
        assert_eq!(canon_path("/a//b/./c/"), "/a/b/c");
        assert_eq!(canon_path("/.."), "/");
        assert_eq!(canon_path("/"), "/");
    }
}

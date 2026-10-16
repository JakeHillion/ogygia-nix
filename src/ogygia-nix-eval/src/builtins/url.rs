//! URL syntax checking for fetchers, following RFC 3986 as Nix applies it.

/// Check that `url` is a URI, and that its port, if any, is in range.
pub fn check(url: &[u8]) -> Result<(), String> {
    let invalid = || format!("'{}' is not a valid URL", String::from_utf8_lossy(url));
    let (scheme, rest) = url.split_at(url.iter().position(|&c| c == b':').ok_or_else(invalid)?);
    let rest = &rest[1..];
    if !scheme.first().is_some_and(u8::is_ascii_alphabetic)
        || !scheme
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
    {
        return Err(invalid());
    }
    let (rest, fragment) = split(rest, b'#');
    // Nix also accepts '#' in the fragment.
    if !chars(fragment, |c| pchar(c) || b"/?#".contains(&c)) {
        return Err(invalid());
    }
    let (rest, query) = split(rest, b'?');
    // Nix also accepts '[' and ']' in the query.
    if !chars(query, |c| pchar(c) || b"/?[]".contains(&c)) {
        return Err(invalid());
    }
    let path = match rest.strip_prefix(b"//") {
        Some(rest) => {
            let end = rest.iter().position(|&c| c == b'/').unwrap_or(rest.len());
            authority(&rest[..end]).map_err(|e| e.unwrap_or_else(invalid))?;
            &rest[end..]
        }
        None => rest,
    };
    if !chars(path, |c| pchar(c) || c == b'/') {
        return Err(invalid());
    }
    Ok(())
}

/// Split `s` at the first `sep`, if any.
fn split(s: &[u8], sep: u8) -> (&[u8], &[u8]) {
    match s.iter().position(|&c| c == sep) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, &[]),
    }
}

/// Fails with `Some` message for a bad port, or `None` for bad syntax.
fn authority(a: &[u8]) -> Result<(), Option<String>> {
    let (userinfo, host_port) = match a.iter().position(|&c| c == b'@') {
        Some(i) => (&a[..i], &a[i + 1..]),
        None => (&[][..], a),
    };
    if !chars(userinfo, |c| unreserved(c) || sub_delim(c) || c == b':') {
        return Err(None);
    }
    let (host_ok, port) = match host_port.strip_prefix(b"[") {
        Some(rest) => {
            let end = rest.iter().position(|&c| c == b']').ok_or(None)?;
            let port = match &rest[end + 1..] {
                [] => None,
                [b':', port @ ..] => Some(port),
                _ => return Err(None),
            };
            (ip_literal(&rest[..end]), port)
        }
        None => {
            let (host, port) = match host_port.iter().position(|&c| c == b':') {
                Some(i) => (&host_port[..i], Some(&host_port[i + 1..])),
                None => (host_port, None),
            };
            (chars(host, |c| unreserved(c) || sub_delim(c)), port)
        }
    };
    if !host_ok {
        return Err(None);
    }
    match port {
        Some(p) if !p.iter().all(u8::is_ascii_digit) => Err(None),
        Some(p @ [_, ..]) => {
            let digits = String::from_utf8_lossy(p);
            match digits.trim_start_matches('0').parse::<u32>() {
                Ok(1..=65535) => Ok(()),
                _ => Err(Some(format!("port '{digits}' is invalid"))),
            }
        }
        _ => Ok(()),
    }
}

fn ip_literal(s: &[u8]) -> bool {
    // Nix accepts only a lowercase 'v'.
    if let Some(future) = s.strip_prefix(b"v") {
        let dot = future.iter().position(|&c| c == b'.');
        return dot.is_some_and(|i| {
            i > 0
                && future[..i].iter().all(u8::is_ascii_hexdigit)
                && future.len() > i + 1
                && future[i + 1..]
                    .iter()
                    .all(|&c| unreserved(c) || sub_delim(c) || c == b':')
        });
    }
    let (addr, zone) = match s.iter().position(|&c| c == b'%') {
        Some(i) => (&s[..i], Some(&s[i..])),
        None => (s, None),
    };
    let zone_ok = zone.is_none_or(|z| {
        z.strip_prefix(b"%25")
            .is_some_and(|id| !id.is_empty() && chars(id, unreserved))
    });
    zone_ok && ipv6(addr)
}

fn ipv6(s: &[u8]) -> bool {
    match s.windows(2).position(|w| w == b"::") {
        Some(i) => match (groups(&s[..i], false), groups(&s[i + 2..], true)) {
            (Some(head), Some(tail)) => head + tail <= 7,
            _ => false,
        },
        None => groups(s, true) == Some(8),
    }
}

/// The number of 16-bit groups in `:`-separated `s`, counting a trailing
/// IPv4 address, allowed if `v4_tail`, as two.
fn groups(s: &[u8], v4_tail: bool) -> Option<usize> {
    if s.is_empty() {
        return Some(0);
    }
    let pieces: Vec<&[u8]> = s.split(|&c| c == b':').collect();
    let mut n = 0;
    for (i, p) in pieces.iter().enumerate() {
        if v4_tail && i == pieces.len() - 1 && p.contains(&b'.') {
            ipv4(p).then_some(())?;
            n += 2;
        } else if (1..=4).contains(&p.len()) && p.iter().all(u8::is_ascii_hexdigit) {
            n += 1;
        } else {
            return None;
        }
    }
    Some(n)
}

fn ipv4(s: &[u8]) -> bool {
    let octets: Vec<&[u8]> = s.split(|&c| c == b'.').collect();
    octets.len() == 4
        && octets.iter().all(|o| {
            matches!(o.len(), 1..=3)
                && o.iter().all(u8::is_ascii_digit)
                && (o.len() == 1 || o[0] != b'0')
                && std::str::from_utf8(o)
                    .ok()
                    .and_then(|o| o.parse::<u8>().ok())
                    .is_some()
        })
}

/// Whether every character of `s` is a percent-encoded byte or satisfies
/// `ok`.
fn chars(s: &[u8], ok: impl Fn(u8) -> bool) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            if !s
                .get(i + 1..i + 3)
                .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))
            {
                return false;
            }
            i += 3;
        } else if ok(s[i]) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

fn unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"-._~".contains(&c)
}

fn sub_delim(c: u8) -> bool {
    b"!$&'()*+,;=".contains(&c)
}

fn pchar(c: u8) -> bool {
    unreserved(c) || sub_delim(c) || c == b':' || c == b'@'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid() {
        for url in [
            "https://u:p:q@h!$&'()*+,;=~_-.:080/a;b=c:@/%41%aB?[]/?#a#b?/:@",
            "https://:65535",
            "https://h:/",
            "https://",
            "https:",
            "file:a",
            "a+b-c.d://h/x",
            "https://h%20b/",
            "https://[::1%25a%41]/",
            "https://[v1.x:y]:80/",
            "https://[::]/",
            "https://[1::]/",
            "https://[1:2:3:4:5:6:7:8]/",
            "https://[1:2:3:4:5:6:7::]/",
            "https://[::1:2:3:4:5:6:7]/",
            "https://[1:2:3:4:5:6:1.2.3.4]/",
            "https://[::ffff:255.0.10.4]/",
            "https://u@[::1]/",
        ] {
            assert_eq!(check(url.as_bytes()), Ok(()), "{url}");
        }
    }

    #[test]
    fn invalid() {
        for url in [
            "file:a =",
            "git:é",
            "a_b://h/x",
            "1a://h/x",
            "https://h/%4",
            "https://h/a%",
            "https://h/%G1",
            "https://h/[",
            "https://h/a]b",
            "https://h#[",
            "https://h#%zz",
            "https://h?%zz",
            "https://h?a b",
            "https://h?{",
            "https://h b/",
            "https://h:x/",
            "https://h:8 0/",
            "https://u@v@h/",
            "https://u[@h/",
            "https://[zz]/",
            "https://[]/",
            "https://[::1/",
            "https://[::1]x/",
            "https://[::1]@h/",
            "https://[V1.x]/",
            "https://[vz.x]/",
            "https://[v1.]/",
            "https://[::1%eth0]/",
            "https://[::1%25]/",
            "https://[::1%25a!]/",
            "https://[1:2:3:4:5:6:7:8:9]/",
            "https://[1:2:3:4:5:6::7:8]/",
            "https://[1:2:3:4:5:6:7:1.2.3.4]/",
            "https://[1::2::3]/",
            "https://[12345::]/",
            "https://[:1]/",
            "https://[1:]/",
            "https://[1.2.3.4]/",
            "https://[::1.2.3]/",
            "https://[::1.2.3.256]/",
            "https://[::01.2.3.4]/",
            "https://[::1.2.3.4:5]/",
            "https://h/\x7f",
        ] {
            assert!(check(url.as_bytes()).is_err(), "{url}");
        }
    }

    #[test]
    fn port_range() {
        for (url, ok) in [
            ("https://h:1/", true),
            ("https://h:00000000000065535/", true),
            ("https://h:0/", false),
            ("https://h:65536/", false),
            ("https://h:4294967296/", false),
        ] {
            assert_eq!(check(url.as_bytes()).is_ok(), ok, "{url}");
        }
    }
}

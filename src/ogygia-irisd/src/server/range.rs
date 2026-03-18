//! HTTP Range header parsing (RFC 7233, single byte ranges only).

/// A parsed single byte range from an HTTP `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=START-END` (inclusive on both ends)
    FromTo(u64, u64),
    /// `bytes=START-` (from offset to end of file)
    From(u64),
    /// `bytes=-N` (last N bytes)
    Suffix(u64),
}

impl ByteRange {
    /// Parse a `Range` header value. Only single byte ranges are supported.
    ///
    /// Returns `None` for malformed or multi-range headers.
    pub fn parse(header: &str) -> Option<Self> {
        let spec = header.strip_prefix("bytes=")?;

        // Reject multi-range
        if spec.contains(',') {
            return None;
        }

        let (start, end) = spec.split_once('-')?;

        if start.is_empty() {
            // bytes=-N (suffix)
            let n: u64 = end.parse().ok()?;
            if n == 0 {
                return None;
            }
            Some(ByteRange::Suffix(n))
        } else if end.is_empty() {
            // bytes=START-
            let start: u64 = start.parse().ok()?;
            Some(ByteRange::From(start))
        } else {
            // bytes=START-END
            let start: u64 = start.parse().ok()?;
            let end: u64 = end.parse().ok()?;
            if start > end {
                return None;
            }
            Some(ByteRange::FromTo(start, end))
        }
    }

    /// Resolve this range against a known file size.
    ///
    /// Returns `(offset, length)` or `None` if the range is unsatisfiable.
    pub fn resolve(self, total_size: u64) -> Option<(u64, u64)> {
        if total_size == 0 {
            return None;
        }
        match self {
            ByteRange::FromTo(start, end) => {
                if start >= total_size {
                    return None;
                }
                let end = end.min(total_size - 1);
                Some((start, end - start + 1))
            }
            ByteRange::From(start) => {
                if start >= total_size {
                    return None;
                }
                Some((start, total_size - start))
            }
            ByteRange::Suffix(n) => {
                let offset = total_size.saturating_sub(n);
                Some((offset, total_size - offset))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_from_to() {
        assert_eq!(
            ByteRange::parse("bytes=0-499"),
            Some(ByteRange::FromTo(0, 499))
        );
        assert_eq!(
            ByteRange::parse("bytes=500-999"),
            Some(ByteRange::FromTo(500, 999))
        );
    }

    #[test]
    fn test_parse_from() {
        assert_eq!(ByteRange::parse("bytes=500-"), Some(ByteRange::From(500)));
    }

    #[test]
    fn test_parse_suffix() {
        assert_eq!(ByteRange::parse("bytes=-500"), Some(ByteRange::Suffix(500)));
    }

    #[test]
    fn test_parse_invalid() {
        assert_eq!(ByteRange::parse("bytes="), None);
        assert_eq!(ByteRange::parse("bytes=500-200"), None); // start > end
        assert_eq!(ByteRange::parse("bytes=-0"), None); // suffix of 0
        assert_eq!(ByteRange::parse("bytes=0-100,200-300"), None); // multi-range
        assert_eq!(ByteRange::parse("items=0-100"), None); // wrong unit
    }

    #[test]
    fn test_resolve_from_to() {
        assert_eq!(ByteRange::FromTo(0, 499).resolve(1000), Some((0, 500)));
        assert_eq!(ByteRange::FromTo(0, 1999).resolve(1000), Some((0, 1000)));
        assert_eq!(ByteRange::FromTo(1000, 1999).resolve(1000), None);
    }

    #[test]
    fn test_resolve_from() {
        assert_eq!(ByteRange::From(500).resolve(1000), Some((500, 500)));
        assert_eq!(ByteRange::From(0).resolve(1000), Some((0, 1000)));
        assert_eq!(ByteRange::From(1000).resolve(1000), None);
    }

    #[test]
    fn test_resolve_suffix() {
        assert_eq!(ByteRange::Suffix(500).resolve(1000), Some((500, 500)));
        assert_eq!(ByteRange::Suffix(2000).resolve(1000), Some((0, 1000)));
    }

    #[test]
    fn test_resolve_empty_file() {
        assert_eq!(ByteRange::From(0).resolve(0), None);
        assert_eq!(ByteRange::Suffix(100).resolve(0), None);
    }
}

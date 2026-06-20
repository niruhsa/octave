//! HTTP `Range` header parser, scoped to single-range `bytes=` requests.
//!
//! We deliberately reject multi-range requests (`bytes=0-9,20-29`). They
//! require a `multipart/byteranges` response, which players don't need
//! for seeking — every real-world audio client we care about sends
//! single-range requests. Returning 416 for multi-range is RFC-allowed.

use std::ops::RangeInclusive;

#[derive(Debug, PartialEq, Eq)]
pub enum RangeParseError {
    /// Header was syntactically malformed (`Range: bananas`, missing
    /// `bytes=`, etc.). HTTP requires we ignore it and serve the whole
    /// body — caller decides whether to honour that.
    Malformed,
    /// Syntactically valid but unsatisfiable against the file size
    /// (start >= size, or `bytes=0-0` against an empty file). Caller
    /// should respond `416 Range Not Satisfiable`.
    Unsatisfiable,
}

/// Parse a `Range` header against a known file size. Returns an
/// **inclusive** byte range `[start, end]`. The `total` argument is the
/// file's total length.
pub fn parse_range(header: &str, total: u64) -> Result<RangeInclusive<u64>, RangeParseError> {
    let raw = header.trim();
    let spec = raw
        .strip_prefix("bytes=")
        .ok_or(RangeParseError::Malformed)?
        .trim();
    if spec.is_empty() || spec.contains(',') {
        return Err(RangeParseError::Malformed);
    }

    let (lhs, rhs) = spec.split_once('-').ok_or(RangeParseError::Malformed)?;
    let (lhs, rhs) = (lhs.trim(), rhs.trim());

    match (lhs.is_empty(), rhs.is_empty()) {
        // bytes=-N — suffix length: last N bytes.
        (true, false) => {
            let n: u64 = rhs.parse().map_err(|_| RangeParseError::Malformed)?;
            if n == 0 || total == 0 {
                return Err(RangeParseError::Unsatisfiable);
            }
            let n = n.min(total);
            Ok((total - n)..=(total - 1))
        }
        // bytes=N- — from N to EOF.
        (false, true) => {
            let start: u64 = lhs.parse().map_err(|_| RangeParseError::Malformed)?;
            if start >= total {
                return Err(RangeParseError::Unsatisfiable);
            }
            Ok(start..=(total - 1))
        }
        // bytes=A-B — explicit window.
        (false, false) => {
            let start: u64 = lhs.parse().map_err(|_| RangeParseError::Malformed)?;
            let end: u64 = rhs.parse().map_err(|_| RangeParseError::Malformed)?;
            if start > end || start >= total {
                return Err(RangeParseError::Unsatisfiable);
            }
            // Clip the upper bound; RFC 7233 §2.1 says "if the last byte
            // is greater than or equal to current length, the last byte
            // is taken to be one less than current length".
            let end = end.min(total - 1);
            Ok(start..=end)
        }
        (true, true) => Err(RangeParseError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_window() {
        assert_eq!(parse_range("bytes=0-99", 1000), Ok(0..=99));
        assert_eq!(parse_range("bytes=100-199", 1000), Ok(100..=199));
    }

    #[test]
    fn open_ended_to_eof() {
        assert_eq!(parse_range("bytes=500-", 1000), Ok(500..=999));
    }

    #[test]
    fn suffix_length() {
        assert_eq!(parse_range("bytes=-200", 1000), Ok(800..=999));
        // suffix larger than file clamps to the whole file
        assert_eq!(parse_range("bytes=-5000", 1000), Ok(0..=999));
    }

    #[test]
    fn upper_bound_clipped() {
        // RFC: end >= size → take size-1
        assert_eq!(parse_range("bytes=0-99999", 1000), Ok(0..=999));
    }

    #[test]
    fn unsatisfiable() {
        assert_eq!(parse_range("bytes=1000-1100", 1000), Err(RangeParseError::Unsatisfiable));
        assert_eq!(parse_range("bytes=-0", 1000), Err(RangeParseError::Unsatisfiable));
        assert_eq!(parse_range("bytes=-1", 0), Err(RangeParseError::Unsatisfiable));
    }

    #[test]
    fn malformed() {
        assert_eq!(parse_range("octets=0-1", 1000), Err(RangeParseError::Malformed));
        assert_eq!(parse_range("bytes=abc-def", 1000), Err(RangeParseError::Malformed));
        assert_eq!(parse_range("bytes=", 1000), Err(RangeParseError::Malformed));
        assert_eq!(parse_range("bytes=-", 1000), Err(RangeParseError::Malformed));
        // multi-range rejected
        assert_eq!(parse_range("bytes=0-9,20-29", 1000), Err(RangeParseError::Malformed));
    }

    #[test]
    fn whitespace_tolerated() {
        assert_eq!(parse_range("  bytes= 0 - 99 ", 1000), Ok(0..=99));
    }
}

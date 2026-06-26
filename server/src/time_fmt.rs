//! Timestamp formatting shared by the gRPC + REST serializers.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Serialize a timestamp as RFC 3339 (`2026-06-24T23:31:00Z`) so the client's
/// JavaScript `new Date()` / `Date.parse` can read it.
///
/// `OffsetDateTime`'s `Display` (what `.to_string()` calls) emits a
/// space-separated form with a seconds-precision offset
/// (`2026-06-24 23:31:00.0 +00:00:00`) that every JS engine rejects as `NaN` —
/// which silently blanked every date the UI tried to render. Falls back to
/// `Display` on a formatting error, which a UTC database timestamp never hits.
pub(crate) fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::rfc3339;
    use time::OffsetDateTime;

    #[test]
    fn emits_a_js_parseable_timestamp() {
        let t = OffsetDateTime::from_unix_timestamp(1_782_689_460).unwrap();
        let s = rfc3339(t);
        // The `time` `Display` form (`… 23:31:00.0 +00:00:00`) makes JS
        // `Date.parse` return NaN; RFC 3339 uses a `T` separator and a
        // minute-precision offset every engine accepts.
        assert!(s.contains('T'), "want a T separator, got {s:?}");
        assert!(!s.contains(' '), "want no spaces, got {s:?}");
        assert!(!s.contains("+00:00:00"), "want a minute-precision offset, got {s:?}");
    }
}

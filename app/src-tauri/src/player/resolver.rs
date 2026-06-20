//! URL helper for the `media://` protocol.
//!
//! Tauri maps a registered `media` scheme to different *actual* URLs per
//! platform (see `register_asynchronous_uri_scheme_protocol` docs):
//!
//! * macOS / iOS / Linux: `media://localhost/<id>`
//! * Windows / Android:   `http://media.localhost/<id>`
//!
//! The frontend can't know which without an OS probe, so we expose a
//! single command that returns the right shape. The path is just the
//! track id — the protocol handler in [`super::stream`] parses it back out.

/// Build the platform-correct media URL for a track id.
pub fn media_url(track_id: &str) -> String {
    // URL-encode the id defensively — server UUIDs are URL-safe, but a
    // stray `/` would be read as a path segment by the handler.
    let encoded = urlencoding_encode(track_id);
    if cfg!(target_os = "windows") || cfg!(target_os = "android") {
        format!("http://media.localhost/{encoded}")
    } else {
        format!("media://localhost/{encoded}")
    }
}

/// Minimal percent-encoding for the subset of bytes that aren't URL-safe
/// in a path segment. Avoids pulling in another crate for one call site.
fn urlencoding_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_unsafe_chars() {
        assert_eq!(urlencoding_encode("abc-123"), "abc-123");
        assert_eq!(urlencoding_encode("a/b"), "a%2Fb");
        assert_eq!(urlencoding_encode("a b"), "a%20b");
    }

    #[test]
    fn url_has_track_id() {
        let url = media_url("01234567-89ab-cdef-0123-456789abcdef");
        assert!(url.ends_with("01234567-89ab-cdef-0123-456789abcdef"));
    }
}

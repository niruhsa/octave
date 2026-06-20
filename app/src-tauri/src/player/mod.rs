//! Playback (Phase 4).
//!
//! Two concerns live here:
//!
//! * [`stream`] — a custom `media://` URI-scheme protocol the webview's
//!   `<audio>` element loads. Per request it resolves the track id to
//!   either a local downloaded file (served with HTTP range semantics) or
//!   a proxied server stream (`GET /tracks/{id}/stream`) with the auth
//!   header injected and the `Range` header forwarded. Centralising this
//!   in Rust means the frontend never branches on online/offline and
//!   never has to handle credentials in the webview.
//! * [`resolver`] — `player_media_url`, the platform-correct URL string
//!   for a given track id (scheme + host differ across macOS/Linux/iOS
//!   vs Windows/Android).
//!
//! Source resolution rule (matches the offline-cache principle): **prefer
//! the local file when downloaded, else stream from the server.** When
//! the server is unreachable and the track isn't cached, the protocol
//! returns 502 so the `<audio>` element surfaces an error to the UI.

pub mod resolver;
pub mod stream;

pub use resolver::media_url;

//! Playback (Phase 4).
//!
//! [`server`] runs an in-app loopback HTTP server that the webview's `<audio>`
//! element streams from (`http://127.0.0.1:<port>/s/<token>/<id>`). Per request
//! it serves a downloaded local file with range support, or proxies the server
//! stream (`GET /tracks/{id}/stream`) with the auth header injected and the body
//! streamed straight through — so playback starts fast and works identically
//! online/offline without the frontend branching on either.
//!
//! Source-resolution rule (matches the offline-cache principle): **prefer the
//! local file when downloaded, else stream from the server.** When the server
//! is unreachable and the track isn't cached, the server returns 502 so the
//! `<audio>` element surfaces an error to the UI.
//!
//! See [`server`]'s module docs for why this replaced the old `media://`
//! custom protocol (which can't stream, and is unusable for media on Android).

pub mod server;

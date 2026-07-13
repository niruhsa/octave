//! Graceful-shutdown coordination.
//!
//! A single [`watch`] channel fans one OS signal out to *every* part of the
//! server that needs to wind down: both transports' graceful-shutdown futures
//! and the long-lived live-progress streams (the uploads WebSocket and the
//! gRPC `StreamUploads` broadcast). Those streams never end on their own, so
//! without an explicit signal a graceful drain would block forever — which is
//! exactly why Ctrl-C used to hang until the process was force-killed.
//!
//! `false` = running, `true` = shutting down. The value is latched on, so a
//! receiver that subscribes (or checks) after the flip still observes it.

use tokio::sync::watch;
use tracing::warn;

/// Sender half — held by the signal listener; flipping it to `true` once
/// triggers shutdown everywhere.
pub type ShutdownTx = watch::Sender<bool>;

/// Receiver half — cloned into each transport and live stream.
pub type ShutdownRx = watch::Receiver<bool>;

/// Create a fresh shutdown channel in the "running" state.
pub fn channel() -> (ShutdownTx, ShutdownRx) {
    watch::channel(false)
}

/// Resolve once shutdown has been requested — immediately if it already has,
/// otherwise when the flag flips. A dropped sender also counts as shutdown
/// (the `Err` from `changed()` is intentionally ignored).
pub async fn wait_for_shutdown(mut rx: ShutdownRx) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}

/// Resolve on the first OS shutdown signal: `SIGINT` (Ctrl-C) or `SIGTERM`
/// (`kill`, `docker stop`, systemd) on Unix; Ctrl-C elsewhere.
pub async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(e) => {
                warn!(error = %e, "could not install SIGTERM handler; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

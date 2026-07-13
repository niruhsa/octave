//! Account-scoped equalizer domain, persistence, resolution, and sync.
//!
//! The server owns the acknowledged synced layer.  Device-only profiles and
//! preferences live in a separate SQLite layer, and queued mutations are
//! materialized over the clean mirror for offline reads.

pub mod audio_output;
pub mod model;
pub mod ops;
pub mod parser;
pub mod repo;
pub mod resolver;
pub mod service;

pub use model::*;
pub use service::EqualizerService;

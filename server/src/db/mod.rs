//! Persistence layer.
//!
//! Postgres is the server's authoritative store (the client mirrors a subset
//! into SQLite for offline use). The schema in `migrations/` is deliberately
//! portable: UUIDs, TEXT-with-CHECK enums, JSON-as-TEXT — every column maps
//! cleanly to SQLite for the client cache.
//!
//! Structure:
//! - [`models`] plain data types shared by repositories
//! - [`repo`]   repository traits (testable, swappable)
//! - [`pg`]     Postgres implementations of the repository traits
//! - [`pool`]   connection-pool construction + migration runner

mod equalizer_pg;
pub mod models;
pub mod pg;
pub mod pool;
pub mod repo;

pub use pool::{connect, run_migrations};

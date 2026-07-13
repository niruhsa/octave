//! Music server crate root.
//!
//! Modules:
//! - [`config`]   environment-driven runtime configuration
//! - [`error`]    central `AppError` + `Result` alias
//! - [`db`]       persistence layer (Phase 1+)
//! - [`services`] business-logic services (Phase 3+)
//! - [`grpc`]     gRPC transport (primary)
//! - [`rest`]     REST transport (fallback)

pub mod auth;
pub mod config;
pub mod db;
mod equalizer_core;
pub mod error;
pub mod grpc;
pub mod rest;
pub mod services;
pub mod shutdown;
pub(crate) mod time_fmt;

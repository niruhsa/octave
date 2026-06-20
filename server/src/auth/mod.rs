//! Authentication & authorization.
//!
//! Two credential mechanisms — both feed into the same [`Identity`] enum that
//! services use for permission checks:
//!
//! 1. **`SECRET_KEY`**: pre-shared key from config; presented as
//!    `Authorization: SecretKey <key>`. Treated as effective **Admin**.
//! 2. **Username/password → session token**: presented as
//!    `Authorization: Bearer <token>`. Resolves to the owning user's identity
//!    and permission tier.
//!
//! Services must call [`Identity::require`] with the minimum tier they need
//! (defense in depth — the transport layer rejects unauthenticated traffic,
//! but services don't trust the transport).

pub mod identity;
pub mod password;
pub mod service;
pub mod token;

pub use identity::{Identity, RequireExt};
pub use service::AuthService;

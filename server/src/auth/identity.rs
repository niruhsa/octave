//! Caller identity + permission-tier enforcement.

use uuid::Uuid;

use crate::db::models::PermissionLevel;
use crate::error::{AppError, Result};

/// Who is making a request, as resolved by the auth layer.
#[derive(Debug, Clone)]
pub enum Identity {
    /// `SECRET_KEY` authentication. Treated as effective Admin.
    SecretKey,
    /// A logged-in user with a known permission tier.
    User {
        id: Uuid,
        username: String,
        level: PermissionLevel,
    },
}

impl Identity {
    /// The effective permission tier of this identity.
    pub fn level(&self) -> PermissionLevel {
        match self {
            Identity::SecretKey => PermissionLevel::Admin,
            Identity::User { level, .. } => *level,
        }
    }

    /// `Some(user_id)` for user identities, `None` for `SECRET_KEY`.
    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            Identity::SecretKey => None,
            Identity::User { id, .. } => Some(*id),
        }
    }

    /// Enforce a minimum permission tier. Returns `PermissionDenied` otherwise.
    pub fn require(&self, required: PermissionLevel) -> Result<()> {
        if self.level().satisfies(required) {
            Ok(())
        } else {
            Err(AppError::PermissionDenied(format!(
                "requires {required:?}, identity has {:?}",
                self.level()
            )))
        }
    }
}

/// Convenience for `Option<&Identity>` (the transport layer always populates
/// the request extension, but services should still fail closed if absent).
pub trait RequireExt<'a> {
    fn require(self, required: PermissionLevel) -> Result<&'a Identity>;
}

impl<'a> RequireExt<'a> for Option<&'a Identity> {
    fn require(self, required: PermissionLevel) -> Result<&'a Identity> {
        match self {
            Some(id) => {
                id.require(required)?;
                Ok(id)
            }
            None => Err(AppError::Unauthenticated("no identity present".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn user(level: PermissionLevel) -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level,
        }
    }

    #[test]
    fn tier_inheritance() {
        let admin = user(PermissionLevel::Admin);
        let manager = user(PermissionLevel::Manager);
        let plain = user(PermissionLevel::User);

        // Admin satisfies every tier.
        assert!(admin.require(PermissionLevel::Admin).is_ok());
        assert!(admin.require(PermissionLevel::Manager).is_ok());
        assert!(admin.require(PermissionLevel::User).is_ok());

        // Manager satisfies Manager + User but not Admin.
        assert!(manager.require(PermissionLevel::Admin).is_err());
        assert!(manager.require(PermissionLevel::Manager).is_ok());
        assert!(manager.require(PermissionLevel::User).is_ok());

        // User satisfies only User.
        assert!(plain.require(PermissionLevel::Admin).is_err());
        assert!(plain.require(PermissionLevel::Manager).is_err());
        assert!(plain.require(PermissionLevel::User).is_ok());
    }

    #[test]
    fn secret_key_is_admin() {
        let id = Identity::SecretKey;
        assert!(id.require(PermissionLevel::Admin).is_ok());
        assert_eq!(id.user_id(), None);
    }
}

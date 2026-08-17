//! Environment variable secret backend.
//!
//! This is the one backend in this crate that genuinely does what it says: it
//! reads a named variable from the daemon's own environment at use time. Note
//! the inherent limit — the value is already in the process environment in
//! plaintext, readable by anything that can read `/proc/self/environ` or call
//! `environ`. This backend does not make it more secret; it makes it *leased*,
//! so its use is audited and its lifetime in this crate's memory is bounded.

use async_trait::async_trait;
use zeroize::Zeroize;

use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeasedSecret};
use crate::{BackendErrorCode, SecretError};

/// Secret backend that resolves credentials from environment variables at use time.
#[derive(Debug, Default, Clone)]
pub struct EnvironmentBackend;

impl EnvironmentBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecretBackend for EnvironmentBackend {
    fn kind(&self) -> SecretBackendKind {
        SecretBackendKind::Environment
    }

    async fn resolve(
        &self,
        locator: &str,
        _context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        let var_name = locator.strip_prefix("env:").unwrap_or(locator);
        let missing = || {
            SecretError::backend(
                BackendErrorCode::EnvMissing,
                "the environment variable named by this locator is unset or empty",
            )
        };
        let Ok(mut value) = std::env::var(var_name) else {
            return Err(missing());
        };
        if value.trim().is_empty() {
            value.zeroize();
            return Err(missing());
        }
        let leased = LeasedSecret::from_text(&value);
        // The `String` handed back by `std::env::var` is a fresh allocation and
        // is ours to scrub. The copy inside the process environment is not, and
        // cannot be.
        value.zeroize();
        Ok(leased)
    }
}

//! HashiCorp Vault secret backend.
//!
//! # Status: not implemented, and it says so
//!
//! There is no Vault client here — no HTTP transport, no auth method, no token
//! renewal, no lease TTL tracking. What used to be here was a `HashMap<String,
//! String>` of plaintext values with a boolean "outage" flag, which meant its
//! "unreachable service" and "lease revoked" behaviours were simulated and its
//! secret values sat in never-zeroized `String`s for the life of the process.
//!
//! That has been removed rather than dressed up. [`VaultBackend`] now refuses
//! every resolve with [`BackendErrorCode::NotConfigured`], so a caller cannot
//! mistake it for a working Vault integration and cannot be told "your secret
//! is not in Vault" by something that never asked Vault.
//!
//! Implementing this for real means: a `reqwest` client against
//! `VAULT_ADDR`, an auth method (AppRole / Kubernetes / token), `GET
//! /v1/<mount>/data/<path>` for KV-v2, the `lease_id` from the response
//! recorded as the lease's `backend_lease_handle`, and `PUT
//! /v1/sys/leases/revoke` in [`SecretBackend::revoke`]. Until that exists, the
//! honest answer is a refusal.

use std::fmt;

use async_trait::async_trait;

use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeasedSecret};
use crate::{BackendErrorCode, SecretError};

const NOT_CONFIGURED: &str =
    "no Vault client is configured in this build; refusing rather than reporting a miss";

/// Secret backend for HashiCorp Vault. Currently a refusal.
///
/// It holds no state at all: there is nowhere for a plaintext value to live,
/// and nothing to mistake for a populated store.
#[derive(Default)]
pub struct VaultBackend {
    _private: (),
}

impl VaultBackend {
    /// Construct the Vault backend. It will refuse every operation until a real
    /// client exists.
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Whether this backend can actually reach Vault. Always `false` today.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        false
    }
}

impl fmt::Debug for VaultBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultBackend")
            .field("configured", &false)
            .finish()
    }
}

#[async_trait]
impl SecretBackend for VaultBackend {
    fn kind(&self) -> SecretBackendKind {
        SecretBackendKind::Vault
    }

    async fn resolve(
        &self,
        _locator: &str,
        _context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        Err(SecretError::backend(
            BackendErrorCode::NotConfigured,
            NOT_CONFIGURED,
        ))
    }

    async fn revoke(&self, _backend_lease_handle: &str) -> Result<(), SecretError> {
        // Refuse rather than reporting success: a caller that believes a Vault
        // lease was revoked when nothing was contacted is worse off than one
        // that is told the revocation did not happen.
        Err(SecretError::backend(
            BackendErrorCode::NotConfigured,
            NOT_CONFIGURED,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn vault_refuses_and_never_reports_a_miss() {
        let backend = VaultBackend::new();
        assert!(!backend.is_configured());
        let ctx = LeaseContext::new(1000, "job", "cap");
        let err = backend
            .resolve("secret/data/codypendent/api_key", &ctx)
            .await
            .expect_err("an unimplemented Vault backend must refuse");
        assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
        assert_ne!(err.outcome_code(), "secrets.backend-secret-not-found");
    }

    #[tokio::test]
    async fn vault_revoke_does_not_claim_success() {
        let backend = VaultBackend::new();
        let err = backend
            .revoke("vault-lease-id-123")
            .await
            .expect_err("revocation against no client must not report success");
        assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
    }
}

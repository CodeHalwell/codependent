//! The SecretBackend trait and supported backend kinds.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::lease::{LeaseContext, LeasedSecret};
use crate::SecretError;

/// Supported secret backends matching the 0045_secret_broker schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackendKind {
    Environment,
    Keychain,
    Managed,
    Vault,
    WorkloadIdentity,
}

impl SecretBackendKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Keychain => "keychain",
            Self::Managed => "managed",
            Self::Vault => "vault",
            Self::WorkloadIdentity => "workload_identity",
        }
    }

    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "environment" => Some(Self::Environment),
            "keychain" => Some(Self::Keychain),
            "managed" => Some(Self::Managed),
            "vault" => Some(Self::Vault),
            "workload_identity" => Some(Self::WorkloadIdentity),
            _ => None,
        }
    }
}

/// A backend capable of resolving secret locators into leased secret material.
#[async_trait]
pub trait SecretBackend: Send + Sync {
    /// The kind of backend.
    fn kind(&self) -> SecretBackendKind;

    /// Resolve credential material for the given locator and lease context.
    async fn resolve(
        &self,
        locator: &str,
        context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError>;

    /// Revoke a lease handle at the backend (e.g. Vault lease ID).
    async fn revoke(&self, backend_lease_handle: &str) -> Result<(), SecretError> {
        let _ = backend_lease_handle;
        Ok(())
    }
}

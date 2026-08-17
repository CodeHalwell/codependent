//! Platform keychain secret backend.
//!
//! # Status: not implemented, and it says so
//!
//! There is no Security-framework client here, no libsecret client, and no
//! Windows Credential Manager client. What used to be here was a
//! `HashMap<String, String>` of plaintext values plus a `supported: bool`
//! flag, so "the platform keychain is unavailable" was a constructor argument
//! rather than a fact about the platform, and a `supported: true` backend
//! answered "keychain item not found" for every lookup without ever consulting
//! a keychain.
//!
//! Both halves of that are gone. [`KeychainBackend::for_current_platform`]
//! decides support from `cfg!(target_os)` — not from a caller-supplied
//! boolean — and on a platform with a keychain it still refuses, with
//! [`BackendErrorCode::NotConfigured`], because the client does not exist yet.
//! No code path can report a miss it did not observe.

use std::fmt;

use async_trait::async_trait;

use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeasedSecret};
use crate::{BackendErrorCode, SecretError};

/// Whether the host OS has a keychain this backend could ever talk to.
///
/// Derived from the compilation target, so it cannot be spoofed by a caller.
const fn platform_has_keychain() -> bool {
    cfg!(any(target_os = "macos", target_os = "ios"))
}

/// Secret backend for the platform keychain or local secure store.
///
/// Holds no state: there is nowhere for a plaintext value to live.
#[derive(Default)]
pub struct KeychainBackend {
    _private: (),
}

impl KeychainBackend {
    /// Construct the keychain backend for the platform this binary was built
    /// for. There is deliberately no way to assert support the platform does
    /// not have.
    #[must_use]
    pub fn for_current_platform() -> Self {
        Self { _private: () }
    }

    /// Whether the host platform has a keychain at all.
    #[must_use]
    pub fn platform_supported(&self) -> bool {
        platform_has_keychain()
    }

    /// Whether this backend can actually read the platform keychain. Always
    /// `false` today: the client is not written.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        false
    }
}

impl fmt::Debug for KeychainBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeychainBackend")
            .field("platform_supported", &platform_has_keychain())
            .field("configured", &false)
            .finish()
    }
}

#[async_trait]
impl SecretBackend for KeychainBackend {
    fn kind(&self) -> SecretBackendKind {
        SecretBackendKind::Keychain
    }

    async fn resolve(
        &self,
        _locator: &str,
        _context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        if platform_has_keychain() {
            return Err(SecretError::backend(
                BackendErrorCode::NotConfigured,
                "no platform keychain client is configured in this build; refusing rather than reporting a miss",
            ));
        }
        Err(SecretError::backend(
            BackendErrorCode::KeychainUnsupported,
            "this platform has no keychain; refusing rather than falling back to another source",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keychain_refuses_and_never_reports_a_miss() {
        let backend = KeychainBackend::for_current_platform();
        assert!(!backend.is_configured());
        let ctx = LeaseContext::new(1000, "job", "cap");
        let err = backend
            .resolve("codypendent/github", &ctx)
            .await
            .expect_err("an unimplemented keychain backend must refuse");

        // Which refusal depends on the build target, but it is always a refusal
        // and never "the item is not there".
        let code = err.outcome_code();
        assert!(
            code == "secrets.backend-not-configured" || code == "secrets.keychain-unsupported",
            "unexpected code {code}"
        );
        assert_ne!(code, "secrets.backend-secret-not-found");
    }

    #[test]
    fn platform_support_is_derived_from_the_target_not_from_a_caller() {
        let backend = KeychainBackend::for_current_platform();
        assert_eq!(
            backend.platform_supported(),
            cfg!(any(target_os = "macos", target_os = "ios"))
        );
    }
}

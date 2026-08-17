//! Workload Identity secret backend.
//!
//! # Status: a locally-minted shared-secret token, not a federated assertion
//!
//! Read this before believing the backend's name. This backend does **not**
//! talk to an identity provider, does not perform OIDC federation, and does not
//! produce a JWT that any third party can validate. It derives a bearer token
//! with `HMAC-SHA256` over a domain-separated encoding of the audience and the
//! lease context, keyed by an operator-supplied seed. It is only meaningful to
//! a relying party that already holds the same seed.
//!
//! Two properties an operator must know:
//!
//! - The token is **deterministic** for a given (seed, audience, principal,
//!   job, capability). It carries no `iat`, no `exp` and no `jti`, so the lease
//!   TTL recorded by the broker is the only expiry, and it is not enforceable by
//!   the relying party.
//! - The seed is credential-grade. It is zeroized on drop and redacted from
//!   `Debug`; it is never serialized and the backend is not `Clone`.
//!
//! A backend constructed without a seed ([`WorkloadIdentityBackend::unconfigured`],
//! which is also its `Default`) refuses.
//!
//! The previous implementation used bare `SHA-256(seed || message)`, which is
//! length-extension malleable: a holder of one token could derive a valid token
//! for an extended message without knowing the seed. `HMAC-SHA256` closes that.

use std::fmt;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeasedSecret};
use crate::{BackendErrorCode, SecretError};

type HmacSha256 = Hmac<Sha256>;

const SEED_LEN: usize = 32;

/// The operator-supplied token signing seed. Zeroized on drop, redacted in `Debug`.
struct SigningSeed([u8; SEED_LEN]);

impl Drop for SigningSeed {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SigningSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningSeed(\"<redacted>\")")
    }
}

/// Secret backend that mints audience-bound workload tokens from a shared seed.
pub struct WorkloadIdentityBackend {
    seed: Option<SigningSeed>,
}

impl fmt::Debug for WorkloadIdentityBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadIdentityBackend")
            .field("configured", &self.seed.is_some())
            .finish()
    }
}

/// Fail closed: an unseeded backend refuses rather than minting a token from a
/// default seed.
impl Default for WorkloadIdentityBackend {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl WorkloadIdentityBackend {
    /// A backend with no seed. Every resolve refuses.
    #[must_use]
    pub fn unconfigured() -> Self {
        Self { seed: None }
    }

    /// A backend seeded with 32 operator-supplied random bytes.
    ///
    /// # Errors
    ///
    /// Refuses a seed that is a single repeated byte: a constant seed makes
    /// every minted token forgeable by anyone who reads this source.
    pub fn with_signing_seed(signing_seed: [u8; SEED_LEN]) -> Result<Self, SecretError> {
        let first = signing_seed[0];
        if signing_seed.iter().all(|b| *b == first) {
            let mut rejected = signing_seed;
            rejected.zeroize();
            return Err(SecretError::backend(
                BackendErrorCode::InvalidKeyMaterial,
                "workload identity seed must be 32 random bytes, not a repeated constant",
            ));
        }
        Ok(Self {
            seed: Some(SigningSeed(signing_seed)),
        })
    }

    /// Whether this backend has a seed and can mint tokens.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.seed.is_some()
    }
}

#[async_trait]
impl SecretBackend for WorkloadIdentityBackend {
    fn kind(&self) -> SecretBackendKind {
        SecretBackendKind::WorkloadIdentity
    }

    async fn resolve(
        &self,
        locator: &str,
        context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        let Some(seed) = self.seed.as_ref() else {
            return Err(SecretError::backend(
                BackendErrorCode::NotConfigured,
                "the workload identity backend has no signing seed; it refuses to mint a token",
            ));
        };

        let audience = locator.trim();
        if audience.is_empty() {
            return Err(SecretError::backend(
                BackendErrorCode::InvalidLocator,
                "workload identity locator must name a non-empty audience",
            ));
        }

        let mut mac = <HmacSha256 as Mac>::new_from_slice(&seed.0).map_err(|_| {
            SecretError::backend(
                BackendErrorCode::Internal,
                "workload identity MAC init failed",
            )
        })?;
        mac.update(b"codypendent-workload-token-v2\n");
        mac.update(audience.as_bytes());
        mac.update(b"\n");
        mac.update(context.principal_uid.to_string().as_bytes());
        mac.update(b"\n");
        mac.update(context.job_id.as_bytes());
        mac.update(b"\n");
        mac.update(context.capability.as_bytes());
        mac.update(b"\n");
        let tag = mac.finalize().into_bytes();

        let mut token = format!(
            "wit_{}_{}",
            hex::encode(&tag[..16]),
            hex::encode(audience.as_bytes())
        );
        let leased = LeasedSecret::from_text(&token);
        token.zeroize();
        Ok(leased)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> [u8; SEED_LEN] {
        let mut s = [0u8; SEED_LEN];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(11).wrapping_add(3);
        }
        s
    }

    #[tokio::test]
    async fn unconfigured_refuses_to_mint() {
        let backend = WorkloadIdentityBackend::default();
        assert!(!backend.is_configured());
        let ctx = LeaseContext::new(1000, "job", "cap");
        let err = backend
            .resolve("api://codypendent", &ctx)
            .await
            .expect_err("an unseeded backend must refuse");
        assert_eq!(err.outcome_code(), "secrets.backend-not-configured");
    }

    #[test]
    fn a_repeated_constant_seed_is_refused() {
        let err = WorkloadIdentityBackend::with_signing_seed([0x24; SEED_LEN])
            .expect_err("placeholder seed must be refused");
        assert_eq!(err.outcome_code(), "secrets.invalid-key-material");
    }

    #[tokio::test]
    async fn tokens_are_bound_to_audience_and_context() {
        let backend = WorkloadIdentityBackend::with_signing_seed(seed()).expect("seeded");
        let a = LeaseContext::new(1000, "job-a", "cap");
        let b = LeaseContext::new(1000, "job-b", "cap");

        let t1 = backend.resolve("api://one", &a).await.expect("mint");
        let t2 = backend.resolve("api://two", &a).await.expect("mint");
        let t3 = backend.resolve("api://one", &b).await.expect("mint");

        assert_ne!(t1.expose(), t2.expose(), "audience must change the token");
        assert_ne!(t1.expose(), t3.expose(), "job id must change the token");
        assert!(t1.expose_str().unwrap().starts_with("wit_"));
    }

    #[tokio::test]
    async fn an_empty_audience_is_refused() {
        let backend = WorkloadIdentityBackend::with_signing_seed(seed()).expect("seeded");
        let ctx = LeaseContext::new(1000, "job", "cap");
        let err = backend
            .resolve("   ", &ctx)
            .await
            .expect_err("empty audience must be refused");
        assert_eq!(err.outcome_code(), "secrets.invalid-locator");
    }

    #[test]
    fn debug_never_shows_the_seed() {
        let backend = WorkloadIdentityBackend::with_signing_seed(seed()).expect("seeded");
        assert_eq!(
            format!("{backend:?}"),
            "WorkloadIdentityBackend { configured: true }"
        );
        assert_eq!(
            format!("{:?}", SigningSeed(seed())),
            "SigningSeed(\"<redacted>\")"
        );
    }
}

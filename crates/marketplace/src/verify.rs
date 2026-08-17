//! Package verification: SHA-256 digest + Ed25519 signature against TrustedPublishers + UnsignedPolicy::Deny.
//!
//! Enforces:
//! - Default-deny posture for unsigned packages (`UnsignedPolicy::Deny`).
//! - Checksum verification occurs before signature check.
//! - Publisher signature is verified against ed25519 public key over the whole-manifest canonical signing digest.
//! - Publisher must be trusted in the provided [`TrustedPublishers`] store (or have an allowlisted key).

use codypendent_sandbox::{
    parse_manifest, verify_artifact, PluginManifest, TrustedPublishers, UnsignedPolicy, Verified,
};

use crate::error::MarketplaceError;

/// Verification helper for marketplace packages.
#[derive(Debug, Clone)]
pub struct PackageVerifier {
    unsigned_policy: UnsignedPolicy,
}

impl Default for PackageVerifier {
    fn default() -> Self {
        Self {
            unsigned_policy: UnsignedPolicy::Deny,
        }
    }
}

impl PackageVerifier {
    /// Create a new verifier with default-deny unsigned policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a verifier with an explicit unsigned policy (e.g. for development mode).
    #[must_use]
    pub fn with_unsigned_policy(unsigned_policy: UnsignedPolicy) -> Self {
        Self { unsigned_policy }
    }

    /// Verify a package artifact against its raw TOML manifest and a trust store.
    ///
    /// Steps:
    /// 1. Parse and validate the manifest TOML.
    /// 2. Look up the publisher's public key from [`TrustedPublishers`].
    /// 3. Verify artifact SHA-256 checksum and Ed25519 signature over canonical signing digest.
    pub fn verify(
        &self,
        manifest_toml: &str,
        artifact: &[u8],
        trust_store: &TrustedPublishers,
    ) -> Result<(PluginManifest, Verified), MarketplaceError> {
        let manifest = parse_manifest(manifest_toml)?;
        let publisher_key = trust_store
            .key_for(&manifest.publisher)
            .map(|key| key.as_slice());

        let verified = verify_artifact(&manifest, artifact, publisher_key, self.unsigned_policy)?;

        Ok((manifest, verified))
    }

    /// Verify a package artifact against a pre-parsed manifest and optional raw 32-byte public key.
    pub fn verify_with_key(
        &self,
        manifest: &PluginManifest,
        artifact: &[u8],
        publisher_key: Option<&[u8]>,
    ) -> Result<Verified, MarketplaceError> {
        let verified = verify_artifact(manifest, artifact, publisher_key, self.unsigned_policy)?;
        Ok(verified)
    }
}

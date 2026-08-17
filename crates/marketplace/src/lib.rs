//! codypendent-marketplace — durable marketplace distribution, package verification, trust, and lifecycle (Milestone 5).
//!
//! Enforces:
//! - Package verification: SHA-256 digest + Ed25519 signature over canonical signing digest against [`TrustedPublishers`].
//! - Default-deny unsigned policy ([`UnsignedPolicy::Deny`]).
//! - Publisher trust is DISTINCT from registry trust.
//! - Host-computed compatibility (a package cannot assert its own compatibility).
//! - Safe archive extraction (enforces size, count, ratio <= [`MAX_COMPRESSION_RATIO`], path safety, no symlinks/hardlinks).
//! - Installation NEVER enables executable code automatically (`InstalledDisabled` -> `SmokeTested` -> `Enabled` -> `Revoked`).
//! - Permission expansion detection requiring human approval receipts.
//! - Retroactive revocation disabling installed packages and invalidating pending receipts.
//! - Hidden-package non-disclosure.

pub mod catalog;
pub mod compatibility;
pub mod distribution;
pub mod error;
pub mod lifecycle;
pub mod permission;
pub mod store;
pub mod trust;
pub mod verify;

pub use catalog::MarketplaceCatalog;
pub use compatibility::CompatibilityChecker;
pub use distribution::{ContentAddressedStore, DownloadAllowlist};
pub use error::MarketplaceError;
pub use lifecycle::MarketplaceLifecycleManager;
pub use permission::PermissionEvaluation;
pub use store::{
    InstallLifecycleState, MarketplaceInstall, MarketplacePackage, MarketplacePermissionReceipt,
    MarketplacePublisher, MarketplaceRevocation, MarketplaceStore, MarketplaceVersion,
    PublisherTrustTier,
};
pub use trust::TrustManager;
pub use verify::PackageVerifier;

// Re-exports from sandbox for convenience
pub use codypendent_sandbox::package::{
    MAX_ARCHIVE_DIRECTORIES, MAX_ARCHIVE_ENTRIES, MAX_ARCHIVE_PATH_BYTES, MAX_ARCHIVE_PATH_DEPTH,
    MAX_COMPRESSION_RATIO, MAX_PACKAGE_ARCHIVE_BYTES, MAX_PACKAGE_BYTES, MAX_PACKAGE_FILES,
    MAX_PACKAGE_FILE_BYTES,
};
pub use codypendent_sandbox::{
    checksum_of, parse_manifest, signing_digest, verify_artifact, InstalledPlugin, LifecycleError,
    LifecycleState, PluginManifest, TrustStoreError, TrustTier, TrustedPublishers, UnsignedPolicy,
    Verified, VerifyError,
};

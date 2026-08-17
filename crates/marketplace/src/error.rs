//! Error types for the marketplace crate.

use codypendent_sandbox::{LifecycleError, ManifestError, PackageError, VerifyError};

#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error("package verification failed: {0}")]
    Verify(#[from] VerifyError),

    #[error("lifecycle error: {0}")]
    Lifecycle(#[from] LifecycleError),

    #[error("package error: {0}")]
    Package(#[from] PackageError),

    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),

    #[error("marketplace store error: {0}")]
    Store(String),

    #[error("publisher `{0}` not found")]
    PublisherNotFound(String),

    #[error("package `{0}` not found")]
    PackageNotFound(String),

    #[error("package `{package_id}` version `{version}` not found")]
    VersionNotFound { package_id: String, version: String },

    #[error("install `{0}` not found")]
    InstallNotFound(String),

    #[error("permission receipt `{0}` not found, already decided, or invalidated")]
    ReceiptNotFoundOrInvalid(String),

    #[error("publisher `{0}` is not trusted and policy denies untrusted packages")]
    UntrustedPublisher(String),

    #[error("publisher `{publisher}` is revoked: {reason}")]
    RevokedPublisher { publisher: String, reason: String },

    #[error("package `{package}` is revoked: {reason}")]
    RevokedPackage { package: String, reason: String },

    #[error("daemon version `{current}` is incompatible with package requirements (min: {min:?}, max: {max:?})")]
    IncompatibleDaemonVersion {
        min: Option<String>,
        max: Option<String>,
        current: String,
    },

    #[error("update expands permissions and requires approval with receipt {receipt}:\n{diff}")]
    UpdateExpandsPermissions { diff: String, receipt: String },

    #[error("download disallowed: {0}")]
    DownloadDisallowed(String),

    #[error("invalid marketplace state: {0}")]
    InvalidState(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML decode error: {0}")]
    Toml(#[from] toml::de::Error),
}

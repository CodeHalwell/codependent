//! Host-computed compatibility evaluation (Milestone 5).
//!
//! A package cannot assert its own compatibility; the host computes compatibility
//! against its own daemon version.

use semver::{Version, VersionReq};

use crate::error::MarketplaceError;

/// Evaluates daemon version compatibility for packages.
#[derive(Debug, Clone)]
pub struct CompatibilityChecker {
    daemon_version: Version,
}

impl CompatibilityChecker {
    pub fn new(daemon_version_str: &str) -> Result<Self, MarketplaceError> {
        let daemon_version = Version::parse(daemon_version_str).map_err(|e| {
            MarketplaceError::InvalidState(format!(
                "invalid daemon semver `{daemon_version_str}`: {e}"
            ))
        })?;
        Ok(Self { daemon_version })
    }

    #[must_use]
    pub fn daemon_version(&self) -> &Version {
        &self.daemon_version
    }

    /// Check whether a package version with optional `min_daemon_version` and `max_daemon_version`
    /// is compatible with the host daemon.
    pub fn is_compatible(
        &self,
        min_daemon_version: Option<&str>,
        max_daemon_version: Option<&str>,
    ) -> Result<bool, MarketplaceError> {
        if let Some(min_str) = min_daemon_version {
            let min_req = VersionReq::parse(&format!(">={min_str}")).map_err(|e| {
                MarketplaceError::InvalidState(format!(
                    "invalid min_daemon_version `{min_str}`: {e}"
                ))
            })?;
            if !min_req.matches(&self.daemon_version) {
                return Ok(false);
            }
        }

        if let Some(max_str) = max_daemon_version {
            let max_req = VersionReq::parse(&format!("<={max_str}")).map_err(|e| {
                MarketplaceError::InvalidState(format!(
                    "invalid max_daemon_version `{max_str}`: {e}"
                ))
            })?;
            if !max_req.matches(&self.daemon_version) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Assert compatibility, returning a typed error if incompatible.
    pub fn assert_compatible(
        &self,
        min_daemon_version: Option<&str>,
        max_daemon_version: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        if !self.is_compatible(min_daemon_version, max_daemon_version)? {
            return Err(MarketplaceError::IncompatibleDaemonVersion {
                min: min_daemon_version.map(String::from),
                max: max_daemon_version.map(String::from),
                current: self.daemon_version.to_string(),
            });
        }
        Ok(())
    }
}

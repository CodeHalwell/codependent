//! Package distribution, download security controls, and safe content-addressed extraction (Milestone 5).

use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use codypendent_sandbox::package::{
    atomic_write_once, create_private_dir, extract_package, freeze_package_tree,
    verify_existing_package, PackageError, MAX_PACKAGE_ARCHIVE_BYTES,
};
use url::Url;

use crate::error::MarketplaceError;

/// Allowlist for package download sources.
///
/// The default is the closed one: no domain is allowed and loopback is refused,
/// so a caller must opt every source in explicitly with [`allow_domain`] —
/// there is deliberately no allow-any-domain mode, accidental or otherwise.
///
/// [`allow_domain`]: DownloadAllowlist::allow_domain
#[derive(Debug, Clone, Default)]
pub struct DownloadAllowlist {
    allowed_domains: HashSet<String>,
    allow_localhost: bool,
}

impl DownloadAllowlist {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_domain(&mut self, domain: impl Into<String>) {
        self.allowed_domains.insert(domain.into().to_lowercase());
    }

    pub fn set_allow_localhost(&mut self, allow: bool) {
        self.allow_localhost = allow;
    }

    /// Check if a given URL is permitted for downloading packages.
    pub fn check_url(&self, url_str: &str) -> Result<Url, MarketplaceError> {
        let url = Url::parse(url_str).map_err(|e| {
            MarketplaceError::DownloadDisallowed(format!("malformed URL `{url_str}`: {e}"))
        })?;

        let scheme = url.scheme();
        let host = url
            .host_str()
            .ok_or_else(|| MarketplaceError::DownloadDisallowed("URL has no host".into()))?;

        // Localhost check (for tests/development)
        if (host == "localhost" || host == "127.0.0.1" || host == "::1") && self.allow_localhost {
            if scheme == "http" || scheme == "https" {
                return Ok(url);
            }
            return Err(MarketplaceError::DownloadDisallowed(
                "localhost downloads require http or https scheme".into(),
            ));
        }

        // Production URLs must use HTTPS
        if scheme != "https" {
            return Err(MarketplaceError::DownloadDisallowed(
                "non-HTTPS package download URL is refused".into(),
            ));
        }

        // SSRF check: reject raw IP addresses that are private/loopback/link-local
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_private_or_restricted_ip(&ip) && !self.allow_localhost {
                return Err(MarketplaceError::DownloadDisallowed(
                    "download to private/restricted IP address is refused".into(),
                ));
            }
        }

        // Domain allowlist check. An empty allowlist admits nothing: the guard
        // that used to skip this block when no domain had been opted in turned
        // the documented closed default into an allow-any-HTTPS-host default.
        let host_lower = host.to_lowercase();
        let is_allowed = self
            .allowed_domains
            .iter()
            .any(|allowed| host_lower == *allowed || host_lower.ends_with(&format!(".{allowed}")));
        if !is_allowed {
            return Err(MarketplaceError::DownloadDisallowed(format!(
                "domain `{host}` is not in download allowlist"
            )));
        }

        Ok(url)
    }
}

fn is_private_or_restricted_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

/// Content-addressed local package manager.
#[derive(Debug, Clone)]
pub struct ContentAddressedStore {
    root: PathBuf,
}

impl ContentAddressedStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, MarketplaceError> {
        let root = root.into();
        create_private_dir(&root)?;
        create_private_dir(&root.join("packages"))?;
        create_private_dir(&root.join("artifacts"))?;
        create_private_dir(&root.join("tmp"))?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn package_dir(&self, content_hash: &str) -> PathBuf {
        let slug = content_hash.replace(':', "_");
        self.root.join("packages").join(slug)
    }

    #[must_use]
    pub fn artifact_path(&self, content_hash: &str) -> PathBuf {
        let slug = content_hash.replace(':', "_");
        self.root.join("artifacts").join(format!("{slug}.tar.gz"))
    }

    /// Store and safely extract a verified artifact in the content-addressed store.
    ///
    /// Steps:
    /// 1. Validate artifact size bounds.
    /// 2. Atomically save artifact bytes.
    /// 3. Safely extract package to a temporary directory enforcing hostile archive bounds.
    /// 4. Freeze package directory permissions.
    /// 5. Atomically move/commit package directory.
    pub fn install_artifact(
        &self,
        content_hash: &str,
        artifact: &[u8],
    ) -> Result<PathBuf, MarketplaceError> {
        if artifact.len() > MAX_PACKAGE_ARCHIVE_BYTES {
            return Err(MarketplaceError::Package(PackageError::LimitExceeded(
                format!(
                    "artifact size {} exceeds maximum {}",
                    artifact.len(),
                    MAX_PACKAGE_ARCHIVE_BYTES
                ),
            )));
        }

        let target_dir = self.package_dir(content_hash);
        let artifact_dest = self.artifact_path(content_hash);

        // Save artifact binary
        atomic_write_once(&artifact_dest, artifact)?;

        if target_dir.exists() {
            // Already extracted — verify integrity
            verify_existing_package(&target_dir, artifact)?;
            return Ok(target_dir);
        }

        let temp_dir = self
            .root
            .join("tmp")
            .join(format!(".extract-{}", uuid::Uuid::now_v7()));

        create_private_dir(&temp_dir)?;

        // Extract safely enforcing all limits
        if let Err(e) = extract_package(artifact, &temp_dir) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(MarketplaceError::Package(e));
        }

        // Move to final destination
        if let Err(source) = std::fs::rename(&temp_dir, &target_dir) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(MarketplaceError::Io(source));
        }

        // Freeze permissions once in final destination
        if let Err(e) = freeze_package_tree(&target_dir) {
            return Err(MarketplaceError::Package(e));
        }

        Ok(target_dir)
    }

    /// Check if a package content hash is already stored and valid on disk.
    pub fn contains_package(&self, content_hash: &str) -> bool {
        self.package_dir(content_hash).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty allowlist is the closed one. Before this was enforced, the
    /// domain check was skipped whenever no domain had been opted in, so a
    /// default-constructed allowlist passed every HTTPS host on the internet.
    #[test]
    fn a_default_allowlist_admits_no_domain() {
        let allowlist = DownloadAllowlist::new();
        for url in [
            "https://evil.example/pkg.tar.gz",
            "https://marketplace.codypendent.io/v1/pkg.tar.gz",
            "https://cdn.trusted-plugins.com/plugin.tar.gz",
        ] {
            let err = allowlist
                .check_url(url)
                .expect_err("an empty allowlist must refuse every domain");
            assert!(
                matches!(err, MarketplaceError::DownloadDisallowed(_)),
                "{url}"
            );
        }
    }

    /// Opting a domain in is the only thing that opens it, and it opens that
    /// domain and its subdomains only.
    #[test]
    fn only_an_opted_in_domain_and_its_subdomains_pass() {
        let mut allowlist = DownloadAllowlist::new();
        allowlist.allow_domain("marketplace.codypendent.io");

        assert!(allowlist
            .check_url("https://marketplace.codypendent.io/v1/pkg.tar.gz")
            .is_ok());
        assert!(allowlist
            .check_url("https://sub.marketplace.codypendent.io/pkg.tar.gz")
            .is_ok());
        assert!(allowlist
            .check_url("https://evil.example/pkg.tar.gz")
            .is_err());
        // A suffix that is not a subdomain boundary must not slip through.
        assert!(allowlist
            .check_url("https://notmarketplace.codypendent.io/pkg.tar.gz")
            .is_err());
    }

    /// `set_allow_localhost` stays an explicit, separate opt-in and does not
    /// widen the domain allowlist for anything else.
    #[test]
    fn localhost_opt_in_does_not_open_any_other_host() {
        let mut allowlist = DownloadAllowlist::new();
        allowlist.set_allow_localhost(true);

        assert!(allowlist
            .check_url("http://localhost:8080/pkg.tar.gz")
            .is_ok());
        assert!(allowlist
            .check_url("http://127.0.0.1:8080/pkg.tar.gz")
            .is_ok());
        assert!(allowlist
            .check_url("https://evil.example/pkg.tar.gz")
            .is_err());
    }
}

//! Package catalog discovery, search, and inspection (Milestone 5).
//!
//! Enforces:
//! - Hidden-package non-disclosure: inspecting or searching for a hidden package
//!   answers identically to a package that does not exist.

use crate::error::MarketplaceError;
use crate::store::{MarketplacePackage, MarketplaceStore, MarketplaceVersion};

/// Catalog service for querying packages and versions.
#[derive(Debug, Clone)]
pub struct MarketplaceCatalog {
    store: MarketplaceStore,
}

impl MarketplaceCatalog {
    #[must_use]
    pub fn new(store: MarketplaceStore) -> Self {
        Self { store }
    }

    /// Discover packages matching an optional search query.
    pub async fn discover(
        &self,
        query: Option<&str>,
        include_hidden: bool,
    ) -> Result<Vec<MarketplacePackage>, MarketplaceError> {
        let packages = self.store.list_packages(include_hidden).await?;

        let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
            return Ok(packages);
        };

        let query_lower = query.to_lowercase();
        let filtered = packages
            .into_iter()
            .filter(|pkg| {
                pkg.id.to_lowercase().contains(&query_lower)
                    || pkg.display_name.to_lowercase().contains(&query_lower)
                    || pkg.summary.to_lowercase().contains(&query_lower)
            })
            .collect();

        Ok(filtered)
    }

    /// Inspect a specific package by ID.
    ///
    /// If `include_hidden` is false and the package is hidden,
    /// returns `MarketplaceError::PackageNotFound` (identical to non-existent package).
    pub async fn inspect(
        &self,
        package_id: &str,
        include_hidden: bool,
    ) -> Result<(MarketplacePackage, Vec<MarketplaceVersion>), MarketplaceError> {
        let package = self
            .store
            .get_package(package_id, include_hidden)
            .await?
            .ok_or_else(|| MarketplaceError::PackageNotFound(package_id.to_string()))?;

        let versions = self.store.list_versions(package_id).await?;

        Ok((package, versions))
    }
}

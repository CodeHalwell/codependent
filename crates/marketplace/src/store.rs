//! Durable SQLite persistence for marketplace distribution, trust, and lifecycle (Milestone 5).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::error::MarketplaceError;

/// Trust tier of a publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublisherTrustTier {
    Untrusted,
    Trusted,
    FirstParty,
}

impl PublisherTrustTier {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::Trusted => "trusted",
            Self::FirstParty => "first_party",
        }
    }

    #[must_use]
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted | Self::FirstParty)
    }
}

impl std::str::FromStr for PublisherTrustTier {
    type Err = MarketplaceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "untrusted" => Ok(Self::Untrusted),
            "trusted" => Ok(Self::Trusted),
            "first_party" => Ok(Self::FirstParty),
            other => Err(MarketplaceError::InvalidState(format!(
                "invalid trust tier: {other}"
            ))),
        }
    }
}

/// Lifecycle state of an installed package in the marketplace store.
///
/// This is a projection of the sandbox's `LifecycleState`, which is the
/// authority at execution. Only states the sandbox can actually reach are ever
/// written — see `MarketplaceLifecycleManager`'s `project_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallLifecycleState {
    InstalledDisabled,
    SmokeTested,
    Enabled,
    /// **Legacy only — never written.** The sandbox has no reversible disable;
    /// a disable is a revocation and is recorded as [`Self::Revoked`]. The
    /// variant is retained so rows written before that was true still parse.
    Disabled,
    Revoked,
}

impl InstallLifecycleState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InstalledDisabled => "installed_disabled",
            Self::SmokeTested => "smoke_tested",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Revoked => "revoked",
        }
    }
}

impl std::str::FromStr for InstallLifecycleState {
    type Err = MarketplaceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "installed_disabled" => Ok(Self::InstalledDisabled),
            "smoke_tested" => Ok(Self::SmokeTested),
            "enabled" => Ok(Self::Enabled),
            "disabled" => Ok(Self::Disabled),
            "revoked" => Ok(Self::Revoked),
            other => Err(MarketplaceError::InvalidState(format!(
                "invalid install lifecycle state: {other}"
            ))),
        }
    }
}

/// A registered publisher record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplacePublisher {
    pub id: String,
    pub display_name: String,
    pub public_key_hex: String,
    pub trust_tier: PublisherTrustTier,
    pub trusted_at: Option<String>,
    pub trusted_by: Option<String>,
    pub revoked_at: Option<String>,
    pub revoked_reason: Option<String>,
    pub created_at: String,
}

/// A marketplace package entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplacePackage {
    pub id: String,
    pub publisher_id: String,
    pub kind: String,
    pub display_name: String,
    pub summary: String,
    pub hidden: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// An immutable published package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceVersion {
    pub id: String,
    pub package_id: String,
    pub version: String,
    pub content_hash: String,
    pub manifest_toml: String,
    pub signature_b64: Option<String>,
    pub signed: bool,
    pub source_url: String,
    pub artifact_bytes: i64,
    pub min_daemon_version: Option<String>,
    pub max_daemon_version: Option<String>,
    pub published_at: String,
    pub yanked_at: Option<String>,
}

/// An install record for a local principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceInstall {
    pub id: String,
    pub package_id: String,
    pub version_id: String,
    pub owner_uid: i64,
    pub lifecycle: InstallLifecycleState,
    pub pinned: bool,
    pub pinned_version: Option<String>,
    pub enabled_scope: Option<String>,
    pub revoked_at: Option<String>,
    pub revoked_reason: Option<String>,
    pub installed_at: String,
    pub updated_at: String,
}

/// A human review record for a permission expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplacePermissionReceipt {
    pub id: String,
    pub install_id: String,
    pub from_version_id: Option<String>,
    pub to_version_id: String,
    pub diff_rendered: String,
    pub expands_permissions: bool,
    pub approved_manifest_hash: String,
    pub decision: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub invalidated_at: Option<String>,
    pub created_at: String,
}

/// An append-only revocation entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceRevocation {
    pub id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub reason: String,
    pub source: String,
    pub recorded_at: String,
}

/// The SQLite-backed store for all marketplace entities.
#[derive(Debug, Clone)]
pub struct MarketplaceStore {
    pool: SqlitePool,
}

impl MarketplaceStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // -------------------------------------------------------------------------
    // Publishers
    // -------------------------------------------------------------------------

    pub async fn upsert_publisher(
        &self,
        publisher: &MarketplacePublisher,
    ) -> Result<(), MarketplaceError> {
        let trust_str = publisher.trust_tier.as_str();
        sqlx::query(
            r#"
            INSERT INTO marketplace_publishers (
                id, display_name, public_key_hex, trust_tier,
                trusted_at, trusted_by, revoked_at, revoked_reason, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT (id) DO UPDATE SET
                display_name = excluded.display_name,
                public_key_hex = excluded.public_key_hex,
                trust_tier = excluded.trust_tier,
                trusted_at = excluded.trusted_at,
                trusted_by = excluded.trusted_by,
                revoked_at = excluded.revoked_at,
                revoked_reason = excluded.revoked_reason
            "#,
        )
        .bind(&publisher.id)
        .bind(&publisher.display_name)
        .bind(&publisher.public_key_hex)
        .bind(trust_str)
        .bind(&publisher.trusted_at)
        .bind(&publisher.trusted_by)
        .bind(&publisher.revoked_at)
        .bind(&publisher.revoked_reason)
        .bind(&publisher.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn get_publisher(
        &self,
        id: &str,
    ) -> Result<Option<MarketplacePublisher>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, display_name, public_key_hex, trust_tier,
                   trusted_at, trusted_by, revoked_at, revoked_reason, created_at
            FROM marketplace_publishers
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let trust_str: String = row.get("trust_tier");
        let trust_tier = trust_str.parse()?;

        Ok(Some(MarketplacePublisher {
            id: row.get("id"),
            display_name: row.get("display_name"),
            public_key_hex: row.get("public_key_hex"),
            trust_tier,
            trusted_at: row.get("trusted_at"),
            trusted_by: row.get("trusted_by"),
            revoked_at: row.get("revoked_at"),
            revoked_reason: row.get("revoked_reason"),
            created_at: row.get("created_at"),
        }))
    }

    pub async fn list_publishers(&self) -> Result<Vec<MarketplacePublisher>, MarketplaceError> {
        let rows = sqlx::query(
            r#"
            SELECT id, display_name, public_key_hex, trust_tier,
                   trusted_at, trusted_by, revoked_at, revoked_reason, created_at
            FROM marketplace_publishers
            ORDER BY id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let mut publishers = Vec::with_capacity(rows.len());
        for row in rows {
            let trust_str: String = row.get("trust_tier");
            let trust_tier = trust_str.parse()?;
            publishers.push(MarketplacePublisher {
                id: row.get("id"),
                display_name: row.get("display_name"),
                public_key_hex: row.get("public_key_hex"),
                trust_tier,
                trusted_at: row.get("trusted_at"),
                trusted_by: row.get("trusted_by"),
                revoked_at: row.get("revoked_at"),
                revoked_reason: row.get("revoked_reason"),
                created_at: row.get("created_at"),
            });
        }
        Ok(publishers)
    }

    pub async fn set_publisher_trust(
        &self,
        id: &str,
        tier: PublisherTrustTier,
        trusted_by: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        let now = Utc::now().to_rfc3339();
        let (trusted_at, trusted_by_val) = if tier.is_trusted() {
            (Some(now), trusted_by.map(String::from))
        } else {
            (None, None)
        };

        let result = sqlx::query(
            r#"
            UPDATE marketplace_publishers
            SET trust_tier = ?1, trusted_at = ?2, trusted_by = ?3
            WHERE id = ?4
            "#,
        )
        .bind(tier.as_str())
        .bind(&trusted_at)
        .bind(&trusted_by_val)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::PublisherNotFound(id.to_string()));
        }
        Ok(())
    }

    pub async fn revoke_publisher(
        &self,
        id: &str,
        reason: &str,
        source: &str,
    ) -> Result<(), MarketplaceError> {
        let now = Utc::now().to_rfc3339();

        // All four writes in one transaction. They were four autocommits, and
        // the partial states between them are the dangerous kind: a failure
        // after step 1 leaves the publisher marked revoked while every install
        // of their packages stays enabled — a revoked publisher's code still
        // running, with the record saying otherwise. Dropping `tx` without a
        // commit rolls the whole revocation back, so it either takes effect
        // everywhere or nowhere and can be retried honestly.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        // 1. Mark publisher revoked.
        let result = sqlx::query(
            r#"
            UPDATE marketplace_publishers
            SET revoked_at = ?1, revoked_reason = ?2
            WHERE id = ?3
            "#,
        )
        .bind(&now)
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::PublisherNotFound(id.to_string()));
        }

        // 2. Record revocation event.
        let rev_id = Uuid::now_v7().to_string();
        sqlx::query(
            r#"
            INSERT INTO marketplace_revocations (
                id, subject_kind, subject_id, reason, source, recorded_at
            ) VALUES (?1, 'publisher', ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(&rev_id)
        .bind(id)
        .bind(reason)
        .bind(source)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        // 3. Retroactively revoke all installs of packages from this publisher.
        sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET lifecycle = 'revoked',
                -- A revoked install is not enabled at any scope; clearing the
                -- scope keeps the `lifecycle = 'enabled' OR enabled_scope IS
                -- NULL` CHECK satisfied and leaves no stale grant behind.
                enabled_scope = NULL,
                revoked_at = ?1,
                revoked_reason = ?2,
                updated_at = ?1
            WHERE package_id IN (
                SELECT id FROM marketplace_packages WHERE publisher_id = ?3
            )
            "#,
        )
        .bind(&now)
        .bind(format!("publisher revoked: {reason}"))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        // 4. Invalidate all pending receipts for this publisher's packages.
        sqlx::query(
            r#"
            UPDATE marketplace_permission_receipts
            SET invalidated_at = ?1
            WHERE decision = 'pending'
              AND install_id IN (
                  SELECT mi.id FROM marketplace_installs mi
                  JOIN marketplace_packages mp ON mi.package_id = mp.id
                  WHERE mp.publisher_id = ?2
              )
            "#,
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Packages & Versions
    // -------------------------------------------------------------------------

    pub async fn create_package(&self, pkg: &MarketplacePackage) -> Result<(), MarketplaceError> {
        sqlx::query(
            r#"
            INSERT INTO marketplace_packages (
                id, publisher_id, kind, display_name, summary, hidden, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&pkg.id)
        .bind(&pkg.publisher_id)
        .bind(&pkg.kind)
        .bind(&pkg.display_name)
        .bind(&pkg.summary)
        .bind(if pkg.hidden { 1 } else { 0 })
        .bind(&pkg.created_at)
        .bind(&pkg.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn get_package(
        &self,
        id: &str,
        include_hidden: bool,
    ) -> Result<Option<MarketplacePackage>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, publisher_id, kind, display_name, summary, hidden, created_at, updated_at
            FROM marketplace_packages
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let hidden: i64 = row.get("hidden");
        if hidden != 0 && !include_hidden {
            // Hidden packages answer identically to non-existent ones (non-disclosure).
            return Ok(None);
        }

        Ok(Some(MarketplacePackage {
            id: row.get("id"),
            publisher_id: row.get("publisher_id"),
            kind: row.get("kind"),
            display_name: row.get("display_name"),
            summary: row.get("summary"),
            hidden: hidden != 0,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    pub async fn list_packages(
        &self,
        include_hidden: bool,
    ) -> Result<Vec<MarketplacePackage>, MarketplaceError> {
        let rows = if include_hidden {
            sqlx::query(
                r#"
                SELECT id, publisher_id, kind, display_name, summary, hidden, created_at, updated_at
                FROM marketplace_packages
                ORDER BY id ASC
                "#,
            )
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                r#"
                SELECT id, publisher_id, kind, display_name, summary, hidden, created_at, updated_at
                FROM marketplace_packages
                WHERE hidden = 0
                ORDER BY id ASC
                "#,
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let mut packages = Vec::with_capacity(rows.len());
        for row in rows {
            let hidden: i64 = row.get("hidden");
            packages.push(MarketplacePackage {
                id: row.get("id"),
                publisher_id: row.get("publisher_id"),
                kind: row.get("kind"),
                display_name: row.get("display_name"),
                summary: row.get("summary"),
                hidden: hidden != 0,
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            });
        }
        Ok(packages)
    }

    pub async fn create_version(&self, ver: &MarketplaceVersion) -> Result<(), MarketplaceError> {
        sqlx::query(
            r#"
            INSERT INTO marketplace_versions (
                id, package_id, version, content_hash, manifest_toml,
                signature_b64, signed, source_url, artifact_bytes,
                min_daemon_version, max_daemon_version, published_at, yanked_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
        )
        .bind(&ver.id)
        .bind(&ver.package_id)
        .bind(&ver.version)
        .bind(&ver.content_hash)
        .bind(&ver.manifest_toml)
        .bind(&ver.signature_b64)
        .bind(if ver.signed { 1 } else { 0 })
        .bind(&ver.source_url)
        .bind(ver.artifact_bytes)
        .bind(&ver.min_daemon_version)
        .bind(&ver.max_daemon_version)
        .bind(&ver.published_at)
        .bind(&ver.yanked_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn get_version(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<Option<MarketplaceVersion>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, package_id, version, content_hash, manifest_toml,
                   signature_b64, signed, source_url, artifact_bytes,
                   min_daemon_version, max_daemon_version, published_at, yanked_at
            FROM marketplace_versions
            WHERE package_id = ?1 AND version = ?2
            "#,
        )
        .bind(package_id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let signed: i64 = row.get("signed");
        Ok(Some(MarketplaceVersion {
            id: row.get("id"),
            package_id: row.get("package_id"),
            version: row.get("version"),
            content_hash: row.get("content_hash"),
            manifest_toml: row.get("manifest_toml"),
            signature_b64: row.get("signature_b64"),
            signed: signed != 0,
            source_url: row.get("source_url"),
            artifact_bytes: row.get("artifact_bytes"),
            min_daemon_version: row.get("min_daemon_version"),
            max_daemon_version: row.get("max_daemon_version"),
            published_at: row.get("published_at"),
            yanked_at: row.get("yanked_at"),
        }))
    }

    pub async fn get_version_by_id(
        &self,
        version_id: &str,
    ) -> Result<Option<MarketplaceVersion>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, package_id, version, content_hash, manifest_toml,
                   signature_b64, signed, source_url, artifact_bytes,
                   min_daemon_version, max_daemon_version, published_at, yanked_at
            FROM marketplace_versions
            WHERE id = ?1
            "#,
        )
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let signed: i64 = row.get("signed");
        Ok(Some(MarketplaceVersion {
            id: row.get("id"),
            package_id: row.get("package_id"),
            version: row.get("version"),
            content_hash: row.get("content_hash"),
            manifest_toml: row.get("manifest_toml"),
            signature_b64: row.get("signature_b64"),
            signed: signed != 0,
            source_url: row.get("source_url"),
            artifact_bytes: row.get("artifact_bytes"),
            min_daemon_version: row.get("min_daemon_version"),
            max_daemon_version: row.get("max_daemon_version"),
            published_at: row.get("published_at"),
            yanked_at: row.get("yanked_at"),
        }))
    }

    pub async fn list_versions(
        &self,
        package_id: &str,
    ) -> Result<Vec<MarketplaceVersion>, MarketplaceError> {
        let rows = sqlx::query(
            r#"
            SELECT id, package_id, version, content_hash, manifest_toml,
                   signature_b64, signed, source_url, artifact_bytes,
                   min_daemon_version, max_daemon_version, published_at, yanked_at
            FROM marketplace_versions
            WHERE package_id = ?1
            ORDER BY published_at DESC
            "#,
        )
        .bind(package_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let mut versions = Vec::with_capacity(rows.len());
        for row in rows {
            let signed: i64 = row.get("signed");
            versions.push(MarketplaceVersion {
                id: row.get("id"),
                package_id: row.get("package_id"),
                version: row.get("version"),
                content_hash: row.get("content_hash"),
                manifest_toml: row.get("manifest_toml"),
                signature_b64: row.get("signature_b64"),
                signed: signed != 0,
                source_url: row.get("source_url"),
                artifact_bytes: row.get("artifact_bytes"),
                min_daemon_version: row.get("min_daemon_version"),
                max_daemon_version: row.get("max_daemon_version"),
                published_at: row.get("published_at"),
                yanked_at: row.get("yanked_at"),
            });
        }
        Ok(versions)
    }

    // -------------------------------------------------------------------------
    // Installs
    // -------------------------------------------------------------------------

    pub async fn record_install(
        &self,
        install: &MarketplaceInstall,
    ) -> Result<(), MarketplaceError> {
        // Same pairing rule as `update_install_lifecycle`: a scope may only be
        // recorded alongside `enabled`, and a pinned version only alongside a
        // pin (both are schema CHECKs in migrations/0046_marketplace.sql).
        if install.lifecycle != InstallLifecycleState::Enabled && install.enabled_scope.is_some() {
            return Err(MarketplaceError::InvalidState(format!(
                "an install in state `{}` cannot carry an enabled scope",
                install.lifecycle.as_str()
            )));
        }
        if !install.pinned && install.pinned_version.is_some() {
            return Err(MarketplaceError::InvalidState(
                "an unpinned install cannot carry a pinned version".into(),
            ));
        }
        sqlx::query(
            r#"
            INSERT INTO marketplace_installs (
                id, package_id, version_id, owner_uid, lifecycle,
                pinned, pinned_version, enabled_scope, revoked_at, revoked_reason,
                installed_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT (package_id, owner_uid) DO UPDATE SET
                version_id = excluded.version_id,
                lifecycle = excluded.lifecycle,
                pinned = excluded.pinned,
                pinned_version = excluded.pinned_version,
                enabled_scope = excluded.enabled_scope,
                revoked_at = excluded.revoked_at,
                revoked_reason = excluded.revoked_reason,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&install.id)
        .bind(&install.package_id)
        .bind(&install.version_id)
        .bind(install.owner_uid)
        .bind(install.lifecycle.as_str())
        .bind(if install.pinned { 1 } else { 0 })
        .bind(&install.pinned_version)
        .bind(&install.enabled_scope)
        .bind(&install.revoked_at)
        .bind(&install.revoked_reason)
        .bind(&install.installed_at)
        .bind(&install.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn get_install(
        &self,
        package_id: &str,
        owner_uid: i64,
    ) -> Result<Option<MarketplaceInstall>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, package_id, version_id, owner_uid, lifecycle,
                   pinned, pinned_version, enabled_scope, revoked_at, revoked_reason,
                   installed_at, updated_at
            FROM marketplace_installs
            WHERE package_id = ?1 AND owner_uid = ?2
            "#,
        )
        .bind(package_id)
        .bind(owner_uid)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let lifecycle_str: String = row.get("lifecycle");
        let lifecycle = lifecycle_str.parse()?;
        let pinned: i64 = row.get("pinned");

        Ok(Some(MarketplaceInstall {
            id: row.get("id"),
            package_id: row.get("package_id"),
            version_id: row.get("version_id"),
            owner_uid: row.get("owner_uid"),
            lifecycle,
            pinned: pinned != 0,
            pinned_version: row.get("pinned_version"),
            enabled_scope: row.get("enabled_scope"),
            revoked_at: row.get("revoked_at"),
            revoked_reason: row.get("revoked_reason"),
            installed_at: row.get("installed_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    pub async fn get_install_by_id(
        &self,
        id: &str,
    ) -> Result<Option<MarketplaceInstall>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, package_id, version_id, owner_uid, lifecycle,
                   pinned, pinned_version, enabled_scope, revoked_at, revoked_reason,
                   installed_at, updated_at
            FROM marketplace_installs
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let lifecycle_str: String = row.get("lifecycle");
        let lifecycle = lifecycle_str.parse()?;
        let pinned: i64 = row.get("pinned");

        Ok(Some(MarketplaceInstall {
            id: row.get("id"),
            package_id: row.get("package_id"),
            version_id: row.get("version_id"),
            owner_uid: row.get("owner_uid"),
            lifecycle,
            pinned: pinned != 0,
            pinned_version: row.get("pinned_version"),
            enabled_scope: row.get("enabled_scope"),
            revoked_at: row.get("revoked_at"),
            revoked_reason: row.get("revoked_reason"),
            installed_at: row.get("installed_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    pub async fn list_installs(
        &self,
        owner_uid: i64,
    ) -> Result<Vec<MarketplaceInstall>, MarketplaceError> {
        let rows = sqlx::query(
            r#"
            SELECT id, package_id, version_id, owner_uid, lifecycle,
                   pinned, pinned_version, enabled_scope, revoked_at, revoked_reason,
                   installed_at, updated_at
            FROM marketplace_installs
            WHERE owner_uid = ?1
            ORDER BY updated_at DESC
            "#,
        )
        .bind(owner_uid)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let mut installs = Vec::with_capacity(rows.len());
        for row in rows {
            let lifecycle_str: String = row.get("lifecycle");
            let lifecycle = lifecycle_str.parse()?;
            let pinned: i64 = row.get("pinned");
            installs.push(MarketplaceInstall {
                id: row.get("id"),
                package_id: row.get("package_id"),
                version_id: row.get("version_id"),
                owner_uid: row.get("owner_uid"),
                lifecycle,
                pinned: pinned != 0,
                pinned_version: row.get("pinned_version"),
                enabled_scope: row.get("enabled_scope"),
                revoked_at: row.get("revoked_at"),
                revoked_reason: row.get("revoked_reason"),
                installed_at: row.get("installed_at"),
                updated_at: row.get("updated_at"),
            });
        }
        Ok(installs)
    }

    /// Write an install's lifecycle and its enabled scope **as a pair**.
    ///
    /// A scope is only meaningful while the install is enabled, and the schema
    /// enforces it (`CHECK (lifecycle = 'enabled' OR enabled_scope IS NULL)`).
    /// Rejecting the impossible pair here turns a would-be constraint failure —
    /// or, worse, a row claiming a scope it is not enabled at — into a named
    /// error at the call site.
    pub async fn update_install_lifecycle(
        &self,
        install_id: &str,
        lifecycle: InstallLifecycleState,
        enabled_scope: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        if lifecycle != InstallLifecycleState::Enabled && enabled_scope.is_some() {
            return Err(MarketplaceError::InvalidState(format!(
                "an install in state `{}` cannot carry an enabled scope",
                lifecycle.as_str()
            )));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET lifecycle = ?1, enabled_scope = ?2, updated_at = ?3
            WHERE id = ?4
            "#,
        )
        .bind(lifecycle.as_str())
        .bind(enabled_scope)
        .bind(&now)
        .bind(install_id)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }
        Ok(())
    }

    /// Pin or unpin an install.
    ///
    /// Unpinning with a version is a contradiction the schema refuses
    /// (`CHECK (pinned = 1 OR pinned_version IS NULL)`), so it is refused here
    /// with a named error rather than surfacing as an opaque constraint failure.
    pub async fn set_install_pin(
        &self,
        install_id: &str,
        pinned: bool,
        version: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        if !pinned && version.is_some() {
            return Err(MarketplaceError::InvalidState(
                "cannot record a pinned version while unpinning an install".into(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE marketplace_installs
            SET pinned = ?1, pinned_version = ?2, updated_at = ?3
            WHERE id = ?4
            "#,
        )
        .bind(if pinned { 1 } else { 0 })
        .bind(version)
        .bind(&now)
        .bind(install_id)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }
        Ok(())
    }

    pub async fn remove_install(&self, install_id: &str) -> Result<(), MarketplaceError> {
        let result = sqlx::query("DELETE FROM marketplace_installs WHERE id = ?1")
            .bind(install_id)
            .execute(&self.pool)
            .await
            .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::InstallNotFound(install_id.to_string()));
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Permission Receipts
    // -------------------------------------------------------------------------

    pub async fn create_permission_receipt(
        &self,
        receipt: &MarketplacePermissionReceipt,
    ) -> Result<(), MarketplaceError> {
        sqlx::query(
            r#"
            INSERT INTO marketplace_permission_receipts (
                id, install_id, from_version_id, to_version_id, diff_rendered,
                expands_permissions, approved_manifest_hash, decision,
                decided_by, decided_at, invalidated_at, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
        )
        .bind(&receipt.id)
        .bind(&receipt.install_id)
        .bind(&receipt.from_version_id)
        .bind(&receipt.to_version_id)
        .bind(&receipt.diff_rendered)
        .bind(if receipt.expands_permissions { 1 } else { 0 })
        .bind(&receipt.approved_manifest_hash)
        .bind(&receipt.decision)
        .bind(&receipt.decided_by)
        .bind(&receipt.decided_at)
        .bind(&receipt.invalidated_at)
        .bind(&receipt.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn get_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<MarketplacePermissionReceipt>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, install_id, from_version_id, to_version_id, diff_rendered,
                   expands_permissions, approved_manifest_hash, decision,
                   decided_by, decided_at, invalidated_at, created_at
            FROM marketplace_permission_receipts
            WHERE id = ?1
            "#,
        )
        .bind(receipt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let expands: i64 = row.get("expands_permissions");
        Ok(Some(MarketplacePermissionReceipt {
            id: row.get("id"),
            install_id: row.get("install_id"),
            from_version_id: row.get("from_version_id"),
            to_version_id: row.get("to_version_id"),
            diff_rendered: row.get("diff_rendered"),
            expands_permissions: expands != 0,
            approved_manifest_hash: row.get("approved_manifest_hash"),
            decision: row.get("decision"),
            decided_by: row.get("decided_by"),
            decided_at: row.get("decided_at"),
            invalidated_at: row.get("invalidated_at"),
            created_at: row.get("created_at"),
        }))
    }

    pub async fn get_pending_receipt(
        &self,
        install_id: &str,
    ) -> Result<Option<MarketplacePermissionReceipt>, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT id, install_id, from_version_id, to_version_id, diff_rendered,
                   expands_permissions, approved_manifest_hash, decision,
                   decided_by, decided_at, invalidated_at, created_at
            FROM marketplace_permission_receipts
            WHERE install_id = ?1 AND decision = 'pending' AND invalidated_at IS NULL
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(install_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let expands: i64 = row.get("expands_permissions");
        Ok(Some(MarketplacePermissionReceipt {
            id: row.get("id"),
            install_id: row.get("install_id"),
            from_version_id: row.get("from_version_id"),
            to_version_id: row.get("to_version_id"),
            diff_rendered: row.get("diff_rendered"),
            expands_permissions: expands != 0,
            approved_manifest_hash: row.get("approved_manifest_hash"),
            decision: row.get("decision"),
            decided_by: row.get("decided_by"),
            decided_at: row.get("decided_at"),
            invalidated_at: row.get("invalidated_at"),
            created_at: row.get("created_at"),
        }))
    }

    pub async fn decide_receipt(
        &self,
        receipt_id: &str,
        decision: &str,
        decided_by: Option<&str>,
    ) -> Result<(), MarketplaceError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE marketplace_permission_receipts
            SET decision = ?1, decided_by = ?2, decided_at = ?3
            WHERE id = ?4 AND decision = 'pending' AND invalidated_at IS NULL
            "#,
        )
        .bind(decision)
        .bind(decided_by)
        .bind(&now)
        .bind(receipt_id)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(MarketplaceError::ReceiptNotFoundOrInvalid(
                receipt_id.to_string(),
            ));
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Revocations
    // -------------------------------------------------------------------------

    pub async fn record_revocation(
        &self,
        revocation: &MarketplaceRevocation,
    ) -> Result<(), MarketplaceError> {
        sqlx::query(
            r#"
            INSERT INTO marketplace_revocations (
                id, subject_kind, subject_id, reason, source, recorded_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT (subject_kind, subject_id, recorded_at) DO NOTHING
            "#,
        )
        .bind(&revocation.id)
        .bind(&revocation.subject_kind)
        .bind(&revocation.subject_id)
        .bind(&revocation.reason)
        .bind(&revocation.source)
        .bind(&revocation.recorded_at)
        .execute(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(())
    }

    pub async fn is_revoked(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> Result<bool, MarketplaceError> {
        let row = sqlx::query(
            r#"
            SELECT 1 FROM marketplace_revocations
            WHERE subject_kind = ?1 AND subject_id = ?2
            LIMIT 1
            "#,
        )
        .bind(subject_kind)
        .bind(subject_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| MarketplaceError::Store(e.to_string()))?;

        Ok(row.is_some())
    }
}

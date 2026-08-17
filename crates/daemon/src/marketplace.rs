//! Marketplace catalog and local installation store.
//!
//! Registration is the security boundary of this module. A package reaches a
//! client through [`MarketplaceStore::install`] / [`MarketplaceStore::update`],
//! so every manifest that arrives with one is verified before a single row is
//! written: the manifest is parsed by the sandbox's own parser, the publisher
//! must already be registered and trusted locally (there is **no**
//! trust-on-first-use — an unknown or revoked publisher is a refusal), and the
//! artifact's SHA-256 and Ed25519 signature are checked by
//! `codypendent_marketplace::PackageVerifier` over
//! `crates/sandbox/src/verify.rs`. Rows are then written from the *verified*
//! facts, never from what the manifest asserted about itself.

use chrono::Utc;
use codypendent_marketplace::store::{
    MarketplacePackage, MarketplaceStore as VerifiedStore, MarketplaceVersion,
};
use codypendent_marketplace::{MarketplaceError, PackageVerifier, TrustManager};
use codypendent_protocol::marketplace::MarketplacePackageView;
use codypendent_protocol::CodypendentError;
use codypendent_sandbox::{UnsignedPolicy, MAX_PACKAGE_ARCHIVE_BYTES};
use sqlx::{Row, SqlitePool};

/// Translate a marketplace verification/store failure into a wire error.
///
/// Verification failures are terminal for the command (`retryable = false`):
/// re-sending the same unverifiable bytes cannot succeed.
fn verification_error(error: &MarketplaceError) -> CodypendentError {
    let code = match error {
        MarketplaceError::Verify(_) => "marketplace.verification-failed",
        MarketplaceError::Manifest(_) | MarketplaceError::Toml(_) => "marketplace.invalid-manifest",
        MarketplaceError::RevokedPublisher { .. } => "marketplace.publisher-revoked",
        MarketplaceError::UntrustedPublisher(_) => "marketplace.untrusted-publisher",
        MarketplaceError::PublisherNotFound(_) => "marketplace.unknown-publisher",
        MarketplaceError::Store(_) => "marketplace.database-error",
        _ => "marketplace.invalid-manifest",
    };
    let retryable = matches!(error, MarketplaceError::Store(_));
    CodypendentError::new(code, error.to_string(), retryable)
}

/// Central manager for marketplace packages, publishers, versions, and local installations.
#[derive(Clone)]
pub struct MarketplaceStore {
    pool: SqlitePool,
}

impl MarketplaceStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The verified-store view of the same pool: the marketplace crate's typed
    /// accessors, which enforce the schema's lifecycle/scope pairing CHECKs.
    fn verified_store(&self) -> VerifiedStore {
        VerifiedStore::new(self.pool.clone())
    }

    /// Search visible marketplace packages by query.
    pub async fn search(
        &self,
        owner_uid: u32,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<MarketplacePackageView>, CodypendentError> {
        let max = limit.unwrap_or(50).clamp(1, 100);
        let q_pattern = if query.is_empty() || query == "*" {
            "%".to_string()
        } else {
            format!("%{query}%")
        };

        let rows = sqlx::query(
            r#"
            SELECT 
                p.id,
                p.publisher_id,
                p.kind,
                p.display_name,
                p.summary,
                COALESCE(
                    (SELECT v.version FROM marketplace_versions v WHERE v.package_id = p.id AND v.yanked_at IS NULL ORDER BY v.published_at DESC LIMIT 1),
                    '0.0.0'
                ) as latest_version,
                i.lifecycle,
                COALESCE(i.pinned, 0) as pinned,
                i.pinned_version,
                i.enabled_scope
            FROM marketplace_packages p
            LEFT JOIN marketplace_installs i ON i.package_id = p.id AND i.owner_uid = ?
            WHERE p.hidden = 0 AND (p.id LIKE ? OR p.display_name LIKE ? OR p.summary LIKE ?)
            ORDER BY p.updated_at DESC
            LIMIT ?
            "#,
        )
        .bind(i64::from(owner_uid))
        .bind(&q_pattern)
        .bind(&q_pattern)
        .bind(&q_pattern)
        .bind(i64::from(max))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let mut results = Vec::with_capacity(rows.len());
        for row in rows {
            let pinned_int: i64 = row.get("pinned");
            results.push(MarketplacePackageView {
                id: row.get("id"),
                publisher_id: row.get("publisher_id"),
                kind: row.get("kind"),
                display_name: row.get("display_name"),
                summary: row.get("summary"),
                latest_version: row.get("latest_version"),
                lifecycle: row.get("lifecycle"),
                pinned: pinned_int != 0,
                pinned_version: row.get("pinned_version"),
                enabled_scope: row.get("enabled_scope"),
            });
        }
        Ok(results)
    }

    /// Retrieve view for a single package.
    pub async fn get_package_view(
        &self,
        owner_uid: u32,
        package_id: &str,
    ) -> Result<Option<MarketplacePackageView>, CodypendentError> {
        let row = sqlx::query(
            r#"
            SELECT 
                p.id,
                p.publisher_id,
                p.kind,
                p.display_name,
                p.summary,
                COALESCE(
                    (SELECT v.version FROM marketplace_versions v WHERE v.package_id = p.id AND v.yanked_at IS NULL ORDER BY v.published_at DESC LIMIT 1),
                    '0.0.0'
                ) as latest_version,
                i.lifecycle,
                COALESCE(i.pinned, 0) as pinned,
                i.pinned_version,
                i.enabled_scope
            FROM marketplace_packages p
            LEFT JOIN marketplace_installs i ON i.package_id = p.id AND i.owner_uid = ?
            WHERE p.id = ? AND p.hidden = 0
            "#,
        )
        .bind(i64::from(owner_uid))
        .bind(package_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        Ok(row.map(|row| {
            let pinned_int: i64 = row.get("pinned");
            MarketplacePackageView {
                id: row.get("id"),
                publisher_id: row.get("publisher_id"),
                kind: row.get("kind"),
                display_name: row.get("display_name"),
                summary: row.get("summary"),
                latest_version: row.get("latest_version"),
                lifecycle: row.get("lifecycle"),
                pinned: pinned_int != 0,
                pinned_version: row.get("pinned_version"),
                enabled_scope: row.get("enabled_scope"),
            }
        }))
    }

    /// Install a package. If manifest provided, registers it first.
    pub async fn install(
        &self,
        owner_uid: u32,
        package_id: &str,
        manifest_toml: Option<&str>,
        artifact_base64: Option<&str>,
        allow_unsigned: bool,
    ) -> Result<MarketplacePackageView, CodypendentError> {
        let now = Utc::now().to_rfc3339();

        if let Some(toml_content) = manifest_toml {
            self.register_from_manifest(toml_content, artifact_base64, allow_unsigned)
                .await?;
        }

        let version_row = sqlx::query(
            "SELECT id, signed FROM marketplace_versions WHERE package_id = ? AND yanked_at IS NULL ORDER BY published_at DESC LIMIT 1",
        )
        .bind(package_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let Some(v_row) = version_row else {
            return Err(CodypendentError::new(
                "marketplace.package-not-found",
                format!("package {package_id} not found in marketplace"),
                false,
            ));
        };

        // A version row records whether a signature verified when it was
        // registered. Installing without a manifest resolves whatever row is
        // already there, so the same unsigned policy applies here — otherwise a
        // row written before this path verified anything (or under an explicit
        // unsigned opt-in) could be installed by a later caller that never asked
        // for unsigned packages.
        let signed: i64 = v_row.get("signed");
        if signed == 0 && !allow_unsigned {
            return Err(CodypendentError::new(
                "marketplace.verification-failed",
                format!("package {package_id} has no verified publisher signature"),
                false,
            ));
        }

        let version_id: String = v_row.get("id");
        let install_id = uuid::Uuid::now_v7().to_string();

        sqlx::query(
            r#"
            INSERT INTO marketplace_installs (id, package_id, version_id, owner_uid, lifecycle, pinned, installed_at, updated_at)
            VALUES (?, ?, ?, ?, 'installed_disabled', 0, ?, ?)
            ON CONFLICT (package_id, owner_uid) DO UPDATE SET
                version_id = excluded.version_id,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&install_id)
        .bind(package_id)
        .bind(&version_id)
        .bind(i64::from(owner_uid))
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        self.get_package_view(owner_uid, package_id)
            .await?
            .ok_or_else(|| {
                CodypendentError::new("marketplace.package-not-found", "package not found", false)
            })
    }

    /// Update an installed package.
    pub async fn update(
        &self,
        owner_uid: u32,
        package_id: &str,
        manifest_toml: Option<&str>,
        artifact_base64: Option<&str>,
        allow_unsigned: bool,
    ) -> Result<MarketplacePackageView, CodypendentError> {
        let now = Utc::now().to_rfc3339();

        let install = sqlx::query(
            "SELECT id, pinned, pinned_version FROM marketplace_installs WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let Some(inst) = install else {
            return Err(CodypendentError::new(
                "marketplace.not-installed",
                format!("package {package_id} is not installed"),
                false,
            ));
        };

        let pinned: i64 = inst.get("pinned");
        if pinned != 0 {
            let pinned_v: Option<String> = inst.get("pinned_version");
            return Err(CodypendentError::new(
                "marketplace.install-pinned",
                format!("package {package_id} is pinned to version {pinned_v:?}"),
                false,
            ));
        }

        if let Some(toml_content) = manifest_toml {
            self.register_from_manifest(toml_content, artifact_base64, allow_unsigned)
                .await?;
        }

        let version_row = sqlx::query(
            "SELECT id, signed FROM marketplace_versions WHERE package_id = ? AND yanked_at IS NULL ORDER BY published_at DESC LIMIT 1",
        )
        .bind(package_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let Some(v_row) = version_row else {
            return Err(CodypendentError::new(
                "marketplace.no-compatible-version",
                format!("no compatible version found for {package_id}"),
                false,
            ));
        };

        // Same unsigned policy as install: moving an install onto an unverified
        // version is an install of unverified code.
        let signed: i64 = v_row.get("signed");
        if signed == 0 && !allow_unsigned {
            return Err(CodypendentError::new(
                "marketplace.verification-failed",
                format!("package {package_id} has no verified publisher signature"),
                false,
            ));
        }

        let version_id: String = v_row.get("id");
        sqlx::query(
            "UPDATE marketplace_installs SET version_id = ?, updated_at = ? WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(&version_id)
        .bind(&now)
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        self.get_package_view(owner_uid, package_id)
            .await?
            .ok_or_else(|| {
                CodypendentError::new("marketplace.package-not-found", "package not found", false)
            })
    }

    /// Enable an installed package.
    pub async fn enable(
        &self,
        owner_uid: u32,
        package_id: &str,
        scope: Option<&str>,
        session_id: Option<codypendent_protocol::SessionId>,
    ) -> Result<MarketplacePackageView, CodypendentError> {
        let now = Utc::now().to_rfc3339();

        let install = sqlx::query(
            "SELECT id, lifecycle FROM marketplace_installs WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let Some(inst) = install else {
            return Err(CodypendentError::new(
                "marketplace.not-installed",
                format!("package {package_id} is not installed"),
                false,
            ));
        };

        let lifecycle: String = inst.get("lifecycle");
        if lifecycle == "revoked" {
            return Err(CodypendentError::new(
                "marketplace.package-revoked",
                format!("package {package_id} has been revoked and cannot be enabled"),
                false,
            ));
        }

        let effective_scope = scope
            .map(ToString::to_string)
            .or_else(|| session_id.map(|s| s.to_string()));

        sqlx::query(
            "UPDATE marketplace_installs SET lifecycle = 'enabled', enabled_scope = ?, updated_at = ? WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(effective_scope)
        .bind(&now)
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        self.get_package_view(owner_uid, package_id)
            .await?
            .ok_or_else(|| {
                CodypendentError::new("marketplace.package-not-found", "package not found", false)
            })
    }

    /// Disable an installed package.
    pub async fn disable(
        &self,
        owner_uid: u32,
        package_id: &str,
    ) -> Result<MarketplacePackageView, CodypendentError> {
        let now = Utc::now().to_rfc3339();

        let install = sqlx::query(
            "SELECT id, lifecycle FROM marketplace_installs WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        let Some(_) = install else {
            return Err(CodypendentError::new(
                "marketplace.not-installed",
                format!("package {package_id} is not installed"),
                false,
            ));
        };

        sqlx::query(
            "UPDATE marketplace_installs SET lifecycle = 'disabled', enabled_scope = NULL, updated_at = ? WHERE package_id = ? AND owner_uid = ?",
        )
        .bind(&now)
        .bind(package_id)
        .bind(i64::from(owner_uid))
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        self.get_package_view(owner_uid, package_id)
            .await?
            .ok_or_else(|| {
                CodypendentError::new("marketplace.package-not-found", "package not found", false)
            })
    }

    /// Revoke a package.
    pub async fn revoke(
        &self,
        owner_uid: u32,
        package_id: &str,
        reason: &str,
    ) -> Result<MarketplacePackageView, CodypendentError> {
        let now = Utc::now().to_rfc3339();
        let rev_id = uuid::Uuid::now_v7().to_string();

        sqlx::query(
            "INSERT OR IGNORE INTO marketplace_revocations (id, subject_kind, subject_id, reason, source, recorded_at) VALUES (?, 'package', ?, ?, 'operator', ?)",
        )
        .bind(&rev_id)
        .bind(package_id)
        .bind(reason)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        sqlx::query(
            "UPDATE marketplace_installs SET lifecycle = 'revoked', revoked_at = ?, revoked_reason = ?, enabled_scope = NULL, updated_at = ? WHERE package_id = ?",
        )
        .bind(&now)
        .bind(reason)
        .bind(&now)
        .bind(package_id)
        .execute(&self.pool)
        .await
        .map_err(|e| CodypendentError::new("marketplace.database-error", e.to_string(), true))?;

        self.get_package_view(owner_uid, package_id)
            .await?
            .ok_or_else(|| {
                CodypendentError::new("marketplace.package-not-found", "package not found", false)
            })
    }

    /// Verify a submitted package and register its package/version rows.
    ///
    /// **Nothing is written until verification passes.** In order:
    ///
    /// 1. The manifest is parsed by `codypendent_sandbox::parse_manifest` (via
    ///    [`PackageVerifier`]) — the real manifest shape, not a hand-rolled
    ///    subset that silently defaults every missing field.
    /// 2. The artifact bytes are **required**. Without them the manifest's
    ///    checksum binds nothing, so a manifest submitted alone is refused
    ///    rather than registered against a hash of its own text.
    /// 3. The publisher must already exist locally, be trusted, and not be
    ///    revoked. There is no trust-on-first-use: this path never creates a
    ///    publisher row, so an unknown publisher can never bootstrap itself into
    ///    the trust table (the previous code inserted one with an all-zero key).
    /// 4. [`PackageVerifier`] checks the SHA-256 checksum and, for a signed
    ///    manifest, the Ed25519 signature over the whole-manifest signing digest
    ///    against the publisher's registered key. `allow_unsigned` selects the
    ///    [`UnsignedPolicy`] and nothing else: it can waive a *missing*
    ///    signature, never a wrong one, a bad checksum, or an untrusted
    ///    publisher. The default (`false`) denies unsigned packages.
    /// 5. Rows are written from the verified manifest — its declared kind, its
    ///    verified checksum, and the signature status verification actually
    ///    reached — so no row can assert more than was proved.
    async fn register_from_manifest(
        &self,
        toml_content: &str,
        artifact_base64: Option<&str>,
        allow_unsigned: bool,
    ) -> Result<(), CodypendentError> {
        // 2. Artifact bytes are mandatory: a checksum with nothing to check is
        //    not verification.
        let Some(encoded) = artifact_base64 else {
            return Err(CodypendentError::new(
                "marketplace.artifact-required",
                "registering a marketplace package requires its artifact bytes so the manifest checksum and signature can be verified",
                false,
            ));
        };
        let artifact = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .map_err(|e| {
                CodypendentError::new(
                    "marketplace.invalid-artifact",
                    format!("artifact_base64 is not valid base64: {e}"),
                    false,
                )
            })?;
        if artifact.is_empty() {
            return Err(CodypendentError::new(
                "marketplace.invalid-artifact",
                "artifact is empty",
                false,
            ));
        }
        if artifact.len() > MAX_PACKAGE_ARCHIVE_BYTES {
            return Err(CodypendentError::new(
                "marketplace.invalid-artifact",
                format!("artifact exceeds {MAX_PACKAGE_ARCHIVE_BYTES} bytes"),
                false,
            ));
        }

        let store = self.verified_store();
        let trust = TrustManager::new(store.clone());

        // 1./4. Parse + verify. The unsigned policy is the ONLY thing
        // `allow_unsigned` controls.
        let policy = if allow_unsigned {
            UnsignedPolicy::Allow
        } else {
            UnsignedPolicy::Deny
        };
        let verifier = PackageVerifier::with_unsigned_policy(policy);

        // The manifest must parse before its publisher can be looked up, and the
        // publisher must be trusted before the signature is worth checking — so
        // parse first, then gate on trust, then verify.
        let manifest = codypendent_sandbox::parse_manifest(toml_content).map_err(|e| {
            CodypendentError::new("marketplace.invalid-manifest", e.to_string(), false)
        })?;

        // 3. Publisher trust, resolved locally. No trust-on-first-use.
        let publisher = store
            .get_publisher(&manifest.publisher)
            .await
            .map_err(|e| verification_error(&e))?
            .ok_or_else(|| {
                CodypendentError::new(
                    "marketplace.unknown-publisher",
                    format!(
                        "publisher `{}` is not registered locally; register and trust the publisher key before installing its packages",
                        manifest.publisher
                    ),
                    false,
                )
            })?;
        if let Some(revoked_at) = publisher.revoked_at.as_deref() {
            return Err(CodypendentError::new(
                "marketplace.publisher-revoked",
                format!(
                    "publisher `{}` was revoked at {revoked_at}: {}",
                    manifest.publisher,
                    publisher
                        .revoked_reason
                        .as_deref()
                        .unwrap_or("no reason recorded")
                ),
                false,
            ));
        }
        if !publisher.trust_tier.is_trusted() {
            return Err(CodypendentError::new(
                "marketplace.untrusted-publisher",
                format!(
                    "publisher `{}` is not trusted; trust the publisher before installing its packages",
                    manifest.publisher
                ),
                false,
            ));
        }

        // Only trusted, non-revoked publisher keys enter the trust store, so an
        // absent key is a verification failure rather than an implicit pass.
        let trust_store = trust
            .load_trusted_publishers()
            .await
            .map_err(|e| verification_error(&e))?;
        let (manifest, verified) = verifier
            .verify(toml_content, &artifact, &trust_store)
            .map_err(|e| verification_error(&e))?;

        // ---- verified from here on; only now is anything written ----

        let now = Utc::now().to_rfc3339();
        let content_hash = manifest.security.checksum.trim().to_string();

        // 5a. Package row. An existing package keeps its publisher: a package id
        // may not change hands on a later submission.
        match store
            .get_package(&manifest.id, true)
            .await
            .map_err(|e| verification_error(&e))?
        {
            Some(existing) if existing.publisher_id != manifest.publisher => {
                return Err(CodypendentError::new(
                    "marketplace.publisher-mismatch",
                    format!(
                        "package `{}` is published by `{}`, not `{}`",
                        manifest.id, existing.publisher_id, manifest.publisher
                    ),
                    false,
                ));
            }
            Some(_) => {}
            None => {
                store
                    .create_package(&MarketplacePackage {
                        id: manifest.id.clone(),
                        publisher_id: manifest.publisher.clone(),
                        kind: manifest.kind.as_str().to_string(),
                        display_name: manifest.name.clone(),
                        summary: manifest.name.clone(),
                        hidden: false,
                        created_at: now.clone(),
                        updated_at: now.clone(),
                    })
                    .await
                    .map_err(|e| verification_error(&e))?;
            }
        }

        // 5b. Version row. Published versions are IMMUTABLE (see
        // migrations/0046_marketplace.sql), so a re-submission of the same
        // version must carry the same bytes; it may not be replaced.
        match store
            .get_version(&manifest.id, &manifest.version)
            .await
            .map_err(|e| verification_error(&e))?
        {
            Some(existing) if existing.content_hash == content_hash => return Ok(()),
            Some(_) => {
                return Err(CodypendentError::new(
                    "marketplace.version-immutable",
                    format!(
                        "package `{}` version `{}` is already published with different content; publish a new version instead",
                        manifest.id, manifest.version
                    ),
                    false,
                ));
            }
            None => {}
        }

        store
            .create_version(&MarketplaceVersion {
                id: uuid::Uuid::now_v7().to_string(),
                package_id: manifest.id.clone(),
                version: manifest.version.clone(),
                content_hash,
                manifest_toml: toml_content.to_string(),
                // Record the signature only when one actually verified, which is
                // what the schema's `signed = 0 OR signature_b64 IS NOT NULL`
                // CHECK requires.
                signature_b64: verified.signed.then(|| manifest.security.signature.clone()),
                signed: verified.signed,
                source_url: "local://submitted".to_string(),
                artifact_bytes: artifact.len() as i64,
                min_daemon_version: None,
                max_daemon_version: None,
                published_at: now,
                yanked_at: None,
            })
            .await
            .map_err(|e| verification_error(&e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_marketplace::PublisherTrustTier;
    use codypendent_sandbox::{checksum_of, parse_manifest, signing_digest};
    use ed25519_dalek::{Signer, SigningKey};
    use sqlx::sqlite::SqlitePoolOptions;

    const OWNER: u32 = 1000;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::raw_sql(include_str!("../../../migrations/0046_marketplace.sql"))
            .execute(&pool)
            .await
            .expect("marketplace migration applies");
        pool
    }

    /// Build a manifest for `artifact`, signed by `signer` when one is given.
    fn manifest_toml(publisher: &str, artifact: &[u8], signer: Option<&SigningKey>) -> String {
        let checksum = checksum_of(artifact);
        let render = |signature: &str| {
            format!(
                r#"
schema_version = 1
id = "calc-tool"
name = "Calc Tool"
version = "1.0.0"
kind = "wasm-component"
publisher = "{publisher}"
scopes = ["workspace"]
[runtime]
command = "main.wasm"
[security]
checksum = "{checksum}"
signature = "{signature}"
"#
            )
        };
        let Some(signer) = signer else {
            return render("");
        };
        let unsigned = parse_manifest(&render("")).expect("unsigned manifest parses");
        let signature = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signer.sign(&signing_digest(&unsigned)).to_bytes(),
        );
        render(&signature)
    }

    async fn register_trusted_publisher(pool: &SqlitePool, id: &str, key: &SigningKey) {
        TrustManager::new(VerifiedStore::new(pool.clone()))
            .register_publisher(
                id,
                id,
                &hex::encode(key.verifying_key().to_bytes()),
                PublisherTrustTier::Trusted,
                Some("operator"),
            )
            .await
            .expect("publisher registers");
    }

    async fn counts(pool: &SqlitePool) -> (i64, i64, i64, i64) {
        let one = |table: &str| format!("SELECT COUNT(*) as n FROM {table}");
        let mut out = [0_i64; 4];
        for (slot, table) in [
            "marketplace_publishers",
            "marketplace_packages",
            "marketplace_versions",
            "marketplace_installs",
        ]
        .iter()
        .enumerate()
        {
            out[slot] = sqlx::query(&one(table))
                .fetch_one(pool)
                .await
                .expect("count")
                .get::<i64, _>("n");
        }
        (out[0], out[1], out[2], out[3])
    }

    /// The default posture on the path a client actually reaches: an unsigned
    /// package is refused, and refusing means writing nothing.
    #[tokio::test]
    async fn unsigned_package_is_refused_by_default() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        register_trusted_publisher(&pool, "acme", &key).await;
        let store = MarketplaceStore::new(pool.clone());

        let artifact = b"artifact bytes".to_vec();
        let toml = manifest_toml("acme", &artifact, None);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &artifact);

        let error = store
            .install(OWNER, "calc-tool", Some(&toml), Some(&encoded), false)
            .await
            .expect_err("an unsigned package must not install");
        assert_eq!(error.code, "marketplace.verification-failed");

        let (publishers, packages, versions, installs) = counts(&pool).await;
        assert_eq!(publishers, 1, "no publisher row is invented");
        assert_eq!((packages, versions, installs), (0, 0, 0));
    }

    /// A signature from a key that is not the publisher's registered key is a
    /// forgery, and `allow_unsigned` cannot wave it through — it waives a
    /// *missing* signature, never a wrong one.
    #[tokio::test]
    async fn a_badly_signed_package_is_refused_even_with_allow_unsigned() {
        let pool = pool().await;
        let publisher_key = SigningKey::from_bytes(&[7u8; 32]);
        let attacker_key = SigningKey::from_bytes(&[9u8; 32]);
        register_trusted_publisher(&pool, "acme", &publisher_key).await;
        let store = MarketplaceStore::new(pool.clone());

        let artifact = b"artifact bytes".to_vec();
        let toml = manifest_toml("acme", &artifact, Some(&attacker_key));
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &artifact);

        for allow_unsigned in [false, true] {
            let error = store
                .install(
                    OWNER,
                    "calc-tool",
                    Some(&toml),
                    Some(&encoded),
                    allow_unsigned,
                )
                .await
                .expect_err("a forged signature must not install");
            assert_eq!(error.code, "marketplace.verification-failed");
        }

        let (_, packages, versions, installs) = counts(&pool).await;
        assert_eq!((packages, versions, installs), (0, 0, 0));
    }

    /// Artifact bytes that do not hash to the manifest's checksum are refused,
    /// signature or not.
    #[tokio::test]
    async fn a_tampered_artifact_is_refused() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        register_trusted_publisher(&pool, "acme", &key).await;
        let store = MarketplaceStore::new(pool.clone());

        let toml = manifest_toml("acme", b"the real bytes", Some(&key));
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"tampered");

        let error = store
            .install(OWNER, "calc-tool", Some(&toml), Some(&encoded), true)
            .await
            .expect_err("a tampered artifact must not install");
        assert_eq!(error.code, "marketplace.verification-failed");

        let (_, packages, versions, installs) = counts(&pool).await;
        assert_eq!((packages, versions, installs), (0, 0, 0));
    }

    /// No trust-on-first-use: an unregistered publisher is refused and, crucially,
    /// is never inserted into the trust table by the act of submitting a package.
    #[tokio::test]
    async fn an_unknown_publisher_is_never_trusted_on_first_use() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let store = MarketplaceStore::new(pool.clone());

        let artifact = b"artifact bytes".to_vec();
        let toml = manifest_toml("stranger", &artifact, Some(&key));
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &artifact);

        let error = store
            .install(OWNER, "calc-tool", Some(&toml), Some(&encoded), true)
            .await
            .expect_err("an unknown publisher must not install");
        assert_eq!(error.code, "marketplace.unknown-publisher");

        let (publishers, packages, versions, installs) = counts(&pool).await;
        assert_eq!(
            (publishers, packages, versions, installs),
            (0, 0, 0, 0),
            "submitting a package must not register its publisher"
        );
    }

    /// A registered-but-untrusted publisher is refused too: a row in the table is
    /// not a grant.
    #[tokio::test]
    async fn an_untrusted_publisher_is_refused() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        TrustManager::new(VerifiedStore::new(pool.clone()))
            .register_publisher(
                "acme",
                "acme",
                &hex::encode(key.verifying_key().to_bytes()),
                PublisherTrustTier::Untrusted,
                None,
            )
            .await
            .expect("publisher registers");
        let store = MarketplaceStore::new(pool.clone());

        let artifact = b"artifact bytes".to_vec();
        let toml = manifest_toml("acme", &artifact, Some(&key));
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &artifact);

        let error = store
            .install(OWNER, "calc-tool", Some(&toml), Some(&encoded), true)
            .await
            .expect_err("an untrusted publisher must not install");
        assert_eq!(error.code, "marketplace.untrusted-publisher");
    }

    /// A manifest submitted without its artifact cannot be verified, so it is
    /// refused rather than registered against a hash of its own text.
    #[tokio::test]
    async fn a_manifest_without_its_artifact_is_refused() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        register_trusted_publisher(&pool, "acme", &key).await;
        let store = MarketplaceStore::new(pool.clone());

        let toml = manifest_toml("acme", b"artifact bytes", Some(&key));
        let error = store
            .install(OWNER, "calc-tool", Some(&toml), None, false)
            .await
            .expect_err("a manifest with no artifact must not install");
        assert_eq!(error.code, "marketplace.artifact-required");

        let (_, packages, versions, installs) = counts(&pool).await;
        assert_eq!((packages, versions, installs), (0, 0, 0));
    }

    /// The positive control: a correctly signed package from a trusted publisher
    /// installs, and the row records the signature that actually verified.
    #[tokio::test]
    async fn a_signed_package_from_a_trusted_publisher_installs_disabled() {
        let pool = pool().await;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        register_trusted_publisher(&pool, "acme", &key).await;
        let store = MarketplaceStore::new(pool.clone());

        let artifact = b"artifact bytes".to_vec();
        let toml = manifest_toml("acme", &artifact, Some(&key));
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &artifact);

        let view = store
            .install(OWNER, "calc-tool", Some(&toml), Some(&encoded), false)
            .await
            .expect("a verified package installs");
        assert_eq!(view.id, "calc-tool");
        assert_eq!(view.publisher_id, "acme");
        assert_eq!(view.kind, "wasm-component");
        assert_eq!(
            view.lifecycle.as_deref(),
            Some("installed_disabled"),
            "installing never enables"
        );

        let row =
            sqlx::query("SELECT signed, signature_b64, content_hash FROM marketplace_versions")
                .fetch_one(&pool)
                .await
                .expect("version row");
        assert_eq!(row.get::<i64, _>("signed"), 1);
        assert!(row.get::<Option<String>, _>("signature_b64").is_some());
        assert_eq!(
            row.get::<String, _>("content_hash"),
            checksum_of(&artifact),
            "the recorded hash is the one verification proved"
        );
    }
}

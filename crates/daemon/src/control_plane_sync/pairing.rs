//! Pairing lifecycle, consent manifests, and credential references.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::{
    DataClassification, FederatedRepositoryId, PublicationClass,
    Repository as ControlPlaneRepository,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::error::ControlPlaneSyncError;

/// Lifecycle state of a pairing record on the local daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairingState {
    Pending,
    Active,
    Revoked,
    Expired,
}

impl PairingState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }

    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// A persistent pairing between this local daemon and a remote control plane organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPlanePairing {
    pub id: String,
    pub owner_uid: u32,
    pub endpoint: String,
    pub organization_id: String,
    pub organization_display_name: String,
    pub consent_manifest: String,
    pub consent_manifest_hash: String,
    pub max_publication_class: PublicationClass,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    pub state: PairingState,
    pub paired_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Repository scope derived from the durable consent manifest and the
/// authenticated control-plane catalog.
///
/// `by_identity` deliberately contains aliases only for repositories already
/// selected by the manifest. It is therefore safe to use while translating
/// local outbox payload identities without turning catalog visibility into
/// publication consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedRepositoryConsent {
    repository_ids: Vec<Uuid>,
    by_identity: HashMap<String, Uuid>,
    policy_by_repository_id: HashMap<Uuid, ResolvedRepositoryPolicy>,
    mapping_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedRepositoryPolicy {
    pub(crate) repository_id: Uuid,
    pub(crate) max_publication_class: PublicationClass,
    pub(crate) max_classification: DataClassification,
}

impl ResolvedRepositoryConsent {
    #[must_use]
    pub(crate) fn repository_ids(&self) -> &[Uuid] {
        &self.repository_ids
    }

    #[must_use]
    pub(crate) fn repository_id_for(&self, identity: &str) -> Option<Uuid> {
        self.by_identity.get(identity).copied().or_else(|| {
            Uuid::parse_str(identity)
                .ok()
                .filter(|candidate| self.repository_ids.contains(candidate))
        })
    }

    #[must_use]
    pub(crate) fn repository_policy_for(&self, identity: &str) -> Option<ResolvedRepositoryPolicy> {
        let repository_id = self.repository_id_for(identity)?;
        self.repository_policy_for_id(repository_id)
    }

    #[must_use]
    pub(crate) fn repository_policy_for_id(
        &self,
        repository_id: Uuid,
    ) -> Option<ResolvedRepositoryPolicy> {
        self.policy_by_repository_id.get(&repository_id).copied()
    }

    #[must_use]
    pub(crate) const fn mapping_changed(&self) -> bool {
        self.mapping_changed
    }
}

impl ControlPlanePairing {
    /// Repository identities the user approved in the durable consent manifest.
    ///
    /// These values are deliberately opaque here. Current manifests contain
    /// federated SHA-256 identities; older manifests can contain local aliases
    /// such as `repo_abc` or, for a short-lived compatibility window, a remote
    /// UUID. Re-serializing or validating the manifest must never rewrite those
    /// identities into a different identifier domain.
    pub fn allowed_repository_identities(&self) -> Result<Vec<String>, ControlPlaneSyncError> {
        let manifest: LocalConsentManifest =
            serde_json::from_str(&self.consent_manifest).map_err(|error| {
                ControlPlaneSyncError::InvalidConsentManifest(format!(
                    "stored consent manifest is invalid: {error}"
                ))
            })?;
        if manifest.compute_hash() != self.consent_manifest_hash {
            return Err(ControlPlaneSyncError::InvalidConsentManifest(
                "stored consent manifest hash does not match its canonical contents".to_string(),
            ));
        }
        if manifest.organization_id != self.organization_id {
            return Err(ControlPlaneSyncError::InvalidConsentManifest(
                "stored consent manifest organization does not match pairing".to_string(),
            ));
        }
        if manifest.allowed_repositories.len() > 256 {
            return Err(ControlPlaneSyncError::InvalidConsentManifest(
                "stored consent manifest names more than 256 repositories".to_string(),
            ));
        }
        if manifest
            .allowed_repositories
            .iter()
            .any(|identity| identity.is_empty())
        {
            return Err(ControlPlaneSyncError::InvalidConsentManifest(
                "stored consent manifest contains an empty repository identity".to_string(),
            ));
        }
        Ok(manifest.allowed_repositories)
    }

    /// Resolve approved repository identities to the UUIDs required by sync.
    ///
    /// The authenticated organization repository catalog is the only trusted
    /// mapping between federated identities and control-plane UUIDs. Exact
    /// federated-id matching is the primary path. A UUID-shaped value is
    /// accepted only when it exactly names a repository in that same catalog,
    /// preserving manifests written by the interim UUID-based implementation.
    /// Arbitrary legacy aliases are first resolved through the daemon's durable
    /// `federated_repository_identity` table, then through the authenticated
    /// remote catalog. A missing or ambiguous link fails closed; guessing from
    /// display names would risk syncing the wrong repository.
    pub(crate) async fn resolve_repository_consent(
        &self,
        pool: &SqlitePool,
        catalog: &[ControlPlaneRepository],
    ) -> Result<ResolvedRepositoryConsent, ControlPlaneSyncError> {
        let organization_id = Uuid::parse_str(&self.organization_id).map_err(|_| {
            ControlPlaneSyncError::InvalidConsentManifest(
                "stored consent manifest organization is not a control-plane UUID".to_string(),
            )
        })?;
        let identities = self.allowed_repository_identities()?;

        let mut by_federated_id = HashMap::with_capacity(catalog.len());
        let mut by_control_plane_id = HashMap::with_capacity(catalog.len());
        let mut federated_id_by_control_plane_id = HashMap::with_capacity(catalog.len());
        let mut catalog_policy_by_repository_id = HashMap::with_capacity(catalog.len());
        for repository in catalog {
            if repository.organization_id.as_uuid() != organization_id {
                return Err(ControlPlaneSyncError::RemoteRejected(
                    "repository catalog contained an entry outside the paired organization"
                        .to_string(),
                ));
            }
            let repository_id = repository.id.as_uuid();
            if repository.max_publication_class == PublicationClass::Unknown
                || repository.max_classification == DataClassification::Unknown
            {
                return Err(ControlPlaneSyncError::RemoteRejected(
                    "repository catalog contained an unrecognized policy ceiling".to_string(),
                ));
            }
            if by_federated_id
                .insert(repository.federated_id.as_str().to_string(), repository_id)
                .is_some()
                || by_control_plane_id
                    .insert(repository_id, repository_id)
                    .is_some()
                || federated_id_by_control_plane_id
                    .insert(repository_id, repository.federated_id.as_str().to_string())
                    .is_some()
                || catalog_policy_by_repository_id
                    .insert(
                        repository_id,
                        ResolvedRepositoryPolicy {
                            repository_id,
                            max_publication_class: repository.max_publication_class,
                            max_classification: repository.max_classification,
                        },
                    )
                    .is_some()
            {
                return Err(ControlPlaneSyncError::RemoteRejected(
                    "repository catalog contained duplicate identities".to_string(),
                ));
            }
        }

        let local_identity_rows: Vec<(String, String)> =
            sqlx::query_as("SELECT repository_id, federated_id FROM federated_repository_identity")
                .fetch_all(pool)
                .await?;
        let mut federated_id_by_local_identity = HashMap::with_capacity(local_identity_rows.len());
        let mut local_identities_by_federated_id: HashMap<String, Vec<String>> =
            HashMap::with_capacity(local_identity_rows.len());
        for (local_identity, federated_id) in local_identity_rows {
            if federated_id_by_local_identity
                .insert(local_identity.clone(), federated_id.clone())
                .is_some()
            {
                return Err(ControlPlaneSyncError::PolicyViolation(
                    "local repository identity table contained duplicate aliases".to_string(),
                ));
            }
            local_identities_by_federated_id
                .entry(federated_id)
                .or_default()
                .push(local_identity);
        }

        let mut seen_identities = HashSet::with_capacity(identities.len());
        let mut seen_repository_ids = HashSet::with_capacity(identities.len());
        let mut repository_ids = Vec::with_capacity(identities.len());
        let mut by_identity = HashMap::with_capacity(identities.len() * 4);
        let mut policy_by_repository_id = HashMap::with_capacity(identities.len());
        let mut consented_local_mappings = HashMap::with_capacity(identities.len());
        for identity in identities {
            if !seen_identities.insert(identity.clone()) {
                return Err(ControlPlaneSyncError::InvalidConsentManifest(format!(
                    "stored consent manifest repeats repository identity `{identity}`"
                )));
            }

            let mut candidates = HashSet::with_capacity(3);
            if let Some(repository_id) = by_federated_id.get(identity.as_str()) {
                candidates.insert(*repository_id);
            }
            if let Some(repository_id) = Uuid::parse_str(&identity)
                .ok()
                .and_then(|id| by_control_plane_id.get(&id))
            {
                candidates.insert(*repository_id);
            }

            if let Some(local_federated_id) = federated_id_by_local_identity.get(&identity) {
                let local_federated_id = FederatedRepositoryId::new(local_federated_id.clone())
                    .map_err(|_| {
                        ControlPlaneSyncError::InvalidConsentManifest(format!(
                            "local repository identity `{identity}` has a malformed federated mapping"
                        ))
                    })?;
                if let Some(repository_id) = by_federated_id.get(local_federated_id.as_str()) {
                    candidates.insert(*repository_id);
                }
            }

            let repository_id = match candidates.len() {
                1 => candidates.into_iter().next().expect("one candidate"),
                0 => {
                    return Err(ControlPlaneSyncError::InvalidConsentManifest(format!(
                        "approved repository identity `{identity}` has no unambiguous mapping in the local identity table and authenticated control-plane catalog; re-pair to refresh repository consent"
                    )))
                }
                _ => {
                    return Err(ControlPlaneSyncError::InvalidConsentManifest(format!(
                        "approved repository identity `{identity}` resolves ambiguously; re-pair to refresh repository consent"
                    )))
                }
            };

            let federated_id = federated_id_by_control_plane_id
                .get(&repository_id)
                .expect("resolved repository came from the validated catalog");
            let policy = *catalog_policy_by_repository_id
                .get(&repository_id)
                .expect("resolved repository came from the validated catalog");
            let policy_fingerprint = format!(
                "{}:{}",
                policy.max_publication_class.as_str(),
                policy.max_classification.as_str()
            );
            let mut aliases = vec![identity, repository_id.to_string(), federated_id.clone()];
            if let Some(local_identities) =
                local_identities_by_federated_id.get(federated_id.as_str())
            {
                if local_identities.len() > 1 {
                    return Err(ControlPlaneSyncError::PolicyViolation(
                        "local repository identity table maps multiple aliases to one federated identity"
                            .to_string(),
                    ));
                }
                aliases.extend(local_identities.iter().cloned());
                for local_identity in local_identities {
                    let mapping = (repository_id, policy_fingerprint.clone());
                    if let Some(existing) =
                        consented_local_mappings.insert(local_identity.clone(), mapping.clone())
                    {
                        if existing != mapping {
                            return Err(ControlPlaneSyncError::InvalidConsentManifest(format!(
                                "local repository identity `{local_identity}` maps to multiple consented repositories"
                            )));
                        }
                    }
                }
            } else {
                consented_local_mappings.insert(
                    repository_id.to_string(),
                    (repository_id, policy_fingerprint),
                );
            }
            for alias in aliases {
                if let Some(existing) = by_identity.insert(alias.clone(), repository_id) {
                    if existing != repository_id {
                        return Err(ControlPlaneSyncError::InvalidConsentManifest(format!(
                            "repository identity `{alias}` resolves ambiguously; re-pair to refresh repository consent"
                        )));
                    }
                }
            }
            if seen_repository_ids.insert(repository_id) {
                repository_ids.push(repository_id);
                policy_by_repository_id.insert(repository_id, policy);
            }
        }

        let existing_local_mappings: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT local_id, remote_id, class FROM control_plane_remote_objects \
             WHERE pairing_id = ? AND local_kind = 'repository-consent'",
        )
        .bind(&self.id)
        .fetch_all(pool)
        .await?;
        let existing_local_mappings: HashMap<String, (Uuid, String)> = existing_local_mappings
            .into_iter()
            .map(|(local_identity, repository_id, policy_fingerprint)| {
                Uuid::parse_str(&repository_id)
                    .map(|repository_id| (local_identity, (repository_id, policy_fingerprint)))
                    .map_err(|_| {
                        ControlPlaneSyncError::PolicyViolation(
                            "cached repository-consent mapping contains an invalid remote UUID"
                                .to_string(),
                        )
                    })
            })
            .collect::<Result<_, _>>()?;
        let mapping_changed = existing_local_mappings != consented_local_mappings;

        // Replace only this pairing's repository-consent cache, and only after
        // the entire manifest/catalog resolution has succeeded. Publication
        // target selection still requires an exact manifest match on the
        // cached remote UUID; catalog visibility alone never grants consent.
        if mapping_changed {
            let mut tx = pool.begin().await?;
            sqlx::query(
                "DELETE FROM control_plane_remote_objects \
                 WHERE pairing_id = ? AND local_kind = 'repository-consent'",
            )
            .bind(&self.id)
            .execute(&mut *tx)
            .await?;
            let now = Utc::now().to_rfc3339();
            for (local_identity, (repository_id, policy_fingerprint)) in &consented_local_mappings {
                sqlx::query(
                    "INSERT INTO control_plane_remote_objects \
                     (pairing_id, local_kind, local_id, remote_id, class, published_at) \
                     VALUES (?, 'repository-consent', ?, ?, ?, ?)",
                )
                .bind(&self.id)
                .bind(local_identity)
                .bind(repository_id.to_string())
                .bind(policy_fingerprint)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
        }

        Ok(ResolvedRepositoryConsent {
            repository_ids,
            by_identity,
            policy_by_repository_id,
            mapping_changed,
        })
    }
}

/// Metadata referencing an OS-keychain-stored bearer/refresh credential handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPlaneCredential {
    pub pairing_id: String,
    pub credential_ref: String,
    pub credential_hash: String,
    pub audience: String,
    pub purpose: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub rotated_at: Option<DateTime<Utc>>,
}

/// Normalized consent manifest displayed to the user prior to confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalConsentManifest {
    pub organization_id: String,
    pub organization_display_name: String,
    pub endpoint: String,
    pub max_publication_class: PublicationClass,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    /// Opaque local/federated identities approved by the user. Never reinterpret
    /// these as control-plane UUIDs without consulting the authenticated remote
    /// repository catalog.
    #[serde(default)]
    pub allowed_repositories: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl LocalConsentManifest {
    /// Compute deterministic SHA-256 hash of the canonical serialized JSON.
    #[must_use]
    pub fn compute_hash(&self) -> String {
        let serialized = serde_json::to_string(self).expect("serialize manifest");
        let mut hasher = Sha256::new();
        hasher.update(serialized.as_bytes());
        hex::encode(hasher.finalize())
    }
}

/// Validate and normalize a control plane base URL.
pub fn normalize_endpoint(raw: &str) -> Result<String, ControlPlaneSyncError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ControlPlaneSyncError::InvalidConsentManifest(
            "endpoint must specify http:// or https:// scheme".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Record a new active or pending pairing alongside its credential handle.
pub async fn record_pairing(
    pool: &SqlitePool,
    pairing: &ControlPlanePairing,
    credential: &ControlPlaneCredential,
) -> Result<(), ControlPlaneSyncError> {
    let mut tx = pool.begin().await?;

    let paired_at_str = pairing.paired_at.map(|t| t.to_rfc3339());
    let expires_at_str = pairing.expires_at.map(|t| t.to_rfc3339());
    let revoked_at_str = pairing.revoked_at.map(|t| t.to_rfc3339());
    let created_at_str = pairing.created_at.to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO control_plane_pairings (
            id, owner_uid, endpoint, organization_id, organization_display_name,
            consent_manifest, consent_manifest_hash, max_publication_class,
            accepts_remote_approvals, accepts_runner_dispatch, state,
            paired_at, expires_at, revoked_at, revoked_reason, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner_uid, endpoint, organization_id) DO UPDATE SET
            organization_display_name = excluded.organization_display_name,
            consent_manifest = excluded.consent_manifest,
            consent_manifest_hash = excluded.consent_manifest_hash,
            max_publication_class = excluded.max_publication_class,
            accepts_remote_approvals = excluded.accepts_remote_approvals,
            accepts_runner_dispatch = excluded.accepts_runner_dispatch,
            state = excluded.state,
            paired_at = excluded.paired_at,
            expires_at = excluded.expires_at,
            revoked_at = excluded.revoked_at,
            revoked_reason = excluded.revoked_reason
        "#,
    )
    .bind(&pairing.id)
    .bind(pairing.owner_uid as i64)
    .bind(&pairing.endpoint)
    .bind(&pairing.organization_id)
    .bind(&pairing.organization_display_name)
    .bind(&pairing.consent_manifest)
    .bind(&pairing.consent_manifest_hash)
    .bind(pairing.max_publication_class.as_str())
    .bind(if pairing.accepts_remote_approvals {
        1
    } else {
        0
    })
    .bind(if pairing.accepts_runner_dispatch {
        1
    } else {
        0
    })
    .bind(pairing.state.as_str())
    .bind(paired_at_str)
    .bind(expires_at_str)
    .bind(revoked_at_str)
    .bind(&pairing.revoked_reason)
    .bind(created_at_str)
    .execute(&mut *tx)
    .await?;

    let cred_issued_str = credential.issued_at.to_rfc3339();
    let cred_expires_str = credential.expires_at.to_rfc3339();
    let cred_rotated_str = credential.rotated_at.map(|t| t.to_rfc3339());

    sqlx::query(
        r#"
        INSERT INTO control_plane_credentials (
            pairing_id, credential_ref, credential_hash, audience, purpose,
            issued_at, expires_at, rotated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(pairing_id) DO UPDATE SET
            credential_ref = excluded.credential_ref,
            credential_hash = excluded.credential_hash,
            audience = excluded.audience,
            purpose = excluded.purpose,
            issued_at = excluded.issued_at,
            expires_at = excluded.expires_at,
            rotated_at = excluded.rotated_at
        "#,
    )
    .bind(&credential.pairing_id)
    .bind(&credential.credential_ref)
    .bind(&credential.credential_hash)
    .bind(&credential.audience)
    .bind(&credential.purpose)
    .bind(cred_issued_str)
    .bind(cred_expires_str)
    .bind(cred_rotated_str)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

fn parse_publication_class(s: &str) -> PublicationClass {
    match s {
        "private-local" => PublicationClass::PrivateLocal,
        "metadata-shared" => PublicationClass::MetadataShared,
        "content-shared" => PublicationClass::ContentShared,
        "organization-knowledge" => PublicationClass::OrganizationKnowledge,
        "public-marketplace" => PublicationClass::PublicMarketplace,
        _ => PublicationClass::Unknown,
    }
}

fn parse_row_to_pairing(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ControlPlanePairing, ControlPlaneSyncError> {
    let id: String = row.get("id");
    let owner_uid: i64 = row.get("owner_uid");
    let endpoint: String = row.get("endpoint");
    let organization_id: String = row.get("organization_id");
    let organization_display_name: String = row.get("organization_display_name");
    let consent_manifest: String = row.get("consent_manifest");
    let consent_manifest_hash: String = row.get("consent_manifest_hash");
    let max_class_str: String = row.get("max_publication_class");
    let accepts_remote_approvals: i64 = row.get("accepts_remote_approvals");
    let accepts_runner_dispatch: i64 = row.get("accepts_runner_dispatch");
    let state_str: String = row.get("state");
    let paired_at_str: Option<String> = row.get("paired_at");
    let expires_at_str: Option<String> = row.get("expires_at");
    let revoked_at_str: Option<String> = row.get("revoked_at");
    let revoked_reason: Option<String> = row.get("revoked_reason");
    let created_at_str: String = row.get("created_at");

    let state = PairingState::parse_str(&state_str).unwrap_or(PairingState::Revoked);
    let max_publication_class = parse_publication_class(&max_class_str);

    let paired_at = paired_at_str.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    });
    let expires_at = expires_at_str.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    });
    let revoked_at = revoked_at_str.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    });
    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    Ok(ControlPlanePairing {
        id,
        owner_uid: owner_uid as u32,
        endpoint,
        organization_id,
        organization_display_name,
        consent_manifest,
        consent_manifest_hash,
        max_publication_class,
        accepts_remote_approvals: accepts_remote_approvals != 0,
        accepts_runner_dispatch: accepts_runner_dispatch != 0,
        state,
        paired_at,
        expires_at,
        revoked_at,
        revoked_reason,
        created_at,
    })
}

/// Fetch a pairing by its unique ID.
pub async fn get_pairing(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<Option<ControlPlanePairing>, ControlPlaneSyncError> {
    let row = sqlx::query("SELECT * FROM control_plane_pairings WHERE id = ?")
        .bind(pairing_id)
        .fetch_optional(pool)
        .await?;

    match row {
        Some(r) => Ok(Some(parse_row_to_pairing(&r)?)),
        None => Ok(None),
    }
}

/// Fetch all active pairings on this daemon.
pub async fn list_active_pairings(
    pool: &SqlitePool,
) -> Result<Vec<ControlPlanePairing>, ControlPlaneSyncError> {
    let rows = sqlx::query(
        "SELECT * FROM control_plane_pairings WHERE state = 'active' ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for r in &rows {
        result.push(parse_row_to_pairing(r)?);
    }
    Ok(result)
}

/// List all pairings owned by a specific OS user.
pub async fn list_pairings_for_owner(
    pool: &SqlitePool,
    owner_uid: u32,
) -> Result<Vec<ControlPlanePairing>, ControlPlaneSyncError> {
    let rows = sqlx::query(
        "SELECT * FROM control_plane_pairings WHERE owner_uid = ? ORDER BY created_at ASC",
    )
    .bind(owner_uid as i64)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for r in &rows {
        result.push(parse_row_to_pairing(r)?);
    }
    Ok(result)
}

/// Revoke an active pairing.
pub async fn revoke_pairing(
    pool: &SqlitePool,
    pairing_id: &str,
    reason: &str,
) -> Result<(), ControlPlaneSyncError> {
    let now = Utc::now().to_rfc3339();
    let rows_affected = sqlx::query(
        r#"
        UPDATE control_plane_pairings
        SET state = 'revoked', revoked_at = ?, revoked_reason = ?
        WHERE id = ? AND state <> 'revoked'
        "#,
    )
    .bind(now)
    .bind(reason)
    .bind(pairing_id)
    .execute(pool)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        // Pairing was either already revoked or not found
        let exists = get_pairing(pool, pairing_id).await?;
        if exists.is_none() {
            return Err(ControlPlaneSyncError::Unpaired);
        }
    }

    Ok(())
}

/// Fetch stored credential reference for a pairing.
pub async fn get_credential(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<Option<ControlPlaneCredential>, ControlPlaneSyncError> {
    let row = sqlx::query("SELECT * FROM control_plane_credentials WHERE pairing_id = ?")
        .bind(pairing_id)
        .fetch_optional(pool)
        .await?;

    match row {
        Some(r) => {
            let pairing_id: String = r.get("pairing_id");
            let credential_ref: String = r.get("credential_ref");
            let credential_hash: String = r.get("credential_hash");
            let audience: String = r.get("audience");
            let purpose: String = r.get("purpose");
            let issued_at_str: String = r.get("issued_at");
            let expires_at_str: String = r.get("expires_at");
            let rotated_at_str: Option<String> = r.get("rotated_at");

            let issued_at = DateTime::parse_from_rfc3339(&issued_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let expires_at = DateTime::parse_from_rfc3339(&expires_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let rotated_at = rotated_at_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });

            Ok(Some(ControlPlaneCredential {
                pairing_id,
                credential_ref,
                credential_hash,
                audience,
                purpose,
                issued_at,
                expires_at,
                rotated_at,
            }))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use codypendent_control_plane_protocol::{DataClassification, OrganizationId, RepositoryId};
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    fn pairing_with_repositories(
        organization_id: Uuid,
        allowed_repositories: Vec<String>,
    ) -> ControlPlanePairing {
        let now = Utc::now();
        let manifest = LocalConsentManifest {
            organization_id: organization_id.to_string(),
            organization_display_name: "Acme".to_string(),
            endpoint: "https://control-plane.example.com".to_string(),
            max_publication_class: PublicationClass::MetadataShared,
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            allowed_repositories,
            created_at: now,
        };
        ControlPlanePairing {
            id: Uuid::now_v7().to_string(),
            owner_uid: 501,
            endpoint: manifest.endpoint.clone(),
            organization_id: manifest.organization_id.clone(),
            organization_display_name: manifest.organization_display_name.clone(),
            consent_manifest: serde_json::to_string(&manifest).expect("serialize manifest"),
            consent_manifest_hash: manifest.compute_hash(),
            max_publication_class: manifest.max_publication_class,
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: PairingState::Active,
            paired_at: Some(now),
            expires_at: None,
            revoked_at: None,
            revoked_reason: None,
            created_at: now,
        }
    }

    fn catalog_repository(
        organization_id: Uuid,
        repository_id: Uuid,
        federated_id: &str,
    ) -> ControlPlaneRepository {
        ControlPlaneRepository {
            id: RepositoryId::from_uuid(repository_id),
            organization_id: OrganizationId::from_uuid(organization_id),
            federated_id: FederatedRepositoryId::new(federated_id)
                .expect("valid federated repository id"),
            display_name: "Repository".to_string(),
            max_publication_class: PublicationClass::MetadataShared,
            max_classification: DataClassification::Internal,
            policy_version: 1,
            created_at: Utc::now(),
        }
    }

    async fn identity_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open identity database");
        sqlx::query(
            "CREATE TABLE federated_repository_identity (\
                repository_id TEXT PRIMARY KEY, \
                federated_id TEXT NOT NULL UNIQUE\
            )",
        )
        .execute(&pool)
        .await
        .expect("create identity table");
        sqlx::query(
            "CREATE TABLE control_plane_remote_objects (\
                pairing_id TEXT NOT NULL, \
                local_kind TEXT NOT NULL, \
                local_id TEXT NOT NULL, \
                remote_id TEXT NOT NULL, \
                class TEXT NOT NULL, \
                published_at TEXT NOT NULL, \
                PRIMARY KEY (pairing_id, local_kind, local_id), \
                UNIQUE (pairing_id, remote_id)\
            )",
        )
        .execute(&pool)
        .await
        .expect("create remote object table");
        pool
    }

    #[tokio::test]
    async fn a_federated_hash_resolves_to_the_control_plane_repository_uuid() {
        let organization_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let federated_id = "a".repeat(64);
        let pairing = pairing_with_repositories(organization_id, vec![federated_id.clone()]);
        let pool = identity_pool().await;
        let catalog = vec![catalog_repository(
            organization_id,
            repository_id,
            &federated_id,
        )];

        let consent = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect("resolve federated identity");
        assert_eq!(consent.repository_ids(), &[repository_id]);
        assert_eq!(
            consent.repository_id_for(&federated_id),
            Some(repository_id)
        );
        assert_eq!(
            pairing
                .allowed_repository_identities()
                .expect("read identities"),
            vec![federated_id]
        );
    }

    #[tokio::test]
    async fn an_arbitrary_legacy_identity_resolves_via_the_local_federated_mapping() {
        let organization_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let federated_id = "b".repeat(64);
        let pairing = pairing_with_repositories(organization_id, vec!["repo_abc".to_string()]);
        let pool = identity_pool().await;
        sqlx::query(
            "INSERT INTO federated_repository_identity (repository_id, federated_id) VALUES (?, ?)",
        )
        .bind("repo_abc")
        .bind(&federated_id)
        .execute(&pool)
        .await
        .expect("seed local federated mapping");
        let catalog = vec![catalog_repository(
            organization_id,
            repository_id,
            &federated_id,
        )];

        assert_eq!(
            pairing
                .allowed_repository_identities()
                .expect("legacy identity remains readable"),
            vec!["repo_abc"]
        );
        let consent = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect("resolve local alias through its federated identity");
        assert_eq!(consent.repository_ids(), &[repository_id]);
        assert_eq!(consent.repository_id_for("repo_abc"), Some(repository_id));
        assert_eq!(
            consent.repository_id_for(&federated_id),
            Some(repository_id)
        );
    }

    #[tokio::test]
    async fn an_interim_control_plane_uuid_manifest_remains_upgrade_compatible() {
        let organization_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let pairing = pairing_with_repositories(organization_id, vec![repository_id.to_string()]);
        let pool = identity_pool().await;
        let federated_id = "c".repeat(64);
        sqlx::query(
            "INSERT INTO federated_repository_identity (repository_id, federated_id) VALUES (?, ?)",
        )
        .bind("repo_local")
        .bind(&federated_id)
        .execute(&pool)
        .await
        .expect("seed local federated mapping");
        let catalog = vec![catalog_repository(
            organization_id,
            repository_id,
            &federated_id,
        )];

        let consent = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect("resolve interim UUID through the catalog");
        assert!(consent.mapping_changed());
        assert_eq!(consent.repository_ids(), &[repository_id]);
        assert_eq!(
            consent.repository_id_for(&repository_id.to_string()),
            Some(repository_id)
        );
        assert_eq!(consent.repository_id_for("repo_local"), Some(repository_id));
        let cached_remote_id: String = sqlx::query_scalar(
            "SELECT remote_id FROM control_plane_remote_objects \
             WHERE pairing_id = ? AND local_kind = 'repository-consent' AND local_id = 'repo_local'",
        )
        .bind(&pairing.id)
        .fetch_one(&pool)
        .await
        .expect("cached pairing-scoped repository mapping");
        assert_eq!(cached_remote_id, repository_id.to_string());
        let unchanged = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect("resolve the unchanged cached mapping");
        assert!(!unchanged.mapping_changed());
    }

    #[tokio::test]
    async fn an_unmapped_legacy_alias_fails_closed() {
        let organization_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let pairing = pairing_with_repositories(organization_id, vec!["repo_abc".to_string()]);
        let pool = identity_pool().await;
        let catalog = vec![catalog_repository(
            organization_id,
            repository_id,
            &"d".repeat(64),
        )];

        let error = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect_err("an unmapped alias must never be guessed from a display name");
        assert!(error.to_string().contains("`repo_abc`"));
        assert!(error.to_string().contains("no unambiguous mapping"));
    }

    #[tokio::test]
    async fn a_tampered_manifest_cannot_resolve_inbound_or_outbound_repository_scope() {
        let organization_id = Uuid::now_v7();
        let repository_id = Uuid::now_v7();
        let mut pairing =
            pairing_with_repositories(organization_id, vec![repository_id.to_string()]);
        pairing.consent_manifest_hash = "0".repeat(64);
        let pool = identity_pool().await;
        let catalog = vec![catalog_repository(
            organization_id,
            repository_id,
            &"d".repeat(64),
        )];

        let error = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect_err("tampered consent must never resolve sync scope");

        assert!(error.to_string().contains("hash does not match"));
    }

    #[tokio::test]
    async fn outbound_aliases_are_added_only_for_manifest_authorized_repositories() {
        let organization_id = Uuid::now_v7();
        let authorized_repository_id = Uuid::now_v7();
        let other_repository_id = Uuid::now_v7();
        let authorized_federated_id = "e".repeat(64);
        let other_federated_id = "f".repeat(64);
        let pairing =
            pairing_with_repositories(organization_id, vec![authorized_federated_id.clone()]);
        let pool = identity_pool().await;
        for (local_identity, federated_id) in [
            ("repo_authorized", authorized_federated_id.as_str()),
            ("repo_other", other_federated_id.as_str()),
        ] {
            sqlx::query(
                "INSERT INTO federated_repository_identity (repository_id, federated_id) VALUES (?, ?)",
            )
            .bind(local_identity)
            .bind(federated_id)
            .execute(&pool)
            .await
            .expect("seed local federated mapping");
        }
        let catalog = vec![
            catalog_repository(
                organization_id,
                authorized_repository_id,
                &authorized_federated_id,
            ),
            catalog_repository(organization_id, other_repository_id, &other_federated_id),
        ];

        let consent = pairing
            .resolve_repository_consent(&pool, &catalog)
            .await
            .expect("resolve authorized repository only");

        assert_eq!(consent.repository_ids(), &[authorized_repository_id]);
        assert_eq!(
            consent.repository_id_for("repo_authorized"),
            Some(authorized_repository_id)
        );
        assert_eq!(consent.repository_id_for("repo_other"), None);
        assert_eq!(consent.repository_id_for(&other_federated_id), None);
        assert_eq!(
            consent.repository_id_for(&other_repository_id.to_string()),
            None
        );
    }
}

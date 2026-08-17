//! Pairing lifecycle, consent manifests, and credential references.

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::PublicationClass;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

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
    pub fn from_str(s: &str) -> Option<Self> {
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

    let state = PairingState::from_str(&state_str).unwrap_or(PairingState::Revoked);
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

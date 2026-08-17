//! The SecretBroker core service.
//!
//! Implements context-bound secret reference registration, lease issuance,
//! lease resolution, revocation, rotation, and non-secret audit logging.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::audit::{AuditEvent, AuditOutcome, SecretAuditRecord};
use crate::backend::{SecretBackend, SecretBackendKind};
use crate::lease::{LeaseContext, LeaseState, LeasedSecret, SecretLease};
use crate::reference::SecretReference;
use crate::{BackendErrorCode, SecretError};

/// The central secret broker.
#[derive(Clone)]
pub struct SecretBroker {
    pool: SqlitePool,
    backends: HashMap<SecretBackendKind, Arc<dyn SecretBackend>>,
}

impl SecretBroker {
    /// Construct a new SecretBroker over a database pool and a set of backends.
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        backends: HashMap<SecretBackendKind, Arc<dyn SecretBackend>>,
    ) -> Self {
        Self { pool, backends }
    }

    /// Construct a broker with the default backend set.
    ///
    /// Every backend kind in the schema is registered, but only
    /// [`EnvironmentBackend`](crate::backends::EnvironmentBackend) can actually
    /// return material. `keychain`, `managed`, `vault` and `workload_identity`
    /// are registered in their **unconfigured** form and refuse with a typed
    /// code, because this build has no keychain client, no KEK, no Vault client
    /// and no signing seed.
    ///
    /// They are registered rather than omitted on purpose: a registered
    /// refuser answers `secrets.backend-not-configured`, which tells an operator
    /// the truth, whereas an omitted backend answers the vaguer
    /// `secrets.backend-unavailable`. Neither ever falls back to another source.
    ///
    /// Previous versions of this constructor installed a managed backend keyed
    /// with a hardcoded `[0x42; 32]` and a workload-identity backend seeded with
    /// a hardcoded `[0x24; 32]`. Both constructors now reject a repeated-constant
    /// key, so that cannot be reintroduced by accident. Use
    /// [`SecretBroker::with_backend`] to install a genuinely configured one.
    #[must_use]
    pub fn with_default_backends(pool: SqlitePool) -> Self {
        let mut backends: HashMap<SecretBackendKind, Arc<dyn SecretBackend>> = HashMap::new();
        backends.insert(
            SecretBackendKind::Environment,
            Arc::new(crate::backends::EnvironmentBackend::new()),
        );
        backends.insert(
            SecretBackendKind::Keychain,
            Arc::new(crate::backends::KeychainBackend::for_current_platform()),
        );
        backends.insert(
            SecretBackendKind::Managed,
            Arc::new(crate::backends::ManagedBackend::unconfigured()),
        );
        backends.insert(
            SecretBackendKind::Vault,
            Arc::new(crate::backends::VaultBackend::new()),
        );
        backends.insert(
            SecretBackendKind::WorkloadIdentity,
            Arc::new(crate::backends::WorkloadIdentityBackend::unconfigured()),
        );
        Self::new(pool, backends)
    }

    /// Register a backend implementation.
    pub fn with_backend(mut self, backend: Arc<dyn SecretBackend>) -> Self {
        self.backends.insert(backend.kind(), backend);
        self
    }

    /// Record a non-secret audit line.
    ///
    /// The outcome is an [`AuditOutcome`], not a `&str`. That is the whole
    /// point: `secret_audit.outcome_code` is specified as "A DOTTED CODE ONLY …
    /// Never a rendered message and never backend output", and this signature
    /// makes it impossible for any caller — including the daemon — to write a
    /// rendered `SecretError`, a Vault response body, or anything else that
    /// could carry secret-adjacent text into the ledger.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_audit(
        &self,
        reference_id: Option<&str>,
        lease_id: Option<&str>,
        event: AuditEvent,
        principal_uid: u32,
        job_id: Option<&str>,
        capability: Option<&str>,
        outcome: AuditOutcome,
        requested_name: Option<&str>,
    ) -> Result<(), SecretError> {
        let id = Uuid::now_v7().to_string();
        let occurred_at = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO secret_audit (id, reference_id, lease_id, event, principal_uid, job_id, capability, outcome_code, requested_name, occurred_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(reference_id)
        .bind(lease_id)
        .bind(event.as_str())
        .bind(i64::from(principal_uid))
        .bind(job_id)
        .bind(capability)
        .bind(outcome.as_str())
        .bind(requested_name)
        .bind(&occurred_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Register a new secret reference.
    #[allow(clippy::too_many_arguments)]
    pub async fn register_reference(
        &self,
        owner_uid: u32,
        name: &str,
        backend: SecretBackendKind,
        locator: &str,
        capability: &str,
        organization_id: Option<&str>,
        repository_id: Option<&str>,
    ) -> Result<SecretReference, SecretError> {
        let id = Uuid::now_v7().to_string();
        let accepted_digest = SecretReference::compute_digest(
            owner_uid,
            name,
            backend,
            locator,
            capability,
            organization_id,
            repository_id,
        );
        let created_at = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO secret_references (id, owner_uid, name, backend, locator, capability, organization_id, repository_id, accepted_digest, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(i64::from(owner_uid))
        .bind(name)
        .bind(backend.as_str())
        .bind(locator)
        .bind(capability)
        .bind(organization_id)
        .bind(repository_id)
        .bind(&accepted_digest)
        .bind(&created_at)
        .execute(&self.pool)
        .await?;

        let reference = SecretReference {
            id: id.clone(),
            owner_uid,
            name: name.to_string(),
            backend,
            locator: locator.to_string(),
            capability: capability.to_string(),
            organization_id: organization_id.map(ToString::to_string),
            repository_id: repository_id.map(ToString::to_string),
            accepted_digest,
            created_at,
            rotated_at: None,
            revoked_at: None,
            revoked_reason: None,
        };

        self.record_audit(
            Some(&id),
            None,
            AuditEvent::Issued,
            owner_uid,
            None,
            Some(capability),
            AuditOutcome::ReferenceCreated,
            Some(name),
        )
        .await?;

        Ok(reference)
    }

    /// Retrieve a secret reference by id, enforcing ownership and non-disclosure.
    /// Returns `SecretError::NotFound` for both unauthorized and missing references.
    pub async fn get_reference(
        &self,
        owner_uid: u32,
        id: &str,
    ) -> Result<SecretReference, SecretError> {
        let row = sqlx::query(
            "SELECT id, owner_uid, name, backend, locator, capability, organization_id, repository_id, accepted_digest, created_at, rotated_at, revoked_at, revoked_reason \
             FROM secret_references WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Err(SecretError::NotFound(id.to_string()));
        };

        let row_owner: i64 = row.get("owner_uid");
        if row_owner as u32 != owner_uid {
            return Err(SecretError::NotFound(id.to_string()));
        }

        let backend_str: String = row.get("backend");
        let backend = SecretBackendKind::parse_str(&backend_str)
            .ok_or_else(|| SecretError::InvalidData(format!("invalid backend: {backend_str}")))?;

        Ok(SecretReference {
            id: row.get("id"),
            owner_uid,
            name: row.get("name"),
            backend,
            locator: row.get("locator"),
            capability: row.get("capability"),
            organization_id: row.get("organization_id"),
            repository_id: row.get("repository_id"),
            accepted_digest: row.get("accepted_digest"),
            created_at: row.get("created_at"),
            rotated_at: row.get("rotated_at"),
            revoked_at: row.get("revoked_at"),
            revoked_reason: row.get("revoked_reason"),
        })
    }

    /// List all active secret references owned by a principal (metadata only).
    pub async fn list_references(
        &self,
        owner_uid: u32,
    ) -> Result<Vec<SecretReference>, SecretError> {
        let rows = sqlx::query(
            "SELECT id, owner_uid, name, backend, locator, capability, organization_id, repository_id, accepted_digest, created_at, rotated_at, revoked_at, revoked_reason \
             FROM secret_references WHERE owner_uid = ? AND revoked_at IS NULL ORDER BY name ASC"
        )
        .bind(i64::from(owner_uid))
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let backend_str: String = row.get("backend");
            let backend = SecretBackendKind::parse_str(&backend_str).ok_or_else(|| {
                SecretError::InvalidData(format!("invalid backend: {backend_str}"))
            })?;
            out.push(SecretReference {
                id: row.get("id"),
                owner_uid,
                name: row.get("name"),
                backend,
                locator: row.get("locator"),
                capability: row.get("capability"),
                organization_id: row.get("organization_id"),
                repository_id: row.get("repository_id"),
                accepted_digest: row.get("accepted_digest"),
                created_at: row.get("created_at"),
                rotated_at: row.get("rotated_at"),
                revoked_at: row.get("revoked_at"),
                revoked_reason: row.get("revoked_reason"),
            });
        }
        Ok(out)
    }

    /// Rotate a secret reference locator and backend.
    pub async fn rotate_reference(
        &self,
        owner_uid: u32,
        id: &str,
        new_locator: &str,
        new_backend: SecretBackendKind,
    ) -> Result<SecretReference, SecretError> {
        let reference = self.get_reference(owner_uid, id).await?;
        let new_digest = SecretReference::compute_digest(
            owner_uid,
            &reference.name,
            new_backend,
            new_locator,
            &reference.capability,
            reference.organization_id.as_deref(),
            reference.repository_id.as_deref(),
        );
        let rotated_at = Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE secret_references SET backend = ?, locator = ?, accepted_digest = ?, rotated_at = ? WHERE id = ?"
        )
        .bind(new_backend.as_str())
        .bind(new_locator)
        .bind(&new_digest)
        .bind(&rotated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;

        self.record_audit(
            Some(id),
            None,
            AuditEvent::Rotated,
            owner_uid,
            None,
            Some(&reference.capability),
            AuditOutcome::ReferenceRotated,
            Some(&reference.name),
        )
        .await?;

        self.get_reference(owner_uid, id).await
    }

    /// Revoke a secret reference and all its active leases.
    pub async fn revoke_reference(
        &self,
        owner_uid: u32,
        id: &str,
        reason: Option<&str>,
    ) -> Result<(), SecretError> {
        let reference = self.get_reference(owner_uid, id).await?;
        let revoked_at = Utc::now().to_rfc3339();

        sqlx::query("UPDATE secret_references SET revoked_at = ?, revoked_reason = ? WHERE id = ?")
            .bind(&revoked_at)
            .bind(reason)
            .bind(id)
            .execute(&self.pool)
            .await?;

        // Also revoke all active leases under this reference
        sqlx::query(
            "UPDATE secret_leases SET state = 'revoked', revoked_at = ?, revoked_reason = ? WHERE reference_id = ? AND state = 'active'"
        )
        .bind(&revoked_at)
        .bind(reason.unwrap_or("parent reference revoked"))
        .bind(id)
        .execute(&self.pool)
        .await?;

        self.record_audit(
            Some(id),
            None,
            AuditEvent::Revoked,
            owner_uid,
            None,
            Some(&reference.capability),
            AuditOutcome::ReferenceRevocationApplied,
            Some(&reference.name),
        )
        .await?;

        Ok(())
    }

    /// Issue a context-bound secret lease.
    pub async fn issue_lease(
        &self,
        reference_name: &str,
        context: &LeaseContext,
        ttl: Duration,
    ) -> Result<SecretLease, SecretError> {
        let row = sqlx::query(
            "SELECT id, owner_uid, name, backend, locator, capability, organization_id, repository_id, accepted_digest, created_at, rotated_at, revoked_at, revoked_reason \
             FROM secret_references WHERE owner_uid = ? AND name = ? AND capability = ?"
        )
        .bind(i64::from(context.principal_uid))
        .bind(reference_name)
        .bind(&context.capability)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            self.record_audit(
                None,
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::UnknownReference,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::NotFound(reference_name.to_string()));
        };

        let reference_id: String = row.get("id");
        let reference_capability: String = row.get("capability");
        let reference_org: Option<String> = row.get("organization_id");
        let reference_repo: Option<String> = row.get("repository_id");
        let revoked_at: Option<String> = row.get("revoked_at");

        if revoked_at.is_some() {
            self.record_audit(
                Some(&reference_id),
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ReferenceRevoked,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::Revoked("secret reference is revoked"));
        }

        let backend_str: String = row.get("backend");
        let backend = SecretBackendKind::parse_str(&backend_str)
            .ok_or_else(|| SecretError::InvalidData(format!("invalid backend: {backend_str}")))?;

        let accepted_digest: String = row.get("accepted_digest");
        let locator: String = row.get("locator");
        let computed = SecretReference::compute_digest(
            context.principal_uid,
            reference_name,
            backend,
            &locator,
            &reference_capability,
            reference_org.as_deref(),
            reference_repo.as_deref(),
        );

        if computed != accepted_digest {
            self.record_audit(
                Some(&reference_id),
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::DigestMismatch,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::DigestMismatch);
        }

        // Check context narrowing
        if reference_capability != context.capability {
            self.record_audit(
                Some(&reference_id),
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ScopeMismatch,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::ScopeMismatch("capability mismatch"));
        }

        if reference_org.is_some() && reference_org != context.organization_id {
            self.record_audit(
                Some(&reference_id),
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ScopeMismatch,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::ScopeMismatch("organization scope mismatch"));
        }

        if reference_repo.is_some() && reference_repo != context.repository_id {
            self.record_audit(
                Some(&reference_id),
                None,
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ScopeMismatch,
                Some(reference_name),
            )
            .await?;
            return Err(SecretError::ScopeMismatch("repository scope mismatch"));
        }

        let issue_key = context.issue_key(&reference_id);
        let now = Utc::now();

        // Idempotency: return existing active lease if already issued for this exact key
        let existing = sqlx::query(
            "SELECT id, reference_id, principal_uid, organization_id, repository_id, job_id, capability, issue_key, issued_at, expires_at, backend_lease_handle, state, revoked_at, revoked_reason \
             FROM secret_leases WHERE issue_key = ?"
        )
        .bind(&issue_key)
        .fetch_optional(&self.pool)
        .await?;

        let issued_at = now.to_rfc3339();
        let expires_at = (now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(300)))
        .to_rfc3339();

        let lease_id = if let Some(row) = existing {
            let existing_id: String = row.get("id");
            let state_str: String = row.get("state");
            let state = LeaseState::parse_str(&state_str).unwrap_or(LeaseState::Failed);
            let revoked_at: Option<String> = row.get("revoked_at");
            let expires_at_str: String = row.get("expires_at");

            if state == LeaseState::Active && revoked_at.is_none() {
                if let Ok(exp) = expires_at_str.parse::<DateTime<Utc>>() {
                    if now < exp {
                        return Ok(SecretLease {
                            id: existing_id,
                            reference_id: row.get("reference_id"),
                            principal_uid: context.principal_uid,
                            organization_id: row.get("organization_id"),
                            repository_id: row.get("repository_id"),
                            job_id: row.get("job_id"),
                            capability: row.get("capability"),
                            issue_key,
                            issued_at: row.get("issued_at"),
                            expires_at: expires_at_str,
                            backend_lease_handle: row.get("backend_lease_handle"),
                            state: LeaseState::Active,
                            revoked_at: None,
                            revoked_reason: None,
                        });
                    }
                }
            }

            // Reactivate/renew existing lease row keeping its primary key id
            sqlx::query(
                "UPDATE secret_leases SET reference_id = ?, state = 'active', issued_at = ?, expires_at = ?, revoked_at = NULL, revoked_reason = NULL WHERE id = ?"
            )
            .bind(&reference_id)
            .bind(&issued_at)
            .bind(&expires_at)
            .bind(&existing_id)
            .execute(&self.pool)
            .await?;

            existing_id
        } else {
            let new_id = Uuid::now_v7().to_string();
            sqlx::query(
                "INSERT INTO secret_leases (id, reference_id, principal_uid, organization_id, repository_id, job_id, capability, issue_key, issued_at, expires_at, state) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&new_id)
            .bind(&reference_id)
            .bind(i64::from(context.principal_uid))
            .bind(&context.organization_id)
            .bind(&context.repository_id)
            .bind(&context.job_id)
            .bind(&context.capability)
            .bind(&issue_key)
            .bind(&issued_at)
            .bind(&expires_at)
            .bind(LeaseState::Active.as_str())
            .execute(&self.pool)
            .await?;

            new_id
        };

        self.record_audit(
            Some(&reference_id),
            Some(&lease_id),
            AuditEvent::Issued,
            context.principal_uid,
            Some(&context.job_id),
            Some(&context.capability),
            AuditOutcome::Issued,
            Some(reference_name),
        )
        .await?;

        Ok(SecretLease {
            id: lease_id,
            reference_id,
            principal_uid: context.principal_uid,
            organization_id: context.organization_id.clone(),
            repository_id: context.repository_id.clone(),
            job_id: context.job_id.clone(),
            capability: context.capability.clone(),
            issue_key,
            issued_at,
            expires_at,
            backend_lease_handle: None,
            state: LeaseState::Active,
            revoked_at: None,
            revoked_reason: None,
        })
    }

    /// Resolve credential material for a lease at the final transport injection point.
    pub async fn resolve_lease(
        &self,
        lease_id: &str,
        context: &LeaseContext,
    ) -> Result<LeasedSecret, SecretError> {
        let row = sqlx::query(
            "SELECT id, reference_id, principal_uid, organization_id, repository_id, job_id, capability, issue_key, issued_at, expires_at, backend_lease_handle, state, revoked_at, revoked_reason \
             FROM secret_leases WHERE id = ?"
        )
        .bind(lease_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            self.record_audit(
                None,
                Some(lease_id),
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::LeaseNotFound,
                None,
            )
            .await?;
            return Err(SecretError::NotFound(lease_id.to_string()));
        };

        let principal_uid: i64 = row.get("principal_uid");
        let lease_job_id: String = row.get("job_id");
        let lease_capability: String = row.get("capability");
        let reference_id: String = row.get("reference_id");

        if principal_uid as u32 != context.principal_uid
            || lease_job_id != context.job_id
            || lease_capability != context.capability
        {
            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ScopeMismatch,
                None,
            )
            .await?;
            return Err(SecretError::ScopeMismatch("lease context mismatch"));
        }

        let state_str: String = row.get("state");
        let state = LeaseState::parse_str(&state_str).unwrap_or(LeaseState::Failed);
        let revoked_at: Option<String> = row.get("revoked_at");

        if state == LeaseState::Revoked || revoked_at.is_some() {
            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Revoked,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::LeaseRevoked,
                None,
            )
            .await?;
            return Err(SecretError::Revoked("lease is revoked"));
        }

        let now = Utc::now();
        let expires_at_str: String = row.get("expires_at");
        let expired = match expires_at_str.parse::<DateTime<Utc>>() {
            Ok(exp) => now >= exp,
            Err(_) => true,
        };

        if expired || state == LeaseState::Expired {
            let _ = sqlx::query("UPDATE secret_leases SET state = 'expired' WHERE id = ?")
                .bind(lease_id)
                .execute(&self.pool)
                .await;

            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Expired,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::LeaseExpired,
                None,
            )
            .await?;
            return Err(SecretError::Expired);
        }

        // Fetch reference
        let ref_row = sqlx::query(
            "SELECT id, owner_uid, name, backend, locator, capability, organization_id, repository_id, accepted_digest, created_at, rotated_at, revoked_at, revoked_reason \
             FROM secret_references WHERE id = ?"
        )
        .bind(&reference_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(ref_row) = ref_row else {
            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ReferenceNotFound,
                None,
            )
            .await?;
            return Err(SecretError::NotFound(reference_id));
        };

        let ref_revoked: Option<String> = ref_row.get("revoked_at");
        if ref_revoked.is_some() {
            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::ReferenceRevoked,
                None,
            )
            .await?;
            return Err(SecretError::Revoked("reference is revoked"));
        }

        let ref_backend_str: String = ref_row.get("backend");
        let ref_backend = SecretBackendKind::parse_str(&ref_backend_str).ok_or_else(|| {
            SecretError::InvalidData(format!("invalid backend: {ref_backend_str}"))
        })?;

        let locator: String = ref_row.get("locator");
        let ref_name: String = ref_row.get("name");
        let ref_cap: String = ref_row.get("capability");
        let ref_org: Option<String> = ref_row.get("organization_id");
        let ref_repo: Option<String> = ref_row.get("repository_id");
        let accepted_digest: String = ref_row.get("accepted_digest");

        let computed = SecretReference::compute_digest(
            context.principal_uid,
            &ref_name,
            ref_backend,
            &locator,
            &ref_cap,
            ref_org.as_deref(),
            ref_repo.as_deref(),
        );

        if computed != accepted_digest {
            self.record_audit(
                Some(&reference_id),
                Some(lease_id),
                AuditEvent::Denied,
                context.principal_uid,
                Some(&context.job_id),
                Some(&context.capability),
                AuditOutcome::DigestMismatch,
                Some(&ref_name),
            )
            .await?;
            return Err(SecretError::DigestMismatch);
        }

        let backend = self.backends.get(&ref_backend).ok_or_else(|| {
            SecretError::backend(
                BackendErrorCode::Unavailable,
                "the backend named by this reference is not registered in this broker",
            )
        });

        let backend = match backend {
            Ok(b) => b,
            Err(e) => {
                self.record_audit(
                    Some(&reference_id),
                    Some(lease_id),
                    AuditEvent::BackendError,
                    context.principal_uid,
                    Some(&context.job_id),
                    Some(&context.capability),
                    AuditOutcome::BackendUnavailable,
                    Some(&ref_name),
                )
                .await?;
                return Err(e);
            }
        };

        match backend.resolve(&locator, context).await {
            Ok(secret) => {
                self.record_audit(
                    Some(&reference_id),
                    Some(lease_id),
                    AuditEvent::Used,
                    context.principal_uid,
                    Some(&context.job_id),
                    Some(&context.capability),
                    AuditOutcome::Used,
                    Some(&ref_name),
                )
                .await?;
                Ok(secret)
            }
            Err(err) => {
                self.record_audit(
                    Some(&reference_id),
                    Some(lease_id),
                    AuditEvent::BackendError,
                    context.principal_uid,
                    Some(&context.job_id),
                    Some(&context.capability),
                    err.audit_outcome(),
                    Some(&ref_name),
                )
                .await?;
                Err(err)
            }
        }
    }

    /// Direct resolve convenience method: issues a short-lived lease and immediately resolves the secret.
    pub async fn resolve_secret(
        &self,
        name: &str,
        context: &LeaseContext,
        ttl: Duration,
    ) -> Result<LeasedSecret, SecretError> {
        let lease = self.issue_lease(name, context, ttl).await?;
        self.resolve_lease(&lease.id, context).await
    }

    /// Revoke a secret lease.
    pub async fn revoke_lease(
        &self,
        lease_id: &str,
        reason: Option<&str>,
    ) -> Result<(), SecretError> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            "SELECT reference_id, principal_uid, job_id, capability, backend_lease_handle \
             FROM secret_leases WHERE id = ?",
        )
        .bind(lease_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Err(SecretError::NotFound(lease_id.to_string()));
        };

        let reference_id: String = row.get("reference_id");
        let principal_uid: i64 = row.get("principal_uid");
        let job_id: String = row.get("job_id");
        let capability: String = row.get("capability");
        let handle: Option<String> = row.get("backend_lease_handle");

        sqlx::query(
            "UPDATE secret_leases SET state = 'revoked', revoked_at = ?, revoked_reason = ? WHERE id = ?"
        )
        .bind(&now)
        .bind(reason)
        .bind(lease_id)
        .execute(&self.pool)
        .await?;

        // The local row is already revoked above, so a backend that cannot
        // revoke remotely does not resurrect the lease here. But it does mean
        // material may still be live at the backend, and that must leave a
        // trace rather than being swallowed by `let _ =`.
        if let Some(handle) = handle {
            if let Ok(ref_row) = sqlx::query("SELECT backend FROM secret_references WHERE id = ?")
                .bind(&reference_id)
                .fetch_one(&self.pool)
                .await
            {
                let backend_str: String = ref_row.get("backend");
                if let Some(backend_kind) = SecretBackendKind::parse_str(&backend_str) {
                    if let Some(backend) = self.backends.get(&backend_kind) {
                        if let Err(err) = backend.revoke(&handle).await {
                            self.record_audit(
                                Some(&reference_id),
                                Some(lease_id),
                                AuditEvent::BackendError,
                                principal_uid as u32,
                                Some(&job_id),
                                Some(&capability),
                                err.audit_outcome(),
                                None,
                            )
                            .await?;
                        }
                    }
                }
            }
        }

        self.record_audit(
            Some(&reference_id),
            Some(lease_id),
            AuditEvent::Revoked,
            principal_uid as u32,
            Some(&job_id),
            Some(&capability),
            AuditOutcome::LeaseRevoked,
            None,
        )
        .await?;

        Ok(())
    }

    /// Fetch audit records.
    pub async fn get_audit_records(
        &self,
        limit: u32,
    ) -> Result<Vec<SecretAuditRecord>, SecretError> {
        let rows = sqlx::query(
            "SELECT id, reference_id, lease_id, event, principal_uid, job_id, capability, outcome_code, requested_name, occurred_at \
             FROM secret_audit ORDER BY occurred_at DESC LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let event_str: String = row.get("event");
            let event = AuditEvent::parse_str(&event_str).unwrap_or(AuditEvent::Denied);
            let principal_uid: i64 = row.get("principal_uid");
            out.push(SecretAuditRecord {
                id: row.get("id"),
                reference_id: row.get("reference_id"),
                lease_id: row.get("lease_id"),
                event,
                principal_uid: principal_uid as u32,
                job_id: row.get("job_id"),
                capability: row.get("capability"),
                outcome_code: row.get("outcome_code"),
                requested_name: row.get("requested_name"),
                occurred_at: row.get("occurred_at"),
            });
        }
        Ok(out)
    }
}

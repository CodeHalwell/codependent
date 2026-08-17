use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    audit::{compute_record_hash, AuditRecord},
    error::{identity_link_refused, ControlPlaneError, SQLSTATE_UNIQUE_VIOLATION},
    store::*,
};

/// Advisory-lock namespace for audit chain appends. Arbitrary but fixed, so the
/// audit lock can never collide with an advisory lock taken for another purpose.
const AUDIT_CHAIN_LOCK_NAMESPACE: i32 = 0x4155_4449_u32 as i32; // "AUDI"

/// Second half of the advisory lock key: a stable 32-bit digest of the
/// organization id, so appends serialize per organization rather than globally.
///
/// A digest collision between two organizations costs contention, never
/// correctness: two unrelated chains would take turns instead of running in
/// parallel.
fn audit_chain_lock_key(org_id: Uuid) -> i32 {
    let digest = Sha256::digest(org_id.as_bytes());
    i32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
}

/// `SELECT ... ORDER BY` fragment shared by the audit reads. See the ordering
/// contract on [`Store::append_audit_record`] — the `id` tiebreaker is what
/// makes the order total.
const AUDIT_SELECT_COLUMNS: &str = "id, organization_id, actor_kind, actor_id, action, target_kind, target_id, action_digest, correlation_id, prev_hash, record_hash, detail, occurred_at";

fn audit_record_from_row(r: &sqlx::postgres::PgRow) -> AuditRecord {
    AuditRecord {
        id: r.get(0),
        organization_id: r.get(1),
        actor_kind: r.get(2),
        actor_id: r.get(3),
        action: r.get(4),
        target_kind: r.get(5),
        target_id: r.get(6),
        action_digest: r.get(7),
        correlation_id: r.get(8),
        prev_hash: r.get(9),
        record_hash: r.get(10),
        detail: r.get(11),
        occurred_at: r.get(12),
    }
}

/// Read the tail of an organization's audit chain using any executor, so the
/// same query serves both the public accessor and the read inside the append
/// transaction.
async fn fetch_latest_audit_record<'e, E>(
    executor: E,
    org_id: Uuid,
) -> Result<Option<AuditRecord>, ControlPlaneError>
where
    E: sqlx::PgExecutor<'e>,
{
    let sql = format!(
        "SELECT {AUDIT_SELECT_COLUMNS} FROM audit_records WHERE organization_id = $1 ORDER BY occurred_at DESC, id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(org_id)
        .fetch_optional(executor)
        .await?;

    Ok(row.as_ref().map(audit_record_from_row))
}

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn run_migrations(&self) -> Result<(), ControlPlaneError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| ControlPlaneError::Database(format!("Migration failed: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl Store for PgStore {
    async fn is_ready(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    async fn create_user(&self, user: User) -> Result<User, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO users (id, display_name, primary_email, state, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(user.id)
        .bind(&user.display_name)
        .bind(&user.primary_email)
        .bind(&user.state)
        .bind(user.created_at)
        .bind(user.updated_at)
        .execute(&self.pool)
        .await?;

        Ok(user)
    }

    async fn get_user(&self, id: Uuid) -> Result<Option<User>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, display_name, primary_email, state, created_at, updated_at
            FROM users WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| User {
            id: r.get(0),
            display_name: r.get(1),
            primary_email: r.get(2),
            state: r.get(3),
            created_at: r.get(4),
            updated_at: r.get(5),
        }))
    }

    async fn create_user_identity(
        &self,
        identity: UserIdentity,
    ) -> Result<UserIdentity, ControlPlaneError> {
        let result = sqlx::query(
            r#"
            INSERT INTO user_identities (id, user_id, provider, issuer, subject, email_at_link, linked_at, link_audit_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(identity.id)
        .bind(identity.user_id)
        .bind(&identity.provider)
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .bind(&identity.email_at_link)
        .bind(identity.linked_at)
        .bind(identity.link_audit_id)
        .execute(&self.pool)
        .await;

        if let Err(sqlx::Error::Database(ref db_err)) = result {
            // Losing the race to claim `(provider, issuer, subject)` proves that
            // another user already linked that identity. Surfacing it as a
            // conflict makes this endpoint an existence oracle for third-party
            // accounts, so it collapses to the same refusal an unauthorized or
            // absent link receives. Matched here as well as in the generic
            // `From<sqlx::Error>` so the collapse does not depend on the driver
            // reporting a table name.
            if db_err.code().as_deref() == Some(SQLSTATE_UNIQUE_VIOLATION) {
                return Err(identity_link_refused());
            }
        }
        result?;

        Ok(identity)
    }

    async fn find_user_identity(
        &self,
        provider: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<UserIdentity>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, provider, issuer, subject, email_at_link, linked_at, link_audit_id
            FROM user_identities
            WHERE provider = $1 AND issuer = $2 AND subject = $3
            "#,
        )
        .bind(provider)
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserIdentity {
            id: r.get(0),
            user_id: r.get(1),
            provider: r.get(2),
            issuer: r.get(3),
            subject: r.get(4),
            email_at_link: r.get(5),
            linked_at: r.get(6),
            link_audit_id: r.get(7),
        }))
    }

    async fn save_refresh_token(&self, token: UserRefreshToken) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO user_refresh_tokens (id, user_id, token_hash, rotated_from, issued_at, expires_at, revoked_at, user_agent_digest)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(token.id)
        .bind(token.user_id)
        .bind(&token.token_hash)
        .bind(token.rotated_from)
        .bind(token.issued_at)
        .bind(token.expires_at)
        .bind(token.revoked_at)
        .bind(&token.user_agent_digest)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn lookup_refresh_token(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<UserRefreshToken>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, token_hash, rotated_from, issued_at, expires_at, revoked_at, user_agent_digest
            FROM user_refresh_tokens
            WHERE token_hash = $1
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserRefreshToken {
            id: r.get(0),
            user_id: r.get(1),
            token_hash: r.get(2),
            rotated_from: r.get(3),
            issued_at: r.get(4),
            expires_at: r.get(5),
            revoked_at: r.get(6),
            user_agent_digest: r.get(7),
        }))
    }

    async fn revoke_refresh_token(&self, id: Uuid) -> Result<(), ControlPlaneError> {
        sqlx::query("UPDATE user_refresh_tokens SET revoked_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn revoke_refresh_token_chain(&self, token_hash: &[u8]) -> Result<(), ControlPlaneError> {
        // Find user_id and revoke all active tokens for that user
        sqlx::query(
            r#"
            UPDATE user_refresh_tokens
            SET revoked_at = now()
            WHERE user_id = (SELECT user_id FROM user_refresh_tokens WHERE token_hash = $1)
            "#,
        )
        .bind(token_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_organization(
        &self,
        org: Organization,
    ) -> Result<Organization, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO organizations (id, slug, display_name, max_publication_class, max_classification, data_residency, retention_days, policy_version, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(org.id)
        .bind(&org.slug)
        .bind(&org.display_name)
        .bind(&org.max_publication_class)
        .bind(&org.max_classification)
        .bind(&org.data_residency)
        .bind(org.retention_days)
        .bind(org.policy_version)
        .bind(org.created_at)
        .execute(&self.pool)
        .await?;

        Ok(org)
    }

    async fn get_organization(&self, id: Uuid) -> Result<Option<Organization>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, display_name, max_publication_class, max_classification, data_residency, retention_days, policy_version, created_at
            FROM organizations WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Organization {
            id: r.get(0),
            slug: r.get(1),
            display_name: r.get(2),
            max_publication_class: r.get(3),
            max_classification: r.get(4),
            data_residency: r.get(5),
            retention_days: r.get(6),
            policy_version: r.get(7),
            created_at: r.get(8),
        }))
    }

    async fn get_organization_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<Organization>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, display_name, max_publication_class, max_classification, data_residency, retention_days, policy_version, created_at
            FROM organizations WHERE lower(slug) = lower($1)
            "#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Organization {
            id: r.get(0),
            slug: r.get(1),
            display_name: r.get(2),
            max_publication_class: r.get(3),
            max_classification: r.get(4),
            data_residency: r.get(5),
            retention_days: r.get(6),
            policy_version: r.get(7),
            created_at: r.get(8),
        }))
    }

    async fn list_user_organizations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<Organization>, ControlPlaneError> {
        let rows = sqlx::query(
            r#"
            SELECT o.id, o.slug, o.display_name, o.max_publication_class, o.max_classification, o.data_residency, o.retention_days, o.policy_version, o.created_at
            FROM organizations o
            JOIN memberships m ON m.organization_id = o.id
            WHERE m.user_id = $1 AND m.state = 'active'
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| Organization {
                id: r.get(0),
                slug: r.get(1),
                display_name: r.get(2),
                max_publication_class: r.get(3),
                max_classification: r.get(4),
                data_residency: r.get(5),
                retention_days: r.get(6),
                policy_version: r.get(7),
                created_at: r.get(8),
            })
            .collect())
    }

    async fn add_membership(&self, membership: Membership) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO memberships (organization_id, user_id, state, joined_at, created_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (organization_id, user_id) DO UPDATE SET state = EXCLUDED.state, joined_at = EXCLUDED.joined_at
            "#,
        )
        .bind(membership.organization_id)
        .bind(membership.user_id)
        .bind(&membership.state)
        .bind(membership.joined_at)
        .bind(membership.created_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn create_role_grant(&self, grant: RoleGrant) -> Result<RoleGrant, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO role_grants (id, organization_id, user_id, team_id, repository_id, role, action_scope, granted_by, granted_at, expires_at, revoked_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(grant.id)
        .bind(grant.organization_id)
        .bind(grant.user_id)
        .bind(grant.team_id)
        .bind(grant.repository_id)
        .bind(&grant.role)
        .bind(&grant.action_scope)
        .bind(grant.granted_by)
        .bind(grant.granted_at)
        .bind(grant.expires_at)
        .bind(grant.revoked_at)
        .execute(&self.pool)
        .await?;

        Ok(grant)
    }

    async fn list_user_grants(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<RoleGrant>, ControlPlaneError> {
        let rows = sqlx::query(
            r#"
            SELECT g.id, g.organization_id, g.user_id, g.team_id, g.repository_id, g.role, g.action_scope, g.granted_by, g.granted_at, g.expires_at, g.revoked_at
            FROM role_grants g
            LEFT JOIN team_members tm ON tm.team_id = g.team_id
            WHERE g.organization_id = $1
              AND (g.user_id = $2 OR tm.user_id = $2)
              AND g.revoked_at IS NULL
              AND (g.expires_at IS NULL OR g.expires_at > now())
            "#,
        )
        .bind(org_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| RoleGrant {
                id: r.get(0),
                organization_id: r.get(1),
                user_id: r.get(2),
                team_id: r.get(3),
                repository_id: r.get(4),
                role: r.get(5),
                action_scope: r.get(6),
                granted_by: r.get(7),
                granted_at: r.get(8),
                expires_at: r.get(9),
                revoked_at: r.get(10),
            })
            .collect())
    }

    async fn create_repository(&self, repo: Repository) -> Result<Repository, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO repositories (id, organization_id, federated_id, display_name, max_publication_class, max_classification, policy_version, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(repo.id)
        .bind(repo.organization_id)
        .bind(&repo.federated_id)
        .bind(&repo.display_name)
        .bind(&repo.max_publication_class)
        .bind(&repo.max_classification)
        .bind(repo.policy_version)
        .bind(repo.created_at)
        .execute(&self.pool)
        .await?;

        Ok(repo)
    }

    async fn get_repository(&self, id: Uuid) -> Result<Option<Repository>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, organization_id, federated_id, display_name, max_publication_class, max_classification, policy_version, created_at
            FROM repositories WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Repository {
            id: r.get(0),
            organization_id: r.get(1),
            federated_id: r.get(2),
            display_name: r.get(3),
            max_publication_class: r.get(4),
            max_classification: r.get(5),
            policy_version: r.get(6),
            created_at: r.get(7),
        }))
    }

    async fn get_repository_in_org(
        &self,
        org_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Option<Repository>, ControlPlaneError> {
        // The organization is part of the WHERE clause, not a post-hoc `.filter`
        // in Rust: a repository in another tenant is not fetched at all.
        let row = sqlx::query(
            r#"
            SELECT id, organization_id, federated_id, display_name, max_publication_class, max_classification, policy_version, created_at
            FROM repositories WHERE organization_id = $1 AND id = $2
            "#,
        )
        .bind(org_id)
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Repository {
            id: r.get(0),
            organization_id: r.get(1),
            federated_id: r.get(2),
            display_name: r.get(3),
            max_publication_class: r.get(4),
            max_classification: r.get(5),
            policy_version: r.get(6),
            created_at: r.get(7),
        }))
    }

    async fn find_repository_by_federated_id(
        &self,
        org_id: Uuid,
        federated_id: &str,
    ) -> Result<Option<Repository>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, organization_id, federated_id, display_name, max_publication_class, max_classification, policy_version, created_at
            FROM repositories WHERE organization_id = $1 AND federated_id = $2
            "#,
        )
        .bind(org_id)
        .bind(federated_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Repository {
            id: r.get(0),
            organization_id: r.get(1),
            federated_id: r.get(2),
            display_name: r.get(3),
            max_publication_class: r.get(4),
            max_classification: r.get(5),
            policy_version: r.get(6),
            created_at: r.get(7),
        }))
    }

    async fn list_authorized_repositories(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Repository>, ControlPlaneError> {
        // Design §5.1: Join to authorized scope CTE directly in the query
        let rows = sqlx::query(
            r#"
            WITH authorized_repositories AS (
                SELECT r.id
                FROM repositories r
                JOIN role_grants g ON g.organization_id = r.organization_id
                                  AND (g.repository_id IS NULL OR g.repository_id = r.id)
                LEFT JOIN team_members tm ON tm.team_id = g.team_id
                WHERE r.organization_id = $1
                  AND g.revoked_at IS NULL
                  AND (g.expires_at IS NULL OR g.expires_at > now())
                  AND (g.user_id = $2 OR tm.user_id = $2)
            )
            SELECT r.id, r.organization_id, r.federated_id, r.display_name, r.max_publication_class, r.max_classification, r.policy_version, r.created_at
            FROM repositories r
            JOIN authorized_repositories a ON a.id = r.id
            "#,
        )
        .bind(org_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| Repository {
                id: r.get(0),
                organization_id: r.get(1),
                federated_id: r.get(2),
                display_name: r.get(3),
                max_publication_class: r.get(4),
                max_classification: r.get(5),
                policy_version: r.get(6),
                created_at: r.get(7),
            })
            .collect())
    }

    async fn create_pairing_challenge(
        &self,
        challenge: PairingChallenge,
    ) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO pairing_challenges (code_hash, organization_id, initiated_by, requested_scope, created_at, expires_at, consumed_at, daemon_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&challenge.code_hash)
        .bind(challenge.organization_id)
        .bind(challenge.initiated_by)
        .bind(&challenge.requested_scope)
        .bind(challenge.created_at)
        .bind(challenge.expires_at)
        .bind(challenge.consumed_at)
        .bind(challenge.daemon_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn consume_pairing_challenge(
        &self,
        code_hash: &[u8],
        daemon_id: Uuid,
    ) -> Result<Option<PairingChallenge>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            UPDATE pairing_challenges
            SET consumed_at = now(), daemon_id = $2
            WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now()
            RETURNING code_hash, organization_id, initiated_by, requested_scope, created_at, expires_at, consumed_at, daemon_id
            "#,
        )
        .bind(code_hash)
        .bind(daemon_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| PairingChallenge {
            code_hash: r.get(0),
            organization_id: r.get(1),
            initiated_by: r.get(2),
            requested_scope: r.get(3),
            created_at: r.get(4),
            expires_at: r.get(5),
            consumed_at: r.get(6),
            daemon_id: r.get(7),
        }))
    }

    async fn register_daemon(&self, daemon: Daemon) -> Result<Daemon, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO daemons (id, organization_id, paired_by, display_name, consent_manifest_hash, max_publication_class, accepts_remote_approvals, accepts_runner_dispatch, state, paired_at, revoked_at, last_seen_at, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(daemon.id)
        .bind(daemon.organization_id)
        .bind(daemon.paired_by)
        .bind(&daemon.display_name)
        .bind(&daemon.consent_manifest_hash)
        .bind(&daemon.max_publication_class)
        .bind(daemon.accepts_remote_approvals)
        .bind(daemon.accepts_runner_dispatch)
        .bind(&daemon.state)
        .bind(daemon.paired_at)
        .bind(daemon.revoked_at)
        .bind(daemon.last_seen_at)
        .bind(daemon.created_at)
        .execute(&self.pool)
        .await?;

        Ok(daemon)
    }

    async fn get_daemon(&self, daemon_id: Uuid) -> Result<Option<Daemon>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, organization_id, paired_by, display_name, consent_manifest_hash, max_publication_class, accepts_remote_approvals, accepts_runner_dispatch, state, paired_at, revoked_at, last_seen_at, created_at
            FROM daemons WHERE id = $1
            "#,
        )
        .bind(daemon_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Daemon {
            id: r.get(0),
            organization_id: r.get(1),
            paired_by: r.get(2),
            display_name: r.get(3),
            consent_manifest_hash: r.get(4),
            max_publication_class: r.get(5),
            accepts_remote_approvals: r.get(6),
            accepts_runner_dispatch: r.get(7),
            state: r.get(8),
            paired_at: r.get(9),
            revoked_at: r.get(10),
            last_seen_at: r.get(11),
            created_at: r.get(12),
        }))
    }

    async fn update_daemon_state(
        &self,
        daemon_id: Uuid,
        state: &str,
    ) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            UPDATE daemons
            SET state = $2, revoked_at = CASE WHEN $2 = 'revoked' THEN now() ELSE revoked_at END
            WHERE id = $1
            "#,
        )
        .bind(daemon_id)
        .bind(state)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn save_workload_credential(
        &self,
        cred: WorkloadCredential,
    ) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO workload_credentials (id, daemon_id, audience, purpose, token_hash, rotated_from, issued_at, expires_at, revoked_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(cred.id)
        .bind(cred.daemon_id)
        .bind(&cred.audience)
        .bind(&cred.purpose)
        .bind(&cred.token_hash)
        .bind(cred.rotated_from)
        .bind(cred.issued_at)
        .bind(cred.expires_at)
        .bind(cred.revoked_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn lookup_workload_credential(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<WorkloadCredential>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, daemon_id, audience, purpose, token_hash, rotated_from, issued_at, expires_at, revoked_at
            FROM workload_credentials
            WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| WorkloadCredential {
            id: r.get(0),
            daemon_id: r.get(1),
            audience: r.get(2),
            purpose: r.get(3),
            token_hash: r.get(4),
            rotated_from: r.get(5),
            issued_at: r.get(6),
            expires_at: r.get(7),
            revoked_at: r.get(8),
        }))
    }

    async fn upsert_shared_session(
        &self,
        session: SharedSession,
    ) -> Result<SharedSession, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            INSERT INTO shared_sessions (id, organization_id, repository_id, daemon_id, remote_session_key, class, title, state, started_at, last_activity_at, tombstoned_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (daemon_id, remote_session_key) DO UPDATE SET
                class = EXCLUDED.class,
                title = EXCLUDED.title,
                state = EXCLUDED.state,
                last_activity_at = EXCLUDED.last_activity_at,
                updated_at = now()
            RETURNING id, organization_id, repository_id, daemon_id, remote_session_key, class, title, state, started_at, last_activity_at, tombstoned_at, updated_at
            "#,
        )
        .bind(session.id)
        .bind(session.organization_id)
        .bind(session.repository_id)
        .bind(session.daemon_id)
        .bind(&session.remote_session_key)
        .bind(&session.class)
        .bind(&session.title)
        .bind(&session.state)
        .bind(session.started_at)
        .bind(session.last_activity_at)
        .bind(session.tombstoned_at)
        .bind(session.updated_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(SharedSession {
            id: row.get(0),
            organization_id: row.get(1),
            repository_id: row.get(2),
            daemon_id: row.get(3),
            remote_session_key: row.get(4),
            class: row.get(5),
            title: row.get(6),
            state: row.get(7),
            started_at: row.get(8),
            last_activity_at: row.get(9),
            tombstoned_at: row.get(10),
            updated_at: row.get(11),
        })
    }

    async fn list_shared_sessions(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SharedSession>, ControlPlaneError> {
        let rows = if let Some(rid) = repo_id {
            sqlx::query(
                r#"
                SELECT id, organization_id, repository_id, daemon_id, remote_session_key, class, title, state, started_at, last_activity_at, tombstoned_at, updated_at
                FROM shared_sessions
                WHERE organization_id = $1 AND repository_id = $2 AND tombstoned_at IS NULL
                ORDER BY last_activity_at DESC NULLS LAST, started_at DESC
                LIMIT $3
                "#,
            )
            .bind(org_id)
            .bind(rid)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, organization_id, repository_id, daemon_id, remote_session_key, class, title, state, started_at, last_activity_at, tombstoned_at, updated_at
                FROM shared_sessions
                WHERE organization_id = $1 AND tombstoned_at IS NULL
                ORDER BY last_activity_at DESC NULLS LAST, started_at DESC
                LIMIT $2
                "#,
            )
            .bind(org_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|r| SharedSession {
                id: r.get(0),
                organization_id: r.get(1),
                repository_id: r.get(2),
                daemon_id: r.get(3),
                remote_session_key: r.get(4),
                class: r.get(5),
                title: r.get(6),
                state: r.get(7),
                started_at: r.get(8),
                last_activity_at: r.get(9),
                tombstoned_at: r.get(10),
                updated_at: r.get(11),
            })
            .collect())
    }

    async fn record_sync_receipt(&self, receipt: SyncReceipt) -> Result<bool, ControlPlaneError> {
        let res = sqlx::query(
            r#"
            INSERT INTO sync_receipts (id, daemon_id, daemon_sequence, delta_kind, payload_hash, class, accepted_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (daemon_id, daemon_sequence) DO NOTHING
            "#,
        )
        .bind(receipt.id)
        .bind(receipt.daemon_id)
        .bind(receipt.daemon_sequence)
        .bind(&receipt.delta_kind)
        .bind(&receipt.payload_hash)
        .bind(&receipt.class)
        .bind(receipt.accepted_at)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    async fn create_tombstone(&self, tombstone: Tombstone) -> Result<(), ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO tombstones (id, organization_id, subject_kind, subject_key, reason, created_at, applied_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (organization_id, subject_kind, subject_key, created_at) DO NOTHING
            "#,
        )
        .bind(tombstone.id)
        .bind(tombstone.organization_id)
        .bind(&tombstone.subject_kind)
        .bind(&tombstone.subject_key)
        .bind(&tombstone.reason)
        .bind(tombstone.created_at)
        .bind(tombstone.applied_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn list_tombstones(
        &self,
        org_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<Tombstone>, ControlPlaneError> {
        let rows = sqlx::query(
            r#"
            SELECT id, organization_id, subject_kind, subject_key, reason, created_at, applied_at
            FROM tombstones
            WHERE organization_id = $1 AND created_at >= $2
            ORDER BY created_at ASC
            "#,
        )
        .bind(org_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| Tombstone {
                id: r.get(0),
                organization_id: r.get(1),
                subject_kind: r.get(2),
                subject_key: r.get(3),
                reason: r.get(4),
                created_at: r.get(5),
                applied_at: r.get(6),
            })
            .collect())
    }

    async fn get_idempotency_record(
        &self,
        principal_kind: &str,
        principal_id: Uuid,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT principal_kind, principal_id, key, request_hash, response_status, response_body, created_at, expires_at
            FROM idempotency_keys
            WHERE principal_kind = $1 AND principal_id = $2 AND key = $3 AND expires_at > now()
            "#,
        )
        .bind(principal_kind)
        .bind(principal_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| IdempotencyRecord {
            principal_kind: r.get(0),
            principal_id: r.get(1),
            key: r.get(2),
            request_hash: r.get(3),
            response_status: r.get(4),
            response_body: r.get(5),
            created_at: r.get(6),
            expires_at: r.get(7),
        }))
    }

    async fn save_idempotency_record(
        &self,
        record: IdempotencyRecord,
    ) -> Result<bool, ControlPlaneError> {
        // First writer wins. A plain INSERT would raise a unique violation when
        // two concurrent requests carry the same key, and that surfaces as a
        // conflict the caller cannot distinguish from a genuine one. `DO NOTHING`
        // reports the loss as `false` instead, which the caller resolves by
        // replaying the stored response.
        let res = sqlx::query(
            r#"
            INSERT INTO idempotency_keys (principal_kind, principal_id, key, request_hash, response_status, response_body, created_at, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (principal_kind, principal_id, key) DO NOTHING
            "#,
        )
        .bind(&record.principal_kind)
        .bind(record.principal_id)
        .bind(&record.key)
        .bind(&record.request_hash)
        .bind(record.response_status)
        .bind(&record.response_body)
        .bind(record.created_at)
        .bind(record.expires_at)
        .execute(&self.pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    async fn append_stream_event(
        &self,
        event: StreamEvent,
    ) -> Result<StreamEvent, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            INSERT INTO stream_events (organization_id, repository_id, stream, payload, created_at)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, organization_id, repository_id, stream, payload, created_at
            "#,
        )
        .bind(event.organization_id)
        .bind(event.repository_id)
        .bind(&event.stream)
        .bind(&event.payload)
        .bind(event.created_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(StreamEvent {
            id: row.get(0),
            organization_id: row.get(1),
            repository_id: row.get(2),
            stream: row.get(3),
            payload: row.get(4),
            created_at: row.get(5),
        })
    }

    async fn query_stream_events(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        stream: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, ControlPlaneError> {
        let rows = if let Some(rid) = repo_id {
            sqlx::query(
                r#"
                SELECT id, organization_id, repository_id, stream, payload, created_at
                FROM stream_events
                WHERE organization_id = $1 AND (repository_id IS NULL OR repository_id = $2) AND stream = $3 AND id > $4
                ORDER BY id ASC
                LIMIT $5
                "#,
            )
            .bind(org_id)
            .bind(rid)
            .bind(stream)
            .bind(after_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, organization_id, repository_id, stream, payload, created_at
                FROM stream_events
                WHERE organization_id = $1 AND stream = $2 AND id > $3
                ORDER BY id ASC
                LIMIT $4
                "#,
            )
            .bind(org_id)
            .bind(stream)
            .bind(after_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|r| StreamEvent {
                id: r.get(0),
                organization_id: r.get(1),
                repository_id: r.get(2),
                stream: r.get(3),
                payload: r.get(4),
                created_at: r.get(5),
            })
            .collect())
    }

    async fn record_published_object(
        &self,
        obj: PublishedObject,
    ) -> Result<PublishedObject, ControlPlaneError> {
        sqlx::query(
            r#"
            INSERT INTO published_objects (id, organization_id, repository_id, content_hash, byte_length, media_type, class, encryption, state, uploaded_by_daemon, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (organization_id, content_hash) DO UPDATE SET state = EXCLUDED.state
            "#,
        )
        .bind(obj.id)
        .bind(obj.organization_id)
        .bind(obj.repository_id)
        .bind(&obj.content_hash)
        .bind(obj.byte_length)
        .bind(&obj.media_type)
        .bind(&obj.class)
        .bind(&obj.encryption)
        .bind(&obj.state)
        .bind(obj.uploaded_by_daemon)
        .bind(obj.created_at)
        .execute(&self.pool)
        .await?;

        Ok(obj)
    }

    async fn get_published_object(
        &self,
        org_id: Uuid,
        content_hash: &[u8],
    ) -> Result<Option<PublishedObject>, ControlPlaneError> {
        let row = sqlx::query(
            r#"
            SELECT id, organization_id, repository_id, content_hash, byte_length, media_type, class, encryption, state, uploaded_by_daemon, created_at
            FROM published_objects
            WHERE organization_id = $1 AND content_hash = $2
            "#,
        )
        .bind(org_id)
        .bind(content_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| PublishedObject {
            id: r.get(0),
            organization_id: r.get(1),
            repository_id: r.get(2),
            content_hash: r.get(3),
            byte_length: r.get(4),
            media_type: r.get(5),
            class: r.get(6),
            encryption: r.get(7),
            state: r.get(8),
            uploaded_by_daemon: r.get(9),
            created_at: r.get(10),
        }))
    }

    async fn update_object_state(&self, id: Uuid, state: &str) -> Result<(), ControlPlaneError> {
        sqlx::query("UPDATE published_objects SET state = $2 WHERE id = $1")
            .bind(id)
            .bind(state)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn append_audit_record(
        &self,
        mut record: AuditRecord,
    ) -> Result<AuditRecord, ControlPlaneError> {
        // The predecessor read and the insert are one atomic step, ported from
        // `crates/daemon/src/ledger.rs::append_next_event`, which claims its
        // sequence inside the INSERT precisely because a separate
        // `next_sequence` + `append_event` race.
        //
        // PostgreSQL has no `BEGIN IMMEDIATE`, and the same trick — deriving the
        // link inside the INSERT — is not available here either: `record_hash` is
        // a SHA-256 computed in Rust over the predecessor's hash, not a value SQL
        // can produce. So the equivalent guarantee comes from a transaction
        // holding a per-organization advisory lock across the read and the write.
        //
        // Row locking is not an option: `SELECT ... FOR UPDATE` needs UPDATE
        // privilege, and `0005_audit.sql` revokes UPDATE on this table by design.
        // An empty chain has no row to lock either, so the very first two appends
        // would still race. The advisory lock has neither problem.
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(AUDIT_CHAIN_LOCK_NAMESPACE)
            .bind(audit_chain_lock_key(record.organization_id))
            .execute(&mut *tx)
            .await?;

        let latest = fetch_latest_audit_record(&mut *tx, record.organization_id).await?;
        let prev_hash = latest.as_ref().map(|l| l.record_hash.clone());
        record.prev_hash = prev_hash.clone();
        // Read order and chain order must be the same order, and `occurred_at`
        // was stamped by the caller before it queued behind this lock.
        record.occurred_at =
            chain_ordered_timestamp(record.occurred_at, latest.as_ref().map(|l| l.occurred_at));

        record.record_hash = compute_record_hash(
            prev_hash.as_deref(),
            record.organization_id,
            &record.actor_kind,
            record.actor_id,
            &record.action,
            &record.target_kind,
            &record.target_id,
            &record.action_digest,
            &record.detail,
            record.occurred_at,
        );

        sqlx::query(
            r#"
            INSERT INTO audit_records (id, organization_id, actor_kind, actor_id, action, target_kind, target_id, action_digest, correlation_id, prev_hash, record_hash, detail, occurred_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(record.id)
        .bind(record.organization_id)
        .bind(&record.actor_kind)
        .bind(record.actor_id)
        .bind(&record.action)
        .bind(&record.target_kind)
        .bind(&record.target_id)
        .bind(&record.action_digest)
        .bind(record.correlation_id)
        .bind(&record.prev_hash)
        .bind(&record.record_hash)
        .bind(&record.detail)
        .bind(record.occurred_at)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(record)
    }

    async fn get_latest_audit_record(
        &self,
        org_id: Uuid,
    ) -> Result<Option<AuditRecord>, ControlPlaneError> {
        fetch_latest_audit_record(&self.pool, org_id).await
    }

    async fn list_audit_records(
        &self,
        org_id: Uuid,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, ControlPlaneError> {
        // `id DESC` is required, not cosmetic: it is the same tiebreaker
        // `fetch_latest_audit_record` uses, and without it two records sharing a
        // timestamp come back in planner-dependent order, which verification
        // reads as a broken chain.
        let sql = format!(
            "SELECT {AUDIT_SELECT_COLUMNS} FROM audit_records WHERE organization_id = $1 ORDER BY occurred_at DESC, id DESC LIMIT $2"
        );
        let rows = sqlx::query(&sql)
            .bind(org_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.iter().map(audit_record_from_row).collect())
    }
}

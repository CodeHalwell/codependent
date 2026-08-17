use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{audit::AuditRecord, error::ControlPlaneError};

pub mod memory;
pub mod postgres;

/// Round a timestamp down to microsecond resolution.
///
/// `audit_records.occurred_at` is a PostgreSQL `timestamptz`, which stores
/// microseconds. `compute_record_hash` hashes the RFC 3339 rendering of the
/// timestamp, so a nanosecond-precision `Utc::now()` hashed *before* the insert
/// produces a hash that can never be reproduced from the row read back — every
/// chain verification against PostgreSQL would fail. Normalizing before hashing
/// makes the hashed value and the stored value the same value.
///
/// Applied by every [`Store`] implementation so the in-memory and PostgreSQL
/// stores compute identical hashes for identical input.
pub fn normalize_audit_timestamp(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts.with_nanosecond(ts.timestamp_subsec_micros() * 1_000)
        .unwrap_or(ts)
}

/// The `occurred_at` a record must carry to sort strictly after `predecessor`
/// under the audit ordering `(occurred_at, id)`.
///
/// The hash chain is a linked list, but the only order the store can read it
/// back in is `ORDER BY occurred_at DESC, id DESC`. `occurred_at` is stamped by
/// the caller *before* it reaches the store, so under concurrency a request that
/// took its timestamp first can be appended second — leaving a record that sorts
/// *before* its own predecessor and a chain that `verify_audit_chain` rejects
/// forever.
///
/// Advancing the timestamp by one microsecond (the storage resolution) when it
/// would not otherwise sort after the tail keeps read order and chain order the
/// same order. The value is only ever moved forward, never invented: a record
/// whose timestamp already sorts after the tail is stored exactly as supplied.
pub fn chain_ordered_timestamp(
    requested: DateTime<Utc>,
    predecessor: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let requested = normalize_audit_timestamp(requested);
    match predecessor {
        Some(prev) if requested <= prev => prev + chrono::Duration::microseconds(1),
        _ => requested,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub display_name: String,
    pub primary_email: Option<String>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdentity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email_at_link: Option<String>,
    pub linked_at: DateTime<Utc>,
    pub link_audit_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub rotated_from: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub user_agent_digest: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFlow {
    pub state: String,
    pub provider: String,
    pub issuer: String,
    pub pkce_verifier_hash: Vec<u8>,
    pub nonce: String,
    pub redirect_uri: String,
    pub linking_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub max_publication_class: String,
    pub max_classification: String,
    pub data_residency: Option<String>,
    pub retention_days: Option<i32>,
    pub policy_version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub state: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub federated_id: String,
    pub display_name: String,
    pub max_publication_class: String,
    pub max_classification: String,
    pub policy_version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleGrant {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub repository_id: Option<Uuid>,
    pub role: String,
    pub action_scope: Option<serde_json::Value>,
    pub granted_by: Uuid,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Daemon {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub paired_by: Uuid,
    pub display_name: String,
    pub consent_manifest_hash: Vec<u8>,
    pub max_publication_class: String,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    pub state: String,
    pub paired_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingChallenge {
    pub code_hash: Vec<u8>,
    pub organization_id: Uuid,
    pub initiated_by: Uuid,
    pub requested_scope: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub daemon_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadCredential {
    pub id: Uuid,
    pub daemon_id: Uuid,
    pub audience: String,
    pub purpose: String,
    pub token_hash: Vec<u8>,
    pub rotated_from: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedSession {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub repository_id: Uuid,
    pub daemon_id: Uuid,
    pub remote_session_key: String,
    pub class: String,
    pub title: Option<String>,
    pub state: String,
    pub started_at: DateTime<Utc>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReceipt {
    pub id: Uuid,
    pub daemon_id: Uuid,
    pub daemon_sequence: i64,
    pub delta_kind: String,
    pub payload_hash: Vec<u8>,
    pub class: String,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub subject_kind: String,
    pub subject_key: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub principal_kind: String,
    pub principal_id: Uuid,
    pub key: String,
    pub request_hash: Vec<u8>,
    pub response_status: i32,
    pub response_body: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub id: i64,
    pub organization_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub stream: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedObject {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub repository_id: Option<Uuid>,
    pub content_hash: Vec<u8>,
    pub byte_length: i64,
    pub media_type: String,
    pub class: String,
    pub encryption: String,
    pub state: String,
    pub uploaded_by_daemon: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn is_ready(&self) -> bool;

    // Users & Identity
    async fn create_user(&self, user: User) -> Result<User, ControlPlaneError>;
    async fn get_user(&self, id: Uuid) -> Result<Option<User>, ControlPlaneError>;
    async fn create_user_identity(
        &self,
        identity: UserIdentity,
    ) -> Result<UserIdentity, ControlPlaneError>;
    async fn find_user_identity(
        &self,
        provider: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<UserIdentity>, ControlPlaneError>;

    // Refresh Tokens
    async fn save_refresh_token(&self, token: UserRefreshToken) -> Result<(), ControlPlaneError>;
    async fn lookup_refresh_token(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<UserRefreshToken>, ControlPlaneError>;
    async fn revoke_refresh_token(&self, id: Uuid) -> Result<(), ControlPlaneError>;
    async fn revoke_refresh_token_chain(&self, token_hash: &[u8]) -> Result<(), ControlPlaneError>;

    // Organizations
    async fn create_organization(
        &self,
        org: Organization,
    ) -> Result<Organization, ControlPlaneError>;
    async fn get_organization(&self, id: Uuid) -> Result<Option<Organization>, ControlPlaneError>;
    async fn get_organization_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<Organization>, ControlPlaneError>;
    async fn list_user_organizations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<Organization>, ControlPlaneError>;

    // Memberships & Role Grants
    async fn add_membership(&self, membership: Membership) -> Result<(), ControlPlaneError>;
    async fn create_role_grant(&self, grant: RoleGrant) -> Result<RoleGrant, ControlPlaneError>;
    async fn list_user_grants(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<RoleGrant>, ControlPlaneError>;

    // Repositories
    async fn create_repository(&self, repo: Repository) -> Result<Repository, ControlPlaneError>;

    /// Look a repository up **without** a tenant scope.
    ///
    /// Every caller must pair this with an organization check, and doing that
    /// check in Rust after the row is already in hand is exactly the pattern the
    /// tenancy rule forbids. Prefer [`Store::get_repository_in_org`], which puts
    /// the organization in the `WHERE` clause.
    async fn get_repository(&self, id: Uuid) -> Result<Option<Repository>, ControlPlaneError>;

    /// Look a repository up scoped to `org_id` in the query itself.
    ///
    /// Returns `None` both when the repository does not exist and when it
    /// belongs to another organization — the two cases are indistinguishable to
    /// the caller by construction, so no caller can turn the difference into an
    /// existence oracle.
    async fn get_repository_in_org(
        &self,
        org_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Option<Repository>, ControlPlaneError>;
    async fn find_repository_by_federated_id(
        &self,
        org_id: Uuid,
        federated_id: &str,
    ) -> Result<Option<Repository>, ControlPlaneError>;
    async fn list_authorized_repositories(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Repository>, ControlPlaneError>;

    // Daemons & Workloads
    async fn create_pairing_challenge(
        &self,
        challenge: PairingChallenge,
    ) -> Result<(), ControlPlaneError>;
    async fn consume_pairing_challenge(
        &self,
        code_hash: &[u8],
        daemon_id: Uuid,
    ) -> Result<Option<PairingChallenge>, ControlPlaneError>;
    async fn register_daemon(&self, daemon: Daemon) -> Result<Daemon, ControlPlaneError>;
    async fn get_daemon(&self, daemon_id: Uuid) -> Result<Option<Daemon>, ControlPlaneError>;
    async fn update_daemon_state(
        &self,
        daemon_id: Uuid,
        state: &str,
    ) -> Result<(), ControlPlaneError>;
    async fn save_workload_credential(
        &self,
        cred: WorkloadCredential,
    ) -> Result<(), ControlPlaneError>;
    async fn lookup_workload_credential(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<WorkloadCredential>, ControlPlaneError>;

    // Sync & Sessions
    async fn upsert_shared_session(
        &self,
        session: SharedSession,
    ) -> Result<SharedSession, ControlPlaneError>;
    async fn list_shared_sessions(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SharedSession>, ControlPlaneError>;
    async fn record_sync_receipt(&self, receipt: SyncReceipt) -> Result<bool, ControlPlaneError>;
    async fn create_tombstone(&self, tombstone: Tombstone) -> Result<(), ControlPlaneError>;
    async fn list_tombstones(
        &self,
        org_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<Tombstone>, ControlPlaneError>;

    // Idempotency
    //
    // STATUS: these two primitives and the `idempotency_keys` table are complete
    // and tested, but **no route calls them today**. Nothing in `src/routes/`
    // reads an `Idempotency-Key` header. They are storage, not a live mechanism.
    //
    // The protocol a route must follow to use them, in order:
    //
    //  1. Read the `Idempotency-Key` header. Absent -> handle the request
    //     normally; these calls are skipped entirely.
    //  2. `request_hash = compute_action_digest(canonical_request_body)`.
    //  3. `get_idempotency_record(principal_kind, principal_id, key)`.
    //     - `Some` with an equal `request_hash`: replay `response_status` and
    //       `response_body` verbatim. Do not re-run the effect.
    //     - `Some` with a different `request_hash`: return
    //       `ControlPlaneError::IdempotencyConflict` — the same key with a
    //       different body is a client bug and must never be served the old
    //       response.
    //     - `None`: continue.
    //  4. Perform the effect, then `save_idempotency_record`. A `false` return
    //     means a concurrent request for the same key won the race: re-read with
    //     `get_idempotency_record` and serve *that* response, so both callers see
    //     one answer.
    //
    // The key is scoped to the principal, never globally, so one tenant cannot
    // probe another tenant's keys by observing a conflict.

    /// Fetch a non-expired idempotency record for this principal, or `None`.
    ///
    /// Expired records are never returned: an expired key is treated as absent,
    /// so a stale response can never be replayed.
    async fn get_idempotency_record(
        &self,
        principal_kind: &str,
        principal_id: Uuid,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, ControlPlaneError>;

    /// Store a response against an idempotency key, first writer wins.
    ///
    /// Returns `true` if this call stored the record, `false` if an entry for
    /// `(principal_kind, principal_id, key)` already existed and was left
    /// untouched. `false` is not an error — it means another concurrent request
    /// recorded the authoritative response and the caller must replay that one.
    async fn save_idempotency_record(
        &self,
        record: IdempotencyRecord,
    ) -> Result<bool, ControlPlaneError>;

    // Resumable Event Streams
    async fn append_stream_event(
        &self,
        event: StreamEvent,
    ) -> Result<StreamEvent, ControlPlaneError>;
    async fn query_stream_events(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        stream: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, ControlPlaneError>;

    // Objects
    async fn record_published_object(
        &self,
        obj: PublishedObject,
    ) -> Result<PublishedObject, ControlPlaneError>;
    async fn get_published_object(
        &self,
        org_id: Uuid,
        content_hash: &[u8],
    ) -> Result<Option<PublishedObject>, ControlPlaneError>;
    async fn update_object_state(&self, id: Uuid, state: &str) -> Result<(), ControlPlaneError>;

    // Audit Records
    //
    // The three calls below share one ordering contract, and the hash chain is
    // only verifiable while all three agree on it:
    //
    //   audit order = (occurred_at ASC, id ASC), total, per organization
    //
    // `append_audit_record` links each record to the tail of that order,
    // `get_latest_audit_record` returns that tail, and `list_audit_records`
    // returns the reverse of that order. `id` is not decoration: without it the
    // order is partial, and two records sharing a timestamp come back in
    // whatever order the planner chose that day, which reads as a broken chain.

    /// Append a record, linking it to the current tail of this organization's
    /// chain.
    ///
    /// `prev_hash`, `record_hash` and (where ordering requires it)
    /// `occurred_at` are set by the store; whatever the caller put in those
    /// fields is overwritten. The returned record is what was persisted.
    ///
    /// Reading the predecessor and writing the successor is atomic per
    /// organization. It has to be: two appends that read the same `prev_hash`
    /// both commit, the chain forks, and `verify_audit_chain` fails from then
    /// on — permanently, because the table is append-only.
    async fn append_audit_record(
        &self,
        record: AuditRecord,
    ) -> Result<AuditRecord, ControlPlaneError>;

    /// The tail of this organization's chain — the record a new append links to.
    async fn get_latest_audit_record(
        &self,
        org_id: Uuid,
    ) -> Result<Option<AuditRecord>, ControlPlaneError>;

    /// The newest `limit` records, newest first.
    ///
    /// Reverse the result to get chain order, which is what
    /// [`crate::audit::verify_audit_chain`] expects. Verifying a *window* of the
    /// chain only succeeds when the window reaches back to the genesis record;
    /// a truncated window legitimately reports a broken first link.
    async fn list_audit_records(
        &self,
        org_id: Uuid,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, ControlPlaneError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64, nanos: u32) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, nanos).unwrap()
    }

    #[test]
    fn normalization_drops_sub_microsecond_precision_only() {
        assert_eq!(
            normalize_audit_timestamp(at(1_000, 123_456_789)),
            at(1_000, 123_456_000)
        );
        // Already at storage resolution: unchanged.
        let exact = at(1_000, 123_456_000);
        assert_eq!(normalize_audit_timestamp(exact), exact);
    }

    #[test]
    fn a_genesis_record_keeps_its_own_timestamp() {
        assert_eq!(
            chain_ordered_timestamp(at(1_000, 500_000), None),
            at(1_000, 500_000)
        );
    }

    #[test]
    fn a_timestamp_already_after_the_tail_is_untouched() {
        let tail = at(1_000, 0);
        assert_eq!(
            chain_ordered_timestamp(at(1_005, 0), Some(tail)),
            at(1_005, 0)
        );
    }

    #[test]
    fn a_tied_or_stale_timestamp_is_advanced_past_the_tail() {
        let tail = at(1_000, 500_000);

        // Tie: two appends inside the same microsecond.
        assert!(chain_ordered_timestamp(tail, Some(tail)) > tail);
        // Stale: this request took its timestamp first but appended second.
        assert!(chain_ordered_timestamp(at(999, 0), Some(tail)) > tail);
        // Advanced by exactly one storage tick, never further.
        assert_eq!(
            chain_ordered_timestamp(at(999, 0), Some(tail)),
            at(1_000, 501_000)
        );
    }

    #[test]
    fn repeated_stale_appends_stay_strictly_increasing() {
        let mut tail = at(1_000, 0);
        for _ in 0..100 {
            let next = chain_ordered_timestamp(at(500, 0), Some(tail));
            assert!(next > tail, "chain order must never stall or go backwards");
            tail = next;
        }
    }
}

use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use codypendent_control_plane_protocol::{daemon::PairingScope, publication::PublicationClass};

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

/// Caller-generated material for one atomic refresh-token rotation.
///
/// The store resolves the old token and authoritative user itself, then revokes
/// the old token and inserts the replacement in one critical section or
/// database transaction. This deliberately does not carry a `user_id` or
/// `rotated_from`: neither value may be supplied by the caller.
#[derive(Debug, Clone)]
pub struct RefreshRotation {
    pub old_token_hash: Vec<u8>,
    pub new_id: Uuid,
    pub new_token_hash: Vec<u8>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub user_agent_digest: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum RefreshRotationOutcome {
    Rotated(User),
    Invalid,
    Expired,
    ReuseDetected,
    InactiveUser,
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

/// Caller-generated material used to complete a pairing challenge atomically.
///
/// Tenant and pairing-user identity come exclusively from the locked challenge
/// row. The caller cannot choose either authority-bearing value.
#[derive(Debug, Clone)]
pub struct PairingCompletion {
    pub daemon_id: Uuid,
    pub display_name: String,
    pub consent_manifest_hash: Vec<u8>,
    pub max_publication_class: String,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    pub credential_id: Uuid,
    pub credential_audience: String,
    pub credential_purpose: String,
    pub credential_token_hash: Vec<u8>,
    pub completed_at: DateTime<Utc>,
    pub credential_expires_at: DateTime<Utc>,
}

/// Decode the authority-bearing scope stored on a pairing challenge.
///
/// Completion requests repeat these fields only as a consent assertion. The
/// locked challenge remains the authority source; malformed or mismatched
/// scope must fail before a daemon or credential becomes visible.
pub(crate) fn validated_pairing_scope(
    challenge: &PairingChallenge,
    completion: &PairingCompletion,
) -> Result<PairingScope, ControlPlaneError> {
    if completion.display_name.trim().is_empty() || completion.display_name.len() > 128 {
        return Err(ControlPlaneError::BadRequest(
            "daemon display name must contain 1 to 128 bytes".into(),
        ));
    }
    if completion.consent_manifest_hash.len() != 32
        || completion.credential_token_hash.len() != 32
        || completion.credential_audience != "control-plane"
        || completion.credential_purpose != "sync"
        || completion.credential_expires_at <= completion.completed_at
    {
        return Err(ControlPlaneError::BadRequest(
            "pairing completion credential material is invalid".into(),
        ));
    }
    let scope: PairingScope = serde_json::from_value(challenge.requested_scope.clone())
        .map_err(|_| ControlPlaneError::BadRequest("pairing challenge scope is invalid".into()))?;
    if scope.max_publication_class
        == codypendent_control_plane_protocol::publication::PublicationClass::Unknown
        || completion.max_publication_class != scope.max_publication_class.as_str()
        || completion.accepts_remote_approvals != scope.accepts_remote_approvals
        || completion.accepts_runner_dispatch != scope.accepts_runner_dispatch
    {
        return Err(ControlPlaneError::BadRequest(
            "pairing completion does not match the approved challenge scope".into(),
        ));
    }
    Ok(scope)
}

/// Re-check a challenge against the organization's current publication policy.
/// The policy can be narrowed during a challenge's 15-minute lifetime, so the
/// check made when the code was issued is not sufficient at completion time.
pub(crate) fn pairing_scope_fits_organization(
    scope: &PairingScope,
    organization: &Organization,
) -> bool {
    let ceiling: PublicationClass = serde_json::from_value(serde_json::Value::String(
        organization.max_publication_class.clone(),
    ))
    .unwrap_or(PublicationClass::Unknown);
    scope.max_publication_class.permits_in_ceiling(ceiling)
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

/// The shared-session projection invalidated by a session tombstone.
///
/// Kept separate from [`Tombstone`] because other tombstone kinds do not have
/// a first-class projection table, while a session deletion must hide the
/// existing row in the same transaction as its receipt and audit record.
#[derive(Debug, Clone)]
pub struct SharedSessionTombstone {
    pub organization_id: Uuid,
    pub repository_id: Uuid,
    pub daemon_id: Uuid,
    pub remote_session_key: String,
    pub tombstoned_at: DateTime<Utc>,
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

/// The projection a sync delta applies, alongside its receipt.
///
/// A delta kind this control plane does not project (`_ => {}` in the route's
/// match) carries [`SyncProjection::None`] — it still earns a receipt and a
/// stream event, it simply has no table of its own to touch.
#[derive(Debug, Clone)]
pub enum SyncProjection {
    None,
    SharedSession(Box<SharedSession>),
    Tombstone {
        record: Box<Tombstone>,
        shared_session: Option<SharedSessionTombstone>,
    },
}

/// Everything one accepted sync delta writes, as a single unit of work.
///
/// These three writes used to be three autocommits in a row, receipt first.
/// A failure after the receipt landed — a dropped connection, a killed
/// process, a constraint violation on the projection — left a receipt for an
/// effect that had never happened. The daemon's retry then hit the duplicate
/// short-circuit, was handed that receipt, and marked its outbox entry
/// acknowledged. The delta was gone, and nothing anywhere reported a loss.
///
/// Committing them together is what makes the receipt mean what its name says:
/// this delta's effect is durable.
#[derive(Debug, Clone)]
pub struct SyncDeltaApplication {
    pub receipt: SyncReceipt,
    pub projection: SyncProjection,
    pub event: StreamEvent,
}

/// What [`Store::apply_sync_delta`] did.
#[derive(Debug, Clone)]
pub enum SyncDeltaOutcome {
    /// First delivery. Receipt, projection and event are all durable, and the
    /// appended event (with its assigned id) is returned so the caller can
    /// publish it — after the commit, never before.
    Applied(Box<StreamEvent>),
    /// A receipt for this `(daemon_id, daemon_sequence)` already existed, so
    /// nothing was written. The caller reads the stored receipt back and
    /// reports it verbatim.
    Duplicate,
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
    async fn rotate_refresh_token(
        &self,
        rotation: RefreshRotation,
    ) -> Result<RefreshRotationOutcome, ControlPlaneError>;

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
    async fn get_membership(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Membership>, ControlPlaneError>;
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
    async fn complete_pairing(
        &self,
        code_hash: &[u8],
        completion: PairingCompletion,
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
    /// Record a receipt, first delivery wins. `false` means this
    /// `(daemon_id, daemon_sequence)` was already durably accepted — a replay,
    /// not an error, and not a second effect.
    async fn record_sync_receipt(&self, receipt: SyncReceipt) -> Result<bool, ControlPlaneError>;

    /// Apply one accepted sync delta **atomically**: receipt, projection and
    /// stream event commit together or not at all.
    ///
    /// The duplicate check happens inside the same transaction as the writes,
    /// which also closes the race two concurrent deliveries of one sequence
    /// used to have. See [`SyncDeltaApplication`] for why the ordering matters.
    async fn apply_sync_delta(
        &self,
        application: SyncDeltaApplication,
    ) -> Result<SyncDeltaOutcome, ControlPlaneError>;

    /// The receipt already stored for this daemon's sequence, if any.
    ///
    /// A replayed delta must be answered with the receipt that was actually
    /// written — its id, the class that was actually stored, and the time it was
    /// actually accepted. Minting a fresh receipt id for a replay would report an
    /// effect that never happened and would hand the daemon a class the control
    /// plane never agreed to.
    ///
    /// Scoped to `daemon_id`, which is only ever the authenticated principal's
    /// own id, so this cannot be turned into a cross-tenant probe.
    async fn get_sync_receipt(
        &self,
        daemon_id: Uuid,
        daemon_sequence: i64,
    ) -> Result<Option<SyncReceipt>, ControlPlaneError>;

    /// Highest sequence durably accepted from this daemon, or `None` when the
    /// daemon has never had a delta accepted.
    ///
    /// `None` is not zero. Zero is a legitimate sequence number, and the caller
    /// must decide how to render "no sequence has ever been accepted" rather
    /// than have this method invent a measurement.
    async fn latest_sync_sequence(&self, daemon_id: Uuid)
        -> Result<Option<i64>, ControlPlaneError>;
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
    use codypendent_control_plane_protocol::daemon::PairingScope;

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

    #[tokio::test]
    async fn pairing_completion_rechecks_the_current_organization_ceiling() {
        let store = memory::MemoryStore::new();
        let now = Utc::now();
        let user_id = Uuid::now_v7();
        let org_id = Uuid::now_v7();
        let daemon_id = Uuid::now_v7();
        store
            .create_user(User {
                id: user_id,
                display_name: "Pairing user".to_string(),
                primary_email: None,
                state: "active".to_string(),
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
        store
            .create_organization(Organization {
                id: org_id,
                slug: "narrowed-org".to_string(),
                display_name: "Narrowed".to_string(),
                max_publication_class: "metadata-shared".to_string(),
                max_classification: "internal".to_string(),
                data_residency: None,
                retention_days: None,
                policy_version: 2,
                created_at: now,
            })
            .await
            .unwrap();
        store
            .add_membership(Membership {
                organization_id: org_id,
                user_id,
                state: "active".to_string(),
                joined_at: Some(now),
                created_at: now,
            })
            .await
            .unwrap();
        let code_hash = vec![7_u8; 32];
        store
            .create_pairing_challenge(PairingChallenge {
                code_hash: code_hash.clone(),
                organization_id: org_id,
                initiated_by: user_id,
                requested_scope: serde_json::to_value(PairingScope {
                    // This could have been allowed when the challenge was
                    // issued; it is above the organization's policy now.
                    max_publication_class: PublicationClass::ContentShared,
                    accepts_remote_approvals: false,
                    accepts_runner_dispatch: false,
                    repositories: Vec::new(),
                })
                .unwrap(),
                created_at: now,
                expires_at: now + chrono::Duration::minutes(15),
                consumed_at: None,
                daemon_id: None,
            })
            .await
            .unwrap();

        let outcome = store
            .complete_pairing(
                &code_hash,
                PairingCompletion {
                    daemon_id,
                    display_name: "daemon".to_string(),
                    consent_manifest_hash: vec![1_u8; 32],
                    max_publication_class: "content-shared".to_string(),
                    accepts_remote_approvals: false,
                    accepts_runner_dispatch: false,
                    credential_id: Uuid::now_v7(),
                    credential_audience: "control-plane".to_string(),
                    credential_purpose: "sync".to_string(),
                    credential_token_hash: vec![2_u8; 32],
                    completed_at: now,
                    credential_expires_at: now + chrono::Duration::days(365),
                },
            )
            .await
            .unwrap();
        assert!(outcome.is_none());
        assert!(store.get_daemon(daemon_id).await.unwrap().is_none());
        assert!(store
            .lookup_workload_credential(&[2_u8; 32])
            .await
            .unwrap()
            .is_none());
    }
}

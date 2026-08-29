//! Golden vectors for the control-plane and runner protocols.
//!
//! The project states a wire-compatibility guarantee. Until this file existed
//! there was nothing behind it for these two protocols: the daemon protocol has
//! committed golden vectors (`protocol-vectors/*.json`, emitted by
//! `crates/protocol/tests/golden_vectors.rs`), the control-plane and runner
//! protocols had none, so any field could be renamed, retyped or dropped and
//! every test in the workspace would still pass.
//!
//! This file follows that daemon file's pattern exactly:
//!
//! * One deterministic instance of every wire type — fixed sentinel ids and a
//!   fixed timestamp, never `Uuid::now_v7()` / `Utc::now()` — so the emitted
//!   bytes are stable across regenerations.
//! * One committed JSON file per source module, under
//!   `<repo-root>/protocol-vectors/control-plane/` and
//!   `<repo-root>/protocol-vectors/runner/`, each a JSON object mapping a
//!   descriptive vector name to the serialized value, with every object's keys
//!   sorted (see [`sort_keys`]) and pretty-printed.
//! * Two CI gates that fail if the committed bytes drift from the Rust types,
//!   plus a partition guard so a NEW wire type cannot silently escape coverage.
//!
//! ## Regenerating
//!
//! ```text
//! cargo test -p codypendent-control-plane-protocol --test golden_vectors regenerate_vectors -- --ignored
//! ```
//!
//! Run it whenever a wire type changes shape, review the diff under
//! `protocol-vectors/`, and commit it alongside the code change. CI never runs
//! the regenerator (it is `#[ignore]`d and it WRITES files); CI runs:
//!
//! * [`committed_vectors_match_current_protocol_types`] — a fresh regeneration
//!   must equal the committed bytes exactly.
//! * [`committed_vectors_round_trip_through_their_rust_types`] — every
//!   committed entry, deserialized through its concrete Rust type and
//!   re-serialized, must reproduce itself exactly.
//! * [`every_wire_type_has_a_golden_vector`] — the partition guard. It reads
//!   this crate's own `src/*.rs` at test time and asserts every `pub struct` /
//!   `pub enum` / `uuid_id!` type either has at least one vector or is named in
//!   [`TYPES_WITHOUT_VECTORS`] with a reason. Adding a type without a vector
//!   fails here, which is the whole point: coverage that is only as good as
//!   whoever last remembered to extend it is not coverage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use codypendent_control_plane_protocol::*;

// ---------------------------------------------------------------------------
// Sentinel builders: fixed, readable, non-random. Every kind of id gets a
// distinct hex prefix so a reader can tell at a glance which domain an id in a
// vector belongs to.
// ---------------------------------------------------------------------------

macro_rules! sentinel_ids {
    ($($name:ident : $ty:ty = $hex:literal;)*) => {
        $(
            fn $name() -> $ty {
                <$ty>::from_uuid(
                    uuid::Uuid::parse_str($hex).expect("sentinel id is a valid uuid"),
                )
            }
        )*
    };
}

sentinel_ids! {
    user_id: UserId = "10000000-0000-0000-0000-000000000001";
    organization_id: OrganizationId = "20000000-0000-0000-0000-000000000001";
    team_id: TeamId = "30000000-0000-0000-0000-000000000001";
    workspace_id: WorkspaceId = "40000000-0000-0000-0000-000000000001";
    repository_id: RepositoryId = "50000000-0000-0000-0000-000000000001";
    daemon_id: DaemonId = "60000000-0000-0000-0000-000000000001";
    grant_id: GrantId = "70000000-0000-0000-0000-000000000001";
    sync_receipt_id: SyncReceiptId = "80000000-0000-0000-0000-000000000001";
    tombstone_id: TombstoneId = "90000000-0000-0000-0000-000000000001";
    audit_record_id: AuditRecordId = "a0000000-0000-0000-0000-000000000001";
    published_object_id: PublishedObjectId = "b0000000-0000-0000-0000-000000000001";
    identity_id: IdentityId = "c0000000-0000-0000-0000-000000000001";
    refresh_token_id: RefreshTokenId = "d0000000-0000-0000-0000-000000000001";
    workload_credential_id: WorkloadCredentialId = "e0000000-0000-0000-0000-000000000001";
    challenge_id: ChallengeId = "f0000000-0000-0000-0000-000000000001";
    runner_id: RunnerId = "11000000-0000-0000-0000-000000000001";
    runner_job_id: RunnerJobId = "12000000-0000-0000-0000-000000000001";
    runner_attempt_id: RunnerAttemptId = "13000000-0000-0000-0000-000000000001";
    runner_lease_id: RunnerLeaseId = "14000000-0000-0000-0000-000000000001";
    runner_output_id: RunnerOutputId = "15000000-0000-0000-0000-000000000001";
    runner_attestation_id: RunnerAttestationId = "16000000-0000-0000-0000-000000000001";
    runner_quarantine_id: RunnerQuarantineId = "17000000-0000-0000-0000-000000000001";
    shared_session_id: SharedSessionId = "18000000-0000-0000-0000-000000000001";
    correlation_id: CorrelationId = "19000000-0000-0000-0000-000000000001";
}

/// A fixed instant — never `Utc::now()` — so every timestamp in the vector set
/// is byte-for-byte stable across regenerations.
fn sentinel_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("sentinel timestamp parses")
        .with_timezone(&Utc)
}

/// A second, later instant for "updated_at"/"expires_at" style fields, so a
/// vector that swapped two timestamp fields would still be visible in a diff.
fn sentinel_time_later() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
        .expect("sentinel timestamp parses")
        .with_timezone(&Utc)
}

fn digest(seed: &str) -> Sha256Digest {
    Sha256Digest::from_bytes(seed.as_bytes())
}

fn federated_repository_id() -> FederatedRepositoryId {
    FederatedRepositoryId::from_seed_bytes(b"github.com/octocat/hello-world")
}

fn organization_slug() -> OrganizationSlug {
    OrganizationSlug::new("acme").expect("sentinel slug is valid")
}

fn team_slug() -> TeamSlug {
    TeamSlug::new("platform").expect("sentinel slug is valid")
}

fn page_cursor() -> PageCursor {
    PageCursor::encode_keyset(
        "0f0f0f0f",
        "2026-01-01T00:00:00Z",
        "10000000-0000-0000-0000-000000000001",
    )
}

// ---------------------------------------------------------------------------
// Vector / manifest plumbing (mirrors crates/protocol/tests/golden_vectors.rs).
// ---------------------------------------------------------------------------

/// One named wire-type instance: its serialized JSON, plus a way to prove that
/// JSON round-trips through its own concrete Rust type (deserialize ->
/// re-serialize -> identical). `round_trip` is a plain function pointer, not a
/// closure with captures.
struct Vector {
    name: &'static str,
    value: Value,
    round_trip: fn(&Value) -> Value,
}

fn vec_of<T>(name: &'static str, instance: T) -> Vector
where
    T: Serialize + DeserializeOwned,
{
    let value = serde_json::to_value(&instance)
        .unwrap_or_else(|e| panic!("{name}: failed to serialize: {e}"));
    Vector {
        name,
        value,
        round_trip: |v: &Value| {
            let parsed: T = serde_json::from_value(v.clone())
                .unwrap_or_else(|e| panic!("failed to deserialize: {e}"));
            serde_json::to_value(&parsed).expect("re-serialize")
        },
    }
}

/// Build the sorted JSON object for one manifest file. Panics on a duplicate
/// vector name within the file — a silent `Map` overwrite would otherwise drop
/// a vector without any signal.
fn manifest_value(vectors: &[Vector]) -> Value {
    let mut map = serde_json::Map::new();
    for v in vectors {
        let previous = map.insert(v.name.to_string(), v.value.clone());
        assert!(
            previous.is_none(),
            "duplicate vector name {:?} — every vector name must be unique within its file",
            v.name
        );
    }
    Value::Object(map)
}

/// Recursively sort every JSON object's keys so a rendered vector is identical
/// regardless of whether `serde_json`'s `Map` is `BTreeMap`-backed (its default)
/// or `IndexMap`-backed (`preserve_order`, which a workspace `--all-features`
/// build turns on). Array order is preserved: it is semantically significant.
fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let object: serde_json::Map<String, Value> = sorted
                .into_iter()
                .map(|(k, v)| (k.clone(), sort_keys(v)))
                .collect();
            Value::Object(object)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

/// Pretty-print with a trailing newline (a normal committed text file).
fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&sort_keys(value)).expect("pretty-print vectors");
    text.push('\n');
    text
}

/// `<repo-root>/protocol-vectors` — resolved from `CARGO_MANIFEST_DIR`
/// (`crates/control-plane-protocol`) so it works regardless of the caller's
/// working directory.
fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("protocol-vectors")
}

/// `crates/control-plane-protocol/src` — read by the partition guard.
fn source_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

// ---------------------------------------------------------------------------
// ids.rs
// ---------------------------------------------------------------------------

fn ids_vectors() -> Vec<Vector> {
    vec![
        vec_of("UserId", user_id()),
        vec_of("OrganizationId", organization_id()),
        vec_of("TeamId", team_id()),
        vec_of("WorkspaceId", workspace_id()),
        vec_of("RepositoryId", repository_id()),
        vec_of("DaemonId", daemon_id()),
        vec_of("GrantId", grant_id()),
        vec_of("SyncReceiptId", sync_receipt_id()),
        vec_of("TombstoneId", tombstone_id()),
        vec_of("AuditRecordId", audit_record_id()),
        vec_of("PublishedObjectId", published_object_id()),
        vec_of("IdentityId", identity_id()),
        vec_of("RefreshTokenId", refresh_token_id()),
        vec_of("WorkloadCredentialId", workload_credential_id()),
        vec_of("ChallengeId", challenge_id()),
        vec_of("RunnerId", runner_id()),
        vec_of("RunnerJobId", runner_job_id()),
        vec_of("RunnerAttemptId", runner_attempt_id()),
        vec_of("RunnerLeaseId", runner_lease_id()),
        vec_of("RunnerOutputId", runner_output_id()),
        vec_of("RunnerAttestationId", runner_attestation_id()),
        vec_of("RunnerQuarantineId", runner_quarantine_id()),
        vec_of("SharedSessionId", shared_session_id()),
        vec_of("CorrelationId", correlation_id()),
        vec_of("FederatedRepositoryId", federated_repository_id()),
        vec_of("Sha256Digest", digest("payload")),
    ]
}

// ---------------------------------------------------------------------------
// version.rs
// ---------------------------------------------------------------------------

fn version_vectors() -> Vec<Vector> {
    vec![
        vec_of("ProtocolVersion", CONTROL_PLANE_PROTOCOL_V1),
        vec_of(
            "ProtocolVersion_min_supported",
            CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED,
        ),
        vec_of(
            "ProtocolHandshakeRequest",
            ProtocolHandshakeRequest {
                client_version: CONTROL_PLANE_PROTOCOL_V1,
                client_kind: "daemon".to_string(),
                client_build_id: Some("0.11.0+a1b2c3d4e5f6".to_string()),
                capabilities: vec!["sync".to_string(), "runner-dispatch".to_string()],
            },
        ),
        vec_of(
            "ProtocolHandshakeResponse",
            ProtocolHandshakeResponse {
                negotiated_version: CONTROL_PLANE_PROTOCOL_V1,
                server_version: ProtocolVersion::new(1, 3),
                min_compatible_version: CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED,
                supported_capabilities: vec!["sync".to_string()],
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// page.rs
// ---------------------------------------------------------------------------

fn page_vectors() -> Vec<Vector> {
    vec![
        vec_of("PageCursor", page_cursor()),
        vec_of(
            "PageRequest",
            PageRequest {
                cursor: Some(page_cursor()),
                limit: Some(50),
            },
        ),
        vec_of("PageRequest_empty", PageRequest::default()),
        vec_of(
            "Page",
            Page {
                items: vec![user_summary()],
                next_cursor: Some(page_cursor()),
                has_more: true,
                total_count: Some(2),
            },
        ),
        vec_of(
            // `total_count` is a MEASUREMENT: when the control plane does not
            // compute a bounded count inside the authorized set it must stay
            // absent, never be coerced to 0. This vector pins that shape.
            "Page_without_a_counted_total",
            Page::<UserSummary> {
                items: Vec::new(),
                next_cursor: None,
                has_more: false,
                total_count: None,
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// publication.rs
// ---------------------------------------------------------------------------

fn publication_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            "PublicationClass_PrivateLocal",
            PublicationClass::PrivateLocal,
        ),
        vec_of(
            "PublicationClass_MetadataShared",
            PublicationClass::MetadataShared,
        ),
        vec_of(
            "PublicationClass_ContentShared",
            PublicationClass::ContentShared,
        ),
        vec_of(
            "PublicationClass_OrganizationKnowledge",
            PublicationClass::OrganizationKnowledge,
        ),
        vec_of(
            "PublicationClass_PublicMarketplace",
            PublicationClass::PublicMarketplace,
        ),
        vec_of("PublicationClass_Unknown", PublicationClass::Unknown),
        vec_of("DataClassification_Public", DataClassification::Public),
        vec_of("DataClassification_Internal", DataClassification::Internal),
        vec_of(
            "DataClassification_Confidential",
            DataClassification::Confidential,
        ),
        vec_of("DataClassification_Secret", DataClassification::Secret),
        vec_of("DataClassification_Unknown", DataClassification::Unknown),
        vec_of(
            "PolicyRestrictions",
            PolicyRestrictions {
                allowed_providers: Some(vec!["anthropic".to_string()]),
                denied_providers: vec!["example-untrusted".to_string()],
                allowed_models: Some(vec!["claude-sonnet-5".to_string()]),
                denied_models: vec!["legacy-model".to_string()],
                allowed_regions: Some(vec!["eu-west-1".to_string()]),
                denied_regions: vec!["us-east-1".to_string()],
                denied_integrations: vec!["pastebin".to_string()],
            },
        ),
        vec_of("PolicyRestrictions_empty", PolicyRestrictions::default()),
        vec_of(
            "PolicySnapshot",
            PolicySnapshot {
                policy_version: 7,
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
                restrictions: PolicyRestrictions::default(),
                received_at: sentinel_time(),
                payload_hash: digest("policy-snapshot"),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// organization.rs
// ---------------------------------------------------------------------------

fn organization_vectors() -> Vec<Vector> {
    vec![
        vec_of("OrganizationSlug", organization_slug()),
        vec_of(
            "Organization",
            Organization {
                id: organization_id(),
                slug: organization_slug(),
                display_name: "Acme".to_string(),
                max_publication_class: PublicationClass::ContentShared,
                max_classification: DataClassification::Internal,
                data_residency: Some("eu".to_string()),
                retention_days: Some(365),
                policy_version: 7,
                created_at: sentinel_time(),
                updated_at: sentinel_time_later(),
            },
        ),
        vec_of(
            "CreateOrganizationRequest",
            CreateOrganizationRequest {
                slug: organization_slug(),
                display_name: "Acme".to_string(),
                max_publication_class: Some(PublicationClass::ContentShared),
                max_classification: Some(DataClassification::Internal),
                data_residency: Some("eu".to_string()),
                retention_days: Some(365),
            },
        ),
        vec_of(
            "UpdateOrganizationRequest",
            UpdateOrganizationRequest {
                display_name: Some("Acme Corp".to_string()),
                max_publication_class: Some(PublicationClass::MetadataShared),
                max_classification: Some(DataClassification::Confidential),
                data_residency: None,
                retention_days: None,
            },
        ),
        vec_of(
            "OrganizationSummary",
            OrganizationSummary {
                id: organization_id(),
                slug: organization_slug(),
                display_name: "Acme".to_string(),
                max_publication_class: PublicationClass::ContentShared,
                member_count: 12,
                repository_count: 4,
                created_at: sentinel_time(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// workspace.rs
// ---------------------------------------------------------------------------

fn workspace_vectors() -> Vec<Vector> {
    vec![
        vec_of("TeamSlug", team_slug()),
        vec_of(
            "Team",
            Team {
                id: team_id(),
                organization_id: organization_id(),
                slug: team_slug(),
                display_name: "Platform".to_string(),
                created_at: sentinel_time(),
            },
        ),
        vec_of(
            "CreateTeamRequest",
            CreateTeamRequest {
                slug: team_slug(),
                display_name: "Platform".to_string(),
            },
        ),
        vec_of(
            "UpdateTeamRequest",
            UpdateTeamRequest {
                display_name: Some("Platform Engineering".to_string()),
            },
        ),
        vec_of("MembershipState_Invited", MembershipState::Invited),
        vec_of("MembershipState_Active", MembershipState::Active),
        vec_of("MembershipState_Suspended", MembershipState::Suspended),
        vec_of("MembershipState_Unknown", MembershipState::Unknown),
        vec_of(
            "OrganizationMembership",
            OrganizationMembership {
                organization_id: organization_id(),
                user_id: user_id(),
                state: MembershipState::Active,
                joined_at: Some(sentinel_time_later()),
                created_at: sentinel_time(),
            },
        ),
        vec_of(
            "TeamMember",
            TeamMember {
                team_id: team_id(),
                user_id: user_id(),
                joined_at: sentinel_time(),
            },
        ),
        vec_of(
            "AddTeamMemberRequest",
            AddTeamMemberRequest { user_id: user_id() },
        ),
        vec_of(
            "Workspace",
            Workspace {
                id: workspace_id(),
                organization_id: organization_id(),
                slug: team_slug(),
                display_name: "Platform".to_string(),
                created_at: sentinel_time(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// repository.rs
// ---------------------------------------------------------------------------

fn repository_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            "Repository",
            Repository {
                id: repository_id(),
                organization_id: organization_id(),
                federated_id: federated_repository_id(),
                display_name: "hello-world".to_string(),
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
                policy_version: 7,
                created_at: sentinel_time(),
            },
        ),
        vec_of(
            "RegisterRepositoryRequest",
            RegisterRepositoryRequest {
                federated_id: federated_repository_id(),
                display_name: "hello-world".to_string(),
                max_publication_class: Some(PublicationClass::MetadataShared),
                max_classification: Some(DataClassification::Internal),
            },
        ),
        vec_of(
            "UpdateRepositoryRequest",
            UpdateRepositoryRequest {
                display_name: Some("hello-world".to_string()),
                max_publication_class: Some(PublicationClass::PrivateLocal),
                max_classification: Some(DataClassification::Confidential),
            },
        ),
        vec_of(
            "RepositorySummary",
            RepositorySummary {
                id: repository_id(),
                organization_id: organization_id(),
                federated_id: federated_repository_id(),
                display_name: "hello-world".to_string(),
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
                shared_session_count: 3,
                published_object_count: 9,
                created_at: sentinel_time(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// user.rs / identity.rs / auth.rs
// ---------------------------------------------------------------------------

fn user_summary() -> UserSummary {
    UserSummary {
        id: user_id(),
        display_name: "Dana".to_string(),
        primary_email: Some("dana@example.com".to_string()),
        state: UserState::Active,
    }
}

fn user() -> User {
    User {
        id: user_id(),
        display_name: "Dana".to_string(),
        primary_email: Some("dana@example.com".to_string()),
        state: UserState::Active,
        created_at: sentinel_time(),
        updated_at: sentinel_time_later(),
    }
}

fn user_vectors() -> Vec<Vector> {
    vec![
        vec_of("UserState_Active", UserState::Active),
        vec_of("UserState_Suspended", UserState::Suspended),
        vec_of("UserState_Deleted", UserState::Deleted),
        vec_of("UserState_Unknown", UserState::Unknown),
        vec_of("User", user()),
        vec_of(
            "UpdateUserRequest",
            UpdateUserRequest {
                display_name: Some("Dana H".to_string()),
                primary_email: None,
            },
        ),
        vec_of("UserSummary", user_summary()),
    ]
}

fn identity_vectors() -> Vec<Vector> {
    vec![
        vec_of("IdentityProvider_Github", IdentityProvider::Github),
        vec_of("IdentityProvider_Oidc", IdentityProvider::Oidc),
        vec_of("IdentityProvider_Unknown", IdentityProvider::Unknown),
        vec_of(
            "UserIdentity",
            UserIdentity {
                id: identity_id(),
                user_id: user_id(),
                provider: IdentityProvider::Github,
                issuer: "https://github.com".to_string(),
                subject: "1234567".to_string(),
                email_at_link: Some("dana@example.com".to_string()),
                linked_at: sentinel_time(),
                link_audit_id: audit_record_id(),
            },
        ),
        vec_of(
            "IdentityLinkRequest",
            IdentityLinkRequest {
                provider: IdentityProvider::Oidc,
                issuer: "https://issuer.example.com".to_string(),
                auth_code: "auth-code-1".to_string(),
                redirect_uri: "https://app.example.com/callback".to_string(),
                code_verifier: "verifier-1".to_string(),
            },
        ),
        vec_of(
            "IdentityLinkResult",
            IdentityLinkResult {
                identity_id: identity_id(),
                user_id: user_id(),
                provider: IdentityProvider::Oidc,
                linked_at: sentinel_time(),
            },
        ),
    ]
}

fn auth_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            "OAuthInitRequest",
            OAuthInitRequest {
                provider: IdentityProvider::Github,
                redirect_uri: "https://app.example.com/callback".to_string(),
                state: "state-1".to_string(),
                code_challenge: "challenge-1".to_string(),
                code_challenge_method: "S256".to_string(),
            },
        ),
        vec_of(
            "OAuthInitResponse",
            OAuthInitResponse {
                authorization_url: "https://github.com/login/oauth/authorize?state=state-1"
                    .to_string(),
                state: "state-1".to_string(),
            },
        ),
        vec_of(
            "OAuthCallbackRequest",
            OAuthCallbackRequest {
                code: "auth-code-1".to_string(),
                state: "state-1".to_string(),
                code_verifier: "verifier-1".to_string(),
            },
        ),
        vec_of(
            "AuthTokenResponse",
            AuthTokenResponse {
                access_token: "access-token-1".to_string(),
                token_type: "Bearer".to_string(),
                expires_in: 3600,
                refresh_token: Some("refresh-token-1".to_string()),
                user: Some(user()),
            },
        ),
        vec_of(
            "RefreshTokenRequest",
            RefreshTokenRequest {
                refresh_token: "refresh-token-1".to_string(),
            },
        ),
        vec_of(
            "RevokeTokenRequest",
            RevokeTokenRequest {
                token: "refresh-token-1".to_string(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// daemon.rs
// ---------------------------------------------------------------------------

fn consent_manifest() -> ConsentManifest {
    ConsentManifest {
        organization_id: organization_id(),
        organization_display_name: "Acme".to_string(),
        endpoint: "https://control.example.com".to_string(),
        allowed_repositories: vec![federated_repository_id()],
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: true,
        expires_at: Some(sentinel_time_later()),
        created_at: sentinel_time(),
    }
}

fn pairing_scope() -> PairingScope {
    PairingScope {
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: true,
        repositories: vec![federated_repository_id()],
    }
}

fn daemon_vectors() -> Vec<Vector> {
    vec![
        vec_of("DaemonState_Pending", DaemonState::Pending),
        vec_of("DaemonState_Active", DaemonState::Active),
        vec_of("DaemonState_Revoked", DaemonState::Revoked),
        vec_of("DaemonState_Expired", DaemonState::Expired),
        vec_of("DaemonState_Unknown", DaemonState::Unknown),
        vec_of(
            "Daemon",
            Daemon {
                id: daemon_id(),
                organization_id: organization_id(),
                paired_by: user_id(),
                display_name: "dana-laptop".to_string(),
                consent_manifest_hash: consent_manifest().compute_hash(),
                max_publication_class: PublicationClass::MetadataShared,
                accepts_remote_approvals: false,
                accepts_runner_dispatch: true,
                state: DaemonState::Active,
                paired_at: Some(sentinel_time()),
                revoked_at: None,
                last_seen_at: Some(sentinel_time_later()),
                created_at: sentinel_time(),
            },
        ),
        vec_of("ConsentManifest", consent_manifest()),
        vec_of("PairingScope", pairing_scope()),
        vec_of(
            "PairingChallenge",
            PairingChallenge {
                code_hash: digest("pairing-code"),
                organization_id: organization_id(),
                initiated_by: user_id(),
                requested_scope: pairing_scope(),
                created_at: sentinel_time(),
                expires_at: sentinel_time_later(),
                consumed_at: None,
                daemon_id: Some(daemon_id()),
            },
        ),
        vec_of(
            "InitiatePairingRequest",
            InitiatePairingRequest {
                organization_id: organization_id(),
                requested_scope: pairing_scope(),
            },
        ),
        vec_of(
            "InitiatePairingResponse",
            InitiatePairingResponse {
                challenge_code: "BRAVO-DELTA-7".to_string(),
                verification_uri: "https://control.example.com/pair/BRAVO-DELTA-7".to_string(),
                expires_at: sentinel_time_later(),
                poll_interval_seconds: 5,
            },
        ),
        vec_of(
            "ExchangePairingCodeRequest",
            ExchangePairingCodeRequest {
                challenge_code: "BRAVO-DELTA-7".to_string(),
                daemon_display_name: "dana-laptop".to_string(),
                consent_manifest: consent_manifest(),
            },
        ),
        vec_of(
            "ExchangePairingCodeResponse",
            ExchangePairingCodeResponse {
                daemon_id: daemon_id(),
                organization_id: organization_id(),
                access_token: "access-token-1".to_string(),
                refresh_token: "refresh-token-1".to_string(),
                expires_at: sentinel_time_later(),
                max_publication_class: PublicationClass::MetadataShared,
            },
        ),
        vec_of(
            "RevokeDaemonRequest",
            RevokeDaemonRequest {
                daemon_id: daemon_id(),
                reason: "laptop retired".to_string(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// rbac.rs
// ---------------------------------------------------------------------------

fn action_scope() -> ActionScope {
    ActionScope {
        repositories: Some(vec![repository_id()]),
        action_kinds: Some(vec!["ExecuteCommand".to_string()]),
        max_risk_level: Some("medium".to_string()),
    }
}

fn rbac_vectors() -> Vec<Vector> {
    vec![
        vec_of("ControlPlaneRole_Observer", ControlPlaneRole::Observer),
        vec_of(
            "ControlPlaneRole_Contributor",
            ControlPlaneRole::Contributor,
        ),
        vec_of("ControlPlaneRole_Approver", ControlPlaneRole::Approver),
        vec_of("ControlPlaneRole_Maintainer", ControlPlaneRole::Maintainer),
        vec_of(
            "ControlPlaneRole_OrganizationAdmin",
            ControlPlaneRole::OrganizationAdmin,
        ),
        vec_of("ControlPlaneRole_Unknown", ControlPlaneRole::Unknown),
        vec_of("RbacAction_ReadMetadata", RbacAction::ReadMetadata),
        vec_of("RbacAction_ReadContent", RbacAction::ReadContent),
        vec_of("RbacAction_WriteContent", RbacAction::WriteContent),
        vec_of("RbacAction_ApproveAction", RbacAction::ApproveAction),
        vec_of(
            "RbacAction_ManageRepositories",
            RbacAction::ManageRepositories,
        ),
        vec_of("RbacAction_ManageTeam", RbacAction::ManageTeam),
        vec_of(
            "RbacAction_ManageOrganization",
            RbacAction::ManageOrganization,
        ),
        vec_of("RbacAction_DispatchRunner", RbacAction::DispatchRunner),
        vec_of("RbacAction_ReadAuditLogs", RbacAction::ReadAuditLogs),
        vec_of("RbacAction_Unknown", RbacAction::Unknown),
        vec_of("ActionScope", action_scope()),
        vec_of("ActionScope_empty", ActionScope::default()),
        vec_of(
            "RoleGrant",
            RoleGrant {
                id: grant_id(),
                organization_id: organization_id(),
                user_id: Some(user_id()),
                team_id: None,
                repository_id: Some(repository_id()),
                role: ControlPlaneRole::Approver,
                action_scope: Some(action_scope()),
                granted_by: user_id(),
                granted_at: sentinel_time(),
                expires_at: Some(sentinel_time_later()),
                revoked_at: None,
            },
        ),
        vec_of(
            "CreateRoleGrantRequest",
            CreateRoleGrantRequest {
                user_id: None,
                team_id: Some(team_id()),
                repository_id: None,
                role: ControlPlaneRole::Contributor,
                action_scope: None,
                expires_at: Some(sentinel_time_later()),
            },
        ),
        vec_of(
            "RevokeRoleGrantRequest",
            RevokeRoleGrantRequest {
                grant_id: grant_id(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// sync.rs
// ---------------------------------------------------------------------------

fn sync_delta() -> SyncDelta {
    SyncDelta {
        id: "delta-1".to_string(),
        sequence: 42,
        kind: SyncDeltaKind::SessionSummary,
        repository_id: Some(repository_id()),
        subject_id: "session-1".to_string(),
        payload: json!({ "state": "running" }),
        class: PublicationClass::MetadataShared,
        payload_hash: digest("delta-payload"),
        created_at: sentinel_time(),
    }
}

fn sync_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            "SyncDeltaKind_SessionSummary",
            SyncDeltaKind::SessionSummary,
        ),
        vec_of("SyncDeltaKind_RunSummary", SyncDeltaKind::RunSummary),
        vec_of(
            "SyncDeltaKind_ArtifactSummary",
            SyncDeltaKind::ArtifactSummary,
        ),
        vec_of("SyncDeltaKind_InboxEntry", SyncDeltaKind::InboxEntry),
        vec_of("SyncDeltaKind_GraphBatch", SyncDeltaKind::GraphBatch),
        vec_of("SyncDeltaKind_Tombstone", SyncDeltaKind::Tombstone),
        vec_of(
            "SyncDeltaKind_ApprovalDecision",
            SyncDeltaKind::ApprovalDecision,
        ),
        vec_of(
            "SyncDeltaKind_UsageAggregate",
            SyncDeltaKind::UsageAggregate,
        ),
        vec_of("SyncDeltaKind_Unknown", SyncDeltaKind::Unknown),
        vec_of("SyncDelta", sync_delta()),
        vec_of(
            // The batched envelope `POST /v1/sync/push` must accept — the shape
            // `wire_contract.rs::sync_push_body_is_a_batched_envelope_carrying_the_protocol_version`
            // asserts, pinned here byte-for-byte.
            "SyncEnvelope",
            SyncEnvelope {
                protocol_version: CONTROL_PLANE_PROTOCOL_V1,
                daemon_id: daemon_id(),
                organization_id: organization_id(),
                sent_at: sentinel_time(),
                deltas: vec![sync_delta()],
            },
        ),
        vec_of(
            "SyncReceipt",
            SyncReceipt {
                id: sync_receipt_id(),
                daemon_id: daemon_id(),
                daemon_sequence: 42,
                delta_kind: SyncDeltaKind::SessionSummary,
                payload_hash: digest("delta-payload"),
                class: PublicationClass::MetadataShared,
                accepted_at: sentinel_time(),
                duplicate: false,
            },
        ),
        vec_of(
            "SyncRejection",
            SyncRejection {
                sequence: 43,
                code: "class-exceeds-ceiling".to_string(),
                reason: "delta class exceeds the daemon pairing ceiling".to_string(),
            },
        ),
        vec_of(
            "SyncBatchResponse",
            SyncBatchResponse {
                receipts: vec![SyncReceipt {
                    id: sync_receipt_id(),
                    daemon_id: daemon_id(),
                    daemon_sequence: 42,
                    delta_kind: SyncDeltaKind::SessionSummary,
                    payload_hash: digest("delta-payload"),
                    class: PublicationClass::MetadataShared,
                    accepted_at: sentinel_time(),
                    duplicate: true,
                }],
                latest_sequence: 42,
                rejected_deltas: vec![SyncRejection {
                    sequence: 43,
                    code: "class-exceeds-ceiling".to_string(),
                    reason: "delta class exceeds the daemon pairing ceiling".to_string(),
                }],
            },
        ),
        vec_of("TombstoneReason_Deleted", TombstoneReason::Deleted),
        vec_of("TombstoneReason_Narrowed", TombstoneReason::Narrowed),
        vec_of("TombstoneReason_Revoked", TombstoneReason::Revoked),
        vec_of("TombstoneReason_Unknown", TombstoneReason::Unknown),
        vec_of(
            "Tombstone",
            Tombstone {
                id: tombstone_id(),
                organization_id: organization_id(),
                subject_kind: "shared-session".to_string(),
                subject_key: "session-1".to_string(),
                reason: TombstoneReason::Deleted,
                created_at: sentinel_time(),
                applied_at: Some(sentinel_time_later()),
            },
        ),
        vec_of("SharedSessionState_Running", SharedSessionState::Running),
        vec_of(
            "SharedSessionState_Completed",
            SharedSessionState::Completed,
        ),
        vec_of("SharedSessionState_Failed", SharedSessionState::Failed),
        vec_of(
            "SharedSessionState_PendingApproval",
            SharedSessionState::PendingApproval,
        ),
        vec_of(
            "SharedSessionState_Cancelled",
            SharedSessionState::Cancelled,
        ),
        vec_of("SharedSessionState_Unknown", SharedSessionState::Unknown),
        vec_of(
            "SharedSession",
            SharedSession {
                id: shared_session_id(),
                organization_id: organization_id(),
                repository_id: repository_id(),
                daemon_id: daemon_id(),
                remote_session_key: "session-1".to_string(),
                class: PublicationClass::ContentShared,
                title: Some("fix the failing test".to_string()),
                state: SharedSessionState::Running,
                started_at: sentinel_time(),
                last_activity_at: Some(sentinel_time_later()),
                tombstoned_at: None,
                updated_at: sentinel_time_later(),
            },
        ),
        vec_of(
            // Below `content-shared` the title is REDACTED to absent — never an
            // empty string, which would leak that a title existed at all.
            "SharedSession_redacted_below_content_shared",
            SharedSession {
                id: shared_session_id(),
                organization_id: organization_id(),
                repository_id: repository_id(),
                daemon_id: daemon_id(),
                remote_session_key: "session-1".to_string(),
                class: PublicationClass::MetadataShared,
                title: None,
                state: SharedSessionState::Running,
                started_at: sentinel_time(),
                last_activity_at: None,
                tombstoned_at: None,
                updated_at: sentinel_time_later(),
            },
        ),
        vec_of(
            "SyncCursor",
            SyncCursor {
                pairing_id: "pairing-1".to_string(),
                stream: "sessions".to_string(),
                cursor: "128".to_string(),
                updated_at: sentinel_time(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// audit.rs
// ---------------------------------------------------------------------------

fn audit_record() -> AuditRecord {
    let organization = organization_id();
    let actor_kind = AuditActorKind::User;
    let actor_id = Some("dana".to_string());
    let action = "repository.register".to_string();
    let target_kind = "repository".to_string();
    let target_id = repository_id().to_string();
    let action_digest = digest("repository.register");
    let correlation = correlation_id();
    let detail = json!({ "display_name": "hello-world" });
    let occurred_at = sentinel_time();
    let record_hash = AuditRecord::compute_hash(
        &organization,
        actor_kind,
        actor_id.as_deref(),
        &action,
        &target_kind,
        &target_id,
        &action_digest,
        Some(&correlation),
        None,
        &detail,
        &occurred_at,
    );
    AuditRecord {
        id: audit_record_id(),
        organization_id: organization,
        actor_kind,
        actor_id,
        action,
        target_kind,
        target_id,
        action_digest,
        correlation_id: Some(correlation),
        prev_hash: None,
        record_hash,
        detail,
        occurred_at,
    }
}

fn audit_vectors() -> Vec<Vector> {
    vec![
        vec_of("AuditActorKind_User", AuditActorKind::User),
        vec_of("AuditActorKind_Daemon", AuditActorKind::Daemon),
        vec_of("AuditActorKind_System", AuditActorKind::System),
        vec_of("AuditActorKind_Unknown", AuditActorKind::Unknown),
        vec_of("AuditRecord", audit_record()),
        vec_of(
            "AuditQuery",
            AuditQuery {
                actor_id: Some("dana".to_string()),
                action: Some("repository.register".to_string()),
                target_kind: Some("repository".to_string()),
                target_id: Some(repository_id().to_string()),
                from_time: Some(sentinel_time()),
                to_time: Some(sentinel_time_later()),
                cursor: Some(page_cursor()),
                limit: Some(50),
            },
        ),
        vec_of("AuditQuery_empty", AuditQuery::default()),
    ]
}

// ---------------------------------------------------------------------------
// events.rs
// ---------------------------------------------------------------------------

fn events_vectors() -> Vec<Vector> {
    vec![
        vec_of("StreamKind_Notifications", StreamKind::Notifications),
        vec_of("StreamKind_Approvals", StreamKind::Approvals),
        vec_of("StreamKind_Schedules", StreamKind::Schedules),
        vec_of("StreamKind_RunnerEvents", StreamKind::RunnerEvents),
        vec_of("StreamKind_Policy", StreamKind::Policy),
        vec_of("StreamKind_Sessions", StreamKind::Sessions),
        vec_of("StreamKind_Sync", StreamKind::Sync),
        vec_of("StreamKind_Unknown", StreamKind::Unknown),
        vec_of(
            "NotificationEvent",
            NotificationEvent {
                id: "notification-1".to_string(),
                title: "Run finished".to_string(),
                body: "The nightly repair run completed.".to_string(),
                category: "run".to_string(),
                read: false,
            },
        ),
        vec_of(
            "ApprovalRequestEvent",
            ApprovalRequestEvent {
                approval_id: "approval-1".to_string(),
                repository_id: repository_id(),
                requested_action: "ExecuteCommand".to_string(),
                action_digest: digest("cargo test --all-features"),
                risk_level: "high".to_string(),
            },
        ),
        vec_of(
            "ScheduleTriggerEvent",
            ScheduleTriggerEvent {
                schedule_id: "schedule-1".to_string(),
                scheduled_time: sentinel_time(),
                target: "nightly-repair".to_string(),
            },
        ),
        vec_of(
            "RunnerStatusEvent",
            RunnerStatusEvent {
                job_id: runner_job_id(),
                status: "executing".to_string(),
                attempt: 1,
                details: Some("claimed by runner-1".to_string()),
            },
        ),
        vec_of(
            "PolicyUpdateEvent",
            PolicyUpdateEvent {
                policy_version: 8,
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
        ),
        vec_of(
            "SyncDeltaEvent",
            SyncDeltaEvent {
                delta_kind: SyncDeltaKind::SessionSummary,
                subject_id: "session-1".to_string(),
                class: PublicationClass::MetadataShared,
                payload: json!({ "state": "running" }),
            },
        ),
        vec_of(
            "StreamEventPayload_Notification",
            StreamEventPayload::Notification(NotificationEvent {
                id: "notification-1".to_string(),
                title: "Run finished".to_string(),
                body: "The nightly repair run completed.".to_string(),
                category: "run".to_string(),
                read: false,
            }),
        ),
        vec_of(
            "StreamEventPayload_ApprovalRequest",
            StreamEventPayload::ApprovalRequest(ApprovalRequestEvent {
                approval_id: "approval-1".to_string(),
                repository_id: repository_id(),
                requested_action: "ExecuteCommand".to_string(),
                action_digest: digest("cargo test --all-features"),
                risk_level: "high".to_string(),
            }),
        ),
        vec_of(
            "StreamEventPayload_ScheduleTrigger",
            StreamEventPayload::ScheduleTrigger(ScheduleTriggerEvent {
                schedule_id: "schedule-1".to_string(),
                scheduled_time: sentinel_time(),
                target: "nightly-repair".to_string(),
            }),
        ),
        vec_of(
            "StreamEventPayload_RunnerStatus",
            StreamEventPayload::RunnerStatus(RunnerStatusEvent {
                job_id: runner_job_id(),
                status: "executing".to_string(),
                attempt: 1,
                details: None,
            }),
        ),
        vec_of(
            "StreamEventPayload_PolicyUpdate",
            StreamEventPayload::PolicyUpdate(PolicyUpdateEvent {
                policy_version: 8,
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            }),
        ),
        vec_of(
            "StreamEventPayload_SyncDelta",
            StreamEventPayload::SyncDelta(SyncDeltaEvent {
                delta_kind: SyncDeltaKind::SessionSummary,
                subject_id: "session-1".to_string(),
                class: PublicationClass::MetadataShared,
                payload: json!({ "state": "running" }),
            }),
        ),
        vec_of(
            // The forward-compatibility shape: a payload kind a newer control
            // plane emits folds to `Unknown` instead of failing the stream, and
            // a consumer must never infer an effect from it.
            "StreamEventPayload_Unknown",
            StreamEventPayload::Unknown,
        ),
        vec_of(
            "StreamEvent",
            StreamEvent {
                id: 128,
                organization_id: organization_id(),
                repository_id: Some(repository_id()),
                stream: StreamKind::Notifications,
                payload: StreamEventPayload::Notification(NotificationEvent {
                    id: "notification-1".to_string(),
                    title: "Run finished".to_string(),
                    body: "The nightly repair run completed.".to_string(),
                    category: "run".to_string(),
                    read: false,
                }),
                created_at: sentinel_time(),
            },
        ),
        vec_of(
            "StreamSubscribeRequest",
            StreamSubscribeRequest {
                stream: StreamKind::Approvals,
                from_cursor: Some(128),
                repository_id: Some(repository_id()),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// object_storage.rs
// ---------------------------------------------------------------------------

fn header_map() -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert(
        "x-amz-meta-class".to_string(),
        "metadata-shared".to_string(),
    );
    headers
}

fn object_storage_vectors() -> Vec<Vector> {
    vec![
        vec_of("ObjectState_Uploading", ObjectState::Uploading),
        vec_of("ObjectState_Available", ObjectState::Available),
        vec_of("ObjectState_Tombstoned", ObjectState::Tombstoned),
        vec_of("ObjectState_Unknown", ObjectState::Unknown),
        vec_of("ObjectEncryption_None", ObjectEncryption::None),
        vec_of("ObjectEncryption_Envelope", ObjectEncryption::Envelope),
        vec_of("ObjectEncryption_Unknown", ObjectEncryption::Unknown),
        vec_of(
            "PublishedObject",
            PublishedObject {
                id: published_object_id(),
                organization_id: organization_id(),
                repository_id: Some(repository_id()),
                content_hash: digest("object-bytes"),
                byte_length: 4096,
                media_type: "application/json".to_string(),
                class: PublicationClass::MetadataShared,
                encryption: ObjectEncryption::Envelope,
                state: ObjectState::Available,
                uploaded_by_daemon: Some(daemon_id()),
                created_at: sentinel_time(),
            },
        ),
        vec_of(
            "PresignedUploadRequest",
            PresignedUploadRequest {
                repository_id: Some(repository_id()),
                content_hash: digest("object-bytes"),
                byte_length: 4096,
                media_type: "application/json".to_string(),
                class: PublicationClass::MetadataShared,
                encryption: ObjectEncryption::Envelope,
            },
        ),
        vec_of(
            "PresignedUploadResponse",
            PresignedUploadResponse {
                object_id: published_object_id(),
                upload_url: "https://objects.example.com/upload/1".to_string(),
                headers: header_map(),
                expires_at: sentinel_time_later(),
            },
        ),
        vec_of(
            "CompleteUploadRequest",
            CompleteUploadRequest {
                object_id: published_object_id(),
                content_hash: digest("object-bytes"),
                actual_byte_length: 4096,
            },
        ),
        vec_of(
            "PresignedDownloadRequest",
            PresignedDownloadRequest {
                object_id: published_object_id(),
            },
        ),
        vec_of(
            "PresignedDownloadResponse",
            PresignedDownloadResponse {
                download_url: "https://objects.example.com/download/1".to_string(),
                headers: header_map(),
                byte_length: 4096,
                media_type: "application/json".to_string(),
                content_hash: digest("object-bytes"),
                expires_at: sentinel_time_later(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// workload.rs
// ---------------------------------------------------------------------------

fn workload_vectors() -> Vec<Vector> {
    vec![
        vec_of("CredentialPurpose_Sync", CredentialPurpose::Sync),
        vec_of("CredentialPurpose_Pairing", CredentialPurpose::Pairing),
        vec_of("CredentialPurpose_RunnerJob", CredentialPurpose::RunnerJob),
        vec_of("CredentialPurpose_Unknown", CredentialPurpose::Unknown),
        vec_of(
            "WorkloadCredential",
            WorkloadCredential {
                id: workload_credential_id(),
                daemon_id: Some(daemon_id()),
                audience: "https://control.example.com/v1/sync".to_string(),
                purpose: CredentialPurpose::Sync,
                issued_at: sentinel_time(),
                expires_at: sentinel_time_later(),
                revoked_at: None,
            },
        ),
        vec_of(
            "ServiceAccountToken",
            ServiceAccountToken {
                token: "workload-token-1".to_string(),
                token_type: "Bearer".to_string(),
                purpose: CredentialPurpose::RunnerJob,
                expires_at: sentinel_time_later(),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// error.rs
// ---------------------------------------------------------------------------

fn error_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            // Unauthorized and absent MUST be indistinguishable: both routes
            // return exactly this body. Two vectors, byte-identical apart from
            // nothing at all — that is the point, and a future change that adds
            // a distinguishing field to either breaks both at once.
            "ControlPlaneError_not_found_absent",
            ControlPlaneError::not_found("repository", "repository not found"),
        ),
        vec_of(
            "ControlPlaneError_not_found_unauthorized",
            ControlPlaneError::not_found("repository", "repository not found"),
        ),
        vec_of(
            "ControlPlaneError_unauthorized",
            ControlPlaneError::unauthorized("authentication required"),
        ),
        vec_of(
            "ControlPlaneError_forbidden",
            ControlPlaneError::forbidden("insufficient privileges"),
        ),
        vec_of(
            "ControlPlaneError_validation",
            ControlPlaneError::validation("slug must be lowercase"),
        ),
        vec_of(
            "ControlPlaneError_conflict",
            ControlPlaneError::conflict("idempotency key already used with a different body"),
        ),
        vec_of(
            "ControlPlaneError_rate_limited",
            ControlPlaneError::rate_limited("too many requests"),
        ),
        vec_of(
            "ControlPlaneError_revoked",
            ControlPlaneError::revoked("daemon pairing revoked"),
        ),
        vec_of(
            "ControlPlaneError_internal",
            ControlPlaneError::internal("internal error"),
        ),
        vec_of(
            "ControlPlaneError_with_detail",
            ControlPlaneError {
                error_type: "validation_error".to_string(),
                resource: Some("organization".to_string()),
                message: "slug must be lowercase".to_string(),
                code: Some("INVALID_SLUG".to_string()),
                detail: Some(json!({ "field": "slug" })),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// runner.rs — split across three files by lifecycle stage.
// ---------------------------------------------------------------------------

fn runner_capabilities() -> RunnerCapabilities {
    let mut tools = BTreeMap::new();
    tools.insert("cargo".to_string(), "1.90.0".to_string());
    tools.insert("git".to_string(), "2.48.0".to_string());
    let mut metadata = BTreeMap::new();
    metadata.insert("pool".to_string(), "default".to_string());
    RunnerCapabilities {
        tools,
        image_digest: Some(format!("sha256:{}", digest("runner-image").as_str())),
        policy_labels: vec!["trusted".to_string()],
        max_concurrency: 4,
        metadata,
    }
}

fn runner_vectors() -> Vec<Vector> {
    vec![
        vec_of("RunnerKind_Container", RunnerKind::Container),
        vec_of("RunnerKind_Kubernetes", RunnerKind::Kubernetes),
        vec_of("RunnerKind_Microvm", RunnerKind::Microvm),
        vec_of("RunnerKind_Macos", RunnerKind::Macos),
        vec_of("RunnerKind_Unknown", RunnerKind::Unknown),
        vec_of("SandboxBackend_Seatbelt", SandboxBackend::Seatbelt),
        vec_of("SandboxBackend_Bubblewrap", SandboxBackend::Bubblewrap),
        vec_of("SandboxBackend_None", SandboxBackend::None),
        vec_of("SandboxBackend_Unknown", SandboxBackend::Unknown),
        vec_of("RunnerStatus_Online", RunnerStatus::Online),
        vec_of("RunnerStatus_Idle", RunnerStatus::Idle),
        vec_of("RunnerStatus_Busy", RunnerStatus::Busy),
        vec_of("RunnerStatus_Draining", RunnerStatus::Draining),
        vec_of("RunnerStatus_Offline", RunnerStatus::Offline),
        vec_of("RunnerStatus_Revoked", RunnerStatus::Revoked),
        vec_of("RunnerStatus_Unknown", RunnerStatus::Unknown),
        vec_of("RunnerCapabilities", runner_capabilities()),
        vec_of("RunnerCapabilities_empty", RunnerCapabilities::default()),
        vec_of(
            "RunnerRegistration",
            RunnerRegistration {
                runner_id: runner_id(),
                organization_id: organization_id(),
                name: "linux-x86-1".to_string(),
                kind: RunnerKind::Container,
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                sandbox_backend: SandboxBackend::Bubblewrap,
                capabilities: runner_capabilities(),
                region: Some("eu-west-1".to_string()),
                attestation_pubkey: hex::encode([7u8; 32]),
                max_concurrency: 4,
                tags: vec!["gpu-less".to_string()],
                status: RunnerStatus::Online,
                registered_at: Some(sentinel_time()),
                last_seen_at: Some(sentinel_time_later()),
            },
        ),
        vec_of(
            "RunnerMetrics",
            RunnerMetrics {
                active_leases: 2,
                cpu_usage_pct: Some(37),
                memory_used_mb: Some(2048),
            },
        ),
        vec_of(
            // Absent measurements stay ABSENT: a runner that did not sample cpu
            // or memory reports `null`, never 0 — a 0 would be read as an idle
            // machine by every downstream scheduler.
            "RunnerMetrics_unmeasured",
            RunnerMetrics {
                active_leases: 0,
                cpu_usage_pct: None,
                memory_used_mb: None,
            },
        ),
        vec_of(
            "RunnerHeartbeat",
            RunnerHeartbeat {
                lease_id: runner_lease_id(),
                attempt_id: Some(runner_attempt_id()),
                runner_id: runner_id(),
                generation: 3,
                lease_token: "lease-token-1".to_string(),
                timestamp: sentinel_time(),
                metrics: Some(RunnerMetrics {
                    active_leases: 2,
                    cpu_usage_pct: Some(37),
                    memory_used_mb: Some(2048),
                }),
            },
        ),
        vec_of(
            "HeartbeatResponse",
            HeartbeatResponse {
                lease_id: runner_lease_id(),
                new_generation: 4,
                expires_at: sentinel_time_later(),
                cancel_requested: false,
            },
        ),
        vec_of(
            "RunnerQuarantineReason_AttestationInvalid",
            RunnerQuarantineReason::AttestationInvalid,
        ),
        vec_of(
            "RunnerQuarantineReason_HashMismatch",
            RunnerQuarantineReason::HashMismatch,
        ),
        vec_of(
            "RunnerQuarantineReason_UndeclaredOutput",
            RunnerQuarantineReason::UndeclaredOutput,
        ),
        vec_of(
            "RunnerQuarantineReason_RevokedImage",
            RunnerQuarantineReason::RevokedImage,
        ),
        vec_of(
            "RunnerQuarantineReason_RevokedKey",
            RunnerQuarantineReason::RevokedKey,
        ),
        vec_of(
            "RunnerQuarantineReason_LeaseMismatch",
            RunnerQuarantineReason::LeaseMismatch,
        ),
        vec_of(
            "RunnerQuarantineReason_Oversized",
            RunnerQuarantineReason::Oversized,
        ),
        vec_of(
            "RunnerQuarantineReason_Unknown",
            RunnerQuarantineReason::Unknown,
        ),
    ]
}

fn sandbox_spec() -> SandboxSpec {
    SandboxSpec {
        write_paths: vec!["/workspace/out".to_string()],
        read_paths: vec!["/workspace".to_string()],
        env_allowlist: vec!["PATH".to_string(), "RUST_BACKTRACE".to_string()],
        brokered_secrets: vec!["github-token".to_string()],
        allow_subprocess: true,
        memory_mb: 4096,
        cpu_seconds: 900,
        wall_seconds: 1800,
        maximum_output_mb: 64,
        network_allowlist: vec!["api.github.com".to_string()],
    }
}

fn resource_spec() -> ResourceSpec {
    ResourceSpec {
        cpu_cores: 4,
        memory_mb: 8192,
        disk_mb: 32768,
        wall_time_secs: 1800,
    }
}

fn output_declaration() -> OutputDeclaration {
    OutputDeclaration {
        name: "junit.xml".to_string(),
        media_type: "application/xml".to_string(),
        optional: false,
    }
}

fn output_registration() -> OutputRegistration {
    OutputRegistration {
        attempt_id: runner_attempt_id(),
        name: "junit.xml".to_string(),
        content_hash: digest("junit-output").0,
        byte_length: 2048,
        media_type: "application/xml".to_string(),
        object_key: "org/acme/attempts/1/junit.xml".to_string(),
        classification: "internal".to_string(),
    }
}

fn job_spec() -> JobSpec {
    let mut env = BTreeMap::new();
    env.insert("RUST_BACKTRACE".to_string(), "1".to_string());
    JobSpec {
        argv: vec![
            "cargo".to_string(),
            "test".to_string(),
            "--all-features".to_string(),
        ],
        env,
        working_dir: Some("/workspace".to_string()),
        workspace_layout: Some("checkout".to_string()),
        input_manifest_ref: digest("input-manifest").0,
        sandbox: sandbox_spec(),
        resource: resource_spec(),
        outputs: vec![output_declaration()],
        max_attempts: 3,
    }
}

fn job_lease() -> JobLease {
    JobLease {
        lease_id: runner_lease_id(),
        job_id: runner_job_id(),
        attempt_id: runner_attempt_id(),
        attempt_number: 1,
        runner_id: runner_id(),
        generation: 3,
        lease_token: "lease-token-1".to_string(),
        acquired_at: sentinel_time(),
        expires_at: sentinel_time_later(),
        job_spec: job_spec(),
        job_spec_hash: digest("job-spec").0,
        input_manifest_hash: digest("input-manifest").0,
        data_classification: "internal".to_string(),
        budget_micro_usd: Some(500_000),
    }
}

fn log_chunk() -> LogChunk {
    LogChunk {
        attempt_id: runner_attempt_id(),
        sequence: 1,
        stream: LogStream::Stdout,
        body: Some(b"running 3 tests\n".to_vec()),
        object_key: None,
        byte_length: 16,
        truncated: false,
        received_at: Some(sentinel_time()),
    }
}

fn job_vectors() -> Vec<Vector> {
    vec![
        vec_of(
            "JobClaimRequest",
            JobClaimRequest {
                runner_id: runner_id(),
                organization_id: organization_id(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                sandbox_backend: SandboxBackend::Bubblewrap,
                capabilities: runner_capabilities(),
                region: Some("eu-west-1".to_string()),
                max_jobs: Some(2),
            },
        ),
        vec_of(
            "JobClaimResponse_lease_granted",
            JobClaimResponse {
                lease: Some(job_lease()),
            },
        ),
        vec_of(
            // No work available is `null`, never a synthetic empty lease.
            "JobClaimResponse_no_work",
            JobClaimResponse { lease: None },
        ),
        vec_of("JobLease", job_lease()),
        vec_of("JobSpec", job_spec()),
        vec_of("SandboxSpec", sandbox_spec()),
        vec_of("SandboxSpec_empty", SandboxSpec::default()),
        vec_of("ResourceSpec", resource_spec()),
        vec_of("OutputDeclaration", output_declaration()),
        vec_of("OutputRegistration", output_registration()),
        vec_of("LogStream_Stdout", LogStream::Stdout),
        vec_of("LogStream_Stderr", LogStream::Stderr),
        vec_of("LogStream_Unknown", LogStream::Unknown),
        vec_of("LogChunk", log_chunk()),
        vec_of(
            "LogChunk_offloaded_to_object_storage",
            LogChunk {
                attempt_id: runner_attempt_id(),
                sequence: 2,
                stream: LogStream::Stderr,
                body: None,
                object_key: Some("org/acme/attempts/1/stderr.2.log".to_string()),
                byte_length: 1_048_576,
                truncated: true,
                received_at: Some(sentinel_time_later()),
            },
        ),
        vec_of("RunnerAttemptState_Claimed", RunnerAttemptState::Claimed),
        vec_of(
            "RunnerAttemptState_Executing",
            RunnerAttemptState::Executing,
        ),
        vec_of(
            "RunnerAttemptState_Uploading",
            RunnerAttemptState::Uploading,
        ),
        vec_of("RunnerAttemptState_Verified", RunnerAttemptState::Verified),
        vec_of("RunnerAttemptState_Rejected", RunnerAttemptState::Rejected),
        vec_of("RunnerAttemptState_Expired", RunnerAttemptState::Expired),
        vec_of(
            "RunnerAttemptState_Cancelled",
            RunnerAttemptState::Cancelled,
        ),
        vec_of("RunnerAttemptState_Unknown", RunnerAttemptState::Unknown),
        vec_of("JobTerminalState_Succeeded", JobTerminalState::Succeeded),
        vec_of("JobTerminalState_Failed", JobTerminalState::Failed),
        vec_of("JobTerminalState_Cancelled", JobTerminalState::Cancelled),
        vec_of(
            "JobTerminalState_Quarantined",
            JobTerminalState::Quarantined,
        ),
        vec_of("JobTerminalState_Unknown", JobTerminalState::Unknown),
        vec_of(
            "JobExecutionEventKind_Log",
            JobExecutionEventKind::Log(log_chunk()),
        ),
        vec_of(
            "JobExecutionEventKind_StatusUpdate",
            JobExecutionEventKind::StatusUpdate {
                state: RunnerAttemptState::Executing,
                detail: Some("compiling".to_string()),
            },
        ),
        vec_of(
            "JobExecutionEventKind_OutputDeclared",
            JobExecutionEventKind::OutputDeclared(output_registration()),
        ),
        vec_of(
            "JobExecutionEventKind_Finished",
            JobExecutionEventKind::Finished {
                exit_code: Some(0),
                result: JobTerminalState::Succeeded,
            },
        ),
        vec_of(
            // A process killed by a signal has NO exit code. Absent, not 0 —
            // 0 is "succeeded" to every reader of this field.
            "JobExecutionEventKind_Finished_without_an_exit_code",
            JobExecutionEventKind::Finished {
                exit_code: None,
                result: JobTerminalState::Failed,
            },
        ),
        vec_of(
            // Forward compatibility: an execution-event kind a newer runner
            // emits must fold to `Unknown` rather than fail the whole frame,
            // and `Unknown` must never be read as a terminal success.
            "JobExecutionEventKind_Unknown",
            JobExecutionEventKind::Unknown,
        ),
        vec_of(
            "JobExecutionEvent",
            JobExecutionEvent {
                lease_id: runner_lease_id(),
                attempt_id: runner_attempt_id(),
                sequence: 7,
                timestamp: sentinel_time(),
                kind: JobExecutionEventKind::StatusUpdate {
                    state: RunnerAttemptState::Executing,
                    detail: None,
                },
            },
        ),
        vec_of(
            "JobCancellation",
            JobCancellation {
                job_id: runner_job_id(),
                lease_id: Some(runner_lease_id()),
                reason: "superseded".to_string(),
                requested_at: sentinel_time(),
                requested_by: Some(user_id()),
                force: false,
            },
        ),
        vec_of(
            "JobCancellationResponse",
            JobCancellationResponse {
                job_id: runner_job_id(),
                cancelled: true,
                current_state: "cancelled".to_string(),
            },
        ),
    ]
}

fn attestation_statement() -> RunnerAttestationStatement {
    RunnerAttestationStatement {
        job_id: runner_job_id(),
        job_spec_hash: digest("job-spec").0,
        attempt_id: runner_attempt_id(),
        attempt_number: 1,
        lease_id: runner_lease_id(),
        lease_generation: 3,
        runner_id: runner_id(),
        image_digest: format!("sha256:{}", digest("runner-image").as_str()),
        input_manifest_hash: digest("input-manifest").0,
        outputs: vec![RunnerAttestationOutput {
            name: "junit.xml".to_string(),
            content_hash: digest("junit-output").0,
            byte_length: 2048,
        }],
        started_at: sentinel_time().to_rfc3339(),
        ended_at: sentinel_time_later().to_rfc3339(),
        exit_code: Some(0),
        result: "succeeded".to_string(),
    }
}

fn attestation_vectors() -> Vec<Vector> {
    // `ed25519-dalek` is pinned with default features, so `SigningKey::generate`
    // is unavailable: build the key from fixed bytes, which is also what makes
    // the emitted signature deterministic.
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let statement = attestation_statement();
    let signature = statement.sign(&signing_key);
    vec![
        vec_of(
            "RunnerAttestationOutput",
            RunnerAttestationOutput {
                name: "junit.xml".to_string(),
                content_hash: digest("junit-output").0,
                byte_length: 2048,
            },
        ),
        vec_of("RunnerAttestationStatement", statement.clone()),
        vec_of(
            "RunnerAttestationStatement_without_an_exit_code",
            RunnerAttestationStatement {
                exit_code: None,
                result: "failed".to_string(),
                ..attestation_statement()
            },
        ),
        vec_of(
            "RunnerAttestationSubmission",
            RunnerAttestationSubmission {
                attempt_id: runner_attempt_id(),
                job_id: runner_job_id(),
                lease_id: runner_lease_id(),
                runner_id: runner_id(),
                scheme: ATTESTATION_SCHEME_V1.to_string(),
                statement,
                signature: hex::encode(signature.to_bytes()),
                signer_pubkey: hex::encode(signing_key.verifying_key().to_bytes()),
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// The single source of truth both the regenerator and the checks iterate.
// Paths are `/`-separated relative to `protocol-vectors/`.
// ---------------------------------------------------------------------------

fn all_files() -> Vec<(&'static str, Vec<Vector>)> {
    vec![
        ("control-plane/ids.json", ids_vectors()),
        ("control-plane/version.json", version_vectors()),
        ("control-plane/page.json", page_vectors()),
        ("control-plane/publication.json", publication_vectors()),
        ("control-plane/organization.json", organization_vectors()),
        ("control-plane/workspace.json", workspace_vectors()),
        ("control-plane/repository.json", repository_vectors()),
        ("control-plane/user.json", user_vectors()),
        ("control-plane/identity.json", identity_vectors()),
        ("control-plane/auth.json", auth_vectors()),
        ("control-plane/daemon.json", daemon_vectors()),
        ("control-plane/rbac.json", rbac_vectors()),
        ("control-plane/sync.json", sync_vectors()),
        ("control-plane/audit.json", audit_vectors()),
        ("control-plane/events.json", events_vectors()),
        (
            "control-plane/object_storage.json",
            object_storage_vectors(),
        ),
        ("control-plane/workload.json", workload_vectors()),
        ("control-plane/error.json", error_vectors()),
        ("runner/runner.json", runner_vectors()),
        ("runner/job.json", job_vectors()),
        ("runner/attestation.json", attestation_vectors()),
    ]
}

/// Types this crate declares that deliberately have NO golden vector, each with
/// the reason. Every one of these is a local `thiserror` error type: it is
/// returned by a validation or verification function and never serialized onto
/// the wire (none of them derive `Serialize`). Anything else added here needs a
/// reason of the same weight.
const TYPES_WITHOUT_VECTORS: &[&str] = &[
    "AuditChainError",
    "AttestationVerificationError",
    "CursorDecodeError",
    "IdValidationError",
    "SlugValidationError",
    "VersionParseError",
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Regenerate every committed vector file from the CURRENT protocol types.
/// Never runs in CI (see the module doc); run it explicitly after a wire
/// change:
///
/// ```text
/// cargo test -p codypendent-control-plane-protocol --test golden_vectors regenerate_vectors -- --ignored
/// ```
#[test]
#[ignore = "writes committed vector files; run explicitly to regenerate them"]
fn regenerate_vectors() {
    let dir = vectors_dir();
    for (relative, vectors) in all_files() {
        let path = dir.join(relative);
        let parent = path.parent().expect("vector path has a parent directory");
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
        let text = render(&manifest_value(&vectors));
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}

/// CI gate #1: every committed vector file equals a fresh regeneration
/// byte-for-byte. A wire change (new field, new variant, a changed sentinel)
/// that is not paired with running the regenerator FAILS here.
#[test]
fn committed_vectors_match_current_protocol_types() {
    let dir = vectors_dir();
    for (relative, vectors) in all_files() {
        let path = dir.join(relative);
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: {e}\n\nrun `cargo test -p codypendent-control-plane-protocol --test golden_vectors regenerate_vectors -- --ignored`, \
                 review the diff under protocol-vectors/, and commit it",
                path.display()
            )
        });
        let fresh = render(&manifest_value(&vectors));
        assert_eq!(
            committed,
            fresh,
            "{} is stale relative to the current protocol types.\n\
             Run `cargo test -p codypendent-control-plane-protocol --test golden_vectors regenerate_vectors -- --ignored`, \
             review the diff, and commit it.",
            path.display()
        );
    }
}

/// CI gate #2: every committed entry, deserialized through its own concrete
/// Rust type and re-serialized, reproduces itself exactly. Reads the vectors
/// straight off disk (not the in-memory values above) so a hand-edited file is
/// caught even if gate #1 were ever bypassed.
#[test]
fn committed_vectors_round_trip_through_their_rust_types() {
    let dir = vectors_dir();
    for (relative, vectors) in all_files() {
        let path = dir.join(relative);
        let committed_text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let committed: Value = serde_json::from_str(&committed_text)
            .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
        let committed_map = committed
            .as_object()
            .unwrap_or_else(|| panic!("{} is not a JSON object", path.display()));
        for vector in &vectors {
            let entry = committed_map.get(vector.name).unwrap_or_else(|| {
                panic!(
                    "{} has no entry named {:?} — run the regeneration command",
                    path.display(),
                    vector.name
                )
            });
            let reserialized = (vector.round_trip)(entry);
            assert_eq!(
                &reserialized, entry,
                "{}::{} does not round-trip through its Rust type unchanged — the wire shape \
                 changed; regenerate the vectors",
                relative, vector.name
            );
        }
    }
}

/// Extract every type name this crate declares: `pub struct X`, `pub enum X`,
/// and the ids generated by the `uuid_id!` macro (which a naive `pub struct`
/// scan cannot see, because the macro writes the declaration).
fn declared_type_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let dir = source_dir();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("read source directory entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut inside_uuid_macro = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if inside_uuid_macro {
                if trimmed == ");" {
                    inside_uuid_macro = false;
                } else if !trimmed.starts_with("///") && !trimmed.is_empty() {
                    names.insert(trimmed.trim_end_matches(',').to_string());
                }
                continue;
            }
            if trimmed == "uuid_id!(" {
                inside_uuid_macro = true;
                continue;
            }
            for prefix in ["pub struct ", "pub enum "] {
                if let Some(rest) = trimmed.strip_prefix(prefix) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        names.insert(name);
                    }
                }
            }
        }
    }
    names
}

/// The partition guard. Two committed-vector files that pass gates #1 and #2
/// still prove nothing about a type nobody remembered to add, so this reads the
/// crate's own source at test time and asserts every declared wire type is
/// either covered by at least one vector or named in [`TYPES_WITHOUT_VECTORS`]
/// with a reason. It also fails on a stale exclusion, so a type that is deleted
/// or renamed cannot leave a phantom entry behind that silently excuses a
/// future type of the same name.
#[test]
fn every_wire_type_has_a_golden_vector() {
    let mut covered: BTreeSet<String> = BTreeSet::new();
    for (_, vectors) in all_files() {
        for vector in vectors {
            // `"RunnerKind_Container"` covers `RunnerKind`; `"JobSpec"` covers
            // `JobSpec`. Register both readings so either naming works.
            covered.insert(vector.name.to_string());
            if let Some((family, _)) = vector.name.split_once('_') {
                covered.insert(family.to_string());
            }
        }
    }

    let declared = declared_type_names();
    assert!(
        declared.len() > 50,
        "the source scan found only {} type names — it stopped working, and a broken scan \
         passes vacuously",
        declared.len()
    );

    let excluded: BTreeSet<String> = TYPES_WITHOUT_VECTORS
        .iter()
        .map(|s| s.to_string())
        .collect();

    let uncovered: Vec<&String> = declared
        .iter()
        .filter(|name| !covered.contains(*name) && !excluded.contains(*name))
        .collect();
    assert!(
        uncovered.is_empty(),
        "wire type(s) with no golden vector and no entry in TYPES_WITHOUT_VECTORS: {uncovered:?}\n\
         Add a vector in crates/control-plane-protocol/tests/golden_vectors.rs (then regenerate), \
         or list the type in TYPES_WITHOUT_VECTORS with the reason it never crosses the wire."
    );

    let phantom: Vec<&&str> = TYPES_WITHOUT_VECTORS
        .iter()
        .filter(|name| !declared.contains(**name))
        .collect();
    assert!(
        phantom.is_empty(),
        "TYPES_WITHOUT_VECTORS names type(s) this crate no longer declares: {phantom:?} — \
         remove them, or a future type reusing the name is excused silently"
    );
}

/// The one rule the vectors themselves cannot state: an unrecognized tag from a
/// newer peer must decode to `Unknown`, and `Unknown` must be the most
/// restrictive reading. `wire_contract.rs` covers the older enums; this covers
/// the runner execution-event kind, whose `Unknown` variant did not exist until
/// these vectors were written.
///
/// It also pins the LIMIT of that arm, which is real and must not be
/// misremembered as full forward compatibility. `JobExecutionEventKind` is
/// **adjacently** tagged (`tag = "type", content = "data"`), and serde's
/// adjacently-tagged deserializer, once it routes an unrecognized tag to the
/// `#[serde(other)]` unit variant, still tries to deserialize whatever sits in
/// `data` INTO that unit variant. A newer kind that carries a payload — which
/// every existing variant does — therefore still fails to deserialize. That is
/// fail-closed (nothing is inferred, no effect is recorded) but it is not
/// graceful: it takes the enclosing `JobExecutionEvent` down with it. Closing
/// that gap needs a hand-written `Deserialize` for this enum, which is tracked
/// rather than done here.
#[test]
fn an_unrecognized_execution_event_kind_decodes_to_unknown_and_is_never_terminal_success() {
    let kind: JobExecutionEventKind = serde_json::from_value(json!({ "type": "teleported" }))
        .expect("an unknown payload-free execution event kind must not fail the frame");
    assert_eq!(kind, JobExecutionEventKind::Unknown);

    let event: JobExecutionEvent = serde_json::from_value(json!({
        "lease_id": runner_lease_id(),
        "attempt_id": runner_attempt_id(),
        "sequence": 9,
        "timestamp": sentinel_time(),
        "kind": { "type": "teleported" },
    }))
    .expect("an unknown execution event kind must not fail the enclosing event");
    assert_eq!(event.kind, JobExecutionEventKind::Unknown);

    // `Unknown` carries no state and no terminal result, so no reader can
    // mistake it for a status transition or a finished-successfully event.
    assert!(!matches!(
        event.kind,
        JobExecutionEventKind::Finished { .. } | JobExecutionEventKind::StatusUpdate { .. }
    ));

    // The documented limit: an unknown tag that CARRIES a payload is refused
    // outright rather than folded. Refusal is safe; it is the frame loss that
    // is the residual gap, and this assertion is what will start failing on the
    // day somebody fixes it.
    let carried: Result<JobExecutionEventKind, _> =
        serde_json::from_value(json!({ "type": "teleported", "data": { "anything": true } }));
    assert!(
        carried.is_err(),
        "adjacently-tagged `other` unexpectedly folded a payload-carrying unknown kind — if this \
         now works, delete this assertion and the caveat in the doc comment above"
    );
}

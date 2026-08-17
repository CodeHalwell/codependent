//! Contract tests for the control-plane wire types.
//!
//! Two invariants are worth a test each, because both are the kind that fail silently:
//!
//! 1. **Every enum that crosses the wire decodes an unrecognized tag to `Unknown`, and every
//!    `Unknown` is the most restrictive answer.** A peer on a newer build sends a tag this
//!    build has never heard of; the only safe reading is "deny", never "closest match".
//! 2. **The batched `SyncEnvelope` is the shape `POST /v1/sync/push` must accept.** The
//!    server currently accepts a flat single-delta body, so this test is what a fix to that
//!    route has to satisfy.

use chrono::{TimeZone, Utc};
use codypendent_control_plane_protocol::*;

fn digest() -> Sha256Digest {
    Sha256Digest::from_bytes(b"payload")
}

#[test]
fn unrecognized_publication_class_is_the_most_restrictive_class() {
    let class: PublicationClass = serde_json::from_str("\"quantum-shared\"").unwrap();
    assert_eq!(class, PublicationClass::Unknown);
    assert_eq!(class.rank(), u8::MAX);
    assert!(!class.allows_off_device());
    assert!(!class.permits_in_ceiling(PublicationClass::PublicMarketplace));
    assert!(!PublicationClass::MetadataShared.permits_in_ceiling(class));
    assert_eq!(
        class.intersect(PublicationClass::PublicMarketplace),
        PublicationClass::PrivateLocal
    );
}

#[test]
fn unrecognized_data_classification_is_the_most_restrictive_classification() {
    let classification: DataClassification = serde_json::from_str("\"top-secret\"").unwrap();
    assert_eq!(classification, DataClassification::Unknown);
    assert_eq!(classification.rank(), u8::MAX);
    assert!(!classification.permits(DataClassification::Secret));
    assert_eq!(
        classification.intersect(DataClassification::Public),
        DataClassification::Secret
    );
}

#[test]
fn unrecognized_role_ranks_below_every_named_role_and_permits_nothing() {
    let role: ControlPlaneRole = serde_json::from_str("\"super-admin\"").unwrap();
    assert_eq!(role, ControlPlaneRole::Unknown);
    assert_eq!(role.privilege_rank(), 0);
    assert!(role < ControlPlaneRole::Observer);
    assert!(role < ControlPlaneRole::OrganizationAdmin);
    for action in [
        RbacAction::ReadMetadata,
        RbacAction::ReadContent,
        RbacAction::WriteContent,
        RbacAction::ApproveAction,
        RbacAction::ManageRepositories,
        RbacAction::ManageTeam,
        RbacAction::ManageOrganization,
        RbacAction::DispatchRunner,
        RbacAction::ReadAuditLogs,
    ] {
        assert!(!role.permits(action), "unknown role permitted {action:?}");
    }
}

#[test]
fn unrecognized_action_is_permitted_by_no_role_including_the_admin() {
    let action: RbacAction = serde_json::from_str("\"delete-everything\"").unwrap();
    assert_eq!(action, RbacAction::Unknown);
    assert!(!ControlPlaneRole::OrganizationAdmin.permits(action));
}

#[test]
fn unrecognized_lifecycle_states_are_never_the_permissive_state() {
    let daemon: DaemonState = serde_json::from_str("\"quarantined\"").unwrap();
    assert_eq!(daemon, DaemonState::Unknown);
    assert!(!daemon.is_operational());

    let user: UserState = serde_json::from_str("\"dormant\"").unwrap();
    assert_eq!(user, UserState::Unknown);
    assert!(!user.is_active());

    let membership: MembershipState = serde_json::from_str("\"pending\"").unwrap();
    assert_eq!(membership, MembershipState::Unknown);
    assert!(!membership.is_active());

    let object: ObjectState = serde_json::from_str("\"quarantined\"").unwrap();
    assert_eq!(object, ObjectState::Unknown);
    assert!(!object.is_readable());

    let encryption: ObjectEncryption = serde_json::from_str("\"kms\"").unwrap();
    assert_eq!(encryption, ObjectEncryption::Unknown);

    let session: SharedSessionState = serde_json::from_str("\"paused\"").unwrap();
    assert_eq!(session, SharedSessionState::Unknown);
}

#[test]
fn unrecognized_credential_purpose_authorizes_nothing() {
    let purpose: CredentialPurpose = serde_json::from_str("\"admin\"").unwrap();
    assert_eq!(purpose, CredentialPurpose::Unknown);
    assert!(!purpose.authorizes(CredentialPurpose::Sync));
    assert!(!CredentialPurpose::Sync.authorizes(purpose));
    assert!(!CredentialPurpose::Sync.authorizes(CredentialPurpose::Pairing));
    assert!(CredentialPurpose::Sync.authorizes(CredentialPurpose::Sync));
}

#[test]
fn unrecognized_sync_delta_kind_is_never_projected() {
    let kind: SyncDeltaKind = serde_json::from_str("\"memory-batch\"").unwrap();
    assert_eq!(kind, SyncDeltaKind::Unknown);
    assert!(!kind.is_projectable());
    assert!(SyncDeltaKind::SessionSummary.is_projectable());
}

#[test]
fn unrecognized_runner_facts_fail_closed() {
    let backend: SandboxBackend = serde_json::from_str("\"gvisor\"").unwrap();
    assert_eq!(backend, SandboxBackend::Unknown);
    assert!(!backend.is_enforceable());

    let status: RunnerStatus = serde_json::from_str("\"rebooting\"").unwrap();
    assert_eq!(status, RunnerStatus::Unknown);
    assert!(!status.accepts_work());

    let attempt: RunnerAttemptState = serde_json::from_str("\"reconciling\"").unwrap();
    assert_eq!(attempt, RunnerAttemptState::Unknown);
    assert!(!attempt.is_publishable());

    let terminal: JobTerminalState = serde_json::from_str("\"partially-succeeded\"").unwrap();
    assert_eq!(terminal, JobTerminalState::Unknown);
    assert!(!terminal.is_success());
}

#[test]
fn unrecognized_stream_kind_and_payload_decode_without_failing_the_stream() {
    let kind: StreamKind = serde_json::from_str("\"billing\"").unwrap();
    assert_eq!(kind, StreamKind::Unknown);

    let payload: StreamEventPayload =
        serde_json::from_str(r#"{"type":"billing-alert","amount":10}"#).unwrap();
    assert_eq!(payload, StreamEventPayload::Unknown);
}

#[test]
fn unrecognized_tombstone_reason_and_actor_kind_decode_to_unknown() {
    let reason: TombstoneReason = serde_json::from_str("\"purged\"").unwrap();
    assert_eq!(reason, TombstoneReason::Unknown);

    let actor: AuditActorKind = serde_json::from_str("\"runner\"").unwrap();
    assert_eq!(actor, AuditActorKind::Unknown);

    let provider: IdentityProvider = serde_json::from_str("\"saml\"").unwrap();
    assert_eq!(provider, IdentityProvider::Unknown);
}

#[test]
fn sync_push_body_is_a_batched_envelope_carrying_the_protocol_version() {
    let envelope = SyncEnvelope {
        protocol_version: CONTROL_PLANE_PROTOCOL_V1,
        daemon_id: DaemonId::new(),
        organization_id: OrganizationId::new(),
        sent_at: Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 0).unwrap(),
        deltas: vec![SyncDelta {
            id: "delta-1".to_owned(),
            sequence: 7,
            kind: SyncDeltaKind::SessionSummary,
            repository_id: Some(RepositoryId::new()),
            subject_id: "session-42".to_owned(),
            payload: serde_json::json!({ "state": "running" }),
            class: PublicationClass::MetadataShared,
            payload_hash: digest(),
            created_at: Utc.with_ymd_and_hms(2026, 8, 17, 11, 59, 0).unwrap(),
        }],
    };

    let value = serde_json::to_value(&envelope).unwrap();
    assert!(value.get("protocol_version").is_some());
    assert!(value.get("daemon_id").is_some());
    assert!(value.get("organization_id").is_some());
    let deltas = value.get("deltas").and_then(|d| d.as_array()).unwrap();
    assert_eq!(deltas.len(), 1);
    assert!(deltas[0].get("repository_id").is_some());

    let decoded: SyncEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, envelope);
}

#[test]
fn sync_receipt_reports_the_stored_class_and_replay_status() {
    let receipt = SyncReceipt {
        id: SyncReceiptId::new(),
        daemon_id: DaemonId::new(),
        daemon_sequence: 7,
        delta_kind: SyncDeltaKind::SessionSummary,
        payload_hash: digest(),
        class: PublicationClass::MetadataShared,
        accepted_at: Utc.with_ymd_and_hms(2026, 8, 17, 12, 0, 1).unwrap(),
        duplicate: false,
    };
    let json = serde_json::to_string(&receipt).unwrap();
    assert!(json.contains("\"duplicate\":false"));

    // `duplicate` defaults so an older daemon's receipt still decodes.
    let without_flag = json.replace(",\"duplicate\":false", "");
    let decoded: SyncReceipt = serde_json::from_str(&without_flag).unwrap();
    assert!(!decoded.duplicate);
}

#[test]
fn a_broken_audit_chain_is_detected() {
    let organization_id = OrganizationId::new();
    let occurred_at = Utc.with_ymd_and_hms(2026, 8, 17, 9, 0, 0).unwrap();
    let action_digest = digest();
    let detail = serde_json::json!({});

    let first_hash = AuditRecord::compute_hash(
        &organization_id,
        AuditActorKind::User,
        None,
        "organization.create",
        "organization",
        "org-1",
        &action_digest,
        None,
        None,
        &detail,
        &occurred_at,
    );
    let first = AuditRecord {
        id: AuditRecordId::new(),
        organization_id,
        actor_kind: AuditActorKind::User,
        actor_id: None,
        action: "organization.create".to_owned(),
        target_kind: "organization".to_owned(),
        target_id: "org-1".to_owned(),
        action_digest: action_digest.clone(),
        correlation_id: None,
        prev_hash: None,
        record_hash: first_hash.clone(),
        detail: detail.clone(),
        occurred_at,
    };
    assert!(first.verify_record_hash());
    assert!(verify_audit_chain(std::slice::from_ref(&first)).is_ok());

    let mut tampered = first.clone();
    tampered.target_id = "org-2".to_owned();
    assert!(!tampered.verify_record_hash());
    assert!(verify_audit_chain(&[tampered]).is_err());
}

#[test]
fn control_plane_protocol_version_negotiates_independently_of_the_local_protocol() {
    let server = CONTROL_PLANE_PROTOCOL_V1;
    assert_eq!(server.to_string(), "1.0");
    assert!(server.is_compatible_with(&CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED));
    assert_eq!(
        server.negotiate(&ProtocolVersion::new(1, 4)),
        Some(ProtocolVersion::new(1, 0))
    );
    assert_eq!(server.negotiate(&ProtocolVersion::new(2, 0)), None);
}

#[test]
fn page_envelope_round_trips_with_an_opaque_cursor() {
    let cursor = PageCursor::encode_keyset("qhash", "2026-08-17T00:00:00Z", "row-1");
    let (query_hash, sort_key, row_id) = cursor.decode_keyset().unwrap();
    assert_eq!(query_hash, "qhash");
    assert_eq!(sort_key, "2026-08-17T00:00:00Z");
    assert_eq!(row_id, "row-1");

    let page = Page {
        items: vec![UserSummary {
            id: UserId::new(),
            display_name: "Alice".to_owned(),
            primary_email: None,
            state: UserState::Active,
        }],
        next_cursor: Some(cursor),
        has_more: true,
        total_count: Some(1),
    };
    let decoded: Page<UserSummary> =
        serde_json::from_value(serde_json::to_value(&page).unwrap()).unwrap();
    assert_eq!(decoded, page);
}

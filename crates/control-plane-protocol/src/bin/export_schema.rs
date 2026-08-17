use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::*;
use schemars::gen::SchemaSettings;
use schemars::JsonSchema;
use serde_json::Value;

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ControlPlaneIdCatalog {
    user_id: UserId,
    organization_id: OrganizationId,
    team_id: TeamId,
    workspace_id: WorkspaceId,
    repository_id: RepositoryId,
    daemon_id: DaemonId,
    grant_id: GrantId,
    sync_receipt_id: SyncReceiptId,
    tombstone_id: TombstoneId,
    audit_record_id: AuditRecordId,
    published_object_id: PublishedObjectId,
    identity_id: IdentityId,
    refresh_token_id: RefreshTokenId,
    workload_credential_id: WorkloadCredentialId,
    challenge_id: ChallengeId,
    runner_job_id: RunnerJobId,
    runner_lease_id: RunnerLeaseId,
    shared_session_id: SharedSessionId,
    correlation_id: CorrelationId,
    federated_repository_id: FederatedRepositoryId,
    sha256_digest: Sha256Digest,
    timestamp: DateTime<Utc>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct OrganizationCatalog {
    organization: Organization,
    summary: OrganizationSummary,
    create_request: CreateOrganizationRequest,
    update_request: UpdateOrganizationRequest,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct WorkspaceCatalog {
    team: Team,
    membership_state: MembershipState,
    team_member: TeamMember,
    membership: OrganizationMembership,
    create_team: CreateTeamRequest,
    update_team: UpdateTeamRequest,
    add_member: AddTeamMemberRequest,
    workspace: Workspace,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct RepositoryCatalog {
    repository: Repository,
    summary: RepositorySummary,
    register_request: RegisterRepositoryRequest,
    update_request: UpdateRepositoryRequest,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct UserCatalog {
    user: User,
    state: UserState,
    summary: UserSummary,
    provider: IdentityProvider,
    update_request: UpdateUserRequest,
    identity: UserIdentity,
    link_request: IdentityLinkRequest,
    link_result: IdentityLinkResult,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct DaemonCatalog {
    daemon: Daemon,
    state: DaemonState,
    consent_manifest: ConsentManifest,
    pairing_challenge: PairingChallenge,
    pairing_scope: PairingScope,
    initiate_request: InitiatePairingRequest,
    initiate_response: InitiatePairingResponse,
    exchange_request: ExchangePairingCodeRequest,
    exchange_response: ExchangePairingCodeResponse,
    revoke_request: RevokeDaemonRequest,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct WorkloadCatalog {
    credential: WorkloadCredential,
    purpose: CredentialPurpose,
    service_token: ServiceAccountToken,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct RbacCatalog {
    role: ControlPlaneRole,
    action: RbacAction,
    action_scope: ActionScope,
    grant: RoleGrant,
    create_grant: CreateRoleGrantRequest,
    revoke_grant: RevokeRoleGrantRequest,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct SyncCatalog {
    envelope: SyncEnvelope,
    delta: SyncDelta,
    delta_kind: SyncDeltaKind,
    receipt: SyncReceipt,
    batch_response: SyncBatchResponse,
    rejection: SyncRejection,
    tombstone: Tombstone,
    tombstone_reason: TombstoneReason,
    shared_session: SharedSession,
    shared_session_state: SharedSessionState,
    cursor: SyncCursor,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct AuditCatalog {
    record: AuditRecord,
    query: AuditQuery,
    actor_kind: AuditActorKind,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct VersionCatalog {
    version: ProtocolVersion,
    handshake_request: ProtocolHandshakeRequest,
    handshake_response: ProtocolHandshakeResponse,
    error: ControlPlaneError,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ObjectStorageCatalog {
    object: PublishedObject,
    state: ObjectState,
    encryption: ObjectEncryption,
    presigned_upload_request: PresignedUploadRequest,
    presigned_upload_response: PresignedUploadResponse,
    complete_upload_request: CompleteUploadRequest,
    presigned_download_request: PresignedDownloadRequest,
    presigned_download_response: PresignedDownloadResponse,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct StreamCatalog {
    event: StreamEvent,
    kind: StreamKind,
    payload: StreamEventPayload,
    notification: NotificationEvent,
    approval_request: ApprovalRequestEvent,
    schedule_trigger: ScheduleTriggerEvent,
    runner_status: RunnerStatusEvent,
    policy_update: PolicyUpdateEvent,
    subscribe_request: StreamSubscribeRequest,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct RunnerCatalog {
    registration: RunnerRegistration,
    lease: JobLease,
    job_spec: JobSpec,
    claim_request: JobClaimRequest,
    claim_response: JobClaimResponse,
    heartbeat_request: RunnerHeartbeat,
    heartbeat_response: HeartbeatResponse,
    execution_event: JobExecutionEvent,
    cancellation_request: JobCancellation,
    cancellation_response: JobCancellationResponse,
    attestation_submission: RunnerAttestationSubmission,
    attestation_statement: RunnerAttestationStatement,
    attestation_output: RunnerAttestationOutput,
    capabilities: RunnerCapabilities,
    runner_kind: RunnerKind,
    runner_status: RunnerStatus,
    sandbox_backend: SandboxBackend,
    attempt_state: RunnerAttemptState,
    terminal_state: JobTerminalState,
    quarantine_reason: RunnerQuarantineReason,
    log_chunk: LogChunk,
    log_stream: LogStream,
    metrics: RunnerMetrics,
    output_declaration: OutputDeclaration,
    output_registration: OutputRegistration,
    resource_spec: ResourceSpec,
    sandbox_spec: SandboxSpec,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct PolicyCatalog {
    snapshot: PolicySnapshot,
    restrictions: PolicyRestrictions,
    publication_class: PublicationClass,
    data_classification: DataClassification,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct AuthCatalog {
    oauth_init_request: OAuthInitRequest,
    oauth_init_response: OAuthInitResponse,
    oauth_callback_request: OAuthCallbackRequest,
    auth_token_response: AuthTokenResponse,
    refresh_token_request: RefreshTokenRequest,
    revoke_token_request: RevokeTokenRequest,
}

/// Every paginated response the control plane returns. `Page<T>` is generic, so it is
/// unreachable from any other catalog: each list endpoint must be named here explicitly or
/// a client has no type for the envelope it actually receives.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct PageCatalog {
    cursor: PageCursor,
    request: PageRequest,
    organization_page: Page<OrganizationSummary>,
    repository_page: Page<RepositorySummary>,
    user_page: Page<UserSummary>,
    team_page: Page<Team>,
    team_member_page: Page<TeamMember>,
    membership_page: Page<OrganizationMembership>,
    daemon_page: Page<Daemon>,
    role_grant_page: Page<RoleGrant>,
    shared_session_page: Page<SharedSession>,
    audit_record_page: Page<AuditRecord>,
    published_object_page: Page<PublishedObject>,
    stream_event_page: Page<StreamEvent>,
    sync_receipt_page: Page<SyncReceipt>,
    tombstone_page: Page<Tombstone>,
}

fn usage() -> &'static str {
    "usage: export_control_plane_schema --output-dir <directory>"
}

fn output_directory() -> Result<PathBuf, String> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--output-dir")) {
        return Err(usage().to_owned());
    }
    let output = arguments
        .next()
        .ok_or_else(|| "missing value for --output-dir".to_owned())?;
    if arguments.next().is_some() {
        return Err(format!("unexpected argument\n{}", usage()));
    }
    Ok(output.into())
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}

fn write_schema<T: JsonSchema>(output: &Path, filename: &str) -> Result<(), Box<dyn Error>> {
    let generator = SchemaSettings::draft07().into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let canonical = canonicalize(serde_json::to_value(schema)?);
    let mut rendered = serde_json::to_string_pretty(&canonical)?;
    rendered.push('\n');
    fs::write(output.join(filename), rendered)?;
    Ok(())
}

fn export(output: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(output)?;

    write_schema::<AuditCatalog>(output, "audit-catalog.schema.json")?;
    write_schema::<AuditRecord>(output, "audit-record.schema.json")?;
    write_schema::<AuthCatalog>(output, "auth-catalog.schema.json")?;
    write_schema::<ControlPlaneError>(output, "control-plane-error.schema.json")?;
    write_schema::<ControlPlaneIdCatalog>(output, "control-plane-id-catalog.schema.json")?;
    write_schema::<DaemonCatalog>(output, "daemon-catalog.schema.json")?;
    write_schema::<Daemon>(output, "daemon.schema.json")?;
    write_schema::<ObjectStorageCatalog>(output, "object-storage-catalog.schema.json")?;
    write_schema::<OrganizationCatalog>(output, "organization-catalog.schema.json")?;
    write_schema::<Organization>(output, "organization.schema.json")?;
    write_schema::<PageCatalog>(output, "page-catalog.schema.json")?;
    write_schema::<PolicyCatalog>(output, "policy-catalog.schema.json")?;
    write_schema::<ProtocolHandshakeRequest>(output, "protocol-handshake-request.schema.json")?;
    write_schema::<ProtocolHandshakeResponse>(output, "protocol-handshake-response.schema.json")?;
    write_schema::<ProtocolVersion>(output, "protocol-version.schema.json")?;
    write_schema::<RbacCatalog>(output, "rbac-catalog.schema.json")?;
    write_schema::<RepositoryCatalog>(output, "repository-catalog.schema.json")?;
    write_schema::<Repository>(output, "repository.schema.json")?;
    write_schema::<RunnerCatalog>(output, "runner-catalog.schema.json")?;
    write_schema::<StreamCatalog>(output, "stream-catalog.schema.json")?;
    write_schema::<StreamEvent>(output, "stream-event.schema.json")?;
    write_schema::<SyncCatalog>(output, "sync-catalog.schema.json")?;
    write_schema::<SyncEnvelope>(output, "sync-envelope.schema.json")?;
    write_schema::<UserCatalog>(output, "user-catalog.schema.json")?;
    write_schema::<VersionCatalog>(output, "version-catalog.schema.json")?;
    write_schema::<User>(output, "user.schema.json")?;
    write_schema::<WorkloadCatalog>(output, "workload-catalog.schema.json")?;
    write_schema::<WorkspaceCatalog>(output, "workspace-catalog.schema.json")?;

    Ok(())
}

fn main() {
    let output = output_directory().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    if let Err(error) = export(&output) {
        eprintln!("failed to export control plane protocol schemas: {error}");
        std::process::exit(1);
    }
}

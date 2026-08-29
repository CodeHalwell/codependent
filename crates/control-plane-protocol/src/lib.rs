//! Codypendent Control Plane Protocol: wire types, identifiers, sync envelopes, audit events, and schemas.
//!
//! Provides authoritative Rust definitions for all network contracts used between daemons,
//! runners, clients, and the control plane service.

pub mod audit;
pub mod auth;
pub mod daemon;
pub mod error;
pub mod events;
pub mod identity;
pub mod ids;
pub mod object_storage;
pub mod organization;
pub mod page;
pub mod publication;
pub mod rbac;
pub mod repository;
pub mod runner;
pub mod sync;
pub mod user;
pub mod version;
pub mod workload;
pub mod workspace;

pub use audit::{verify_audit_chain, AuditActorKind, AuditChainError, AuditQuery, AuditRecord};
pub use auth::{
    AuthTokenResponse, OAuthCallbackRequest, OAuthInitRequest, OAuthInitResponse,
    RefreshTokenRequest, RevokeTokenRequest,
};
pub use daemon::{
    ConsentManifest, Daemon, DaemonState, ExchangePairingCodeRequest, ExchangePairingCodeResponse,
    InitiatePairingRequest, InitiatePairingResponse, PairingChallenge, PairingScope,
    RevokeDaemonRequest,
};
pub use error::ControlPlaneError;
pub use events::{
    ApprovalRequestEvent, NotificationEvent, PolicyUpdateEvent, RunnerStatusEvent,
    ScheduleTriggerEvent, StreamEvent, StreamEventPayload, StreamKind, StreamSubscribeRequest,
    SyncDeltaEvent,
};
pub use identity::{IdentityLinkRequest, IdentityLinkResult, IdentityProvider, UserIdentity};
pub use ids::{
    AuditRecordId, ChallengeId, CorrelationId, DaemonId, FederatedRepositoryId, GrantId,
    IdValidationError, IdentityId, OrganizationId, PublishedObjectId, RefreshTokenId, RepositoryId,
    RunnerAttemptId, RunnerAttestationId, RunnerId, RunnerJobId, RunnerLeaseId, RunnerOutputId,
    RunnerQuarantineId, Sha256Digest, SharedSessionId, SyncReceiptId, TeamId, TombstoneId, UserId,
    WorkloadCredentialId, WorkspaceId,
};
pub use object_storage::{
    CompleteUploadRequest, ObjectEncryption, ObjectState, PresignedDownloadRequest,
    PresignedDownloadResponse, PresignedUploadRequest, PresignedUploadResponse, PublishedObject,
};
pub use organization::{
    CreateOrganizationRequest, Organization, OrganizationSlug, OrganizationSummary,
    SlugValidationError, UpdateOrganizationRequest,
};
pub use page::{CursorDecodeError, Page, PageCursor, PageRequest};
pub use publication::{DataClassification, PolicyRestrictions, PolicySnapshot, PublicationClass};
pub use rbac::{
    ActionScope, ControlPlaneRole, CreateRoleGrantRequest, RbacAction, RevokeRoleGrantRequest,
    RoleGrant,
};
pub use repository::{
    RegisterRepositoryRequest, Repository, RepositorySummary, UpdateRepositoryRequest,
};
pub use runner::{
    AttestationVerificationError, HeartbeatResponse, JobCancellation, JobCancellationResponse,
    JobClaimRequest, JobClaimResponse, JobExecutionEvent, JobExecutionEventKind, JobLease, JobSpec,
    JobTerminalState, LogChunk, LogStream, OutputDeclaration, OutputRegistration, ResourceSpec,
    RunnerAttemptState, RunnerAttestationOutput, RunnerAttestationStatement,
    RunnerAttestationSubmission, RunnerCapabilities, RunnerHeartbeat, RunnerKind, RunnerMetrics,
    RunnerQuarantineReason, RunnerRegistration, RunnerStatus, SandboxBackend, SandboxSpec,
    ATTESTATION_SCHEME_V1,
};
pub use sync::{
    SharedSession, SharedSessionState, SyncBatchResponse, SyncCursor, SyncDelta, SyncDeltaKind,
    SyncEnvelope, SyncReceipt, SyncRejection, Tombstone, TombstoneReason,
};
pub use user::{UpdateUserRequest, User, UserState, UserSummary};
pub use version::{
    ProtocolHandshakeRequest, ProtocolHandshakeResponse, ProtocolVersion, VersionParseError,
    CONTROL_PLANE_PROTOCOL_MIN_SUPPORTED, CONTROL_PLANE_PROTOCOL_V1,
};
pub use workload::{CredentialPurpose, ServiceAccountToken, WorkloadCredential};
pub use workspace::{
    AddTeamMemberRequest, CreateTeamRequest, MembershipState, OrganizationMembership, Team,
    TeamMember, TeamSlug, UpdateTeamRequest, Workspace,
};

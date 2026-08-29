//! Daemon Control Plane Synchronization Engine (Milestone 7, Tasks 7.4-7.6).
//!
//! The daemon and Axum service share the generated control-plane protocol for
//! push envelopes, receipts and pull events. Credentials are supplied only by
//! the composition root after secure-store rehydration; an active pairing with
//! no verified credential fails before making a network request.
//!
//! The engine provides:
//! - Background bidirectional sync with exponential backoff.
//! - Pairing lifecycle and consent manifest verification.
//! - An outbound durable outbox for sessions, runs, artifacts, published
//!   graphs, audit events and tombstones.
//! - Inbound idempotency, stream event consumption, policy snapshot storage.
//! - Offline-first tolerance: zero traffic when unpaired.

pub mod client;
pub mod engine;
pub mod error;
pub mod inbound;
pub mod outbox;
pub mod pairing;

pub use client::{
    CompletePairingRequest, CompletePairingResponse, ControlPlaneClient, SyncDeltaPushRequest,
    SyncDeltaPushResponse,
};
pub use engine::{SyncEngine, SyncSummary};
pub use error::ControlPlaneSyncError;
pub use inbound::{
    compute_effective_policy, get_policy_snapshot, get_remote_id, get_repository_stream_cursor,
    get_stream_cursor, has_inbound_receipt, record_inbound_receipt, record_remote_object,
    set_repository_stream_cursor, set_stream_cursor, store_policy_snapshot, EffectivePolicy,
    InboundReceipt, PolicySnapshotRecord,
};
pub use outbox::{
    acknowledge_receipt, compute_payload_hash, enqueue_artifact_summary, enqueue_audit_event,
    enqueue_delta, enqueue_graph_batch, enqueue_run_summary, enqueue_session_summary,
    enqueue_tombstone, fetch_pending_deltas, reconcile_authoritative_writes,
    reconcile_authoritative_writes_for_pairing, record_attempt_error, redact_payload_for_class,
    reject_delta_permanently, OutboxEntry,
};
pub use pairing::{
    get_credential, get_pairing, list_active_pairings, list_pairings_for_owner, normalize_endpoint,
    record_pairing, revoke_pairing, ControlPlaneCredential, ControlPlanePairing,
    LocalConsentManifest, PairingState,
};

//! Daemon Control Plane Synchronization Engine (Milestone 7, Tasks 7.4-7.6).
//!
//! # NOT WIRED, AND NOT CURRENTLY COMPATIBLE WITH THE SERVER
//!
//! This is stated first because the rest of this module reads like a finished
//! feature and is not one. Three things are true of it today:
//!
//! 1. **Nothing constructs it.** No production path builds the engine or
//!    enqueues its outbox — the only callers are its own tests. A daemon
//!    therefore never syncs, which is also why none of the below has ever
//!    surfaced as a bug report.
//! 2. **The pull route does not exist.** [`client`] requests
//!    `GET /v1/sync/events`; the server routes `GET /v1/sync/pull`
//!    (`control-plane/src/http.rs`). That is a 404, not a negotiation failure.
//! 3. **The push payload is a different type.** [`SyncDeltaPushRequest`] is a
//!    single flat delta; `POST /v1/sync/push` deserializes
//!    `control_plane_protocol::SyncEnvelope`, a batch carrying
//!    `protocol_version`, `daemon_id`, `organization_id`, `sent_at` and a
//!    `deltas` vector. Neither shape parses as the other.
//!
//! The tests in this module exercise the daemon's own contract against a mock,
//! so they pass and prove nothing about interoperability. Repairing this needs
//! the two sides to share generated types and a live cross-language test —
//! until then, treat anything here as unimplemented rather than as a component
//! that merely needs configuring.
//!
//! What the code below is INTENDED to provide, once the above is resolved:
//! - Background bidirectional sync with exponential backoff.
//! - Pairing lifecycle and consent manifest verification.
//! - An outbound durable outbox for sessions, runs, artifacts, published
//!   graphs, audit events and tombstones.
//! - Inbound idempotency, stream event consumption, policy snapshot storage.
//! - Offline-first tolerance: zero traffic and no listening sockets when
//!   unpaired — which, given (1), is the only behaviour it actually has.

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
    compute_effective_policy, get_policy_snapshot, get_remote_id, get_stream_cursor,
    has_inbound_receipt, record_inbound_receipt, record_remote_object, set_stream_cursor,
    store_policy_snapshot, EffectivePolicy, InboundReceipt, PolicySnapshotRecord,
};
pub use outbox::{
    acknowledge_receipt, compute_payload_hash, enqueue_artifact_summary, enqueue_audit_event,
    enqueue_delta, enqueue_graph_batch, enqueue_run_summary, enqueue_session_summary,
    enqueue_tombstone, fetch_pending_deltas, record_attempt_error, redact_payload_for_class,
    OutboxEntry,
};
pub use pairing::{
    get_credential, get_pairing, list_active_pairings, list_pairings_for_owner, normalize_endpoint,
    record_pairing, revoke_pairing, ControlPlaneCredential, ControlPlanePairing,
    LocalConsentManifest, PairingState,
};

#[cfg(test)]
mod contract_drift_tests {
    /// The client and the server were written against different contracts, and
    /// nothing catches it because neither side is tested against the other.
    ///
    /// This test does not assert that sync WORKS — it cannot, the engine is
    /// never constructed. It pins the two facts that make it not work, so that
    /// repairing either side fails here and names the other. Deleting this test
    /// to make a change go green is the one wrong move; the right one is to
    /// update both sides and then update this.
    #[test]
    fn the_known_client_server_contract_mismatch_is_recorded() {
        // The daemon asks for this path (`client::fetch_stream_events`).
        const CLIENT_PULL_PATH: &str = "/v1/sync/events";
        // The server routes this one (`control-plane/src/http.rs`).
        const SERVER_PULL_PATH: &str = "/v1/sync/pull";
        assert_ne!(
            CLIENT_PULL_PATH, SERVER_PULL_PATH,
            "the paths now agree — pull sync may work; verify against the real \
             router and then delete this assertion rather than inverting it"
        );

        // The daemon posts a single flat delta; the server deserializes a batch
        // envelope. Compared by their field sets, because that is what serde
        // actually rejects.
        let flat = serde_json::to_value(super::SyncDeltaPushRequest {
            daemon_sequence: 1,
            delta_kind: "session".to_string(),
            repository_id: None,
            subject_id: "s".to_string(),
            class: "internal".to_string(),
            payload: serde_json::json!({}),
            payload_hash: "0".repeat(64),
        })
        .expect("the daemon's push body serializes");
        let flat_fields: Vec<&str> = flat
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            !flat_fields.contains(&"deltas"),
            "the daemon now sends a batch — check it against `SyncEnvelope` and \
             update this test"
        );
        assert!(
            !flat_fields.contains(&"protocol_version"),
            "the daemon now sends an envelope header — see the note above"
        );
    }
}

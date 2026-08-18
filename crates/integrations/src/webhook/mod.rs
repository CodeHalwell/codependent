//! Webhook ingestion (Phase 3 STEP 3.3, Milestone 4 Task 4.2).
//!
//! GitHub delivers events over HTTP. Ingestion is deliberately ordered so a
//! forged or replayed delivery can never reach the rest of the system:
//!
//! 1. [`verify`] checks the `X-Hub-Signature-256` HMAC **before any parsing** —
//!    an unsigned or mis-signed body is rejected without ever being deserialized.
//! 2. [`store`] records the `X-GitHub-Delivery` GUID; a redelivery (same GUID)
//!    is acknowledged but never processed a second time (replay idempotency).
//! 3. [`normalize`] turns the raw payload into a small internal
//!    [`NormalizedEvent`]; unknown event types degrade to `Other`.
//!
//! [`ingest::WebhookIngestor`] ties these together, and [`server`] is a minimal
//! hand-rolled localhost HTTP/1.1 listener that maps outcomes to status codes.
//! Workflows are triggered through the injected [`WebhookEventSink`] (Task 4.2).
//!
//! # Which endpoint governs a delivery
//!
//! `POST /webhooks/<endpoint_id>` is resolved against `automation_endpoints`
//! (migration 0044) by [`store::SqliteDeliveryStore`], which the daemon attaches
//! as the ingestor's [`ingest::EndpointResolver`]. Rows are written by
//! `codypendent webhook endpoint add|rotate|disable` — until that command
//! existed the table had one SELECT and no INSERT anywhere in the repository, so
//! every request fell through to the unregistered path and the per-endpoint
//! signing key, body ceiling and replay window governed nothing.
//!
//! Two rules keep the unregistered path from being a soft option:
//!
//! - An unregistered endpoint id other than `default` is REFUSED
//!   ([`ingest::IngestOutcome::EndpointUnknown`]), and so is a registered
//!   endpoint whose scheme this build cannot verify or whose `signing_key_ref`
//!   does not resolve — all three indistinguishable on the wire (401), with the
//!   real reason logged rather than answered.
//! - `default` still falls back to the `webhooks.toml` global secret, but with
//!   [`ingest::UNREGISTERED_BODY_LIMIT_BYTES`] — the migration's own per-endpoint
//!   default — never the listener's 8 MiB ceiling. Registering an endpoint can
//!   only ever tighten or explicitly widen; failing to register can never widen.
//!
//! # What is NOT enforced
//!
//! `automation_endpoints.replay_window_seconds` is stored, resolved and
//! **never enforced as a time window** — see [`ingest::EndpointConfig`] for why
//! enforcing it on an unsigned timestamp would be worse than not enforcing it.
//! Replay is suppressed instead by the permanent content-fingerprint
//! reservation in [`store::DeliveryStore::reserve_if_new`].

pub mod config;
pub mod ingest;
pub mod normalize;
pub mod server;
pub mod store;
pub mod verify;

pub use config::WebhooksConfig;
pub use ingest::{
    resolve_signing_key, DeliveryHeaders, EndpointConfig, EndpointResolver,
    InMemoryEndpointResolver, IngestOutcome, WebhookEventSink, WebhookIngestor,
    SUPPORTED_SIGNATURE_SCHEME, UNREGISTERED_BODY_LIMIT_BYTES,
};
pub use normalize::NormalizedEvent;
pub use server::parse_endpoint_id;
pub use store::{DeliveryStore, InMemoryDeliveryStore, SqliteDeliveryStore};
pub use verify::{sign, verify_signature};

/// A failure during webhook ingestion.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The delivery-idempotency store failed.
    #[error("webhook store error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// A payload could not be (de)serialized.
    #[error("webhook serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// An I/O failure (reading a config file, socket, …).
    #[error("webhook I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The webhook configuration was invalid.
    #[error("webhook configuration error: {0}")]
    Config(String),
    /// The payload was malformed (e.g. invalid JSON body).
    #[error("malformed webhook payload: {0}")]
    Malformed(String),
    /// A webhook dispatch error occurred.
    #[error("webhook dispatch error: {0}")]
    Dispatch(String),
}

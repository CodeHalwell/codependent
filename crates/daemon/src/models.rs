//! Model-probe seam (dependency inversion) — the daemon half of
//! `CommandBody::ProbeModel`.
//!
//! Answering "can this model serve a run?" means resolving credentials the way
//! a run does and, when asked, calling the provider — work that lives in
//! `codypendent-runtime`'s `ModelRegistry`, a crate this one cannot name (the
//! daemon is below the runtime in the graph). So, exactly like
//! [`MemoryGateway`](crate::memory::MemoryGateway) and
//! [`CodeGraphGateway`](crate::codegraph::CodeGraphGateway), the daemon
//! declares the seam and the `codypendentd` assembly fills it. The
//! default-`None` [`ServerState::model_probe`](crate::server::ServerState::model_probe)
//! leaves it unwired, and the lib-only / test server then rejects every probe
//! with [`probe_unavailable`].
//!
//! # What this seam does NOT decide
//!
//! Whether the caller may ask. `ProbeModel` names
//! `NamedResource::DaemonStore(DaemonStore::Models)`, so the server's single
//! ownership gate has already refused a foreign principal before the seam is
//! reached. That gate is the point: a probe SPENDS the operator's credentials
//! against the provider, and the reply enumerates which models they have
//! configured.

use std::future::Future;
use std::pin::Pin;

use codypendent_protocol::{CodypendentError, ModelId, ModelProbe};

/// Probe one configured model, or every one of them.
#[derive(Debug, Clone)]
pub struct ProbeModelRequest {
    /// The `models.toml` id, or `None` for every configured model.
    pub model: Option<ModelId>,
    /// Whether the implementation may reach the provider over the network.
    /// `false` resolves credentials only.
    pub network: bool,
}

/// The future a probe returns. Boxed so the trait stays object-safe without an
/// `async-trait` dependency, matching [`crate::memory`]'s futures.
pub type ModelProbeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ModelProbe>, CodypendentError>> + Send + 'a>>;

/// The daemon's seam for model readiness.
///
/// A model that is configured but not ready is a ROW, not an error: the reply
/// carries its verdict like any other, because a client showing a readiness
/// column needs the failures most. An `Err` here means the probe could not be
/// performed at all (no model configuration to read, a naming an id that does
/// not exist), not that a model is unavailable.
pub trait ModelProbeGateway: Send + Sync {
    fn probe(&self, request: ProbeModelRequest) -> ModelProbeFuture<'_>;
}

/// The rejection a daemon with no seam answers with — the lib-only server in
/// tests, and any assembly that did not wire the runtime.
#[must_use]
pub fn probe_unavailable() -> CodypendentError {
    CodypendentError::new(
        "model.probe-unavailable",
        "this daemon cannot probe models: it was built without a model registry".to_string(),
        false,
    )
}

/// The rejection for an id that is not in `models.toml`. Carries the id
/// because the caller already named it, and the configured set is not a secret
/// from a principal the ownership gate has already admitted.
#[must_use]
pub fn model_not_configured(id: &ModelId) -> CodypendentError {
    CodypendentError::new(
        "model.not-configured",
        format!("no model `{}` is configured", id.0),
        false,
    )
}

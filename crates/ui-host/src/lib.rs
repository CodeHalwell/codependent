//! Trusted host for renderer-independent component documents.
//!
//! Component runtimes are untrusted producers. This crate is the boundary that
//! validates complete trees, applies revisioned patch batches atomically,
//! rejects stale interactions, governs contribution slots, and frames messages
//! to sandboxed workers. It deliberately contains no Ratatui or browser code.

mod framing;
mod registry;
mod runtime;
mod session;
mod store;

pub use framing::{
    read_ui_message, read_ui_message_with_limits, read_ui_message_with_limits_and_gate,
    write_ui_message, UiFramingError, MAX_UI_FRAME_BYTES,
};
pub use registry::{ContributionRegistry, RegistrationTrust, UiRegistryError};
pub use runtime::{
    UiWorker, UiWorkerCircuitStatus, UiWorkerConfig, UiWorkerDiagnostics, UiWorkerError,
    UiWorkerLaunch, UiWorkerLaunchPurpose, UiWorkerRuntime, UiWorkerSignal, UiWorkerSupervisor,
    VerifiedUiContribution,
};
pub use session::{UiHostSession, UiSessionError, UiSessionUpdate};
pub use store::{DocumentStore, UiHostError};

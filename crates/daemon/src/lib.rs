//! `codypendentd` library: persistence, ledger, replay, and the client
//! protocol server. The `codypendentd` binary — in the sibling
//! `crates/codypendentd` assembly crate — wires these together and injects a
//! [`RunExecutor`](crate::executor::RunExecutor) over the runtime agent loop.

// Phase 0
pub mod db;
pub mod instance;
pub mod ledger;
pub mod remote_ui;
pub mod remote_ui_plugins;
pub mod remote_ui_workers;
pub mod replay;
pub mod server;

// Phase 1
pub mod approvals;
pub mod artifacts;
pub mod blackboard;
pub mod commands;
pub mod documents;
pub mod executor;
pub mod model_profiles;
pub mod policy;
// Outcome 19: the connection principal, derived from the socket's peer
// credentials rather than asserted by the client.
pub mod principal;
pub mod projections;
pub mod promotion;
pub mod recovery;
pub mod subscriptions;
// Voice v1 (rubric 8): the speech-to-text seam the assembly implements.
pub mod transcription;
pub mod workflow_stream;
pub mod workflows;
pub mod worktrees;

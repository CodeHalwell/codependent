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
pub mod analytics;
pub mod approvals;
pub mod artifacts;
pub mod automation;
/// The durable scheduler and trigger dispatcher that actually FIRES an
/// automation binding: the atomic due-claim, the binding lease, the receipt and
/// attempt writers, and the event-sourced dispatch path the webhook sink uses.
/// Public because the `codypendentd` assembly is the only place that can supply
/// its [`automation_scheduler::AutomationEnvironment`] and `WorkflowStarter`.
pub mod automation_scheduler;
pub mod blackboard;
pub mod bundles;
pub mod checkpoints;
pub mod codegraph;
pub mod commands;
pub mod control_plane_sync;
pub mod documents;
pub mod executor;
// Milestone 6: the daemon half of cross-repository federation — publication
// policy, the outbound shared-graph projection, tombstones, access-safe
// traversal and the campaign coordinator, over `codypendent-federation`.
pub mod federation;
pub mod file_index;
pub mod forks;
pub mod hook_engine;
pub mod hook_exec;
pub mod hooks;
pub mod inbox;
pub mod marketplace;
// Outcome 17: the curated-memory store's inspect/correct/forget seam, filled by
// the assembly (only it can name `codypendent-knowledge`).
pub mod memory;
pub mod model_profiles;
pub mod poison;
pub mod policy;
// Outcomes 12/13: the daemon-side implementation of the sandbox capability
// seam — the one layer where the run and plugin capability models both belong.
pub mod policy_gate;
// Outcome 19: the connection principal, derived from the socket's peer
// credentials rather than asserted by the client.
pub mod principal;
pub mod projections;
pub mod promotion;
pub mod questions;
pub mod recovery;
pub mod session_library;
pub mod subscriptions;
// Voice v1 (rubric 8): the speech-to-text seam the assembly implements.
pub mod prompt_queue;
pub mod transcription;
pub mod unified_exec;
pub mod workflow_stream;
pub mod workflows;
pub mod worktrees;

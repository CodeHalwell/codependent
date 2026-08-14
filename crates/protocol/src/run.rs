//! Run-domain wire types: modes, states, actions, approvals, budgets.
//!
//! These describe an agent run as it crosses the wire in commands and events.
//! Every enum here is internally tagged (`#[serde(tag = "type")]`) and carries a
//! `#[serde(other)] Unknown` fallback, so a value produced by a newer peer
//! deserializes to `Unknown` rather than failing the whole frame.

use serde::{Deserialize, Serialize};

use crate::ids::{ArtifactId, DocumentId};

/// A mode preset: a bundle of policy and interaction defaults, not merely a
/// prompt (Chapter 20). Modes are enforced by the policy engine — an `Explore`
/// run proposing a write is denied regardless of what the model says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum AgentMode {
    /// Explain, answer, retrieve. Writes and commands denied.
    Ask,
    /// Investigate the repository. Read-only tools; writes denied.
    Explore,
    /// Produce an execution plan. May write plan artifacts only.
    Plan,
    /// Implement approved work in the worktree write scope.
    Build,
    /// Inspect code or a change set. Read plus comment.
    Review,
    #[serde(other)]
    Unknown,
}

/// The lifecycle state of a run (Chapter 04). Transitions are persisted before
/// they are exposed to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum RunState {
    Queued,
    Preparing,
    Running,
    WaitingForApproval,
    WaitingForUserInput,
    Paused,
    Recovering,
    Completed,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// The terminal outcome of a run, carried by `RunCompleted`.
///
/// Chapter 04 names the terminal `RunState`s but leaves the disposition detail
/// open at Phase 1; this is the minimal reasonable shape — the terminal kind
/// plus a short human-readable summary or reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum RunDisposition {
    Completed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Failed {
        reason: String,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// A side-effecting action an agent proposes, pending policy evaluation and
/// possibly approval.
///
/// This started as the Phase 1 minimal subset of the Chapter 14 shape; Phase 3
/// adds `GitHubMutation` for remote GitHub writes. Further variants
/// (`InstallPlugin`, structured `CommandRequest` / `NetworkDestination`) arrive
/// in later phases. Paths and destinations are carried as strings on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ProposedAction {
    ReadFiles {
        paths: Vec<String>,
    },
    WritePatch {
        patch: ArtifactId,
    },
    ExecuteCommand {
        program: String,
        args: Vec<String>,
        /// The child's *complete* environment as name/value pairs (empty means it
        /// inherits nothing). Carried on the action so the approver and the audit
        /// ledger see exactly what the command runs with: an unshown,
        /// model-controlled environment could otherwise smuggle
        /// execution-hijacking variables (`LD_PRELOAD`, `RUSTC_WRAPPER`, a shadowed
        /// `PATH`, …) past a benign-looking `run cargo test` approval. Defaulted so
        /// an older client that sends none still parses.
        #[serde(default)]
        environment: Vec<(String, String)>,
        /// The working directory the command runs in, when constrained.
        #[serde(default)]
        cwd: Option<String>,
    },
    NetworkRequest {
        destination: String,
    },
    GitCommit {
        repository: String,
    },
    GitPush {
        remote: String,
        branch: String,
    },
    /// A write to a remote GitHub resource (draft PR, review comment, PR
    /// update, check-run summary) via the GitHub API (Phase 3 STEP 3.1). Every
    /// such write is approval-gated and network-scoped to the GitHub API
    /// endpoint by the policy engine.
    GitHubMutation {
        /// The `owner/repo` slug the mutation targets.
        repository: String,
        /// A short human-readable description of the write, rendered on the
        /// approval card (e.g. `create draft PR on owner/repo`).
        summary: String,
    },
    /// Publish a document's deterministic Markdown render to a Git target
    /// (Phase 4 STEP 4.4). Every publish is approval-gated; the approval card
    /// renders `target`, `changed_files`, and `git_action` **verbatim** from
    /// the computed plan (STEP 4.4.2: "every publish displays target, changed
    /// files, and resulting Git action before approval").
    PublishDocument {
        document_id: DocumentId,
        /// A short human description of the target (e.g. `repository file
        /// docs/architecture.md`).
        target: String,
        /// The repo-relative files the publish changes.
        changed_files: Vec<String>,
        /// The resulting Git action (e.g. `commit docs/x.md on branch
        /// docs/publish`).
        git_action: String,
    },
    /// Post a typed artifact to a workflow run's blackboard (Phase 5 STEP 5.3) —
    /// the run-scoped coordination channel a workflow's agents share. Always
    /// permitted by policy within a workflow run (the `blackboard.post` tool is
    /// only offered when the run is a workflow node), but recorded as a proposed
    /// action so every board write is traced like any other tool call. Not a
    /// filesystem, repository, or remote write — it targets only the run's own
    /// board, so it never reaches the approval gate.
    BlackboardPost {
        /// The workflow run whose board is written (server-derived from the run
        /// context, never model-supplied).
        workflow_run_id: String,
        /// The artifact kind being posted (`finding`, `decision`, …).
        kind: String,
    },
    /// Query a workflow run's blackboard (Phase 5 STEP 5.3). A read of the run's
    /// own coordination channel; always permitted by policy within a workflow run
    /// and recorded so every board access is traced.
    BlackboardQuery {
        /// The workflow run whose board is read (server-derived).
        workflow_run_id: String,
    },
    /// Call a tool on an external MCP server (PR B — MCP client). The server is
    /// operator-declared in the trusted `<config_dir>/codypendent/mcp.toml` (the
    /// model can never name one into existence) and the call is dispositioned by
    /// the policy engine's `[mcp]` section: allow-listed servers may skip the
    /// gate, everything else is approval-gated or denied. The approval card
    /// renders `summary` plus `args` **verbatim**; `args` is canonical JSON
    /// (recursively key-sorted) so the Run-scoped auto-approval digest matches
    /// exactly-identical repeats.
    McpToolCall {
        /// The server name from `mcp.toml` (server-derived from the tool name's
        /// `mcp.<server>.<tool>` prefix, never free-form model text).
        server: String,
        /// The tool name on that server (from the server's `tools/list`).
        tool: String,
        /// A short human-readable description of the call, rendered on the
        /// approval card (e.g. `github.create_issue("…")`).
        summary: String,
        /// The model-supplied arguments as canonical JSON text (a `String`, not
        /// a `Value`, so the enum stays `Eq` and the digest is stable).
        args: String,
    },
    /// A permission request made by an external ACP agent. The agent owns its
    /// tool implementation, so Codypendent cannot truthfully coerce the call
    /// into one of its native command/filesystem variants. `details` is bounded,
    /// canonical JSON suitable for the approval card and stable action digest.
    AcpToolCall {
        /// Official registry id of the connected agent.
        agent: String,
        /// Human-readable tool/call title reported by ACP.
        title: String,
        /// Canonical, bounded ACP tool-call description.
        details: String,
    },
    /// Record a memory proposal note (the `memory.remember` core tool, smarter-memory
    /// M2). Appends a `NoteAppended` to the run's own ledger — no filesystem, command,
    /// network, or remote effect. Always policy-`Allow`ed (see the daemon policy
    /// engine's explicit arm); never serialized into a `ToolProposed` (never gated by
    /// approval), so it needs no golden wire vector.
    RecordMemory,
    /// Search the knowledge registry for the tools/skills that fit a task (the
    /// `skills.search` core tool, rubric 9). A READ of the daemon's own registry
    /// — no filesystem, command, network, or remote effect, and no
    /// model-supplied path (a skill's package directory comes from its registry
    /// row). Always policy-`Allow`ed, exactly like [`Self::RecordMemory`], and
    /// likewise never serialized into a `ToolProposed`, so it needs no golden
    /// wire vector.
    SearchRegistry,
    /// An agent `docs.*` tool call touching a collaborative document (rubric #4
    /// doc-writer). Targets only the knowledge fabric's document store — not the
    /// filesystem, a command, the network, or any remote — and every content
    /// edit is still gated by the document's collaboration mode (organization
    /// docs default to Suggest, so an agent edit lands as a reviewable
    /// suggestion), while publishing to Git stays behind the separate
    /// approval-gated `PublishDocument` pipeline. Always policy-`Allow`ed like
    /// [`RecordMemory`](Self::RecordMemory) and recorded purely so the access
    /// is traced; never serialized into a `ToolProposed`, so it needs no golden
    /// wire vector.
    DocumentEdit {
        /// The document the tool call targets; empty for `docs.create` (the
        /// document does not exist yet) and `docs.read` listings.
        document_id: String,
        /// A short human description of the access (e.g. `docs.edit block p`),
        /// for the trace.
        summary: String,
    },
    /// Query a durable workflow run's graph state (the `workflow.query` runtime
    /// tool, rubric 5): nodes, states, dependency edges, and measured costs. A
    /// pure read of Codypendent's own durable store — no filesystem, command,
    /// network, or remote effect — so it is always policy-`Allow`ed (see the
    /// daemon policy engine's explicit arm) and recorded only so the access is
    /// traced like any other tool call.
    WorkflowQuery {
        /// The workflow run being read, or empty when listing the repository's
        /// runs (server-derived from the run context / validated args).
        workflow_run_id: String,
    },
    /// Persist a validated, reviewable workflow manifest in the user's workflow
    /// directory. The full structured draft stays tool-local; this bounded
    /// preview is what policy, approval UIs, and the audit ledger record.
    WorkflowCreate {
        workflow_id: String,
        summary: String,
    },
    /// Start a named persisted workflow or a validated inline workflow in the
    /// current run's repository. Both forms can spend model/tool budget and are
    /// therefore explicitly approval-gated.
    WorkflowRun {
        workflow_id: String,
        /// `named` or `inline`, so approval surfaces distinguish persistence
        /// from an ephemeral manifest without decoding tool arguments.
        kind: String,
        summary: String,
    },
    /// Write a card on the repository task board (the `task.create` /
    /// `task.update` / `task.move` runtime tools, rubric 10). Internal
    /// coordination state in Codypendent's own store — like a blackboard post it
    /// touches no file, command, network, or remote — so policy allows it
    /// without an approval gate; the action is recorded so every board write is
    /// traced and attributable.
    TaskWrite {
        /// The canonical repository whose board is written (server-derived from
        /// the run context, never model-supplied).
        repository: String,
        /// A short human rendering of the write (e.g. `create "wire the DAG"`).
        summary: String,
    },
    /// Read the repository task board (the `task.list` runtime tool). A read of
    /// internal state; always policy-`Allow`ed and recorded for the trace.
    TaskRead {
        /// The canonical repository whose board is read (server-derived).
        repository: String,
    },
    /// Persist a validated multi-model council definition. The summary is a
    /// bounded policy preview; the full typed definition remains tool-local.
    CouncilCreate {
        name: String,
        summary: String,
    },
    /// Convene a persisted council. This may fan out paid model requests, so it
    /// always reaches an explicit approval before execution.
    CouncilRun {
        name: String,
        summary: String,
    },
    /// Read a durable council result by stable result id or council name.
    CouncilResultRead {
        selector: String,
    },
    /// Read the repository's derived code graph (the `graph.callers_of` /
    /// `graph.blast_radius` / `graph.tests_covering` runtime tools, outcome 5).
    /// A pure read of Codypendent's OWN derived projection — no filesystem,
    /// command, network, or remote effect, and the model-supplied symbol or path
    /// is matched against stored `code_nodes` rows, never opened. Always
    /// policy-`Allow`ed like [`Self::SearchRegistry`], and likewise never
    /// serialized into a `ToolProposed`, so it needs no golden wire vector.
    CodeGraphQuery {
        /// The canonical repository whose graph is read (server-derived from the
        /// run context, never model-supplied).
        repository: String,
        /// A short human rendering of the question (e.g. `callers of
        /// Router::decide`), for the trace.
        summary: String,
    },
    /// Assert an edge onto the repository's derived code graph (the
    /// `graph.assert_edge` runtime tool) — a relation the parser cannot see, such
    /// as a route handler to the service it dispatches to.
    ///
    /// A WRITE, and deliberately its own variant rather than a
    /// [`Self::CodeGraphQuery`]: recording it as a query would make the audit
    /// ledger say the agent read the graph when it changed it. What it writes is
    /// Codypendent's OWN derived projection — no filesystem, command, network or
    /// remote effect — and it cannot invent a node: both endpoints must already
    /// match stored `code_nodes` rows, which are never opened as paths. So it is
    /// allowed without an approval gate on the same reasoning as
    /// [`Self::TaskWrite`], and recorded so every assertion is traced and
    /// attributable. Never serialized into a `ToolProposed`, so it needs no
    /// golden wire vector.
    CodeGraphAssert {
        /// The canonical repository whose graph is written (server-derived from
        /// the run context, never model-supplied).
        repository: String,
        /// A short human rendering of the assertion (e.g. `assert handle_charge
        /// calls ChargeService::run`), for the trace.
        summary: String,
    },
    #[serde(other)]
    Unknown,
}

/// A structured risk assessment attached to a proposed action or approval
/// request. Chapter 14 leaves the exact shape open at Phase 1; this is the
/// minimal reasonable form — a severity level plus human-readable reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Risk {
    pub level: RiskLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// Severity buckets for a [`Risk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
    #[serde(other)]
    Unknown,
}

/// The decision an approver returns for a proposed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ApprovalDecision {
    Approve,
    Reject,
    #[serde(other)]
    Unknown,
}

/// How widely an approval applies (Chapter 04 / STEP 1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ApprovalScope {
    /// This single proposal only.
    Once,
    /// Every identical proposal for the remainder of the run.
    Run,
    /// A recorded pattern of similar proposals.
    Pattern,
    /// Any matching proposal in this repository.
    Repository,
    #[serde(other)]
    Unknown,
}

/// Which budget a `BudgetWarning` is about. The unit of the reported
/// `used`/`limit` is implied by the dimension (tokens, minor currency units,
/// seconds, or a count of calls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BudgetDimension {
    Tokens,
    Cost,
    WallClock,
    ToolCalls,
    #[serde(other)]
    Unknown,
}

/// The outcome of a completed tool call, carried by `ToolCompleted`.
///
/// Chapter 03 lists tool-completed as an event category without fixing its
/// payload; this is the minimal reasonable shape — success, or failure with a
/// short message. Bulk output travels as an `ArtifactRef`, never here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ToolOutcome {
    Succeeded,
    Failed {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a value through JSON and assert it is unchanged.
    fn round_trip<T>(value: T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(&value).expect("serialize");
        let parsed: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, parsed);
    }

    #[test]
    fn run_domain_types_round_trip() {
        round_trip(AgentMode::Build);
        round_trip(RunState::WaitingForApproval);
        round_trip(RunDisposition::Completed {
            summary: Some("done".to_string()),
        });
        round_trip(RunDisposition::Failed {
            reason: "daemon restart".to_string(),
        });
        round_trip(RunDisposition::Cancelled { reason: None });
        round_trip(ProposedAction::ExecuteCommand {
            program: "cargo".to_string(),
            args: vec!["test".to_string()],
            environment: vec![("RUST_BACKTRACE".to_string(), "1".to_string())],
            cwd: Some("/repo".to_string()),
        });
        round_trip(ProposedAction::WritePatch {
            patch: ArtifactId::new(),
        });
        round_trip(ProposedAction::GitHubMutation {
            repository: "octocat/hello-world".to_string(),
            summary: "create draft PR on octocat/hello-world".to_string(),
        });
        round_trip(ProposedAction::AcpToolCall {
            agent: "claude-acp".to_string(),
            title: "write file".to_string(),
            details: "{\"path\":\"src/lib.rs\"}".to_string(),
        });
        round_trip(ProposedAction::DocumentEdit {
            document_id: "70000000-0000-0000-0000-000000000001".to_string(),
            summary: "docs.edit block p".to_string(),
        });
        round_trip(ProposedAction::PublishDocument {
            document_id: DocumentId::new(),
            target: "repository file docs/architecture.md".to_string(),
            changed_files: vec!["docs/architecture.md".to_string()],
            git_action:
                "write docs/architecture.md in the working tree (approval-gated change set)"
                    .to_string(),
        });
        round_trip(Risk {
            level: RiskLevel::High,
            reasons: vec!["writes outside the worktree".to_string()],
        });
        round_trip(ApprovalDecision::Approve);
        round_trip(ApprovalScope::Run);
        round_trip(BudgetDimension::Tokens);
        round_trip(ToolOutcome::Failed {
            message: "exit 1".to_string(),
        });
    }

    #[test]
    fn unknown_tags_deserialize_to_unknown() {
        let future = serde_json::json!({ "type": "FromTheFuture", "extra": 1 });
        assert!(matches!(
            serde_json::from_value::<AgentMode>(future.clone()).expect("mode"),
            AgentMode::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<RunState>(future.clone()).expect("state"),
            RunState::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<RunDisposition>(future.clone()).expect("disposition"),
            RunDisposition::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<ProposedAction>(future.clone()).expect("action"),
            ProposedAction::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<RiskLevel>(future.clone()).expect("risk"),
            RiskLevel::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<ApprovalDecision>(future.clone()).expect("decision"),
            ApprovalDecision::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<ApprovalScope>(future.clone()).expect("scope"),
            ApprovalScope::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<BudgetDimension>(future.clone()).expect("dimension"),
            BudgetDimension::Unknown
        ));
        assert!(matches!(
            serde_json::from_value::<ToolOutcome>(future).expect("outcome"),
            ToolOutcome::Unknown
        ));
    }
}

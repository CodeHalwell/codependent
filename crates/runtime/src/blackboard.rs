//! The blackboard write/read seam for the agent loop (Phase 5 STEP 5.3).
//!
//! The `blackboard.post` / `blackboard.query` tools let a workflow agent read and
//! write its run's typed artifact channel. The authoritative store lives in
//! `codypendent-workflow` (`BlackboardStore`, over the SQLite pool), which this
//! crate cannot name — `sqlx` is not a dependency (ADR-009) and neither is the
//! workflow crate. So, exactly as the tool layer reaches the artifact store through
//! the pool-erased [`ArtifactSink`](crate::tools::ArtifactSink) and the loop reaches
//! the ledger through the [`RunJournal`](crate::agent::RunJournal), the loop reaches
//! the blackboard through this trait: the `codypendentd` assembly implements it over
//! a real `BlackboardStore` + pool + the daemon's per-run fan-out hub, and injects it
//! into the runtime (see [`FrameworkAgentRuntime::with_blackboard`]).
//!
//! The seam is **workflow-type-erased**: a kind is a plain string (the assembly
//! parses it against `BlackboardKind`), and payload/author/evidence ride as opaque
//! JSON — so this crate stays decoupled from the workflow domain types. It returns
//! the protocol [`BlackboardItemView`] (which this crate *can* name) so a posted or
//! queried item is described once, wire-ready.
//!
//! [`FrameworkAgentRuntime::with_blackboard`]: crate::agent::FrameworkAgentRuntime::with_blackboard

use async_trait::async_trait;
use codypendent_protocol::BlackboardItemView;
use serde_json::Value;

/// An artifact an agent asks to post (or supersede) on its run's board. The
/// `author` is built **server-side** by the runtime from the run context, never
/// from model-supplied identity — the tool overwrites whatever the model sent.
#[derive(Debug, Clone)]
pub struct BlackboardPost {
    /// The artifact kind (`finding`, `decision`, …) — the assembly validates it.
    pub kind: String,
    /// The artifact body (opaque JSON).
    pub payload: Value,
    /// Attribution built from the authoring node's run context
    /// (`{role, run_id, node_id, workflow_run_id}`).
    pub author: Value,
    /// The author's confidence in `[0, 1]`, if given.
    pub confidence: Option<f64>,
    /// Evidence references grounding the artifact. Claim-like kinds require at
    /// least one; the store enforces it and the refusal surfaces to the agent.
    pub evidence: Vec<Value>,
    /// When set, this post *supersedes* the identified prior item (a correction):
    /// the store posts the replacement at the next revision and stamps the old one.
    pub supersedes: Option<String>,
}

/// A structured blackboard failure, mapped by the assembly from the store's error.
///
/// Every variant carries a stable dotted [`code`](BlackboardChannelError::code) and
/// a legible `Display`, so the tool can feed the reason back to the agent as a
/// **correctable** observation — most importantly [`EvidenceRequired`], which the
/// agent fixes by re-posting with evidence.
///
/// [`EvidenceRequired`]: BlackboardChannelError::EvidenceRequired
#[derive(Debug, thiserror::Error)]
pub enum BlackboardChannelError {
    /// A claim-like artifact was posted without evidence — the agent should retry
    /// with at least one evidence reference.
    #[error("a {0} must carry at least one evidence reference — retry with evidence")]
    EvidenceRequired(String),
    /// The item to supersede does not exist on this run's board.
    #[error("no such blackboard item to supersede: {0}")]
    NotFound(String),
    /// The item to supersede was already superseded by a concurrent correction.
    #[error("blackboard item {0} has already been superseded")]
    AlreadySuperseded(String),
    /// The posted/queried kind is not a known blackboard artifact kind.
    #[error("`{0}` is not a known blackboard artifact kind")]
    UnknownKind(String),
    /// The blackboard is not available for this run (no channel, or not a workflow
    /// run) — the tool should not have been offered.
    #[error("the blackboard is not available for this run")]
    Unavailable,
    /// An underlying store/backend failure (surfaced without leaking internals).
    #[error("blackboard backend error: {0}")]
    Backend(String),
}

impl BlackboardChannelError {
    /// A stable, dotted machine code for this failure, for a `ToolCompleted`
    /// payload's `Failed` message.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            BlackboardChannelError::EvidenceRequired(_) => "blackboard.evidence-required",
            BlackboardChannelError::NotFound(_) => "blackboard.item-not-found",
            BlackboardChannelError::AlreadySuperseded(_) => "blackboard.already-superseded",
            BlackboardChannelError::UnknownKind(_) => "blackboard.unknown-kind",
            BlackboardChannelError::Unavailable => "blackboard.unavailable",
            BlackboardChannelError::Backend(_) => "blackboard.backend-error",
        }
    }
}

/// The pool-erased seam the agent loop posts to and queries the run's board
/// through. Implemented by the `codypendentd` assembly over a real
/// `BlackboardStore` + pool + the daemon's per-run fan-out hub.
#[async_trait]
pub trait BlackboardChannel: Send + Sync {
    /// Post (or supersede) an artifact on `workflow_run_id`'s board, returning the
    /// stored item's view. A successful post is fanned out to the run's
    /// subscribers by the implementation.
    async fn post(
        &self,
        workflow_run_id: &str,
        post: BlackboardPost,
    ) -> Result<BlackboardItemView, BlackboardChannelError>;

    /// Query `workflow_run_id`'s board, optionally filtered by `kind`; superseded
    /// items are excluded unless `include_superseded`. Newest first.
    async fn query(
        &self,
        workflow_run_id: &str,
        kind: Option<String>,
        include_superseded: bool,
    ) -> Result<Vec<BlackboardItemView>, BlackboardChannelError>;
}

/// A backlog card an agent asks to place on a repository's task board (the
/// `task.create` tool, rubric 10). Like [`BlackboardPost`], the `author` is built
/// **server-side** by the runtime from the run context, never from model-supplied
/// identity.
#[derive(Debug, Clone)]
pub struct TaskCardDraft {
    /// The card body (opaque JSON — conventionally `{ "title", "description" }`).
    pub payload: Value,
    /// Attribution built from the run context.
    pub author: Value,
    /// The starting column; the assembly defaults a card with none to `todo`.
    pub status: Option<String>,
    /// Who the card is assigned to, if anyone.
    pub assignee: Option<String>,
    /// The within-column position; absent appends to the end of the column.
    pub ordinal: Option<i64>,
}

/// The fields a `task.update` / `task.move` replaces. Everything absent is carried
/// forward from the superseded card, so a move never has to restate the body.
#[derive(Debug, Clone)]
pub struct TaskCardChange {
    /// The new column, when moving.
    pub status: Option<String>,
    /// The new assignee, when re-assigning.
    pub assignee: Option<String>,
    /// The new within-column position; absent appends when the column changed.
    pub ordinal: Option<i64>,
    /// A replacement card body, when editing.
    pub payload: Option<Value>,
    /// Attribution for the revision, built server-side from the run context.
    pub author: Value,
}

/// The pool-erased seam the `task.*` tools reach a **repository task board**
/// through — the kanban half of the blackboard (rubric 10).
///
/// Separate from [`BlackboardChannel`] because the two are scoped differently and
/// offered differently: a blackboard post targets the agent's own *workflow run*
/// and exists only inside one, while a task card targets the *repository*, so the
/// tools work from a plain chat run too ("break this feature into backlog cards").
/// The assembly implements both over one `BlackboardStore`, so a card an agent
/// creates and a card a human creates in the TUI are the same durable row.
#[async_trait]
pub trait TaskBoardChannel: Send + Sync {
    /// Create a card on `repository`'s board, returning the stored card. The
    /// board is created on first write.
    async fn create(
        &self,
        repository: &str,
        draft: TaskCardDraft,
    ) -> Result<BlackboardItemView, BlackboardChannelError>;

    /// Supersede a live card with a revised one (a move, a re-assignment, a
    /// re-order, or an edit), returning the replacement.
    async fn update(
        &self,
        repository: &str,
        item_id: &str,
        change: TaskCardChange,
    ) -> Result<BlackboardItemView, BlackboardChannelError>;

    /// Every live card on `repository`'s board. An unwritten board reads empty.
    async fn list(
        &self,
        repository: &str,
    ) -> Result<Vec<BlackboardItemView>, BlackboardChannelError>;
}

/// One line of the repository's recent-run list — what `workflow.query` answers
/// with when it is not pointed at a single run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunSummary {
    /// The durable workflow-run id (the subject of a follow-up query).
    pub workflow_run_id: String,
    /// The manifest's workflow id (e.g. `repair-github-check`).
    pub workflow_id: String,
    /// The run's current phase, lowercased (`running`, `completed`, …).
    pub phase: String,
}

/// The pool-erased seam the `workflow.query` tool reads durable workflow state
/// through (rubric 5) — the agent-facing counterpart of the daemon's
/// `ReadWorkflowRun`.
///
/// Returns the protocol [`WorkflowRunSnapshot`] (which this crate *can* name), so
/// the graph an agent sees is projected by exactly the code that projects the one
/// a client sees: node states, dependency edges, and measured costs cannot drift
/// between the two surfaces.
#[async_trait]
pub trait WorkflowQueryChannel: Send + Sync {
    /// One run's full graph state, or `None` when no such run exists.
    async fn snapshot(
        &self,
        workflow_run_id: &str,
    ) -> Result<Option<codypendent_protocol::WorkflowRunSnapshot>, BlackboardChannelError>;

    /// The repository's most recent runs, newest first — the entry point for an
    /// agent that has no run id yet.
    async fn recent_runs(
        &self,
        repository: &str,
        limit: u32,
    ) -> Result<Vec<WorkflowRunSummary>, BlackboardChannelError>;
}

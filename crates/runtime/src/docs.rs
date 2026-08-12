//! The collaborative-document seam for the agent loop (rubric #4, doc writer).
//!
//! The `docs.*` tools let an agent draft, read, edit, and propose changes to a
//! knowledge-fabric document. The authoritative document lives in
//! `codypendent-knowledge` (a Loro CRDT over the SQLite pool), which this crate
//! cannot name — `sqlx` is not a dependency (ADR-009) and neither is the
//! knowledge crate. So, exactly as the loop reaches the blackboard through
//! [`BlackboardChannel`](crate::blackboard::BlackboardChannel), it reaches
//! documents through this trait: the `codypendentd` assembly implements it over
//! the same `apply_mutation` seam a human client's `MutateDocument` goes
//! through, and injects it (see [`FrameworkAgentRuntime::with_docs`]).
//!
//! # Why this is safe to give an agent
//!
//! Every write goes through the document's **collaboration mode** gate
//! (`apply_mutation`), attributed to [`DocsAuthor`] as a knowledge
//! `DocumentAuthor::Agent { run_id, model, policy_version }` — the traceability
//! triple the attribution schema was built for. Organization-scope documents
//! default to `Suggest`, so an agent edit there lands as a **pending
//! suggestion** a human accepts or rejects in the Docs Studio's review rail, not
//! as a silent content change. `docs.suggest` proposes unconditionally in any
//! mode that permits proposing. Publishing to Git is NOT reachable from here at
//! all: it stays behind the separate approval-gated `PublishDocument` pipeline.
//!
//! [`FrameworkAgentRuntime::with_docs`]: crate::agent::FrameworkAgentRuntime::with_docs

use async_trait::async_trait;
use codypendent_protocol::RunId;

/// Who a `docs.*` write is attributed to. Built **server-side** by the runtime
/// from the run context and the active policy — never from model-supplied
/// identity. The assembly maps it to knowledge's
/// `DocumentAuthor::Agent { run_id, model, policy_version }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsAuthor {
    pub run_id: RunId,
    /// The model driving this run.
    pub model: String,
    /// The policy version in force, so a document sentence is traceable to the
    /// rules that permitted it.
    pub policy_version: String,
}

/// A document an agent asks to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsCreate {
    pub title: String,
    /// The scope string (`"repository"` / `"system"` / `"organization:<uuid>"`),
    /// or `None` for the assembly's default (this run's repository).
    pub scope: Option<String>,
    /// Markdown to seed the document's blocks from, imported at block
    /// granularity. `None` creates an empty document.
    pub markdown: Option<String>,
}

/// A block-text edit an agent asks for. Whether it APPLIES or lands as a
/// suggestion is the document's collaboration mode's decision, never the
/// caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsEdit {
    pub document_id: String,
    pub block_id: String,
    /// The block's full replacement text.
    pub text: String,
}

/// A change an agent PROPOSES over a range of a block. Always a suggestion,
/// whatever the mode (a mode that forbids proposing refuses it outright).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsSuggest {
    pub document_id: String,
    pub block_id: String,
    /// Character offsets `[start, end)` within the block's text. An empty range
    /// is an insertion at `start`.
    pub range_start: u32,
    pub range_end: u32,
    pub replacement: String,
    /// Why — shown verbatim in the Docs Studio review rail.
    pub rationale: Option<String>,
}

/// What a `docs.edit` / `docs.suggest` call did — the distinction the agent must
/// see, because "proposed for review" is a materially different outcome from
/// "applied".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocsWriteEffect {
    /// Applied directly to the document (Edit mode), at the new revision.
    Applied { revision: u64 },
    /// Recorded as a pending suggestion awaiting human review.
    Suggested { suggestion_id: String },
}

/// A structured document failure, mapped by the assembly from the engine's
/// errors. Each carries a stable dotted [`code`](DocsChannelError::code) and a
/// legible `Display`, so the tool feeds the reason back to the agent as a
/// **correctable** observation.
#[derive(Debug, thiserror::Error)]
pub enum DocsChannelError {
    /// No document with that id (or the id was not a document id at all).
    #[error("no such document: {0}")]
    NotFound(String),
    /// The document's collaboration mode forbids this write (e.g. an `Ask` or
    /// `Review` mode document). Not retryable by rephrasing.
    #[error("{0}")]
    ModeDenied(String),
    /// The block's text moved under the proposed range — re-read the document
    /// and propose again against the current text.
    #[error("{0} — re-read the document and propose again")]
    Drifted(String),
    /// The request was malformed for this document (unknown block, inverted
    /// range, blank title).
    #[error("{0}")]
    Invalid(String),
    /// Documents are not available in this build/embedding (no channel wired).
    #[error("the document fabric is not available for this run")]
    Unavailable,
    /// An underlying store/CRDT failure (surfaced without leaking internals).
    #[error("document backend error: {0}")]
    Backend(String),
}

impl DocsChannelError {
    /// A stable, dotted machine code for a `ToolCompleted` payload's `Failed`
    /// message.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            DocsChannelError::NotFound(_) => "docs.not-found",
            DocsChannelError::ModeDenied(_) => "docs.mode-denied",
            DocsChannelError::Drifted(_) => "docs.range-drifted",
            DocsChannelError::Invalid(_) => "docs.invalid-request",
            DocsChannelError::Unavailable => "docs.unavailable",
            DocsChannelError::Backend(_) => "docs.backend-error",
        }
    }
}

/// The pool-erased seam the agent loop reads and writes documents through.
/// Implemented by the `codypendentd` assembly over `codypendent-knowledge`'s
/// `DocumentStore` + `apply_mutation` (the same mode gate a human client's
/// `MutateDocument` passes through).
#[async_trait]
pub trait DocsChannel: Send + Sync {
    /// Create a document, attributed to `author`. Returns its id.
    async fn create(
        &self,
        author: &DocsAuthor,
        request: DocsCreate,
        repository: &str,
    ) -> Result<String, DocsChannelError>;

    /// Render a document as Markdown, with a `block <id>` index the agent needs
    /// to target `docs.edit` / `docs.suggest`. With no `document_id`, lists the
    /// documents visible to `repository` instead.
    async fn read(
        &self,
        document_id: Option<&str>,
        repository: &str,
    ) -> Result<String, DocsChannelError>;

    /// Replace a block's text, attributed to `author` and routed through the
    /// document's collaboration mode (so an organization document's default
    /// `Suggest` turns this into a reviewable suggestion).
    async fn edit(
        &self,
        author: &DocsAuthor,
        request: DocsEdit,
    ) -> Result<DocsWriteEffect, DocsChannelError>;

    /// Propose a range replacement — always a suggestion, whatever the mode.
    async fn suggest(
        &self,
        author: &DocsAuthor,
        request: DocsSuggest,
    ) -> Result<DocsWriteEffect, DocsChannelError>;
}

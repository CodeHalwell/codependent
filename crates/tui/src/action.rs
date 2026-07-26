//! Semantic actions (STEP 1.12 RULE 1).
//!
//! Everything that can change the app funnels through [`Action`]. Two sources
//! feed it: the CLI's connection task, which wraps each daemon [`SessionEvent`]
//! as [`Action::DaemonEvent`]; and the input layer ([`crate::input::map_event`]),
//! which turns a key/mouse/paste/resize into a navigation or command action.
//! The reducer ([`crate::reduce::reduce`]) is the only place that reads an
//! `Action`, and it performs no I/O.

use codypendent_protocol::{ApprovalScope, DocumentId, DocumentMutation, RunId, SessionEvent};

use crate::state::{DocBlockView, DocSuggestionView, Pane};

/// A semantic action the reducer folds into [`crate::state::AppState`].
///
/// The large [`SessionEvent`] is boxed so every other (small) variant does not
/// pay for it — and so the whole enum stays cheap to move.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    // --- from the connection task ---
    /// A durable daemon event to fold into state.
    DaemonEvent(Box<SessionEvent>),
    /// A catch-up *snapshot* (the session was too far behind for an event
    /// replay): seed the session title, closed flag, and its active runs as
    /// stubs so a reopened long-running session is not blank until the next live
    /// event fills a run in.
    CatchupSnapshot {
        title: String,
        closed: bool,
        runs: Vec<RunId>,
    },
    /// A periodic timer tick (spinner animation, elapsed timers). No I/O.
    Tick,
    /// A transient status-line notice from the harness (e.g. a rejected
    /// command's code + message). Cleared automatically a few seconds later.
    Notice(String),

    // --- navigation (from keys / mouse) ---
    /// Move keyboard focus to the next pane (`Tab`).
    CyclePane,
    /// Move keyboard focus to a specific pane (mouse click).
    FocusPane(Pane),
    /// Select the previous item / scroll up in the focused pane (`Up`/`k`/wheel-up).
    SelectPrev,
    /// Select the next item / scroll down in the focused pane (`Down`/`j`/wheel-down).
    SelectNext,
    /// Scroll the transcript up a page (`PageUp`).
    ScrollPageUp,
    /// Scroll the transcript down a page (`PageDown`).
    ScrollPageDown,
    /// Open / expand the selected item (`Enter`).
    Expand,

    // --- run control ---
    /// Switch the conversation to the previous run (`Ctrl-↑`).
    PrevRun,
    /// Switch the conversation to the next run (`Ctrl-↓`).
    NextRun,
    /// Open the new-run prompt (`n`).
    NewRun,
    /// Pause the selected run, or resume it if already paused (`p`).
    Pause,
    /// Ask to cancel the selected run — opens a confirm modal (`c`).
    Cancel,
    /// Confirm a pending cancel (`y`/`Enter` in the confirm modal).
    ConfirmCancel,
    /// Open the steering-input prompt (`s`).
    Steer,

    // --- approvals ---
    /// Approve the focused pending approval with the given scope
    /// (`a` = once, `A` = for the run).
    Approve(ApprovalScope),
    /// Reject the focused pending approval (`r`).
    Reject,

    // --- text entry (active only while a prompt overlay is open) ---
    /// Append a character to the open prompt.
    InputChar(char),
    /// Insert bracketed-paste text into the open prompt.
    InputPaste(String),
    /// Delete the last character of the open prompt.
    InputBackspace,
    /// Submit the open prompt (`Enter`).
    InputSubmit,
    /// Abandon the open prompt (`Esc`).
    InputCancel,

    // --- knowledge browsers (STEP 2.6) ---
    /// Toggle the Skill Studio browser (`S`).
    OpenSkills,
    /// Toggle the memory browser (`M`).
    OpenMemory,
    /// Reveal the focused memory's source in full (`o`, or `Enter` in the memory
    /// browser). The TUI does no I/O, so this surfaces the source string rather
    /// than opening a file.
    OpenSource,

    // --- Docs Studio & code intelligence (Phase 4 client wiring) ---
    /// Toggle the Docs Studio browser (`D`): tree / editor rail / review rail.
    OpenDocs,
    /// Toggle the code-graph edge inspector (`G`).
    OpenEdges,
    /// Toggle the workflow-graph view (`W`): nodes with state, action, agent,
    /// worktree, approval, retry, dependencies, and declared outputs.
    OpenWorkflow,
    /// Toggle the blackboard view (`B`): the typed artifacts agents share within
    /// a workflow run, with author, confidence, evidence, and payload summary.
    OpenBlackboard,

    // --- Docs Studio live editing (Phase 4 STEP 4.3 client wiring) ---
    /// Begin editing the focused block in the Docs editor rail (`e`): opens the
    /// block-edit prompt. Submitting it acquires the block lease and, on the grant,
    /// sends the mutation.
    EditDoc,
    /// A merged replica update, projected by the CLI harness after it folded an
    /// incoming `DocumentSync` into the document's client replica and re-read its
    /// pending suggestions. Replaces the matching card's blocks/suggestions/revision
    /// so the editor reflects the authoritative result. The whole CRDT merge stays
    /// in the harness (which owns the Loro replica); the reducer folds the ready
    /// projection.
    DocumentSynced {
        document_id: DocumentId,
        /// The document's revision after the sync, pre-rendered (e.g. `"r8"`).
        revision: String,
        blocks: Vec<DocBlockView>,
        suggestions: Vec<DocSuggestionView>,
    },
    /// The daemon granted the edit lease this client requested — the reply the
    /// harness forwards from `Payload::DocumentLeaseGranted`. Marks the in-flight
    /// edit *held* and releases its queued mutation.
    DocumentLeaseGranted {
        document_id: DocumentId,
        lease_id: String,
    },
    /// The daemon refused the edit lease: the block range is held by another writer
    /// (`document.range-leased`). Marks the in-flight edit *blocked* and surfaces a
    /// visible notice — the presence-lite "someone else is editing" signal.
    DocumentLeaseBlocked,

    /// A live workflow node transition, projected by the CLI harness after it folded
    /// an incoming `Payload::WorkflowEvent` (Phase 5 T9). Overlays the matching
    /// workflow-graph card's live `state` / `cost` / `error` (each pre-rendered by the
    /// harness), so the graph view's `node_state_color` branches come alive as the run
    /// advances. The whole subscription/rendering stays in the harness (which owns the
    /// socket); the reducer folds the ready projection by node id — idempotent
    /// overwrite, so an overlap between the snapshot baseline and the live stream is a
    /// harmless re-write.
    WorkflowNodeUpdated {
        /// The node (step) id to overlay — matches [`WorkflowNodeCard::id`].
        node_id: String,
        /// The node's live state, pre-rendered (e.g. `running` / `completed` /
        /// `skipped`).
        state: String,
        /// The node's measured cost, pre-rendered (e.g. `"12s · 3 tool calls"`), or
        /// `"—"` when none.
        cost: String,
        /// The node's failure/block reason, pre-rendered, or `"—"` when none.
        error: String,
    },

    /// Toggle the command palette (`/`): a searchable list of every command.
    OpenPalette,
    /// Begin the add-model flow for the focused provider in the `/provider` picker
    /// (`Tab`): opens the model-name prompt (step 2). A no-op outside the provider
    /// picker.
    BeginAddModel,
    /// Flip between the chat single-column and the workspace panes (`F2`).
    ToggleLayout,

    // --- overlays / lifecycle ---
    /// Toggle the help overlay (`?`).
    Help,
    /// Detach this client (`q`). Never kills the run.
    Detach,
    /// Dismiss the top-most overlay / modal (`Esc`).
    Dismiss,

    /// A recognized-but-inert event (e.g. an unmapped key). Kept so the input
    /// mapper can stay total and callers never juggle `Option`.
    NoOp,
}

impl Action {
    /// Convenience constructor that boxes the event for [`Action::DaemonEvent`].
    #[must_use]
    pub fn daemon_event(event: SessionEvent) -> Self {
        Action::DaemonEvent(Box::new(event))
    }
}

/// A secret API key carried from the add-model flow to the CLI harness (the one
/// place that performs I/O), for a hosted provider. The `tui` crate never writes
/// it to disk; the harness stores it in `auth.json` (mode `0600`). `Debug` is
/// hand-written to REDACT the value — mirroring
/// `codypendent_providers::credential::ResolvedCredential` — so a stray
/// `{intent:?}` can never leak the key into a log or a snapshot. `PartialEq`/`Eq`
/// compare the inner value (so a test can assert on the exact key it supplied).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(pub String);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// A semantic command the reducer wants sent to the daemon.
///
/// The TUI performs no I/O, so instead of talking to the daemon it appends an
/// `Intent` to [`crate::state::AppState::outbox`]. The CLI's connection task
/// drains the outbox after each reduce and turns each intent into a protocol
/// `Command`. This keeps `reduce` pure and unit-testable: a test asserts on the
/// intents produced, never on a socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Start a new run in the attached session.
    StartRun {
        objective: String,
        mode: codypendent_protocol::AgentMode,
        /// The model the operator pinned via the `/model` picker (STEP MP2),
        /// carried so the started run executes on exactly that model. `None`
        /// lets the daemon resolve/route the model as before.
        model: Option<codypendent_protocol::ModelId>,
    },
    /// Continue the attached session with a follow-up message once the
    /// selected run has reached a terminal state (continuous-session plan,
    /// Task 5): the daemon reconstructs the session's prior turns from its
    /// event ledger and seeds a new continuation run, rather than starting a
    /// context-free one (an active run instead steers via
    /// [`Intent::QueueSteering`], unchanged). Mirrors [`Intent::StartRun`]'s
    /// `mode` field; the session id is supplied by the harness the same way
    /// `StartRun`'s is (see `intent_to_command`).
    ///
    /// Carries the current pinned `model` so a mid-conversation model switch is
    /// instant: when the operator re-picks a model in the `/model` picker, the
    /// very next follow-up in the SAME session runs on it, no restart and no new
    /// session. `Some(id)` pins this continuation (and becomes the session's
    /// current model server-side); `None` lets the daemon INHERIT the session's
    /// existing model (I-2) from its provenance, exactly as before. Repository
    /// (I-1) is still never carried here: it is stable across a session and the
    /// client is not authoritative for it.
    SubmitUserInput {
        text: String,
        mode: codypendent_protocol::AgentMode,
        /// The model the operator pinned via the `/model` picker, carried so a
        /// re-pick applies to this very follow-up (mid-conversation model
        /// switch). `None` inherits the session's current model server-side.
        model: Option<codypendent_protocol::ModelId>,
    },
    /// Resolve a pending approval.
    ResolveApproval {
        approval_id: codypendent_protocol::ApprovalId,
        decision: codypendent_protocol::ApprovalDecision,
        scope: ApprovalScope,
    },
    /// Pause a run.
    PauseRun { run_id: codypendent_protocol::RunId },
    /// Resume a paused run.
    ResumeRun { run_id: codypendent_protocol::RunId },
    /// Cancel a run.
    CancelRun { run_id: codypendent_protocol::RunId },
    /// Queue steering text to apply at the next safe point.
    QueueSteering {
        run_id: codypendent_protocol::RunId,
        text: String,
    },

    // --- Docs Studio live editing (Phase 4 STEP 4.3 client wiring) ---
    /// Acquire (or renew) the edit lease over a document block before mutating it.
    /// The harness ensures it is subscribed to the document's sync stream first,
    /// then sends `AcquireDocumentLease`.
    AcquireDocumentLease {
        document_id: DocumentId,
        /// The block to lease (`None` = a whole-document structural lease).
        block_id: Option<String>,
    },
    /// Release a held document lease by its id.
    ReleaseDocumentLease { lease_id: String },
    /// Apply a semantic mutation to a document (a direct edit, a proposed edit, or
    /// an accept/reject of a suggestion). The daemon's collaboration mode decides
    /// whether a content edit applies directly or lands as a suggestion.
    MutateDocument {
        document_id: DocumentId,
        mutation: DocumentMutation,
    },

    /// Add a usable model from the TUI (client-only — NOT a daemon command). The
    /// harness maps this to local `models.toml` + `auth.json` writes and never
    /// sends an envelope, so it is intercepted in the drain loop before
    /// `intent_to_command`. `display_id` is the `models.toml` id (the flow
    /// defaults it to `<provider>/<model>`); `provider_id` selects the catalog
    /// entry the harness reads `base_url` from; `model` is the provider-side model
    /// name. `api_key` is the entered key for a hosted provider (redacted in
    /// `Debug`), or `None` for a local/no-auth provider.
    AddModel {
        display_id: String,
        provider_id: String,
        model: String,
        api_key: Option<SecretKey>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_debug_is_redacted() {
        let k = SecretKey("sk-super-secret".to_string());
        let dbg = format!("{k:?}");
        assert!(
            !dbg.contains("sk-super-secret"),
            "key redacted in Debug: {dbg}"
        );
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn add_model_intent_debug_redacts_the_key() {
        let intent = Intent::AddModel {
            display_id: "groq/llama".to_string(),
            provider_id: "groq".to_string(),
            model: "llama-3.1-8b".to_string(),
            api_key: Some(SecretKey("sk-secret".to_string())),
        };
        assert!(
            !format!("{intent:?}").contains("sk-secret"),
            "the key must never leak through the intent's Debug"
        );
    }
}

//! Application state and its projections (STEP 1.12 RULE 2).
//!
//! [`AppState`] is the single source of truth the renderer reads. It is mutated
//! only by [`crate::reduce::reduce`]; it holds no I/O handles. All state is
//! derived deterministically from the ordered [`SessionEvent`] stream plus local
//! navigation, so replaying the same events yields the same state.

use std::cell::{Cell, RefCell};

use chrono::{DateTime, Utc};

use ratatui::layout::Rect;

use codypendent_protocol::{
    AgentMode, ApprovalId, ArtifactRef, BudgetDimension, ChangeSetId, DocumentId, DocumentMutation,
    ModelId, ProposedAction, Risk, RunDisposition, RunId, RunState, ToolOutcome,
};

use crate::action::{Action, Intent, KeyTarget, SecretKey};
use crate::remote_ui_host::RemoteUiHostState;
use crate::theme::{Theme, ThemeVariant};

/// Maximum code-graph rows held in UI state at once. Shared by the renderer,
/// reducer paging logic, and the CLI's SQLite query so range labels and page
/// boundaries cannot drift.
pub const EDGE_PAGE_SIZE: usize = 100;

/// Which pane currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// Left pane: the session / run list.
    Sessions,
    /// Center pane: the transcript.
    Transcript,
    /// Right pane: pending approvals + run details.
    Approvals,
}

impl Pane {
    /// The next pane in `Tab` order.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Pane::Sessions => Pane::Transcript,
            Pane::Transcript => Pane::Approvals,
            Pane::Approvals => Pane::Sessions,
        }
    }
}

/// Which base layout the shell renders. Toggled at runtime (`F2` or the palette);
/// the composer and status footer are identical in both — only the region above
/// them changes, and the input model (composer / palette / approval modal) is the
/// same in each, so the panes are at-a-glance context, not a separate mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// The single-column conversation (the Claude Code / Codex feel). Default.
    #[default]
    Chat,
    /// Runs │ conversation │ approvals panes, for at-a-glance workspace state.
    Workspace,
}

impl LayoutMode {
    /// The other layout.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            LayoutMode::Chat => LayoutMode::Workspace,
            LayoutMode::Workspace => LayoutMode::Chat,
        }
    }
}

/// How the input layer should interpret the next key (see
/// [`crate::input::map_event`]). Derived from the active overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// A navigable overlay (Skills / Memory / Docs / Edges / Workflow /
    /// Blackboard / Help) is live: the arrow/command key table drives it.
    Normal,
    /// A text prompt is capturing printable keys.
    Editing,
    /// A yes/no confirmation is awaiting a decision.
    Confirm,
    /// The command palette is capturing a filter query while staying navigable
    /// (printable keys filter; arrows move the selection; Enter runs it).
    Palette,
    /// The base conversation view: the persistent composer captures typed text;
    /// `/` (on an empty composer) opens the palette; Enter sends; PgUp/PgDn
    /// scroll the transcript; Ctrl-↑/↓ switch runs.
    Composer,
    /// A pending approval owns the screen: only the decision keys (`a`/`A`/`r`)
    /// and selection arrows are live, so an approval is never typed past.
    Approval,
    /// A mounted public Remote UI surface owns focus.
    RemoteUi,
}

/// One model/role pair assembled by the host-owned council wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilMemberDraft {
    pub model: String,
    pub role: String,
}

/// The current page of the council creation wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CouncilBuilderStep {
    Name,
    Description,
    MemberModel,
    MemberRole,
    Chair,
    Rounds,
    Review,
}

/// Pure TUI state for creating a persisted multi-model council. The CLI harness
/// performs the eventual validated, atomic write; the renderer and reducer only
/// edit this draft and emit a client-local intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilBuilderState {
    pub step: CouncilBuilderStep,
    pub name: String,
    pub description: String,
    pub members: Vec<CouncilMemberDraft>,
    pub chair: Option<String>,
    pub rounds: u8,
    /// Filter text for the member/chair model pickers.
    pub query: String,
    /// Cursor into the current page's visible rows.
    pub selected: usize,
    /// Model awaiting a role on the MemberRole page.
    pub pending_member_model: Option<String>,
    pub role: String,
}

impl Default for CouncilBuilderState {
    fn default() -> Self {
        Self {
            step: CouncilBuilderStep::Name,
            name: String::new(),
            description: String::new(),
            members: Vec::new(),
            chair: None,
            rounds: 1,
            query: String::new(),
            selected: 0,
            pending_member_model: None,
            role: String::new(),
        }
    }
}

/// The top-most modal / overlay, if any. Text prompts carry their buffer inline.
///
/// `PartialEq` but NOT `Eq`: the add-model pick-list carries catalog prices as
/// floats, which have no total equality. Every comparison in the codebase is a
/// structural `==` / `assert_eq!`, which `PartialEq` alone serves.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Overlay {
    /// No overlay; the base layout is interactive.
    #[default]
    None,
    /// The help overlay listing key bindings.
    Help,
    /// Persistent setup and runtime diagnostics. Unlike the one-line transient
    /// notice, issues survive presence chatter and stay available until the
    /// operator clears them or restarts after resolving the cause.
    Issues,
    /// The new-run objective prompt (buffer inline).
    NewRun(String),
    /// The steering-text prompt (buffer inline).
    Steering(String),
    /// A "cancel this run?" confirmation.
    ConfirmCancel,
    /// The Skill Studio browser (STEP 2.6): the [`AppState::skills`] list plus a
    /// detail panel that shows the selected skill's description, risk, and its
    /// requested permissions **verbatim** ("skill permissions are visible").
    Skills,
    /// The memory browser (STEP 2.6): the [`AppState::memories`] list plus a
    /// provenance card. `source_open` is whether the focused memory's source has
    /// been revealed by the "open source" affordance — the TUI does no I/O, so
    /// opening surfaces the full source string in place; a real file-open is the
    /// CLI's job later ("every retrieved memory opens its source").
    Memory { source_open: bool },
    /// The Docs Studio browser (Phase 4 client wiring): the [`AppState::docs`]
    /// tree on the left, and the focused document's editor rail (its blocks) +
    /// review rail (its pending suggestions) on the right. Read-only — the live
    /// CRDT edit transport is a separate follow-up.
    Docs,
    /// The code-graph edge inspector (Phase 4 exit criterion 4): the
    /// [`AppState::edges`] list on the left and, for the focused edge, its
    /// relation, confidence, evidence kind + source, and revision on the right.
    Edges,
    /// Text prompt for the code-graph inspector's database-backed filter.
    EdgeSearch(String),
    /// The workflow-graph view (Phase 5 STEP 5.2, exit criterion 3): the
    /// [`AppState::workflow`] node list on the left and, for the focused node,
    /// its action, state, agent, workspace, approval, retry, dependencies, and
    /// declared outputs on the right. It is also the workflow control surface:
    /// start, pause/resume, retry-from-node, and cancel are wired to the daemon.
    Workflow,
    /// The blackboard view (Phase 5 STEP 5.3): the [`AppState::blackboard`] item
    /// list (the typed, attributed artifacts agents share within a workflow run —
    /// findings, decisions, patches, …) grouped by run, and — for the focused
    /// item — its kind, author, confidence, evidence, revision, and payload
    /// summary. The focused workflow run is subscribed while this view is open.
    Blackboard,
    /// The repository task board (rubric 10): the [`AppState::kanban`] cards laid
    /// out in status columns, with keyboard moves that supersede a card into its
    /// new column through the daemon. The repository's board channel is
    /// subscribed while this view is open, so a card an agent creates with
    /// `task.create` appears live.
    Kanban,
    /// Host-owned management surface for verified Remote UI plugins. Plugin
    /// code can never draw or intercept its own trust or revocation controls.
    UiPlugins,
    /// Confirm the exact daemon-issued update receipt after its permission diff
    /// has been shown in the plugin detail rail.
    ConfirmUiPluginApprove { plugin_id: String, receipt: String },
    /// Reject the exact pending update receipt.
    ConfirmUiPluginReject { plugin_id: String, receipt: String },
    /// Revoke the selected plugin and tear down its workers.
    ConfirmUiPluginRevoke { plugin_id: String },
    /// Confirm cancellation of a durable workflow run.
    ConfirmWorkflowCancel { workflow_run_id: String },
    /// JSON inputs for a new durable workflow run. Blank means `{}`.
    WorkflowInputs { workflow_id: String, buffer: String },
    /// The command palette: a searchable list of every command the TUI exposes,
    /// so the growing feature set stays reachable without consuming a single-key
    /// binding each. `query` is the live filter; `selected` indexes the filtered
    /// results (reset to 0 whenever the query changes). Opened with `/`.
    Palette { query: String, selected: usize },
    /// Host-owned council creation wizard. It selects only configured model
    /// profiles; persistence and final validation happen in the CLI harness.
    CouncilBuilder(CouncilBuilderState),
    /// The council browser (rubric 6 TUI wiring): the [`AppState::councils`]
    /// list on the left and, for the focused council, its chair/rounds/
    /// evidence/members on the right — the same list+detail shape as
    /// [`Overlay::Skills`]/[`Overlay::UiPlugins`]. `n` opens
    /// [`Overlay::CouncilBuilder`] to create a new one; `r` prompts for an
    /// objective and runs the focused council; `d` asks to remove it.
    CouncilBrowser,
    /// The council run-objective prompt (`r` from the browser): a free-text
    /// buffer for what the focused council should deliberate. Submitting
    /// emits `Intent::RunCouncil` and returns to the browser, which shows
    /// progress as it streams back until the host-driven run completes.
    CouncilRunObjective { name: String, buffer: String },
    /// Confirm removal of a council definition (`d` from the browser). Its
    /// prior saved run reports remain on disk — only the definition goes.
    ConfirmCouncilDelete { name: String },
    /// The Docs Studio block-edit prompt (Phase 4 STEP 4.3 client wiring): a
    /// single-line buffer for the text to insert into the focused block. On submit
    /// the reducer acquires the block's edit lease and, once granted, sends the
    /// `MutateDocument`; the daemon's collaboration mode decides whether it applies
    /// directly (Edit) or lands as a suggestion (Suggest). `block_id` is the block
    /// the edit targets, captured when the prompt opened.
    DocEdit {
        block_id: String,
        buffer: String,
        /// The block's text as it was when the prompt opened, prefilled into
        /// `buffer`. Submit sends a FULL REPLACE — delete exactly this many
        /// characters, then insert the buffer — so `e` is a real editor rather
        /// than the prepend-only insertion it used to be.
        original: String,
    },
    /// The Docs Studio new-document prompt: a single-line title buffer. Submit
    /// sends `CreateDocument` (rubric #4 — before this the Docs Studio browsed a
    /// set nothing could populate).
    DocNew { buffer: String },
    /// The Docs Studio new-block prompt: text for a paragraph inserted directly
    /// below the focused block (or at the top of an empty document).
    DocInsert { index: u32, buffer: String },
    /// Confirm deleting the focused block. Destructive and un-undoable from the
    /// TUI, so it never fires straight off a keypress.
    DocDeleteConfirm { block_id: String, label: String },
    /// Repository-relative Markdown path for publishing the focused document.
    DocPublishPath {
        document_id: DocumentId,
        buffer: String,
    },
    /// The model picker (MP1): a fuzzy-filterable list of the models
    /// selectable for a run (see [`AppState::models`]), opened from the
    /// command palette's `/model` entry. `query` filters by id/provider
    /// substring; `selected` indexes the filtered results (reset to 0
    /// whenever the query changes) — the same shape as [`Overlay::Palette`].
    /// Marks the model serving the active run as current; `Enter` stages the
    /// focused row on [`AppState::pending_model`] (advisory only this task —
    /// MP2 wires it to actually pin the next run's model).
    ModelPicker { query: String, selected: usize },
    /// The provider-catalog picker (Task 8): a fuzzy-filterable list of the
    /// providers selectable for a run (see [`AppState::providers`]), opened
    /// from the command palette's `/provider` entry. `query` filters by
    /// id/name/protocol substring; `selected` indexes the filtered results
    /// (reset to 0 whenever the query changes) — the same shape as
    /// [`Overlay::ModelPicker`]. `Enter` (or `Tab`) begins the add-model flow
    /// for the focused provider (model-discovery).
    ProviderPicker { query: String, selected: usize },
    /// The mode picker (PR C2 — plan mode): a fuzzy-filterable list of the
    /// submission modes for the next run (see [`MODE_CARDS`]), opened from the
    /// command palette's `/mode` entry. `query` filters by label/summary
    /// substring; `selected` indexes the filtered results (reset to 0 whenever
    /// the query changes) — the same shape as [`Overlay::ModelPicker`].
    /// `Enter` sets [`AppState::default_mode`]; the current default is marked
    /// in the list.
    ModePicker { query: String, selected: usize },
    /// The theme picker, opened from the command palette's `/theme` entry: the
    /// seven built-in variants plus any data-only packs the CLI loaded at boot
    /// ([`AppState::themes`]). The same filter/selection shape as
    /// [`Overlay::ModePicker`]; moving the cursor previews the focused theme
    /// across the WHOLE shell (see [`AppState::effective_theme`]), and `Enter`
    /// keeps it and asks the harness to persist it.
    ThemePicker { query: String, selected: usize },
    /// Add-model flow, free-text fallback (step 2): the provider-side model name,
    /// for the catalog provider chosen in step 1 (`provider_id`). `requires_key`
    /// was read from that provider's card. `api_key`:
    ///   `None`    = no key captured yet → today's rule (`requires_key` ? advance
    ///               to [`Overlay::AddModelKey`] : emit `Intent::AddModel` with `None`).
    ///   `Some(k)` = key already captured (a can-list provider's failed query fell
    ///               back here, possibly blank) → emit `AddModel` directly with `k`
    ///               (blank normalized to `None`), no re-prompt.
    /// A blank name is rejected (the prompt stays open).
    AddModelId {
        provider_id: String,
        requires_key: bool,
        api_key: Option<SecretKey>,
        buffer: String,
    },
    /// Add-model flow, step 3 (masked text prompt; key-requiring providers only):
    /// the API key for the chosen `provider_id` + `model`. `buffer` holds the key in
    /// a REDACTING newtype so it can never leak through `Debug`; the render masks it
    /// on screen. On submit, emits `Intent::AddModel` with the key handed to the
    /// harness (an empty key emits `api_key: None`).
    AddModelKey {
        provider_id: String,
        model: String,
        buffer: SecretKey,
    },
    /// Add-model flow, key-first masked prompt (hosted, can-list only), shown
    /// BEFORE the query. `buffer` is the redacting [`SecretKey`] newtype (masked
    /// in render). On submit: emit `Intent::QueryProviderModels { provider_id,
    /// api_key }` and open [`Overlay::AddModelQuerying`].
    AddModelProviderKey {
        provider_id: String,
        buffer: SecretKey,
    },
    /// Add-model flow, transient "Fetching models from <provider>…" state while
    /// the harness GETs. Holds `api_key` across the round trip so the fed-back
    /// `Action` need not carry it. Non-interactive except `Esc` (cancels; a late
    /// result is ignored via the `provider_id`/overlay match guard).
    AddModelQuerying {
        provider_id: String,
        api_key: Option<SecretKey>,
    },
    /// Add-model flow, the model pick-list — fuzzy-filterable like the
    /// model/provider pickers. `Enter` on a row → `Intent::AddModel { display_id:
    /// "<provider>/<picked>", provider_id, model: <picked>, api_key,
    /// context_tokens }`; `Esc` closes. `query` filters `models` by substring;
    /// `selected` indexes the filtered results (reset to 0 when the query
    /// changes). Rows carry the catalog metadata the harness merged onto the
    /// live listing (or the catalog rows alone when there is no listing);
    /// `origin` says which, so the header can be honest about it, and
    /// `refreshing` marks a manual `Ctrl-R` re-fetch still in flight.
    AddModelPick {
        provider_id: String,
        api_key: Option<SecretKey>,
        models: Vec<AddModelRow>,
        query: String,
        selected: usize,
        origin: ModelListOrigin,
        refreshing: bool,
    },
    /// The `/keys` overlay (D1): a fuzzy-filterable list of every configured
    /// model (see [`AppState::models`]) plus a final `Tavily (web.search)` row,
    /// each with its key status (see [`AppState::key_status`] /
    /// [`AppState::tavily_key_status`]). `query` filters by id/provider
    /// substring; `selected` indexes the filtered results (reset to 0 whenever
    /// the query changes) — the same shape as [`Overlay::ModePicker`]. `Enter`
    /// opens the masked set/replace prompt; `Delete` on a row with a stored key
    /// opens the remove confirm.
    ApiKeys { query: String, selected: usize },
    /// The `/keys` set/replace prompt (D1): a masked single-line buffer for the
    /// key being saved against `target`. `buffer` is the redacting [`SecretKey`]
    /// newtype (masked in render, redacted in `Debug`). On submit, emits
    /// [`Intent::SetApiKey`]; a blank buffer is rejected with a notice (nothing
    /// is written).
    ApiKeySet {
        target: KeyTarget,
        buffer: SecretKey,
    },
    /// The `/keys` remove confirmation (D1): `y`/`Enter` emits
    /// [`Intent::RemoveApiKey`]; `n`/`Esc` dismisses. Opened by `Delete` on a row
    /// whose status is [`KeyStatus::Stored`].
    ApiKeyRemoveConfirm { target: KeyTarget },

    /// Local models: browse the Unsloth GGUF catalog, step 1 of 4 — a
    /// fuzzy-filterable list of repos fetched from the Hugging Face Hub,
    /// opened from the command palette's "Local models: browse Unsloth
    /// catalog" entry. `loading` is `true` from the moment the palette
    /// command fires (`repos` empty, non-interactive — see
    /// [`AppState::input_mode`]) until the harness's
    /// [`crate::action::Intent::ListUnslothRepos`] round trip lands; `query`/
    /// `selected` are the same filterable-list shape as
    /// [`Overlay::ProviderPicker`]. `Enter` on a row begins step 2
    /// ([`Overlay::UnslothQuants`]).
    UnslothRepos {
        repos: Vec<UnslothRepoCard>,
        query: String,
        selected: usize,
        loading: bool,
    },
    /// Step 2: the quant variants (with sizes) for the repo chosen in
    /// [`Overlay::UnslothRepos`], fetched from the Hub's file tree. Same
    /// loading/filterable shape as step 1. `Enter` on a row begins step 3
    /// ([`Overlay::UnslothConfirmPull`]).
    UnslothQuants {
        repo_id: String,
        quants: Vec<UnslothQuantCard>,
        query: String,
        selected: usize,
        loading: bool,
    },
    /// Step 3: confirm the pull before it drives `ollama pull` (a real
    /// multi-gigabyte download) — the same y/n confirm shape as
    /// [`Overlay::ConfirmWorkflowCancel`]. `y`/`Enter` begins step 4
    /// ([`Overlay::UnslothPulling`]); `n`/`Esc` backs out to
    /// [`Overlay::None`] (mirrors every other confirm overlay here — none of
    /// them return to the picker they were opened from).
    UnslothConfirmPull {
        repo_id: String,
        quant: String,
        size_label: String,
    },
    /// Step 4: live `ollama pull` progress, then the registered-model notice
    /// once it completes. `lines` is the tail of parsed progress output
    /// (oldest first); `done` flips once the harness's pull task finishes
    /// (success or failure), at which point exactly one of `error` /
    /// `registered_id` is `Some`. Non-interactive except `Esc` (dismiss —
    /// the pull itself is NOT cancelled: it keeps running detached, and a
    /// late [`crate::action::Action::UnslothPullFinished`] is dropped by the
    /// same repo_id/quant match guard [`Overlay::AddModelQuerying`]'s docs
    /// describe for its own late-result case).
    UnslothPulling {
        repo_id: String,
        quant: String,
        lines: Vec<String>,
        done: bool,
        error: Option<String>,
        registered_id: Option<String>,
    },
}

/// The lifecycle of a single tool card in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// Proposed and awaiting policy / approval.
    Proposed,
    /// Executing.
    Running,
    /// Finished (see [`ToolCard::outcome`]).
    Completed,
}

/// A tool invocation rendered as an expandable card.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    /// Tool name, e.g. `shell.run` (empty until [`ToolStarted`] names it).
    ///
    /// [`ToolStarted`]: codypendent_protocol::EventBody::ToolStarted
    pub tool: String,
    pub status: ToolStatus,
    /// The proposed action (present when the card began as a proposal).
    pub action: Option<ProposedAction>,
    /// Digest of the tool arguments (never the arguments themselves).
    pub args_digest: Option<String>,
    /// A short, human-readable display label for the call, e.g. the file
    /// path a `workspace.read_file` targets — set from [`ToolStarted`]'s
    /// `label` field when present, `None` otherwise (an older daemon, or a
    /// tool `tool_label` does not recognize). A bounded display string, never
    /// the full arguments.
    ///
    /// [`ToolStarted`]: codypendent_protocol::EventBody::ToolStarted
    pub label: Option<String>,
    /// Terminal outcome, once completed.
    pub outcome: Option<ToolOutcome>,
    /// Bulk output reference, if the tool produced one.
    pub artifact: Option<ArtifactRef>,
    /// The approval this proposal is gated on, if any.
    pub approval_id: Option<ApprovalId>,
    /// Whether the card is expanded to show detail.
    pub expanded: bool,
}

/// A proposed patch / change set rendered as an expandable summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSummary {
    pub changeset_id: ChangeSetId,
    pub artifact: ArtifactRef,
    pub files: Vec<String>,
    pub additions: u64,
    pub deletions: u64,
    pub preview: String,
    pub preview_truncated: bool,
    pub expanded: bool,
}

/// One entry in a run's transcript. Streaming model text is coalesced into a
/// single [`TranscriptEntry::Model`] run; every other event kind is its own
/// entry so it can be selected and expanded independently.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEntry {
    /// The user's own message — the run objective, or a steering follow-up.
    User { text: String },
    /// Coalesced streamed model prose. `rendered` is the parse-once rich cache:
    /// `None` while streaming (render plain); `Some` once finalized (render rich).
    Model {
        text: String,
        rendered: Option<Vec<crate::markdown::RichLine>>,
    },
    /// A tool card (boxed: it is by far the largest variant).
    Tool(Box<ToolCard>),
    /// A proposed patch / change set.
    Patch(PatchSummary),
    /// A steering marker.
    Steering { applied: bool },
    /// A budget warning.
    Budget {
        dimension: BudgetDimension,
        used: u64,
        limit: u64,
    },
    /// The run's terminal marker. `expanded` (client-only view state, mirroring
    /// the other fold flags) reveals the full raw failure chain beneath the
    /// concise summary; `disposition` is the only wire data.
    Completed {
        disposition: RunDisposition,
        expanded: bool,
    },
    /// A note appended to the session. Long/multiline notes fold by default,
    /// mirroring [`ToolCard`]/[`PatchSummary`]; `expanded` is client-only view
    /// state — it is never part of the `NoteAppended` wire event.
    Note { text: String, expanded: bool },
    /// Folded backstage material for the run: the context manifest and
    /// curated-memory writes, which are real but not part of the visible
    /// conversation. Rendered as one dim, expandable line instead of a
    /// visible [`TranscriptEntry::Note`] cell — at most one per run (later
    /// `NoteAppended`s of either kind update the same entry's counts/`raw`
    /// rather than creating another). Entirely client-only view state; never
    /// part of the wire (the underlying `NoteAppended` events are unchanged).
    Backstage {
        /// The most recently seen context manifest's line count, or `None`
        /// if this run has not received one.
        context_lines: Option<usize>,
        /// How many curated-memory (`remembered:`) notes have folded in.
        memory_updates: usize,
        /// The full text of every folded note, in arrival order — revealed
        /// when `expanded`.
        raw: Vec<String>,
        /// Whether the raw bodies are shown below the summary line.
        expanded: bool,
    },
    /// A forward-compatibility placeholder for an event this build does not
    /// understand (protocol RULE 1: render, do not crash).
    Unsupported { label: String },
}

/// Notes at or under this many lines render inline; a longer note folds
/// (mirroring [`ToolCard`]/[`PatchSummary`]). Lives here, next to
/// [`TranscriptEntry::is_foldable`], so the renderer's click targets and the
/// reducer's keyboard walk can never disagree about which notes fold.
pub(crate) const NOTE_INLINE_LINE_THRESHOLD: usize = 2;

impl TranscriptEntry {
    /// Whether this entry renders a collapsible head — a tool card, a patch
    /// diff, the backstage fold, a long note, or a failed run's raw error
    /// chain. The single source of truth for RULE 3 parity here: the renderer
    /// registers a click target on exactly these entries, and `Alt-↑`/`Alt-↓`
    /// walk exactly these entries, so every fold reachable by mouse is
    /// reachable by keyboard and vice versa.
    #[must_use]
    pub fn is_foldable(&self) -> bool {
        match self {
            TranscriptEntry::Tool(_)
            | TranscriptEntry::Patch(_)
            | TranscriptEntry::Backstage { .. } => true,
            TranscriptEntry::Note { text, .. } => text.lines().count() > NOTE_INLINE_LINE_THRESHOLD,
            TranscriptEntry::Completed { disposition, .. } => {
                matches!(disposition, RunDisposition::Failed { .. })
            }
            _ => false,
        }
    }
}

/// A pending approval awaiting a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub approval_id: ApprovalId,
    pub action: ProposedAction,
    pub risk: Risk,
    /// The run this approval belongs to, when it can be inferred (a
    /// `ToolProposed` links an approval to a run; a bare `ApprovalRequested`
    /// does not carry the run id).
    pub run_id: Option<RunId>,
}

/// A run's current derived activity — never fetched, always folded from the
/// event stream (STEP 1.12 RULE 2): the reducer transitions it as it folds
/// run-state, streamed model text, and tool-lifecycle events, so the renderer
/// always has an explanation for a run that would otherwise look paused
/// between transcript updates. Defaults to [`RunActivity::Idle`] (a fresh run
/// has not started preparing yet).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RunActivity {
    /// Not running: queued, paused, awaiting approval/input, or terminal.
    #[default]
    Idle,
    /// Preparing or running, with no model text streaming and no tool in
    /// flight — the agent is composing its next step.
    Thinking,
    /// Model text is actively streaming into the transcript.
    Streaming,
    /// A tool is executing; carries the tool's name.
    RunningTool(String),
}

/// Everything known about one run, and its transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    pub run_id: RunId,
    pub objective: String,
    pub mode: AgentMode,
    pub state: RunState,
    /// The run's derived live-activity status: sets/clears as the reducer
    /// folds run-state, streaming, and tool-lifecycle events; the renderer
    /// shows it as a dim status row so a run is never silently paused with
    /// no explanation.
    pub activity: RunActivity,
    /// The model serving the run, learned from agent-authored events.
    pub model: Option<ModelId>,
    /// The worktree name, once known.
    pub worktree: Option<String>,
    /// Context-window usage percent, projected from the token budget.
    pub context_percent: Option<u16>,
    /// Cost so far, in minor currency units, projected from the cost budget.
    pub cost_minor: Option<u64>,
    pub disposition: Option<RunDisposition>,
    /// The ordered transcript.
    pub transcript: Vec<TranscriptEntry>,
    /// When each transcript entry's originating event occurred
    /// (`SessionEvent.occurred_at`), parallel to `transcript` — index `i` is
    /// entry `i`'s time. Kept in lockstep by [`AppState::push_entry`], the one
    /// writer of both, and read only through [`RunView::entry_time`]. Carried
    /// as a side vector rather than a field on every `TranscriptEntry` variant
    /// so that folding a time onto an entry costs nothing at the ~30 match
    /// sites that do not care about it.
    pub entry_times: Vec<DateTime<Utc>>,
    /// Selected transcript entry (for expand / detail).
    pub transcript_selected: usize,
    /// Transcript scroll offset in rows (used only when not following).
    pub scroll: u16,
    /// Whether the conversation is pinned to the latest content. `true` by
    /// default and after sending; scrolling up with PgUp leaves follow mode, and
    /// paging back to the bottom re-enters it. When following, the renderer shows
    /// the tail of the transcript regardless of `scroll`.
    pub follow: bool,
}

impl RunView {
    fn new(run_id: RunId, objective: String, mode: AgentMode) -> Self {
        Self {
            run_id,
            objective,
            mode,
            state: RunState::Queued,
            activity: RunActivity::Idle,
            model: None,
            worktree: None,
            context_percent: None,
            cost_minor: None,
            disposition: None,
            transcript: Vec::new(),
            entry_times: Vec::new(),
            transcript_selected: 0,
            scroll: 0,
            follow: true,
        }
    }

    /// When transcript entry `idx` arrived, if known. `None` for an entry
    /// pushed by a test straight into `transcript` (the reducer always goes
    /// through [`AppState::push_entry`]).
    #[must_use]
    pub fn entry_time(&self, idx: usize) -> Option<DateTime<Utc>> {
        self.entry_times.get(idx).copied()
    }
}

/// A Skill Studio card (STEP 2.6): one registry item projected for the Skills
/// browser. Self-contained — the TUI never depends on `codypendent-knowledge`;
/// the CLI harness maps each `RegistryItem` into this shape (the one place the
/// two worlds meet). Every field is a pre-rendered human string so the renderer
/// stays a pure projection.
///
/// `permissions` are the requested capabilities rendered **verbatim** (e.g.
/// `"filesystem_read: $REPOSITORY"`, `"command: cargo"`) — the exact strings the
/// package declared, never a paraphrase — so the "skill permissions are visible"
/// exit criterion holds at a glance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCard {
    /// The item's name (its registry identity within a scope).
    pub name: String,
    /// The kind label (`tool` / `skill` / `plugin` / `hook` / `command`).
    pub kind: String,
    /// The scope the item is installed at (e.g. `system`, `workspace …`).
    pub scope: String,
    /// The provenance trust tier (`untrusted` … `first-party`).
    pub trust: String,
    /// The lifecycle status (`draft` / `active` / `modified` / `deprecated`).
    pub status: String,
    /// The coarse risk class (`safe` / `low` / `medium` / `high`).
    pub risk: String,
    /// The item's description (untrusted content; shown, never trusted).
    pub description: String,
    /// The requested capabilities, one verbatim string per capability.
    pub permissions: Vec<String>,
}

/// A council browser row (rubric 6 TUI wiring): one persisted council
/// definition projected for the `/council` browser. Self-contained — the CLI
/// maps a `councils.toml` entry (`crate::council::CouncilDefinition`, a
/// cli-crate-only type this dependency-free crate cannot name) into this
/// plain struct, exactly like [`SkillCard`]/[`ModelCard`] above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilCard {
    pub name: String,
    pub description: String,
    pub chair: String,
    pub rounds: u8,
    /// Evidence mode (rubric 6): members explore the repository read-only and
    /// cite `file:line` instead of reasoning with no tools.
    pub evidence: bool,
    /// `(model, role)` per member, in the order they deliberate.
    pub members: Vec<(String, String)>,
}

/// The plain, host-formatted result of one off-thread council run (rubric 6
/// TUI wiring), handed back through `ReaderSignal::CouncilRunFinished` and
/// folded into the transcript by [`crate::reduce::reduce`]. Pre-formatted
/// host-side (participant lines, cost line) so this dependency-free crate
/// never needs to name `crate::council`'s types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilRunSummary {
    /// The chair's final synthesis text, verbatim.
    pub synthesis: String,
    /// One attributed line per member plus the chair (model, role, session,
    /// run id, and measured usage where measured).
    pub participants: Vec<String>,
    /// The measured-only aggregate cost line (never a fabricated estimate).
    pub cost_line: String,
    /// Where the durable JSON+Markdown report landed, for the transcript note.
    pub report_markdown: String,
}

/// A memory provenance card (STEP 2.6): one curated memory projected for the
/// Memory browser. Also self-contained — the CLI maps a `MemoryRecord` (via its
/// `ProvenanceCard`) into it. The renderer draws the Chapter 06 provenance card
/// (statement, source, revision, scope, confidence) from these fields alone.
///
/// `source` is a human rendering of the memory's evidence ref (e.g. `"events
/// 3..7 of session <id>"` or `"artifact <ref> (path)"`); the "open source"
/// affordance surfaces it in full so every retrieved memory opens its source.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCard {
    /// The remembered fact.
    pub statement: String,
    /// The memory class (`semantic` / `procedural` / `preference` / …).
    pub class: String,
    /// The scope the memory lives in (cross-repository isolation is enforced in
    /// the store, never here).
    pub scope: String,
    /// The revision the memory is valid from.
    pub revision: String,
    /// When the memory was observed (a date string).
    pub observed: String,
    /// The curator's confidence in the fact, in `[0, 1]`.
    pub confidence: f32,
    /// The human-readable evidence source (what "open source" reveals).
    pub source: String,
}

/// A Docs Studio card (STEP 4.x client wiring): one [`KnowledgeDocument`]
/// projected for the Docs browser's tree/editor/review rails. Self-contained —
/// the TUI never depends on `codypendent-knowledge`; the CLI maps a document
/// snapshot (plus its pending suggestions) into this shape. Every field is a
/// pre-rendered human string so the renderer stays a pure projection.
///
/// [`KnowledgeDocument`]: (mapped by the CLI from `codypendent-knowledge`)
#[derive(Debug, Clone, PartialEq)]
pub struct DocCard {
    /// The document's stable id — the key that correlates an incoming
    /// [`DocumentSync`](codypendent_protocol::DocumentSync) (merged into the
    /// client replica by the CLI harness) back to this card, and the target of an
    /// edit's `MutateDocument`/`AcquireDocumentLease`.
    pub document_id: DocumentId,
    /// The document title (its heading in the tree).
    pub title: String,
    /// The scope the document lives in (e.g. `system`, `workspace …`).
    pub scope: String,
    /// The lifecycle status (`draft` / `in_review` / `published` / `archived`).
    pub status: String,
    /// The collaboration mode governing agent edits (`ask` / `suggest` / `edit`
    /// / `co_author` / `review` / `maintain`) — org docs default to `suggest`.
    pub mode: String,
    /// The document's monotonic revision, pre-rendered (e.g. `"r7"`).
    pub revision: String,
    /// The rendered blocks (the editor rail), in document order.
    pub blocks: Vec<DocBlockView>,
    /// The pending suggestions on the document (the review rail).
    pub suggestions: Vec<DocSuggestionView>,
}

/// One rendered document block (the editor rail). `text` is the block's primary
/// text or a structured-block label — never the raw serialized content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocBlockView {
    /// The block's stable id — the target an edit action leases and mutates (never
    /// rendered; carried so the reducer can name the block without a second lookup).
    pub id: String,
    /// The block kind label (`heading` / `paragraph` / `code` / …).
    pub kind: String,
    /// A one-line human rendering of the block's content.
    pub text: String,
    /// The block's primary text VERBATIM (newlines included), or `None` for a
    /// structured/embed block that has no single editable text container. This
    /// is what the `e` prompt prefills and what its full-replace deletes — the
    /// one-line `text` above is display-only and lossy.
    pub editable: Option<String>,
}

/// One pending suggestion on a document (the review rail): a proposed
/// replacement over a character range, with its author and rationale. Rendered
/// read-only — accept/reject is a later live-transport concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSuggestionView {
    /// The suggestion's stable id — the target of an accept/reject
    /// `MutateDocument` (never rendered; carried so the reducer can resolve the
    /// focused suggestion without a second lookup).
    pub id: String,
    /// The suggestion status (`pending` for the review rail).
    pub status: String,
    /// Who proposed it, pre-rendered (e.g. `"agent"` / `"human"`).
    pub author: String,
    /// The character range it targets, pre-rendered (e.g. `"12..40"`).
    pub range: String,
    /// The proposed replacement text.
    pub replacement: String,
    /// The proposer's rationale, when given.
    pub rationale: Option<String>,
}

/// Which rail of the Docs Studio overlay the keyboard drives (Phase 4 client
/// wiring). Defaults to [`DocFocus::Tree`] so the overlay opens on the document
/// list exactly as before this focus existed; `Tab` cycles it, and `↑/↓` then move
/// the selection within the focused rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocFocus {
    /// The document tree (left): `↑/↓` move [`AppState::selected_doc`].
    #[default]
    Tree,
    /// The editor rail (right, top): `↑/↓` move [`AppState::selected_block`]; `e`
    /// edits the focused block.
    Editor,
    /// The review rail (right, bottom): `↑/↓` move [`AppState::selected_suggestion`];
    /// `a`/`r` accept/reject the focused suggestion.
    Review,
}

impl DocFocus {
    /// The next rail in `Tab` order (Tree → Editor → Review → Tree).
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            DocFocus::Tree => DocFocus::Editor,
            DocFocus::Editor => DocFocus::Review,
            DocFocus::Review => DocFocus::Tree,
        }
    }
}

/// The state of the client's edit lease over one document block (Phase 4 client
/// wiring, presence-lite). Surfaced in the Docs editor rail so a writer sees
/// whether it holds the block or is blocked by another writer — the one
/// status-line touch the collaboration slice needs (no cursors/presence UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocLeaseState {
    /// `AcquireDocumentLease` sent; awaiting the grant.
    Acquiring,
    /// The lease is held — edits may apply.
    Held,
    /// The range is leased by another writer (`document.range-leased`).
    Blocked,
}

/// One in-flight document edit: the block being leased/edited, the lease's
/// lifecycle, and the mutation queued until the lease is granted. The reducer
/// stores this on [`AppState::doc_edit`] so the lease→mutate handshake — inherently
/// two round-trips — is driven by folding the daemon's replies, keeping the TUI a
/// pure reducer.
#[derive(Debug, Clone, PartialEq)]
pub struct DocEdit {
    /// The document being edited.
    pub document_id: DocumentId,
    /// The block the lease covers (`None` would be a whole-document structural
    /// lease; the editor only takes block leases).
    pub block_id: Option<String>,
    /// Where the lease is in its lifecycle.
    pub lease: DocLeaseState,
    /// The granted lease id, once held — the capability needed to release it.
    pub lease_id: Option<String>,
    /// The mutation to send once the lease is granted, then cleared (fired once).
    pub pending: Option<DocumentMutation>,
}

/// A code-graph edge projected for the graph-edge inspector (Phase 4 exit
/// criterion 4: "graph edges expose evidence + revision"). Self-contained — the
/// CLI maps a `CodeEdge` (resolving its endpoint node ids to qualified names)
/// into this shape. Every field is a pre-rendered human string.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdgeCard {
    /// The source symbol's qualified name (or a fallback id when unresolved).
    pub from: String,
    /// The target symbol's qualified name (or a fallback id when unresolved).
    pub to: String,
    /// The relation label (`calls` / `defines` / `imports` / …).
    pub relation: String,
    /// The edge confidence in `[0, 1]` — the tier its evidence earns.
    pub confidence: f32,
    /// The evidence layer that produced it (`syntax_inferred` / `lsp_resolved`
    /// / `compiler_resolved` / `runtime_observed`).
    pub evidence_kind: String,
    /// A human rendering of the descriptive evidence ref, or `"(none)"`.
    pub evidence: String,
    /// The git revision the edge was recorded at.
    pub revision: String,
}

/// A workflow-graph node projected for the workflow view (Phase 5 STEP 5.2, exit
/// criterion 3: "per-node state, cost, agent, worktree"). Self-contained — the
/// TUI never depends on `codypendent-workflow`; the CLI compiles a
/// `workflow.yaml` manifest and maps each `CompiledNode` (overlaid with the
/// durable node record's state/cost when a run exists) into this shape. Every
/// field is a pre-rendered human string so the renderer stays a pure projection.
///
/// Nodes are listed in the compiled topological order, grouped by their
/// `workflow` label, so the view reads as an ordered graph rather than a flat
/// pile when a repository declares more than one workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowNodeCard {
    /// Stable manifest id used by `StartWorkflow`.
    pub workflow_id: String,
    /// The owning workflow, pre-rendered (e.g. `"repair-github-check v1"`), so
    /// several workflows can share the list under labeled groups.
    pub workflow: String,
    /// Latest durable run for this workflow, when one exists.
    pub workflow_run_id: Option<String>,
    /// Latest durable run phase (`not started` before the first run).
    pub run_phase: String,
    /// Declared workflow inputs, pre-rendered (`name:type*`, `*` = required).
    pub inputs: String,
    /// The node (step) id, unique within its workflow.
    pub id: String,
    /// The node's action, pre-rendered (e.g. `"agent implementer · skill
    /// code.repair"` or `"tool repository.test"`).
    pub action: String,
    /// The action kind label (`agent` / `tool`) — drives the list glyph color.
    pub kind: String,
    /// The node's lifecycle state, pre-rendered (`pending` until a durable run
    /// record overlays a live state such as `running` / `completed`).
    pub state: String,
    /// The agent role, when this is an agent node, else `"—"`.
    pub agent: String,
    /// The model-selection policy for an agent node, else `"—"`.
    pub model_policy: String,
    /// How the node's workspace is provisioned (`shared worktree` / `isolated
    /// worktree`) — the exit-criterion "worktree" field.
    pub workspace: String,
    /// The approval policy gating the node (`before write` / `always` / `none`).
    pub approval: String,
    /// The retry policy, pre-rendered (e.g. `"1 attempt"` / `"2 attempts · 5s
    /// backoff"`).
    pub retry: String,
    /// The nodes this one depends on, pre-rendered (comma-joined, or `"—"`).
    pub depends_on: String,
    /// The same dependencies as raw node ids — the graph's **edges** (rubric 5),
    /// which the pre-rendered [`depends_on`](Self::depends_on) string above cannot
    /// be parsed back out of. The workflow pane lays these out into ASCII lanes;
    /// empty means a root node (or a projection that predates edges, which simply
    /// renders the flat list it always did).
    pub depends_on_ids: Vec<String>,
    /// The blackboard artifact kinds the node declares to produce, pre-rendered
    /// (comma-joined, or `"—"`).
    pub outputs: String,
    /// The node's MEASURED cost, pre-rendered (e.g. `"12s · 3 tool calls"`, or
    /// `"—"` until a durable run records one). Only measured dimensions appear —
    /// never a fabricated token/USD figure (Phase 5 T8).
    pub cost: String,
    /// The node's latest failure or budget-block reason when a durable run
    /// recorded one (P5-D4), else `"—"`. Surfaced in the node detail so a
    /// `failed`/`blocked` node explains itself.
    pub error: String,
}

/// A blackboard artifact projected for the blackboard view (Phase 5 STEP 5.3).
/// Self-contained — the TUI never depends on `codypendent-workflow`; the CLI maps
/// a `BlackboardItem` (its opaque JSON payload/author/evidence rendered to human
/// strings) into this shape. Items are grouped by their `run` label, so several
/// workflow runs' boards read as labeled groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlackboardItemCard {
    /// Stable artifact id, used to merge live revisions idempotently.
    pub id: String,
    /// Stable owning workflow-run id, used for subscriptions and replacement.
    pub workflow_run_id: String,
    /// The owning workflow run, pre-rendered (e.g. `"repair-github-check · run
    /// 0f2a"`), so several runs' boards share the list under labeled groups.
    pub run: String,
    /// The artifact kind, pre-rendered (`finding` / `decision` / `proposed_patch`
    /// / …).
    pub kind: String,
    /// A one-line human summary of the artifact's payload.
    pub summary: String,
    /// Who produced it, pre-rendered from the author record (e.g. `"agent
    /// investigator"`).
    pub author: String,
    /// The producer's confidence, pre-rendered (`"0.85"` or `"—"`).
    pub confidence: String,
    /// The evidence backing the artifact, pre-rendered (e.g. `"2 ref(s)"` or
    /// `"—"`) — claim-like kinds always carry it.
    pub evidence: String,
    /// The artifact's revision, pre-rendered (e.g. `"r1"`).
    pub revision: String,
    /// Whether this item has been superseded by a later revision (the review
    /// rail shows the live item; a superseded one is dimmed).
    pub superseded: bool,
}

/// The board columns the kanban pane renders, in display order (rubric 10).
///
/// A card's `status` is a free string in the store — a team may grow its own
/// columns — but these four are the defaults every write lands in and every
/// client renders first. A card whose status is none of them is shown in the
/// first column rather than dropped, so an unknown column never hides work.
pub const KANBAN_COLUMNS: [&str; 4] = ["todo", "doing", "review", "done"];

/// One backlog card projected for the kanban board (rubric 10).
///
/// Self-contained, like [`BlackboardItemCard`]: the TUI never depends on
/// `codypendent-workflow`, so the CLI renders the stored item's opaque JSON
/// payload into these strings. A card IS a blackboard item of kind `task` living
/// on the repository's board, so it carries the same id and supersession-aware
/// identity — a move republishes the card at a new revision and the pane merges
/// it by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanCard {
    /// Stable card id — what a move/update command names, and the merge key for
    /// a live delivery.
    pub id: String,
    /// The card's one-line title, rendered from the stored payload.
    pub title: String,
    /// The column the card sits in (one of [`KANBAN_COLUMNS`], or a team's own).
    pub status: String,
    /// Who the card is assigned to, pre-rendered (`"—"` when nobody).
    pub assignee: String,
    /// The artifact kind, pre-rendered — `task` for a backlog card, but a board
    /// can also hold a promoted `decision` or `open_question`, so the kind is
    /// shown rather than assumed.
    pub kind: String,
    /// Who created (or last revised) it, pre-rendered (e.g. `"agent"` /
    /// `"operator"`), so a human can see which cards the model wrote.
    pub author: String,
    /// The card's position within its column (lower sorts first).
    pub ordinal: i64,
}

/// Where a model-picker card's model runs (MP1). A tui-local mirror of just
/// the two labels `codypendent_routing::ModelLocation` carries — the `tui`
/// crate speaks only `codypendent-protocol` and must never depend on
/// `codypendent-routing` (STEP 1.12 RULE 1), so this is a self-contained copy
/// the CLI harness maps a measured model profile's location onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelLocationLabel {
    /// On-device (embedded, subprocess, or LAN service treated as local).
    Local,
    /// Off-device (a hosted/cloud provider).
    Hosted,
}

/// Truthful readiness of a configured model at the time the TUI loaded it.
/// Local endpoints are actively verified; hosted models remain unverified
/// until an explicit deep check so startup never makes surprise cloud calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelReadiness {
    Ready,
    Unverified,
    Unavailable(String),
}

/// A model-picker card (MP1): one selectable model from `models.toml`,
/// enriched with its measured profile from the `model_profiles` table when one
/// exists. Self-contained — the TUI never depends on `codypendent-routing`;
/// the CLI harness maps a `ModelConfig` (plus any matching measured profile)
/// into this shape, exactly as it maps a `RegistryItem` into a [`SkillCard`].
/// "current" is not a field here: the [`Overlay::ModelPicker`] browser
/// computes it at render by comparing `id` to the active run's serving model
/// (`RunView::model`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCard {
    /// The model's id, as configured in `models.toml`.
    pub id: ModelId,
    /// The wire-protocol adapter this model uses (e.g. `"openai-compatible"`
    /// — the only value Phase 1 supports; see `ModelConfig::provider`).
    pub provider: String,
    /// Whether the exact provider-side model has been verified available.
    pub readiness: ModelReadiness,
    /// Where the model runs, when a measured profile exists. `None` when the
    /// model has no `model_profiles` row (badges are best-effort;
    /// `models.toml` is the authoritative selectable list — STEP 1.9).
    pub location: Option<ModelLocationLabel>,
    /// The measured blended cost per 1K tokens, in USD, when a profile
    /// exists.
    pub cost_per_1k_usd: Option<f64>,
    /// The model's declared context window, in tokens, when a profile
    /// exists.
    pub context_tokens: Option<u64>,
}

/// The indices into `models` whose id or provider case-insensitively contains
/// `query` — the model picker's substring filter, in list order (mirrors
/// [`crate::palette::filtered`]'s shape, adapted to instance data rather than
/// a static table). An empty query matches every model. A free function
/// (rather than an `AppState` method) so a caller already holding a live
/// borrow of `AppState::overlay` can pass `&state.models` directly alongside
/// it without a borrow conflict.
#[must_use]
pub(crate) fn filter_models(models: &[ModelCard], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    models
        .iter()
        .enumerate()
        .filter(|(_, card)| {
            needle.is_empty()
                || card.id.0.to_lowercase().contains(&needle)
                || card.provider.to_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Configured model rows matching a council-picker query, excluding profiles
/// already serving as members. The chair picker deliberately uses
/// [`filter_models`] instead because a member may also chair the synthesis.
#[must_use]
pub(crate) fn filter_council_member_models(
    models: &[ModelCard],
    query: &str,
    members: &[CouncilMemberDraft],
) -> Vec<usize> {
    filter_models(models, query)
        .into_iter()
        .filter(|idx| {
            models
                .get(*idx)
                .is_some_and(|card| !members.iter().any(|member| member.model == card.id.0))
        })
        .collect()
}

/// One offerable model in the add-model pick-list: the provider-side id plus
/// whatever metadata could be attached to it. The id is the only required
/// field — a provider that answers `/models` with bare ids still produces a
/// complete row, just with empty columns. The CLI harness builds these by
/// merging the live `/models` response with the built-in catalog's `[[model]]`
/// rows for that provider (catalog metadata attached where ids match, and
/// catalog-only rows offered when the provider has no listing endpoint or the
/// request failed), so the picker is never a dead end.
#[derive(Debug, Clone, PartialEq)]
pub struct AddModelRow {
    /// The provider-side model id, exactly as it must be sent on the wire.
    pub id: String,
    /// A human display name, when the catalog or the provider supplied one.
    pub name: Option<String>,
    /// The model's context window in tokens, when known.
    pub context_tokens: Option<u64>,
    /// USD per 1M input tokens, when known. DISPLAY-ONLY — never summed into
    /// a budget (the catalog's own honesty rule).
    pub cost_per_1m_input_usd: Option<f64>,
    /// USD per 1M output tokens, when known. Display-only, as above.
    pub cost_per_1m_output_usd: Option<f64>,
    /// Whether the provider itself listed this model just now. `false` marks a
    /// catalog-only row: offerable, but unconfirmed against the live endpoint.
    pub live: bool,
}

impl AddModelRow {
    /// A bare live row — the shape a provider that answers with ids only
    /// produces, before any catalog metadata is merged onto it.
    #[must_use]
    pub fn live(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            context_tokens: None,
            cost_per_1m_input_usd: None,
            cost_per_1m_output_usd: None,
            live: true,
        }
    }
}

/// Where an add-model pick-list's rows came from, for an honest header: the
/// operator must be able to tell a confirmed live listing from a catalog-only
/// offering (which may name a model this account cannot actually reach).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelListOrigin {
    /// Fetched from the provider's `/models` endpoint just now.
    Live,
    /// Seeded from `<data_dir>/model_lists/<provider>.json`; the string is a
    /// human age label ("4m ago"), never a raw timestamp to parse.
    Cached(String),
    /// The built-in catalog only — the provider has no listing endpoint, or
    /// the request failed. The string is the key-free reason, when there was
    /// one.
    Catalog(String),
}

/// The indices into `models` whose id or display name case-insensitively
/// contains `query` — the add-model pick-list's substring filter, in list
/// order. Mirrors [`filter_models`] adapted to [`AddModelRow`]. An empty query
/// matches every row.
#[must_use]
pub(crate) fn filter_model_names(models: &[AddModelRow], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    models
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            needle.is_empty()
                || row.id.to_lowercase().contains(&needle)
                || row
                    .name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&needle))
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// How a model's (or the Tavily `web.search` integration's) API key is
/// configured, projected for the `/keys` overlay (D1). Loaded by the CLI
/// harness from `auth.json` + `models.toml` and folded in via
/// [`Action::ApiKeyStatusesLoaded`](crate::action::Action::ApiKeyStatusesLoaded)
/// — the tui crate does no I/O. Carries NO key material: the env variant holds
/// the variable NAME (shown to the operator), never its value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KeyStatus {
    /// A key is saved in `auth.json` under this id (rendered `●`).
    Stored,
    /// No `auth.json` entry, but `models.toml` declares an `api_key_env` —
    /// the NAME is shown (rendered `◐ env NAME`), never the value.
    Env(String),
    /// No key configured anywhere (rendered `○`). The default: a fresh state
    /// has loaded no statuses yet, which renders identically to "missing".
    #[default]
    Missing,
}

/// The indices into the `/keys` row list whose model id or provider
/// case-insensitively contains `query` — the overlay's substring filter, in
/// list order. The row list is `models` followed by one final Tavily
/// `web.search` row at index `models.len()`; the Tavily row matches the
/// `"tavily (web.search)"` label. An empty query matches every row. Mirrors
/// [`filter_models`], extended by the one non-model row.
#[must_use]
pub(crate) fn filter_key_rows(models: &[ModelCard], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    let mut indices: Vec<usize> = models
        .iter()
        .enumerate()
        .filter(|(_, card)| {
            needle.is_empty()
                || card.id.0.to_lowercase().contains(&needle)
                || card.provider.to_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect();
    if needle.is_empty() || "tavily (web.search)".contains(&needle) {
        indices.push(models.len());
    }
    indices
}

/// The [`KeyTarget`] a `/keys` row index addresses: indices into `models` are
/// that model's id; `models.len()` is the final Tavily `web.search` row (see
/// [`filter_key_rows`]).
#[must_use]
pub(crate) fn key_row_target(models: &[ModelCard], idx: usize) -> KeyTarget {
    match models.get(idx) {
        Some(card) => KeyTarget::Model(card.id.0.clone()),
        None => KeyTarget::Tavily,
    }
}

/// One provider-catalog row for the `/provider` picker projection (Task 8).
/// The TUI performs no I/O; the CLI harness seeds this from
/// `codypendent_providers::Catalog` (the built-in ~40-provider catalog,
/// layered with any user `providers.toml`), exactly as it maps a
/// `ModelConfig` into a [`ModelCard`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCard {
    /// The provider's id, as configured in the catalog (e.g. `"groq"`).
    pub id: String,
    /// The provider's display name (e.g. `"Groq"`).
    pub name: String,
    /// Wire protocol label, e.g. `"openai-chat"` | `"anthropic"` | `"acp"`.
    pub protocol: String,
    /// Auth label, e.g. `"api-key: GROQ_API_KEY"` | `"none"` | `"acp: npx"`.
    pub auth: String,
    /// On-device (Ollama/LM Studio/vLLM) vs. hosted.
    pub local: bool,
    /// Whether adding a model from this provider needs an API key (its first auth
    /// method is `ApiKey`). Drives the add-model flow's key step — a local/no-auth/
    /// ACP provider skips it. Set by the CLI harness from the catalog `AuthMethod`.
    pub requires_key: bool,
    /// Whether this provider can serve an OpenAI-compatible `/models` list:
    /// protocol is `OpenAiChat`, a `base_url` is set, and the first auth method
    /// is `ApiKey` or `None` (or absent). Set by the harness
    /// (`provider_can_list_models`), mirroring `requires_key`. Drives the
    /// Enter/Tab branch: `true` → live pick-list; `false` → today's free-text flow.
    pub can_list_models: bool,
    /// Whether this build can actually execute models configured from this
    /// provider. Catalog entries for native Anthropic/Gemini, ACP, OAuth, and
    /// cloud-IAM remain discoverable, but are disabled until their runtime
    /// adapter/auth flow is wired — never presented as a successful add.
    pub available: bool,
    /// How many curated `[[model]]` rows the provider catalog ships for this
    /// provider. Non-zero means the add flow has something to offer even when
    /// the provider has no `/models` endpoint (Perplexity) or the request
    /// fails — the picker is never a free-text dead end.
    pub catalog_models: usize,
    /// Whether a key for this provider is already stored in `auth.json` (under
    /// the provider-wide `provider/<id>` entry). Set by the harness; the
    /// add-model flow uses it to skip re-prompting for a key it already holds.
    pub has_key: bool,
}

/// The indices into `providers` whose id/name/protocol case-insensitively
/// contains `query` — the provider picker's substring filter, in list order.
/// Mirrors [`filter_models`] exactly, adapted to [`ProviderCard`] fields. An
/// empty query matches every provider.
#[must_use]
pub(crate) fn filter_providers(providers: &[ProviderCard], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    providers
        .iter()
        .enumerate()
        .filter(|(_, card)| {
            needle.is_empty()
                || card.id.to_lowercase().contains(&needle)
                || card.name.to_lowercase().contains(&needle)
                || card.protocol.to_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// One Unsloth GGUF repo row for the "Local models: browse Unsloth catalog"
/// overlay ([`Overlay::UnslothRepos`]). Every field is pre-rendered by the
/// CLI harness from the Hugging Face Hub discovery client
/// (`codypendent_integrations::unsloth`) — the tui crate performs no
/// formatting, mirroring [`ModelCard`]/[`ProviderCard`]'s convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnslothRepoCard {
    /// The full repo id, e.g. `unsloth/Qwen3-32B-GGUF`.
    pub id: String,
    /// Pre-rendered download count, e.g. `"6.6M downloads"`.
    pub downloads_label: String,
    /// Pre-rendered like count, e.g. `"891 likes"`.
    pub likes_label: String,
    /// Pre-rendered last-updated date, or `"updated unknown"` when the Hub
    /// reported none — never a fabricated date.
    pub updated_label: String,
}

/// The indices into `repos` whose id case-insensitively contains `query` —
/// the Unsloth repo browser's substring filter, in list order. Mirrors
/// [`filter_providers`]. An empty query matches every repo.
#[must_use]
pub(crate) fn filter_unsloth_repos(repos: &[UnslothRepoCard], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    repos
        .iter()
        .enumerate()
        .filter(|(_, card)| needle.is_empty() || card.id.to_lowercase().contains(&needle))
        .map(|(idx, _)| idx)
        .collect()
}

/// One quant-variant row for a chosen Unsloth repo
/// ([`Overlay::UnslothQuants`]). Pre-rendered by the CLI harness from
/// `codypendent_integrations::unsloth::QuantVariant`; `size_bytes` rides
/// alongside the label so a later step (the confirm dialog) can reuse it
/// without re-parsing the display string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnslothQuantCard {
    /// The quant tag, e.g. `UD-Q4_K_XL` — passed verbatim to `ollama pull
    /// hf.co/<repo>:<quant>` and then used as the registered model id.
    pub quant: String,
    /// Pre-rendered download size, e.g. `"18.7 GB"`.
    pub size_label: String,
    /// How many split files make up this quant (`1` for the common case).
    pub file_count: usize,
    /// The raw combined size, carried through for the confirm step.
    pub size_bytes: u64,
}

/// The indices into `quants` whose quant tag case-insensitively contains
/// `query`. Mirrors [`filter_unsloth_repos`]. An empty query matches every
/// quant.
#[must_use]
pub(crate) fn filter_unsloth_quants(quants: &[UnslothQuantCard], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    quants
        .iter()
        .enumerate()
        .filter(|(_, card)| needle.is_empty() || card.quant.to_lowercase().contains(&needle))
        .map(|(idx, _)| idx)
        .collect()
}

/// One row of the `/mode` picker (PR C2 — plan mode): a submission mode the
/// operator can pick for the next run. The table is static — the mode set is a
/// protocol enum, not configured data — so "current" is not a field here
/// either: the picker marks the row whose `mode` equals
/// [`AppState::default_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeCard {
    /// The mode `Enter` stages on [`AppState::default_mode`].
    pub mode: AgentMode,
    /// The row's display label (also the status line's `next` field).
    pub label: &'static str,
    /// A one-line summary of what the mode permits, shown under the label.
    pub summary: &'static str,
}

/// The modes the `/mode` picker offers, in presentation order (the read-only
/// modes first, Build last-but-one so the read-only default-adjacent modes
/// read top-down). Summaries mirror the policy overlay each mode maps to.
pub(crate) const MODE_CARDS: &[ModeCard] = &[
    ModeCard {
        mode: AgentMode::Ask,
        label: "Ask",
        summary: "read-only Q&A — no writes, commands, or network",
    },
    ModeCard {
        mode: AgentMode::Explore,
        label: "Explore",
        summary: "read-only investigation — no writes, commands, or network",
    },
    ModeCard {
        mode: AgentMode::Plan,
        label: "Plan",
        summary: "investigate read-only, then finish with a numbered implementation plan",
    },
    ModeCard {
        mode: AgentMode::Build,
        label: "Build",
        summary: "full worktree access — writes, commands, and network (the default)",
    },
    ModeCard {
        mode: AgentMode::Review,
        label: "Review",
        summary: "read-only verification with commands — no writes or network",
    },
];

/// One selectable theme: a built-in variant, or a data-only theme pack the CLI
/// loaded from `<data-dir>/themes/<id>.toml` at boot. The resolved [`Theme`]
/// travels with the row so the picker can preview it live — the TUI crate
/// performs no I/O, so a pack's colours must arrive already parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    /// The id `--theme`/`CODYPENDENT_THEME` and the persisted preference use.
    pub id: String,
    /// What this theme is for, in one line.
    pub summary: String,
    /// The colours themselves.
    pub theme: Theme,
    /// `true` for a data-only pack, `false` for a built-in variant.
    pub pack: bool,
}

/// The seven built-in variants, in the order the picker lists them: the two
/// everyday themes first, then the accessibility variants, then the
/// reduced-depth fallbacks.
#[must_use]
pub fn builtin_theme_choices() -> Vec<ThemeChoice> {
    [
        ("dark", "true-colour dark — the default", ThemeVariant::Dark),
        ("light", "true-colour light terminals", ThemeVariant::Light),
        (
            "high-contrast",
            "pure black on white, maximum contrast",
            ThemeVariant::HighContrast,
        ),
        (
            "color-blind-safe",
            "Okabe–Ito hues, safe for all common colour vision",
            ThemeVariant::ColorBlindSafe,
        ),
        (
            "ansi256",
            "xterm-256 indexed palette",
            ThemeVariant::Ansi256,
        ),
        (
            "ansi16",
            "basic ANSI palette for 16-colour terminals",
            ThemeVariant::Ansi16,
        ),
        (
            "monochrome",
            "no colour at all — white, grey, black",
            ThemeVariant::Monochrome,
        ),
    ]
    .into_iter()
    .map(|(id, summary, variant)| ThemeChoice {
        id: id.to_owned(),
        summary: summary.to_owned(),
        theme: Theme::variant(variant),
        pack: false,
    })
    .collect()
}

/// The indices into `themes` whose id or summary case-insensitively contains
/// `query`, in list order — the theme picker's substring filter, the same
/// shape as [`filter_models`]. An empty query matches every theme.
#[must_use]
pub fn filter_themes(themes: &[ThemeChoice], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    themes
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            needle.is_empty()
                || choice.id.to_lowercase().contains(&needle)
                || choice.summary.to_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// The indices into [`MODE_CARDS`] whose label or summary case-insensitively
/// contains `query` — the mode picker's substring filter, in table order.
/// Mirrors [`filter_models`], adapted to a static table (no state list to
/// borrow). An empty query matches every mode.
#[must_use]
pub(crate) fn filter_modes(query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    MODE_CARDS
        .iter()
        .enumerate()
        .filter(|(_, card)| {
            needle.is_empty()
                || card.label.to_lowercase().contains(&needle)
                || card.summary.to_lowercase().contains(&needle)
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Ceiling on retained transcript entries per run (the ledger is the durable
/// record; this is a bounded view for an in-terminal scrollback).
pub(crate) const MAX_TRANSCRIPT_ENTRIES: usize = 2000;
/// Ceiling on one coalesced model-text entry's bytes.
pub(crate) const MAX_MODEL_ENTRY_BYTES: usize = 256 * 1024;

/// The status-line projection (STEP 1.12 RULE 4, [Chapter 20] projections):
/// mode, run state, model, context %, cost, worktree, pending-approval count.
///
/// [Chapter 20]: ../../../docs/docs/20-interaction-and-autonomy-model.md
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusProjection {
    pub mode: Option<AgentMode>,
    pub run_state: Option<RunState>,
    pub model: Option<ModelId>,
    pub context_percent: Option<u16>,
    pub cost_minor: Option<u64>,
    pub worktree: Option<String>,
    pub pending_approvals: usize,
}

/// The whole application state. Read by the renderer, mutated only by `reduce`.
#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    /// Validated renderer-independent documents and terminal-local view state.
    pub remote_ui: RemoteUiHostState,
    /// Daemon-owned installed-plugin lifecycle projection. It carries display
    /// metadata only, never executable or trust authority.
    pub ui_plugins: Vec<codypendent_protocol::UiPluginLifecycleStatus>,
    /// Focused installed plugin in the host-owned lifecycle surface.
    pub selected_ui_plugin: usize,
    /// The attached session's title, once known.
    pub session_title: Option<String>,
    /// The running daemon's build id (D3), captured from the handshake by the
    /// CLI harness AFTER the build-mismatch reconcile (so a just-restarted
    /// daemon shows the new id, not the stale handshaken one). Rendered by
    /// the chat header at the full-width tier; `None` before attach.
    pub daemon_build_id: Option<String>,
    /// Whether the session has been closed.
    pub session_closed: bool,
    /// All runs, in arrival order.
    pub runs: Vec<RunView>,
    /// Index into `runs` of the selected run.
    pub selected_run: usize,
    /// Pending approvals across the session.
    pub pending_approvals: Vec<PendingApproval>,
    /// Index into `pending_approvals` of the focused approval.
    pub selected_approval: usize,
    /// The Skill Studio projection (STEP 2.6): every registered item, mapped to a
    /// self-contained [`SkillCard`] by the CLI. Populated once at attach; the
    /// [`Overlay::Skills`] browser reads it.
    pub skills: Vec<SkillCard>,
    /// Index into `skills` of the focused skill.
    pub selected_skill: usize,
    /// The memory projection (STEP 2.6): the visible-scope memories, mapped to
    /// self-contained [`MemoryCard`]s by the CLI. May be empty. The
    /// [`Overlay::Memory`] browser reads it.
    pub memories: Vec<MemoryCard>,
    /// Index into `memories` of the focused memory.
    pub selected_memory: usize,
    /// The Docs Studio projection (Phase 4 client wiring): the visible-scope
    /// documents, mapped to self-contained [`DocCard`]s by the CLI. May be
    /// empty. The [`Overlay::Docs`] browser reads it.
    pub docs: Vec<DocCard>,
    /// Index into `docs` of the focused document.
    pub selected_doc: usize,
    /// Index into the focused document's `blocks` of the focused block (the editor
    /// rail cursor; the edit action targets this block).
    pub selected_block: usize,
    /// Index into the focused document's `suggestions` of the focused suggestion
    /// (the review rail cursor; accept/reject target this suggestion).
    pub selected_suggestion: usize,
    /// Which rail of the Docs overlay the keyboard drives (`Tab` cycles it).
    pub doc_focus: DocFocus,
    /// The in-flight document edit (lease lifecycle + queued mutation), if any.
    /// Drives the editor rail's lease indicator and the lease→mutate handshake.
    pub doc_edit: Option<DocEdit>,
    /// The code-graph edge projection (Phase 4 exit criterion 4): this
    /// repository's edges, mapped to self-contained [`GraphEdgeCard`]s by the
    /// CLI. May be empty. The [`Overlay::Edges`] inspector reads it.
    pub edges: Vec<GraphEdgeCard>,
    /// Total matching rows in SQLite, not just the current page.
    pub edge_total: usize,
    /// Current zero-based result page.
    pub edge_page: usize,
    /// Current graph filter query.
    pub edge_query: String,
    /// Whether an edge page request is in flight. This distinguishes the
    /// first asynchronous load from a genuinely empty repository/query.
    pub edge_loading: bool,
    /// Index into `edges` of the focused edge.
    pub selected_edge: usize,
    /// The workflow-graph projection (Phase 5 STEP 5.2): the nodes of the
    /// repository's compiled workflow manifests, mapped to self-contained
    /// [`WorkflowNodeCard`]s by the CLI, in topological order. May be empty. The
    /// [`Overlay::Workflow`] view reads it.
    pub workflow: Vec<WorkflowNodeCard>,
    /// Index into `workflow` of the focused node.
    pub selected_node: usize,
    /// The blackboard projection (Phase 5 STEP 5.3): the artifacts on the active
    /// workflow runs' boards, mapped to self-contained [`BlackboardItemCard`]s by
    /// the CLI, grouped by run. May be empty (until the executor posts artifacts).
    /// The [`Overlay::Blackboard`] view reads it.
    pub blackboard: Vec<BlackboardItemCard>,
    /// Index into `blackboard` of the focused item.
    pub selected_item: usize,
    /// The council browser projection (rubric 6 TUI wiring): every council
    /// persisted in `councils.toml`, mapped to a self-contained
    /// [`CouncilCard`] by the CLI harness. Populated at attach and reloaded
    /// after every create/run/delete. The [`Overlay::CouncilBrowser`] reads it.
    pub councils: Vec<CouncilCard>,
    /// Index into `councils` of the focused council.
    pub selected_council: usize,
    /// The repository task board (rubric 10): every live `task` card on the
    /// repository's board, mapped to self-contained [`KanbanCard`]s by the CLI.
    /// The [`Overlay::Kanban`] view reads it; a live `BlackboardPosted` on the
    /// board's channel merges into it by id, so an agent's `task.create` appears
    /// in the pane without a refresh.
    pub kanban: Vec<KanbanCard>,
    /// Index into the board's DISPLAY order (column-major, the order the pane
    /// walks columns and cards) of the focused card.
    pub selected_card: usize,
    /// The model-picker projection (MP1): every model configured in
    /// `models.toml`, enriched with its measured profile from
    /// `model_profiles` when one exists, mapped to a self-contained
    /// [`ModelCard`] by the CLI harness. Populated once at attach; the
    /// [`Overlay::ModelPicker`] browser reads it.
    pub models: Vec<ModelCard>,
    /// Index into `models` of the focused card — kept resolved to the
    /// picker's live filtered selection by the reducer, so
    /// [`AppState::focused_model`] reads uniformly with every other
    /// browser's `focused_*` accessor.
    pub selected_model: usize,
    /// The model staged from the picker (`Enter` on a row). Advisory only
    /// this task (MP1) — nothing yet reads it to change routing behavior; a
    /// later task (MP2) wires it to pin the next run's model.
    pub pending_model: Option<ModelId>,
    /// The provider-catalog projection for the `/provider` picker (Task 8):
    /// the built-in ~40-provider catalog, layered with any user
    /// `providers.toml`, mapped to a self-contained [`ProviderCard`] by the
    /// CLI harness. Populated once at attach; the [`Overlay::ProviderPicker`]
    /// browser reads it.
    pub providers: Vec<ProviderCard>,
    /// Index into `providers` of the focused card — kept resolved to the
    /// picker's live filtered selection by the reducer, mirroring
    /// `selected_model`.
    pub selected_provider: usize,
    /// The `/keys` status projection (D1): one `(model_id, status)` per
    /// configured model, folded from [`Action::ApiKeyStatusesLoaded`] — loaded
    /// by the CLI harness from `auth.json` + `models.toml` after the other
    /// projections, and re-fired after every key write and daemon restart.
    /// The [`Overlay::ApiKeys`] overlay reads it; it carries no key material.
    pub key_status: Vec<(String, KeyStatus)>,
    /// The Tavily `web.search` row's key status (D1), folded from the same
    /// [`Action::ApiKeyStatusesLoaded`] as [`AppState::key_status`].
    pub tavily_key_status: KeyStatus,
    /// Persistent, de-duplicated setup/runtime diagnostics. Boot loader failures
    /// land here instead of competing for the single transient notice slot.
    pub issues: Vec<String>,
    /// Index into [`AppState::issues`] for the diagnostics overlay.
    pub selected_issue: usize,
    /// The focused pane. Vestigial in the conversation-centred shell (the
    /// transcript is the single main surface); retained for catch-up/mouse code.
    pub focus: Pane,
    /// The persistent composer buffer (the always-present bottom input). Typed
    /// text lands here; Enter sends it (starting a run, or steering the active
    /// one). Empty by default.
    pub composer: String,
    /// The insertion point in [`AppState::composer`], as a **byte** offset that
    /// is always on a `char` boundary and never past `composer.len()`. Every
    /// composer mutation goes through the splice helpers in [`crate::reduce`],
    /// which maintain both invariants; `Left`/`Right` step whole graphemes, so
    /// a multi-byte character or a combining sequence is never split.
    pub composer_cursor: usize,
    /// Prior composer submissions (shell-style history), oldest first. `Up`
    /// (`HistoryPrev`) walks backward from the newest entry; `Down`
    /// (`HistoryNext`) walks forward. Populated by `InputSubmit` on a
    /// non-empty draft; a submission identical to the last entry is skipped
    /// (no consecutive duplicates). Client-only — never sent over the wire.
    pub composer_history: Vec<String>,
    /// While recalling history, the index into `composer_history` currently
    /// loaded into `composer`. `None` means `composer` holds the user's own
    /// in-progress text, not a recalled entry — every edit action
    /// (`InputChar` / `InputBackspace` / `InputNewline` / `InputPaste`) resets
    /// this to `None` whenever it touches a recalled entry (shell-style:
    /// editing a recalled command detaches it from history).
    pub history_cursor: Option<usize>,
    /// The user's in-progress draft, stashed by `HistoryPrev` the moment it
    /// first recalls an entry (`history_cursor` goes `None` → `Some`) — so
    /// `HistoryNext` walking back past the newest entry can restore it
    /// verbatim. The in-progress text is never lost.
    pub composer_stash: Option<String>,
    /// Whether the transcript fold selection is live: the base view is
    /// *browsing* its folds (`Alt-↑`/`Alt-↓`) rather than purely composing.
    /// While set, the selected run's `transcript_selected` entry renders
    /// highlighted, the viewport keeps it in sight, and `Alt-Enter` toggles it
    /// instead of inserting a line break. Cleared by typing, scrolling, or
    /// `Esc` — every gesture that means "I am driving the composer/view
    /// again". Client-only view state; never on the wire.
    pub transcript_browse: bool,
    /// Every theme the `/theme` picker offers: the seven built-in variants,
    /// plus any data-only packs the CLI loaded from `<data-dir>/themes/` at
    /// boot (the TUI crate does no I/O, so a pack arrives already parsed).
    pub themes: Vec<ThemeChoice>,
    /// Index into [`AppState::themes`] of the theme in force, once the operator
    /// has picked one. `None` means "whatever the harness resolved at boot"
    /// (the `--theme` flag, `CODYPENDENT_THEME`, a persisted preference, or
    /// terminal detection) — see [`AppState::effective_theme`].
    pub theme_selected: Option<usize>,
    /// Which base layout is rendered (chat single-column vs. workspace panes).
    /// Toggled with `F2`; defaults to [`LayoutMode::Chat`].
    pub layout: LayoutMode,
    /// The maximum transcript scroll offset (rows below the top that still fill
    /// the viewport), cached by the renderer each frame. The renderer knows the
    /// wrapped height and viewport; the reducer reads this cache so PgUp can leave
    /// follow mode at the true bottom and PgDn can re-enter it. A one-frame-stale
    /// layout metric — never domain state — which is why it is a [`Cell`] the
    /// draw-only renderer may update through a shared reference.
    pub transcript_max_scroll: Cell<u16>,
    /// A render→input geometry cache (mirrors `transcript_max_scroll`): every
    /// interactive widget registers its `Rect` + the `Action` a click fires here
    /// during render; the input layer resolves a left click to the topmost hit.
    /// A one-frame-fresh layout metric, cleared at the start of every render —
    /// never domain state. `RefCell` (not `Cell`) because the payload is a
    /// non-`Copy` `Vec`. Default/clone/eq are harmless: it defaults empty and is
    /// only populated during render, so reducer-only tests keep comparing equal.
    pub hit_map: RefCell<Vec<(Rect, Action)>>,
    /// The top-most overlay / modal.
    pub overlay: Overlay,
    /// The mode used for the next new run (the new-run prompt inherits it).
    pub default_mode: AgentMode,
    /// Set when the user detaches (`q`). The CLI observes this to leave the TUI
    /// loop; the run is never affected.
    pub should_detach: bool,
    /// A monotonic tick counter for spinner animation.
    pub tick: u64,
    /// A transient status-line notice and the tick at which it expires.
    pub notice: Option<(String, u64)>,
    /// Voice input/output state (voice v1, rubric 8). Purely presentational
    /// here: the capture and speech work itself lives in the CLI's voice host
    /// (`codypendent_cli::voice`), which owns the recorder/player subprocesses.
    /// This state is what the status line renders and what the host reads to
    /// decide whether to speak a finalized reply.
    pub voice: VoiceState,
    /// Semantic commands the CLI must send to the daemon. Drained by the CLI
    /// after every reduce; never touched by the renderer.
    pub outbox: Vec<Intent>,
}

/// Voice input/output state (voice v1, rubric 8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceState {
    /// Whether push-to-talk capture is running right now. Rendered as a
    /// prominent status-line indicator: a hot microphone must never be
    /// invisible.
    pub recording: bool,
    /// Whether finalized assistant turns are spoken aloud. Toggled from the
    /// palette; read by the CLI's voice host, which does the synthesis
    /// off-thread so a slow provider never stalls the UI.
    pub speak_replies: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// A fresh, empty state (nothing attached yet).
    #[must_use]
    pub fn new() -> Self {
        Self {
            remote_ui: RemoteUiHostState::default(),
            ui_plugins: Vec::new(),
            selected_ui_plugin: 0,
            session_title: None,
            daemon_build_id: None,
            session_closed: false,
            runs: Vec::new(),
            selected_run: 0,
            pending_approvals: Vec::new(),
            selected_approval: 0,
            skills: Vec::new(),
            selected_skill: 0,
            memories: Vec::new(),
            selected_memory: 0,
            docs: Vec::new(),
            selected_doc: 0,
            selected_block: 0,
            selected_suggestion: 0,
            doc_focus: DocFocus::default(),
            doc_edit: None,
            edges: Vec::new(),
            edge_total: 0,
            edge_page: 0,
            edge_query: String::new(),
            edge_loading: false,
            selected_edge: 0,
            workflow: Vec::new(),
            selected_node: 0,
            blackboard: Vec::new(),
            selected_item: 0,
            councils: Vec::new(),
            selected_council: 0,
            kanban: Vec::new(),
            selected_card: 0,
            models: Vec::new(),
            selected_model: 0,
            pending_model: None,
            providers: Vec::new(),
            selected_provider: 0,
            key_status: Vec::new(),
            tavily_key_status: KeyStatus::Missing,
            issues: Vec::new(),
            selected_issue: 0,
            focus: Pane::Sessions,
            composer: String::new(),
            composer_cursor: 0,
            composer_history: Vec::new(),
            history_cursor: None,
            composer_stash: None,
            transcript_browse: false,
            themes: builtin_theme_choices(),
            theme_selected: None,
            layout: LayoutMode::Chat,
            transcript_max_scroll: Cell::new(0),
            hit_map: RefCell::new(Vec::new()),
            overlay: Overlay::None,
            default_mode: AgentMode::Build,
            should_detach: false,
            tick: 0,
            notice: None,
            voice: VoiceState::default(),
            outbox: Vec::new(),
        }
    }

    /// The input mode the next key should be interpreted in.
    #[must_use]
    pub fn input_mode(&self) -> InputMode {
        if let Overlay::CouncilBuilder(builder) = &self.overlay {
            return match builder.step {
                CouncilBuilderStep::Name
                | CouncilBuilderStep::Description
                | CouncilBuilderStep::MemberRole => InputMode::Editing,
                CouncilBuilderStep::MemberModel
                | CouncilBuilderStep::Chair
                | CouncilBuilderStep::Rounds
                | CouncilBuilderStep::Review => InputMode::Palette,
            };
        }
        // The Unsloth repo/quant browsers share one overlay each across their
        // loading and loaded sub-states (mirrors the CouncilBuilder
        // resolution above): non-interactive while fetching, filterable once
        // loaded.
        if let Overlay::UnslothRepos { loading, .. } = &self.overlay {
            return if *loading {
                InputMode::Normal
            } else {
                InputMode::Palette
            };
        }
        if let Overlay::UnslothQuants { loading, .. } = &self.overlay {
            return if *loading {
                InputMode::Normal
            } else {
                InputMode::Palette
            };
        }
        match self.overlay {
            Overlay::NewRun(_)
            | Overlay::Steering(_)
            | Overlay::WorkflowInputs { .. }
            | Overlay::EdgeSearch(_)
            | Overlay::DocEdit { .. }
            | Overlay::DocNew { .. }
            | Overlay::DocInsert { .. }
            | Overlay::DocPublishPath { .. }
            | Overlay::AddModelId { .. }
            | Overlay::AddModelKey { .. }
            | Overlay::AddModelProviderKey { .. }
            | Overlay::ApiKeySet { .. }
            | Overlay::CouncilRunObjective { .. } => InputMode::Editing,
            Overlay::ConfirmCancel
            | Overlay::ConfirmWorkflowCancel { .. }
            | Overlay::ApiKeyRemoveConfirm { .. }
            | Overlay::ConfirmUiPluginApprove { .. }
            | Overlay::ConfirmUiPluginReject { .. }
            | Overlay::ConfirmUiPluginRevoke { .. }
            | Overlay::ConfirmCouncilDelete { .. }
            | Overlay::UnslothConfirmPull { .. }
            | Overlay::DocDeleteConfirm { .. } => InputMode::Confirm,
            // The palette, the model picker, the provider picker, the mode
            // picker, the `/keys` overlay, and the add-model pick-list all
            // filter on printable keys while staying arrow-navigable, so they
            // share this input mode (see [`crate::input::map_palette_key`]).
            Overlay::Palette { .. }
            | Overlay::ModelPicker { .. }
            | Overlay::ProviderPicker { .. }
            | Overlay::ModePicker { .. }
            | Overlay::ThemePicker { .. }
            | Overlay::ApiKeys { .. }
            | Overlay::AddModelPick { .. } => InputMode::Palette,
            // The Skills / Memory / Docs / Edges / Workflow / Help browsers are
            // navigable with the arrow/command key table, so they stay in `Normal`
            // mode. The add-model querying box is likewise non-interactive except
            // `Esc` (dismiss), so it shares this mode too.
            Overlay::Help
            | Overlay::Issues
            | Overlay::Skills
            | Overlay::Memory { .. }
            | Overlay::Docs
            | Overlay::Edges
            | Overlay::Workflow
            | Overlay::Blackboard
            | Overlay::Kanban
            | Overlay::UiPlugins
            | Overlay::CouncilBrowser
            | Overlay::AddModelQuerying { .. }
            | Overlay::UnslothPulling { .. } => InputMode::Normal,
            Overlay::CouncilBuilder(_) => {
                unreachable!("council builder input mode is resolved above")
            }
            Overlay::UnslothRepos { .. } | Overlay::UnslothQuants { .. } => {
                unreachable!("unsloth repo/quant browser input mode is resolved above")
            }
            // The base conversation view: an unresolved approval owns the screen
            // (decision keys only); otherwise the composer captures typed text.
            Overlay::None => {
                if self.show_approval_modal() {
                    InputMode::Approval
                } else if self.remote_ui.active && !self.remote_ui.mounted_documents().is_empty() {
                    InputMode::RemoteUi
                } else {
                    InputMode::Composer
                }
            }
        }
    }

    /// The currently selected run, if any.
    #[must_use]
    pub fn selected_run(&self) -> Option<&RunView> {
        self.runs.get(self.selected_run)
    }

    /// Whether the selected run is still live — i.e. a composer message should
    /// *steer* it rather than start a fresh run. `false` when no run is selected
    /// or the selected run has reached a terminal state.
    #[must_use]
    pub fn selected_run_is_active(&self) -> bool {
        self.selected_run().is_some_and(|run| {
            !matches!(
                run.state,
                RunState::Completed | RunState::Failed | RunState::Cancelled
            )
        })
    }

    /// Whether the approval modal should be shown: there is at least one pending
    /// approval and no other overlay is competing for the foreground.
    #[must_use]
    pub fn show_approval_modal(&self) -> bool {
        !self.pending_approvals.is_empty() && matches!(self.overlay, Overlay::None)
    }

    /// The focused pending approval, if any.
    #[must_use]
    pub fn focused_approval(&self) -> Option<&PendingApproval> {
        self.pending_approvals.get(self.selected_approval)
    }

    /// The focused Skill Studio card, if any.
    #[must_use]
    pub fn focused_skill(&self) -> Option<&SkillCard> {
        self.skills.get(self.selected_skill)
    }

    /// The focused memory card, if any.
    #[must_use]
    pub fn focused_memory(&self) -> Option<&MemoryCard> {
        self.memories.get(self.selected_memory)
    }

    /// The focused Docs Studio card, if any.
    #[must_use]
    pub fn focused_doc(&self) -> Option<&DocCard> {
        self.docs.get(self.selected_doc)
    }

    /// The focused block of the focused document, if any (the editor rail cursor).
    #[must_use]
    pub fn focused_block(&self) -> Option<&DocBlockView> {
        self.focused_doc()?.blocks.get(self.selected_block)
    }

    /// The focused suggestion of the focused document, if any (the review rail
    /// cursor).
    #[must_use]
    pub fn focused_suggestion(&self) -> Option<&DocSuggestionView> {
        self.focused_doc()?
            .suggestions
            .get(self.selected_suggestion)
    }

    /// The focused code-graph edge card, if any.
    #[must_use]
    pub fn focused_edge(&self) -> Option<&GraphEdgeCard> {
        self.edges.get(self.selected_edge)
    }

    /// The focused workflow node card, if any.
    #[must_use]
    pub fn focused_node(&self) -> Option<&WorkflowNodeCard> {
        self.workflow.get(self.selected_node)
    }

    /// The focused blackboard item card, if any.
    #[must_use]
    pub fn focused_item(&self) -> Option<&BlackboardItemCard> {
        self.blackboard.get(self.selected_item)
    }

    /// The board's cards in DISPLAY order: column by column in
    /// [`KANBAN_COLUMNS`] order, each column sorted by `ordinal` then title.
    ///
    /// One ordering serves the renderer, the keyboard selection, and the hit
    /// regions, so "the third card" means the same thing to all three. A card
    /// whose status matches no known column is shown in the FIRST column rather
    /// than dropped — an unrecognized column must never hide work.
    #[must_use]
    pub fn kanban_columns(&self) -> Vec<(&'static str, Vec<&KanbanCard>)> {
        KANBAN_COLUMNS
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let mut cards: Vec<&KanbanCard> = self
                    .kanban
                    .iter()
                    .filter(|card| {
                        card.status.eq_ignore_ascii_case(column)
                            || (index == 0
                                && !KANBAN_COLUMNS
                                    .iter()
                                    .any(|known| card.status.eq_ignore_ascii_case(known)))
                    })
                    .collect();
                cards.sort_by(|a, b| {
                    a.ordinal
                        .cmp(&b.ordinal)
                        .then_with(|| a.title.cmp(&b.title))
                });
                (*column, cards)
            })
            .collect()
    }

    /// The board's cards flattened in display order — the sequence
    /// [`selected_card`](Self::selected_card) indexes.
    #[must_use]
    pub fn kanban_in_display_order(&self) -> Vec<&KanbanCard> {
        self.kanban_columns()
            .into_iter()
            .flat_map(|(_, cards)| cards)
            .collect()
    }

    /// The focused board card, if any.
    #[must_use]
    pub fn focused_card(&self) -> Option<&KanbanCard> {
        self.kanban_in_display_order()
            .get(self.selected_card)
            .copied()
    }

    /// The focused host-managed Remote UI plugin, if one is installed.
    #[must_use]
    pub fn focused_ui_plugin(&self) -> Option<&codypendent_protocol::UiPluginLifecycleStatus> {
        self.ui_plugins.get(self.selected_ui_plugin)
    }

    /// The focused council browser card, if any (rubric 6 TUI wiring).
    #[must_use]
    pub fn focused_council(&self) -> Option<&CouncilCard> {
        self.councils.get(self.selected_council)
    }

    /// The focused model-picker card, if any.
    #[must_use]
    pub fn focused_model(&self) -> Option<&ModelCard> {
        self.models.get(self.selected_model)
    }

    /// The focused provider-picker card, if any.
    #[must_use]
    pub fn focused_provider(&self) -> Option<&ProviderCard> {
        self.providers.get(self.selected_provider)
    }

    /// Project the status-line fields from the selected run + pending approvals.
    #[must_use]
    pub fn status(&self) -> StatusProjection {
        let run = self.selected_run();
        StatusProjection {
            mode: run.map(|r| r.mode),
            run_state: run.map(|r| r.state),
            model: run.and_then(|r| r.model.clone()),
            context_percent: run.and_then(|r| r.context_percent),
            cost_minor: run.and_then(|r| r.cost_minor),
            worktree: run.and_then(|r| r.worktree.clone()),
            pending_approvals: self.pending_approvals.len(),
        }
    }

    /// The theme this frame draws in: the row the `/theme` picker is focused on
    /// (so moving the cursor previews the whole shell in that theme), else the
    /// operator's kept choice, else `boot` — whatever the harness resolved from
    /// `--theme`/`CODYPENDENT_THEME`/the persisted preference/terminal
    /// detection. Pure, so the renderer stays a projection of state.
    #[must_use]
    pub fn effective_theme(&self, boot: &Theme) -> Theme {
        if let Overlay::ThemePicker { query, selected } = &self.overlay {
            let filtered = filter_themes(&self.themes, query);
            if let Some(choice) = filtered
                .get(*selected)
                .and_then(|&idx| self.themes.get(idx))
            {
                return choice.theme;
            }
        }
        self.theme_selected
            .and_then(|idx| self.themes.get(idx))
            .map_or(*boot, |choice| choice.theme)
    }

    /// Whether any surface on screen right now has a moving part — a run that
    /// is thinking or executing a tool, a code-graph page in flight, or a
    /// provider's model list being fetched.
    ///
    /// The interactive client redraws on every tick while this holds, and
    /// falls back to its sparse keep-alive redraw otherwise. Without it, the
    /// spinners were repainted once every 25 ticks (~5s) — a "spinner" that
    /// changes frame once every five seconds reads as a frozen UI, which is
    /// the exact opposite of what it is there to say.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.edge_loading
            || matches!(self.overlay, Overlay::AddModelQuerying { .. })
            || self
                .runs
                .iter()
                .any(|run| !matches!(run.activity, RunActivity::Idle))
    }

    /// Drain the outbox of intents accumulated since the last call. The CLI's
    /// connection task calls this after each reduce to dispatch commands.
    pub fn drain_outbox(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.outbox)
    }

    /// Clear only session-scoped state after the harness creates a fresh
    /// session. Workspace projections, model/provider setup, diagnostics,
    /// preferences, and composer history remain available in place.
    pub fn begin_new_session(&mut self) {
        self.remote_ui = RemoteUiHostState::default();
        self.session_title = None;
        self.session_closed = false;
        self.runs.clear();
        self.selected_run = 0;
        self.pending_approvals.clear();
        self.selected_approval = 0;
        self.composer.clear();
        self.composer_stash = None;
        self.history_cursor = None;
        self.doc_edit = None;
        self.overlay = Overlay::None;
        self.edge_loading = false;
        self.transcript_max_scroll.set(0);
        self.should_detach = false;
    }

    /// Register an interactive rect → the Action a left click on it fires. Called
    /// by the renderer (interior mutability; the reducer never touches it).
    pub fn register_hit(&self, rect: Rect, action: Action) {
        self.hit_map.borrow_mut().push((rect, action));
    }

    // --- internal helpers used by the reducer ---

    pub(crate) fn run_mut(&mut self, run_id: RunId) -> Option<&mut RunView> {
        self.runs.iter_mut().find(|r| r.run_id == run_id)
    }

    pub(crate) fn ensure_run(
        &mut self,
        run_id: RunId,
        objective: String,
        mode: AgentMode,
    ) -> &mut RunView {
        if let Some(idx) = self.runs.iter().position(|r| r.run_id == run_id) {
            // An already-known run re-announcing itself (catch-up overlap,
            // another client's activity) must not steal the selection.
            return &mut self.runs[idx];
        }
        self.runs.push(RunView::new(run_id, objective, mode));
        let last = self.runs.len() - 1;
        // Focus the new run unless the user is mid-draft. Our own submit
        // clears the composer before its RunStarted folds back, so this still
        // follows the action for runs this client starts — while another
        // client's RunStarted in a shared session cannot retarget a message
        // being composed (Enter submits against `selected_run` at that
        // moment).
        if self.composer.is_empty() {
            self.selected_run = last;
        }
        &mut self.runs[last]
    }

    pub(crate) fn selected_run_mut(&mut self) -> Option<&mut RunView> {
        self.runs.get_mut(self.selected_run)
    }

    /// Append model text, coalescing into a trailing `Model` entry. The
    /// coalesced entry keeps the timestamp of its FIRST delta — that is when
    /// the turn began, which is what the turn header shows.
    pub(crate) fn append_model_text(run: &mut RunView, text: &str, at: DateTime<Utc>) {
        if let Some(TranscriptEntry::Model {
            text: existing,
            rendered,
        }) = run.transcript.last_mut()
        {
            // Bound one coalesced model entry: an hours-long stream must not grow
            // a single String without limit (the full text is in the ledger; the
            // transcript is a view). Past the cap, start a fresh entry so the
            // entry-count cap in `push_entry` takes over.
            if existing.len() + text.len() <= MAX_MODEL_ENTRY_BYTES {
                existing.push_str(text);
                // The only entry that receives appends is the never-finalized
                // streaming tail; keep its cache empty so it renders plain.
                *rendered = None;
                return;
            }
        }
        Self::push_entry(
            run,
            TranscriptEntry::Model {
                text: text.to_owned(),
                rendered: None,
            },
            at,
        );
    }

    /// Append a transcript entry and the wall-clock time of the event that
    /// produced it, holding the transcript to [`MAX_TRANSCRIPT_ENTRIES`] by
    /// dropping the oldest — the ledger, not this view, is the durable record.
    /// Selection/scroll indices shift with the drop so the focused entry stays
    /// the same one.
    ///
    /// This is the ONLY writer of [`RunView::entry_times`]; keeping the push
    /// and the timestamp in one call is what holds the two vectors in lockstep
    /// (asserted by `transcript_and_entry_times_stay_in_lockstep`).
    pub(crate) fn push_entry(run: &mut RunView, entry: TranscriptEntry, at: DateTime<Utc>) {
        run.transcript.push(entry);
        run.entry_times.push(at);
        while run.transcript.len() > MAX_TRANSCRIPT_ENTRIES {
            run.transcript.remove(0);
            run.entry_times.remove(0);
            run.transcript_selected = run.transcript_selected.saturating_sub(1);
            run.scroll = run.scroll.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`filter_model_names`] mirrors [`filter_models`]/[`filter_providers`]'s
    /// substring-match shape, adapted to the add-model pick-list's
    /// [`AddModelRow`] cards.
    #[test]
    fn filter_model_names_matches_case_insensitive_substrings() {
        let models = vec![
            AddModelRow::live("llama-3.1-8b"),
            AddModelRow::live("gpt-oss-20b"),
        ];
        assert_eq!(
            filter_model_names(&models, ""),
            vec![0, 1],
            "an empty query matches every name"
        );
        assert_eq!(
            filter_model_names(&models, "LLAMA"),
            vec![0],
            "the match is case-insensitive"
        );
        assert_eq!(
            filter_model_names(&models, "oss"),
            vec![1],
            "a mid-string substring matches"
        );
        assert!(
            filter_model_names(&models, "zzz-no-such-model").is_empty(),
            "no match returns an empty list"
        );
    }

    /// A catalog row's display NAME is searchable too: an operator who knows a
    /// model as "Llama 3.3 70B" should not have to guess the provider's id
    /// spelling to find it.
    #[test]
    fn filter_model_names_also_matches_the_display_name() {
        let models = vec![AddModelRow {
            id: "meta-llama/Llama-3.3-70B-Instruct".to_owned(),
            name: Some("Llama 3.3 70B Instruct".to_owned()),
            context_tokens: Some(128_000),
            cost_per_1m_input_usd: Some(0.13),
            cost_per_1m_output_usd: Some(0.4),
            live: false,
        }];
        assert_eq!(filter_model_names(&models, "3.3 70b"), vec![0]);
        assert_eq!(filter_model_names(&models, "META-LLAMA"), vec![0]);
    }

    /// Secret hygiene (model discovery): every new overlay that carries a
    /// [`SecretKey`] — directly or via `AddModelId`'s new `api_key` field — must
    /// redact it through `Overlay`'s derived `Debug`, exactly like the
    /// pre-existing `AddModelKey` overlay. `Overlay` derives `Debug` structurally,
    /// so this holds as long as every such field's own type is `SecretKey` (never
    /// a raw `String`); asserted directly here rather than only inferred from the
    /// render tests (which check the SCREEN, not `{:?}`).
    #[test]
    fn new_overlays_debug_redacts_the_key() {
        let cases = [
            format!(
                "{:?}",
                Overlay::AddModelProviderKey {
                    provider_id: "groq".to_owned(),
                    buffer: SecretKey("sk-secret".to_owned()),
                }
            ),
            format!(
                "{:?}",
                Overlay::AddModelQuerying {
                    provider_id: "groq".to_owned(),
                    api_key: Some(SecretKey("sk-secret".to_owned())),
                }
            ),
            format!(
                "{:?}",
                Overlay::AddModelPick {
                    provider_id: "groq".to_owned(),
                    api_key: Some(SecretKey("sk-secret".to_owned())),
                    models: vec![AddModelRow::live("llama-3.1-8b")],
                    query: String::new(),
                    selected: 0,
                    origin: ModelListOrigin::Live,
                    refreshing: false,
                }
            ),
            format!(
                "{:?}",
                Overlay::AddModelId {
                    provider_id: "groq".to_owned(),
                    requires_key: true,
                    api_key: Some(SecretKey("sk-secret".to_owned())),
                    buffer: String::new(),
                }
            ),
            // D1: the `/keys` set prompt carries the key in the same newtype.
            format!(
                "{:?}",
                Overlay::ApiKeySet {
                    target: KeyTarget::Model("groq/llama".to_owned()),
                    buffer: SecretKey("sk-secret".to_owned()),
                }
            ),
        ];
        for dbg in cases {
            assert!(
                !dbg.contains("sk-secret"),
                "the key must never leak through Debug: {dbg}"
            );
            assert!(
                dbg.contains("<redacted>"),
                "expected a redaction marker: {dbg}"
            );
        }
    }
}

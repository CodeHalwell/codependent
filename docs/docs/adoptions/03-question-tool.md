# Adoption 03 — Question tool

**Effort:** S · **Depends on:** nothing · **Reference:** reference-repos/opencode/packages/opencode/src/tool/question.ts, src/tool/question.txt, src/question/index.ts, packages/schema/src/v1/question.ts, packages/tui/src/routes/session/question.tsx, src/permission/index.ts (`CorrectedError`)
**Ported from:** opencode · **Status:** ⬜ not started

## 1. Summary

Give the model a `user.ask` tool: one or more structured questions (multiple-choice options, optional multi-select, optional free-text "type your own answer"), asked mid-run, **parked durably** the way an approval is parked — the run transitions to `WaitingForUserInput`, the question survives a daemon restart, and the answer (or a rejection) wakes the run. The TUI renders the question as a first-class card with the same exclusive-focus discipline as an approval card. A rejection may carry an optional free-text message which is fed back to the model as a typed correction ("no, and here's why") — the port of opencode's `CorrectedError`.

## 2. Reference implementation

All paths relative to `reference-repos/opencode/`.

**Schema** (`packages/schema/src/v1/question.ts`): a question is
`{ question: string, header: string (≤30 chars), options: [{label, description}], multiple?: bool, custom?: bool }`.
The **tool-facing** `Prompt` struct deliberately omits `custom` (so the model cannot turn free-text off; it defaults on), while the internal `Info` carries it (internal callers like plan-exit set `custom: false`). Answers are `Answer = string[]` (selected labels, plus the custom text verbatim if typed), one array per question, in question order. `Request = { id, sessionID, questions, tool?: {messageID, callID} }` — the `tool` link is what lets the TUI render the card **inline in the transcript** at the exact tool-call part.

**Service** (`packages/opencode/src/question/index.ts`): `ask()` allocates an id, stores `{info, deferred}` in a pending map, publishes `question.asked` on the event bus, then blocks on the deferred (`Effect.ensuring` deletes the entry either way). `reply({requestID, answers})` publishes `question.replied` and succeeds the deferred; `reject(requestID)` publishes `question.rejected` and fails it with `RejectedError` ("The user dismissed this question"). Instance teardown fails all pending deferreds. `list()` exposes pending questions so a re-attaching client can re-render them.

**Tool** (`packages/opencode/src/tool/question.ts`): parameters = `{questions: Prompt[]}`. On answers, the tool output is
`User has answered your questions: "<q1>"="<a1, a2>", "<q2>"="Unanswered". You can now continue with the user's answers in mind.`
— an unanswered slot renders as the literal `Unanswered`. Title: `Asked N question(s)`.

**Prompt text** (`packages/opencode/src/tool/question.txt`) — this encodes the product taste and is ported nearly verbatim:
- when `custom` is enabled (default), a "Type your own answer" option is added automatically, so the model must **not** include "Other"/catch-all options;
- answers come back as arrays of labels; `multiple: true` allows multi-select;
- a recommended option goes **first** with `" (Recommended)"` appended to its label.

**TUI** (`packages/tui/src/routes/session/question.tsx`): tabbed UI — one tab per question plus a confirm tab, except a **single single-select question auto-submits on pick** (no confirm step). The custom row is always present when `custom !== false`; selecting it opens an inline textarea; digits fast-select options. Reject is a first-class key.

**Reject-with-feedback** (`packages/opencode/src/permission/index.ts` lines 119–127, `packages/core/src/v1/permission.ts`): a permission reply of `reject` with a `message` fails the deferred with `CorrectedError { feedback }` whose model-facing message is
`The user rejected permission to use this specific tool call with the following feedback: <feedback>` —
the rejection becomes steering, not a dead end. This spec folds that mechanism into the question tool's reject path (and only there; approval-reject feedback is out of scope here).

## 3. Current state in codypendent (verified)

- **Approval parking is the model to copy.** `crates/daemon/src/approvals.rs` — `ApprovalBroker` persists a `pending` row + `ApprovalRequested` ledger event inside one `BEGIN IMMEDIATE` transaction, registers an in-memory waiter (`tokio::sync::watch` keyed by `ApprovalId`, `send_replace` so a decision delivered before subscription is retained), publishes post-commit through an optional `SubscriptionHub`, and `await_decision` blocks on the watch. `reload_pending` re-registers waiters on restart; `expire_orphaned` kills pending rows whose run is terminal; `forget_waiter` prevents a leak when a parked run is cancelled. `resolve_in_tx` is driven inside the command processor's transaction (`crates/daemon/src/commands.rs::apply_resolve_approval`) so the resolution is atomic with the command row and revision guard, with `resolved_by` taken from `ctx.principal.user_id()`.
- **The run-state machinery already exists.** `RunState::WaitingForUserInput` is a declared state (`crates/protocol/src/run.rs`) with legal transitions already wired in `crates/daemon/src/ledger.rs` (`Running → WaitingForUserInput → Running`) and recovery handling in `crates/daemon/src/recovery.rs`. **Nothing currently transitions into it** — this spec is its first producer.
- **Tool middleware**: `crates/runtime/src/agent.rs::run_tool` maps every tool call to a `ProposedAction` via `prepare()`, evaluates `self.policy.evaluate(...)`, and for `RequireApproval` parks via `self.journal.request(ApprovalRequest{...})` + `self.approvals.await_decision(approval_id)` raced against the cancel token. Always-allowed internal actions (`ProposedAction::RecordMemory`, `SearchRegistry`, …) have explicit `Allow` arms in `crates/daemon/src/policy/mod.rs::evaluate` and are documented as "never serialized into a `ToolProposed`".
- **Tool advertisement**: `crates/runtime/src/agent.rs::static_tool_definitions()` declares tools as `agent_framework_core::tools::ToolDefinition` values; conditionally-wired tools (e.g. `artifact.read` behind `self.artifacts`, `web.search` behind `self.search`) are gated in `advertised_tool_definitions`/`offered_tool_names`. Refusal-shaped observations are recognized by `observation_is_refusal` (agent.rs ~line 5199: `output.starts_with("policy denied") || output == "approval rejected"`) which feeds the repeated-call loop-breaker.
- **TUI**: `crates/tui/src/state.rs` — `PendingApproval` list + `InputMode::Approval` ("a pending approval owns the screen"); `crates/tui/src/input.rs::map_approval_key`; `crates/tui/src/reduce.rs` folds `ApprovalRequested`/`ApprovalResolved` and emits `Intent::ResolveApproval` from `resolve_focused`; `crates/cli/src/tui.rs` (~line 4051) maps `Intent::ResolveApproval` to `CommandBody::ResolveApproval`. This exact pipeline is duplicated for questions.
- **Protocol discipline**: every enum in `crates/protocol/src/run.rs`/`events.rs`/`command.rs` is internally tagged with a `#[serde(other)] Unknown` fallback and `#[non_exhaustive]`; ids come from the `uuid_id!` macro in `crates/protocol/src/ids.rs`.
- **Must not break**: the approval broker's waiter semantics and `commands.rs` idempotency/`resume_received` flow (which currently special-cases only `ResolveApproval` as "the one command with an external effect in Phase 1" — the new `ResolveQuestion` becomes the second); migrations are append-only (latest is `migrations/0033_workflow_run_owner.sql` at time of writing).

## 4. Design

A `QuestionBroker` in the daemon, structurally a sibling of `ApprovalBroker`; a `user.ask` tool in the runtime that maps to a new always-allowed `ProposedAction::AskUser`, parks the run in `WaitingForUserInput`, and awaits the broker through a pool-erased `QuestionChannel` seam (same pattern as `RunJournal`'s approval closure); `QuestionAsked`/`QuestionResolved` ledger events; a `ResolveQuestion` command; a TUI question card with `InputMode::Question`.

Deviations from the reference, and why:

1. **Durable, not in-memory.** opencode's pending questions are process state (lost on restart). Codypendent parks them in a `questions` table exactly as approvals are parked, because "an approval is a workflow state, not a UI modal" (approvals.rs module doc) applies verbatim to questions. `reload_pending` + `expire_orphaned` mirror the approval broker.
2. **Reject-with-feedback lands on questions, not approvals.** opencode's `CorrectedError` lives on the permission service. Retrofitting `ResolveApproval` would change an existing wire command; the question path is new surface, so the typed correction ships here. (Extending approval rejection with feedback is a natural follow-up, noted in §10.)
3. **`custom` is a wire field but not a tool-schema field**, mirroring opencode's `Prompt` (no `custom`) vs `Info` (has it): the model can never disable free-text; a future internal caller (e.g. a plan-exit flow) can.
4. **The run visibly waits.** opencode blocks the tool call silently; codypendent transitions the run to `WaitingForUserInput` (the state exists and has never had a producer), so every client and the recovery path see an honest state.
5. **Unattended runs never get the tool.** opencode registers the question tool "only for interactive clients". Here the daemon assembly wires the `QuestionChannel` only for interactive session runs — workflow agent nodes and webhook-triggered runs get `None`, and an unwired channel means the tool is never advertised (the `artifact.read` gating pattern).
6. **Tabbed multi-question UI is simplified** to sequential per-question navigation in one card (ratatui, not a component tree); single single-select still auto-submits on Enter, multi-question/multi-select ends on a confirm row — the interaction contract is kept, the widget tree is not.

## 5. Changes, file by file

### 5.1 `crates/protocol/src/ids.rs`

```rust
uuid_id!(QuestionId);
```

### 5.2 `crates/protocol/src/run.rs`

Append to `ProposedAction` (before `#[serde(other)] Unknown`):

```rust
    /// Ask the operator one or more structured questions (the `user.ask` core
    /// tool, adoption 03). Targets only the session's own human — no filesystem,
    /// command, network, or remote effect — so it is always policy-`Allow`ed
    /// like [`Self::RecordMemory`] and recorded purely so the ask is traced.
    /// Never serialized into a `ToolProposed`, so it needs no golden wire vector.
    AskUser {
        /// How many questions the call carries.
        question_count: usize,
        /// The bounded `header` of each question, for the trace.
        headers: Vec<String>,
    },
```

New question wire types (a new `crates/protocol/src/question.rs`, re-exported from `lib.rs`):

```rust
//! Question-domain wire types (adoption 03 — the `user.ask` tool).

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// One selectable choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Display text (1–5 words, concise).
    pub label: String,
    /// Explanation of the choice (may be empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question as asked. `custom` is carried on the wire but deliberately NOT
/// advertised in the tool schema — the model can never disable free-text
/// answers (opencode's Prompt/Info split).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    /// The complete question.
    pub question: String,
    /// Very short label (≤ 30 chars) shown as the card/tab title.
    pub header: String,
    /// Available choices (may be empty only when `custom` is true).
    pub options: Vec<QuestionOption>,
    /// Allow selecting more than one option.
    #[serde(default)]
    pub multiple: bool,
    /// Allow typing a custom answer (default true).
    #[serde(default = "default_true")]
    pub custom: bool,
}

/// How a question was resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum QuestionOutcome {
    /// One answer array per question, in question order; each answer is the
    /// selected labels (custom text is carried verbatim as a label).
    Answered { answers: Vec<Vec<String>> },
    /// The user dismissed the question; `feedback` is the optional typed
    /// correction fed back to the model (the CorrectedError port).
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    #[serde(other)]
    Unknown,
}
```

### 5.3 `crates/protocol/src/events.rs`

Append to `EventBody` (before `Unknown`):

```rust
    /// The run asked the operator structured questions and parked (adoption 03).
    QuestionAsked {
        question_id: QuestionId,
        run_id: RunId,
        questions: Vec<QuestionPrompt>,
    },
    /// The parked question was answered, rejected, or expired.
    QuestionResolved {
        question_id: QuestionId,
        outcome: QuestionOutcome,
    },
```

Add both to the round-trip test in the module tests, following `ApprovalRequested`'s existing vector.

### 5.4 `crates/protocol/src/command.rs`

Append to `CommandBody`:

```rust
    /// Resolve a parked question (adoption 03). Mirrors `ResolveApproval`:
    /// session-scoped, idempotent, revision-guarded.
    ResolveQuestion {
        question_id: QuestionId,
        outcome: QuestionOutcome,
    },
```

Extend the `session_id`-less routing arm the same way `ResolveApproval { approval_id, .. }` is handled (command.rs ~line 847): the daemon resolves the session by joining `questions → runs`.

### 5.5 `migrations/0034_questions.sql` (append-only; renumber to the next free number if 0034 is taken by the time this lands)

```sql
-- Adoption 03: durable question parking (the `user.ask` tool). A question is an
-- approval card with options instead of allow/deny, and it parks the same way:
-- a pending row + a ledger event, resurfaced on restart, expired when its run
-- can never consume the answer.
CREATE TABLE questions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    questions_json TEXT NOT NULL,          -- Vec<QuestionPrompt>
    state TEXT NOT NULL,                   -- pending | answered | rejected | expired
    answers_json TEXT,                     -- Vec<Vec<String>> when answered
    feedback TEXT,                         -- optional rejection feedback
    resolved_by TEXT,
    asked_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE INDEX idx_questions_pending ON questions(state, run_id);
```

### 5.6 `crates/daemon/src/questions.rs` (new)

A structural sibling of `approvals.rs`. Key shapes (bodies follow `ApprovalBroker` line-for-line where the comment says so):

```rust
//! Question broker (adoption 03): durable parking for `user.ask`.
//!
//! Deliberately a sibling of [`crate::approvals::ApprovalBroker`] — same
//! persist-then-publish, same watch-channel waiters, same restart story.

use codypendent_protocol::{
    Actor, EventBody, QuestionId, QuestionOutcome, QuestionPrompt, RunId, SessionEvent, SessionId,
};

/// What the parked run receives when the question resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionReply {
    Answered(Vec<Vec<String>>),
    Rejected { feedback: Option<String> },
}

#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub question_id: QuestionId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub questions: Vec<QuestionPrompt>,
    pub asked_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum QuestionError { /* NotFound, AlreadyResolved, WaiterGone, Corrupt, Database, Serde —
                            mirror ApprovalError variant-for-variant */ }

type Waiters = Arc<Mutex<HashMap<QuestionId, watch::Sender<Option<QuestionReply>>>>>;

#[derive(Debug, Clone, Default)]
pub struct QuestionBroker {
    waiters: Waiters,
    subscriptions: Option<SubscriptionHub>,
}

impl QuestionBroker {
    pub fn new() -> Self { Self::default() }
    #[must_use]
    pub fn with_subscriptions(mut self, subscriptions: SubscriptionHub) -> Self { /* as ApprovalBroker */ }

    /// Persist a `pending` row + append `QuestionAsked` in one BEGIN IMMEDIATE
    /// transaction, register the waiter BEFORE publishing (same race note as
    /// `ApprovalBroker::request_with_id_and_reuse`), publish post-commit.
    pub async fn ask(
        &self,
        pool: &SqlitePool,
        session_id: SessionId,
        run_id: RunId,
        questions: Vec<QuestionPrompt>,
    ) -> Result<QuestionId, QuestionError>;

    /// Block until resolved. Same borrow-before-await watch loop as
    /// `ApprovalBroker::await_decision`; removes the waiter on return.
    pub async fn await_reply(&self, question_id: QuestionId) -> Result<QuestionReply, QuestionError>;

    /// Flip the pending row and append `QuestionResolved` INSIDE the caller's
    /// transaction (the command write path), returning the exact event to
    /// publish. Mirrors `ApprovalBroker::resolve_in_tx`, including the
    /// rows_affected()==1 lost-race guard.
    pub(crate) async fn resolve_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        question_id: QuestionId,
        outcome: QuestionOutcome,
        resolved_by: String,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SessionEvent, QuestionError>;

    /// Standalone resolve (tests, recovery): tx + commit + wake.
    pub async fn resolve(&self, pool: &SqlitePool, question_id: QuestionId,
        outcome: QuestionOutcome, resolved_by: String) -> Result<SessionEvent, QuestionError>;

    /// Restart resurfacing — mirror of `ApprovalBroker::reload_pending`.
    pub async fn reload_pending(&self, pool: &SqlitePool) -> Result<Vec<PendingQuestion>, QuestionError>;

    /// Expire pending questions whose run is terminal — mirror of
    /// `ApprovalBroker::expire_orphaned` (state='expired', append
    /// `QuestionResolved { Rejected { feedback: None } }`, wake with Rejected).
    pub async fn expire_orphaned(&self, pool: &SqlitePool, now: chrono::DateTime<chrono::Utc>)
        -> Result<Vec<QuestionId>, QuestionError>;

    pub fn forget_waiter(&self, question_id: QuestionId);
    pub(crate) async fn wake(&self, question_id: QuestionId, reply: QuestionReply);
}
```

`state → outcome` mapping in `resolve_in_tx`: `Answered → 'answered'` (persist `answers_json`), `Rejected → 'rejected'` (persist `feedback`), anything else → a `QuestionError::UnsupportedOutcome`. Export `QuestionBroker` and types from `crates/daemon/src/lib.rs`.

### 5.7 `crates/daemon/src/policy/mod.rs`

New always-allow arm in `evaluate` (next to `RecordMemory`):

```rust
            ProposedAction::AskUser { .. } => self.eval_ask_user(),
```

```rust
    /// A `user.ask` call (adoption 03) is always permitted: it asks the
    /// session's own human a question — no filesystem, command, network, or
    /// remote effect — so it grants no capability and never reaches the
    /// approval gate, exactly like a memory proposal note.
    fn eval_ask_user(&self) -> PolicyDecision {
        PolicyDecision {
            decision: Decision::Allow,
            reasons: vec![PolicyReason::new(
                "policy.ask-user-allowed",
                "a question to the operator targets only the session's own human",
            )],
            capability_grant: None,
            policy_version: self.version.clone(),
            approval_reusable: false,
        }
    }
```

### 5.8 `crates/daemon/src/commands.rs`

- `CommandProcessor` gains a `questions: QuestionBroker` field (constructor-threaded like `approvals`).
- New dispatch arm `CommandBody::ResolveQuestion { question_id, outcome } => self.apply_resolve_question(...)`, implemented as a structural copy of `apply_resolve_question`'s sibling `apply_resolve_approval`: resolve the session via `SELECT r.session_id FROM questions q JOIN runs r ON q.run_id = r.id WHERE q.id = ?`, insert the `received` command row, apply the optional `expected_revision` guard under the same write lock, call `questions.resolve_in_tx(...)` with `ctx.principal.user_id().0` as `resolved_by`, commit, wake (`Answered → QuestionReply::Answered`, `Rejected → QuestionReply::Rejected`), publish the returned event, mark `applied`.
- `resume_received` gains the `ResolveQuestion` branch next to the existing `ResolveApproval` one (drive the resolve idempotently; `AlreadyResolved` is success).

### 5.9 `crates/daemon/src/recovery.rs` + daemon startup (`crates/codypendentd/src/lib.rs`)

Where startup currently calls `approvals.reload_pending(&pool)` / `expire_orphaned`, add the identical pair on the shared `QuestionBroker` instance. A resurfaced `PendingQuestion` re-registers its waiter so a recovering run's `await_reply` still lands.

### 5.10 `crates/runtime/src/tools/question.rs` (new)

```rust
//! The `user.ask` tool (adoption 03): structured mid-run questions.

use codypendent_protocol::{ProposedAction, QuestionOption, QuestionPrompt};

pub struct AskUser;

impl AskUser {
    pub const NAME: &'static str = "user.ask";
    /// Bounds — refused before anything parks.
    pub const MAX_QUESTIONS: usize = 5;
    pub const MAX_OPTIONS: usize = 12;
    pub const MAX_HEADER_CHARS: usize = 30;
    pub const MAX_TEXT_CHARS: usize = 2_000;

    pub fn proposed_action(questions: &[QuestionPrompt]) -> ProposedAction {
        ProposedAction::AskUser {
            question_count: questions.len(),
            headers: questions.iter().map(|q| q.header.clone()).collect(),
        }
    }
}

/// Parse and validate the model-supplied arguments. Errors are model-facing
/// correction prose (the opencode InvalidArgumentsError idiom).
pub fn parse_ask_user(args: &serde_json::Value) -> Result<Vec<QuestionPrompt>, String> {
    // - `questions` present, 1..=MAX_QUESTIONS
    // - each question/header/label/description non-empty where required,
    //   header ≤ MAX_HEADER_CHARS, every string ≤ MAX_TEXT_CHARS
    // - options ≤ MAX_OPTIONS; empty options allowed (custom is always on for
    //   model calls — free-text-only question)
    // - `custom` in the args is IGNORED (schema does not advertise it; a model
    //   that sends it does not get to disable free text)
    // - duplicate labels within one question rejected ("labels are the answer
    //   key — make them unique")
}

/// The tool observation for a full answer set — port of question.ts formatting.
pub fn render_answers(questions: &[QuestionPrompt], answers: &[Vec<String>]) -> String {
    let formatted = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let answer = answers
                .get(i)
                .filter(|a| !a.is_empty())
                .map(|a| a.join(", "))
                .unwrap_or_else(|| "Unanswered".to_string());
            format!("\"{}\"=\"{}\"", q.question, answer)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "User has answered your questions: {formatted}. \
         You can now continue with the user's answers in mind."
    )
}
```

Export from `crates/runtime/src/tools/mod.rs` (`pub use question::{parse_ask_user, render_answers, AskUser};`).

### 5.11 `crates/runtime/src/agent.rs`

**Seam** (next to `RoutingOutcomeSink`):

```rust
/// Pool-erased question parking (adoption 03), mirroring the RunJournal
/// approval closure: the daemon implements it over `QuestionBroker` + pool.
#[async_trait]
pub trait QuestionChannel: Send + Sync {
    async fn ask(
        &self,
        session_id: SessionId,
        run_id: RunId,
        questions: Vec<QuestionPrompt>,
    ) -> anyhow::Result<QuestionId>;
    async fn await_reply(&self, question_id: QuestionId) -> anyhow::Result<QuestionReplyWire>;
    /// Drop the waiter when the parked run is cancelled (leak guard, mirroring
    /// `ApprovalBroker::forget_waiter`).
    fn forget(&self, question_id: QuestionId);
}

/// The reply as the runtime sees it (the daemon's `QuestionReply`, re-declared
/// here because runtime→daemon is the only allowed direction and the daemon
/// type is already importable — use `codypendent_daemon::questions::QuestionReply`
/// directly instead if no ADR objects; then this alias disappears).
pub type QuestionReplyWire = codypendent_daemon::questions::QuestionReply;
```

(`codypendent-runtime` already depends on `codypendent-daemon`, so importing the broker's `QuestionReply` directly is legal; the trait exists so tests can stub it without a pool.)

**Runtime field + builder** (pattern: `artifacts`):

```rust
    /// The question channel `user.ask` parks on (adoption 03), if wired. `None`
    /// (workflow agent nodes, webhook runs, unattended surfaces) leaves the
    /// tool unoffered — a question nobody can answer must not be askable.
    questions: Option<Arc<dyn QuestionChannel>>,
```

```rust
    #[must_use]
    pub fn with_questions(mut self, questions: Arc<dyn QuestionChannel>) -> Self {
        self.questions = Some(questions);
        self
    }
```

**`static_tool_definitions()`** — add (description ports question.txt; note the schema has no `custom`):

```rust
        decl(
            AskUser::NAME,
            "Ask the user one or more structured questions when you need a decision, a \
             preference, or clarification you cannot infer. A \"Type your own answer\" option \
             is always added automatically, so never include an \"Other\" or catch-all option. \
             If you recommend an option, put it FIRST and append \" (Recommended)\" to its \
             label. Answers return as arrays of selected labels; set `multiple: true` to allow \
             selecting more than one. The run pauses until the user answers.",
            json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {"type": "string", "description": "Complete question"},
                                "header": {"type": "string", "description": "Very short label (max 30 chars)"},
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {"type": "string", "description": "Display text (1-5 words)"},
                                            "description": {"type": "string", "description": "Explanation of choice"}
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "multiple": {"type": "boolean", "description": "Allow selecting multiple choices"}
                            },
                            "required": ["question", "header", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        ),
```

Gate it in `offered_tool_names`/`advertised_tool_definitions` on `self.questions.is_some()` (the `artifact.read` pattern), and add it to the always-advertised set so the retrieval funnel can never hide it from an interactive run.

**`prepare()`** — new arm:

```rust
            AskUser::NAME => {
                let questions = parse_ask_user(args)?;
                Ok(PreparedTool {
                    action: AskUser::proposed_action(&questions),
                    input: PreparedInput::AskUser(questions),
                })
            }
```

**Execution** — because `user.ask` parks rather than executes, handle it in `run_tool` after the policy `Allow` (it is always `Allow`), *before* the generic `execute_prepared` tail, following the approval-parking block's structure:

```rust
        if let PreparedInput::AskUser(questions) = &prepared.input {
            let Some(channel) = &self.questions else {
                // unreachable when advertisement is gated, but fail legibly.
                return /* ToolCompleted Failed "user.ask is not available in this run" */;
            };
            let question_id = channel
                .ask(run.session_id, run.run_id, questions.clone())
                .await?;
            self.transition(run.session_id, run.run_id, RunState::WaitingForUserInput).await?;
            // ToolStarted with label "user.ask · <first header>"
            let reply = tokio::select! {
                reply = channel.await_reply(question_id) => reply?,
                _ = cancel.cancelled() => {
                    channel.forget(question_id);
                    return Ok(ToolFlow::Cancelled);
                }
            };
            self.transition(run.session_id, run.run_id, RunState::Running).await?;
            return match reply {
                QuestionReplyWire::Answered(answers) => {
                    // ToolCompleted Succeeded; observation = render_answers(...)
                }
                QuestionReplyWire::Rejected { feedback: Some(feedback) } => {
                    // ToolCompleted Failed; observation =
                    // format!("question rejected: the user declined to answer and said: {feedback}. \
                    //          Treat this as a correction and continue; do not re-ask.")
                }
                QuestionReplyWire::Rejected { feedback: None } => {
                    // ToolCompleted Failed; observation =
                    // "question rejected: the user dismissed this question. \
                    //  Continue without this information; do not re-ask."
                }
            };
        }
```

**`observation_is_refusal`** — extend so the loop-breaker sees a dismissed question as a refusal:

```rust
    output.starts_with("policy denied")
        || output == "approval rejected"
        || output.starts_with("question rejected")
```

### 5.12 `crates/codypendentd/src/executor.rs` (assembly)

Where `run_journal(&self.pool, &self.approvals)` is built, construct the channel over the shared broker and wire it **only for interactive session runs** (not in `workflow_exec.rs`):

```rust
struct PoolQuestionChannel {
    pool: SqlitePool,
    broker: QuestionBroker,
}

#[async_trait]
impl QuestionChannel for PoolQuestionChannel {
    async fn ask(&self, session_id: SessionId, run_id: RunId, questions: Vec<QuestionPrompt>)
        -> anyhow::Result<QuestionId>
    { Ok(self.broker.ask(&self.pool, session_id, run_id, questions).await?) }

    async fn await_reply(&self, id: QuestionId) -> anyhow::Result<QuestionReply> {
        Ok(self.broker.await_reply(id).await?)
    }

    fn forget(&self, id: QuestionId) { self.broker.forget_waiter(id); }
}
```

The `QuestionBroker` is created once at daemon assembly, bound `with_subscriptions(hub)`, shared by the executor and the `CommandProcessor` (same rule as `approvals`: "must be the same broker (a clone) … so `await_decision` observes resolutions").

### 5.13 TUI — `crates/tui/src/state.rs`, `input.rs`, `reduce.rs`, `render.rs`, `action.rs`; `crates/cli/src/tui.rs`

- `state.rs`: `PendingQuestion { question_id, run_id, questions: Vec<QuestionPrompt> }` + `pending_questions: Vec<PendingQuestion>` + card interaction state `QuestionCardState { index: usize /* current question */, selected: usize /* highlighted row */, picked: Vec<Vec<String>>, custom_text: Vec<String>, editing_custom: bool, confirming: bool, feedback: Option<String> /* Some = reject-feedback prompt open */ }`. New `InputMode::Question` variant with the same "owns the screen" doc as `Approval`.
- `input.rs`: `map_question_key` — Up/Down move the highlighted row (options, then the always-present "Type your own answer" row when `custom`, then Reject); digits `1..=9` fast-select; Space toggles when `multiple`; Enter selects (single single-select question ⇒ submit immediately, opencode's `single()` behavior; otherwise advance to the next question, and on the last, to a confirm row); typing any printable character while the custom row is highlighted enters `editing_custom` (printable keys then append; Enter commits the text as the answer label); `r` opens the reject-feedback prompt (a one-line editing prompt; Enter with text ⇒ `Rejected { feedback: Some(..) }`, Enter empty ⇒ `Rejected { feedback: None }`, Esc backs out to the card).
- `action.rs`: `Intent::ResolveQuestion { question_id, outcome: QuestionOutcome }`.
- `reduce.rs`: fold `EventBody::QuestionAsked` into `pending_questions` (idempotent upsert, mirroring the `ApprovalRequested` fold) and set `InputMode::Question` when a question targets the focused session; fold `QuestionResolved` by removing the entry, clamping selection, and finishing the associated tool card (`ToolOutcome::Failed { "question rejected" }` on reject — mirror the `ApprovalResolved` reject fold). `resolve_focused_question` pushes the intent.
- `render.rs`: the question card renders in the approvals pane region with the header as title, the question text, numbered options with descriptions, `▸` on the highlighted row, `✓` marks in multi-select, the custom row labeled `Type your own answer`, and a footer `↑↓ move · 1-9 pick · Space toggle · Enter select · r reject`. Progress `Question 2/3` when multiple questions.
- `crates/cli/src/tui.rs`: map `Intent::ResolveQuestion` → `CommandBody::ResolveQuestion` (next to the existing `Intent::ResolveApproval` arm ~line 4051).

## 6. Protocol & persistence

- **Commands**: `CommandBody::ResolveQuestion { question_id: QuestionId, outcome: QuestionOutcome }` — idempotent via `idempotency_key`, revision-guarded, `resolved_by` derived from the connection principal (never wire-supplied). Rejected for the Observer role like `ResolveApproval`.
- **Events**: `EventBody::QuestionAsked { question_id, run_id, questions }`, `EventBody::QuestionResolved { question_id, outcome }`. Persist-before-publish, both appended inside the broker's/processor's transaction, published post-commit through the `SubscriptionHub`. Both must appear in the events round-trip tests and deserialize to `Unknown` on a future tag.
- **Ledger/run state**: `Running → WaitingForUserInput` on ask, `WaitingForUserInput → Running` on resolve — transitions already legal in `crates/daemon/src/ledger.rs`; no ledger change needed.
- **Migration**: `0034_questions.sql` (§5.5), append-only.
- **Serde discipline**: `QuestionOutcome` internally tagged with `#[serde(other)] Unknown`; `QuestionPrompt.custom` defaults true; `QuestionOption.description` defaults empty.

## 7. Acceptance criteria

1. RUN `cargo test -p codypendent-protocol` — EXPECT the new round-trip vectors for `QuestionAsked`/`QuestionResolved`/`ResolveQuestion`/`ProposedAction::AskUser` pass, and a future-tagged `QuestionOutcome` deserializes to `Unknown`.
2. A run that calls `user.ask` transitions to `WaitingForUserInput` (visible in `RunStateChanged`), and the `questions` row is `pending`.
3. `ResolveQuestion` with `Answered` wakes the parked run; the model observation is exactly the `render_answers` format including the literal `Unanswered` for an empty slot; run returns to `Running`.
4. `ResolveQuestion` with `Rejected { feedback: Some("use sqlite") }` produces a `ToolCompleted { Failed }` and an observation containing `use sqlite` — the model sees the correction text.
5. Daemon restart with a pending question: `reload_pending` re-surfaces it (a newly attached TUI renders the card), and a subsequent `ResolveQuestion` still wakes the resumed run.
6. A pending question whose run is terminal is expired on boot (`expire_orphaned`), appending `QuestionResolved { Rejected }`.
7. The tool is **not advertised** when no `QuestionChannel` is wired: RUN a workflow agent-node run — EXPECT `user.ask` absent from `advertised_tool_definitions`.
8. `parse_ask_user` refuses: zero questions, > 5 questions, a header > 30 chars, duplicate labels in one question — each with a correction-prose error, and nothing parks.
9. A model-supplied `"custom": false` in the args is ignored (the parsed prompt has `custom == true`).
10. Duplicate `ResolveQuestion` (same idempotency key) replays the recorded outcome; a second resolve with a fresh key returns `AlreadyResolved`.
11. Cancelling a run parked on a question drops the waiter (`forget`) — no entry remains in the broker's waiter map.
12. RUN `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace` — EXPECT green.

## 8. Tests

- `crates/protocol/src/question.rs` — `question_types_round_trip`, `unknown_outcome_tag_deserializes_to_unknown`, `custom_defaults_true`.
- `crates/daemon/src/questions.rs` (mirror the approvals test suite, same fixtures):
  `answer_round_trip_wakes_the_waiter`, `reject_with_feedback_carries_the_feedback`,
  `restart_re_surfaces_pending_and_still_resolves`, `orphaned_question_is_expired_on_boot`,
  `double_resolve_reports_already_resolved`, `resolving_a_missing_question_is_not_found`.
- `crates/daemon/src/commands.rs` — `resolve_question_is_idempotent_and_revision_guarded`, `resume_received_drives_resolve_question`.
- `crates/daemon/src/policy/mod.rs` — `ask_user_is_always_allowed_and_never_reusable`.
- `crates/runtime` (agent.rs tests, following the existing in-file middleware tests):
  `ask_user_parks_in_waiting_for_user_input_and_resumes_on_answer` (stub `QuestionChannel`),
  `ask_user_reject_feedback_reaches_the_observation`,
  `ask_user_unavailable_without_a_channel`,
  `parse_ask_user_bounds` (unit, tools/question.rs),
  `render_answers_matches_opencode_format`,
  `question_rejection_counts_as_refusal` (extends the `observation_is_refusal` test at agent.rs ~line 10421).
- `crates/tui` (reducer tests, following the `ApprovalRequested` fixtures at reduce.rs ~line 7602):
  `question_asked_enters_question_mode`, `single_single_select_submits_on_enter`,
  `multi_select_toggles_and_confirms`, `custom_answer_text_becomes_the_label`,
  `reject_with_feedback_emits_resolve_question_intent`, `question_resolved_clears_the_card`.

## 9. Gotchas

1. **Register the waiter before publishing** (approvals.rs comment at `request_with_id_and_reuse`): a live controller can answer between commit and waiter registration; `QuestionBroker::ask` must copy the ordering and `wake` must create-pre-resolved for an unknown id, or the run parks forever.
2. **Never `.await` while holding the waiters mutex** — the approvals module doc explains why the std `Mutex` is correct only because it is never held across an await. Copy the `borrow_and_update` loop exactly.
3. **`forget_waiter` on cancellation** — `run_tool`'s approval arm calls `forget_waiter` in the cancel race; omit the question-side call and every cancelled parked question leaks a watch sender for the daemon's lifetime.
4. **Do not put the question text in `ProposedAction::AskUser`** — actions are serialized into approval/audit records with an `Eq` + stable-digest contract; the full prompt list lives in the event and the `questions` table. Only counts and bounded headers travel on the action.
5. **The `Unanswered` literal is load-bearing** — opencode's tool output uses exactly `"Unanswered"` for an empty slot; models are steered by the prompt text to treat it as skippable. Don't "improve" it to an empty string.
6. **`custom` must not appear in the tool JSON schema** — advertising it invites the model to set `custom: false` and strip the user's free-text option, which is the exact taste question.txt encodes (opencode splits `Prompt` from `Info` for this one field).
7. **Answers echo user text into the model transcript verbatim** — the custom answer is untrusted user input but it is *the user's own instruction*; do not sanitize it beyond the `MAX_TEXT_CHARS` bound (contrast with sandbox output, which must pass `sanitize_untrusted`).
8. **`resume_received` must learn the new command** — commands.rs documents "only `ResolveApproval` has one [external effect] in Phase 1"; a crash mid-`ResolveQuestion` that isn't resumed leaves the row `received` forever and the answer lost.
9. **The reducer must upsert, not push** — `ApprovalRequested`'s fold replaces an existing entry by id (restart resurfacing re-delivers the event); copy that or reconnects duplicate cards.
10. **Interactive-only wiring is the safety rule, not the advertisement gate alone** — even if a future caller hands a workflow run a channel, a question with nobody attached parks a run indefinitely; `expire_orphaned` only fires on boot. Keep the wiring decision in the executor, where run provenance is known.

## 10. Out of scope

- Reject-with-feedback on **approvals** (`ResolveApproval` keeps its current shape; the natural follow-up reuses `QuestionOutcome::Rejected`'s pattern).
- Question deadlines/expiry timers (`expires_at`) — approvals have them; questions expire only via run-terminal orphaning in v1.
- Internal (non-model) callers of the question service (opencode's plan-exit "switch to build?" flow) — the `custom: false` wire field exists for them, nothing uses it yet.
- Inline transcript anchoring of the card at the exact tool-call part (opencode's `tool: {messageID, callID}` link) — the card renders in the approvals pane region; a `ToolStarted` label marks the transcript position.
- Multi-question tab mouse navigation and per-question descriptions rendering beyond one line.
- ACP / Remote UI surfaces for questions.

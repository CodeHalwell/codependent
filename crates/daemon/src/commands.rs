//! Command handling and the crash-consistent write path (STEP 1.3).
//!
//! This is the single most important algorithm in the product: the *idempotent*,
//! *crash-consistent* application of a client [`Command`]. Every command follows
//! the same six-step sequence (Chapter 03 "Crash consistency"):
//!
//! 1. **Idempotency check first.** Look up `commands.idempotency_key`. A row in
//!    `status = 'applied'` returns its recorded `result_json` verbatim — nothing
//!    re-executes (this is the exit criterion: *duplicate delivery produces one
//!    effect and one result*). A row in `status = 'received'` means a crash
//!    landed mid-apply, so we *resume reconciliation* rather than re-execute.
//! 2. **Validate.** Schema ([`CommandBody::Unknown`] → `protocol.unsupported-payload`),
//!    session/run existence where required, and the caller's [`ClientRole`]
//!    ([`ClientRole::Observer`] issuing `StartRun` → `protocol.role-denied`).
//!    Handlers return a structured [`CodypendentError`]; they never panic.
//! 3. **One transaction.** Insert the `commands` row (`received`), insert any
//!    `pending_effects`, append the resulting ledger event(s) — allocating
//!    `sequence` *inside this tx* (the [`crate::approvals`] atomic-append
//!    pattern) — update the projection rows (`runs`), set `commands.status =
//!    'applied'` with its `result_json`, and COMMIT.
//! 4. **Perform the external side effect** (if any) *outside* the transaction.
//!    Almost every Phase 1 command has none — the real tool effects happen in
//!    the agent loop (STEP 1.10). `ResolveApproval`'s effect (flip the approval
//!    row + append `ApprovalResolved`) is folded *into* the command transaction
//!    via [`crate::approvals::ApprovalBroker::resolve_in_tx`], so its
//!    `expected_revision` guard, the append, and the revision bump are all
//!    atomic (issue #6 item 2); only the parked-waiter wake happens after commit.
//! 5. **Persist the outcome** (`pending_effects` → `performed`/`reconciled`,
//!    append an outcome event) once the effect completes.
//! 6. **Publish** the persisted events through the [`SubscriptionHub`] — *after*
//!    commit, never before (persist before publish, RULE 2).
//!
//! Because steps 3's `received`→`applied` transition is atomic, a committed
//! `commands` row is always `applied`; the `received` state is only durable for
//! rows written by a crash-injection test. Startup recovery
//! ([`CommandProcessor::reconcile_pending_effects`]) sweeps any orphaned
//! `pending_effects`; STEP 1.14 extends that recovery.

use std::str::FromStr;

use chrono::Utc;
use codypendent_protocol::{
    Actor, AgentMode, ApprovalDecision, ApprovalScope, ClientId, ClientRole, CodypendentError,
    Command, CommandBody, CommandId, EventBody, ModelId, PromptDelivery, PromptId, QuestionId,
    QuestionOutcome, RunId, RunState, SessionEvent, SessionId,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::approvals::{ApprovalBroker, ApprovalError};
use crate::principal::PeerPrincipal;
use crate::projections;
use crate::questions::{QuestionBroker, QuestionError, QuestionReply};
use crate::subscriptions::SubscriptionHub;

/// A run's resolved model policy is not carried by the Phase 1 `StartRun`
/// command; the write path records this default (a `models.toml` profile id).
const DEFAULT_MODEL_POLICY: &str = "hosted-default";
/// Likewise the run budget: an empty JSON object until the agent loop sets one.
const DEFAULT_BUDGET_JSON: &str = "{}";

/// Who is issuing a command, for validation and event attribution.
///
/// Three separate things, deliberately not conflated:
///
/// * `principal` — **who you are**, derived by the server from the connection's
///   peer credentials ([`PeerPrincipal`]). This is what authorizes access to a
///   session and what every `Actor::Human` is minted from.
/// * `role` — **what you asked to be limited to**. A client-supplied assertion
///   that can only narrow (see [`role_permits`]); it never widens what the
///   principal may reach.
/// * `client_id` — **which connection this is**, for correlation: the
///   `commands` row, presence, and event attribution. Not authority.
#[derive(Debug, Clone)]
pub struct ApplyContext {
    pub client_id: ClientId,
    pub role: ClientRole,
    pub principal: PeerPrincipal,
}

/// The recorded result of applying a command, stored as `commands.result_json`
/// and replayed **verbatim** on an idempotent repeat. Two applications of the
/// same envelope therefore return an equal `CommandOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub command_id: CommandId,
    /// The session created by a `CreateSession`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_session: Option<SessionId>,
    /// The run created by a `StartRun`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_run: Option<RunId>,
    /// The sequence of the last event this command appended, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sequence: Option<u64>,
    /// Whether THIS call freshly applied the command, as opposed to replaying a
    /// recorded outcome for a duplicate idempotency key. Never persisted, so a
    /// replayed outcome (deserialized from the `commands` row) is always `false`
    /// — which is exactly how the server launches the executor **once** per run
    /// instead of again on every duplicate `StartRun` delivery.
    #[serde(skip)]
    pub newly_applied: bool,
}

/// Applies commands through the crash-consistent write path, owning the shared
/// [`SubscriptionHub`] it publishes to and the [`ApprovalBroker`] / [`QuestionBroker`]
/// it delegates resolutions to. Cloning shares all three (each is `Arc`-backed).
#[derive(Debug, Clone, Default)]
pub struct CommandProcessor {
    subscriptions: SubscriptionHub,
    approvals: ApprovalBroker,
    questions: QuestionBroker,
}

impl CommandProcessor {
    /// A processor wired to a shared subscription hub, approval broker, and question broker.
    pub fn new(
        subscriptions: SubscriptionHub,
        approvals: ApprovalBroker,
        questions: QuestionBroker,
    ) -> Self {
        Self {
            subscriptions,
            approvals,
            questions,
        }
    }

    /// The shared fan-out this processor publishes committed events to. Callers
    /// (the protocol server, tests) clone it to `subscribe`.
    pub fn subscriptions(&self) -> &SubscriptionHub {
        &self.subscriptions
    }

    /// The approval broker this processor delegates `ResolveApproval` to.
    pub fn approvals(&self) -> &ApprovalBroker {
        &self.approvals
    }

    /// The question broker this processor delegates `ResolveQuestion` to.
    pub fn questions(&self) -> &QuestionBroker {
        &self.questions
    }

    /// Replay an already-recorded command outcome without admitting a new
    /// command. The server uses this before an expensive external input
    /// preprocessing step (voice transcription): a lost reply must not send the
    /// same audio off-device again merely to discover that the write path had
    /// already applied its idempotency key.
    pub async fn replay_existing(
        &self,
        pool: &SqlitePool,
        idempotency_key: &str,
    ) -> Result<Option<CommandOutcome>, CodypendentError> {
        let existing = lookup_command(pool, idempotency_key)
            .await
            .map_err(internal_error)?;
        match existing {
            Some(existing) => self.handle_existing(pool, existing).await.map(Some),
            None => Ok(None),
        }
    }

    /// Apply one command through the full six-step sequence. Idempotent on
    /// `idempotency_key`; returns a structured [`CodypendentError`] on any bad
    /// input, never panics.
    pub async fn apply(
        &self,
        pool: &SqlitePool,
        ctx: ApplyContext,
        command: Command,
    ) -> Result<CommandOutcome, CodypendentError> {
        // Step 1: idempotency check FIRST.
        if let Some(existing) = lookup_command(pool, &command.idempotency_key)
            .await
            .map_err(internal_error)?
        {
            return self.handle_existing(pool, existing).await;
        }

        // Step 2: validate (schema, existence, role).
        self.validate(pool, &ctx, &command).await?;

        // Steps 3-6 per variant.
        match command.body.clone() {
            CommandBody::CreateSession { title, .. } => {
                self.apply_create_session(pool, &ctx, &command, title).await
            }
            CommandBody::StartRun {
                session_id,
                objective,
                mode,
                // `repository` is consumed by the server when it builds the
                // executor's `RunLaunch` (it decides the run's repository
                // identity), not by the write path — the ledger row is the same.
                ..
            } => {
                self.apply_start_run(pool, &ctx, &command, session_id, objective, mode)
                    .await
            }
            CommandBody::SubmitUserInput {
                session_id,
                text,
                mode,
                // `model` (a mid-conversation pin) is persisted verbatim on the
                // command body — exactly like `StartRun.model` — and recovered by
                // `session_run_provenance` / `queued_run_overrides`, not written by
                // the projection path. The ledger row is the same either way.
                ..
            } => {
                self.apply_submit_input(pool, &ctx, &command, session_id, text, mode)
                    .await
            }
            CommandBody::QueueSteering { run_id, .. } => {
                self.apply_queue_steering(pool, &ctx, &command, run_id)
                    .await
            }
            CommandBody::CancelRun { run_id } => {
                self.apply_run_state(pool, &ctx, &command, run_id, RunState::Cancelled)
                    .await
            }
            CommandBody::PauseRun { run_id } => {
                self.apply_run_state(pool, &ctx, &command, run_id, RunState::Paused)
                    .await
            }
            CommandBody::ResumeRun { run_id } => {
                self.apply_run_state(pool, &ctx, &command, run_id, RunState::Running)
                    .await
            }
            CommandBody::ResolveApproval {
                approval_id,
                decision,
                scope,
            } => {
                self.apply_resolve_approval(pool, &ctx, &command, approval_id, decision, scope)
                    .await
            }
            CommandBody::ResolveQuestion {
                question_id,
                outcome,
            } => {
                self.apply_resolve_question(pool, &ctx, &command, question_id, outcome)
                    .await
            }
            CommandBody::QueuePrompt {
                session_id,
                text,
                mode,
                delivery,
            } => {
                self.apply_queue_prompt(pool, &ctx, &command, session_id, text, mode, delivery)
                    .await
            }
            CommandBody::UpdateQueuedPrompt {
                session_id,
                prompt_id,
                text,
                delivery,
            } => {
                self.apply_update_queued_prompt(
                    pool, &ctx, &command, session_id, prompt_id, text, delivery,
                )
                .await
            }
            CommandBody::PromoteQueuedPrompt {
                session_id,
                prompt_id,
            } => {
                self.apply_promote_queued_prompt(pool, &ctx, &command, session_id, prompt_id)
                    .await
            }
            CommandBody::DeleteQueuedPrompt {
                session_id,
                prompt_id,
            } => {
                self.apply_delete_queued_prompt(pool, &ctx, &command, session_id, prompt_id)
                    .await
            }
            // `AttachSession`/`Unknown` are already rejected in `validate`; this
            // catch-all keeps the (non_exhaustive) match total and restates the
            // rejection defensively.
            _ => Err(rejected_for_body(&command.body)),
        }
    }

    /// Scan `pending_effects` still in flight (`intended`, or `performed`
    /// awaiting an outcome) and reconcile them against reality, then mark each
    /// `reconciled`/`abandoned` and append a reconciliation event. Returns how
    /// many effects were reconciled. Called on startup and by the `received`
    /// resume path; STEP 1.14 layers richer recovery on top.
    pub async fn reconcile_pending_effects(&self, pool: &SqlitePool) -> anyhow::Result<usize> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, command_id, kind, state FROM pending_effects \
             WHERE state IN ('intended', 'performed')",
        )
        .fetch_all(pool)
        .await?;

        let mut reconciled = 0usize;
        for (id, command_id, kind, state) in rows {
            if self
                .reconcile_effect(pool, &id, &command_id, &kind, &state)
                .await?
            {
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    // --- idempotency branches -------------------------------------------------

    /// Handle a command whose `idempotency_key` is already recorded.
    async fn handle_existing(
        &self,
        pool: &SqlitePool,
        existing: ExistingCommand,
    ) -> Result<CommandOutcome, CodypendentError> {
        match existing.status.as_str() {
            // Applied: replay the recorded outcome verbatim, execute nothing.
            "applied" => {
                let json = existing
                    .result_json
                    .ok_or_else(|| internal_error("applied command row is missing result_json"))?;
                serde_json::from_str(&json).map_err(internal_error)
            }
            // Received: a crash landed mid-apply — resume, do not re-execute.
            "received" => self.resume_received(pool, existing).await,
            other => Err(internal_error(format!(
                "command in unexpected status {other:?}"
            ))),
        }
    }

    /// Resume a command that committed its `received` row but crashed before it
    /// finished. Reconcile its pending effects, drive its external effect to
    /// completion idempotently (only `ResolveApproval` has one in Phase 1), then
    /// mark it `applied`.
    async fn resume_received(
        &self,
        pool: &SqlitePool,
        existing: ExistingCommand,
    ) -> Result<CommandOutcome, CodypendentError> {
        self.reconcile_command_effects(pool, &existing.command_id)
            .await
            .map_err(internal_error)?;

        let body: CommandBody = serde_json::from_str(&existing.body).map_err(internal_error)?;

        // `ForkSession` has an external effect (creating the forked session) that
        // is NOT folded into the command transaction. Complete it idempotently on
        // resume — mirroring how `ResolveApproval`'s effect is re-driven below —
        // so a fork whose `applied` finalize was skipped by a crash is finished on
        // recovery/retry, returning the SAME forked session id rather than leaving
        // it permanently `fork.in-progress`.
        if let CommandBody::ForkSession {
            session_id,
            checkpoint,
            name,
        } = &body
        {
            if let Some(outcome) = self
                .resume_fork(pool, &existing, *session_id, *checkpoint, name.clone())
                .await?
            {
                return Ok(outcome);
            }
        }

        if let CommandBody::ResolveApproval {
            approval_id,
            decision,
            scope,
        } = body
        {
            match self
                .approvals
                .resolve(
                    pool,
                    approval_id,
                    decision,
                    scope,
                    existing.client_id.clone(),
                )
                .await
            {
                // Completed now: publish the exact appended `ApprovalResolved`
                // so live subscribers observe it instead of a sequence gap they
                // only close on re-attach (persist-before-publish: `resolve`
                // committed before returning the event).
                Ok(event) => {
                    if let Some(session_id) = existing.session_id {
                        self.subscriptions.publish(session_id, event);
                    }
                }
                // Already resolved before the crash — the effect is done exactly
                // once and its event was published by whoever resolved it.
                Err(ApprovalError::AlreadyResolved { .. }) => {}
                Err(e) => return Err(map_approval_error(e)),
            }
        } else if let CommandBody::ResolveQuestion {
            question_id,
            outcome,
        } = body
        {
            match self
                .questions
                .resolve(pool, question_id, outcome, existing.client_id.clone())
                .await
            {
                Ok(event) => {
                    if let Some(session_id) = existing.session_id {
                        self.subscriptions.publish(session_id, event);
                    }
                }
                Err(QuestionError::AlreadyResolved { .. }) => {}
                Err(e) => return Err(map_question_error(e)),
            }
        }

        let last_sequence = match existing.session_id {
            Some(session_id) => max_sequence(pool, session_id)
                .await
                .map_err(internal_error)?,
            None => None,
        };
        let outcome = CommandOutcome {
            command_id: existing.command_id,
            created_session: None,
            created_run: None,
            last_sequence,
            newly_applied: false,
        };
        finalize_applied(pool, existing.command_id, &outcome)
            .await
            .map_err(internal_error)?;
        Ok(outcome)
    }

    /// Complete a `ForkSession` reservation whose `applied` finalize was skipped
    /// by a crash. The forked session id was pre-recorded on the reservation's
    /// `result_json`, so this re-drives the (idempotent, atomic) fork with that
    /// SAME id and finalizes `applied` — recovery returns the same forked session,
    /// never a second fork and never a permanent `fork.in-progress`.
    ///
    /// Returns `Ok(None)` when the reservation carries no recorded fork id (a
    /// legacy row), so the caller falls back to the generic finalize.
    async fn resume_fork(
        &self,
        pool: &SqlitePool,
        existing: &ExistingCommand,
        session_id: SessionId,
        checkpoint: codypendent_protocol::CheckpointId,
        name: Option<String>,
    ) -> Result<Option<CommandOutcome>, CodypendentError> {
        let recorded: Option<CommandOutcome> = existing
            .result_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok());
        let Some(fork_id) = recorded
            .as_ref()
            .and_then(|outcome| outcome.created_session)
        else {
            return Ok(None);
        };

        let checkpoint_row = crate::worktrees::fetch_checkpoint(pool, checkpoint)
            .await
            .map_err(internal_error)?;
        if let Some(checkpoint_row) = checkpoint_row {
            let owner_uid = session_owner_uid(pool, session_id)
                .await
                .map_err(internal_error)?;
            // Idempotent + atomic: if the fork already committed this returns the
            // same id immediately; otherwise it re-drives the whole fork.
            crate::forks::fork_session(pool, session_id, checkpoint_row, name, owner_uid, fork_id)
                .await?;
        } else if !crate::ledger::session_exists(pool, fork_id)
            .await
            .map_err(internal_error)?
        {
            // The checkpoint is gone AND the fork never committed — it cannot be
            // completed now. Leave the reservation `received` and reject retryably
            // rather than fabricate an outcome for a session that does not exist.
            return Err(CodypendentError::new(
                "fork.in-progress",
                "fork cannot be completed on recovery: checkpoint is no longer available",
                true,
            ));
        }

        let outcome = CommandOutcome {
            command_id: existing.command_id,
            created_session: Some(fork_id),
            created_run: None,
            last_sequence: recorded.and_then(|outcome| outcome.last_sequence),
            newly_applied: false,
        };
        finalize_applied(pool, existing.command_id, &outcome)
            .await
            .map_err(internal_error)?;
        Ok(Some(outcome))
    }

    // --- validation -----------------------------------------------------------

    async fn validate(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
    ) -> Result<(), CodypendentError> {
        // Schema: a body from a newer client, or attach (a connection-level
        // concern, not the generic write path — STEP 1.11).
        match &command.body {
            CommandBody::Unknown => {
                return Err(CodypendentError::new(
                    "protocol.unsupported-payload",
                    "unknown command body",
                    false,
                ));
            }
            CommandBody::AttachSession { .. } => {
                return Err(CodypendentError::new(
                    "protocol.attach-is-connection-level",
                    "AttachSession is handled by the connection layer, not the command write path",
                    false,
                ));
            }
            _ => {}
        }

        // Role: checked before existence so a denied role never leaks whether a
        // resource exists, and `Observer`-issues-`StartRun` is `role-denied`
        // regardless of the session.
        if !role_permits(ctx.role, &command.body) {
            return Err(CodypendentError::new(
                "protocol.role-denied",
                format!("role {:?} may not issue this command", ctx.role),
                false,
            ));
        }

        // Existence where the command targets pre-existing state.
        match &command.body {
            CommandBody::StartRun { session_id, .. }
            | CommandBody::SubmitUserInput { session_id, .. } => {
                if !session_exists(pool, *session_id)
                    .await
                    .map_err(internal_error)?
                {
                    return Err(CodypendentError::new(
                        "protocol.session-not-found",
                        format!("no session {session_id}"),
                        false,
                    ));
                }
            }
            CommandBody::CancelRun { run_id }
            | CommandBody::PauseRun { run_id }
            | CommandBody::ResumeRun { run_id } => {
                let state = projections::load_run_state(pool, *run_id)
                    .await
                    .map_err(internal_error)?
                    .ok_or_else(|| run_not_found(*run_id))?;
                validate_run_transition(&command.body, *run_id, state)?;
            }
            CommandBody::QueueSteering { run_id, .. } => {
                if projections::run_session(pool, *run_id)
                    .await
                    .map_err(internal_error)?
                    .is_none()
                {
                    return Err(run_not_found(*run_id));
                }
            }
            CommandBody::ResolveApproval { approval_id, .. } => {
                let existing_session = approval_session(pool, *approval_id)
                    .await
                    .map_err(internal_error)?;
                if existing_session.is_none() {
                    return Err(CodypendentError::new(
                        "approval.not-found",
                        format!("no approval {approval_id}"),
                        false,
                    ));
                }
            }
            CommandBody::QueuePrompt {
                session_id, text, ..
            } => {
                if text.trim().is_empty() {
                    return Err(CodypendentError::new(
                        "prompt-queue.empty",
                        "queued prompt text cannot be empty",
                        false,
                    ));
                }
                if !session_exists(pool, *session_id)
                    .await
                    .map_err(internal_error)?
                {
                    return Err(CodypendentError::new(
                        "protocol.session-not-found",
                        format!("no session {session_id}"),
                        false,
                    ));
                }
            }
            CommandBody::UpdateQueuedPrompt {
                session_id, text, ..
            } => {
                if text.as_ref().is_some_and(|t| t.trim().is_empty()) {
                    return Err(CodypendentError::new(
                        "prompt-queue.empty",
                        "queued prompt text cannot be empty",
                        false,
                    ));
                }
                if !session_exists(pool, *session_id)
                    .await
                    .map_err(internal_error)?
                {
                    return Err(CodypendentError::new(
                        "protocol.session-not-found",
                        format!("no session {session_id}"),
                        false,
                    ));
                }
            }
            CommandBody::PromoteQueuedPrompt { session_id, .. }
            | CommandBody::DeleteQueuedPrompt { session_id, .. }
                if !session_exists(pool, *session_id)
                    .await
                    .map_err(internal_error)? =>
            {
                return Err(CodypendentError::new(
                    "protocol.session-not-found",
                    format!("no session {session_id}"),
                    false,
                ));
            }
            _ => {}
        }
        Ok(())
    }

    // --- per-command handlers -------------------------------------------------

    async fn apply_create_session(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        title: String,
    ) -> Result<CommandOutcome, CodypendentError> {
        let session_id = SessionId::new();
        // The session row is created *inside* the write transaction (inlined
        // rather than `ledger::create_session`, which takes a pool) so it is
        // atomic with the `SessionCreated` event, the `commands` row, and the
        // idempotency guarantee — a retry with the same key can never mint a
        // second session.
        let events = vec![(
            Actor::Client {
                client_id: ctx.client_id,
            },
            EventBody::SessionCreated {
                title: title.clone(),
            },
        )];
        self.run_transaction(
            pool,
            ctx,
            command,
            Some(session_id),
            session_id,
            PreInsert::Session {
                session_id,
                title: &title,
                owner_uid: ctx.principal.uid(),
            },
            events,
            ProjectionOp::None,
            (Some(session_id), None),
            // The session is being created now, at revision 0. There is no prior
            // session to guard, so `expected_revision` is ignored here (the
            // sensible rule for `CreateSession`).
            RevisionOp::Establish,
        )
        .await
    }

    async fn apply_start_run(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        objective: String,
        mode: AgentMode,
    ) -> Result<CommandOutcome, CodypendentError> {
        let run_id = RunId::new();
        let events = vec![(
            Actor::Client {
                client_id: ctx.client_id,
            },
            EventBody::RunStarted {
                run_id,
                objective: objective.clone(),
                mode,
            },
        )];
        self.run_transaction(
            pool,
            ctx,
            command,
            Some(session_id),
            session_id,
            PreInsert::None,
            events,
            ProjectionOp::InsertRun {
                run_id,
                session_id,
                objective,
                mode,
            },
            (None, Some(run_id)),
            RevisionOp::Bump {
                expected: command.expected_revision,
            },
        )
        .await
    }

    /// A follow-up message CONTINUES the conversation: it launches a new bounded
    /// run whose objective is the user's `text` (continuous-session plan, Task 3).
    /// Mirrors [`apply_start_run`](Self::apply_start_run) exactly — mint a
    /// `RunId`, append `RunStarted`, insert the run projection, under the same
    /// role gate and `expected_revision`/idempotency handling — so a
    /// `SubmitUserInput` becomes an ordinary run. The daemon does NOT reconstruct
    /// the prior transcript here (it cannot build the runtime's `TurnItem`); the
    /// assembly executor seeds that from the session ledger at run start.
    async fn apply_submit_input(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        text: String,
        mode: AgentMode,
    ) -> Result<CommandOutcome, CodypendentError> {
        let run_id = RunId::new();
        let events = vec![(
            Actor::Client {
                client_id: ctx.client_id,
            },
            EventBody::RunStarted {
                run_id,
                objective: text.clone(),
                mode,
            },
        )];
        self.run_transaction(
            pool,
            ctx,
            command,
            Some(session_id),
            session_id,
            PreInsert::None,
            events,
            ProjectionOp::InsertRun {
                run_id,
                session_id,
                objective: text,
                mode,
            },
            (None, Some(run_id)),
            RevisionOp::Bump {
                expected: command.expected_revision,
            },
        )
        .await
    }

    async fn apply_queue_steering(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        run_id: RunId,
    ) -> Result<CommandOutcome, CodypendentError> {
        let session_id = projections::run_session(pool, run_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| run_not_found(run_id))?;

        let text = match &command.body {
            CommandBody::QueueSteering { text, .. } => text.clone(),
            _ => String::new(),
        };

        let run_mode = projections::load_run_mode(pool, run_id)
            .await
            .map_err(internal_error)?
            .unwrap_or(AgentMode::Build);

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        let prompts = if !text.trim().is_empty() {
            crate::prompt_queue::enqueue(
                &mut tx,
                session_id,
                &text,
                run_mode,
                PromptDelivery::Steer,
            )
            .await
            .map_err(internal_error)?
        } else {
            crate::prompt_queue::snapshot(&mut tx, session_id)
                .await
                .map_err(internal_error)?
        };

        let seq1 = next_sequence(&mut *tx, session_id)
            .await
            .map_err(internal_error)?;
        let actor = Actor::Client {
            client_id: ctx.client_id,
        };
        let body1 = EventBody::SteeringQueued { run_id };
        append_event(
            &mut *tx,
            session_id,
            seq1,
            &actor,
            &body1,
            &now_str,
            Some(command.command_id),
        )
        .await
        .map_err(internal_error)?;

        let seq2 = next_sequence(&mut *tx, session_id)
            .await
            .map_err(internal_error)?;
        let body2 = EventBody::PendingPromptsChanged { prompts };
        append_event(
            &mut *tx,
            session_id,
            seq2,
            &actor,
            &body2,
            &now_str,
            Some(command.command_id),
        )
        .await
        .map_err(internal_error)?;

        sqlx::query("UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;

        sqlx::query("UPDATE commands SET status = 'applied', applied_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(command.command_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        let event1 = SessionEvent {
            sequence: u64::try_from(seq1).map_err(internal_error)?,
            occurred_at: now,
            causation_id: Some(command.command_id),
            correlation_id: None,
            actor: actor.clone(),
            body: body1,
        };
        let event2 = SessionEvent {
            sequence: u64::try_from(seq2).map_err(internal_error)?,
            occurred_at: now,
            causation_id: Some(command.command_id),
            correlation_id: None,
            actor,
            body: body2,
        };
        self.subscriptions.publish(session_id, event1.clone());
        self.subscriptions.publish(session_id, event2.clone());

        Ok(CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence: Some(u64::try_from(seq2).map_err(internal_error)?),
            newly_applied: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_queue_prompt(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        text: String,
        mode: AgentMode,
        delivery: PromptDelivery,
    ) -> Result<CommandOutcome, CodypendentError> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        let prompts = crate::prompt_queue::enqueue(&mut tx, session_id, &text, mode, delivery)
            .await
            .map_err(internal_error)?;

        let seq = next_sequence(&mut *tx, session_id)
            .await
            .map_err(internal_error)?;
        let actor = Actor::Client {
            client_id: ctx.client_id,
        };
        let body = EventBody::PendingPromptsChanged { prompts };

        append_event(
            &mut *tx,
            session_id,
            seq,
            &actor,
            &body,
            &now_str,
            Some(command.command_id),
        )
        .await
        .map_err(internal_error)?;

        sqlx::query("UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;

        let outcome = CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence: Some(u64::try_from(seq).map_err(internal_error)?),
            newly_applied: true,
        };

        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome).map_err(internal_error)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        let event = SessionEvent {
            sequence: u64::try_from(seq).map_err(internal_error)?,
            occurred_at: now,
            causation_id: Some(command.command_id),
            correlation_id: None,
            actor,
            body,
        };
        self.subscriptions.publish(session_id, event.clone());

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_update_queued_prompt(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        prompt_id: PromptId,
        text: Option<String>,
        delivery: Option<PromptDelivery>,
    ) -> Result<CommandOutcome, CodypendentError> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        let updated =
            crate::prompt_queue::update(&mut tx, session_id, prompt_id, text.as_deref(), delivery)
                .await
                .map_err(internal_error)?;

        let prompts = match updated {
            Some(p) => p,
            None => {
                let _ = tx.rollback().await;
                return Err(CodypendentError::new(
                    "prompt-queue.not-found",
                    format!("no queued prompt {prompt_id}"),
                    false,
                ));
            }
        };

        let seq = next_sequence(&mut *tx, session_id)
            .await
            .map_err(internal_error)?;
        let actor = Actor::Client {
            client_id: ctx.client_id,
        };
        let body = EventBody::PendingPromptsChanged { prompts };

        append_event(
            &mut *tx,
            session_id,
            seq,
            &actor,
            &body,
            &now_str,
            Some(command.command_id),
        )
        .await
        .map_err(internal_error)?;

        sqlx::query("UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;

        let outcome = CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence: Some(u64::try_from(seq).map_err(internal_error)?),
            newly_applied: true,
        };

        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome).map_err(internal_error)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        let event = SessionEvent {
            sequence: u64::try_from(seq).map_err(internal_error)?,
            occurred_at: now,
            causation_id: Some(command.command_id),
            correlation_id: None,
            actor,
            body,
        };
        self.subscriptions.publish(session_id, event.clone());

        Ok(outcome)
    }

    async fn apply_promote_queued_prompt(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        prompt_id: PromptId,
    ) -> Result<CommandOutcome, CodypendentError> {
        self.apply_update_queued_prompt(
            pool,
            ctx,
            command,
            session_id,
            prompt_id,
            None,
            Some(PromptDelivery::Steer),
        )
        .await
    }

    async fn apply_delete_queued_prompt(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        session_id: SessionId,
        prompt_id: PromptId,
    ) -> Result<CommandOutcome, CodypendentError> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        let deleted = crate::prompt_queue::delete(&mut tx, session_id, prompt_id)
            .await
            .map_err(internal_error)?;

        let prompts = match deleted {
            Some(p) => p,
            None => {
                let _ = tx.rollback().await;
                return Err(CodypendentError::new(
                    "prompt-queue.not-found",
                    format!("no queued prompt {prompt_id}"),
                    false,
                ));
            }
        };

        let seq = next_sequence(&mut *tx, session_id)
            .await
            .map_err(internal_error)?;
        let actor = Actor::Client {
            client_id: ctx.client_id,
        };
        let body = EventBody::PendingPromptsChanged { prompts };

        append_event(
            &mut *tx,
            session_id,
            seq,
            &actor,
            &body,
            &now_str,
            Some(command.command_id),
        )
        .await
        .map_err(internal_error)?;

        sqlx::query("UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;

        let outcome = CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence: Some(u64::try_from(seq).map_err(internal_error)?),
            newly_applied: true,
        };

        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome).map_err(internal_error)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        let event = SessionEvent {
            sequence: u64::try_from(seq).map_err(internal_error)?,
            occurred_at: now,
            causation_id: Some(command.command_id),
            correlation_id: None,
            actor,
            body,
        };
        self.subscriptions.publish(session_id, event.clone());

        Ok(outcome)
    }

    async fn apply_run_state(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        run_id: RunId,
        state: RunState,
    ) -> Result<CommandOutcome, CodypendentError> {
        let session_id = projections::run_session(pool, run_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| run_not_found(run_id))?;
        let events = vec![(
            Actor::Client {
                client_id: ctx.client_id,
            },
            EventBody::RunStateChanged { run_id, state },
        )];
        self.run_transaction(
            pool,
            ctx,
            command,
            Some(session_id),
            session_id,
            PreInsert::None,
            events,
            ProjectionOp::SetRunState { run_id, state },
            (None, None),
            RevisionOp::Bump {
                expected: command.expected_revision,
            },
        )
        .await
    }

    /// `ResolveApproval` is the one Phase 1 command with an external effect (flip
    /// the approval row + append `ApprovalResolved` + wake the parked runtime
    /// waiter). ONE transaction holds the whole command: the `received` command
    /// row, the `expected_revision` guard, the broker's flip + append (via
    /// [`ApprovalBroker::resolve_in_tx`]), the session-revision bump, and the flip
    /// to `applied`. Holding the guard *and* the bump in the same transaction as
    /// the append is what makes two commands sharing one `expected_revision`
    /// mutually exclusive (issue #6 item 2b, previously three separate txs). After
    /// commit we publish *exactly* the appended event (never the session tail,
    /// which a concurrent append may have changed — item 2a) and wake the waiter.
    async fn apply_resolve_approval(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        approval_id: codypendent_protocol::ApprovalId,
        decision: ApprovalDecision,
        scope: ApprovalScope,
    ) -> Result<CommandOutcome, CodypendentError> {
        let session_id = approval_session(pool, approval_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| approval_not_found(approval_id))?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        // 1. Command row (received). A concurrent duplicate that loses this insert
        //    replays the recorded outcome instead of erroring.
        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        // 2. Optimistic-concurrency guard, read under the write lock so no
        //    concurrent ResolveApproval can slip between this check and the bump.
        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        // 3. The external effect, INSIDE this tx: flip the approval and append
        //    `ApprovalResolved`, getting back that exact event to publish.
        let event = match self
            .approvals
            .resolve_in_tx(
                &mut tx,
                approval_id,
                decision,
                scope,
                // `approvals.resolved_by` and the `Actor::Human` on the appended
                // `ApprovalResolved` both come from here. It used to be the
                // client's own UUID, which made the audit trail a record of what
                // the caller typed; it is now the peer uid the kernel reported.
                ctx.principal.user_id().0,
                now,
            )
            .await
        {
            Ok(event) => {
                // 4. Bump the session revision, atomic with the append it reflects.
                sqlx::query(
                    "UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?",
                )
                .bind(&now_str)
                .bind(session_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(internal_error)?;
                Some(event)
            }
            // Already resolved (a prior delivery, another resolver, or an expiry):
            // a successful no-op — the decision is already on the ledger. Record
            // the command `applied` with no new event and no bump, matching the
            // resume-replay path so first delivery and replay agree.
            Err(ApprovalError::AlreadyResolved { .. }) => None,
            Err(err @ ApprovalError::NotFound { .. }) => {
                let _ = tx.rollback().await;
                return Err(map_approval_error(err));
            }
            Err(err) => {
                let _ = tx.rollback().await;
                return Err(map_approval_error(err));
            }
        };

        // 5. Compute the outcome and flip the command to `applied`, still in the tx.
        let last_sequence = match &event {
            Some(event) => Some(event.sequence),
            None => tx_max_sequence(&mut *tx, session_id)
                .await
                .map_err(internal_error)?,
        };
        let outcome = CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence,
            newly_applied: false,
        };
        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome).map_err(internal_error)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        // 6. Post-commit (persist before publish): wake the parked runtime waiter
        //    and publish exactly the appended event.
        if let Some(event) = event {
            self.approvals.wake(approval_id, decision).await;
            self.subscriptions.publish(session_id, event);
        }

        Ok(outcome)
    }

    /// Resolve a parked question (adoption 03). Mirrors `apply_resolve_approval`:
    /// session-scoped, idempotent, revision-guarded. Inside the command's own
    /// transaction we flip the row to answered/rejected and append `QuestionResolved`,
    /// bump the revision, mark `commands` applied, and commit; only after
    /// commit do we publish the event and wake the waiter.
    async fn apply_resolve_question(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        question_id: codypendent_protocol::QuestionId,
        outcome: QuestionOutcome,
    ) -> Result<CommandOutcome, CodypendentError> {
        let session_id = question_session(pool, question_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| question_not_found(question_id))?;

        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let body_json = serde_json::to_string(&command.body).map_err(internal_error)?;

        let mut tx = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(internal_error)?;

        // 1. Command row (received).
        if let Err(err) = sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(session_id.to_string())
        .bind(ctx.client_id.to_string())
        .bind(&body_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            let err = anyhow::Error::from(err);
            if is_unique_violation(&err) {
                if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                    .await
                    .map_err(internal_error)?
                {
                    return self.handle_existing(pool, existing).await;
                }
            }
            return Err(internal_error(err));
        }

        // 2. Optimistic concurrency guard.
        if let Some(expected) = command.expected_revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_error)?;
            let current = u64::try_from(current).map_err(internal_error)?;
            if expected != current {
                let _ = tx.rollback().await;
                return Err(revision_conflict(expected, current));
            }
        }

        // 3. Resolve inside tx.
        let event = match self
            .questions
            .resolve_in_tx(
                &mut tx,
                question_id,
                outcome.clone(),
                ctx.principal.user_id().0,
                now,
            )
            .await
        {
            Ok(event) => {
                // 4. Bump session revision.
                sqlx::query(
                    "UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?",
                )
                .bind(&now_str)
                .bind(session_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(internal_error)?;
                Some(event)
            }
            Err(QuestionError::AlreadyResolved { .. }) => None,
            Err(err @ QuestionError::NotFound { .. }) => {
                let _ = tx.rollback().await;
                return Err(map_question_error(err));
            }
            Err(err) => {
                let _ = tx.rollback().await;
                return Err(map_question_error(err));
            }
        };

        // 5. Outcome and applied update.
        let last_sequence = match &event {
            Some(event) => Some(event.sequence),
            None => tx_max_sequence(&mut *tx, session_id)
                .await
                .map_err(internal_error)?,
        };
        let outcome_dto = CommandOutcome {
            command_id: command.command_id,
            created_session: None,
            created_run: None,
            last_sequence,
            newly_applied: false,
        };
        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome_dto).map_err(internal_error)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;

        tx.commit().await.map_err(internal_error)?;

        // 6. Post-commit wake + publish.
        if let Some(event) = event {
            let reply = match outcome {
                QuestionOutcome::Answered { answers } => QuestionReply::Answered(answers),
                QuestionOutcome::Rejected { feedback } => QuestionReply::Rejected { feedback },
                _ => QuestionReply::Rejected { feedback: None },
            };
            self.questions.wake(question_id, reply).await;
            self.subscriptions.publish(session_id, event);
        }

        Ok(outcome_dto)
    }

    // --- the transaction ------------------------------------------------------

    /// Run steps 3 and 6 for an effect-free command: one transaction that
    /// records the command, appends its events (allocating sequence inside the
    /// tx), updates projections, and commits `applied`; then publishes the
    /// committed events. Infrastructure failures become an `internal` error.
    #[allow(clippy::too_many_arguments)] // the write path threads many typed pieces through one atomic tx.
    async fn run_transaction(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        command_session: Option<SessionId>,
        event_session: SessionId,
        pre: PreInsert<'_>,
        events: Vec<(Actor, EventBody)>,
        projection: ProjectionOp,
        created: (Option<SessionId>, Option<RunId>),
        revision: RevisionOp,
    ) -> Result<CommandOutcome, CodypendentError> {
        let committed = self
            .commit(
                pool,
                ctx,
                command,
                command_session,
                event_session,
                pre,
                events,
                projection,
                created,
                revision,
            )
            .await;

        let (outcome, persisted) = match committed {
            Ok(value) => value,
            Err(err) => {
                // A failed `expected_revision` guard is a structured protocol
                // conflict, not an infrastructure failure — the tx rolled back,
                // so nothing was applied.
                if let Some(conflict) = err.downcast_ref::<RevisionConflict>() {
                    return Err(revision_conflict(conflict.expected, conflict.actual));
                }
                // A run-state transition rejected by the atomic conditional
                // write (FP-3) — likewise a structured protocol rejection, not
                // an infrastructure failure; the tx rolled back.
                if let Some(rejected) = err.downcast_ref::<RunTransitionRejected>() {
                    return Err(rejected.0.clone());
                }
                // A concurrent duplicate delivery won the race to insert the
                // `commands` row (its `UNIQUE(idempotency_key)`/PK tripped). That
                // is not `internal.command-apply-failed`: the winner already
                // recorded the outcome, so replay it via the existing-command
                // path (RULE: duplicate delivery = one effect, one result). We
                // re-run the idempotency lookup and only replay when a row with
                // this key exists, so an unrelated unique violation still errors.
                if is_unique_violation(&err) {
                    if let Some(existing) = lookup_command(pool, &command.idempotency_key)
                        .await
                        .map_err(internal_error)?
                    {
                        return self.handle_existing(pool, existing).await;
                    }
                }
                return Err(internal_error(err));
            }
        };

        // Step 6: publish only after the commit (persist before publish).
        for event in persisted {
            self.subscriptions.publish(event_session, event);
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit(
        &self,
        pool: &SqlitePool,
        ctx: &ApplyContext,
        command: &Command,
        command_session: Option<SessionId>,
        event_session: SessionId,
        pre: PreInsert<'_>,
        events: Vec<(Actor, EventBody)>,
        projection: ProjectionOp,
        created: (Option<SessionId>, Option<RunId>),
        revision: RevisionOp,
    ) -> anyhow::Result<(CommandOutcome, Vec<SessionEvent>)> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

        // Optimistic-concurrency guard + revision advance, atomic (inside this
        // tx) with the append it protects. `Establish` (CreateSession) inserts a
        // fresh session at revision 0 below and ignores `expected_revision`;
        // `Bump` checks the guard against the *live* revision — read under the
        // write lock so no concurrent command can slip between check and bump —
        // and advances it. On a mismatch we abort the whole tx (nothing applied).
        if let RevisionOp::Bump { expected } = revision {
            let (current,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(event_session.to_string())
                .fetch_one(&mut *tx)
                .await?;
            let current = u64::try_from(current)?;
            if let Some(expected) = expected {
                if expected != current {
                    return Err(RevisionConflict {
                        expected,
                        actual: current,
                    }
                    .into());
                }
            }
            sqlx::query("UPDATE sessions SET revision = revision + 1, updated_at = ? WHERE id = ?")
                .bind(&now_str)
                .bind(event_session.to_string())
                .execute(&mut *tx)
                .await?;
        }

        // commands row (received).
        sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?)",
        )
        .bind(command.command_id.to_string())
        .bind(&command.idempotency_key)
        .bind(command_session.map(|s| s.to_string()))
        .bind(ctx.client_id.to_string())
        .bind(serde_json::to_string(&command.body)?)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;

        // Session pre-insert must precede its events (the events FK references
        // sessions(id)).
        if let PreInsert::Session {
            session_id,
            title,
            owner_uid,
        } = pre
        {
            sqlx::query(
                "INSERT INTO sessions \
                 (id, title, state, created_at, updated_at, revision, owner_uid) \
                 VALUES (?, ?, 'open', ?, ?, 0, ?)",
            )
            .bind(session_id.to_string())
            .bind(title)
            .bind(&now_str)
            .bind(&now_str)
            .bind(i64::from(owner_uid))
            .execute(&mut *tx)
            .await?;
        }

        // Append events, allocating each sequence inside this tx.
        let mut persisted = Vec::with_capacity(events.len());
        for (actor, body) in events {
            let sequence = next_sequence(&mut *tx, event_session).await?;
            append_event(
                &mut *tx,
                event_session,
                sequence,
                &actor,
                &body,
                &now_str,
                Some(command.command_id),
            )
            .await?;
            persisted.push(SessionEvent {
                sequence: u64::try_from(sequence)?,
                occurred_at: now,
                causation_id: Some(command.command_id),
                correlation_id: None,
                actor,
                body,
            });
        }

        // Projection rows.
        match projection {
            ProjectionOp::None => {}
            ProjectionOp::InsertRun {
                run_id,
                session_id,
                objective,
                mode,
            } => {
                projections::insert_run(
                    &mut *tx,
                    run_id,
                    session_id,
                    &objective,
                    mode,
                    DEFAULT_MODEL_POLICY,
                    DEFAULT_BUDGET_JSON,
                )
                .await?;
            }
            ProjectionOp::SetRunState { run_id, state } => {
                // Assert the CURRENT state is legal for this transition via a
                // single conditional UPDATE, not a separate read-then-write
                // (FP-3): `validate()`'s pre-transaction read can go stale
                // between two concurrent lifecycle commands on the same run —
                // e.g. a `CancelRun` and a `PauseRun` both reading `Running`
                // and both passing that check — so the write itself
                // re-asserts the prior state and only applies when it still
                // holds. `BEGIN IMMEDIATE` above means whichever of two
                // racing commands reaches this point SECOND sees the FIRST
                // one's already-committed state, so an invalid transition
                // (e.g. a `Cancelled` run flipped back to `Paused`) can never
                // commit.
                let legal_from = legal_prior_states(&command.body, run_id);
                let affected =
                    projections::set_run_state_if_legal(&mut *tx, run_id, &legal_from, state)
                        .await?;
                if affected == 0 {
                    // Not legal from the run's CURRENT state (re-read fresh,
                    // under this transaction's write lock) — either
                    // `validate()`'s earlier read was stale (a concurrent
                    // command committed first) or the run no longer exists.
                    // Reject with the same structured error `validate()` would
                    // have produced; the whole transaction rolls back (nothing
                    // applied).
                    let current = projections::load_run_state(&mut *tx, run_id).await?;
                    let rejection = match current {
                        Some(fresh_state) => validate_run_transition(
                            &command.body,
                            run_id,
                            fresh_state,
                        )
                        .expect_err(
                            "a state excluded from legal_prior_states must fail validate_run_transition",
                        ),
                        None => run_not_found(run_id),
                    };
                    return Err(RunTransitionRejected(rejection).into());
                }
            }
        }

        let outcome = CommandOutcome {
            command_id: command.command_id,
            created_session: created.0,
            created_run: created.1,
            last_sequence: persisted.last().map(|e| e.sequence),
            // `run_transaction` runs only on the FIRST application (the
            // idempotency check returns a replay before reaching here), so this
            // is the one place `newly_applied` is true — the signal the server
            // uses to launch the executor exactly once per created run.
            newly_applied: true,
        };

        // Flip received -> applied with the recorded outcome, still in the tx.
        sqlx::query(
            "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
        )
        .bind(serde_json::to_string(&outcome)?)
        .bind(&now_str)
        .bind(command.command_id.to_string())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok((outcome, persisted))
    }

    // --- pending-effect reconciliation ---------------------------------------

    async fn reconcile_command_effects(
        &self,
        pool: &SqlitePool,
        command_id: &CommandId,
    ) -> anyhow::Result<usize> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, kind, state FROM pending_effects \
             WHERE command_id = ? AND state IN ('intended', 'performed')",
        )
        .bind(command_id.to_string())
        .fetch_all(pool)
        .await?;

        let command_id = command_id.to_string();
        let mut reconciled = 0usize;
        for (id, kind, state) in rows {
            if self
                .reconcile_effect(pool, &id, &command_id, &kind, &state)
                .await?
            {
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    /// Reconcile one pending effect. Phase 1.3 has no verifiable real-world
    /// effects yet (tool effects land in the agent loop, STEP 1.10), so an
    /// `intended` effect that never ran is **abandoned** — re-performing it blind
    /// would risk the very duplicate the crash-consistency contract forbids —
    /// and a `performed` effect awaiting its outcome is **reconciled**. A
    /// reconciliation `NoteAppended` records the decision on the session ledger.
    /// STEP 1.14 replaces the heuristic with real reality-checks. Returns whether
    /// this call changed the row (false if another sweep won the race).
    async fn reconcile_effect(
        &self,
        pool: &SqlitePool,
        id: &str,
        command_id: &str,
        kind: &str,
        state: &str,
    ) -> anyhow::Result<bool> {
        let new_state = if state == "performed" {
            "reconciled"
        } else {
            "abandoned"
        };

        let session: Option<(Option<String>,)> =
            sqlx::query_as("SELECT session_id FROM commands WHERE id = ?")
                .bind(command_id)
                .fetch_optional(pool)
                .await?;
        let session_id = session
            .and_then(|(s,)| s)
            .and_then(|s| SessionId::from_str(&s).ok());

        let now = Utc::now().to_rfc3339();
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query(
            "UPDATE pending_effects SET state = ?, resolved_at = ? WHERE id = ? AND state = ?",
        )
        .bind(new_state)
        .bind(&now)
        .bind(id)
        .bind(state)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            // Raced with another reconciler; leave its work intact.
            tx.rollback().await?;
            return Ok(false);
        }

        if let Some(session_id) = session_id {
            let sequence = next_sequence(&mut *tx, session_id).await?;
            append_event(
                &mut *tx,
                session_id,
                sequence,
                &Actor::System,
                &EventBody::NoteAppended {
                    text: format!("pending-effect {id} ({kind}) reconciled as {new_state}"),
                    run_id: None,
                },
                &now,
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

// --- free helpers ------------------------------------------------------------

/// Whether `role` may issue `body`. `Observer` may issue nothing (read-only);
/// `Contributor` may create/start/steer/submit; `Controller` additionally
/// controls runs and (as the most privileged role) resolves approvals;
/// `Approver` resolves approvals plus the contributor set. `AttachSession` and
/// `Unknown` are rejected before this check.
fn role_permits(role: ClientRole, body: &CommandBody) -> bool {
    use ClientRole::{Approver, Contributor, Controller};
    match body {
        CommandBody::CreateSession { .. }
        | CommandBody::StartRun { .. }
        | CommandBody::SubmitUserInput { .. }
        | CommandBody::QueueSteering { .. }
        | CommandBody::QueuePrompt { .. }
        | CommandBody::UpdateQueuedPrompt { .. }
        | CommandBody::PromoteQueuedPrompt { .. }
        | CommandBody::DeleteQueuedPrompt { .. } => {
            matches!(role, Contributor | Controller | Approver)
        }
        CommandBody::CancelRun { .. }
        | CommandBody::PauseRun { .. }
        | CommandBody::ResumeRun { .. } => matches!(role, Controller),
        CommandBody::ResolveApproval { .. } => matches!(role, Approver | Controller),
        CommandBody::ResolveQuestion { .. } => {
            matches!(role, Contributor | Controller | Approver)
        }
        _ => false,
    }
}

/// The `internal.command-apply-failed` error every infrastructure (DB/serde)
/// failure collapses to — retryable, since a transient DB error may clear.
fn internal_error(err: impl std::fmt::Display) -> CodypendentError {
    CodypendentError::new("internal.command-apply-failed", err.to_string(), true)
}

fn run_not_found(run_id: RunId) -> CodypendentError {
    CodypendentError::new("protocol.run-not-found", format!("no run {run_id}"), false)
}

/// Whether a lifecycle command is legal from the run's current state.
///
/// Without this guard `ResumeRun` on a `Completed` run flipped the projection
/// back to `Running` with no executor attached — a zombie polluting
/// `active_runs` until the next boot's recovery force-failed it and appended
/// contradictory terminal events onto an already-finished run.
fn validate_run_transition(
    body: &CommandBody,
    run_id: RunId,
    state: RunState,
) -> Result<(), CodypendentError> {
    let terminal = matches!(
        state,
        RunState::Completed | RunState::Failed | RunState::Cancelled
    );
    let (verb, legal) = match body {
        // Cancelling is legal from any live state.
        CommandBody::CancelRun { .. } => ("cancel", !terminal && state != RunState::Unknown),
        // Pausing is legal from any live, not-already-paused state.
        CommandBody::PauseRun { .. } => (
            "pause",
            !terminal && !matches!(state, RunState::Paused | RunState::Unknown),
        ),
        // Resuming means "leave Paused" — anything else is already live or done.
        CommandBody::ResumeRun { .. } => ("resume", state == RunState::Paused),
        _ => ("transition", true),
    };
    if legal {
        Ok(())
    } else {
        Err(CodypendentError::new(
            "run.invalid-transition",
            format!("cannot {verb} run {run_id} in state {state:?}"),
            false,
        ))
    }
}

/// The [`RunState`]s from which `body`'s transition is legal, as a set the
/// write path can assert atomically via
/// [`projections::set_run_state_if_legal`] (FP-3). Derived directly from
/// [`validate_run_transition`] (evaluated against every known state) rather
/// than duplicating its rule as a second, hand-maintained list — so the two
/// can never drift apart.
fn legal_prior_states(body: &CommandBody, run_id: RunId) -> Vec<RunState> {
    const ALL_STATES: [RunState; 10] = [
        RunState::Queued,
        RunState::Preparing,
        RunState::Running,
        RunState::WaitingForApproval,
        RunState::WaitingForUserInput,
        RunState::Paused,
        RunState::Recovering,
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
    ];
    ALL_STATES
        .into_iter()
        .filter(|&state| validate_run_transition(body, run_id, state).is_ok())
        .collect()
}

fn approval_not_found(approval_id: codypendent_protocol::ApprovalId) -> CodypendentError {
    CodypendentError::new(
        "approval.not-found",
        format!("no approval {approval_id}"),
        false,
    )
}

/// The structured `protocol.revision-conflict` returned when a command's
/// `expected_revision` guard does not match the session's live revision. Not
/// retryable (an identical retry would carry the same stale revision).
fn revision_conflict(expected: u64, actual: u64) -> CodypendentError {
    CodypendentError::new(
        "protocol.revision-conflict",
        format!("expected session revision {expected} but it is at {actual}"),
        false,
    )
}

/// Whether `err` wraps a SQLite UNIQUE / PRIMARY KEY constraint violation — the
/// signal that a concurrent delivery of the same command won the race to insert
/// the `commands` row. Detected via the typed `sqlx` database error (not string
/// matching), so unrelated infrastructure failures are never mistaken for a
/// duplicate.
fn is_unique_violation(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::Database(db)) if db.is_unique_violation()
    )
}

/// A failed `expected_revision` guard, carried out of the write transaction as a
/// downcastable error so the caller can surface it as `protocol.revision-conflict`
/// (distinct from an infrastructure failure).
#[derive(Debug)]
struct RevisionConflict {
    expected: u64,
    actual: u64,
}

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "expected session revision {} but it is at {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for RevisionConflict {}

/// A run-state lifecycle transition that failed re-validation *inside* the
/// write transaction (FP-3) — carried out of [`commit`](CommandProcessor::commit)
/// as a downcastable error, exactly like [`RevisionConflict`], so
/// [`run_transaction`](CommandProcessor::run_transaction) can surface the SAME
/// structured [`CodypendentError`] `validate_run_transition`/`run_not_found`
/// would have produced, rather than a generic internal error.
#[derive(Debug)]
struct RunTransitionRejected(CodypendentError);

impl std::fmt::Display for RunTransitionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.message)
    }
}

impl std::error::Error for RunTransitionRejected {}

/// The highest event sequence for a session, read inside the caller's tx (so it
/// reflects appends made earlier in the same transaction). `None` for a session
/// with no events yet. Used by the `ResolveApproval` no-op (already-resolved)
/// path to report a sensible `last_sequence`.
async fn tx_max_sequence(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
) -> anyhow::Result<Option<u64>> {
    let (max,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(exec)
            .await?;
    Ok(if max > 0 {
        Some(u64::try_from(max)?)
    } else {
        None
    })
}

/// Restated rejection for the `AttachSession`/`Unknown` arms of `apply` (already
/// rejected in `validate`; this keeps the dispatch match total).
fn rejected_for_body(body: &CommandBody) -> CodypendentError {
    match body {
        CommandBody::AttachSession { .. } => CodypendentError::new(
            "protocol.attach-is-connection-level",
            "AttachSession is handled by the connection layer, not the command write path",
            false,
        ),
        _ => CodypendentError::new("protocol.unsupported-payload", "unsupported command", false),
    }
}

fn map_approval_error(err: ApprovalError) -> CodypendentError {
    match err {
        ApprovalError::NotFound { .. } => {
            CodypendentError::new("approval.not-found", err.to_string(), false)
        }
        ApprovalError::AlreadyResolved { .. } => {
            CodypendentError::new("approval.already-resolved", err.to_string(), false)
        }
        ApprovalError::UnsupportedDecision | ApprovalError::UnsupportedScope => {
            CodypendentError::new("protocol.unsupported-payload", err.to_string(), false)
        }
        ApprovalError::PatternUnavailable => {
            CodypendentError::new("approval.pattern-unavailable", err.to_string(), false)
        }
        other => internal_error(other),
    }
}

fn map_question_error(err: QuestionError) -> CodypendentError {
    match err {
        QuestionError::NotFound { .. } => {
            CodypendentError::new("question.not-found", err.to_string(), false)
        }
        QuestionError::AlreadyResolved { .. } => {
            CodypendentError::new("question.already-resolved", err.to_string(), false)
        }
        QuestionError::UnsupportedOutcome => {
            CodypendentError::new("protocol.unsupported-payload", err.to_string(), false)
        }
        other => internal_error(other),
    }
}

async fn question_session(
    pool: &SqlitePool,
    question_id: QuestionId,
) -> anyhow::Result<Option<SessionId>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT r.session_id FROM questions q JOIN runs r ON q.run_id = r.id WHERE q.id = ?",
    )
    .bind(question_id.to_string())
    .fetch_optional(pool)
    .await?;
    match row {
        Some((id_str,)) => Ok(Some(SessionId::from_str(&id_str)?)),
        None => Ok(None),
    }
}

fn question_not_found(question_id: QuestionId) -> CodypendentError {
    CodypendentError::new(
        "question.not-found",
        format!("no question with id {question_id}"),
        false,
    )
}

/// The columns of a recorded command that idempotency handling needs.
struct ExistingCommand {
    command_id: CommandId,
    status: String,
    result_json: Option<String>,
    body: String,
    session_id: Option<SessionId>,
    client_id: String,
}

/// Raw row shape of [`lookup_command`]:
/// (id, status, result_json, body, session_id, client_id).
type CommandRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
);

async fn lookup_command(
    pool: &SqlitePool,
    idempotency_key: &str,
) -> anyhow::Result<Option<ExistingCommand>> {
    let row: Option<CommandRow> = sqlx::query_as(
        "SELECT id, status, result_json, body, session_id, client_id \
             FROM commands WHERE idempotency_key = ?",
    )
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(None),
        Some((id, status, result_json, body, session_id, client_id)) => Ok(Some(ExistingCommand {
            command_id: CommandId::from_str(&id)?,
            status,
            result_json,
            body,
            session_id: session_id.map(|s| SessionId::from_str(&s)).transpose()?,
            client_id,
        })),
    }
}

async fn finalize_applied(
    pool: &SqlitePool,
    command_id: CommandId,
    outcome: &CommandOutcome,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE commands SET status = 'applied', result_json = ?, applied_at = ? WHERE id = ?",
    )
    .bind(serde_json::to_string(outcome)?)
    .bind(Utc::now().to_rfc3339())
    .bind(command_id.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn session_exists(pool: &SqlitePool, session_id: SessionId) -> anyhow::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// The `owner_uid` of a session (migration 0031), for a fork re-driven on
/// recovery to inherit its source's owner. `None` when the session is missing or
/// its row predates the column.
async fn session_owner_uid(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<Option<u32>> {
    let row: Option<(Option<i64>,)> = sqlx::query_as("SELECT owner_uid FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .fetch_optional(pool)
        .await?;
    Ok(row
        .and_then(|(uid,)| uid)
        .and_then(|uid| u32::try_from(uid).ok()))
}

/// The repository and pinned model a session's runs inherit, recovered from the
/// session's originating [`StartRun`](CommandBody::StartRun) command
/// (continuous-session I-1/I-2).
///
/// Neither value is projected onto the `runs` row: the row carries no repository
/// column at all, and only a *default* `model_policy` ([`DEFAULT_MODEL_POLICY`],
/// supplied by the write path) — never the operator's pin. The authoritative
/// source is the persisted `StartRun` command body itself, which the idempotent
/// write path stores verbatim, so the operator's `repository`/`model` survive on
/// it. A follow-up ([`SubmitUserInput`](CommandBody::SubmitUserInput)) carries
/// neither on the wire, so a continuation reads them from here to run against the
/// SAME checkout and pinned model as the session's first run — instead of the
/// daemon's (possibly startup-frozen) `current_dir()` and an unpinned default.
#[derive(Debug, Default, Clone)]
pub(crate) struct SessionRunProvenance {
    /// The repository root the session's originating run was launched against
    /// (`StartRun.repository`); `None` when that run carried none (an older
    /// client) or the session has no applied `StartRun`.
    pub repository: Option<String>,
    /// The model the session's originating run pinned (`StartRun.model`); `None`
    /// when unpinned or the session has no applied `StartRun`.
    pub model: Option<ModelId>,
}

/// Recover a session's [`SessionRunProvenance`] from its applied command ledger.
///
/// **Model** (the pin): the most recent command that carried one — a `StartRun`
/// *or* a mid-conversation `SubmitUserInput` re-pin — wins, scanning newest-first.
/// So a session re-pinned mid-conversation inherits its LATEST pin (the switch
/// sticks for the next follow-up), while a follow-up that carried none never
/// clobbers the session's current model. A `SubmitUserInput` with no pin
/// (`model: None`) is transparent to this scan.
///
/// **Repository** (stable across a session): only a `StartRun` carries one, so
/// this takes the most recent `StartRun`'s `repository` — unchanged from before
/// this function also considered continuations for the model.
///
/// A session with no applied `StartRun`/pin yields the default (both `None`), so
/// the caller falls back exactly as an older client's continuation did.
///
/// The `body LIKE` clause only *bounds* the rows scanned — the command body is
/// compact JSON, internally tagged `"type":"StartRun"` / `"type":"SubmitUserInput"`
/// — while the deserialize-and-match below is the authoritative extractor.
pub(crate) async fn session_run_provenance(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<SessionRunProvenance> {
    let mut provenance = SessionRunProvenance::default();
    let mut model_found = false;
    let mut repository_found = false;
    session_run_provenance_inner(
        pool,
        session_id,
        0,
        &mut provenance,
        &mut model_found,
        &mut repository_found,
    )
    .await?;
    Ok(provenance)
}

async fn session_run_provenance_inner(
    pool: &SqlitePool,
    session_id: SessionId,
    depth: usize,
    provenance: &mut SessionRunProvenance,
    model_found: &mut bool,
    repository_found: &mut bool,
) -> anyhow::Result<()> {
    if depth > 8 {
        return Ok(());
    }
    let bodies: Vec<(String,)> = sqlx::query_as(
        "SELECT body FROM commands \
         WHERE session_id = ? AND status = 'applied' \
           AND (body LIKE '%\"type\":\"StartRun\"%' \
                OR body LIKE '%\"type\":\"SubmitUserInput\"%') \
         ORDER BY received_at DESC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await?;
    for (body,) in bodies {
        match serde_json::from_str::<CommandBody>(&body) {
            // A `StartRun` is authoritative for the repository, and (like any
            // pinned command) supplies the model when no newer pin was found.
            Ok(CommandBody::StartRun {
                repository, model, ..
            }) => {
                if !*model_found {
                    provenance.model = model;
                    *model_found = true;
                }
                if !*repository_found {
                    provenance.repository = repository;
                    *repository_found = true;
                }
            }
            // A mid-conversation re-pin carries no repository. It only overrides
            // the model, and only when it actually pinned one — a `None`
            // follow-up (matched by the catch-all below) is transparent and must
            // NOT clobber the session's model.
            Ok(CommandBody::SubmitUserInput {
                model: Some(model), ..
            }) if !*model_found => {
                provenance.model = Some(model);
                *model_found = true;
            }
            _ => {}
        }
        if *model_found && *repository_found {
            return Ok(());
        }
    }
    if !*model_found || !*repository_found {
        let parent_row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT forked_from_session_id FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(pool)
                .await?;

        if let Some((Some(parent_id_str),)) = parent_row {
            if let Ok(parent_id) = SessionId::from_str(&parent_id_str) {
                Box::pin(session_run_provenance_inner(
                    pool,
                    parent_id,
                    depth + 1,
                    provenance,
                    model_found,
                    repository_found,
                ))
                .await?;
            }
        }
    }
    Ok(())
}

async fn approval_session(
    pool: &SqlitePool,
    approval_id: codypendent_protocol::ApprovalId,
) -> anyhow::Result<Option<SessionId>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT r.session_id FROM approvals a JOIN runs r ON a.run_id = r.id WHERE a.id = ?",
    )
    .bind(approval_id.to_string())
    .fetch_optional(pool)
    .await?;
    row.map(|(s,)| SessionId::from_str(&s))
        .transpose()
        .map_err(Into::into)
}

async fn max_sequence(pool: &SqlitePool, session_id: SessionId) -> anyhow::Result<Option<u64>> {
    let (max,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(pool)
            .await?;
    Ok(if max > 0 {
        Some(u64::try_from(max)?)
    } else {
        None
    })
}

/// The next 1-based event sequence for a session, read inside the caller's tx so
/// Atomically allocate the next event sequence for `session_id`. `BEGIN
/// IMMEDIATE` is required on the enclosing transaction so no other writer
/// interleaves between this SELECT and the append that claims it.
pub(crate) async fn next_sequence(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
) -> Result<i64, sqlx::Error> {
    let (max,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(exec)
            .await?;
    Ok(max + 1)
}

/// Append one event within the caller's transaction, stamping `causation_id`
/// with the command that produced it (unlike the approval broker's helper, which
/// leaves causation null for its own housekeeping events).
pub(crate) async fn append_event(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
    sequence: i64,
    actor: &Actor,
    body: &EventBody,
    occurred_at: &str,
    causation_id: Option<CommandId>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         VALUES (?, ?, ?, ?, ?, ?, NULL, 1)",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .bind(occurred_at)
    .bind(serde_json::to_string(actor)?)
    .bind(serde_json::to_string(body)?)
    .bind(causation_id.map(|id| id.to_string()))
    .execute(exec)
    .await?;
    Ok(())
}

/// An optional row to insert before a command's events (only `CreateSession`
/// needs one, for the events FK).
enum PreInsert<'a> {
    None,
    Session {
        session_id: SessionId,
        title: &'a str,
        /// The creating principal's uid, recorded so every later by-id read and
        /// every subscription can re-derive permission from what the *server*
        /// stored rather than from what the request claims (outcome 19).
        owner_uid: u32,
    },
}

/// How a command's write transaction handles `sessions.revision` (STEP 1.3
/// optimistic concurrency).
enum RevisionOp {
    /// The command creates the session now (`CreateSession`): it is inserted at
    /// revision 0 and `expected_revision` is ignored (no prior session to guard).
    Establish,
    /// The command mutates an existing session's state: check `expected` (when
    /// `Some`) against the live revision inside the tx, then advance it by one.
    Bump { expected: Option<u64> },
}

/// The projection mutation a command performs inside its transaction.
enum ProjectionOp {
    None,
    InsertRun {
        run_id: RunId,
        session_id: SessionId,
        objective: String,
        mode: AgentMode,
    },
    SetRunState {
        run_id: RunId,
        state: RunState,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{AgentMode, ApprovalDecision, ApprovalScope};
    use std::path::Path;
    use tempfile::tempdir;

    async fn test_pool(dir: &Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    /// The unit tests here drive the write path directly, without a socket, so
    /// they name a principal explicitly. Ownership *enforcement* lives at the
    /// wire boundary (`server::authorize_command`) and is covered by
    /// `tests/multi_user_it.rs` against a real daemon.
    const TEST_UID: u32 = 4242;

    fn ctx(role: ClientRole) -> ApplyContext {
        ApplyContext {
            client_id: ClientId::new(),
            role,
            principal: PeerPrincipal::from_uid(TEST_UID),
        }
    }

    fn command(body: CommandBody, key: &str) -> Command {
        Command {
            command_id: CommandId::new(),
            idempotency_key: key.to_string(),
            expected_revision: None,
            body,
        }
    }

    async fn create_session(
        processor: &CommandProcessor,
        pool: &SqlitePool,
        key: &str,
    ) -> SessionId {
        let outcome = processor
            .apply(
                pool,
                ctx(ClientRole::Contributor),
                command(
                    CommandBody::CreateSession {
                        workspace: codypendent_protocol::WorkspaceId::new(),
                        title: "diagnose the failing test".to_string(),
                        repository: None,
                    },
                    key,
                ),
            )
            .await
            .expect("create session");
        outcome.created_session.expect("session id in outcome")
    }

    async fn run_count(pool: &SqlitePool, session_id: SessionId) -> i64 {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM runs WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(pool)
            .await
            .expect("count runs");
        count
    }

    #[tokio::test]
    async fn duplicate_command_is_idempotent() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "idem-create").await;

        let start = command(
            CommandBody::StartRun {
                session_id: session,
                objective: "fix it".to_string(),
                mode: AgentMode::Build,
                repository: None,
                model: None,
            },
            "idem-start",
        );

        // The SAME envelope, delivered twice.
        let first = processor
            .apply(&pool, ctx(ClientRole::Contributor), start.clone())
            .await
            .expect("first apply");
        let second = processor
            .apply(&pool, ctx(ClientRole::Contributor), start.clone())
            .await
            .expect("second apply");

        // The first delivery freshly applies; the duplicate replays. That
        // distinction (never sent to the client) is what makes the server launch
        // the executor exactly once, while the user-facing outcome is identical.
        assert!(first.newly_applied, "first delivery is a fresh application");
        assert!(!second.newly_applied, "duplicate delivery is a replay");
        assert_eq!(
            CommandOutcome {
                newly_applied: false,
                ..first.clone()
            },
            second,
            "idempotent replay returns the same (user-facing) outcome"
        );
        assert_eq!(run_count(&pool, session).await, 1, "exactly one run row");
        assert!(first.created_run.is_some());
    }

    #[tokio::test]
    async fn create_session_then_start_run_projects() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();

        let session = create_session(&processor, &pool, "create").await;

        // Subscribe before the run's events are published.
        let mut rx = processor.subscriptions().subscribe(session);

        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .expect("start run")
            .created_run
            .expect("run id");

        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "focus on the parser".to_string(),
                        mode: AgentMode::Build,
                        model: None,
                        envelope: None,
                    },
                    "input",
                ),
            )
            .await
            .expect("submit input");

        // Projection: the run row exists in Queued, and the snapshot lists it
        // as active.
        assert_eq!(
            projections::load_run_state(&pool, run).await.unwrap(),
            Some(RunState::Queued),
        );
        let projection = projections::session_projection(&pool, session)
            .await
            .unwrap();
        assert!(projection.active_runs.contains(&run));
        assert_eq!(projection.title, "diagnose the failing test");

        // Published events arrive in order: the StartRun's RunStarted, then the
        // SubmitUserInput's RunStarted (a follow-up now launches its OWN run —
        // continuous-session plan, Task 3 — rather than recording a bare note).
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert!(matches!(first.body, EventBody::RunStarted { .. }));
        assert!(matches!(second.body, EventBody::RunStarted { .. }));
        assert!(first.sequence < second.sequence);
    }

    #[tokio::test]
    async fn session_run_provenance_recovers_repository_and_pinned_model() {
        // I-1/I-2: a session's repository and pinned model live only on its
        // originating `StartRun` command body (the `runs` row carries neither),
        // and must be recoverable so a later continuation inherits them.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "prov-create").await;

        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: Some("/work/some-checkout".to_string()),
                        model: Some(ModelId("pinned-model-x".to_string())),
                    },
                    "prov-start",
                ),
            )
            .await
            .expect("start run");

        // A follow-up carries neither repository nor model, and must NOT clobber
        // the session's recoverable provenance.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "keep going".to_string(),
                        mode: AgentMode::Build,
                        model: None,
                        envelope: None,
                    },
                    "prov-input",
                ),
            )
            .await
            .expect("submit input");

        let provenance = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(
            provenance.repository.as_deref(),
            Some("/work/some-checkout"),
            "the continuation must inherit the session's StartRun repository"
        );
        assert_eq!(
            provenance.model,
            Some(ModelId("pinned-model-x".to_string())),
            "the continuation must inherit the session's pinned model"
        );
    }

    #[tokio::test]
    async fn session_run_provenance_follows_a_mid_conversation_repin() {
        // The instant, same-session model switch: a `SubmitUserInput` that
        // carries a pin RE-pins the session, so the next follow-up inherits the
        // LATEST pick (the switch sticks), a further re-pin updates it again, and
        // an unpinned follow-up never clobbers the current model. The repository
        // stays the session's originating one throughout.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "repin-create").await;

        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: Some("/work/some-checkout".to_string()),
                        model: Some(ModelId("model-x".to_string())),
                    },
                    "repin-start",
                ),
            )
            .await
            .expect("start run");

        // First re-pick mid-conversation: switch to model-y.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "switch to Y".to_string(),
                        mode: AgentMode::Build,
                        model: Some(ModelId("model-y".to_string())),
                        envelope: None,
                    },
                    "repin-y",
                ),
            )
            .await
            .expect("submit input Y");

        let after_y = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(
            after_y.model,
            Some(ModelId("model-y".to_string())),
            "a mid-conversation re-pin must stick for the next follow-up"
        );
        assert_eq!(
            after_y.repository.as_deref(),
            Some("/work/some-checkout"),
            "the repository stays the session's originating one"
        );

        // Second re-pick: switch again to model-z; the latest pick wins.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "now Z".to_string(),
                        mode: AgentMode::Build,
                        model: Some(ModelId("model-z".to_string())),
                        envelope: None,
                    },
                    "repin-z",
                ),
            )
            .await
            .expect("submit input Z");

        let after_z = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(
            after_z.model,
            Some(ModelId("model-z".to_string())),
            "a second re-pin must update the session's current model"
        );

        // An unpinned follow-up must NOT clobber the current pin (inherits Z).
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "keep going".to_string(),
                        mode: AgentMode::Build,
                        model: None,
                        envelope: None,
                    },
                    "repin-none",
                ),
            )
            .await
            .expect("submit input none");

        let after_none = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(
            after_none.model,
            Some(ModelId("model-z".to_string())),
            "an unpinned follow-up inherits the session's current model, never clobbers it"
        );
    }

    #[tokio::test]
    async fn session_run_provenance_repin_switches_a_session_started_unpinned() {
        // The user-reported bug, at the daemon level: a session STARTED on the
        // default (unpinned) model, then the operator re-picks a model
        // mid-conversation. The re-pick must take effect for the next follow-up
        // instead of the session forever using the model it started with.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "unpinned-create").await;

        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "unpinned-start",
                ),
            )
            .await
            .expect("start run");

        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "use the big model".to_string(),
                        mode: AgentMode::Build,
                        model: Some(ModelId("just-picked".to_string())),
                        envelope: None,
                    },
                    "unpinned-repin",
                ),
            )
            .await
            .expect("submit input");

        let provenance = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(
            provenance.model,
            Some(ModelId("just-picked".to_string())),
            "re-picking a model mid-conversation must switch the session, not be dropped"
        );
    }

    #[tokio::test]
    async fn session_run_provenance_defaults_without_a_start_run() {
        // A session with no applied `StartRun` (e.g. an unpinned older client
        // that sent no repository) yields the default, so a continuation falls
        // back exactly as before rather than misattributing a repository/model.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "prov-empty-create").await;

        // An unpinned StartRun (no repository, no model) leaves nothing to
        // inherit — the recovered provenance is empty on both fields.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "prov-empty-start",
                ),
            )
            .await
            .expect("start run");

        let provenance = session_run_provenance(&pool, session)
            .await
            .expect("recover provenance");
        assert_eq!(provenance.repository, None);
        assert_eq!(provenance.model, None);
    }

    #[tokio::test]
    async fn observer_cannot_start_run() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "create").await;

        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Observer),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .expect_err("observer must be denied");

        assert_eq!(err.code, "protocol.role-denied");
        assert_eq!(run_count(&pool, session).await, 0, "no run row created");
    }

    #[tokio::test]
    async fn crash_between_persist_and_effect() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "create").await;

        // Simulate a crash mid-apply: a command left `received` with an
        // `intended` pending effect that never ran.
        let command_id = CommandId::new();
        sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, received_at) \
             VALUES (?, 'crashed', ?, 'client', '{\"type\":\"SubmitUserInput\"}', 'received', ?)",
        )
        .bind(command_id.to_string())
        .bind(session.to_string())
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let effect_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pending_effects (id, command_id, kind, intent_json, state, created_at) \
             VALUES (?, ?, 'shell', '{}', 'intended', ?)",
        )
        .bind(&effect_id)
        .bind(command_id.to_string())
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let reconciled = processor.reconcile_pending_effects(&pool).await.unwrap();
        assert_eq!(reconciled, 1);

        // The effect ended reconciled/abandoned — exactly once, no duplicate.
        let (state,): (String,) = sqlx::query_as("SELECT state FROM pending_effects WHERE id = ?")
            .bind(&effect_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            state == "abandoned" || state == "reconciled",
            "unexpected state {state}"
        );
        let (effect_rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_effects")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(effect_rows, 1, "no second effect row appeared");
    }

    #[tokio::test]
    async fn lifecycle_commands_validate_run_state() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();

        let session = create_session(&processor, &pool, "create").await;
        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();

        // Resuming a run that is not paused is refused.
        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::ResumeRun { run_id: run }, "resume-live"),
            )
            .await
            .expect_err("resuming a non-paused run must be rejected");
        assert_eq!(err.code, "run.invalid-transition");

        // Cancel is legal from a live state…
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::CancelRun { run_id: run }, "cancel-1"),
            )
            .await
            .unwrap();

        // …but resuming (or re-cancelling) a terminal run must be refused: the
        // old behavior flipped a Completed/Cancelled run back to `Running` with
        // no executor attached — a zombie in `active_runs` until next boot.
        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::ResumeRun { run_id: run }, "resume-done"),
            )
            .await
            .expect_err("resuming a cancelled run must be rejected");
        assert_eq!(err.code, "run.invalid-transition");
        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::CancelRun { run_id: run }, "cancel-2"),
            )
            .await
            .expect_err("cancelling an already-cancelled run must be rejected");
        assert_eq!(err.code, "run.invalid-transition");
    }

    #[tokio::test]
    async fn set_run_state_if_legal_refuses_a_transition_past_a_stale_prior_state() {
        // FP-3, a direct store-level pin of the atomic conditional write: the
        // guard must refuse to apply a transition once the run's CURRENT state
        // is no longer in the legal set — even though, at some earlier moment
        // (mirroring a stale pre-transaction `validate()` read), it would have
        // been legal. This is the primitive that closes the check-then-act
        // race without needing to orchestrate real concurrency.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "create").await;
        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();

        // The run is `Queued` (StartRun's initial projection) — both Cancel and
        // Pause are legal from Queued.
        let cancel_legal = legal_prior_states(&CommandBody::CancelRun { run_id: run }, run);
        let pause_legal = legal_prior_states(&CommandBody::PauseRun { run_id: run }, run);

        // The first write (the WINNING racer) succeeds and lands the run in a
        // state (`Cancelled`) from which Pause is no longer legal.
        let affected =
            projections::set_run_state_if_legal(&pool, run, &cancel_legal, RunState::Cancelled)
                .await
                .unwrap();
        assert_eq!(affected, 1, "the first (winning) transition applies");

        // The second write (the LOSING racer, whose `legal_from` was computed
        // against the STALE `Queued` read) must now be refused: the run's
        // ACTUAL current state (`Cancelled`) is not in `pause_legal`.
        let affected =
            projections::set_run_state_if_legal(&pool, run, &pause_legal, RunState::Paused)
                .await
                .unwrap();
        assert_eq!(affected, 0, "the second (losing) transition must not apply");

        // The run is still `Cancelled` — never resurrected to `Paused`.
        assert_eq!(
            projections::load_run_state(&pool, run).await.unwrap(),
            Some(RunState::Cancelled)
        );
    }

    #[tokio::test]
    async fn a_write_whose_validation_is_now_stale_cannot_commit() {
        // FP-3, deterministic reproduction of the exact race window (rather
        // than relying on real thread scheduling to interleave two `apply()`
        // calls, which is inherently non-deterministic — a `tokio::join!`
        // version of this test was tried and only reproduced the bug on
        // roughly half of runs even on a multi-threaded runtime): a command
        // whose `validate()` already passed (against the run's state at read
        // time) reaches its write stage — modeled here by calling the private
        // write-path method directly, exactly the state a command is in
        // between `validate()` returning `Ok` and `commit()` running — but by
        // then a DIFFERENT command has already taken the run to a state from
        // which this one is no longer legal. The write itself must refuse,
        // not blindly apply what an earlier read decided.
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();
        let session = create_session(&processor, &pool, "create").await;
        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();

        // The WINNING command commits first, taking the run terminal.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::CancelRun { run_id: run }, "winner-cancel"),
            )
            .await
            .expect("cancel applies");
        assert_eq!(
            projections::load_run_state(&pool, run).await.unwrap(),
            Some(RunState::Cancelled)
        );

        // The LOSING command reaches its write stage as if its OWN `validate()`
        // had already passed against the run's PRE-cancellation state (calling
        // the write-path method directly skips `apply()`'s own validate call,
        // modeling exactly that moment).
        let pause_command = command(CommandBody::PauseRun { run_id: run }, "loser-pause");
        let err = processor
            .apply_run_state(
                &pool,
                &ctx(ClientRole::Controller),
                &pause_command,
                run,
                RunState::Paused,
            )
            .await
            .expect_err("the write must re-validate against the CURRENT state and refuse");
        assert_eq!(err.code, "run.invalid-transition");

        // The run is still `Cancelled` — never resurrected to `Paused`.
        assert_eq!(
            projections::load_run_state(&pool, run).await.unwrap(),
            Some(RunState::Cancelled)
        );
    }

    #[tokio::test]
    async fn replay_is_deterministic() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();

        let session = create_session(&processor, &pool, "create").await;
        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::PauseRun { run_id: run }, "pause"),
            )
            .await
            .unwrap();
        processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::SubmitUserInput {
                        session_id: session,
                        text: "keep going".to_string(),
                        mode: AgentMode::Build,
                        model: None,
                        envelope: None,
                    },
                    "input",
                ),
            )
            .await
            .unwrap();

        // Fold the ledger events into the projection by hand, and assert it
        // equals the DB-backed projection: derived state is deterministic.
        let events = crate::ledger::load_events(&pool, session).await.unwrap();
        let mut title = String::new();
        let mut closed = false;
        let mut active: Vec<RunId> = Vec::new();
        let mut last_sequence = 0u64;
        for event in &events {
            last_sequence = event.sequence;
            match &event.body {
                EventBody::SessionCreated { title: t } => title = t.clone(),
                EventBody::SessionClosed => closed = true,
                EventBody::RunStarted { run_id, .. } => active.push(*run_id),
                EventBody::RunStateChanged { run_id, state }
                    if projections::is_terminal(*state) =>
                {
                    active.retain(|r| r != run_id);
                }
                _ => {}
            }
        }
        active.sort();
        let folded = codypendent_protocol::SessionProjection {
            session_id: session,
            title,
            last_sequence,
            active_runs: active,
            pending_approvals: Vec::new(),
            pending_prompts: Vec::new(),
            closed,
        };

        let projected = projections::session_projection(&pool, session)
            .await
            .unwrap();
        assert_eq!(folded, projected);
    }

    #[tokio::test]
    async fn unknown_command_is_rejected_structurally() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();

        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(CommandBody::Unknown, "unknown"),
            )
            .await
            .expect_err("unknown body rejected");
        assert_eq!(err.code, "protocol.unsupported-payload");
    }

    #[tokio::test]
    async fn attach_session_is_rejected_by_the_write_path() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let processor = CommandProcessor::default();

        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::AttachSession {
                        session_id: SessionId::new(),
                        last_seen_sequence: None,
                        subscriptions: vec![],
                        requested_role: ClientRole::Observer,
                        repository: None,
                    },
                    "attach",
                ),
            )
            .await
            .expect_err("attach rejected");
        assert_eq!(err.code, "protocol.attach-is-connection-level");
    }

    #[tokio::test]
    async fn resolve_approval_delegates_to_the_broker() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let broker = ApprovalBroker::new();
        let questions = QuestionBroker::new();
        let processor =
            CommandProcessor::new(SubscriptionHub::new(), broker.clone(), questions.clone());
        let session = create_session(&processor, &pool, "create").await;

        // Seed a run + a pending approval to resolve.
        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();
        let approval_id = broker
            .request(
                &pool,
                session,
                run,
                None,
                codypendent_protocol::ProposedAction::ExecuteCommand {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                    environment: Vec::new(),
                    cwd: None,
                },
                codypendent_protocol::Risk {
                    level: codypendent_protocol::RiskLevel::Medium,
                    reasons: vec![],
                },
                vec![],
                None,
            )
            .await
            .unwrap();

        let mut rx = processor.subscriptions().subscribe(session);
        processor
            .apply(
                &pool,
                ctx(ClientRole::Approver),
                command(
                    CommandBody::ResolveApproval {
                        approval_id,
                        decision: ApprovalDecision::Approve,
                        scope: ApprovalScope::Once,
                    },
                    "resolve",
                ),
            )
            .await
            .expect("resolve approval");

        let (state,): (String,) = sqlx::query_as("SELECT state FROM approvals WHERE id = ?")
            .bind(approval_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "approved");

        // The processor re-published the broker's ApprovalResolved.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event.body,
            EventBody::ApprovalResolved {
                decision: ApprovalDecision::Approve,
                ..
            }
        ));
    }

    /// issue #6 item 2b: the `expected_revision` guard and the revision bump are
    /// held in the same transaction as the `ApprovalResolved` append, so a resolve
    /// consumes exactly one revision and a second command carrying the now-stale
    /// revision is rejected instead of also passing.
    #[tokio::test]
    async fn resolve_approval_guards_and_bumps_the_session_revision() {
        use codypendent_protocol::{ProposedAction, Risk, RiskLevel};

        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let broker = ApprovalBroker::new();
        let questions = QuestionBroker::new();
        let processor =
            CommandProcessor::new(SubscriptionHub::new(), broker.clone(), questions.clone());
        let session = create_session(&processor, &pool, "create").await;

        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();

        let a1 = broker
            .request(
                &pool,
                session,
                run,
                None,
                ProposedAction::ExecuteCommand {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                    environment: Vec::new(),
                    cwd: None,
                },
                Risk {
                    level: RiskLevel::Low,
                    reasons: vec![],
                },
                vec![],
                None,
            )
            .await
            .unwrap();
        let a2 = broker
            .request(
                &pool,
                session,
                run,
                None,
                ProposedAction::ExecuteCommand {
                    program: "cargo".to_string(),
                    args: vec!["bench".to_string()],
                    environment: Vec::new(),
                    cwd: None,
                },
                Risk {
                    level: RiskLevel::Low,
                    reasons: vec![],
                },
                vec![],
                None,
            )
            .await
            .unwrap();

        let revision = |pool: SqlitePool| async move {
            let (r,): (i64,) = sqlx::query_as("SELECT revision FROM sessions WHERE id = ?")
                .bind(session.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
            u64::try_from(r).unwrap()
        };
        let rev = revision(pool.clone()).await;

        let resolve_cmd = |approval, key: &str, expected| {
            let mut cmd = command(
                CommandBody::ResolveApproval {
                    approval_id: approval,
                    decision: ApprovalDecision::Approve,
                    scope: ApprovalScope::Once,
                },
                key,
            );
            cmd.expected_revision = expected;
            cmd
        };

        // Resolve a1 at the current revision → applies and bumps by one.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Approver),
                resolve_cmd(a1, "r1", Some(rev)),
            )
            .await
            .expect("first resolve applies");
        assert_eq!(
            revision(pool.clone()).await,
            rev + 1,
            "resolving bumped the session revision"
        );

        // Resolve a2 carrying the stale revision → rejected, a2 untouched.
        let err = processor
            .apply(
                &pool,
                ctx(ClientRole::Approver),
                resolve_cmd(a2, "r2", Some(rev)),
            )
            .await
            .expect_err("a stale expected_revision is rejected");
        assert_eq!(err.code, "protocol.revision-conflict");
        let (state,): (String,) = sqlx::query_as("SELECT state FROM approvals WHERE id = ?")
            .bind(a2.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "pending", "the rejected command applied nothing");

        // Resolve a2 at the fresh revision → applies.
        processor
            .apply(
                &pool,
                ctx(ClientRole::Approver),
                resolve_cmd(a2, "r3", Some(rev + 1)),
            )
            .await
            .expect("resolve at the fresh revision applies");
    }

    #[tokio::test]
    async fn resolve_question_delegates_to_broker_and_bumps_revision() {
        use codypendent_protocol::{QuestionOption, QuestionPrompt};

        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let broker = ApprovalBroker::new();
        let questions = QuestionBroker::new();
        let processor =
            CommandProcessor::new(SubscriptionHub::new(), broker.clone(), questions.clone());
        let session = create_session(&processor, &pool, "create").await;

        let run = processor
            .apply(
                &pool,
                ctx(ClientRole::Controller),
                command(
                    CommandBody::StartRun {
                        session_id: session,
                        objective: "diagnose".to_string(),
                        mode: AgentMode::Build,
                        repository: None,
                        model: None,
                    },
                    "start",
                ),
            )
            .await
            .unwrap()
            .created_run
            .unwrap();

        let prompt = QuestionPrompt {
            question: "Confirm?".to_string(),
            header: "Confirm".to_string(),
            options: vec![QuestionOption {
                label: "Yes".to_string(),
                description: String::new(),
            }],
            multiple: false,
            custom: true,
        };

        let question_id = questions
            .ask(&pool, session, run, vec![prompt])
            .await
            .unwrap();

        let mut rx = processor.subscriptions().subscribe(session);

        let resolve_cmd = command(
            CommandBody::ResolveQuestion {
                question_id,
                outcome: QuestionOutcome::Answered {
                    answers: vec![vec!["Yes".to_string()]],
                },
            },
            "resolve-q",
        );

        let outcome = processor
            .apply(&pool, ctx(ClientRole::Approver), resolve_cmd)
            .await
            .expect("resolve question applies");

        assert!(outcome.last_sequence.is_some());

        let (state,): (String,) = sqlx::query_as("SELECT state FROM questions WHERE id = ?")
            .bind(question_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "answered");

        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event.body,
            EventBody::QuestionResolved {
                outcome: QuestionOutcome::Answered { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn queue_prompt_appends_one_snapshot_event_in_the_command_transaction() {
        let tmp = tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let hub = SubscriptionHub::new();
        let broker = ApprovalBroker::new();
        let qbroker = QuestionBroker::new();
        let proc = CommandProcessor::new(hub, broker, qbroker);

        let session_id = SessionId::new();
        crate::ledger::create_session(&pool, session_id, "Test Session")
            .await
            .unwrap();

        let queue_cmd = Command {
            command_id: CommandId::new(),
            idempotency_key: "k-queue-1".to_string(),
            expected_revision: None,
            body: CommandBody::QueuePrompt {
                session_id,
                text: "queued message".to_string(),
                mode: AgentMode::Build,
                delivery: PromptDelivery::Queue,
            },
        };

        let outcome = proc
            .apply(&pool, ctx(ClientRole::Contributor), queue_cmd.clone())
            .await
            .unwrap();

        assert!(outcome.newly_applied);
        let events = crate::ledger::load_events(&pool, session_id).await.unwrap();
        assert_eq!(events.len(), 1); // PendingPromptsChanged
        let ev = &events[0];
        match &ev.body {
            EventBody::PendingPromptsChanged { prompts } => {
                assert_eq!(prompts.len(), 1);
                assert_eq!(prompts[0].text, "queued message");
                assert_eq!(prompts[0].delivery, PromptDelivery::Queue);
            }
            other => panic!("expected PendingPromptsChanged, got {other:?}"),
        }

        // Duplicate delivery produces one result (idempotency replay)
        let outcome_dup = proc
            .apply(&pool, ctx(ClientRole::Contributor), queue_cmd)
            .await
            .unwrap();
        assert!(!outcome_dup.newly_applied);
        let events_after_dup = crate::ledger::load_events(&pool, session_id).await.unwrap();
        assert_eq!(events_after_dup.len(), 1);
    }

    #[tokio::test]
    async fn queue_steering_enqueues_steer_entry_and_journals_steering_queued() {
        let tmp = tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let hub = SubscriptionHub::new();
        let broker = ApprovalBroker::new();
        let qbroker = QuestionBroker::new();
        let proc = CommandProcessor::new(hub, broker, qbroker);

        let session_id = SessionId::new();
        crate::ledger::create_session(&pool, session_id, "Test Session")
            .await
            .unwrap();

        // Start a run
        let start_cmd = Command {
            command_id: CommandId::new(),
            idempotency_key: "k-start-1".to_string(),
            expected_revision: None,
            body: CommandBody::StartRun {
                session_id,
                objective: "build app".to_string(),
                mode: AgentMode::Build,
                repository: None,
                model: None,
            },
        };
        let start_outcome = proc
            .apply(&pool, ctx(ClientRole::Contributor), start_cmd)
            .await
            .unwrap();
        let started_run_id = start_outcome.created_run.unwrap();

        // Queue steering text
        let steer_cmd = Command {
            command_id: CommandId::new(),
            idempotency_key: "k-steer-1".to_string(),
            expected_revision: None,
            body: CommandBody::QueueSteering {
                run_id: started_run_id,
                text: "steer live run".to_string(),
            },
        };

        let outcome = proc
            .apply(&pool, ctx(ClientRole::Contributor), steer_cmd)
            .await
            .unwrap();
        assert!(outcome.newly_applied);

        let events = crate::ledger::load_events(&pool, session_id).await.unwrap();
        // Events: RunStarted, SteeringQueued, PendingPromptsChanged
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[1].body,
            EventBody::SteeringQueued { run_id: r } if r == started_run_id
        ));
        match &events[2].body {
            EventBody::PendingPromptsChanged { prompts } => {
                assert_eq!(prompts.len(), 1);
                assert_eq!(prompts[0].text, "steer live run");
                assert_eq!(prompts[0].delivery, PromptDelivery::Steer);
            }
            other => panic!("expected PendingPromptsChanged, got {other:?}"),
        }

        // Verify prompt was persisted in pending_prompts table
        let snap = crate::prompt_queue::snapshot_pool(&pool, session_id)
            .await
            .unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].text, "steer live run");
        assert_eq!(snap[0].delivery, PromptDelivery::Steer);
    }
}

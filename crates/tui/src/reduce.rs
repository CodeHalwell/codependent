//! The reducer (STEP 1.12 RULE 3): the one pure state transition.
//!
//! `reduce` performs no I/O. Every daemon event and every input-derived action
//! is folded here, deterministically, into [`AppState`]. Commands the daemon
//! must run are appended to [`AppState::outbox`] as [`Intent`]s for the CLI to
//! dispatch — the reducer never touches a socket. Folding [`EventBody`] into
//! transcript/run/approval state is the core, and it is what the unit tests
//! below exercise.

use codypendent_protocol::{
    Actor, ApprovalDecision, ApprovalScope, BudgetDimension, DocumentId, DocumentMutation,
    EventBody, ProposedAction, RunDisposition, RunState, SessionEvent,
};

use crate::action::{Action, Intent, SecretKey};
use crate::state::{
    filter_model_names, filter_models, filter_modes, filter_providers, AppState, DocBlockView,
    DocEdit, DocFocus, DocLeaseState, DocSuggestionView, Overlay, Pane, PatchSummary,
    PendingApproval, RunActivity, RunView, ToolCard, ToolStatus, TranscriptEntry,
};

/// Above this size a message stays on the fast plain path (its single parse
/// would be too costly). 64 KiB — a quarter of `MAX_MODEL_ENTRY_BYTES`.
const RICH_MARKDOWN_MAX_BYTES: usize = 64 * 1024;

/// Parse every finalized (non-streaming-tail) `Model` entry into its rich cache
/// exactly once. Runs at the tail of every folded `DaemonEvent`, so it catches
/// all stream-ending transitions without enumerating them. Idempotent (skips any
/// entry already `Some`); bounded (O(total Model entries) cheap `is_none` checks).
pub(crate) fn finalize_streamed_models(state: &mut AppState) {
    let last_run = state.runs.len().checked_sub(1);
    for (idx, run) in state.runs.iter_mut().enumerate() {
        // The live streaming tail (only possible in the last run) is skipped.
        let tail = if Some(idx) == last_run && run.activity == RunActivity::Streaming {
            run.transcript.len().checked_sub(1)
        } else {
            None
        };
        for (i, entry) in run.transcript.iter_mut().enumerate() {
            if Some(i) == tail {
                continue;
            }
            if let TranscriptEntry::Model { text, rendered } = entry {
                if rendered.is_none() && text.len() <= RICH_MARKDOWN_MAX_BYTES {
                    *rendered = Some(crate::markdown::parse(text));
                }
            }
        }
    }
}

/// Fold a single [`Action`] into the state. Pure: the only side effect is
/// mutating `state` (including appending intents to its outbox).
pub fn reduce(state: &mut AppState, action: Action) {
    match action {
        Action::DaemonEvent(event) => {
            apply_event(state, *event);
            finalize_streamed_models(state);
        }
        Action::CatchupSnapshot {
            title,
            closed,
            runs,
        } => {
            // Too far behind for an event replay: seed what the snapshot carries.
            // Runs become stubs (their objective/mode fill in from the next live
            // event) so the session is not blank on reopen.
            state.session_title = Some(title);
            state.session_closed = closed;
            let mode = state.default_mode;
            for run_id in runs {
                state.ensure_run(run_id, String::new(), mode);
            }
        }
        Action::Tick => {
            state.tick = state.tick.wrapping_add(1);
            if let Some((_, expires)) = &state.notice {
                if state.tick >= *expires {
                    state.notice = None;
                }
            }
        }
        // ~5 seconds at the 5 fps tick.
        Action::Notice(text) => state.notice = Some((text, state.tick + 25)),

        // In the Docs overlay `Tab` cycles the tree / editor / review rail focus;
        // elsewhere it cycles the (vestigial) pane focus.
        Action::CyclePane => {
            if matches!(state.overlay, Overlay::Docs) {
                state.doc_focus = state.doc_focus.next();
            } else {
                state.focus = state.focus.next();
            }
        }
        Action::FocusPane(pane) => state.focus = pane,
        Action::ActivateRow(n) => activate_row(state, n),
        Action::SelectRun(n) => {
            let mut idx = n;
            clamp(&mut idx, state.runs.len());
            state.selected_run = idx;
        }
        Action::SelectPrev => nav(state, -1),
        Action::SelectNext => nav(state, 1),
        Action::ScrollPageUp => scroll_page(state, true),
        Action::ScrollPageDown => scroll_page(state, false),
        Action::Expand => expand_selected(state),

        Action::PrevRun => cycle_run(state, -1),
        Action::NextRun => cycle_run(state, 1),
        Action::NewRun => state.overlay = Overlay::NewRun(String::new()),
        Action::Pause => pause_or_resume(state),
        Action::Cancel => request_cancel(state),
        Action::ConfirmCancel => confirm_cancel(state),
        Action::Steer => begin_steering(state),

        // `a`/`r` resolve a document suggestion when the Docs review rail is
        // focused (going through the same `MutateDocument` accept/reject the daemon
        // gates on the Approver/Controller role); otherwise they resolve a pending
        // approval, exactly as before.
        Action::Approve(scope) => {
            if matches!(state.overlay, Overlay::Docs) {
                resolve_focused_suggestion(state, true);
            } else {
                resolve_focused(state, ApprovalDecision::Approve, scope);
            }
        }
        Action::Reject => {
            if matches!(state.overlay, Overlay::Docs) {
                resolve_focused_suggestion(state, false);
            } else {
                resolve_focused(state, ApprovalDecision::Reject, ApprovalScope::Once);
            }
        }

        Action::InputChar(c) => input_char(state, c),
        Action::InputPaste(text) => {
            edit_prompt(state, move |buf| buf.push_str(&text));
            detach_history_on_edit(state);
        }
        Action::InputBackspace => {
            edit_prompt(state, |buf| {
                buf.pop();
            });
            detach_history_on_edit(state);
        }
        Action::InputNewline => {
            edit_prompt(state, |buf| buf.push('\n'));
            detach_history_on_edit(state);
        }
        Action::InputSubmit => submit_prompt(state),
        Action::InputCancel => input_cancel(state),
        Action::HistoryPrev => history_prev(state),
        Action::HistoryNext => history_next(state),

        Action::OpenSkills => {
            state.overlay = match state.overlay {
                Overlay::Skills => Overlay::None,
                _ => Overlay::Skills,
            }
        }
        Action::OpenMemory => {
            state.overlay = match state.overlay {
                Overlay::Memory { .. } => Overlay::None,
                _ => Overlay::Memory { source_open: false },
            }
        }
        Action::OpenSource => open_source(state),

        Action::OpenDocs => {
            if matches!(state.overlay, Overlay::Docs) {
                // Closing the browser releases any block lease this client holds.
                release_doc_lease(state);
                state.overlay = Overlay::None;
            } else {
                state.overlay = Overlay::Docs;
            }
        }
        Action::OpenEdges => {
            state.overlay = match state.overlay {
                Overlay::Edges => Overlay::None,
                _ => Overlay::Edges,
            }
        }
        Action::OpenWorkflow => {
            state.overlay = match state.overlay {
                Overlay::Workflow => Overlay::None,
                _ => Overlay::Workflow,
            }
        }
        Action::OpenBlackboard => {
            state.overlay = match state.overlay {
                Overlay::Blackboard => Overlay::None,
                _ => Overlay::Blackboard,
            }
        }
        Action::OpenPalette => {
            state.overlay = match state.overlay {
                Overlay::Palette { .. } => Overlay::None,
                _ => Overlay::Palette {
                    query: String::new(),
                    selected: 0,
                },
            }
        }
        Action::BeginAddModel => begin_add_model(state),
        Action::ToggleLayout => state.layout = state.layout.toggled(),

        Action::Help => {
            state.overlay = match state.overlay {
                Overlay::Help => Overlay::None,
                _ => Overlay::Help,
            }
        }
        Action::Detach => state.should_detach = true,
        Action::Dismiss => {
            // Leaving the Docs browser releases any block lease this client holds.
            if matches!(state.overlay, Overlay::Docs) {
                release_doc_lease(state);
            }
            state.overlay = Overlay::None;
        }

        // --- Docs Studio live editing (Phase 4 STEP 4.3 client wiring) ---
        Action::EditDoc => begin_doc_edit(state),
        Action::DocumentSynced {
            document_id,
            revision,
            blocks,
            suggestions,
        } => apply_document_sync(state, document_id, revision, blocks, suggestions),
        Action::DocumentLeaseGranted {
            document_id,
            lease_id,
        } => on_lease_granted(state, document_id, lease_id),
        Action::DocumentLeaseBlocked => on_lease_blocked(state),

        // --- Workflow-graph live overlay (Phase 5 T9) ---
        Action::WorkflowNodeUpdated {
            node_id,
            state: node_state,
            cost,
            error,
        } => apply_workflow_node_update(state, &node_id, node_state, cost, error),

        // --- model discovery: the harness's fetched-list return path ---
        Action::ProviderModelsLoaded {
            provider_id,
            models,
        } => on_provider_models_loaded(state, provider_id, models),
        Action::ProviderModelsFailed {
            provider_id,
            reason,
        } => on_provider_models_failed(state, provider_id, reason),

        Action::NoOp => {}
    }
}

/// Overlay a live workflow node transition onto the graph-view cards (Phase 5 T9):
/// every card matching `node_id` takes the transition's pre-rendered `state` / `cost`
/// / `error`, so the view reflects the run advancing instead of the forever-`pending`
/// pre-run placeholders. Idempotent overwrite (a re-delivered transition writes the
/// same values), keyed by node id — the fold the CLI harness feeds after folding a
/// `Payload::WorkflowEvent`.
fn apply_workflow_node_update(
    state: &mut AppState,
    node_id: &str,
    node_state: String,
    cost: String,
    error: String,
) {
    for card in state.workflow.iter_mut().filter(|card| card.id == node_id) {
        card.state = node_state.clone();
        card.cost = cost.clone();
        card.error = error.clone();
    }
}

/// Fold one durable event into run / transcript / approval state.
fn apply_event(state: &mut AppState, event: SessionEvent) {
    let SessionEvent { actor, body, .. } = event;

    // Learn the serving model from any agent-authored event.
    if let Actor::Agent { run_id, model, .. } = &actor {
        let (rid, model) = (*run_id, model.clone());
        if let Some(run) = state.run_mut(rid) {
            run.model = Some(model);
        }
    }

    match body {
        EventBody::SessionCreated { title } => state.session_title = Some(title),
        EventBody::NoteAppended { text, run_id } => {
            // A run-scoped note (context manifest, curated memory) is routed to
            // its own run so it can't land on whatever run happens to be selected
            // when runs interleave (issue #6 item 3); a session-level note (no
            // run_id) still attaches to the focused run.
            let target = match run_id {
                Some(run_id) => state.run_mut(run_id),
                None => state.selected_run_mut(),
            };
            let Some(run) = target else { return };

            // Backstage fold (Task 2): the context manifest and curated-memory
            // writes are real, but not part of the visible conversation. The
            // daemon labels both by the note's own text prefix (context:
            // `crates/knowledge/src/context.rs`'s `=== CONTEXT` manifest
            // header; memory: `executor.rs`'s `remembered: {statement}`), so
            // classify on that prefix and fold into the run's single
            // `Backstage` entry (find-or-push, update counts) instead of a
            // visible `Note` cell. Every other note falls through to the
            // existing declutter fold below, unchanged.
            let is_context = text.starts_with("=== CONTEXT");
            let is_memory = text.trim_start().starts_with("remembered:");
            if is_context || is_memory {
                let existing = run.transcript.iter_mut().find_map(|entry| match entry {
                    TranscriptEntry::Backstage { .. } => Some(entry),
                    _ => None,
                });
                let backstage = match existing {
                    Some(TranscriptEntry::Backstage {
                        context_lines,
                        memory_updates,
                        raw,
                        ..
                    }) => {
                        if is_context {
                            *context_lines = Some(text.lines().count());
                        }
                        if is_memory {
                            *memory_updates += 1;
                        }
                        raw.push(text);
                        return; // folded into the existing entry — no visible Note
                    }
                    _ => TranscriptEntry::Backstage {
                        context_lines: is_context.then(|| text.lines().count()),
                        memory_updates: is_memory as usize,
                        raw: vec![text],
                        expanded: false,
                    },
                };
                AppState::push_entry(run, backstage);
                return;
            }

            AppState::push_entry(
                run,
                TranscriptEntry::Note {
                    text,
                    expanded: false,
                },
            );
        }
        EventBody::SessionClosed => state.session_closed = true,

        EventBody::RunStarted {
            run_id,
            objective,
            mode,
        } => {
            let run = state.ensure_run(run_id, objective.clone(), mode);
            run.state = RunState::Preparing;
            // The objective is the run's opening turn — shown once here as the
            // first transcript entry, in addition to the pane title `ensure_run`
            // already set on `RunView.objective` (unchanged).
            AppState::push_entry(run, TranscriptEntry::User { text: objective });
        }
        EventBody::RunStateChanged { run_id, state: rs } => {
            if let Some(run) = state.run_mut(run_id) {
                run.state = rs;
                // Only the states this task's transition table names move
                // `activity`; anything else (paused, awaiting approval/input,
                // recovering, unknown) leaves whatever activity was last
                // observed in place.
                match rs {
                    RunState::Preparing | RunState::Running => {
                        run.activity = RunActivity::Thinking;
                    }
                    RunState::Completed | RunState::Failed | RunState::Cancelled => {
                        run.activity = RunActivity::Idle;
                    }
                    _ => {}
                }
            }
        }
        EventBody::ModelStreamDelta { run_id, text } => {
            if let Some(run) = state.run_mut(run_id) {
                AppState::append_model_text(run, &text);
                run.activity = RunActivity::Streaming;
            }
        }
        EventBody::ToolProposed {
            run_id,
            approval_id,
            action,
        } => {
            if let Some(run) = state.run_mut(run_id) {
                AppState::push_entry(
                    run,
                    TranscriptEntry::Tool(Box::new(ToolCard {
                        tool: String::new(),
                        status: ToolStatus::Proposed,
                        action: Some(action),
                        args_digest: None,
                        label: None,
                        outcome: None,
                        artifact: None,
                        approval_id: Some(approval_id),
                        expanded: false,
                    })),
                );
            }
            // Backfill the run link onto a matching pending approval.
            if let Some(pending) = state
                .pending_approvals
                .iter_mut()
                .find(|p| p.approval_id == approval_id)
            {
                pending.run_id = Some(run_id);
            }
        }
        EventBody::ToolStarted {
            run_id,
            tool,
            args_digest,
            label,
        } => {
            if let Some(run) = state.run_mut(run_id) {
                // Cloned before `tool` moves into the card below: the tool
                // card entering `Running` is what `RunActivity::RunningTool`
                // names.
                let tool_name = tool.clone();
                match last_card(run, |c| c.status == ToolStatus::Proposed) {
                    Some(card) => {
                        card.tool = tool;
                        card.args_digest = Some(args_digest);
                        card.label = label;
                        card.status = ToolStatus::Running;
                    }
                    None => AppState::push_entry(
                        run,
                        TranscriptEntry::Tool(Box::new(ToolCard {
                            tool,
                            status: ToolStatus::Running,
                            action: None,
                            args_digest: Some(args_digest),
                            label,
                            outcome: None,
                            artifact: None,
                            approval_id: None,
                            expanded: false,
                        })),
                    ),
                }
                run.activity = RunActivity::RunningTool(tool_name);
            }
        }
        EventBody::ToolCompleted {
            run_id,
            tool,
            outcome,
            artifact,
        } => {
            if let Some(run) = state.run_mut(run_id) {
                match last_card(run, |c| c.status != ToolStatus::Completed) {
                    Some(card) => {
                        if card.tool.is_empty() {
                            card.tool = tool;
                        }
                        card.status = ToolStatus::Completed;
                        card.outcome = Some(outcome);
                        card.artifact = artifact;
                    }
                    None => AppState::push_entry(
                        run,
                        TranscriptEntry::Tool(Box::new(ToolCard {
                            tool,
                            status: ToolStatus::Completed,
                            action: None,
                            args_digest: None,
                            label: None,
                            outcome: Some(outcome),
                            artifact,
                            approval_id: None,
                            expanded: false,
                        })),
                    ),
                }
                // The tool finished; the agent is back to composing its next
                // step.
                run.activity = RunActivity::Thinking;
            }
        }
        EventBody::PatchProposed {
            run_id,
            changeset_id,
            artifact,
        } => {
            if let Some(run) = state.run_mut(run_id) {
                AppState::push_entry(
                    run,
                    TranscriptEntry::Patch(PatchSummary {
                        changeset_id,
                        artifact,
                        expanded: false,
                    }),
                );
            }
        }
        EventBody::ApprovalRequested {
            approval_id,
            action,
            risk,
        } => {
            let run_id = run_of_approval(state, approval_id);
            state.pending_approvals.push(PendingApproval {
                approval_id,
                action,
                risk,
                run_id,
            });
        }
        EventBody::ApprovalResolved { approval_id, .. } => {
            state
                .pending_approvals
                .retain(|p| p.approval_id != approval_id);
            clamp(&mut state.selected_approval, state.pending_approvals.len());
        }
        EventBody::SteeringQueued { run_id } => {
            if let Some(run) = state.run_mut(run_id) {
                AppState::push_entry(run, TranscriptEntry::Steering { applied: false });
            }
        }
        EventBody::SteeringApplied { run_id } => {
            if let Some(run) = state.run_mut(run_id) {
                let marked = run.transcript.iter_mut().rev().find_map(|e| match e {
                    TranscriptEntry::Steering { applied } if !*applied => Some(applied),
                    _ => None,
                });
                match marked {
                    Some(applied) => *applied = true,
                    None => AppState::push_entry(run, TranscriptEntry::Steering { applied: true }),
                }
            }
        }
        EventBody::BudgetWarning {
            run_id,
            dimension,
            used,
            limit,
        } => {
            if let Some(run) = state.run_mut(run_id) {
                match dimension {
                    BudgetDimension::Tokens => {
                        let pct = used.saturating_mul(100) / limit.max(1);
                        run.context_percent = Some(pct.min(100) as u16);
                    }
                    BudgetDimension::Cost => run.cost_minor = Some(used),
                    _ => {}
                }
                AppState::push_entry(
                    run,
                    TranscriptEntry::Budget {
                        dimension,
                        used,
                        limit,
                    },
                );
            }
        }
        EventBody::RunCompleted {
            run_id,
            disposition,
            ..
        } => {
            if let Some(run) = state.run_mut(run_id) {
                run.state = terminal_state(&disposition);
                AppState::push_entry(
                    run,
                    TranscriptEntry::Completed {
                        disposition: disposition.clone(),
                        expanded: false,
                    },
                );
                run.disposition = Some(disposition);
                run.activity = RunActivity::Idle;
            }
        }

        // Presence: another client joined or left this session (STEP 3.7). A
        // transient status notice, not a transcript entry — presence is
        // ambient, and the flagship handoff demo must not read as
        // "unsupported event".
        EventBody::ClientPresenceChanged {
            client_id,
            role,
            present,
        } => {
            let id = client_id.to_string();
            let short = id.get(..8).unwrap_or(&id);
            let verb = if present { "joined" } else { "left" };
            state.notice = Some((
                format!("client {short} {verb} ({})", role_label(role)),
                state.tick + 25,
            ));
        }

        // `Unknown` and any future event type this build predates render a
        // placeholder and keep going (protocol RULE 1).
        _ => {
            if let Some(run) = state.selected_run_mut() {
                AppState::push_entry(
                    run,
                    TranscriptEntry::Unsupported {
                        label: "unsupported event".to_owned(),
                    },
                );
            }
        }
    }
}

/// A short human label for a client role (presence notices).
fn role_label(role: codypendent_protocol::ClientRole) -> &'static str {
    use codypendent_protocol::ClientRole;
    match role {
        ClientRole::Observer => "observer",
        ClientRole::Contributor => "contributor",
        ClientRole::Controller => "controller",
        ClientRole::Approver => "approver",
        _ => "unknown role",
    }
}

/// Find the most recent tool card matching `pred`, mutably.
fn last_card(run: &mut RunView, pred: impl Fn(&ToolCard) -> bool) -> Option<&mut ToolCard> {
    run.transcript.iter_mut().rev().find_map(|e| match e {
        TranscriptEntry::Tool(card) if pred(card) => Some(card.as_mut()),
        _ => None,
    })
}

/// Which run (if any) owns a proposed approval, inferred from tool cards.
fn run_of_approval(
    state: &AppState,
    approval_id: codypendent_protocol::ApprovalId,
) -> Option<codypendent_protocol::RunId> {
    state.runs.iter().find_map(|run| {
        run.transcript.iter().find_map(|e| match e {
            TranscriptEntry::Tool(card) if card.approval_id == Some(approval_id) => {
                Some(run.run_id)
            }
            _ => None,
        })
    })
}

fn terminal_state(disposition: &RunDisposition) -> RunState {
    match disposition {
        RunDisposition::Completed { .. } => RunState::Completed,
        RunDisposition::Failed { .. } => RunState::Failed,
        RunDisposition::Cancelled { .. } => RunState::Cancelled,
        _ => RunState::Unknown,
    }
}

/// Move the selection / scroll by `delta` (-1 or +1). When a knowledge browser
/// is open it drives that browser's list; otherwise it drives the focused pane.
fn nav(state: &mut AppState, delta: i32) {
    match state.overlay {
        Overlay::Skills => {
            step(&mut state.selected_skill, state.skills.len(), delta);
            return;
        }
        Overlay::Memory { .. } => {
            step(&mut state.selected_memory, state.memories.len(), delta);
            // Moving to a different memory collapses any revealed source.
            state.overlay = Overlay::Memory { source_open: false };
            return;
        }
        Overlay::Docs => {
            match state.doc_focus {
                // The tree drives the document selection (the default rail, so this
                // is the pre-editing behaviour). A different document resets the
                // block/suggestion cursors so they never point past the new lists.
                DocFocus::Tree => {
                    step(&mut state.selected_doc, state.docs.len(), delta);
                    state.selected_block = 0;
                    state.selected_suggestion = 0;
                }
                DocFocus::Editor => {
                    let len = state.focused_doc().map_or(0, |d| d.blocks.len());
                    step(&mut state.selected_block, len, delta);
                }
                DocFocus::Review => {
                    let len = state.focused_doc().map_or(0, |d| d.suggestions.len());
                    step(&mut state.selected_suggestion, len, delta);
                }
            }
            return;
        }
        Overlay::Edges => {
            step(&mut state.selected_edge, state.edges.len(), delta);
            return;
        }
        Overlay::Workflow => {
            step(&mut state.selected_node, state.workflow.len(), delta);
            return;
        }
        Overlay::Blackboard => {
            step(&mut state.selected_item, state.blackboard.len(), delta);
            return;
        }
        Overlay::Palette {
            ref query,
            ref mut selected,
        } => {
            let count = crate::palette::filtered_len(query);
            step(selected, count, delta);
            return;
        }
        Overlay::ModelPicker {
            ref query,
            ref mut selected,
        } => {
            let indices = filter_models(&state.models, query);
            step(selected, indices.len(), delta);
            // Keep `selected_model` resolved to the same card the filtered
            // cursor points at, so `focused_model()` (the detail panel, and
            // Enter's staging) reads it without re-deriving the filter.
            state.selected_model = indices.get(*selected).copied().unwrap_or(0);
            return;
        }
        // Same shape as the model picker (Task 8): keep `selected_provider`
        // resolved to the same card the filtered cursor points at.
        Overlay::ProviderPicker {
            ref query,
            ref mut selected,
        } => {
            let indices = filter_providers(&state.providers, query);
            step(selected, indices.len(), delta);
            state.selected_provider = indices.get(*selected).copied().unwrap_or(0);
            return;
        }
        // The mode picker (PR C2): same filtered-cursor shape, over the static
        // [`MODE_CARDS`] table — there is no `AppState` list to re-resolve.
        Overlay::ModePicker {
            ref query,
            ref mut selected,
        } => {
            let indices = filter_modes(query);
            step(selected, indices.len(), delta);
            return;
        }
        // The add-model pick-list (model-discovery): the same shape as the
        // model/provider pickers, over the overlay's own `models` field rather
        // than an `AppState` list.
        Overlay::AddModelPick {
            ref query,
            ref mut selected,
            ref models,
            ..
        } => {
            let indices = filter_model_names(models, query);
            step(selected, indices.len(), delta);
            return;
        }
        _ => {}
    }
    // Base view: a pending approval owns the arrows (move between stacked
    // approvals). Otherwise the composer is active and the input layer routes
    // arrows to scroll / run-switch, so this legacy pane path is inert.
    if state.show_approval_modal() {
        step(
            &mut state.selected_approval,
            state.pending_approvals.len(),
            delta,
        );
        return;
    }
    match state.focus {
        Pane::Sessions => step(&mut state.selected_run, state.runs.len(), delta),
        Pane::Approvals => step(
            &mut state.selected_approval,
            state.pending_approvals.len(),
            delta,
        ),
        Pane::Transcript => {
            let idx = state.selected_run;
            if let Some(run) = state.runs.get_mut(idx) {
                step(&mut run.transcript_selected, run.transcript.len(), delta);
                run.scroll = run.transcript_selected.min(usize::from(u16::MAX)) as u16;
            }
        }
    }
}

fn scroll_page(state: &mut AppState, up: bool) {
    const PAGE: u16 = 10;
    // The renderer cached the true bottom last frame; use it so leaving follow
    // mode starts a page up from the bottom (not a jump to the top), and paging
    // back to the bottom re-enters follow.
    let max = state.transcript_max_scroll.get();
    let idx = state.selected_run;
    if let Some(run) = state.runs.get_mut(idx) {
        if up {
            if run.follow {
                run.follow = false;
                run.scroll = max;
            }
            run.scroll = run.scroll.saturating_sub(PAGE);
        } else {
            run.scroll = run.scroll.saturating_add(PAGE).min(max);
            if run.scroll >= max {
                run.follow = true;
            }
        }
    }
}

fn expand_selected(state: &mut AppState) {
    // In the memory browser, `Enter` opens the focused memory's source.
    if matches!(state.overlay, Overlay::Memory { .. }) {
        open_source(state);
        return;
    }
    if state.focus != Pane::Transcript {
        return;
    }
    let idx = state.selected_run;
    if let Some(run) = state.runs.get_mut(idx) {
        if let Some(entry) = run.transcript.get_mut(run.transcript_selected) {
            match entry {
                TranscriptEntry::Tool(card) => card.expanded = !card.expanded,
                TranscriptEntry::Patch(patch) => patch.expanded = !patch.expanded,
                TranscriptEntry::Note { expanded, .. } => *expanded = !*expanded,
                TranscriptEntry::Backstage { expanded, .. } => *expanded = !*expanded,
                TranscriptEntry::Completed { expanded, .. } => *expanded = !*expanded,
                _ => {}
            }
        }
    }
}

/// Reveal the focused memory's source in the memory browser. A no-op unless the
/// memory browser is open with at least one memory to open. The TUI does no I/O,
/// so "open" flips the overlay's `source_open` flag; the renderer then surfaces
/// the full source string (a real file-open is the CLI's job later).
fn open_source(state: &mut AppState) {
    if matches!(state.overlay, Overlay::Memory { .. }) && !state.memories.is_empty() {
        state.overlay = Overlay::Memory { source_open: true };
    }
}

fn pause_or_resume(state: &mut AppState) {
    let Some(run) = state.selected_run() else {
        return;
    };
    let run_id = run.run_id;
    let intent = match run.state {
        RunState::Paused => Some(Intent::ResumeRun { run_id }),
        RunState::Running | RunState::Preparing | RunState::Queued => {
            Some(Intent::PauseRun { run_id })
        }
        _ => None,
    };
    if let Some(intent) = intent {
        state.outbox.push(intent);
    }
}

fn request_cancel(state: &mut AppState) {
    let Some(run) = state.selected_run() else {
        return;
    };
    if !is_terminal(run.state) {
        state.overlay = Overlay::ConfirmCancel;
    }
}

fn confirm_cancel(state: &mut AppState) {
    if !matches!(state.overlay, Overlay::ConfirmCancel) {
        return;
    }
    state.overlay = Overlay::None;
    if let Some(run) = state.selected_run() {
        let run_id = run.run_id;
        state.outbox.push(Intent::CancelRun { run_id });
    }
}

fn begin_steering(state: &mut AppState) {
    if state.selected_run().is_some() {
        state.overlay = Overlay::Steering(String::new());
    }
}

fn resolve_focused(state: &mut AppState, decision: ApprovalDecision, scope: ApprovalScope) {
    // A decision must only be possible while its card is on screen. The
    // approval modal renders only when no overlay is open — with a browser or
    // Help overlay covering it, `a`/`r` are live Normal-mode keys and would
    // otherwise resolve an action the user cannot see.
    if !matches!(state.overlay, Overlay::None) {
        return;
    }
    if let Some(pending) = state.focused_approval() {
        let approval_id = pending.approval_id;
        state.outbox.push(Intent::ResolveApproval {
            approval_id,
            decision,
            scope,
        });
    }
}

// --- Docs Studio live editing (Phase 4 STEP 4.3 client wiring) ---

/// Begin editing the focused block: open the block-edit prompt. Only meaningful
/// with the editor rail focused and a block under the cursor.
fn begin_doc_edit(state: &mut AppState) {
    if !matches!(state.overlay, Overlay::Docs) || state.doc_focus != DocFocus::Editor {
        return;
    }
    if let Some(block_id) = state.focused_block().map(|block| block.id.clone()) {
        state.overlay = Overlay::DocEdit {
            block_id,
            buffer: String::new(),
        };
    }
}

/// Acquire `block_id`'s edit lease and queue `mutation` to fire once the daemon
/// grants it. Releases any lease this client already holds first, so switching to
/// a new block never orphans the old lease.
fn start_doc_edit(
    state: &mut AppState,
    document_id: DocumentId,
    block_id: Option<String>,
    mutation: DocumentMutation,
) {
    release_doc_lease(state);
    state.doc_edit = Some(DocEdit {
        document_id,
        block_id: block_id.clone(),
        lease: DocLeaseState::Acquiring,
        lease_id: None,
        pending: Some(mutation),
    });
    state.outbox.push(Intent::AcquireDocumentLease {
        document_id,
        block_id,
    });
}

/// The daemon granted the requested lease: mark the edit held and fire its queued
/// mutation exactly once. Ignores a grant for a document that is no longer the
/// in-flight edit (e.g. the browser was closed before it arrived).
fn on_lease_granted(state: &mut AppState, document_id: DocumentId, lease_id: String) {
    let mutation = match state.doc_edit.as_mut() {
        Some(edit) if edit.document_id == document_id => {
            edit.lease = DocLeaseState::Held;
            edit.lease_id = Some(lease_id);
            edit.pending.take()
        }
        _ => return,
    };
    if let Some(mutation) = mutation {
        state.outbox.push(Intent::MutateDocument {
            document_id,
            mutation,
        });
    }
}

/// The daemon refused the lease (`document.range-leased`): mark the edit blocked,
/// drop its queued mutation, and surface the presence-lite notice.
fn on_lease_blocked(state: &mut AppState) {
    if let Some(edit) = state.doc_edit.as_mut() {
        edit.lease = DocLeaseState::Blocked;
        edit.pending = None;
    }
    state.notice = Some((
        "block is being edited by another writer".to_owned(),
        state.tick + 25,
    ));
}

/// Release a held block lease (if any). Only a *held* lease carries an id to
/// release; an acquiring or blocked one just clears.
fn release_doc_lease(state: &mut AppState) {
    if let Some(edit) = state.doc_edit.take() {
        if let Some(lease_id) = edit.lease_id {
            state.outbox.push(Intent::ReleaseDocumentLease { lease_id });
        }
    }
}

/// Accept (`accept = true`) or reject the focused suggestion in the review rail,
/// through the daemon's `MutateDocument` accept/reject (role-gated there — a
/// resolution needs no edit lease). Only fires with the review rail focused and a
/// suggestion under the cursor.
fn resolve_focused_suggestion(state: &mut AppState, accept: bool) {
    if state.doc_focus != DocFocus::Review {
        return;
    }
    let Some(document_id) = state.focused_doc().map(|doc| doc.document_id) else {
        return;
    };
    let Some(suggestion_id) = state.focused_suggestion().map(|s| s.id.clone()) else {
        return;
    };
    let mutation = if accept {
        DocumentMutation::AcceptSuggestion { suggestion_id }
    } else {
        DocumentMutation::RejectSuggestion { suggestion_id }
    };
    state.outbox.push(Intent::MutateDocument {
        document_id,
        mutation,
    });
}

/// Fold a merged replica update (already projected by the harness) into the
/// matching card, replacing its blocks, suggestions, and revision so the editor
/// reflects the authoritative result, then re-clamp the rail cursors.
fn apply_document_sync(
    state: &mut AppState,
    document_id: DocumentId,
    revision: String,
    blocks: Vec<DocBlockView>,
    suggestions: Vec<DocSuggestionView>,
) {
    let Some(card) = state.docs.iter_mut().find(|d| d.document_id == document_id) else {
        return;
    };
    card.revision = revision;
    card.blocks = blocks;
    card.suggestions = suggestions;
    let blocks_len = card.blocks.len();
    let suggestions_len = card.suggestions.len();
    clamp(&mut state.selected_block, blocks_len);
    clamp(&mut state.selected_suggestion, suggestions_len);
}

fn edit_prompt(state: &mut AppState, edit: impl FnOnce(&mut String)) {
    match &mut state.overlay {
        Overlay::NewRun(buf) | Overlay::Steering(buf) => edit(buf),
        Overlay::DocEdit { buffer, .. } => edit(buffer),
        Overlay::AddModelId { buffer, .. } => edit(buffer),
        // The key buffer is a redacting newtype; edit its inner String.
        Overlay::AddModelKey { buffer, .. } => edit(&mut buffer.0),
        // The key-first prompt masks a redacting newtype, like `AddModelKey`.
        Overlay::AddModelProviderKey { buffer, .. } => edit(&mut buffer.0),
        // The pick-list filters like the model picker: editing the query resets
        // the selection to the top of the new filtered set.
        Overlay::AddModelPick {
            query, selected, ..
        } => {
            edit(query);
            *selected = 0;
        }
        // Editing the palette query changes the filtered set, so the selection
        // returns to the top rather than pointing past the new results.
        Overlay::Palette { query, selected } => {
            edit(query);
            *selected = 0;
        }
        // Same shape as the palette: editing the model picker's query changes
        // the filtered set, so the selection returns to the top.
        Overlay::ModelPicker { query, selected } => {
            edit(query);
            *selected = 0;
        }
        // Same shape as the model picker (Task 8): editing the provider
        // picker's query changes the filtered set, so the selection returns
        // to the top.
        Overlay::ProviderPicker { query, selected } => {
            edit(query);
            *selected = 0;
        }
        // Same shape as the provider picker (PR C2): editing the mode
        // picker's query changes the filtered set, so the selection returns
        // to the top.
        Overlay::ModePicker { query, selected } => {
            edit(query);
            *selected = 0;
        }
        // The base view: text lands in the persistent composer draft.
        Overlay::None => edit(&mut state.composer),
        _ => {}
    }
    // Keep `selected_model` resolved to the new top-of-filter card (mirrors
    // the reset above, against the full list — see `AppState::selected_model`).
    if let Overlay::ModelPicker { query, .. } = &state.overlay {
        state.selected_model = filter_models(&state.models, query)
            .first()
            .copied()
            .unwrap_or(0);
    }
    // Same re-resolution for the provider picker (Task 8) — see
    // `AppState::selected_provider`.
    if let Overlay::ProviderPicker { query, .. } = &state.overlay {
        state.selected_provider = filter_providers(&state.providers, query)
            .first()
            .copied()
            .unwrap_or(0);
    }
}

/// A typed character. In the base view `/` on an *empty* composer opens the
/// command palette (the Codex-style slash entry); every other key extends the
/// active text buffer.
fn input_char(state: &mut AppState, c: char) {
    if c == '/' && matches!(state.overlay, Overlay::None) && state.composer.is_empty() {
        state.overlay = Overlay::Palette {
            query: String::new(),
            selected: 0,
        };
        return;
    }
    edit_prompt(state, |buf| buf.push(c));
    detach_history_on_edit(state);
}

/// Editing the composer while a recalled history entry is loaded detaches it
/// from history: `composer` becomes an ordinary in-progress draft again, so
/// the next `HistoryPrev` stashes *this* text rather than resuming the old
/// recall walk (shell-style: touching a recalled command loses its history
/// binding). A no-op for every other overlay's buffer (they have no history).
fn detach_history_on_edit(state: &mut AppState) {
    if matches!(state.overlay, Overlay::None) {
        state.history_cursor = None;
    }
}

/// `HistoryPrev` (`Up` in the composer): shell-style recall, walking
/// backward. The first press stashes the in-progress draft — so it is never
/// lost — and loads the newest entry; each subsequent press walks toward
/// older entries, saturating at the oldest. A no-op with empty history.
fn history_prev(state: &mut AppState) {
    if state.composer_history.is_empty() {
        return;
    }
    let idx = match state.history_cursor {
        None => {
            state.composer_stash = Some(state.composer.clone());
            state.composer_history.len() - 1
        }
        Some(idx) => idx.saturating_sub(1),
    };
    state.composer = state.composer_history[idx].clone();
    state.history_cursor = Some(idx);
}

/// `HistoryNext` (`Down` in the composer): walk toward newer entries; moving
/// past the newest restores the stashed in-progress draft and detaches from
/// history entirely. A no-op when not currently recalling.
fn history_next(state: &mut AppState) {
    let Some(idx) = state.history_cursor else {
        return;
    };
    if idx + 1 >= state.composer_history.len() {
        state.composer = state.composer_stash.take().unwrap_or_default();
        state.history_cursor = None;
    } else {
        let idx = idx + 1;
        state.composer = state.composer_history[idx].clone();
        state.history_cursor = Some(idx);
    }
}

/// `Esc`: clear the composer draft in the base view, return the block-edit prompt
/// to the Docs browser it opened from, or close whatever other overlay is active.
fn input_cancel(state: &mut AppState) {
    match state.overlay {
        Overlay::None => state.composer.clear(),
        // Abandoning the block-edit prompt returns to the browser, not the base
        // view (no lease was taken yet — the acquire only fires on submit).
        Overlay::DocEdit { .. } => state.overlay = Overlay::Docs,
        _ => state.overlay = Overlay::None,
    }
}

/// Switch the conversation to another run (`Ctrl-↑/↓`), clamping at the ends.
fn cycle_run(state: &mut AppState, delta: i32) {
    step(&mut state.selected_run, state.runs.len(), delta);
}

/// Set the open list overlay's `selected` to `n`, mirroring `nav`'s picker
/// resolution (keeps `selected_model`/`selected_provider` pointed at the same
/// filtered card). A no-op for a non-list overlay.
fn set_overlay_selected(state: &mut AppState, n: usize) {
    match state.overlay {
        Overlay::Palette {
            ref mut selected, ..
        }
        | Overlay::AddModelPick {
            ref mut selected, ..
        } => {
            *selected = n;
        }
        Overlay::ModelPicker {
            ref query,
            ref mut selected,
        } => {
            *selected = n;
            let indices = filter_models(&state.models, query);
            state.selected_model = indices.get(n).copied().unwrap_or(0);
        }
        Overlay::ProviderPicker {
            ref query,
            ref mut selected,
        } => {
            *selected = n;
            let indices = filter_providers(&state.providers, query);
            state.selected_provider = indices.get(n).copied().unwrap_or(0);
        }
        // The mode picker keeps no resolved `AppState` index (PR C2) — the
        // cursor alone identifies the row, exactly like the palette.
        Overlay::ModePicker {
            ref mut selected, ..
        } => {
            *selected = n;
        }
        _ => {}
    }
}

/// A click on row N: activate the open list overlay's row N (same effect as
/// selecting it + `Enter`), or — with no overlay — toggle the transcript fold
/// line at entry N of the selected run (same effect as `Enter` on that entry).
fn activate_row(state: &mut AppState, n: usize) {
    match state.overlay {
        Overlay::Palette { .. }
        | Overlay::ModelPicker { .. }
        | Overlay::ProviderPicker { .. }
        | Overlay::ModePicker { .. }
        | Overlay::AddModelPick { .. } => {
            set_overlay_selected(state, n);
            submit_prompt(state);
        }
        Overlay::None => {
            state.focus = Pane::Transcript;
            let idx = state.selected_run;
            if let Some(run) = state.runs.get_mut(idx) {
                if n < run.transcript.len() {
                    run.transcript_selected = n;
                }
            }
            expand_selected(state);
        }
        _ => {}
    }
}

fn submit_prompt(state: &mut AppState) {
    match std::mem::take(&mut state.overlay) {
        Overlay::NewRun(text) => {
            let objective = text.trim().to_owned();
            if !objective.is_empty() {
                state.outbox.push(Intent::StartRun {
                    objective,
                    mode: state.default_mode,
                    // Pin the operator's chosen model (STEP MP2). Session-default:
                    // `pending_model` is NOT cleared here, so one pick applies to
                    // this run and every subsequent one until the operator changes
                    // it in the `/model` picker.
                    model: state.pending_model.clone(),
                });
            }
        }
        Overlay::Steering(text) => {
            let text = text.trim().to_owned();
            let run_id = state.selected_run().map(|r| r.run_id);
            if let (false, Some(run_id)) = (text.is_empty(), run_id) {
                state.outbox.push(Intent::QueueSteering { run_id, text });
            }
        }
        // Submit the block-edit prompt: acquire the block's lease and queue the
        // insertion to fire once it is granted. `mem::take` left the overlay
        // `None`; restore the Docs browser so the reflected sync lands in view.
        Overlay::DocEdit { block_id, buffer } => {
            state.overlay = Overlay::Docs;
            let text = buffer.trim().to_owned();
            let document_id = state.focused_doc().map(|doc| doc.document_id);
            if let (false, Some(document_id)) = (text.is_empty(), document_id) {
                // Insert the typed text at the start of the block. A pure insertion
                // (delete_len 0) needs no knowledge of the block's current length —
                // in Edit mode it applies directly, in Suggest mode the daemon turns
                // it into a suggestion over the empty range [0,0).
                let mutation = DocumentMutation::EditText {
                    block_id: block_id.clone(),
                    position: 0,
                    delete_len: 0,
                    insert: text,
                };
                start_doc_edit(state, document_id, Some(block_id), mutation);
            }
        }
        // `mem::take` already closed the palette (left `None`); run the
        // highlighted command, which may open its own overlay.
        Overlay::Palette { query, selected } => {
            if let Some(entry) = crate::palette::filtered(&query).get(selected) {
                run_palette_command(state, entry.command);
            }
        }
        // Enter stages the focused model on `pending_model` and emits a status
        // notice. `pending_model` now PINS the model for the run(s) the operator
        // starts (STEP MP2 wired it through `Intent::StartRun` → the `StartRun`
        // command's `model` field); as a session default it applies to this run
        // and every subsequent one until changed here.
        // Re-derives the filtered list from the overlay's own `query` /
        // `selected` (mirroring the palette arm above) rather than trusting
        // `selected_model`: that field's `.unwrap_or(0)` fallback (see `nav`
        // / `edit_prompt`) points at the full list's row 0 whenever the
        // filter matches nothing, and a query with zero matches must stage
        // nothing — not silently pick a model the picker isn't even
        // showing. `mem::take` already closed the picker (left the overlay
        // `None`).
        Overlay::ModelPicker { query, selected } => {
            if let Some(&idx) = filter_models(&state.models, &query).get(selected) {
                if let Some(card) = state.models.get(idx) {
                    let id = card.id.clone();
                    state.pending_model = Some(id.clone());
                    state.notice = Some((
                        format!("model set to {id} — applies to your next run"),
                        state.tick + 25,
                    ));
                }
            }
        }
        // Enter begins the add-model flow for the focused provider — the same
        // branch `Tab` takes (model-discovery). The old `pending_provider`
        // staging + "applies to your next run" notice are removed: nothing ever
        // consumed the staged value. Re-derives the filtered selection from the
        // overlay's own `query`/`selected` (the zero-match guard the model picker
        // uses); `mem::take` already closed the picker, so `enter_add_model_flow`
        // sets the next overlay directly.
        Overlay::ProviderPicker { query, selected } => {
            if let Some(&idx) = filter_providers(&state.providers, &query).get(selected) {
                if let Some(card) = state.providers.get(idx) {
                    let provider_id = card.id.clone();
                    let requires_key = card.requires_key;
                    let can_list_models = card.can_list_models;
                    enter_add_model_flow(state, provider_id, requires_key, can_list_models);
                }
            }
        }
        // Enter sets the submission mode for the next run on `default_mode`
        // (PR C2 — plan mode) and emits a status notice. Outbound intents
        // already read `default_mode`, so a picked mode applies to the very
        // next message — no wire change. Re-derives the filtered selection
        // from the overlay's own `query`/`selected` (the zero-match guard the
        // model picker uses): a query matching nothing sets nothing.
        // `mem::take` already closed the picker.
        Overlay::ModePicker { query, selected } => {
            if let Some(&idx) = filter_modes(&query).get(selected) {
                let card = crate::state::MODE_CARDS[idx];
                state.default_mode = card.mode;
                state.notice = Some((
                    format!("mode set to {} — applies to your next run", card.label),
                    state.tick + 25,
                ));
            }
        }
        // Base view (`mem::take` left `None`): send the composer. A live run is
        // steered; a terminal run is followed up (continuing the same
        // conversation); with no run at all yet, the message starts the
        // session's first one. The draft clears either way.
        Overlay::None => {
            let text = state.composer.trim().to_owned();
            if !text.is_empty() {
                // Shell-style history: record the submission (skip a
                // consecutive duplicate) and end any in-flight recall — the
                // walk-back state from *this* submission is stale now.
                if state.composer_history.last().map(String::as_str) != Some(text.as_str()) {
                    state.composer_history.push(text.clone());
                }
                state.history_cursor = None;
                state.composer_stash = None;
                if state.selected_run_is_active() {
                    if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                        state.outbox.push(Intent::QueueSteering { run_id, text });
                    }
                } else if state.selected_run().is_some() {
                    // Task 5 (continuous-session plan): a run already exists and
                    // has reached a terminal state — this message continues the
                    // SAME session rather than starting a context-free run, so
                    // the daemon seeds the continuation from the prior turns
                    // (Tasks 1-4). The prior turn stays visible in the render
                    // (all of the session's runs, not just this one).
                    state.outbox.push(Intent::SubmitUserInput {
                        text,
                        mode: state.default_mode,
                        // Carry the current pin so a mid-conversation model
                        // switch is instant: a re-pick applies to THIS very
                        // follow-up, not just a fresh run. `None` (never pinned)
                        // inherits the session's model server-side, unchanged.
                        model: state.pending_model.clone(),
                    });
                } else {
                    // No run yet this session: nothing to continue — start one.
                    state.outbox.push(Intent::StartRun {
                        objective: text,
                        mode: state.default_mode,
                        // Carry the pinned model (STEP MP2); session-default, so
                        // it is not cleared and applies to subsequent runs too.
                        model: state.pending_model.clone(),
                    });
                }
            }
            state.composer.clear();
            // Snap the conversation back to the latest so the reply is in view.
            if let Some(run) = state.selected_run_mut() {
                run.follow = true;
            }
        }
        // Add-model free-text fallback: a captured key emits directly; otherwise
        // today's rule (hosted → masked key prompt; local → emit now). A blank
        // name reopens the prompt, carrying any captured key. `mem::take` left
        // the overlay `None`.
        Overlay::AddModelId {
            provider_id,
            requires_key,
            api_key,
            buffer,
        } => {
            let model = buffer.trim().to_owned();
            if model.is_empty() {
                state.notice = Some(("model name cannot be blank".to_owned(), state.tick + 25));
                state.overlay = Overlay::AddModelId {
                    provider_id,
                    requires_key,
                    api_key,
                    buffer: String::new(),
                };
            } else if let Some(key) = api_key {
                // A key was already captured (a can-list provider's failed query
                // fell back here). Emit directly — never re-prompt. A blank inner
                // key normalizes to `None`.
                let display_id = format!("{provider_id}/{model}");
                let inner = key.0.trim().to_owned();
                let api_key = if inner.is_empty() {
                    None
                } else {
                    Some(SecretKey(inner))
                };
                state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                state.outbox.push(Intent::AddModel {
                    display_id,
                    provider_id,
                    model,
                    api_key,
                });
            } else if requires_key {
                state.overlay = Overlay::AddModelKey {
                    provider_id,
                    model,
                    buffer: SecretKey(String::new()),
                };
            } else {
                let display_id = format!("{provider_id}/{model}");
                state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                state.outbox.push(Intent::AddModel {
                    display_id,
                    provider_id,
                    model,
                    api_key: None,
                });
            }
        }
        // Add-model flow step 3 (masked key): emit `Intent::AddModel` with the key
        // handed to the harness. An empty key emits `api_key: None`.
        Overlay::AddModelKey {
            provider_id,
            model,
            buffer,
        } => {
            let key = buffer.0.trim().to_owned();
            let display_id = format!("{provider_id}/{model}");
            let api_key = if key.is_empty() {
                None
            } else {
                Some(SecretKey(key))
            };
            state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
            state.outbox.push(Intent::AddModel {
                display_id,
                provider_id,
                model,
                api_key,
            });
        }
        // Key-first prompt (can-list hosted): emit the query with the entered key
        // (blank → no key) and open the transient "Fetching…" state, keeping the
        // key in the overlay for the round trip.
        Overlay::AddModelProviderKey {
            provider_id,
            buffer,
        } => {
            let key = buffer.0.trim().to_owned();
            let api_key = if key.is_empty() {
                None
            } else {
                Some(SecretKey(key))
            };
            state.outbox.push(Intent::QueryProviderModels {
                provider_id: provider_id.clone(),
                api_key: api_key.clone(),
            });
            state.overlay = Overlay::AddModelQuerying {
                provider_id,
                api_key,
            };
        }
        // The pick-list: resolve the filtered selection (same zero-match guard as
        // the model picker) and emit `AddModel` for the chosen name, moving the
        // stashed key into the intent.
        Overlay::AddModelPick {
            provider_id,
            api_key,
            models,
            query,
            selected,
        } => {
            if let Some(&idx) = filter_model_names(&models, &query).get(selected) {
                if let Some(model) = models.get(idx) {
                    let model = model.clone();
                    let display_id = format!("{provider_id}/{model}");
                    state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                    state.outbox.push(Intent::AddModel {
                        display_id,
                        provider_id,
                        model,
                        api_key,
                    });
                }
            }
        }
        // Nothing to submit; restore the (non-text) overlay we took.
        other => state.overlay = other,
    }
}

/// The shared add-model entry, called by both `Tab` (`begin_add_model`) and
/// `Enter` (the `ProviderPicker` submit arm). Branches on the focused provider's
/// gates (model-discovery):
/// - can-list + hosted → key-first masked prompt (the key is needed before the
///   model name exists), which on submit queries `<base_url>/models`.
/// - can-list + local/no-auth → query immediately (no key).
/// - cannot-list → today's free-text `AddModelId` flow, unchanged.
fn enter_add_model_flow(
    state: &mut AppState,
    provider_id: String,
    requires_key: bool,
    can_list_models: bool,
) {
    state.overlay = if can_list_models && requires_key {
        Overlay::AddModelProviderKey {
            provider_id,
            buffer: SecretKey(String::new()),
        }
    } else if can_list_models {
        state.outbox.push(Intent::QueryProviderModels {
            provider_id: provider_id.clone(),
            api_key: None,
        });
        Overlay::AddModelQuerying {
            provider_id,
            api_key: None,
        }
    } else {
        Overlay::AddModelId {
            provider_id,
            requires_key,
            api_key: None,
            buffer: String::new(),
        }
    };
}

/// Begin the add-model flow (`Tab` in the `/provider` picker) for the focused
/// catalog provider. A no-op outside the provider picker, or when the filtered
/// selection matches no provider (the same zero-match guard the Enter arm uses).
fn begin_add_model(state: &mut AppState) {
    let (provider_id, requires_key, can_list_models) = {
        let Overlay::ProviderPicker { query, selected } = &state.overlay else {
            return;
        };
        let Some(&idx) = filter_providers(&state.providers, query).get(*selected) else {
            return;
        };
        match state.providers.get(idx) {
            Some(card) => (card.id.clone(), card.requires_key, card.can_list_models),
            None => return,
        }
    };
    enter_add_model_flow(state, provider_id, requires_key, can_list_models);
}

/// Fold a fetched provider model list into the in-flight query overlay
/// (model-discovery). Moves the stashed `api_key` from `AddModelQuerying` into
/// the pick-list so the round-trip `Action` never carries the key. If the
/// overlay is no longer the matching `AddModelQuerying` (the user dismissed or
/// opened something else, or this is a stale result for another provider), the
/// result is ignored — the race guard.
fn on_provider_models_loaded(state: &mut AppState, provider_id: String, models: Vec<String>) {
    let matched = matches!(
        &state.overlay,
        Overlay::AddModelQuerying { provider_id: pid, .. } if *pid == provider_id
    );
    if !matched {
        return;
    }
    if let Overlay::AddModelQuerying {
        provider_id: pid,
        api_key,
    } = std::mem::replace(&mut state.overlay, Overlay::None)
    {
        state.overlay = Overlay::AddModelPick {
            provider_id: pid,
            api_key,
            models,
            query: String::new(),
            selected: 0,
        };
    }
}

/// Fold a failed model-list query into the free-text fallback (model-discovery):
/// move the stashed `api_key` from `AddModelQuerying` into `AddModelId` so a
/// hosted provider is never asked for its key twice, and surface a key-free
/// notice. Ignored (race guard) if the overlay no longer matches.
///
/// `requires_key` is derived from the provider's own catalog card, not from
/// whether this particular query happened to carry a key: a hosted provider
/// queried with a blank key still requires one on the free-text fallback, so
/// the flow re-prompts for it instead of silently adding a keyless model that
/// can only fail later at run time.
fn on_provider_models_failed(state: &mut AppState, provider_id: String, reason: String) {
    let matched = matches!(
        &state.overlay,
        Overlay::AddModelQuerying { provider_id: pid, .. } if *pid == provider_id
    );
    if !matched {
        return;
    }
    if let Overlay::AddModelQuerying {
        provider_id: pid,
        api_key,
    } = std::mem::replace(&mut state.overlay, Overlay::None)
    {
        let requires_key = state
            .providers
            .iter()
            .find(|c| c.id == provider_id)
            .map_or(api_key.is_some(), |c| c.requires_key);
        state.notice = Some((
            format!("couldn't fetch models ({reason}); type the model name"),
            state.tick + 25,
        ));
        state.overlay = Overlay::AddModelId {
            provider_id: pid,
            requires_key,
            api_key,
            buffer: String::new(),
        };
    }
}

/// Run a command chosen from the palette. Each maps onto the same effect its
/// single-key binding produces — the palette is a front door to the existing
/// commands, not a second code path. The palette overlay is already closed when
/// this runs, so a command that opens its own overlay simply sets it.
fn run_palette_command(state: &mut AppState, command: crate::palette::PaletteCommand) {
    use crate::palette::PaletteCommand;
    match command {
        PaletteCommand::NewRun => state.overlay = Overlay::NewRun(String::new()),
        PaletteCommand::Steer => begin_steering(state),
        PaletteCommand::PauseResume => pause_or_resume(state),
        PaletteCommand::Cancel => request_cancel(state),
        PaletteCommand::Skills => state.overlay = Overlay::Skills,
        PaletteCommand::Memory => state.overlay = Overlay::Memory { source_open: false },
        PaletteCommand::Docs => state.overlay = Overlay::Docs,
        PaletteCommand::Edges => state.overlay = Overlay::Edges,
        PaletteCommand::Workflow => state.overlay = Overlay::Workflow,
        PaletteCommand::Blackboard => state.overlay = Overlay::Blackboard,
        PaletteCommand::Model => {
            state.selected_model = 0;
            state.overlay = Overlay::ModelPicker {
                query: String::new(),
                selected: 0,
            };
        }
        PaletteCommand::Provider => {
            state.selected_provider = 0;
            state.overlay = Overlay::ProviderPicker {
                query: String::new(),
                selected: 0,
            };
        }
        // PR C2: open the mode picker with the cursor pre-selected on the
        // CURRENT default, so the picker's starting point reflects what the
        // next run would use.
        PaletteCommand::Mode => {
            state.overlay = Overlay::ModePicker {
                query: String::new(),
                selected: crate::state::MODE_CARDS
                    .iter()
                    .position(|card| card.mode == state.default_mode)
                    .unwrap_or(0),
            };
        }
        PaletteCommand::ToggleLayout => state.layout = state.layout.toggled(),
        PaletteCommand::Help => state.overlay = Overlay::Help,
        PaletteCommand::Detach => state.should_detach = true,
        // Task 5: a genuinely fresh conversation needs a brand-new session (see
        // `AppState::start_new_conversation`'s doc comment), which this pure
        // reducer cannot mint — so this detaches exactly like
        // `PaletteCommand::Detach` (the current run, if any, is unaffected) and
        // additionally flags the harness to forget this repository's
        // remembered session before it tears down the connection.
        PaletteCommand::NewConversation => {
            state.should_detach = true;
            state.start_new_conversation = true;
        }
    }
}

fn is_terminal(rs: RunState) -> bool {
    matches!(
        rs,
        RunState::Completed | RunState::Failed | RunState::Cancelled
    )
}

/// Move an index within `[0, len)` by `delta`, clamping at the ends.
fn step(index: &mut usize, len: usize, delta: i32) {
    if len == 0 {
        *index = 0;
        return;
    }
    let max = len - 1;
    if delta < 0 {
        *index = index.saturating_sub(1);
    } else {
        *index = (*index + 1).min(max);
    }
}

/// Clamp an index to be a valid selection for a list of `len` items.
fn clamp(index: &mut usize, len: usize) {
    if len == 0 {
        *index = 0;
    } else if *index >= len {
        *index = len - 1;
    }
}

// A convenience the render layer and tests reuse: a human label for a proposed
// action's requested capability. Kept next to the reducer because it mirrors the
// event → state mapping.
#[must_use]
pub(crate) fn capability_label(action: &ProposedAction) -> String {
    match action {
        ProposedAction::ReadFiles { paths } => format!("FileRead ({} path(s))", paths.len()),
        ProposedAction::WritePatch { .. } => "FileWrite (apply patch)".to_owned(),
        ProposedAction::ExecuteCommand { program, .. } => format!("CommandExecute ({program})"),
        ProposedAction::NetworkRequest { destination } => format!("NetworkConnect ({destination})"),
        ProposedAction::GitCommit { repository } => format!("GitCommit ({repository})"),
        ProposedAction::GitPush { remote, branch } => format!("GitPush ({remote} {branch})"),
        ProposedAction::PublishDocument { target, .. } => format!("GitCommit ({target})"),
        ProposedAction::McpToolCall { server, tool, .. } => {
            format!("McpToolCall ({server}.{tool})")
        }
        _ => "unsupported capability".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use codypendent_protocol::{
        AgentMode, ApprovalId, ArtifactId, ArtifactRef, ChangeSetId, DataClassification, ModelId,
        Risk, RiskLevel, RunId, ToolOutcome,
    };

    fn agent_actor(run_id: RunId) -> Actor {
        Actor::Agent {
            agent_id: codypendent_protocol::AgentId::new(),
            run_id,
            model: ModelId("gpt-5.1-codex".to_owned()),
        }
    }

    fn ev(actor: Actor, body: EventBody) -> Action {
        Action::daemon_event(SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor,
            body,
        })
    }

    fn system_ev(body: EventBody) -> Action {
        ev(Actor::System, body)
    }

    fn artifact() -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(),
            media_type: "text/x-diff".to_owned(),
            byte_length: 10,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        }
    }

    #[test]
    fn run_started_then_state_changed_updates_run_state() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "diagnose".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        assert_eq!(s.runs.len(), 1);
        assert_eq!(s.runs[0].state, RunState::Preparing);
        assert_eq!(s.runs[0].objective, "diagnose");

        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        assert_eq!(s.runs[0].state, RunState::Running);
    }

    #[test]
    fn run_started_pushes_a_user_turn_with_the_objective() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "add a test".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        assert!(matches!(
            &s.runs[0].transcript[0],
            TranscriptEntry::User { text } if text == "add a test"
        ));
    }

    /// C13: every transcript-pushing reducer arm routes through `push_entry`,
    /// so a run's transcript is bounded by `MAX_TRANSCRIPT_ENTRIES` regardless
    /// of how many events arrive. The arms the fix converted from a direct
    /// `transcript.push` — tool-started, tool-completed, steering, budget — are
    /// each flooded past the cap here; a regression to a direct push (skipping
    /// the trim) would let the transcript grow without bound.
    #[test]
    fn transcript_entries_respect_the_cap_in_every_formerly_direct_arm() {
        let cap = crate::state::MAX_TRANSCRIPT_ENTRIES;
        let over = cap + 37;

        // Flood one arm with `over` events that each push a fresh transcript
        // entry, and return the resulting transcript length.
        let flood = |make: &dyn Fn(RunId, usize) -> Action| -> usize {
            let mut s = AppState::new();
            let run_id = RunId::new();
            reduce(
                &mut s,
                system_ev(EventBody::RunStarted {
                    run_id,
                    objective: "diagnose".to_owned(),
                    mode: AgentMode::Build,
                }),
            );
            for i in 0..over {
                reduce(&mut s, make(run_id, i));
            }
            s.runs
                .iter()
                .find(|r| r.run_id == run_id)
                .unwrap()
                .transcript
                .len()
        };

        // tool-started with no preceding proposed card → the None (push) branch.
        let tool_started = flood(&|run_id, i| {
            ev(
                agent_actor(run_id),
                EventBody::ToolStarted {
                    run_id,
                    tool: format!("tool.{i}"),
                    args_digest: format!("d{i}"),
                    label: None,
                },
            )
        });
        // tool-completed with no non-completed card → the None (push) branch.
        let tool_completed = flood(&|run_id, i| {
            ev(
                agent_actor(run_id),
                EventBody::ToolCompleted {
                    run_id,
                    tool: format!("tool.{i}"),
                    outcome: ToolOutcome::Succeeded,
                    artifact: None,
                },
            )
        });
        // steering queued → a fresh (unapplied) Steering entry each time.
        let steering =
            flood(&|run_id, _i| ev(agent_actor(run_id), EventBody::SteeringQueued { run_id }));
        // budget warning → a fresh Budget entry each time.
        let budget = flood(&|run_id, i| {
            ev(
                agent_actor(run_id),
                EventBody::BudgetWarning {
                    run_id,
                    dimension: BudgetDimension::Cost,
                    used: i as u64,
                    limit: 100,
                },
            )
        });

        for (arm, len) in [
            ("tool-started", tool_started),
            ("tool-completed", tool_completed),
            ("steering", steering),
            ("budget", budget),
        ] {
            assert_eq!(len, cap, "{arm}: transcript must be trimmed to the cap");
        }
    }

    fn note_count(s: &AppState, run_id: RunId) -> usize {
        s.runs
            .iter()
            .find(|r| r.run_id == run_id)
            .map(|r| {
                r.transcript
                    .iter()
                    .filter(|e| matches!(e, TranscriptEntry::Note { .. }))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn a_run_scoped_note_lands_on_its_run_not_the_selected_one() {
        // Two runs; `ensure_run` selects the most-recently-started, so B is
        // focused. This is exactly the interleaving that misrouted run-scoped
        // notes before issue #6 item 3.
        let mut s = AppState::new();
        let run_a = RunId::new();
        let run_b = RunId::new();
        for (run_id, objective) in [(run_a, "a"), (run_b, "b")] {
            reduce(
                &mut s,
                system_ev(EventBody::RunStarted {
                    run_id,
                    objective: objective.to_owned(),
                    mode: AgentMode::Build,
                }),
            );
        }
        assert_eq!(
            s.selected_run().map(|r| r.run_id),
            Some(run_b),
            "B is the selected run"
        );

        // A run-scoped note for A must attach to A even though B is selected.
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "context for A".to_owned(),
                run_id: Some(run_a),
            }),
        );
        assert_eq!(note_count(&s, run_a), 1, "A's note landed on A");
        assert_eq!(note_count(&s, run_b), 0, "B did not receive A's note");

        // A session-level note (no run_id) still attaches to the focused run.
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "session note".to_owned(),
                run_id: None,
            }),
        );
        assert_eq!(
            note_count(&s, run_b),
            1,
            "session note went to the selected run"
        );
        assert_eq!(
            note_count(&s, run_a),
            1,
            "A is unchanged by the session note"
        );
    }

    #[test]
    fn a_long_note_folds_by_default_and_expand_toggles_it() {
        // Mirrors the ToolCard/Patch fold pattern (Chapter 07 transcript
        // declutter fix): a NoteAppended folds into Note{expanded:false}
        // regardless of length, and the same Action::Expand that toggles a
        // tool card or patch also toggles a selected note.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let long_text = "line one\nline two\nline three\nline four".to_owned();
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: long_text.clone(),
                run_id: Some(run_id),
            }),
        );
        // transcript[0] is the User turn RunStarted pushes for the objective;
        // the note folds in right after it.
        let TranscriptEntry::Note { text, expanded } = &s.runs[0].transcript[1] else {
            unreachable!("NoteAppended must fold into a Note entry")
        };
        assert_eq!(text, &long_text);
        assert!(!expanded, "a note starts folded, same as a fresh tool card");

        s.focus = Pane::Transcript;
        s.runs[0].transcript_selected = 1;
        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!()
        };
        assert!(*expanded, "Expand toggles a selected note's expanded state");

        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!()
        };
        assert!(!*expanded, "Expand toggles it back off");
    }

    #[test]
    fn a_short_note_folds_the_same_way_as_a_long_one() {
        // `reduce` does not special-case note length — every NoteAppended folds
        // into Note{expanded:false} identically. "A short note stays inline" is
        // purely a render-layer decision (see render.rs's note_lines), not a
        // different shape here; Expand still flips this note's state too.
        // (Not a `remembered:`/`=== CONTEXT` note — those fold into
        // `Backstage` instead, covered by the backstage-fold tests below.)
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );
        // transcript[0] is the User turn RunStarted pushes for the objective;
        // the note folds in right after it.
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!("NoteAppended must fold into a Note entry")
        };
        assert!(!expanded, "every note starts unexpanded, short or long");

        s.focus = Pane::Transcript;
        s.runs[0].transcript_selected = 1;
        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!()
        };
        assert!(*expanded, "Expand flips it regardless of length");
    }

    #[test]
    fn context_and_memory_notes_fold_into_backstage_not_visible_notes() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\nline\nline\nline".to_owned(),
                run_id: Some(run_id),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "remembered: the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );
        // No visible Note cells; exactly one Backstage entry with the right counts.
        assert!(
            !s.runs[0]
                .transcript
                .iter()
                .any(|e| matches!(e, TranscriptEntry::Note { .. })),
            "context/memory notes must never create a visible Note cell"
        );
        let bs = s.runs[0].transcript.iter().find_map(|e| match e {
            TranscriptEntry::Backstage {
                context_lines,
                memory_updates,
                ..
            } => Some((*context_lines, *memory_updates)),
            _ => None,
        });
        assert_eq!(bs, Some((Some(4), 1)));
    }

    #[test]
    fn an_ordinary_note_still_renders_as_a_note_cell() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "a plain observation".to_owned(),
                run_id: Some(run_id),
            }),
        );
        assert!(s.runs[0]
            .transcript
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Note { .. })));
    }

    #[test]
    fn expand_toggles_a_selected_backstage_entry() {
        // Mirrors the Note/Tool/Patch expand pattern: the same Action::Expand
        // that toggles a selected note also toggles a selected Backstage entry.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "remembered: the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );
        let idx = s.runs[0]
            .transcript
            .iter()
            .position(|e| matches!(e, TranscriptEntry::Backstage { .. }))
            .expect("a Backstage entry was folded in");

        s.focus = Pane::Transcript;
        s.runs[0].transcript_selected = idx;
        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Backstage { expanded, .. } = &s.runs[0].transcript[idx] else {
            unreachable!()
        };
        assert!(*expanded, "Expand opens the selected Backstage entry");

        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Backstage { expanded, .. } = &s.runs[0].transcript[idx] else {
            unreachable!()
        };
        assert!(!*expanded, "Expand toggles it back off");
    }

    #[test]
    fn expand_toggles_a_selected_completed_entry() {
        // Task 3: the same Action::Expand that toggles a selected Backstage
        // entry also toggles a failed run's `Completed` entry, revealing the
        // full raw error chain beneath the concise summary (render.rs).
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Failed {
                    reason: "boom".to_owned(),
                },
                chronicle: artifact(),
            }),
        );
        let idx = s.runs[0]
            .transcript
            .iter()
            .position(|e| matches!(e, TranscriptEntry::Completed { .. }))
            .expect("a Completed entry was folded in");

        s.focus = Pane::Transcript;
        s.runs[0].transcript_selected = idx;
        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Completed { expanded, .. } = &s.runs[0].transcript[idx] else {
            unreachable!()
        };
        assert!(*expanded, "Expand opens the selected Completed entry");

        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Completed { expanded, .. } = &s.runs[0].transcript[idx] else {
            unreachable!()
        };
        assert!(!*expanded, "Expand toggles it back off");
    }

    #[test]
    fn catchup_snapshot_seeds_title_and_run_stubs() {
        // A too-far-behind reopen folds the projection, not events: the title and
        // a stub per active run so the session is not blank.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            Action::CatchupSnapshot {
                title: "long session".to_owned(),
                closed: false,
                runs: vec![run_id],
            },
        );
        assert_eq!(s.session_title.as_deref(), Some("long session"));
        assert!(!s.session_closed);
        assert_eq!(s.runs.len(), 1);
        assert_eq!(s.runs[0].run_id, run_id);
    }

    #[test]
    fn model_stream_deltas_coalesce_and_learn_model() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            ev(
                agent_actor(run_id),
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "Hello, ".to_owned(),
                },
            ),
        );
        reduce(
            &mut s,
            ev(
                agent_actor(run_id),
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "world".to_owned(),
                },
            ),
        );
        // Two deltas coalesce into one transcript entry, right after the User
        // turn RunStarted pushes for the objective.
        assert_eq!(s.runs[0].transcript.len(), 2);
        match &s.runs[0].transcript[1] {
            TranscriptEntry::Model { text, .. } => assert_eq!(text, "Hello, world"),
            other => panic!("expected coalesced Model entry, got {other:?}"),
        }
        // The serving model was learned from the agent actor.
        assert_eq!(s.runs[0].model, Some(ModelId("gpt-5.1-codex".to_owned())));
    }

    #[test]
    fn approval_requested_adds_and_resolved_removes() {
        let mut s = AppState::new();
        let approval_id = ApprovalId::new();
        reduce(
            &mut s,
            system_ev(EventBody::ApprovalRequested {
                approval_id,
                action: ProposedAction::ExecuteCommand {
                    program: "cargo".to_owned(),
                    args: vec!["test".to_owned()],
                    environment: Vec::new(),
                    cwd: None,
                },
                risk: Risk {
                    level: RiskLevel::Medium,
                    reasons: vec!["runs a command".to_owned()],
                },
            }),
        );
        assert_eq!(s.pending_approvals.len(), 1);
        assert!(s.show_approval_modal());

        reduce(
            &mut s,
            system_ev(EventBody::ApprovalResolved {
                approval_id,
                decision: ApprovalDecision::Approve,
            }),
        );
        assert!(s.pending_approvals.is_empty());
        assert!(!s.show_approval_modal());
    }

    /// PR B (MCP client): an `McpToolCall` gets its own one-line summary —
    /// `McpToolCall (server.tool)` — never the wildcard "unsupported
    /// capability" fallback.
    #[test]
    fn mcp_tool_call_capability_label_names_server_and_tool() {
        let action = ProposedAction::McpToolCall {
            server: "github".to_owned(),
            tool: "create_issue".to_owned(),
            summary: "create an issue".to_owned(),
            args: "{\"title\":\"bug\"}".to_owned(),
        };
        assert_eq!(
            capability_label(&action),
            "McpToolCall (github.create_issue)"
        );
    }

    #[test]
    fn approval_keys_are_inert_while_an_overlay_hides_the_card() {
        let mut s = AppState::new();
        // A browser overlay is open when the approval arrives: the modal is
        // covered (it renders only with no overlay), yet `a`/`r` are live
        // Normal-mode keys — they must not resolve a card the user cannot see.
        reduce(&mut s, Action::OpenSkills);
        let approval_id = ApprovalId::new();
        reduce(
            &mut s,
            system_ev(EventBody::ApprovalRequested {
                approval_id,
                action: ProposedAction::ExecuteCommand {
                    program: "cargo".to_owned(),
                    args: vec!["test".to_owned()],
                    environment: Vec::new(),
                    cwd: None,
                },
                risk: Risk {
                    level: RiskLevel::Medium,
                    reasons: vec!["runs a command".to_owned()],
                },
            }),
        );
        assert!(!s.show_approval_modal(), "overlay covers the modal");

        reduce(&mut s, Action::Approve(ApprovalScope::Once));
        reduce(&mut s, Action::Reject);
        assert!(
            s.drain_outbox().is_empty(),
            "no decision may fire while the card is hidden"
        );
        assert_eq!(s.pending_approvals.len(), 1);

        // Dismissing the overlay reveals the card and re-arms the keys.
        reduce(&mut s, Action::Dismiss);
        assert!(s.show_approval_modal());
        reduce(&mut s, Action::Approve(ApprovalScope::Once));
        let intents = s.drain_outbox();
        assert!(
            matches!(intents.as_slice(), [Intent::ResolveApproval { .. }]),
            "the visible card resolves normally, got {intents:?}"
        );
    }

    #[test]
    fn run_started_does_not_steal_selection_mid_draft() {
        let mut s = AppState::new();
        let mine = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id: mine,
                objective: "mine".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        assert_eq!(s.selected_run, 0);

        // A draft is in progress: another client's RunStarted (shared session)
        // must not move the selection — Enter submits against `selected_run`,
        // so a steal here would retarget the message being composed.
        reduce(&mut s, Action::InputChar('h'));
        let theirs = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id: theirs,
                objective: "theirs".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        assert_eq!(s.runs.len(), 2);
        assert_eq!(s.selected_run, 0, "a mid-draft selection must not move");

        // With an empty composer a new run takes focus (follow the action) —
        // this is also what keeps our own submits selected, since submitting
        // clears the composer before its RunStarted folds back.
        s.composer.clear();
        let third = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id: third,
                objective: "next".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        assert_eq!(s.selected_run, 2);
    }

    #[test]
    fn tool_lifecycle_folds_into_one_card() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        let approval_id = ApprovalId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolProposed {
                run_id,
                approval_id,
                action: ProposedAction::ExecuteCommand {
                    program: "cargo".to_owned(),
                    args: vec!["test".to_owned()],
                    environment: Vec::new(),
                    cwd: None,
                },
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc".to_owned(),
                label: Some("cargo test".to_owned()),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "shell.run".to_owned(),
                outcome: ToolOutcome::Succeeded,
                artifact: Some(artifact()),
            }),
        );
        // Proposed → Started → Completed collapses to a single card.
        let tools: Vec<_> = s.runs[0]
            .transcript
            .iter()
            .filter(|e| matches!(e, TranscriptEntry::Tool(_)))
            .collect();
        assert_eq!(tools.len(), 1);
        let TranscriptEntry::Tool(card) = tools[0] else {
            unreachable!()
        };
        assert_eq!(card.tool, "shell.run");
        // `ToolStarted.label` (STARTED, not PROPOSED or COMPLETED — neither
        // carries a label) lands on the already-Proposed card unchanged
        // through completion.
        assert_eq!(card.label, Some("cargo test".to_owned()));
        assert_eq!(card.status, ToolStatus::Completed);
        assert_eq!(card.outcome, Some(ToolOutcome::Succeeded));
        assert!(card.artifact.is_some());
    }

    #[test]
    fn budget_warning_projects_context_and_cost() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::BudgetWarning {
                run_id,
                dimension: BudgetDimension::Tokens,
                used: 90_000,
                limit: 100_000,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::BudgetWarning {
                run_id,
                dimension: BudgetDimension::Cost,
                used: 125,
                limit: 500,
            }),
        );
        assert_eq!(s.runs[0].context_percent, Some(90));
        assert_eq!(s.runs[0].cost_minor, Some(125));
        let status = s.status();
        assert_eq!(status.context_percent, Some(90));
        assert_eq!(status.cost_minor, Some(125));
        assert_eq!(status.mode, Some(AgentMode::Build));
    }

    #[test]
    fn budget_warning_tokens_brings_the_dead_context_footer_alive() {
        // Context-window protection (BT5): the plain (non-workflow) loop's
        // new `BudgetWarning{Tokens}` emitter (BT3) must drive the exact same
        // `context_percent` projection the workflow budget engine already
        // did — proving this reducer arm (`reduce.rs:535-546`) needs zero
        // change to bring the footer alive for normal chat.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        // Honesty: before any `BudgetWarning{Tokens}` event lands, the
        // footer's source field must stay unknown — never a fabricated
        // percent.
        assert_eq!(s.runs[0].context_percent, None);

        reduce(
            &mut s,
            system_ev(EventBody::BudgetWarning {
                run_id,
                dimension: BudgetDimension::Tokens,
                used: 8_192,
                limit: 32_768,
            }),
        );
        assert_eq!(s.runs[0].context_percent, Some(25));
    }

    #[test]
    fn run_completed_sets_terminal_state_and_disposition() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Failed {
                    reason: "boom".to_owned(),
                },
                chronicle: artifact(),
            }),
        );
        assert_eq!(s.runs[0].state, RunState::Failed);
        assert!(matches!(
            s.runs[0].disposition,
            Some(RunDisposition::Failed { .. })
        ));
    }

    /// Task 3: `RunActivity` is derived purely from folding run-state,
    /// streaming, and tool-lifecycle events — never fetched. Walks a run
    /// through every transition the reducer owns: `Running` ⇒ `Thinking`, a
    /// model delta ⇒ `Streaming`, a tool starting ⇒ `RunningTool(name)`, that
    /// tool completing ⇒ back to `Thinking`, and the terminal `RunCompleted`
    /// ⇒ `Idle`.
    #[test]
    fn run_activity_tracks_thinking_streaming_tool_and_idle() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        assert_eq!(s.runs[0].activity, RunActivity::Thinking);

        reduce(
            &mut s,
            ev(
                agent_actor(run_id),
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "hi".to_owned(),
                },
            ),
        );
        assert_eq!(s.runs[0].activity, RunActivity::Streaming);

        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc".to_owned(),
                label: None,
            }),
        );
        assert_eq!(
            s.runs[0].activity,
            RunActivity::RunningTool("shell.run".to_owned())
        );

        reduce(
            &mut s,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "shell.run".to_owned(),
                outcome: ToolOutcome::Succeeded,
                artifact: None,
            }),
        );
        assert_eq!(s.runs[0].activity, RunActivity::Thinking);

        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_owned()),
                },
                chronicle: artifact(),
            }),
        );
        assert_eq!(s.runs[0].activity, RunActivity::Idle);
    }

    #[test]
    fn approve_emits_resolve_intent_but_does_not_remove_locally() {
        let mut s = AppState::new();
        let approval_id = ApprovalId::new();
        reduce(
            &mut s,
            system_ev(EventBody::ApprovalRequested {
                approval_id,
                action: ProposedAction::GitCommit {
                    repository: "acme/widget".to_owned(),
                },
                risk: Risk {
                    level: RiskLevel::High,
                    reasons: vec![],
                },
            }),
        );
        reduce(&mut s, Action::Approve(ApprovalScope::Run));
        // Intent queued for the CLI; state unchanged until the daemon confirms.
        assert_eq!(s.pending_approvals.len(), 1);
        let intents = s.drain_outbox();
        assert_eq!(intents.len(), 1);
        match &intents[0] {
            Intent::ResolveApproval {
                approval_id: id,
                decision,
                scope,
            } => {
                assert_eq!(*id, approval_id);
                assert_eq!(*decision, ApprovalDecision::Approve);
                assert_eq!(*scope, ApprovalScope::Run);
            }
            other => panic!("expected ResolveApproval, got {other:?}"),
        }
        assert!(s.outbox.is_empty(), "outbox drained");
    }

    #[test]
    fn new_run_prompt_submits_start_run_intent() {
        let mut s = AppState::new();
        reduce(&mut s, Action::NewRun);
        assert_eq!(s.input_mode(), crate::state::InputMode::Editing);
        for c in "fix the test".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert!(matches!(s.overlay, Overlay::None));
        let intents = s.drain_outbox();
        assert_eq!(
            intents,
            vec![Intent::StartRun {
                objective: "fix the test".to_owned(),
                mode: AgentMode::Build,
                // No model was staged, so the run carries no pin.
                model: None,
            }]
        );
    }

    #[test]
    fn starting_a_run_after_staging_a_model_carries_the_pin() {
        // STEP MP2: a model picked in the `/model` popup pins the model for the
        // run the operator then starts — the staged `pending_model` flows into
        // the `StartRun` intent. Session-default: the pin also survives on
        // `pending_model` for subsequent runs (it is not cleared on submit).
        let mut s = AppState::new();
        s.models = vec![
            model_card("local-qwen", "openai-compatible"),
            model_card("hosted-gpt", "openai-compatible"),
        ];
        open_model_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // focus "hosted-gpt"
        reduce(&mut s, Action::InputSubmit); // stage it on pending_model
        assert_eq!(s.pending_model, Some(ModelId("hosted-gpt".to_owned())));

        // Start a run via the NewRun overlay.
        reduce(&mut s, Action::NewRun);
        for c in "fix the test".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        assert_eq!(
            s.drain_outbox(),
            vec![Intent::StartRun {
                objective: "fix the test".to_owned(),
                mode: AgentMode::Build,
                model: Some(ModelId("hosted-gpt".to_owned())),
            }],
            "the staged model pins the started run"
        );
        assert_eq!(
            s.pending_model,
            Some(ModelId("hosted-gpt".to_owned())),
            "session-default: the pin persists for subsequent runs"
        );
    }

    #[test]
    fn cancel_requires_confirmation_then_emits_intent() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        reduce(&mut s, Action::Cancel);
        assert!(matches!(s.overlay, Overlay::ConfirmCancel));
        assert!(s.outbox.is_empty(), "no cancel until confirmed");
        reduce(&mut s, Action::ConfirmCancel);
        assert!(matches!(s.overlay, Overlay::None));
        assert_eq!(s.drain_outbox(), vec![Intent::CancelRun { run_id }]);
    }

    #[test]
    fn pause_toggles_between_pause_and_resume() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        reduce(&mut s, Action::Pause);
        assert_eq!(s.drain_outbox(), vec![Intent::PauseRun { run_id }]);
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Paused,
            }),
        );
        reduce(&mut s, Action::Pause);
        assert_eq!(s.drain_outbox(), vec![Intent::ResumeRun { run_id }]);
    }

    #[test]
    fn unknown_event_renders_placeholder_not_crash() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(&mut s, system_ev(EventBody::Unknown));
        assert!(s.runs[0]
            .transcript
            .iter()
            .any(|e| matches!(e, TranscriptEntry::Unsupported { .. })));
    }

    fn skill(name: &str, permissions: &[&str]) -> crate::state::SkillCard {
        crate::state::SkillCard {
            name: name.to_owned(),
            kind: "skill".to_owned(),
            scope: "repository".to_owned(),
            trust: "first-party".to_owned(),
            status: "active".to_owned(),
            risk: "medium".to_owned(),
            description: "a test skill".to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn memory(statement: &str, source: &str) -> crate::state::MemoryCard {
        crate::state::MemoryCard {
            statement: statement.to_owned(),
            class: "semantic".to_owned(),
            scope: "repository".to_owned(),
            revision: "79acbf1".to_owned(),
            observed: "2026-07-14".to_owned(),
            confidence: 1.0,
            source: source.to_owned(),
        }
    }

    #[test]
    fn open_skills_toggles_the_studio_overlay() {
        let mut s = AppState::new();
        s.skills = vec![skill("rust.fix-ci", &["command: cargo"])];
        reduce(&mut s, Action::OpenSkills);
        assert_eq!(s.overlay, Overlay::Skills);
        assert_eq!(s.input_mode(), crate::state::InputMode::Normal);
        // Toggling closes it again.
        reduce(&mut s, Action::OpenSkills);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn open_memory_toggles_the_memory_overlay() {
        let mut s = AppState::new();
        s.memories = vec![memory(
            "tests use cargo nextest",
            "events 3..7 of session x",
        )];
        reduce(&mut s, Action::OpenMemory);
        assert_eq!(s.overlay, Overlay::Memory { source_open: false });
        reduce(&mut s, Action::OpenMemory);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn skill_navigation_moves_selection_within_the_studio() {
        let mut s = AppState::new();
        s.skills = vec![
            skill("a", &["command: cargo"]),
            skill("b", &["filesystem_read: $REPOSITORY"]),
        ];
        reduce(&mut s, Action::OpenSkills);
        assert_eq!(s.selected_skill, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_skill, 1);
        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_skill, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_skill, 0);
    }

    #[test]
    fn memory_navigation_moves_selection_and_collapses_source() {
        let mut s = AppState::new();
        s.memories = vec![memory("m0", "src0"), memory("m1", "src1")];
        reduce(&mut s, Action::OpenMemory);
        // Open the first memory's source, then navigate: the source collapses.
        reduce(&mut s, Action::OpenSource);
        assert_eq!(s.overlay, Overlay::Memory { source_open: true });
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_memory, 1);
        assert_eq!(s.overlay, Overlay::Memory { source_open: false });
    }

    #[test]
    fn open_source_reveals_the_focused_memory_source() {
        let mut s = AppState::new();
        s.memories = vec![memory(
            "tests use cargo nextest",
            "artifact abc (rust-toolchain.toml)",
        )];
        reduce(&mut s, Action::OpenMemory);
        assert_eq!(s.overlay, Overlay::Memory { source_open: false });
        // Both the explicit key and Enter open the source.
        reduce(&mut s, Action::OpenSource);
        assert_eq!(s.overlay, Overlay::Memory { source_open: true });
        // Re-open the browser and use Enter (Expand) this time.
        reduce(&mut s, Action::OpenMemory); // close
        reduce(&mut s, Action::OpenMemory); // reopen, source collapsed
        assert_eq!(s.overlay, Overlay::Memory { source_open: false });
        reduce(&mut s, Action::Expand);
        assert_eq!(s.overlay, Overlay::Memory { source_open: true });
    }

    #[test]
    fn open_source_is_inert_without_the_memory_overlay() {
        let mut s = AppState::new();
        s.memories = vec![memory("m", "src")];
        // No overlay open: opening a source does nothing.
        reduce(&mut s, Action::OpenSource);
        assert_eq!(s.overlay, Overlay::None);
    }

    fn doc(title: &str) -> crate::state::DocCard {
        crate::state::DocCard {
            document_id: codypendent_protocol::DocumentId::new(),
            title: title.to_owned(),
            scope: "organization".to_owned(),
            status: "draft".to_owned(),
            mode: "suggest".to_owned(),
            revision: "r3".to_owned(),
            blocks: vec![crate::state::DocBlockView {
                id: "b1".to_owned(),
                kind: "heading".to_owned(),
                text: title.to_owned(),
            }],
            suggestions: vec![crate::state::DocSuggestionView {
                id: "s1".to_owned(),
                status: "pending".to_owned(),
                author: "agent".to_owned(),
                range: "0..4".to_owned(),
                replacement: "new".to_owned(),
                rationale: Some("clearer".to_owned()),
            }],
        }
    }

    fn edge(from: &str, to: &str) -> crate::state::GraphEdgeCard {
        crate::state::GraphEdgeCard {
            from: from.to_owned(),
            to: to.to_owned(),
            relation: "calls".to_owned(),
            confidence: 0.45,
            evidence_kind: "syntax_inferred".to_owned(),
            evidence: "artifact abc (src/lib.rs)".to_owned(),
            revision: "79acbf1".to_owned(),
        }
    }

    #[test]
    fn open_docs_toggles_the_docs_overlay() {
        let mut s = AppState::new();
        s.docs = vec![doc("Payments guide")];
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.overlay, Overlay::Docs);
        assert_eq!(s.input_mode(), crate::state::InputMode::Normal);
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn open_edges_toggles_the_edge_inspector() {
        let mut s = AppState::new();
        s.edges = vec![edge("a::f", "b::g")];
        reduce(&mut s, Action::OpenEdges);
        assert_eq!(s.overlay, Overlay::Edges);
        assert_eq!(s.input_mode(), crate::state::InputMode::Normal);
        reduce(&mut s, Action::OpenEdges);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn docs_navigation_moves_selection_within_the_tree() {
        let mut s = AppState::new();
        s.docs = vec![doc("a"), doc("b")];
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.selected_doc, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_doc, 1);
        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_doc, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_doc, 0);
    }

    // --- Docs Studio live editing (Phase 4 STEP 4.3 client wiring) ---

    /// Open the Docs browser focused on the review rail (Tree → Editor → Review).
    fn docs_on_review(docs: Vec<crate::state::DocCard>) -> AppState {
        let mut s = AppState::new();
        s.docs = docs;
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane); // Editor
        reduce(&mut s, Action::CyclePane); // Review
        s
    }

    #[test]
    fn tab_cycles_the_docs_rail_focus() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.doc_focus, DocFocus::Tree);
        reduce(&mut s, Action::CyclePane);
        assert_eq!(s.doc_focus, DocFocus::Editor);
        reduce(&mut s, Action::CyclePane);
        assert_eq!(s.doc_focus, DocFocus::Review);
        reduce(&mut s, Action::CyclePane);
        assert_eq!(s.doc_focus, DocFocus::Tree);
    }

    #[test]
    fn docs_editor_rail_nav_moves_the_block_cursor_not_the_tree() {
        let mut s = AppState::new();
        let mut card = doc("a");
        card.blocks.push(crate::state::DocBlockView {
            id: "b2".to_owned(),
            kind: "paragraph".to_owned(),
            text: "second".to_owned(),
        });
        s.docs = vec![card, doc("b")];
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane); // Editor rail
        assert_eq!(s.selected_doc, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_block, 1, "the block cursor moves");
        assert_eq!(s.selected_doc, 0, "the tree selection stays put");
    }

    #[test]
    fn edit_doc_opens_the_block_edit_prompt_for_the_focused_block() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane); // Editor rail
        reduce(&mut s, Action::EditDoc);
        match &s.overlay {
            Overlay::DocEdit { block_id, buffer } => {
                assert_eq!(block_id, "b1");
                assert!(buffer.is_empty());
            }
            other => panic!("expected the block-edit prompt, got {other:?}"),
        }
        // Outside the editor rail, `e` is inert.
        let mut t = AppState::new();
        t.docs = vec![doc("a")];
        reduce(&mut t, Action::OpenDocs); // Tree focus
        reduce(&mut t, Action::EditDoc);
        assert_eq!(t.overlay, Overlay::Docs);
    }

    #[test]
    fn submitting_a_block_edit_acquires_the_lease_and_queues_the_mutation() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane); // Editor rail
        reduce(&mut s, Action::EditDoc);
        for c in "hi".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        // The prompt closed back to the browser and a lease was requested.
        assert_eq!(s.overlay, Overlay::Docs);
        assert_eq!(
            s.outbox,
            vec![Intent::AcquireDocumentLease {
                document_id,
                block_id: Some("b1".to_owned()),
            }]
        );
        // The mutation is queued (not yet sent) and the lease is being acquired.
        let edit = s.doc_edit.as_ref().expect("an edit is in flight");
        assert_eq!(edit.lease, DocLeaseState::Acquiring);
        assert_eq!(
            edit.pending,
            Some(DocumentMutation::EditText {
                block_id: "b1".to_owned(),
                position: 0,
                delete_len: 0,
                insert: "hi".to_owned(),
            })
        );
    }

    #[test]
    fn an_empty_block_edit_submits_nothing() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane);
        reduce(&mut s, Action::EditDoc);
        reduce(&mut s, Action::InputSubmit); // empty buffer
        assert_eq!(s.overlay, Overlay::Docs);
        assert!(s.outbox.is_empty());
        assert!(s.doc_edit.is_none());
    }

    #[test]
    fn a_lease_grant_marks_held_and_fires_the_queued_mutation() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane);
        reduce(&mut s, Action::EditDoc);
        reduce(&mut s, Action::InputChar('x'));
        reduce(&mut s, Action::InputSubmit);
        let _ = s.drain_outbox(); // the AcquireDocumentLease intent

        reduce(
            &mut s,
            Action::DocumentLeaseGranted {
                document_id,
                lease_id: "lease-1".to_owned(),
            },
        );

        let edit = s.doc_edit.as_ref().expect("still tracking the edit");
        assert_eq!(edit.lease, DocLeaseState::Held);
        assert_eq!(edit.lease_id.as_deref(), Some("lease-1"));
        assert!(edit.pending.is_none(), "the queued mutation was fired");
        assert_eq!(
            s.outbox,
            vec![Intent::MutateDocument {
                document_id,
                mutation: DocumentMutation::EditText {
                    block_id: "b1".to_owned(),
                    position: 0,
                    delete_len: 0,
                    insert: "x".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn a_lease_rejection_blocks_the_edit_and_shows_a_notice() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane);
        reduce(&mut s, Action::EditDoc);
        reduce(&mut s, Action::InputChar('x'));
        reduce(&mut s, Action::InputSubmit);
        let _ = s.drain_outbox();

        reduce(&mut s, Action::DocumentLeaseBlocked);

        let edit = s.doc_edit.as_ref().expect("still tracking the edit");
        assert_eq!(edit.lease, DocLeaseState::Blocked);
        assert!(edit.pending.is_none(), "the queued mutation was dropped");
        assert!(s.outbox.is_empty(), "nothing is sent for a blocked lease");
        let notice = s.notice.as_ref().expect("a visible notice").0.clone();
        assert!(
            notice.contains("another writer"),
            "the range-leased notice must be visible: {notice}"
        );
        // Correlation to `document_id` is implicit: the client holds one in-flight
        // edit at a time, so a range-leased rejection is for that edit.
        assert_eq!(edit.document_id, document_id);
    }

    #[test]
    fn accepting_the_focused_suggestion_emits_an_accept_mutation() {
        let mut s = docs_on_review(vec![doc("a")]);
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::Approve(ApprovalScope::Once));
        assert_eq!(
            s.outbox,
            vec![Intent::MutateDocument {
                document_id,
                mutation: DocumentMutation::AcceptSuggestion {
                    suggestion_id: "s1".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn rejecting_the_focused_suggestion_emits_a_reject_mutation() {
        let mut s = docs_on_review(vec![doc("a")]);
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::Reject);
        assert_eq!(
            s.outbox,
            vec![Intent::MutateDocument {
                document_id,
                mutation: DocumentMutation::RejectSuggestion {
                    suggestion_id: "s1".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn suggestion_resolution_needs_the_review_rail_focused() {
        // On the tree rail, `a`/`r` resolve nothing (and, with the Docs overlay up,
        // they never touch a pending approval either).
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        reduce(&mut s, Action::OpenDocs); // Tree focus
        reduce(&mut s, Action::Approve(ApprovalScope::Once));
        reduce(&mut s, Action::Reject);
        assert!(s.outbox.is_empty());
    }

    #[test]
    fn a_document_sync_replaces_the_matching_cards_content() {
        let mut s = AppState::new();
        s.docs = vec![doc("a"), doc("b")];
        let target = s.docs[1].document_id;
        s.selected_doc = 1;
        s.selected_block = 5; // stale cursor, must be re-clamped
        reduce(
            &mut s,
            Action::DocumentSynced {
                document_id: target,
                revision: "r9".to_owned(),
                blocks: vec![crate::state::DocBlockView {
                    id: "b1".to_owned(),
                    kind: "paragraph".to_owned(),
                    text: "merged".to_owned(),
                }],
                suggestions: vec![],
            },
        );
        assert_eq!(s.docs[1].revision, "r9");
        assert_eq!(s.docs[1].blocks[0].text, "merged");
        assert!(s.docs[1].suggestions.is_empty());
        assert_eq!(s.selected_block, 0, "the block cursor was re-clamped");
        // The other card is untouched.
        assert_eq!(s.docs[0].revision, "r3");
    }

    #[test]
    fn a_document_sync_for_an_unknown_document_is_inert() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        reduce(
            &mut s,
            Action::DocumentSynced {
                document_id: codypendent_protocol::DocumentId::new(),
                revision: "r9".to_owned(),
                blocks: vec![],
                suggestions: vec![],
            },
        );
        assert_eq!(s.docs[0].revision, "r3", "no card matched, nothing changed");
    }

    #[test]
    fn closing_the_docs_browser_releases_a_held_lease() {
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        let document_id = s.docs[0].document_id;
        reduce(&mut s, Action::OpenDocs);
        reduce(&mut s, Action::CyclePane);
        reduce(&mut s, Action::EditDoc);
        reduce(&mut s, Action::InputChar('x'));
        reduce(&mut s, Action::InputSubmit);
        reduce(
            &mut s,
            Action::DocumentLeaseGranted {
                document_id,
                lease_id: "lease-7".to_owned(),
            },
        );
        let _ = s.drain_outbox();

        // Closing the browser (toggle `D`, or `Esc`) releases the held lease.
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.doc_edit.is_none());
        assert_eq!(
            s.outbox,
            vec![Intent::ReleaseDocumentLease {
                lease_id: "lease-7".to_owned(),
            }]
        );
    }

    #[test]
    fn edge_navigation_moves_selection_within_the_inspector() {
        let mut s = AppState::new();
        s.edges = vec![edge("a::f", "b::g"), edge("c::h", "d::i")];
        reduce(&mut s, Action::OpenEdges);
        assert_eq!(s.selected_edge, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_edge, 1);
        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_edge, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_edge, 0);
    }

    fn node(id: &str) -> crate::state::WorkflowNodeCard {
        crate::state::WorkflowNodeCard {
            workflow: "repair-github-check v1".to_owned(),
            id: id.to_owned(),
            action: "tool repository.test".to_owned(),
            kind: "tool".to_owned(),
            state: "pending".to_owned(),
            agent: "—".to_owned(),
            model_policy: "—".to_owned(),
            workspace: "shared worktree".to_owned(),
            approval: "none".to_owned(),
            retry: "1 attempt".to_owned(),
            depends_on: "—".to_owned(),
            outputs: "test_result".to_owned(),
            cost: "—".to_owned(),
            error: "—".to_owned(),
        }
    }

    #[test]
    fn open_workflow_toggles_the_workflow_view() {
        let mut s = AppState::new();
        s.workflow = vec![node("inspect")];
        reduce(&mut s, Action::OpenWorkflow);
        assert_eq!(s.overlay, Overlay::Workflow);
        assert_eq!(s.input_mode(), crate::state::InputMode::Normal);
        reduce(&mut s, Action::OpenWorkflow);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn workflow_navigation_moves_selection_within_the_graph() {
        let mut s = AppState::new();
        s.workflow = vec![node("inspect"), node("patch")];
        reduce(&mut s, Action::OpenWorkflow);
        assert_eq!(s.selected_node, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_node, 1);
        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_node, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_node, 0);
    }

    #[test]
    fn palette_opens_the_workflow_view() {
        // "workflow" routes through the palette to the workflow-graph overlay,
        // the discoverable front door in the conversation shell where a bare `W`
        // composes text.
        let mut s = AppState::new();
        s.workflow = vec![node("inspect")];
        reduce(&mut s, Action::OpenPalette);
        for c in "workflow".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::Workflow);
    }

    #[test]
    fn a_live_workflow_transition_overlays_the_matching_graph_card() {
        // T9: a live node transition folds into the graph view — the forever-`pending`
        // placeholder becomes the run's real state/cost/error, so `node_state_color`'s
        // non-pending branches come alive. Only the matching node id is touched.
        let mut s = AppState::new();
        s.workflow = vec![node("inspect"), node("verify")];

        reduce(
            &mut s,
            Action::WorkflowNodeUpdated {
                node_id: "inspect".to_owned(),
                state: "completed".to_owned(),
                cost: "12s · 3 tool calls".to_owned(),
                error: "—".to_owned(),
            },
        );

        let inspect = s.workflow.iter().find(|c| c.id == "inspect").unwrap();
        assert_eq!(inspect.state, "completed");
        assert_eq!(inspect.cost, "12s · 3 tool calls");
        assert_eq!(inspect.error, "—");
        // The other node is untouched by the transition.
        let verify = s.workflow.iter().find(|c| c.id == "verify").unwrap();
        assert_eq!(verify.state, "pending");

        // A failing transition carries its reason, and the fold is idempotent — a
        // re-delivered transition writes the same values (overlap is harmless).
        reduce(
            &mut s,
            Action::WorkflowNodeUpdated {
                node_id: "verify".to_owned(),
                state: "failed".to_owned(),
                cost: "—".to_owned(),
                error: "the test command exited 1".to_owned(),
            },
        );
        reduce(
            &mut s,
            Action::WorkflowNodeUpdated {
                node_id: "verify".to_owned(),
                state: "failed".to_owned(),
                cost: "—".to_owned(),
                error: "the test command exited 1".to_owned(),
            },
        );
        let verify = s.workflow.iter().find(|c| c.id == "verify").unwrap();
        assert_eq!(verify.state, "failed");
        assert_eq!(verify.error, "the test command exited 1");
    }

    fn item(kind: &str) -> crate::state::BlackboardItemCard {
        crate::state::BlackboardItemCard {
            run: "repair-github-check · run 0f2a".to_owned(),
            kind: kind.to_owned(),
            summary: "the failing test asserts an off-by-one".to_owned(),
            author: "agent investigator".to_owned(),
            confidence: "0.85".to_owned(),
            evidence: "2 ref(s)".to_owned(),
            revision: "r1".to_owned(),
            superseded: false,
        }
    }

    #[test]
    fn open_blackboard_toggles_the_blackboard_view() {
        let mut s = AppState::new();
        s.blackboard = vec![item("finding")];
        reduce(&mut s, Action::OpenBlackboard);
        assert_eq!(s.overlay, Overlay::Blackboard);
        assert_eq!(s.input_mode(), crate::state::InputMode::Normal);
        reduce(&mut s, Action::OpenBlackboard);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn blackboard_navigation_moves_selection_within_the_board() {
        let mut s = AppState::new();
        s.blackboard = vec![item("finding"), item("decision")];
        reduce(&mut s, Action::OpenBlackboard);
        assert_eq!(s.selected_item, 0);
        reduce(&mut s, Action::SelectNext);
        assert_eq!(s.selected_item, 1);
        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_item, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_item, 0);
    }

    #[test]
    fn palette_opens_the_blackboard_view() {
        let mut s = AppState::new();
        s.blackboard = vec![item("finding")];
        reduce(&mut s, Action::OpenPalette);
        for c in "blackboard".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::Blackboard);
    }

    #[test]
    fn opening_one_browser_replaces_another() {
        // The overlays are mutually exclusive: opening Docs over an open Edges
        // inspector swaps rather than stacks.
        let mut s = AppState::new();
        s.docs = vec![doc("a")];
        s.edges = vec![edge("a::f", "b::g")];
        reduce(&mut s, Action::OpenEdges);
        assert_eq!(s.overlay, Overlay::Edges);
        reduce(&mut s, Action::OpenDocs);
        assert_eq!(s.overlay, Overlay::Docs);
    }

    #[test]
    fn palette_opens_filters_and_stays_navigable() {
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        assert_eq!(
            s.overlay,
            Overlay::Palette {
                query: String::new(),
                selected: 0,
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Palette);

        // Navigation moves the selection within the (unfiltered) command list.
        reduce(&mut s, Action::SelectNext);
        assert_eq!(
            s.overlay,
            Overlay::Palette {
                query: String::new(),
                selected: 1,
            }
        );

        // Typing filters and resets the selection to the top.
        reduce(&mut s, Action::InputChar('d'));
        reduce(&mut s, Action::InputChar('o'));
        reduce(&mut s, Action::InputChar('c'));
        assert_eq!(
            s.overlay,
            Overlay::Palette {
                query: "doc".to_owned(),
                selected: 0,
            }
        );
        // Backspace edits the query too.
        reduce(&mut s, Action::InputBackspace);
        assert_eq!(
            s.overlay,
            Overlay::Palette {
                query: "do".to_owned(),
                selected: 0,
            }
        );
    }

    #[test]
    fn palette_submit_runs_the_highlighted_command() {
        // Filter to "docs" and run it: the palette closes and the Docs browser opens.
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        for c in "docs".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::Docs);
    }

    #[test]
    fn palette_submit_can_open_a_text_prompt() {
        // "new run" routes through the palette to the new-run prompt overlay.
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        for c in "new".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert!(matches!(s.overlay, Overlay::NewRun(_)));
    }

    #[test]
    fn palette_new_conversation_detaches_and_asks_to_forget_the_session() {
        // Task 5: "New conversation" cannot mint a session itself (pure,
        // I/O-free reducer) — it detaches, exactly like plain `Detach`, and
        // additionally flags `start_new_conversation` so the CLI harness
        // forgets this repository's remembered session before it tears down.
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        for c in "conversation".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert!(s.should_detach, "detaches like a plain Detach");
        assert!(
            s.start_new_conversation,
            "flags the harness to forget the remembered session"
        );
    }

    #[test]
    fn palette_escape_closes_without_running_anything() {
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        reduce(&mut s, Action::InputCancel);
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn palette_submit_with_no_match_is_inert() {
        let mut s = AppState::new();
        reduce(&mut s, Action::OpenPalette);
        for c in "zzzz".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        // Closed (mem::take), nothing opened.
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn composer_captures_text_and_esc_clears_it() {
        let mut s = AppState::new();
        for c in "fix the bug".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert_eq!(s.composer, "fix the bug");
        reduce(&mut s, Action::InputBackspace);
        assert_eq!(s.composer, "fix the bu");
        reduce(&mut s, Action::InputCancel);
        assert!(s.composer.is_empty());
    }

    #[test]
    fn slash_opens_the_palette_only_on_an_empty_composer() {
        // Slash on an empty composer opens the palette.
        let mut s = AppState::new();
        reduce(&mut s, Action::InputChar('/'));
        assert!(matches!(s.overlay, Overlay::Palette { .. }));
        assert!(s.composer.is_empty());

        // Slash after text is a literal character.
        let mut s2 = AppState::new();
        reduce(&mut s2, Action::InputChar('a'));
        reduce(&mut s2, Action::InputChar('/'));
        assert_eq!(s2.composer, "a/");
        assert_eq!(s2.overlay, Overlay::None);
    }

    #[test]
    fn composer_submit_starts_a_run_when_idle() {
        let mut s = AppState::new();
        for c in "diagnose the failing test".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert!(s.composer.is_empty(), "draft cleared after send");
        let intents = s.drain_outbox();
        assert!(
            matches!(
                intents.as_slice(),
                [Intent::StartRun { objective, .. }] if objective == "diagnose the failing test"
            ),
            "expected a StartRun intent, got {intents:?}"
        );
    }

    #[test]
    fn alt_enter_inserts_a_newline_without_submitting() {
        let mut s = AppState::new();
        for c in "line one".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputNewline);
        for c in "line two".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert_eq!(s.composer, "line one\nline two");
        // Nothing was submitted — no run started, draft still intact.
        assert!(s.drain_outbox().is_empty());
        assert!(!s.composer.is_empty());
    }

    #[test]
    fn submitting_pushes_to_history_skipping_consecutive_dupes() {
        let mut s = AppState::new();
        for c in "first message".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.composer_history, vec!["first message".to_owned()]);

        // A repeat of the very same message is not pushed again.
        for c in "first message".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.composer_history,
            vec!["first message".to_owned()],
            "consecutive duplicate must be skipped"
        );

        // A genuinely new message is appended.
        for c in "second message".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.composer_history,
            vec!["first message".to_owned(), "second message".to_owned()]
        );
    }

    #[test]
    fn history_prev_stashes_the_in_progress_draft_and_walks_backward() {
        let mut s = AppState::new();
        for text in ["oldest", "newest"] {
            for c in text.chars() {
                reduce(&mut s, Action::InputChar(c));
            }
            reduce(&mut s, Action::InputSubmit);
        }
        assert_eq!(
            s.composer_history,
            vec!["oldest".to_owned(), "newest".to_owned()]
        );

        // Start a fresh, in-progress draft — this must never be lost.
        for c in "in progress".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert_eq!(s.composer, "in progress");

        // First Up: stashes the in-progress draft, loads the newest entry.
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "newest");
        assert_eq!(s.composer_stash, Some("in progress".to_owned()));

        // Second Up: walks to the older entry.
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "oldest");

        // A third Up saturates at the oldest entry (no history before it).
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "oldest");
    }

    #[test]
    fn history_next_walks_forward_and_restores_the_stash_past_the_newest() {
        let mut s = AppState::new();
        for text in ["oldest", "newest"] {
            for c in text.chars() {
                reduce(&mut s, Action::InputChar(c));
            }
            reduce(&mut s, Action::InputSubmit);
        }
        for c in "in progress".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::HistoryPrev); // -> "newest" (stash "in progress")
        reduce(&mut s, Action::HistoryPrev); // -> "oldest"
        assert_eq!(s.composer, "oldest");

        // Down walks back toward newer entries.
        reduce(&mut s, Action::HistoryNext);
        assert_eq!(s.composer, "newest");

        // Down again moves past the newest: the stashed draft comes back,
        // verbatim, and the walk is over (further Down is a no-op).
        reduce(&mut s, Action::HistoryNext);
        assert_eq!(s.composer, "in progress");
        assert_eq!(s.history_cursor, None);

        reduce(&mut s, Action::HistoryNext);
        assert_eq!(
            s.composer, "in progress",
            "Down with no active recall must be a no-op"
        );
    }

    #[test]
    fn history_prev_is_a_noop_with_empty_history() {
        let mut s = AppState::new();
        for c in "draft".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "draft", "no history yet — nothing to recall");
        assert_eq!(s.history_cursor, None);
    }

    #[test]
    fn editing_a_recalled_entry_detaches_it_so_the_next_up_restashes() {
        let mut s = AppState::new();
        for c in "old one".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        for c in "working draft".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "old one");
        assert_eq!(s.history_cursor, Some(0));

        // Typing into the recalled entry detaches it from history.
        reduce(&mut s, Action::InputChar('!'));
        assert_eq!(s.composer, "old one!");
        assert_eq!(s.history_cursor, None);

        // The next Up re-stashes *this* edited text, not the original stash.
        reduce(&mut s, Action::HistoryPrev);
        assert_eq!(s.composer, "old one");
        reduce(&mut s, Action::HistoryNext);
        assert_eq!(s.composer, "old one!", "the edited draft must not be lost");
    }

    #[test]
    fn composer_submit_steers_a_live_run() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        // The run is live (non-terminal), so a message steers rather than restarts.
        assert!(s.selected_run_is_active());
        for c in "also add tests".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        let intents = s.drain_outbox();
        assert!(
            matches!(
                intents.as_slice(),
                [Intent::QueueSteering { text, run_id: r }] if text == "also add tests" && *r == run_id
            ),
            "expected a QueueSteering intent, got {intents:?}"
        );
    }

    #[test]
    fn a_follow_up_after_a_run_completes_continues_the_conversation() {
        // Task 5 (continuous-session plan): once the selected run reaches a
        // terminal state, the composer's next message must continue the SAME
        // session — pushing `SubmitUserInput`, not a context-free `StartRun` —
        // so the daemon (Tasks 1-4) seeds it with the prior turns instead of
        // starting cold. The prior turn must stay visible; it is the render
        // side (not this reducer path) that keeps it in view.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "fix the bug".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_owned()),
                },
                chronicle: artifact(),
            }),
        );
        assert!(!s.selected_run_is_active(), "the run is terminal");

        for c in "follow up".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        let intents = s.drain_outbox();
        assert!(
            matches!(
                intents.as_slice(),
                [Intent::SubmitUserInput {
                    text,
                    mode: AgentMode::Build,
                    // No model was ever pinned in this session, so the follow-up
                    // inherits the session's model server-side (carries None).
                    model: None,
                }] if text == "follow up"
            ),
            "expected a SubmitUserInput intent, got {intents:?}"
        );
    }

    #[test]
    fn follow_up_carries_the_pinned_model_for_an_instant_switch() {
        // The mid-conversation model switch: with a run already terminal, a
        // model pinned via the `/model` picker must ride on the very next
        // follow-up (`SubmitUserInput.model`), so the switch is instant and
        // applies in the SAME session rather than being silently dropped.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "fix the bug".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_owned()),
                },
                chronicle: artifact(),
            }),
        );
        assert!(!s.selected_run_is_active(), "the run is terminal");

        // The operator re-picks a model mid-conversation.
        s.pending_model = Some(codypendent_protocol::ModelId("pinned-model-x".to_owned()));

        for c in "use the big model now".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        let intents = s.drain_outbox();
        assert!(
            matches!(
                intents.as_slice(),
                [Intent::SubmitUserInput { model: Some(m), .. }]
                    if m.0 == "pinned-model-x"
            ),
            "the follow-up must carry the current pin, got {intents:?}"
        );
    }

    #[test]
    fn empty_composer_submit_sends_nothing() {
        let mut s = AppState::new();
        reduce(&mut s, Action::InputSubmit);
        assert!(s.drain_outbox().is_empty());
    }

    #[test]
    fn ctrl_arrows_cycle_between_runs() {
        let mut s = AppState::new();
        for (obj, _) in [("a", ()), ("b", ())] {
            reduce(
                &mut s,
                system_ev(EventBody::RunStarted {
                    run_id: RunId::new(),
                    objective: obj.to_owned(),
                    mode: AgentMode::Build,
                }),
            );
        }
        // The latest run is selected; Ctrl-↑ moves to the previous one.
        assert_eq!(s.selected_run, 1);
        reduce(&mut s, Action::PrevRun);
        assert_eq!(s.selected_run, 0);
        reduce(&mut s, Action::PrevRun); // clamps at the start
        assert_eq!(s.selected_run, 0);
        reduce(&mut s, Action::NextRun);
        assert_eq!(s.selected_run, 1);
    }

    #[test]
    fn paging_leaves_and_re_enters_follow_mode() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        // The renderer would cache the bottom offset; simulate a tall transcript.
        s.transcript_max_scroll.set(50);
        assert!(s.runs[0].follow, "runs follow by default");

        // Paging up leaves follow, starting a page up from the true bottom.
        reduce(&mut s, Action::ScrollPageUp);
        assert!(!s.runs[0].follow);
        assert_eq!(s.runs[0].scroll, 40);

        // Paging back down to the bottom re-enters follow.
        reduce(&mut s, Action::ScrollPageDown);
        assert_eq!(s.runs[0].scroll, 50);
        assert!(s.runs[0].follow);
    }

    #[test]
    fn sending_a_message_re_follows_the_latest() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        s.transcript_max_scroll.set(50);
        reduce(&mut s, Action::ScrollPageUp);
        assert!(!s.runs[0].follow);

        // Sending snaps the conversation back to the latest.
        for c in "keep going".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert!(s.runs[0].follow);
    }

    #[test]
    fn f2_toggles_between_chat_and_workspace_layouts() {
        use crate::state::LayoutMode;
        let mut s = AppState::new();
        assert_eq!(s.layout, LayoutMode::Chat);
        reduce(&mut s, Action::ToggleLayout);
        assert_eq!(s.layout, LayoutMode::Workspace);
        reduce(&mut s, Action::ToggleLayout);
        assert_eq!(s.layout, LayoutMode::Chat);
        // The palette command reaches the same toggle.
        reduce(&mut s, Action::OpenPalette);
        for c in "layout".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.layout, LayoutMode::Workspace);
    }

    fn model_card(id: &str, provider: &str) -> crate::state::ModelCard {
        crate::state::ModelCard {
            id: ModelId(id.to_owned()),
            provider: provider.to_owned(),
            location: None,
            cost_per_1k_usd: None,
            context_tokens: None,
        }
    }

    /// Open the model picker via the palette front door: `/` → filter "model"
    /// → Enter. Every other test below starts from this.
    fn open_model_picker(s: &mut AppState) {
        reduce(s, Action::OpenPalette);
        for c in "model".chars() {
            reduce(s, Action::InputChar(c));
        }
        reduce(s, Action::InputSubmit);
    }

    #[test]
    fn palette_opens_the_model_picker() {
        let mut s = AppState::new();
        s.models = vec![model_card("local-qwen", "openai-compatible")];
        open_model_picker(&mut s);
        assert_eq!(
            s.overlay,
            Overlay::ModelPicker {
                query: String::new(),
                selected: 0,
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Palette);
    }

    #[test]
    fn model_picker_navigation_moves_selection_and_resolves_the_focused_card() {
        let mut s = AppState::new();
        s.models = vec![
            model_card("local-qwen", "openai-compatible"),
            model_card("hosted-gpt", "openai-compatible"),
        ];
        open_model_picker(&mut s);
        assert_eq!(s.selected_model, 0);

        reduce(&mut s, Action::SelectNext);
        assert_eq!(
            s.overlay,
            Overlay::ModelPicker {
                query: String::new(),
                selected: 1,
            }
        );
        assert_eq!(
            s.selected_model, 1,
            "the resolved index tracks the filtered cursor"
        );
        assert_eq!(
            s.focused_model().map(|c| c.id.0.as_str()),
            Some("hosted-gpt")
        );

        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_model, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_model, 0);
        assert_eq!(
            s.focused_model().map(|c| c.id.0.as_str()),
            Some("local-qwen")
        );
    }

    #[test]
    fn model_picker_filters_by_id_substring_and_resets_selection() {
        let mut s = AppState::new();
        s.models = vec![
            model_card("local-qwen", "openai-compatible"),
            model_card("hosted-gpt", "openai-compatible"),
        ];
        open_model_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // move onto "hosted-gpt" first
        assert_eq!(s.selected_model, 1);

        for c in "qwen".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        // Filtering narrows the list to "local-qwen" and resets the cursor to
        // its top, resolving `selected_model` back to the matching full-list
        // index rather than leaving it pointing at the no-longer-visible row.
        match &s.overlay {
            Overlay::ModelPicker { query, selected } => {
                assert_eq!(query, "qwen");
                assert_eq!(*selected, 0);
            }
            other => panic!("expected the model picker, got {other:?}"),
        }
        assert_eq!(s.selected_model, 0);
        assert_eq!(
            s.focused_model().map(|c| c.id.0.as_str()),
            Some("local-qwen")
        );
    }

    #[test]
    fn model_picker_enter_stages_the_focused_model_and_emits_a_notice() {
        let mut s = AppState::new();
        s.models = vec![
            model_card("local-qwen", "openai-compatible"),
            model_card("hosted-gpt", "openai-compatible"),
        ];
        open_model_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // focus "hosted-gpt"
        reduce(&mut s, Action::InputSubmit); // stage it

        assert_eq!(s.overlay, Overlay::None, "the picker closes on select");
        assert_eq!(s.pending_model, Some(ModelId("hosted-gpt".to_owned())));
        let notice = s.notice.as_ref().expect("a visible notice").0.clone();
        assert!(
            notice.contains("hosted-gpt"),
            "the notice names the staged model: {notice}"
        );
        assert!(
            notice.contains("next run"),
            "the notice explains staging is advisory: {notice}"
        );
    }

    #[test]
    fn model_picker_enter_with_zero_matches_stages_nothing() {
        // Regression: `selected_model`'s `.unwrap_or(0)` fallback (see `nav`
        // and `edit_prompt`) points at the full list's row 0 whenever the
        // live query matches nothing — Enter must NOT silently stage that
        // row (the list is showing "no matching model", not row 0).
        let mut s = AppState::new();
        s.models = vec![
            model_card("local-qwen", "openai-compatible"),
            model_card("hosted-gpt", "openai-compatible"),
        ];
        open_model_picker(&mut s);
        for c in "zzz-no-such-model".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert!(
            crate::state::filter_models(&s.models, "zzz-no-such-model").is_empty(),
            "precondition: the query must match nothing"
        );

        reduce(&mut s, Action::InputSubmit);

        assert_eq!(
            s.overlay,
            Overlay::None,
            "the picker still closes (mirrors the palette's no-match submit)"
        );
        assert!(
            s.pending_model.is_none(),
            "a zero-match submit must not silently stage models[0]"
        );
        assert!(
            s.notice.is_none(),
            "a zero-match submit must not emit a staging notice"
        );
    }

    #[test]
    fn model_picker_escape_closes_without_staging() {
        let mut s = AppState::new();
        s.models = vec![model_card("local-qwen", "openai-compatible")];
        open_model_picker(&mut s);
        reduce(&mut s, Action::InputCancel);
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.pending_model.is_none(), "Esc must not stage anything");
    }

    // --- Task 8: the `/provider` picker (mirrors the model-picker tests above) ---

    fn provider_card(
        id: &str,
        name: &str,
        protocol: &str,
        auth: &str,
        local: bool,
    ) -> crate::state::ProviderCard {
        crate::state::ProviderCard {
            id: id.to_owned(),
            name: name.to_owned(),
            protocol: protocol.to_owned(),
            auth: auth.to_owned(),
            local,
            requires_key: auth.starts_with("api-key"),
            // Mirrors the harness gate closely enough for reducer tests: an
            // OpenAI-compatible provider with an api-key/none auth badge lists.
            can_list_models: protocol == "openai-chat"
                && (auth.starts_with("api-key") || auth == "none"),
        }
    }

    /// Open the provider picker via the palette front door: `/` → filter
    /// "provider" → Enter. Every other test below starts from this.
    fn open_provider_picker(s: &mut AppState) {
        reduce(s, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(s, Action::InputChar(c));
        }
        reduce(s, Action::InputSubmit);
    }

    #[test]
    fn palette_opens_the_provider_picker() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        assert_eq!(
            s.overlay,
            Overlay::ProviderPicker {
                query: String::new(),
                selected: 0,
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Palette);
    }

    #[test]
    fn provider_picker_navigation_moves_selection_and_resolves_the_focused_card() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card(
                "groq",
                "Groq",
                "openai-chat",
                "api-key: GROQ_API_KEY",
                false,
            ),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        assert_eq!(s.selected_provider, 0);

        reduce(&mut s, Action::SelectNext);
        assert_eq!(
            s.overlay,
            Overlay::ProviderPicker {
                query: String::new(),
                selected: 1,
            }
        );
        assert_eq!(
            s.selected_provider, 1,
            "the resolved index tracks the filtered cursor"
        );
        assert_eq!(s.focused_provider().map(|c| c.id.as_str()), Some("ollama"));

        reduce(&mut s, Action::SelectNext); // clamps at the end
        assert_eq!(s.selected_provider, 1);
        reduce(&mut s, Action::SelectPrev);
        assert_eq!(s.selected_provider, 0);
        assert_eq!(s.focused_provider().map(|c| c.id.as_str()), Some("groq"));
    }

    #[test]
    fn provider_picker_filters_by_id_substring_and_resets_selection() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card(
                "groq",
                "Groq",
                "openai-chat",
                "api-key: GROQ_API_KEY",
                false,
            ),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // move onto "ollama" first
        assert_eq!(s.selected_provider, 1);

        for c in "groq".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        // Filtering narrows the list to "groq" and resets the cursor to its
        // top, resolving `selected_provider` back to the matching full-list
        // index rather than leaving it pointing at the no-longer-visible row.
        match &s.overlay {
            Overlay::ProviderPicker { query, selected } => {
                assert_eq!(query, "groq");
                assert_eq!(*selected, 0);
            }
            other => panic!("expected the provider picker, got {other:?}"),
        }
        assert_eq!(s.selected_provider, 0);
        assert_eq!(s.focused_provider().map(|c| c.id.as_str()), Some("groq"));
    }

    #[test]
    fn provider_picker_enter_begins_the_flow_for_the_focused_provider() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card(
                "groq",
                "Groq",
                "openai-chat",
                "api-key: GROQ_API_KEY",
                false,
            ),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // focus "ollama" (can-list local)
        reduce(&mut s, Action::InputSubmit); // Enter begins the flow

        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "ollama".to_owned(),
                api_key: None,
            },
            "the picker gives way to the add-model flow, not a staged marker"
        );
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }]
        );
    }

    #[test]
    fn provider_picker_enter_with_zero_matches_begins_nothing() {
        // Regression (mirrors the model-picker's own regression test):
        // `selected_provider`'s `.unwrap_or(0)` fallback (see `nav` and
        // `edit_prompt`) points at the full list's row 0 whenever the live
        // query matches nothing — Enter must NOT silently begin the flow for
        // that row.
        let mut s = AppState::new();
        s.providers = vec![
            provider_card(
                "groq",
                "Groq",
                "openai-chat",
                "api-key: GROQ_API_KEY",
                false,
            ),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s);
        for c in "zzz-no-such-provider".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert!(
            crate::state::filter_providers(&s.providers, "zzz-no-such-provider").is_empty(),
            "precondition: the query must match nothing"
        );

        reduce(&mut s, Action::InputSubmit);

        assert_eq!(s.overlay, Overlay::None, "the picker still closes");
        assert!(
            s.outbox.is_empty(),
            "a zero-match submit must begin no flow"
        );
    }

    #[test]
    fn provider_picker_escape_closes_the_picker() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputCancel);
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.outbox.is_empty(), "Esc begins no flow");
    }

    // --- PR C2 (plan mode): the `/mode` picker (mirrors the pickers above) ---

    /// Open the mode picker via the palette front door: `/` → filter
    /// "mode picker" → Enter. ("mode" alone also substring-matches the Model
    /// picker's title, so the full row title is the unambiguous query.) Every
    /// other test below starts from this.
    fn open_mode_picker(s: &mut AppState) {
        reduce(s, Action::OpenPalette);
        for c in "mode picker".chars() {
            reduce(s, Action::InputChar(c));
        }
        reduce(s, Action::InputSubmit);
    }

    #[test]
    fn palette_opens_the_mode_picker_on_the_current_default() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        // The cursor pre-selects the current `default_mode` (Build, the
        // fourth row) rather than the top of the list.
        assert_eq!(
            s.overlay,
            Overlay::ModePicker {
                query: String::new(),
                selected: 3,
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Palette);
    }

    #[test]
    fn mode_picker_navigation_moves_the_selection() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectPrev); // Build (3) -> Plan (2)
        assert_eq!(
            s.overlay,
            Overlay::ModePicker {
                query: String::new(),
                selected: 2,
            }
        );
        reduce(&mut s, Action::SelectNext);
        reduce(&mut s, Action::SelectNext); // Plan -> Build -> Review (4)
        match &s.overlay {
            Overlay::ModePicker { selected, .. } => assert_eq!(*selected, 4),
            other => panic!("expected the mode picker, got {other:?}"),
        }
        reduce(&mut s, Action::SelectNext); // clamps at the end
        match &s.overlay {
            Overlay::ModePicker { selected, .. } => assert_eq!(*selected, 4),
            other => panic!("expected the mode picker, got {other:?}"),
        }
    }

    #[test]
    fn mode_picker_filters_by_label_and_resets_selection() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        for c in "plan".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        match &s.overlay {
            Overlay::ModePicker { query, selected } => {
                assert_eq!(query, "plan");
                assert_eq!(*selected, 0, "the cursor resets to the filtered top");
            }
            other => panic!("expected the mode picker, got {other:?}"),
        }
        // "plan" matches only the Plan card (its summary names a "plan").
        assert_eq!(crate::state::filter_modes("plan"), vec![2]);
    }

    #[test]
    fn mode_picker_enter_sets_default_mode_and_emits_a_notice() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectPrev); // Build -> Plan
        reduce(&mut s, Action::InputSubmit);

        assert_eq!(s.overlay, Overlay::None, "the picker closes on select");
        assert_eq!(s.default_mode, AgentMode::Plan);
        let notice = s.notice.as_ref().expect("a visible notice").0.clone();
        assert!(
            notice.contains("Plan"),
            "the notice names the mode: {notice}"
        );
        assert!(
            notice.contains("next run"),
            "the notice explains when it applies: {notice}"
        );
    }

    #[test]
    fn mode_picker_enter_with_zero_matches_changes_nothing() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        for c in "zzz-no-such-mode".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        assert!(
            crate::state::filter_modes("zzz-no-such-mode").is_empty(),
            "precondition: the query must match nothing"
        );

        reduce(&mut s, Action::InputSubmit);

        assert_eq!(s.overlay, Overlay::None, "the picker still closes");
        assert_eq!(
            s.default_mode,
            AgentMode::Build,
            "a zero-match submit must not change the mode"
        );
        assert!(
            s.notice.is_none(),
            "a zero-match submit must not emit a notice"
        );
    }

    #[test]
    fn mode_picker_escape_closes_without_changing_the_default() {
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectPrev); // move onto Plan, then abandon
        reduce(&mut s, Action::InputCancel);
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.default_mode,
            AgentMode::Build,
            "Esc must not stage anything"
        );
    }

    #[test]
    fn a_run_started_after_picking_a_mode_carries_it() {
        // PR C2: the picked `default_mode` flows into the `StartRun` intent —
        // the plan → build handoff needs no wire change.
        let mut s = AppState::new();
        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectPrev); // Build -> Plan
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.default_mode, AgentMode::Plan);

        reduce(&mut s, Action::NewRun);
        for c in "plan the fix".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        assert_eq!(
            s.drain_outbox(),
            vec![Intent::StartRun {
                objective: "plan the fix".to_owned(),
                mode: AgentMode::Plan,
                model: None,
            }],
            "the started run carries the picked mode"
        );
    }

    #[test]
    fn a_follow_up_after_picking_a_mode_carries_it() {
        // A continuation (`SubmitUserInput`) reads the same `default_mode`:
        // reviewing the plan in Build is "switch mode, submit 'implement it'".
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "plan the fix".to_owned(),
                mode: AgentMode::Plan,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );

        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectPrev); // cursor starts on Build (3) -> Plan (2)
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.default_mode, AgentMode::Plan);
        // Now flip to Build for the execution handoff: the picker reopens
        // with the cursor on Plan, so one step lands on Build.
        open_mode_picker(&mut s);
        reduce(&mut s, Action::SelectNext); // Plan -> Build
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.default_mode, AgentMode::Build);

        for c in "implement it".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit);

        assert_eq!(
            s.drain_outbox(),
            vec![Intent::SubmitUserInput {
                text: "implement it".to_owned(),
                mode: AgentMode::Build,
                model: None,
            }],
            "the follow-up carries the re-picked mode"
        );
    }

    // --- Task 4: the add-model flow (pick provider -> name -> masked key -> emit) ---

    #[test]
    fn provider_picker_tab_begins_the_add_model_flow_for_the_focused_provider() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card(
                "groq",
                "Groq",
                "openai-chat",
                "api-key: GROQ_API_KEY",
                false,
            ),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s); // focuses row 0 (groq)
        reduce(&mut s, Action::BeginAddModel);
        assert_eq!(
            s.overlay,
            Overlay::AddModelProviderKey {
                provider_id: "groq".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Editing);
    }

    #[test]
    fn add_model_cannot_list_hosted_flow_prompts_name_then_key_then_emits() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel); // cannot-list → free-text name prompt
        assert!(matches!(
            s.overlay,
            Overlay::AddModelId {
                requires_key: true,
                ..
            }
        ));
        for c in "claude-haiku-4.5".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // name → masked key
        assert_eq!(
            s.overlay,
            Overlay::AddModelKey {
                provider_id: "anthropic".to_owned(),
                model: "claude-haiku-4.5".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert!(s.outbox.is_empty(), "no intent until the key is entered");

        for c in "sk-secret".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // key → emit
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "anthropic/claude-haiku-4.5".to_owned(),
                provider_id: "anthropic".to_owned(),
                model: "claude-haiku-4.5".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }

    #[test]
    fn add_model_cannot_list_keyless_flow_skips_the_key_step() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "claude-code",
            "Claude Code (ACP)",
            "acp",
            "acp: npx",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel); // cannot-list, no key → name prompt
        assert!(matches!(
            s.overlay,
            Overlay::AddModelId {
                requires_key: false,
                ..
            }
        ));
        for c in "some-model".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // no key step → emit directly
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "claude-code/some-model".to_owned(),
                provider_id: "claude-code".to_owned(),
                model: "some-model".to_owned(),
                api_key: None,
            }]
        );
    }

    #[test]
    fn add_model_rejects_a_blank_model_name() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        reduce(&mut s, Action::InputSubmit); // empty buffer
        assert!(
            matches!(s.overlay, Overlay::AddModelId { .. }),
            "the prompt stays open on a blank name"
        );
        assert!(s.outbox.is_empty(), "no intent for a blank model name");
        assert!(s.notice.is_some(), "a notice explains the rejection");
    }

    #[test]
    fn add_model_escape_abandons_the_flow_without_emitting() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        for c in "x".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputCancel); // Esc on the model-name prompt
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.outbox.is_empty());
    }

    // --- model discovery: Enter/Tab begin the add-model flow ---

    #[test]
    fn provider_picker_enter_can_list_hosted_opens_the_key_prompt() {
        let mut s = AppState::new();
        // groq: openai-chat + api-key → can_list + requires_key.
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s); // focuses groq
        reduce(&mut s, Action::InputSubmit); // Enter begins the flow
        assert_eq!(
            s.overlay,
            Overlay::AddModelProviderKey {
                provider_id: "groq".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert!(s.outbox.is_empty(), "no query until the key is entered");
    }

    #[test]
    fn provider_picker_enter_can_list_local_queries_immediately() {
        let mut s = AppState::new();
        // ollama: openai-chat + none → can_list, no key.
        s.providers = vec![provider_card(
            "ollama",
            "Ollama (local)",
            "openai-chat",
            "none",
            true,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "ollama".to_owned(),
                api_key: None,
            }
        );
    }

    #[test]
    fn provider_picker_enter_cannot_list_opens_the_free_text_prompt() {
        let mut s = AppState::new();
        // anthropic: native protocol → cannot list, but needs a key.
        s.providers = vec![provider_card(
            "anthropic",
            "Anthropic",
            "anthropic",
            "api-key: ANTHROPIC_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "anthropic".to_owned(),
                requires_key: true,
                api_key: None,
                buffer: String::new(),
            }
        );
        assert!(s.outbox.is_empty(), "the free-text path emits nothing yet");
    }

    #[test]
    fn provider_picker_tab_and_enter_take_the_same_branch() {
        let providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];

        let mut via_enter = AppState::new();
        via_enter.providers = providers.clone();
        open_provider_picker(&mut via_enter);
        reduce(&mut via_enter, Action::InputSubmit);

        let mut via_tab = AppState::new();
        via_tab.providers = providers;
        open_provider_picker(&mut via_tab);
        reduce(&mut via_tab, Action::BeginAddModel);

        assert_eq!(via_enter.overlay, via_tab.overlay);
        assert!(matches!(
            via_enter.overlay,
            Overlay::AddModelProviderKey { .. }
        ));
    }

    // --- model discovery: Action handlers (isolated, no Enter/Tab flow) ---

    #[test]
    fn provider_models_loaded_opens_the_pick_list_carrying_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
        };
        reduce(
            &mut s,
            Action::ProviderModelsLoaded {
                provider_id: "groq".to_owned(),
                models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelPick {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
                models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
                query: String::new(),
                selected: 0,
            }
        );
    }

    #[test]
    fn provider_models_loaded_for_a_mismatched_provider_is_ignored() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsLoaded {
                provider_id: "ollama".to_owned(),
                models: vec!["qwen".to_owned()],
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: None,
            },
            "a stale result for another provider must not replace the overlay"
        );
    }

    #[test]
    fn provider_models_failed_falls_back_to_free_text_carrying_the_key() {
        let mut s = AppState::new();
        // Hosted + listable, and it already has a real key on the failed query —
        // the card's own `requires_key: true` must agree with `api_key.is_some()`.
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "groq".to_owned(),
                reason: "HTTP 401".to_owned(),
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "groq".to_owned(),
                requires_key: true,
                api_key: Some(SecretKey("sk-secret".to_owned())),
                buffer: String::new(),
            }
        );
        let notice = s.notice.as_ref().expect("a fallback notice").0.clone();
        assert!(
            notice.contains("HTTP 401"),
            "the notice explains why: {notice}"
        );
    }

    #[test]
    fn provider_models_failed_for_a_hosted_provider_blank_key_still_requires_key() {
        // Regression for the add-model bug: a hosted+listable provider (e.g.
        // groq) queried with a BLANK key (`api_key: None`) fails (401), and the
        // free-text fallback must still know this provider needs a key — derived
        // from the provider's own catalog card, not from whether a key was typed
        // on this particular query. Getting this wrong lets a keyless, unrunnable
        // hosted model be added with no way back to the key prompt.
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "groq".to_owned(),
                reason: "HTTP 401".to_owned(),
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "groq".to_owned(),
                requires_key: true,
                api_key: None,
                buffer: String::new(),
            },
            "a hosted provider must still require a key on fallback even though \
             this particular query carried none"
        );
    }

    #[test]
    fn provider_models_failed_for_a_local_provider_falls_back_with_no_key() {
        let mut s = AppState::new();
        // Local + listable + no-auth — the card's `requires_key: false` must
        // still hold on fallback.
        s.providers = vec![provider_card(
            "ollama",
            "Ollama (local)",
            "openai-chat",
            "none",
            true,
        )];
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "ollama".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "ollama".to_owned(),
                reason: "could not connect to the provider".to_owned(),
            },
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "ollama".to_owned(),
                requires_key: false,
                api_key: None,
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn provider_models_failed_for_a_mismatched_provider_is_ignored() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: None,
        };
        reduce(
            &mut s,
            Action::ProviderModelsFailed {
                provider_id: "ollama".to_owned(),
                reason: "x".to_owned(),
            },
        );
        assert!(matches!(s.overlay, Overlay::AddModelQuerying { .. }));
    }

    // --- model discovery: new overlay submit arms (isolated) ---

    #[test]
    fn add_model_provider_key_submit_queries_with_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: SecretKey("sk-secret".to_owned()),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }
        );
    }

    #[test]
    fn add_model_provider_key_blank_queries_with_no_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: SecretKey(String::new()),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.outbox,
            vec![Intent::QueryProviderModels {
                provider_id: "groq".to_owned(),
                api_key: None,
            }]
        );
        assert_eq!(
            s.overlay,
            Overlay::AddModelQuerying {
                provider_id: "groq".to_owned(),
                api_key: None,
            }
        );
    }

    #[test]
    fn add_model_pick_submit_emits_add_model_with_the_key() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: Some(SecretKey("sk-secret".to_owned())),
            models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
            query: String::new(),
            selected: 1,
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "groq/llama-3.3-70b".to_owned(),
                provider_id: "groq".to_owned(),
                model: "llama-3.3-70b".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }

    #[test]
    fn add_model_pick_zero_match_emits_nothing() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec!["llama-3.1-8b".to_owned()],
            query: "zzz-nope".to_owned(),
            selected: 0,
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(s.overlay, Overlay::None, "the picker still closes");
        assert!(s.outbox.is_empty(), "a zero-match submit adds nothing");
    }

    #[test]
    fn add_model_pick_filters_and_navigates() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec!["llama-3.1-8b".to_owned(), "gpt-oss-20b".to_owned()],
            query: String::new(),
            selected: 1,
        };
        // Typing resets the selection to the top of the new filtered set.
        for c in "gpt".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        match &s.overlay {
            Overlay::AddModelPick {
                query, selected, ..
            } => {
                assert_eq!(query, "gpt");
                assert_eq!(*selected, 0);
            }
            other => panic!("expected the pick-list, got {other:?}"),
        }
        // Down clamps at the single filtered row.
        reduce(&mut s, Action::SelectNext);
        match &s.overlay {
            Overlay::AddModelPick { selected, .. } => assert_eq!(*selected, 0),
            other => panic!("expected the pick-list, got {other:?}"),
        }
    }

    #[test]
    fn add_model_id_with_a_captured_key_emits_directly_without_re_prompting() {
        let mut s = AppState::new();
        s.overlay = Overlay::AddModelId {
            provider_id: "groq".to_owned(),
            requires_key: true,
            api_key: Some(SecretKey("sk-secret".to_owned())),
            buffer: "llama-3.1-8b".to_owned(),
        };
        reduce(&mut s, Action::InputSubmit);
        assert_eq!(
            s.overlay,
            Overlay::None,
            "no AddModelKey step — key already held"
        );
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "groq/llama-3.1-8b".to_owned(),
                provider_id: "groq".to_owned(),
                model: "llama-3.1-8b".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }

    #[test]
    fn patch_proposed_adds_expandable_summary() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::PatchProposed {
                run_id,
                changeset_id: ChangeSetId::new(),
                artifact: artifact(),
            }),
        );
        s.focus = Pane::Transcript;
        // The patch is the selected entry; expand toggles it.
        assert!(matches!(
            s.runs[0].transcript.last(),
            Some(TranscriptEntry::Patch(_))
        ));
        // transcript[0] is the User turn RunStarted pushes for the objective;
        // the patch is the next entry.
        s.runs[0].transcript_selected = 1;
        reduce(&mut s, Action::Expand);
        let TranscriptEntry::Patch(p) = &s.runs[0].transcript[1] else {
            unreachable!()
        };
        assert!(p.expanded);
    }

    #[test]
    fn select_run_sets_the_selected_run_clamped() {
        let mut s = AppState::new();
        for obj in ["a", "b", "c"] {
            reduce(
                &mut s,
                system_ev(EventBody::RunStarted {
                    run_id: RunId::new(),
                    objective: obj.to_owned(),
                    mode: AgentMode::Build,
                }),
            );
        }
        reduce(&mut s, Action::SelectRun(1));
        assert_eq!(s.selected_run, 1);
        reduce(&mut s, Action::SelectRun(99)); // clamps to last
        assert_eq!(s.selected_run, 2);
    }

    #[test]
    fn activate_row_with_no_overlay_selects_and_toggles_the_transcript_fold() {
        // A click on a transcript row (no overlay open) is "select it + Enter":
        // it focuses the transcript, moves the selection to entry N, and toggles
        // its fold — mirroring `a_short_note_folds_the_same_way_as_a_long_one`.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::NoteAppended {
                text: "the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );
        // transcript[0] is the User turn RunStarted pushes for the objective;
        // the note folds in right after it, starting collapsed.
        s.focus = Pane::Sessions; // not on the transcript yet — the click must focus it
        reduce(&mut s, Action::ActivateRow(1));
        assert_eq!(s.focus, Pane::Transcript, "a click focuses the transcript");
        assert_eq!(
            s.runs[0].transcript_selected, 1,
            "the click selects entry N"
        );
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!("NoteAppended must fold into a Note entry")
        };
        assert!(
            *expanded,
            "ActivateRow toggles the fold, exactly like Enter"
        );

        reduce(&mut s, Action::ActivateRow(1));
        let TranscriptEntry::Note { expanded, .. } = &s.runs[0].transcript[1] else {
            unreachable!()
        };
        assert!(!*expanded, "a second click toggles it back off");
    }

    #[test]
    fn activate_row_in_an_overlay_selects_and_runs_that_row() {
        // A click on overlay row N is "select it + Enter": it must move the
        // overlay's own `selected` to N (not just activate whatever was already
        // selected) and then run it — mirroring
        // `palette_submit_runs_the_highlighted_command`.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(&mut s, Action::OpenPalette);
        for c in "run".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        // "run" filters (in table order) to [New run, Steer run, Pause/resume
        // run, Cancel run, Model picker, Detach]; row 1 is "Steer run".
        reduce(&mut s, Action::ActivateRow(1));
        assert_eq!(
            s.overlay,
            Overlay::Steering(String::new()),
            "row 1 of the filtered list ('Steer run') ran, not row 0"
        );
    }

    #[test]
    fn finalize_leaves_streaming_tail_plain_then_snaps_on_stop() {
        use crate::state::TranscriptEntry;
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "go".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "# Title\n**bold**".to_owned(),
            }),
        );
        // Still streaming ⇒ the tail Model stays plain (rendered None).
        let model = s.runs[0]
            .transcript
            .iter()
            .rev()
            .find(|e| matches!(e, TranscriptEntry::Model { .. }))
            .unwrap();
        assert!(matches!(
            model,
            TranscriptEntry::Model { rendered: None, .. }
        ));

        // Stream ends (activity leaves Streaming) ⇒ finalize parses it once.
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );
        let model = s.runs[0]
            .transcript
            .iter()
            .rev()
            .find(|e| matches!(e, TranscriptEntry::Model { .. }))
            .unwrap();
        match model {
            TranscriptEntry::Model {
                rendered: Some(lines),
                ..
            } => assert!(!lines.is_empty()),
            other => panic!("expected finalized Model, got {other:?}"),
        }
    }

    #[test]
    fn finalize_is_idempotent() {
        use crate::markdown::PARSE_CALLS;
        use std::sync::atomic::Ordering;
        // Serialize against the other PARSE_CALLS-dependent test
        // (`render::theme_change_re_renders_without_re_parsing`): PARSE_CALLS
        // is one process-wide counter and `cargo test` runs tests in parallel
        // by default, so two such tests racing could otherwise interleave
        // their reset/read and flake this assertion. Held for the whole test
        // (see `markdown::lock_parse_calls` doc comment for why this cannot
        // deadlock against this test's own finalize steps below).
        let _serialize_parse_calls = crate::markdown::lock_parse_calls();
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "go".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "hello".to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );
        PARSE_CALLS.store(0, Ordering::Relaxed);
        // Further events run the sweep again; the finalized entry is not re-parsed.
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );
        assert_eq!(
            PARSE_CALLS.load(Ordering::Relaxed),
            0,
            "already-cached entry re-parsed"
        );
    }
}

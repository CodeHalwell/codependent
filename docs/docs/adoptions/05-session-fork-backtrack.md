# Adoption 05 — Session forking + Esc-Esc backtrack

**Effort:** M · **Depends on:** 04 · **Reference:** reference-repos/opencode/packages/opencode/src/session/session.ts (fork, ~line 693), reference-repos/codex/codex-rs/tui/src/app_backtrack.rs
**Ported from:** opencode (fork data model) + codex (backtrack UX) · **Status:** ⬜ not started

## 1. Summary

Closes roadmap STEP 5.6 (`ForkSession{checkpoint}` — the one Fleet-adjacent overlay not built; ROADMAP.md line 427, `docs/docs/build/15-phase-5-workflows-and-multi-agent.md` §STEP 5.6, `docs/docs/03-daemon-client-protocol.md` line 276).

A new `ForkSession` command clones a session's event ledger **up to a checkpoint boundary** into a fresh session: events are copied in order with run ids remapped through an id-map (opencode's `fork()` pattern), run projection rows are cloned under the new ids, and the fork records which Adoption-04 checkpoint it branched from so its runs carve their worktrees from the **checkpointed** filesystem state instead of current HEAD. Forks share immutable prior artifacts for free (the artifact store is content-addressed and daemon-global) and get independent runs/worktrees/budgets — exactly the STEP 5.6 contract. The source session is untouched (source-preserving branch).

The TUI grows codex's Esc-Esc backtrack: with an empty composer, the first Esc primes backtrack (hint in the status line), the second opens a transcript-selection overlay with the **latest user message highlighted**; ↑/↓ step through prior user messages (indexed as "nth user message since session start", robust to transcript mutation); Enter forks the session before the selected message and **refills the composer** with that message's text so the user edits and resubmits on the new branch.

## 2. Reference implementation

**Fork data model — `reference-repos/opencode/packages/opencode/src/session/session.ts`, `fork()` (line ~693):**

- Creates a new session (`createNext`) with a derived title (`getForkedTitle` → `"(fork #N)"` auto-increment) and a **structuredClone of the original metadata**.
- Loads all messages of the source; the cut point is `msgs.findIndex(msg => msg.info.id === input.messageID)` — everything **before** the target message is copied, the target itself and everything after is not.
- For each copied message it mints a fresh ascending id (`MessageID.ascending()`) and records `idMap.set(oldId, newId)`; parent references are remapped through the map (`assistant.parentID ? idMap.get(...)`), and message parts get fresh part ids with `compaction.tail_start_id` also remapped (`p.tail_start_id = idMap.get(p.tail_start_id)`). The remap is what keeps the clone internally consistent while never aliasing the source's id space.
- The review (`scratchpad/reviews/opencode-session-model.md` line 15) confirms this is the load-bearing shape: "copies each message and part with fresh ascending IDs, keeping an idMap so `assistant.parentID` and `compaction.tail_start_id` references are remapped into the new ID space".
- opencode's fork dialog **pre-fills the composer** with the forked-from prompt text (`scratchpad/reviews/opencode.md` line 47) — same refill behaviour codex has.

**Backtrack UX — `reference-repos/codex/codex-rs/tui/src/app_backtrack.rs`:**

- `BacktrackState` (line 58): `primed: bool`, `base_id: Option<ThreadId>` (selections are ignored if the thread changed under the overlay), `nth_user_message: usize` (`usize::MAX` = no selection — an index into the **filtered "user messages since the last session start" view, not into transcript cells**), `overlay_preview_active: bool`.
- `handle_backtrack_esc_key` (line 172): only when the composer is empty; first Esc → `prime_backtrack` (line 258: set primed, capture base thread id, show composer hint); second Esc → `open_backtrack_preview` (line 268: open the transcript overlay, mark preview active, select+highlight the latest user message); further Esc while the overlay shows → `step_backtrack_and_highlight` (line 305: step selection one **older**, clamped at 0).
- Arrow keys inside the overlay step older/newer (`overlay_step_backtrack`/`_forward`, lines 428/438); Enter confirms (`overlay_confirm_backtrack`, line 417): closes the overlay, builds a `BacktrackSelection { thread_id, nth_user_message, prompt }` from the highlighted `UserHistoryCell` (line 468) and requests `ForkSessionForPromptEdit` — fork before the selection on a source-preserving branch, prompt back in the composer.
- `nth_user_position`/`user_count` resolve the ordinal against the visible transcript; `backtrack_fork_before_turn_id` (line 508) re-resolves it against the **persisted** thread and refuses two cases that codypendent must mirror: a selected prompt that is a mid-turn **steer** ("cannot be branched independently", line 557-558) and a turn still in progress (line 560-561).
- Closing the overlay by any path fully resets backtrack state (`close_transcript_overlay` line 233, `reset_backtrack_state` line 460).

## 3. Current state in codypendent (verified)

- **STEP 5.6 authoritative constraints:** `docs/docs/03-daemon-client-protocol.md` line 276 names the command shape `ForkSession { checkpoint: CheckpointId, name: String }`. `docs/docs/build/15-phase-5-workflows-and-multi-agent.md` §STEP 5.6: "forks share immutable prior artifacts, get independent plans/worktrees/budgets; TUI shows the fork tree and a comparison view (chronicle + changeset diff side by side)", with TESTS "fork isolation (mutating fork B's worktree/plan leaves A untouched; both reference the same pre-fork artifacts)". ROADMAP line 623-624 converges "Side conversations & forks" onto this same command. This spec implements the command, isolation, shared artifacts, and a minimal fork indicator; the side-by-side comparison view is explicitly deferred (§10) — a deviation from STEP 5.6's full text, stated here so it is a decision, not an omission.
- **Ledger**: `crates/daemon/src/ledger.rs` — `create_session` (line 13), `append_event` (line 35, caller-supplied sequence, UNIQUE `(session_id, sequence)`), `load_events` (line 62), `append_next_event` (line 185). `crates/daemon/src/replay.rs` folds events into `SessionProjection` (title/note_count/closed) — pure, will fold a fork's copied events identically.
- **Events carrying run ids** (`crates/protocol/src/events.rs`): `NoteAppended{run_id: Option}`, `RunStarted`, `RunStateChanged`, `ModelStreamDelta`, `ToolProposed`, `ToolDenied`, `ToolStarted`, `ToolCompleted`, `PatchProposed`, `SteeringQueued`, `SteeringApplied`, `BudgetWarning`, `RunCompleted`, `RunUsage`, `LearningsCaptured`, and Adoption 04's `CheckpointRecorded`/`CheckpointRestored`. `ApprovalRequested`/`ApprovalResolved` carry only `ApprovalId`. `Actor::Agent` also embeds a `run_id`.
- **Runs projection**: `runs` table (`migrations/0001`/`0002`) with PK `id` — a run id can exist in **one** session only, so copied events must remap run ids (aliasing would break `projections::run_session` and the ownership gate in `CommandBody::named_resources`, `crates/protocol/src/command.rs` line 799).
- **Continuations**: `SubmitUserInput` launches a fresh run whose prior transcript is reconstructed **from the session ledger** (`crates/codypendentd/src/session_history.rs`, `continuation_prior` line 127 / `session_transcript` line 173) — so a forked session with a copied ledger gets correct conversational context with zero extra work. Repository/model inheritance comes from `session_run_provenance` (`crates/daemon/src/commands.rs` line 1495), which reads the session's **commands** table rows — a forked session has none, so provenance must learn to follow the fork parent (§5.4).
- **Worktrees**: every writing run carves a fresh worktree at `HEAD` (`WorktreeManager::allocate`, `crates/daemon/src/worktrees.rs` line 257 — note it already passes an explicit `<base>` to `git worktree add`, it just always computes it from HEAD). Adoption 04 records each run's launch checkpoint (`run_checkpoints.base_commit`, kind, sha).
- **TUI**: `Overlay` enum (`crates/tui/src/state.rs` line 212) with reducer-owned transitions; Esc maps to `Action::InputCancel` in Composer mode and `Action::Dismiss` in Normal mode (`crates/tui/src/input.rs` lines 446-497 / 401-424); `input_cancel` (`crates/tui/src/reduce.rs` line 4366) clears the composer in the base view — the priming hook. `Overlay`s without an explicit `input_mode` arm run in `InputMode::Normal` (`state.rs` `input_mode()`, line 2624), where ↑/↓/Enter/Esc arrive as `SelectPrev`/`SelectNext`/`Expand`/`Dismiss`. `AppState.runs: Vec<RunView>` in arrival order; each run's `objective` is its user message (`RunView`, `state.rs` line 981). Adoption 04 adds `RunView.launch_checkpoint: Option<CheckpointId>`.
- **CLI harness**: `crates/cli/src/tui.rs` `intent_to_command` (line 4063) maps `Intent`s to `CommandBody`s; the harness owns the attached session id and already has a fresh-session swap path (`pending_run_start` doc, `state.rs` line ~2237).
- **Must not break**: the ~600 pure-reducer TUI tests; ledger immutability (copies append to a NEW session, the source is never rewritten); the ownership gate (`named_resources` has no wildcard arm — the new command will not compile until classified).

## 4. Design

**Command** (deviation from Chapter 03's two-field shape, stated): 

```
ForkSession { session_id, checkpoint, name: Option<String> }
```

`session_id` is added because the ownership gate authorizes commands by the resources they *name* (`named_resources` is pure over the body — it cannot query which session a checkpoint belongs to), and `name` becomes optional with an opencode-style derived default (`"<title> (fork)"`, `" (fork #2)"`, …). The daemon verifies the checkpoint's run belongs to `session_id` and answers `checkpoint.not-found` identically for absent and mismatched (no enumeration oracle).

**Fork point.** Only **ordinal-1** checkpoints (run-launch) are forkable: "fork before user message N" = "fork before run N" = copy events with `sequence < sequence_of(RunStarted{run_N})`. Ordinals ≥ 2 are mid-run steering turns and are restore-only (Adoption 04) — the codex parallel is exact ("the selected prompt is a steer and cannot be branched independently", app_backtrack.rs line 557), as is cline's `trimMessagesBeforeUserRun` span guard. Rejected `fork.mid-run-checkpoint`.

**Ledger copy with id remap** (opencode `fork()` ported to events):

1. Create the fork session row (`state='open'`, `owner_uid` copied from the source, plus new fork columns — migration 0035).
2. Load source events `sequence <= cut` (`cut = RunStarted(run_N).sequence - 1`).
3. Walk in order; on each `RunStarted{run_id}` mint a fresh `RunId` and record `id_map[old] = new`. Rewrite **every** `run_id` field in event bodies and in `Actor::Agent` through the map (§5.3 lists the exhaustive match). `ApprovalId`s are left as-is: they are immutable historical evidence, all pre-cut approvals are resolved (a pending approval implies a non-terminal run, and the cut precedes run N — see gotcha 4), and `ResolveApproval` only acts on `pending` rows.
4. Append each rewritten event to the fork with fresh sequences `1..=cut` via `append_event`.
5. Clone the `runs` rows for each mapped run under the new ids (`session_id` = fork, all other columns copied — states are terminal pre-cut).
6. Append one `SessionForked { from_session, checkpoint }` marker event (sequence `cut+1`) so the fork's own ledger records its origin.
7. Reply `Payload::SessionForked { session_id }`.

**Filesystem state.** The fork stores the checkpoint's `base_commit`, `commit_sha`, and `kind` on its session row. Every **writing run launched in a forked session** carves its worktree at `fork_base_commit` instead of HEAD (new `WorktreeManager::allocate_at`), and, when the checkpoint kind is `Stash`, runs `git stash apply <commit_sha>` in the fresh worktree before the agent loop starts — reproducing run N's exact starting filesystem (worktrees share the repository object store, so the pinned checkpoint objects are reachable from any worktree). This gives the STEP 5.6 isolation property structurally: fork A and fork B allocate distinct worktrees on distinct `codypendent/run-*` branches, and mutating one cannot touch the other.

**Shared immutable artifacts.** Copied events reference `ArtifactRef`s by content-addressed id in the daemon-global store (`crates/daemon/src/artifacts.rs`) — both branches resolve the same bytes; nothing is duplicated. This satisfies "both reference the same pre-fork artifacts" with zero code.

**Provenance inheritance.** `session_run_provenance` recurses into `forked_from_session_id` when the session has no `StartRun`/`SubmitUserInput` command rows of its own, so the fork's first continuation inherits repository and pinned model (bounded depth — follow at most 8 parents to tolerate fork-of-fork without cycles).

**Backtrack UX (TUI)** — codex's state machine with one stated deviation: inside the overlay, **Esc closes** (codypendent's universal Esc-dismisses-overlay idiom; codex uses Esc-to-step) and stepping is ↑/↓ only. Priming is identical: Esc on an empty composer primes; a second Esc opens `Overlay::Backtrack` with the newest user message selected. Selection = index into `state.runs` (each run contributes exactly one user message — its objective; steering markers are not user messages, mirroring codex's steer exclusion). Enter emits `Intent::ForkSession { checkpoint }` using the selected run's `launch_checkpoint` and refills the composer with that run's `objective`. Any composer edit, overlay open, or daemon event that swaps the session clears the primed flag (codex's base-thread-id guard maps to: the harness owns one attached session; a session swap rebuilds `AppState`, which resets the flag structurally).

**After the fork**, the CLI harness receives `SessionForked{session_id}` and re-attaches to the new session (the same swap path a fresh session uses), leaving the composer refill intact.

## 5. Changes, file by file

No `Cargo.toml` changes.

### 5.1 `crates/protocol/src/command.rs`

```rust
    /// Fork a session at a recorded run-launch checkpoint (Phase 5 STEP 5.6,
    /// `ForkSession{checkpoint}` from Chapter 03; Adoption 05). The daemon
    /// copies the session's ledger up to (excluding) the checkpointed run —
    /// remapping run ids into a fresh id space — into a NEW session that
    /// records its fork origin, and replies
    /// [`SessionForked`](crate::envelope::Payload::SessionForked). The source
    /// session is never modified. Runs launched in the fork carve their
    /// worktrees from the checkpointed filesystem state, so the two branches
    /// are isolated while sharing all immutable pre-fork artifacts.
    /// Controller-only. `checkpoint` must be an ordinal-1 (run-launch)
    /// checkpoint of a run in `session_id`; a mid-run steering checkpoint is
    /// rejected `fork.mid-run-checkpoint`, an absent or foreign checkpoint
    /// `checkpoint.not-found` (identically — no oracle).
    ForkSession {
        session_id: SessionId,
        checkpoint: CheckpointId,
        /// The fork's title; absent derives `"<source title> (fork)"` with an
        /// opencode-style `#N` auto-increment on collision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
```

`named_resources` arm:

```rust
    Self::ForkSession { session_id, .. } => vec![NamedResource::Session(*session_id)],
```

### 5.2 `crates/protocol/src/events.rs` + `envelope.rs`

```rust
    /// This session was created by forking another at a checkpoint
    /// (Adoption 05). Appended once, immediately after the copied history, so
    /// the fork's own ledger records its origin. Clients render it as a
    /// "forked from …" marker; `Unknown` on older builds (RULE 1).
    SessionForked {
        from_session: SessionId,
        checkpoint: CheckpointId,
    },
```

`Payload` (envelope.rs, next to `DocumentCreated`):

```rust
    /// Reply to `ForkSession`: the freshly created fork.
    SessionForked {
        command_id: CommandId,
        session_id: SessionId,
    },
```

### 5.3 `migrations/0036_session_forks.sql`

```sql
-- Adoption 05 (STEP 5.6): a session may be a fork of another at an
-- Adoption-04 checkpoint. All columns nullable: every pre-existing session
-- reads NULL = "not a fork". fork_base_commit / fork_checkpoint_sha /
-- fork_checkpoint_kind are denormalized from run_checkpoints at fork time so
-- worktree allocation for the fork's runs needs no join against a row the
-- source session owns.
ALTER TABLE sessions ADD COLUMN forked_from_session_id TEXT;
ALTER TABLE sessions ADD COLUMN forked_at_sequence INTEGER;
ALTER TABLE sessions ADD COLUMN fork_base_commit TEXT;
ALTER TABLE sessions ADD COLUMN fork_checkpoint_sha TEXT;
ALTER TABLE sessions ADD COLUMN fork_checkpoint_kind TEXT;
```

(Renumber to next-free if adoptions land out of order.)

### 5.4 `crates/daemon/src/ledger.rs` — the copy

```rust
/// Copy `events` (already loaded, already ordered) into `target`, remapping
/// run ids through `id_map` and renumbering sequences from 1. Pure over its
/// inputs plus the appends; the source session is untouched.
pub async fn copy_events_remapped(
    pool: &SqlitePool,
    target: SessionId,
    events: &[SessionEvent],
    id_map: &HashMap<RunId, RunId>,
) -> anyhow::Result<u64> {
    let mut sequence = 0u64;
    for event in events {
        sequence += 1;
        let copied = SessionEvent {
            sequence,
            occurred_at: event.occurred_at,
            causation_id: None,      // command rows are not copied
            correlation_id: event.correlation_id,
            actor: remap_actor(&event.actor, id_map),
            body: remap_body(&event.body, id_map),
        };
        append_event(pool, target, &copied).await?;
    }
    Ok(sequence)
}
```

`remap_body` is an **exhaustive** match over `EventBody` (no wildcard for run-carrying variants; the `_ => body.clone()` arm covers only variants that provably carry no `RunId` — list them in a comment so a new run-carrying variant is caught in review):

```rust
fn remap(id: RunId, map: &HashMap<RunId, RunId>) -> RunId {
    *map.get(&id).unwrap_or(&id)
}

fn remap_actor(actor: &Actor, map: &HashMap<RunId, RunId>) -> Actor {
    match actor {
        Actor::Agent { agent_id, run_id, model } => Actor::Agent {
            agent_id: *agent_id,
            run_id: remap(*run_id, map),
            model: model.clone(),
        },
        other => other.clone(),
    }
}

fn remap_body(body: &EventBody, map: &HashMap<RunId, RunId>) -> EventBody {
    use EventBody::*;
    match body {
        NoteAppended { text, run_id } => NoteAppended {
            text: text.clone(),
            run_id: run_id.map(|id| remap(id, map)),
        },
        RunStarted { run_id, objective, mode } => RunStarted {
            run_id: remap(*run_id, map),
            objective: objective.clone(),
            mode: *mode,
        },
        RunStateChanged { run_id, state } => RunStateChanged { run_id: remap(*run_id, map), state: *state },
        ModelStreamDelta { run_id, text } => /* remap run_id, clone text */,
        ToolProposed { .. } | ToolDenied { .. } | ToolStarted { .. } | ToolCompleted { .. }
        | PatchProposed { .. } | SteeringQueued { .. } | SteeringApplied { .. }
        | BudgetWarning { .. } | RunCompleted { .. } | RunUsage { .. }
        | LearningsCaptured { .. }
        | CheckpointRecorded { .. } | CheckpointRestored { .. } => /* clone with run_id remapped */,
        // No RunId inside: SessionCreated, SessionClosed, ApprovalRequested,
        // ApprovalResolved, ClientPresenceChanged, SessionForked, Unknown.
        other => other.clone(),
    }
}
```

Also add `run_started_sequence(pool, session_id, run_id) -> anyhow::Result<Option<u64>>` (a `SELECT sequence FROM events WHERE session_id = ? AND body LIKE '%"type":"RunStarted"%' …` is fragile — instead load events and scan for `RunStarted{run_id}`; sessions are bounded and this runs once per fork).

`crates/daemon/src/commands.rs`: `session_run_provenance` (line 1495) — when the loop finds neither model nor repository, look up `forked_from_session_id` on the session row and recurse (depth-capped at 8).

### 5.5 `crates/daemon/src/forks.rs` (new module)

```rust
/// Everything ForkSession does after validation, in order. Not one SQLite
/// transaction end-to-end (append_event autocommits) — instead the fork
/// session row is written LAST-VISIBLE: the row is inserted first but only
/// counted usable once the SessionForked marker lands; a crash mid-copy
/// leaves a half-copied orphan session that attach can see but that owns no
/// runs and no live state — harmless, documented, and re-forkable.
pub async fn fork_session(
    pool: &SqlitePool,
    source: SessionId,
    checkpoint: RunCheckpoint,      // fetched + validated by the caller
    name: Option<String>,
    owner_uid: Option<u32>,
) -> Result<SessionId, CodypendentError> {
    // 1. Cut point: the checkpointed run's RunStarted.
    if checkpoint.ordinal != 1 { /* fork.mid-run-checkpoint */ }
    let events = ledger::load_events(pool, source).await…;
    let cut = events.iter()
        .find(|e| matches!(&e.body, EventBody::RunStarted { run_id, .. } if *run_id == checkpoint.run_id))
        .map(|e| e.sequence - 1)
        .ok_or(/* checkpoint.not-found */)?;
    let head: Vec<SessionEvent> = events.into_iter().filter(|e| e.sequence <= cut).collect();

    // 2. Id map: one fresh RunId per RunStarted in the head.
    let mut id_map = HashMap::new();
    for event in &head {
        if let EventBody::RunStarted { run_id, .. } = &event.body {
            id_map.insert(*run_id, RunId::new());
        }
    }

    // 3. Fork session row (title derivation, fork columns, owner copied).
    let fork = SessionId::new();
    let title = derive_fork_title(pool, source, name).await?;
    /* INSERT INTO sessions (id, title, state, created_at, updated_at, revision,
         owner_uid, forked_from_session_id, forked_at_sequence,
         fork_base_commit, fork_checkpoint_sha, fork_checkpoint_kind)
       VALUES (?, ?, 'open', ?, ?, 0, ?, ?, ?, ?, ?, ?) */

    // 4. Copy events; 5. clone runs rows under mapped ids;
    let copied = ledger::copy_events_remapped(pool, fork, &head, &id_map).await…;
    clone_run_rows(pool, fork, &id_map).await…;   // INSERT INTO runs SELECT with new id/session_id

    // 6. Marker event (sequence copied + 1, via append_next_event).
    ledger::append_next_event(pool, fork, &Actor::System,
        &EventBody::SessionForked { from_session: source, checkpoint: checkpoint.id },
        Utc::now()).await…;
    Ok(fork)
}
```

`clone_run_rows` copies `objective, state, mode, model_policy, budget_json, started_at, ended_at, prompt_tokens, completion_tokens, cost_micros` (NOT `workspace_lease_id` — the fork owns no leases yet).

### 5.6 `crates/daemon/src/server.rs`

Connection-level intercept (the `PublishDocument`/`StartWorkflow` pattern): `CommandBody::ForkSession { session_id, checkpoint, name }` — Controller role; ownership gate already ran via `named_resources`; fetch checkpoint (`worktrees::fetch_checkpoint`), resolve its run's session via `projections::run_session` and require `== session_id` (else `checkpoint.not-found`); call `forks::fork_session`; reply `Payload::SessionForked { command_id, session_id: fork }`. Idempotency: route through `CommandProcessor::replay_existing` first and record the outcome (`created_session`) like `CreateSession` does, so a duplicate delivery returns the same fork instead of minting a second.

### 5.7 `crates/daemon/src/worktrees.rs` + `crates/codypendentd/src/executor.rs` — fork-based worktrees

`worktrees.rs`: split `allocate` into

```rust
pub async fn allocate(&self, pool, repository, run_id) -> …          // unchanged behavior
pub async fn allocate_at(&self, pool, repository, run_id,
                         base: Option<&str>) -> Result<WorkspaceLease, WorktreeError>
```

where `allocate` delegates to `allocate_at(…, None)` and `allocate_at` uses `base` verbatim as the `git worktree add -b <branch> <path> <base>` argument (verified with `git cat-file -e <base>^{commit}` first, error `Git` otherwise) instead of `git rev-parse HEAD`. The lease's `base_commit` records the actual base used.

`executor.rs`: `bind_run_worktree` (line 2911) gains a `fork: Option<SessionFork>` parameter, where

```rust
pub(crate) struct SessionFork {
    pub base_commit: String,
    pub checkpoint_sha: String,
    pub kind: CheckpointKind,
}
```

loaded by `RuntimeExecutor::execute` from the session row (`SELECT fork_base_commit, fork_checkpoint_sha, fork_checkpoint_kind FROM sessions WHERE id = ?`). When present and the run isolates: `manager.allocate_at(pool, repository, run_id, Some(&fork.base_commit))`, then if `fork.kind == CheckpointKind::Stash` run `git stash apply <checkpoint_sha>` inside the fresh worktree (via a small `worktrees::apply_stash(worktree, sha)` helper using `run_git`) — failure fails the run loudly (`could not restore the fork's checkpointed state`), never silently proceeds from the wrong tree.

### 5.8 `crates/tui/src/state.rs`

```rust
/// Esc-Esc backtrack (Adoption 05, codex app_backtrack.rs port). `primed`
/// arms on Esc with an empty composer in the base view; the second Esc opens
/// [`Overlay::Backtrack`]. Reset by any composer edit, overlay open, or
/// submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BacktrackState {
    pub primed: bool,
}
```

`AppState` gains `pub backtrack: BacktrackState`. New overlay variant:

```rust
    /// Transcript selection for Esc-Esc backtrack (Adoption 05): `nth_user`
    /// indexes the session's user messages in arrival order — i.e.
    /// [`AppState::runs`], each run contributing exactly one user message
    /// (its objective; steering markers are not independent turns, mirroring
    /// codex). Enter forks the session before the selected message and
    /// refills the composer with its text; Esc closes.
    Backtrack { nth_user: usize },
```

`input_mode()` needs no arm — the default match maps unlisted overlays to `InputMode::Normal` (↑/↓/Enter/Esc arrive as `SelectPrev`/`SelectNext`/`Expand`/`Dismiss`). `RunView.launch_checkpoint: Option<CheckpointId>` arrives with Adoption 04.

### 5.9 `crates/tui/src/action.rs`

```rust
    /// Fork the attached session before the run holding `checkpoint`
    /// (Adoption 05 / STEP 5.6). The harness sends `ForkSession` and
    /// re-attaches to the reply's new session id.
    ForkSession {
        checkpoint: codypendent_protocol::CheckpointId,
    },
```

added to `Intent`; plus `Action::SessionForkFailed { reason: String }` for the harness to report a rejection (reducer surfaces it as a notice and re-arms nothing).

### 5.10 `crates/tui/src/reduce.rs`

- **Priming** — in `input_cancel`'s `Overlay::None` arm (line ~4414):

```rust
        Overlay::None => {
            if state.composer.is_empty() {
                if state.backtrack.primed {
                    state.backtrack.primed = false;
                    open_backtrack(state);        // Overlay::Backtrack at newest
                } else if !state.runs.is_empty() {
                    state.backtrack.primed = true;
                    state.notice = Some((
                        "Esc again to edit a previous message".to_owned(),
                        state.tick + 25,
                    ));
                }
            } else {
                state.composer.clear();
                state.composer_cursor = 0;
                state.backtrack.primed = false;
            }
        }
```

with `fn open_backtrack(state)` setting `Overlay::Backtrack { nth_user: state.runs.len() - 1 }` (guard: empty runs → notice "No previous message to edit.", exactly codex's `NO_PREVIOUS_MESSAGE_TO_EDIT`).
- **Un-priming**: `Action::InputChar`/`InputBackspace`/`InputSubmit` and every overlay-opening action clear `state.backtrack.primed` (one helper `disarm_backtrack(state)` called from the composer-edit arms; a reducer test enforces it).
- **Overlay stepping**: in the `Action::SelectPrev`/`SelectNext` arms, when `Overlay::Backtrack { nth_user }`: `SelectPrev` → older (`nth_user.saturating_sub(1)`), `SelectNext` → newer (`min(nth_user + 1, runs.len() - 1)`). Clamped, exactly `step_backtrack_and_highlight`'s arithmetic.
- **Confirm**: `Action::Expand` with `Overlay::Backtrack { nth_user }`:

```rust
        let Some(run) = state.runs.get(nth_user) else { state.overlay = Overlay::None; return };
        let Some(checkpoint) = run.launch_checkpoint else {
            state.notice = Some(("no checkpoint recorded for that turn (pre-checkpoint session)".into(), state.tick + 25));
            return;
        };
        state.composer = run.objective.clone();
        state.composer_cursor = state.composer.chars().count();
        state.overlay = Overlay::None;
        state.outbox.push(Intent::ForkSession { checkpoint });
```

- **Dismiss**: `Overlay::Backtrack { .. } => Overlay::None` in the `Action::Dismiss` overlay match (and it clears `backtrack.primed`).
- **Folding `SessionForked`**: render as a `TranscriptEntry::Note { text: format!("forked from session {from_session} at checkpoint …"), .. }`-style entry at the top of the session (reuse the `NoteAppended` fold path), and set a new `AppState.forked_from: Option<SessionId>` the header can show.

### 5.11 `crates/tui/src/render.rs`

`Overlay::Backtrack` renders a centered transcript-selection panel over the base layout (same chrome as `Overlay::ConfirmCancel`): a titled list of the session's user messages (each run's `objective`, first line, truncated to width), the selected row highlighted with the standard selection style, newest at the bottom; a footer hint `↑/↓ select · Enter fork & edit · Esc close`. The status line, while `backtrack.primed`, shows the hint text from the notice (already handled by the notice mechanism — no renderer change beyond the overlay itself).

### 5.12 `crates/cli/src/tui.rs`

- `intent_to_command` (line 4063): `Intent::ForkSession { checkpoint } => CommandBody::ForkSession { session_id, checkpoint, name: None }` (the harness supplies the attached session id exactly as it does for `StartRun`).
- Handle the `Payload::SessionForked { session_id, .. }` reply: swap the attachment to the new session (the existing fresh-session swap: detach subscriptions, `AttachSession` with the new id, rebuild `AppState` from catch-up), then re-apply the composer draft (preserve `state.composer` across the swap — the refilled prompt must survive; thread it through the swap path the same way the session title does).
- A `CommandRejected` correlated to the fork sends `Action::SessionForkFailed { reason }`.

## 6. Protocol & persistence

- **Command:** `{"type":"ForkSession","session_id":"<uuid>","checkpoint":"<uuid>"}` (+ optional `"name":"…"`); Controller-only; idempotent via the standard envelope key, outcome records `created_session`.
- **Reply:** `Payload::SessionForked { command_id, session_id }`.
- **Event:** `{"type":"SessionForked","from_session":"<uuid>","checkpoint":"<uuid>"}` — appended once to the fork, never to the source. Older clients fold it as `Unknown` (placeholder), acceptable.
- **Migration:** `migrations/0036_session_forks.sql` (§5.3).
- **Error codes:** `checkpoint.not-found`, `fork.mid-run-checkpoint`, `fork.copy-failed` (retryable=false; a partial fork is inert and a retry mints a fresh session).
- **Ledger invariants preserved:** source events untouched; fork events are new rows under a new session id with sequences `1..=cut+1`; `(session_id, sequence)` uniqueness holds; `replay::project` folds the copied `SessionCreated`+history to the same projection shape it gave the source at that watermark.

## 7. Acceptance criteria

RULES (MUST):

1. Forking never writes to the source session: `SELECT COUNT(*), MAX(sequence) FROM events WHERE session_id = <source>` is identical before and after a fork.
2. The fork's ledger is the source's head verbatim modulo run-id remap: same length (`cut + 1` including the marker), same bodies field-for-field after mapping run ids, same `occurred_at`s.
3. No run id appears in two sessions: every `RunStarted` in the fork carries a run id absent from the source, and `runs` rows exist for each with `session_id = <fork>`.
4. A `SubmitUserInput` on the fork launches a run whose reconstructed prior equals the pre-fork conversation (the `session_history::continuation_prior` output over the fork's ledger).
5. Fork isolation (STEP 5.6 test, verbatim): mutating fork B's worktree leaves fork A's worktree and the source untouched; all three resolve the same pre-fork `ArtifactRef` bytes.
6. A fork's writing run carves its worktree at `fork_base_commit` (lease `base_commit` equals it, not current HEAD) and, for a `Stash` checkpoint, the worktree contains the stashed changes before the agent's first tool call.
7. Esc-Esc: first Esc with an empty composer primes (status hint visible, no overlay); second opens `Overlay::Backtrack` with the newest user message selected; typing anything disarms priming; Esc with a non-empty composer clears the draft and does NOT prime.
8. Enter in the overlay pushes exactly one `Intent::ForkSession` with the selected run's ordinal-1 checkpoint id and leaves the composer holding that run's objective text.
9. A mid-run (ordinal ≥ 2) checkpoint is rejected `fork.mid-run-checkpoint`; a checkpoint from another session is rejected `checkpoint.not-found` with the same answer as a nonexistent id.
10. House rules: migrations append-only; no event ever edited or deleted; deny-wins untouched; no `unsafe`.

RUN/EXPECT:

- RUN `cargo test -p codypendent-daemon fork` → EXPECT the §8 daemon tests green.
- RUN `cargo test -p codypendent-tui backtrack` → EXPECT the §8 reducer tests green.
- RUN `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` → EXPECT green.

## 8. Tests

`crates/daemon/src/forks.rs` (tempdir pool + real git via the worktrees test helpers):

- `fork_copies_history_up_to_the_checkpoint_with_remapped_run_ids` — seed a session with two completed runs + tool/note events, checkpoint run 2's launch, fork before run 2; assert RULES 1-3.
- `fork_before_the_first_run_yields_only_session_created_plus_marker`.
- `a_mid_run_checkpoint_is_not_forkable` — ordinal 2 → `fork.mid-run-checkpoint`.
- `a_foreign_checkpoint_answers_not_found_exactly_like_an_absent_one` — byte-compare the two `CodypendentError`s.
- `fork_title_derivation_auto_increments` — `"t (fork)"`, `"t (fork #2)"`.
- `duplicate_fork_delivery_returns_the_same_session` — idempotency replay.
- `provenance_recurses_into_the_fork_parent` (in `commands.rs` tests) — fork has no command rows; `session_run_provenance` returns the source's repository/model.

`crates/daemon/src/worktrees.rs`:

- `allocate_at_carves_the_worktree_from_the_given_base` — commit twice, allocate at the first commit, assert lease `base_commit` and worktree file content match commit 1.
- `a_fork_run_reapplies_a_stash_checkpoint` (executor-level, or a worktrees-level test of `apply_stash`) — checkpoint a dirty tree (Adoption 04), allocate at its base, apply, assert file contents equal the checkpointed state.

`crates/tui/src/reduce.rs` (pure-reducer style — construct `AppState`, feed `Action`s, assert state + `outbox`):

- `esc_on_empty_composer_primes_then_opens_backtrack` (RULE 7).
- `esc_with_a_draft_clears_it_and_does_not_prime`.
- `typing_disarms_a_primed_backtrack`.
- `backtrack_steps_are_clamped_at_both_ends` — SelectPrev at 0 stays 0; SelectNext at newest stays newest.
- `backtrack_enter_emits_fork_intent_and_refills_the_composer` (RULE 8).
- `backtrack_enter_without_a_checkpoint_notices_and_keeps_the_overlay`.
- `backtrack_dismiss_closes_and_disarms`.
- `session_forked_event_folds_to_a_visible_marker`.

`crates/protocol`: round-trips for `ForkSession` (with and without `name` — absent key must reparse to `None`), `SessionForked` event, `Payload::SessionForked`.

## 9. Gotchas

1. **Run ids must be remapped, not shared** — `runs.id` is a primary key and `projections::run_session` + the ownership gate assume one session per run. opencode's `idMap` is not cosmetic; skipping it breaks the fork at insert time (UNIQUE violation) or, worse, at authorization time.
2. **The remap match must be exhaustive over run-carrying variants.** A new `EventBody` variant with a `run_id` added later and cloned unmapped silently leaks the source's id space into forks. Keep the "no RunId inside" comment-list in `remap_body` current, and add a review checklist note to `events.rs`.
3. **`Actor::Agent` embeds a run id too** — remap it (§5.4 `remap_actor`); it is easy to remap bodies and forget actors.
4. **Approvals are not remapped and must not be.** All pre-cut approvals are resolved (the cut precedes a run launch; a pending approval belongs to a non-terminal run, which cannot precede the cut). If a session somehow holds a stale pending approval row, the fork's copied `ApprovalRequested` still names the ORIGINAL approval — resolving it would act on the source's row. Guard: `fork_session` refuses (`fork.copy-failed`, reason "pending approval in fork head") if any copied `ApprovalRequested` lacks a matching `ApprovalResolved` in the head.
5. **Only ordinal-1 checkpoints fork** — codex bails on steers ("cannot be branched independently") and cline throws on span-folded runs; mid-run forks would need mid-run transcript surgery this adoption deliberately avoids.
6. **Selection is "nth user message", not a transcript-cell index** (codex line 66-70) — in codypendent that is the run index. Do not index into `TranscriptEntry` vectors: they interleave tool cards, notes and steering markers, and mutate while streaming.
7. **Priming must disarm aggressively.** Codex resets on overlay close, thread change, and composer use; miss one reset and a stale second Esc later "randomly" opens the overlay. The reducer tests for disarm-on-typing and disarm-on-dismiss are not optional.
8. **Preserve the refilled composer across the re-attach.** The harness rebuilds `AppState` when it swaps sessions; if the draft is not threaded through, the whole point of backtrack (edit the old prompt) is lost.
9. **Fork-of-fork provenance recursion needs a depth cap** — a cycle in `forked_from_session_id` (corrupted DB) must not hang the daemon.
10. **`allocate_at` must verify the base exists** (`git cat-file -e <base>^{commit}`) — a fork whose repository has since been GC'd or re-cloned should fail with a legible error, and the checkpoint refs from Adoption 04 are exactly what prevents the GC case; do not delete them while forks may reference them.
11. **The half-copied-fork crash window** (§5.5) is accepted and documented: an orphan fork session with a partial ledger and no marker event. It owns no runs in flight and no worktrees; attach shows a truncated transcript. Do not try to make the copy one giant transaction through `append_event` without measuring — sessions can be large — but DO order writes so the marker is last.

## 10. Out of scope

- The STEP 5.6 **comparison view** (chronicle + changeset diff side by side) and a graphical fork **tree** — this adoption ships only the "forked from …" marker + header line. Explicit deviation from STEP 5.6's full text; the command/data model it needs is all here.
- Forking at mid-run (ordinal ≥ 2) checkpoints.
- Fork deletion/archival, and checkpoint-ref GC tied to fork lifetimes.
- Merging a fork back / cross-fork cherry-pick.
- codex's on-demand older-history pagination inside the overlay (codypendent sessions render fully; `ReadSessionEvents` paging exists if this ever grows).
- Automatic Fleet-style decomposition (stays out until Phase 7 evaluation, per STEP 5.6).

# Adoption 06 — Server-side prompt queue with steer/edit

**Effort:** M · **Depends on:** nothing · **Reference:** reference-repos/cline/sdk/packages/core/src/runtime/turn-queue/pending-prompt-service.ts, reference-repos/cline/apps/cli/src/tui/components/queued-prompts.tsx, reference-repos/cline/apps/cli/src/tui/hooks/use-prompt-input-controller.ts
**Ported from:** cline · **Status:** ⬜ not started

## 1. Summary

Closes the open "Composer polish … queue-while-working" roadmap bullet (ROADMAP.md line 620-623). While a run is working, Enter no longer fires a message into the void: it **queues** the prompt in a **daemon-owned, durable** pending-prompt queue that survives client detach/reattach and daemon restart. Queued prompts render in a visible box above the composer: ↑/↓ selects, Tab edits in place, Enter promotes an entry to **steer** (delivered into the live run at its next safe point), a spinner marks the next-steer entry, and a context-sensitive hint line explains the keys. Queue entries drain automatically: `steer` entries feed the live run's steering channel; `queue` entries launch `SubmitUserInput` continuations one at a time whenever the session has no active run. Cancelling a run discards the remaining queue (the user said "stop the queued work", not just "stop this response").

This adoption also **fixes a real defect it sits on top of**: today the `QueueSteering` command's text never reaches a run at all (§3) — the runtime's steering channel exists but nothing wires it. The queue's steer path is that missing wire.

## 2. Reference implementation

**Queue semantics — `reference-repos/cline/sdk/packages/core/src/runtime/turn-queue/pending-prompt-service.ts`:**

- `PendingPromptEntry { id, prompt, mode?, delivery: "queue" | "steer", … }`; the queue is a single ordered list per session (line 16-27).
- `enqueue` (line 137): **dedupes by exact prompt text** — re-submitting a queued text moves/updates the existing entry instead of duplicating; `delivery === "steer"` entries `unshift` to the **front**, `queue` entries `push` to the back; a re-enqueue where either old or new delivery is steer keeps steer and goes front.
- `update` (line 59): edits prompt/mode/delivery for an id; an emptied prompt **throws** ("prompt cannot be empty"); `insertUpdatedPrompt` (line 400) repositions — newly-steer → front, formerly-steer-now-queue → back, otherwise stays at its index.
- `consumeSteer` (line 177): removes and returns the first `delivery === "steer"` entry (the live turn pulls it at a safe point); `shiftNext` (line 188) pops the front regardless of delivery (the idle drain); `requeueFront` (line 193) puts a failed entry back at the front.
- `PendingPromptsController`: every mutation `emitPrompts` — a full-snapshot `pending_prompts` broadcast (line 282-290) — "UI is a projection of truth" (review, `scratchpad/reviews/cline.md` line 30) — and then `scheduleDrain`.
- `scheduleDrain`/`drain` (line 292-354): refuses while aborting, already draining, or the agent cannot start a run; `drain` shifts the front entry, emits `pending_prompt_submitted`, sends it as a real turn; on a thrown send it **requeues at the front** and stops; on an error-finish turn the entry is *not* requeued (it ran; the error is visible) but draining stops "instead of firing the remaining queue into a failing provider"; on success it re-drains until empty unless the session failed/cancelled.
- `discardQueue` (line 276): clears everything — called only when the user aborts a queue-initiated turn ("that gesture means 'stop the queued work'"). Enqueue keeps working during the abort window (line 250-255) so a prompt typed right after Escape joins the queue instead of being dropped.

**Queue UI — `reference-repos/cline/apps/cli/src/tui/components/queued-prompts.tsx`:**

- Renders only when non-empty; a bordered box above the composer titled "Queued messages:"; each row shows `❯` on selection, a **spinner** instead of the marker when `item.steer && !editing` (the "next steer" affordance), the prompt truncated to 64 chars, and an attachment count.
- Editing swaps the row's text for an inline `<input>` prefilled with the prompt; submit confirms.
- The hint line is context-sensitive (line 30-39): no selection → `"↑ steer or edit messages"`; selected+editing → `"Enter confirm, Esc cancel"`; selected steer entry → `"Waiting. ↑/↓ navigate, Tab edit, Esc cancels turn"` (running) / `"Steered next. …"` (idle); selected queue entry → `"↑/↓ navigate, Enter steer, Tab edit, …"`.

**Input routing — `reference-repos/cline/apps/cli/src/tui/hooks/use-prompt-input-controller.ts`:**

- `handleSubmit` (line 421): Enter submits with `delivery = "queue"` **iff a run is active** (`session.isRunning ? "queue" : undefined`), plain submit otherwise; `submitPrompt(delivery)` (line 262) skips the local run-state bookkeeping for queued deliveries and returns early on `result.queued` (line 357).
- The review (`scratchpad/reviews/cline.md` lines 94, 115): input-history recall is **disabled while queued prompts exist** ("queue navigation wins"); the prioritized key router places queued-prompt selection above input history; Ctrl+S steers directly.

## 3. Current state in codypendent (verified)

- **The steering text is dropped today.** `CommandBody::QueueSteering { run_id, text }` (`crates/protocol/src/command.rs` line 197) is applied by `apply_queue_steering` (`crates/daemon/src/commands.rs` line 573), which appends `EventBody::SteeringQueued { run_id }` — an event that **carries no text** — and discards `text` (the match arm binds `run_id, ..`). The runtime has a complete consumption side: `RunContext.steering: Option<mpsc::UnboundedReceiver<String>>` (`crates/runtime/src/agent.rs` line 1147), `with_steering` (line 1213), and `drain_steering` (line 3142) draining at safe points (between nodes line 2536, around tool completion lines 2910/2935) and emitting `SteeringApplied` — but the **only** `with_steering` caller in the tree is a runtime integration test (`crates/runtime/tests/agent_it.rs` line 807). The assembly executor (`crates/codypendentd/src/executor.rs`) never attaches a channel and `RunExecutor` (`crates/daemon/src/executor.rs`) has no steer method. Net: the TUI's steering box appends a marker to the ledger and the text vanishes. (`crates/codypendentd/src/session_history.rs` line 31-39 documents the intended channel; the wire is simply missing.)
- **TUI composer behaviour**: submitting while the selected run is active pushes `Intent::QueueSteering { run_id, text }` immediately (`crates/tui/src/reduce.rs` line 5481-5484); a terminal run submits `Intent::SubmitUserInput`; no run submits `Intent::StartRun` (line 5455-5510). The `s`-key `Overlay::Steering(String)` prompt does the same (line 4810-4815). `TranscriptEntry::Steering { applied }` renders the queued→applied lifecycle (line 1936-1951).
- **Command write path**: six-step crash-consistent apply in `crates/daemon/src/commands.rs` (`CommandProcessor::apply` line 154; per-command handlers like `apply_queue_steering` build events and call `run_transaction`). Idempotency by `idempotency_key`; outcomes replay verbatim.
- **Run dispatch**: the server launches accepted runs at `crates/daemon/src/server.rs` line 2946-3043 (`executor.spawn_run(RunLaunch{…})` for `StartRun` and `SubmitUserInput`, gated on `outcome.newly_applied`); `RunExecutor` (`crates/daemon/src/executor.rs`) is the daemon→assembly seam with fire-and-forget methods (`spawn_run`, `cancel_run`, `pause_run`, `resume_run`) and per-run registries in `RuntimeExecutor` (`cancellations`, `pending_cancellations` — `crates/codypendentd/src/executor.rs` line 154-166) that a steer registry can mirror.
- **Event fan-out & catch-up**: `SubscriptionHub::subscribe(session_id)`/`publish` (`crates/daemon/src/subscriptions.rs` lines 43/54); attach catch-up replays missed events, or sends `Catchup::Snapshot { projection: SessionProjection }` beyond ~500 events — the snapshot carries `pending_approvals: Vec<PendingApprovalProjection>` (`crates/protocol/src/catchup.rs` line 41-54), the precedent for carrying pending **prompts** too.
- **Migrations**: append-only in `migrations/`; highest is `0033` (0034/0035 claimed by Adoptions 04/05).
- **Must not break**: `QueueSteering` stays on the wire (older clients send it); `SteeringQueued`/`SteeringApplied` ledger semantics and the TUI's `Steering { applied }` fold; the crash-consistent write path; the ~600 pure-reducer test idioms.

## 4. Design

- **Ownership**: the queue is **daemon state**, durable in a `pending_prompts` table (migration 0036), one ordered list per session. Clients only send commands and render snapshots — cline's "UI is a projection of truth", upgraded from cline's in-memory server state to SQLite so the queue survives daemon restarts too.
- **Protocol**: three mutating commands — `QueuePrompt` (enqueue with `delivery`), `UpdateQueuedPrompt` (edit text/delivery in place; also the delete carrier via a `remove` flag? No — explicit `DeleteQueuedPrompt`), `PromoteQueuedPrompt` (delivery → steer, moved to the front) — plus `DeleteQueuedPrompt`. Every mutation appends **one snapshot event** `EventBody::PendingPromptsChanged { prompts }` to the session ledger in the same command transaction: latest-wins on fold, replayed by ordinary attach catch-up, and visible to every attached client. (Deviation from cline, which broadcasts transient snapshots: routing through the ledger buys durability + reattach for free at the cost of one small event row per queue edit — acceptable; queue edits are human-rate.)
- **Steer delivery** (the missing wire): `RunExecutor` gains `steer_run(&self, run_id: RunId, text: String)` (default no-op). `RuntimeExecutor` keeps a `steerings: Arc<Mutex<HashMap<RunId, mpsc::UnboundedSender<String>>>>` registry; `execute` creates the channel, attaches `ctx.with_steering(rx)`, registers the sender, and removes it at terminal. The daemon's drain calls `executor.steer_run` — the runtime's existing `drain_steering` then applies it at the next safe point and journals `SteeringApplied`, exactly as designed. The legacy `QueueSteering` command is **re-pointed at the queue**: the server enqueues its text as a `steer` entry (so older clients get the new behaviour, and the text finally goes somewhere).
- **Drain rules** (cline's `scheduleDrain`/`drain` ported to run/session structure):
  - A `steer` entry is consumed immediately whenever the session's selected-active run is in a live state (`Running`/`Preparing`/`WaitingForApproval`/`WaitingForUserInput`/`Paused`) — `executor.steer_run` + `SteeringQueued` event + row delete + snapshot. If no run is live, steer entries sit at the front and drain like queue entries (cline's "Steered next" idle case).
  - A `queue` entry drains only when the session has **no** non-terminal run: pop the front, synthesize a `SubmitUserInput` command (daemon-generated idempotency key `prompt-queue:<prompt_id>`, applied through `CommandProcessor::apply` with the daemon's own principal), and dispatch it through the same executor glue a client-sent command uses (§5.6 extracts that glue). One at a time; the next entry drains when that run terminates successfully.
  - Failure handling: a rejected/failed **apply** requeues the entry at the front and stops draining (cline's catch branch); a run that ends `Failed` stops draining but does **not** requeue (it ran; the failure is on the transcript); `Cancelled` **discards the whole queue** (cline's `discardQueue` on abort).
  - Trigger points: after every queue mutation, and on every terminal `RunStateChanged` observed via a `SubscriptionHub` subscription held by the drainer for sessions with non-empty queues. On daemon startup, sessions with rows drain once recovery settles.
- **TUI**: Enter while the selected run is active emits `Intent::QueuePrompt` (replacing today's direct `Intent::QueueSteering`); the `s` steering overlay emits `Intent::QueuePrompt { delivery: Steer }`. The queue renders above the composer from `AppState.pending_prompts` (folded from snapshot events); ↑ at the composer's top edge enters queue selection **instead of** history recall while the queue is non-empty (cline: queue navigation wins); ↑/↓ move, Tab edits in place, Enter promotes (or confirms an edit), Delete removes, Esc exits edit → deselects → then normal Esc behaviour. Steer entries render a spinner-marker and the hint line is context-sensitive, per queued-prompts.tsx.

## 5. Changes, file by file

No `Cargo.toml` changes (uuid/sqlx/tokio/serde already present in every touched crate).

### 5.1 `crates/protocol/src/ids.rs`

```rust
uuid_id!(PromptId);
```

### 5.2 `crates/protocol/src/run.rs`

```rust
/// How a pending prompt is delivered (Adoption 06, cline's
/// `PendingPromptDelivery`). `Steer` feeds the live run's steering channel at
/// its next safe point; `Queue` waits for the session to go idle and launches
/// a continuation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum PromptDelivery {
    Queue,
    Steer,
    #[serde(other)]
    Unknown,
}

/// One pending prompt, as carried on the `PendingPromptsChanged` snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPromptView {
    pub id: PromptId,
    pub text: String,
    pub mode: AgentMode,
    pub delivery: PromptDelivery,
}
```

### 5.3 `crates/protocol/src/command.rs`

```rust
    /// Queue a prompt on the session's server-side pending queue
    /// (Adoption 06). `delivery: Steer` targets the live run's next safe
    /// point; `Queue` waits for the session to go idle. Re-queuing text that
    /// is already queued updates the existing entry instead of duplicating
    /// it. Controller-only; blank text rejected `prompt-queue.empty`.
    QueuePrompt {
        session_id: SessionId,
        text: String,
        mode: AgentMode,
        delivery: PromptDelivery,
    },
    /// Edit a queued prompt in place (Tab-edit in the queue UI). Absent
    /// fields keep their values; an emptied `text` is rejected
    /// `prompt-queue.empty`; an unknown id `prompt-queue.not-found`.
    UpdateQueuedPrompt {
        session_id: SessionId,
        prompt_id: PromptId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<PromptDelivery>,
    },
    /// Promote a queued prompt to steer: delivery becomes `Steer` and the
    /// entry moves to the front (Enter on a selected queue row).
    PromoteQueuedPrompt {
        session_id: SessionId,
        prompt_id: PromptId,
    },
    /// Remove a queued prompt without running it.
    DeleteQueuedPrompt {
        session_id: SessionId,
        prompt_id: PromptId,
    },
```

`named_resources` — all four name the session:

```rust
    Self::QueuePrompt { session_id, .. }
    | Self::UpdateQueuedPrompt { session_id, .. }
    | Self::PromoteQueuedPrompt { session_id, .. }
    | Self::DeleteQueuedPrompt { session_id, .. } => {
        vec![NamedResource::Session(*session_id)]
    }
```

### 5.4 `crates/protocol/src/events.rs` + `catchup.rs`

```rust
    /// Full snapshot of the session's server-side pending-prompt queue after
    /// a mutation (Adoption 06). Latest-wins: a client folds it by REPLACING
    /// its queue projection, so replaying history converges on the final
    /// queue. Emitted from the same transaction as the mutation it records.
    PendingPromptsChanged {
        prompts: Vec<PendingPromptView>,
    },
```

`catchup.rs` — `SessionProjection` gains (additive, defaulted — an older daemon's snapshot parses with an empty queue):

```rust
    /// Pending queued prompts at the snapshot watermark, so a >500-event
    /// catch-up still shows the queue (mirrors `pending_approvals`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_prompts: Vec<PendingPromptView>,
```

and the daemon's snapshot builder (`crates/daemon/src/server.rs`, where `SessionProjection` is assembled for `Catchup::Snapshot`) fills it from the table.

### 5.5 `migrations/0037_pending_prompts.sql`

```sql
-- Adoption 06: the daemon-owned pending-prompt queue (cline
-- pending-prompt-service.ts, made durable). One ordered list per session;
-- `position` is a sparse ordering key (renumbered on reorder), `delivery`
-- is 'queue' | 'steer'. Rows are deleted when consumed, drained, or
-- discarded; the ledger's PendingPromptsChanged snapshots are the
-- client-visible history.
CREATE TABLE pending_prompts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    mode TEXT NOT NULL,
    delivery TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_pending_prompts_session ON pending_prompts (session_id, position);
```

(Renumber to next-free if adoptions land out of order.)

### 5.6 `crates/daemon/src/prompt_queue.rs` (new module)

Pure queue mechanics over the table (each function takes `&mut SqliteConnection` so callers compose them into the command transaction), porting the service verbatim:

```rust
pub struct QueueEntry {            // row mirror
    pub id: PromptId,
    pub session_id: SessionId,
    pub position: i64,
    pub text: String,
    pub mode: AgentMode,
    pub delivery: PromptDelivery,
}

/// cline `enqueue`: dedupe by exact text (update-in-place, steer wins and
/// fronts); else insert — steer at front (min(position)-1), queue at back.
pub async fn enqueue(tx: &mut SqliteConnection, session_id: SessionId,
                     text: &str, mode: AgentMode, delivery: PromptDelivery)
                     -> anyhow::Result<Vec<PendingPromptView>>;

/// cline `update` + `insertUpdatedPrompt`: newly-steer fronts,
/// formerly-steer-now-queue backs, otherwise keeps its position.
pub async fn update(tx: &mut SqliteConnection, session_id: SessionId,
                    prompt_id: PromptId, text: Option<&str>,
                    delivery: Option<PromptDelivery>)
                    -> anyhow::Result<Option<Vec<PendingPromptView>>>;   // None = not found

pub async fn delete(…) -> anyhow::Result<Option<Vec<PendingPromptView>>>;

/// cline `consumeSteer`: pop the first delivery='steer' row.
pub async fn consume_steer(…) -> anyhow::Result<Option<(QueueEntry, Vec<PendingPromptView>)>>;
/// cline `shiftNext`: pop the front row regardless of delivery.
pub async fn shift_next(…) -> anyhow::Result<Option<(QueueEntry, Vec<PendingPromptView>)>>;
/// cline `requeueFront`.
pub async fn requeue_front(…, entry: &QueueEntry) -> anyhow::Result<Vec<PendingPromptView>>;
/// cline `discardQueue` (run cancelled).
pub async fn clear(…) -> anyhow::Result<Vec<PendingPromptView>>;
pub async fn snapshot(pool: &SqlitePool, session_id: SessionId) -> anyhow::Result<Vec<PendingPromptView>>;
```

Plus the **drainer**:

```rust
/// Watches sessions with non-empty queues and drains them per the §4 rules.
/// One task per watched session, subscribed to the SubscriptionHub; tasks
/// stop when their queue empties. `notify(session_id)` is called after every
/// queue mutation and at startup for every session with rows.
pub struct PromptQueueDrainer {
    pool: SqlitePool,
    subscriptions: SubscriptionHub,
    /* Arc<Mutex<HashSet<SessionId>>> of live watch tasks */
}
impl PromptQueueDrainer {
    pub fn notify(&self, session_id: SessionId);
    async fn drain_once(&self, session_id: SessionId) { /* §4 rules:
        1. while a live run exists: consume_steer → executor.steer_run + journal SteeringQueued + snapshot event; repeat.
        2. if no non-terminal run: shift_next → synthesize SubmitUserInput
           (idempotency_key = format!("prompt-queue:{}", entry.id)) → apply via
           CommandProcessor + dispatch_accepted_run; on apply error requeue_front + stop.
        3. observe RunStateChanged: Completed → drain again; Failed → stop;
           Cancelled → clear() + snapshot event. */ }
}
```

The drainer needs the executor and the run-dispatch glue, so it is constructed in `server.rs` (§5.8), not free-standing.

### 5.7 `crates/daemon/src/commands.rs`

- New handlers `apply_queue_prompt` / `apply_update_queued_prompt` / `apply_promote_queued_prompt` / `apply_delete_queued_prompt`, following `apply_queue_steering`'s shape (line 573): validate (session exists, role ≥ Contributor for queueing — mirror `SubmitUserInput`'s gating; blank text → `prompt-queue.empty`), then inside the command transaction call the matching `prompt_queue` mutation and append **one** `PendingPromptsChanged { prompts }` event (actor `Actor::Client { client_id }`). Wire them into the `apply` match (line 172) and the validation match.
- `apply_queue_steering` changes: instead of dropping `text`, it enqueues `(text, mode = the run's recorded mode, delivery = Steer)` on the run's session and appends **both** `SteeringQueued { run_id }` (unchanged, keeps the TUI's marker fold working) and `PendingPromptsChanged`. The dropped-text defect ends here.

### 5.8 `crates/daemon/src/server.rs` + `crates/daemon/src/executor.rs`

- Extract the run-dispatch glue (line 2946-3043) into `async fn dispatch_accepted_run(state: &Arc<ServerState>, body: &CommandBody, outcome: &CommandOutcome)` so the drainer's synthesized `SubmitUserInput` launches exactly like a client's.
- Construct one `PromptQueueDrainer` in server assembly (where the executor/subscriptions are wired); call `drainer.notify(session_id)` after every accepted queue command and after `apply_queue_steering`; at startup, `SELECT DISTINCT session_id FROM pending_prompts` → notify each.
- On an accepted `CancelRun`, after `executor.cancel_run` (line 3053-3057): resolve the run's session, `prompt_queue::clear` + append/publish the empty snapshot (cline's discard-on-abort).
- `crates/daemon/src/executor.rs` — the seam:

```rust
    /// Deliver steering text into a live run's steering channel (Adoption 06).
    /// Fire-and-forget and idempotent-on-absence: a no-op if the run is not
    /// currently executing in this process (finished, never launched here).
    /// The runtime applies it at its next safe point and journals
    /// `SteeringApplied`.
    fn steer_run(&self, _run_id: RunId, _text: String) {}
```

### 5.9 `crates/codypendentd/src/executor.rs`

- `RuntimeExecutor` field: `steerings: Arc<Mutex<HashMap<RunId, mpsc::UnboundedSender<String>>>>` (next to `cancellations`, line 161; carried across `with_github`-style rebuilds the same way).
- In `execute` (after the `RunContext` is built, line 954-971):

```rust
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        self.steerings.lock().expect("steerings lock").insert(launch.run_id, steer_tx);
        ctx = ctx.with_steering(steer_rx);
```

and remove the entry in the same cleanup that drops the cancellation handle after the loop is terminal.
- `impl RunExecutor for RuntimeExecutor`:

```rust
    fn steer_run(&self, run_id: RunId, text: String) {
        if let Some(tx) = self.steerings.lock().expect("steerings lock").get(&run_id) {
            let _ = tx.send(text); // a closed channel = run just finished; the drainer's
                                   // run-terminal observation re-drains the entry path
        }
    }
```

### 5.10 `crates/tui/src/state.rs`

```rust
/// One pending prompt as rendered above the composer (Adoption 06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPromptCard {
    pub id: codypendent_protocol::PromptId,
    pub text: String,
    pub delivery: codypendent_protocol::PromptDelivery,
}
```

`AppState` gains:

```rust
    /// The session's server-side pending-prompt queue (latest
    /// `PendingPromptsChanged` snapshot). Rendered above the composer.
    pub pending_prompts: Vec<PendingPromptCard>,
    /// Selected queue row, when the operator stepped up into the queue.
    pub queue_selected: Option<usize>,
    /// In-place edit buffer for the selected row (Tab-edit), if editing.
    pub queue_editing: Option<String>,
```

### 5.11 `crates/tui/src/action.rs`

`Intent` additions:

```rust
    QueuePrompt {
        text: String,
        mode: codypendent_protocol::AgentMode,
        delivery: codypendent_protocol::PromptDelivery,
    },
    UpdateQueuedPrompt {
        prompt_id: codypendent_protocol::PromptId,
        text: String,
    },
    PromoteQueuedPrompt { prompt_id: codypendent_protocol::PromptId },
    DeleteQueuedPrompt { prompt_id: codypendent_protocol::PromptId },
```

(`session_id` supplied by the harness, as for `StartRun`.) `Intent::QueueSteering` stays for wire back-compat but the reducer stops emitting it.

### 5.12 `crates/tui/src/reduce.rs`

All pure; keys arrive through the existing composer mapping (`crates/tui/src/input.rs` `map_composer_key`: ↑=`HistoryPrev`, ↓=`HistoryNext`, Tab=`CyclePane`, Enter=`InputSubmit`, Esc=`InputCancel`, Delete unbound in composer — add `KeyCode::Delete => Action::DeleteEntry` or reuse an existing removal action). Reducer changes:

- **Fold** `EventBody::PendingPromptsChanged { prompts }`: replace `state.pending_prompts`, clamp `queue_selected`, drop `queue_editing` if its row vanished.
- **Submit while running** (line 5481-5484): replace the `Intent::QueueSteering` push with `Intent::QueuePrompt { text, mode: state.default_mode, delivery: PromptDelivery::Queue }` (cline: Enter queues while running). The `Overlay::Steering` submit (line 4810-4815) becomes `Intent::QueuePrompt { …, delivery: PromptDelivery::Steer }`.
- **Queue navigation**: in `composer_up` (`Action::HistoryPrev`), when the cursor is at the draft's top edge **and** `pending_prompts` is non-empty, enter/step queue selection (newest-entered first at the bottom: start at `pending_prompts.len() - 1`, step toward 0) instead of history recall — cline's "queue navigation wins". `Action::HistoryNext` steps back down and exits selection past the last row.
- **While `queue_selected` is `Some`**:
  - `CyclePane` (Tab) → `queue_editing = Some(row.text.clone())` (prefill).
  - `InputChar`/`InputBackspace` → edit `queue_editing` when editing (composer untouched).
  - `InputSubmit` (Enter) → editing: emit `Intent::UpdateQueuedPrompt { prompt_id, text: buffer }` (blank buffer → notice, no intent — the daemon would reject it anyway), clear `queue_editing`; not editing: emit `Intent::PromoteQueuedPrompt { prompt_id }`.
  - `DeleteEntry` → `Intent::DeleteQueuedPrompt`.
  - `InputCancel` (Esc) → editing: cancel the edit; else: `queue_selected = None`. (Ordering with Adoption 05: queue deselect runs **before** backtrack priming — an Esc that deselects the queue never primes.)

### 5.13 `crates/tui/src/render.rs`

New `render_pending_prompts` drawn between the transcript and the composer, only when non-empty (layout gives it `2 + len` rows, capped at ~6 with the selection kept visible): a rounded-border box titled `Queued messages:`; per row — `❯` when selected (spinner glyph from the existing tick-driven spinner set instead of `❯` for `delivery == Steer` rows), the text truncated to width−6 with `…`, the edit buffer rendered in place with a cursor when `queue_editing` targets the row; a dim hint line as the last row, context-sensitive per queued-prompts.tsx line 30-39:

- nothing selected → `↑ steer or edit messages`
- selected + editing → `Enter confirm · Esc cancel`
- selected steer row → `waiting for a safe point · ↑/↓ navigate · Tab edit`
- selected queue row → `↑/↓ navigate · Enter steer · Tab edit · Del remove`

### 5.14 `crates/cli/src/tui.rs`

`intent_to_command` (line 4063): four new arms mapping to the §5.3 commands with the harness's `session_id`.

## 6. Protocol & persistence

- **Commands** (internally tagged): `{"type":"QueuePrompt","session_id":"…","text":"also add tests","mode":{"type":"Build"},"delivery":{"type":"Queue"}}`; `{"type":"UpdateQueuedPrompt","session_id":"…","prompt_id":"…","text":"…"}` (absent optional keys omitted); `{"type":"PromoteQueuedPrompt",…}`; `{"type":"DeleteQueuedPrompt",…}`.
- **Event**: `{"type":"PendingPromptsChanged","prompts":[{"id":"…","text":"…","mode":{"type":"Build"},"delivery":{"type":"Steer"}}]}` — snapshot semantics; ordering in the array IS the queue order; empty array = empty queue. Older clients fold it as `Unknown` (placeholder) — harmless.
- **Catch-up**: `SessionProjection.pending_prompts` (additive, `#[serde(default)]`).
- **Migration**: `migrations/0037_pending_prompts.sql` (§5.5).
- **Error codes**: `prompt-queue.empty`, `prompt-queue.not-found` (both non-retryable). Synthesized drain commands use idempotency keys `prompt-queue:<prompt_id>` so a crash between apply and row-delete cannot double-run a prompt (the replayed outcome is not `newly_applied`, so no second launch — the same guard `StartRun` relies on, `crates/daemon/src/commands.rs` line 94-100).
- **Ledger compatibility**: `SteeringQueued`/`SteeringApplied` unchanged; `QueueSteering` accepted forever.

## 7. Acceptance criteria

RULES (MUST):

1. Enter while the selected run is active queues (a `pending_prompts` row + `PendingPromptsChanged` on the ledger) and does NOT launch a run or emit `Intent::QueueSteering`.
2. A `steer`-delivery prompt against a live run reaches the runtime: `SteeringApplied` appears on the ledger and the run's next model request contains the steering text (the `drain_steering` path — currently dead in production — executes).
3. A `queue`-delivery prompt drains exactly once into a `SubmitUserInput` continuation when the session's last run reaches `Completed`; with three queued prompts, three runs execute strictly serially in queue order.
4. The queue survives client reattach: detach, reattach with `last_seen_sequence` behind, and the replayed `PendingPromptsChanged` (or the snapshot's `pending_prompts` on the >500 path) reproduces the queue. It also survives a daemon restart: rows persist and drain after recovery.
5. `CancelRun` discards the session's remaining queue (empty snapshot journaled); a run ending `Failed` stops draining but leaves the queue intact; a failed drain **apply** requeues the entry at the front.
6. Enqueueing text already queued updates the existing entry (no duplicate row); promoting moves it to the front; editing to blank is rejected `prompt-queue.empty` and changes nothing.
7. Duplicate delivery of any queue command produces one effect (idempotency replay); duplicate drain dispatch launches one run (RULE via `newly_applied`).
8. Legacy `QueueSteering` from an old client steers the live run (its text is no longer dropped) and still journals `SteeringQueued`.
9. House rules: migrations append-only; deny-wins untouched; no `unsafe`; the queue never deletes user data other than its own rows on the defined discard paths.

RUN/EXPECT:

- RUN `cargo test -p codypendent-daemon prompt_queue` → EXPECT §8 daemon tests green.
- RUN `cargo test -p codypendent-tui queue` → EXPECT §8 reducer tests green.
- RUN (manual) start a long Build run, type two messages + promote one: EXPECT the box above the composer shows both, spinner on the promoted one, `SteeringApplied` lands mid-run, the other launches after completion.
- RUN `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` → EXPECT green.

## 8. Tests

`crates/daemon/src/prompt_queue.rs` (tempdir pool idiom from `ledger.rs::tests`):

- `enqueue_dedupes_by_text_and_steer_goes_front` — queue A, queue B, steer C → order C,A,B; re-queue B → still one B.
- `update_repositions_on_delivery_change` — queue→steer fronts; steer→queue backs; text-only edit keeps position.
- `update_to_blank_is_refused_and_changes_nothing`.
- `consume_steer_takes_the_first_steer_only`; `shift_next_takes_the_front`; `requeue_front_restores_order`.
- `clear_empties_and_snapshots_empty`.
- `snapshot_round_trips_through_the_event` — mutate, load the appended `PendingPromptsChanged`, assert it equals `snapshot()`.

`crates/daemon/src/commands.rs`:

- `queue_prompt_appends_one_snapshot_event_in_the_command_transaction`.
- `queue_steering_now_enqueues_a_steer_entry_and_still_journals_steering_queued` (RULE 8).
- `duplicate_queue_prompt_delivery_applies_once`.

Daemon integration (`crates/daemon/tests/` or the server test harness, with a stub `RunExecutor` recording `steer_run`/`spawn_run` calls — the existing executor-less test pattern extended):

- `a_steer_entry_reaches_steer_run_while_the_run_is_live` (RULE 2's daemon half).
- `queue_entries_drain_serially_on_completion_and_stop_on_failure` (RULES 3, 5).
- `cancel_discards_the_queue` (RULE 5).
- `drain_after_restart` — insert rows, restart the drainer, expect a dispatch (RULE 4's second half).

`crates/runtime/tests/agent_it.rs`: already proves `with_steering` → `drain_steering` → `SteeringApplied` (line 807); add `steering_sent_mid_run_lands_in_the_next_model_request` if not covered.

`crates/tui/src/reduce.rs` (pure-reducer style):

- `submit_while_running_queues_instead_of_steering` — assert `[Intent::QueuePrompt { delivery: Queue, .. }]` and NO `QueueSteering` (updates the existing test at line 10495).
- `steering_overlay_submits_a_steer_delivery_prompt`.
- `pending_prompts_changed_replaces_the_projection_and_clamps_selection`.
- `up_at_the_draft_top_enters_the_queue_when_nonempty_and_history_otherwise`.
- `tab_edits_in_place_and_enter_confirms_the_edit` — asserts `Intent::UpdateQueuedPrompt` with the buffer.
- `enter_on_a_selected_row_promotes` — `Intent::PromoteQueuedPrompt`.
- `esc_cancels_edit_then_deselects_then_falls_through` (including the Adoption-05 ordering: no backtrack priming off a queue-deselect Esc).
- `editing_a_vanished_row_is_dropped_on_snapshot` — a snapshot without the edited id clears `queue_editing`.

`crates/protocol`: round-trips for all four commands (optional-field omission included), `PendingPromptsChanged`, `PendingPromptView`, `PromptDelivery`, and the extended `SessionProjection` (a payload without `pending_prompts` parses to empty).

## 9. Gotchas

1. **The steer wire is the fix, not a refactor.** Production steering text is dropped today (§3); any test that "already passed" against `SteeringQueued` alone was proving the marker, not the delivery. RULE 2 must assert the text reaches the model request.
2. **Queue mutations must keep working during the cancel window** (cline line 250-255): a prompt typed right after Esc/cancel joins the queue rather than being dropped — so `clear` runs on the accepted `CancelRun` only, and `enqueue` is never gated on run state.
3. **Requeue only on apply failure, not on run failure** (cline line 329-339): a run that executed and failed is on the transcript; requeuing it would loop a failing provider. Stop draining instead; the rest stays queued and drains on the next enqueue/update or successful turn.
4. **Serial drain with an idempotent key.** The drain must synthesize the `SubmitUserInput` with `prompt-queue:<prompt_id>` and delete the row only after a successful apply — the replay guard (`newly_applied`) is what makes a crash between apply and delete safe. Never generate a fresh key per attempt.
5. **`steer_run` on a just-terminated run silently no-ops** (closed/absent channel). The entry was already consumed from the table — so consume the steer row only after `steer_run` had a live registry hit, or accept the documented loss and re-journal it as a `queue` retry; this spec chooses: check the registry **before** consuming (add `fn has_live_run(&self, run_id) -> bool`-style return from `steer_run` — make it return `bool`, delivered-or-not, and only consume on `true`).
6. **Snapshot events are latest-wins — the fold must replace, never merge.** Appending would duplicate entries on catch-up replay. The reducer test for replacement is mandatory.
7. **Input-history recall vs queue navigation**: cline disables history while the queue exists; port that exactly (↑ at top edge = queue when non-empty) or the two features fight for the same key.
8. **Esc ordering with Adoption 05**: queue-edit-cancel → queue-deselect → (only then) backtrack priming/draft-clear. Get this wrong and Esc-Esc primes backtrack while the user is managing the queue.
9. **The drainer must not hold locks across `.await`** — follow the approvals-waiter discipline (`crates/daemon/src/approvals.rs` module doc): registry mutexes for synchronous map ops only.
10. **`AgentMode` on queued entries**: the mode is captured at enqueue time (the composer's `default_mode`), not at drain time — an operator who flips mode after queueing gets what each entry shows, matching cline (mode rides the entry).
11. **Unbounded text**: queue rows go through the normal command frame limits (16 MiB) but render truncated at 64-ish chars; never let the render panic on multi-line text (take the first line, like the backtrack list).

## 10. Out of scope

- Attachments/images on queued prompts (cline's `userImages`/`userFiles`) — codypendent's `InputEnvelope` can ride `QueuePrompt` later as an additive field.
- A dedicated Ctrl+S "steer directly" binding (the `s` overlay already covers it); keybinding rework generally.
- Queue reordering beyond promote-to-front.
- Cross-session or workflow-run queues (workflow steering is a different surface).
- Drain-time model re-pinning (queued continuations inherit the session pin via `session_run_provenance`, unchanged).
- Retention/pruning of `PendingPromptsChanged` history (ledger growth at human queue-edit rate is negligible).

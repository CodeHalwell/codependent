# Adoption 04 — Filesystem checkpoints as stash-commits under private refs

**Effort:** M · **Depends on:** nothing · **Reference:** reference-repos/cline/sdk/packages/core/src/hooks/checkpoint-hooks.ts, reference-repos/cline/sdk/packages/core/src/session/checkpoint-restore.ts
**Ported from:** cline · **Status:** ⬜ not started

## 1. Summary

Every user turn of a writing run gets a filesystem checkpoint of the run's operating worktree: a stash-compatible snapshot commit that captures tracked changes, the index, **and untracked files as a synthesized third parent**, stored under the private ref namespace `refs/codypendent/checkpoints/<run-id>/<ordinal>` so it is GC-safe but invisible to `git stash list`, `git branch`, and every other porcelain surface the user looks at. A clean tree records HEAD instead (kind `commit`). Each checkpoint is journaled as a ledger event (`CheckpointRecorded`) plus a `run_checkpoints` projection row, giving Adoption 05 (session forking) a durable `CheckpointId` per user turn. Restore is a new `RestoreCheckpoint` command that is **approval-gated** through the existing `ApprovalBroker` and executed as a transaction: the current worktree state is first captured behind `refs/codypendent/restore-transactions/<uuid>` so a failed restore rolls back losslessly, and `git clean -fd` runs **only** when the checkpoint captured untracked files — untracked files that cannot be reconstructed are never deleted.

No shadow repositories, no `.git` copies, no new dependencies: everything is plain `git` plumbing spawned through the argument-vector discipline `crates/daemon/src/worktrees.rs` already enforces.

## 2. Reference implementation

All mechanism is in two cline files, read in full.

**Creation — `reference-repos/cline/sdk/packages/core/src/hooks/checkpoint-hooks.ts`:**

- `createWorktreeStashCommit(cwd, message)` (line 168): runs `git stash create <message>` (which snapshots tracked changes + index without touching the worktree, but omits untracked files), then `createUntrackedParentCommit`.
- `createUntrackedParentCommit(cwd)` (line 117): lists untracked-but-not-ignored files with `git ls-files --others --exclude-standard -z`; if any, creates a **temp directory** with a private index file and, with `GIT_INDEX_FILE=<tmp>/index` in the environment, runs `git add --force --pathspec-from-file <tmp>/pathspec --pathspec-file-nul` (paths fed NUL-delimited through a file so huge untracked sets cannot overflow argv), then `git write-tree`, then `git commit-tree <tree> -m "untracked files on cline checkpoint"`. The real index and worktree are never touched.
- The two are merged (line 175-204): resolve `<stash>^{tree}`, `<stash>^1` (base), `<stash>^2` (index parent), and synthesize `git commit-tree <tree> -p <base> -p <indexParent> -p <untrackedParent> -m <message>` — exactly the three-parent shape `git stash create --include-untracked` would have produced, and one `git stash apply` accepts.
- Clean tracked tree + untracked files present (line 206-247): synthesize the index parent from `HEAD^{tree}` (`git commit-tree <headTree> -p <head> -m "index on cline checkpoint"`) and then the three-parent commit, so even a "nothing tracked changed but the agent created files" state is a restorable stash.
- Clean tree, no untracked → `createCheckpoint` falls back to `git rev-parse HEAD` with `kind: "commit"` (line 358-380).
- Storage (line 397-412): `git update-ref refs/cline/checkpoints/<sessionId>/<runCount> <ref>` — the comment explains that only `refs/stash` populates `git stash list`, so any other ref path keeps the object reachable without surfacing it.
- Turn counting (line 429-478): checkpoints fire in `beforeModel` of iteration 1; `runCount` is span-aware (`countUserRunMessages`) so numbering **survives compaction**, and the durable persisted checkpoint history — not any in-memory counter — is what prevents a continuation/resume from overwriting a pre-run snapshot with the now-mutated workspace.
- `deleteCheckpointRefs` / `retainCheckpointRefs` (line 255-291) manage the namespace on session delete/retention.

**Restore — `reference-repos/cline/sdk/packages/core/src/session/checkpoint-restore.ts`:**

- `beginWorktreeRestoreTransaction(cwd)` (line 50): asserts a work tree, records `originalHead` (`git rev-parse --verify HEAD`) and the pre-existing `refs/stash` tip; runs `git stash push --include-untracked --message "cline restore transaction <uuid>"`; if a new stash appeared, `git update-ref refs/cline/restore-transactions/<uuid> <captured>` then `git stash drop stash@{0}` (the private ref holds the recovery object; the user's stash list stays clean). If moving the object fails, it immediately rolls the push back (`reset --hard` + `clean -fd` + `stash apply --index <captured>`). Returns `commit()` (delete the private ref, failures swallowed) and `rollback()` (`git reset --hard <originalHead>`; `git clean -fd`; `git stash apply --index <privateRef>`; delete the ref).
- `applyCheckpointToWorktree(cwd, checkpoint)` (line 357): verifies `<ref>^{commit}` exists; resolves the checkpoint kind — stored `kind`, else `git show -s --format=%P%x00%B <ref>` and requires **both** ≥2 parents and the checkpoint stash-message marker so an ordinary merge commit is never fed to `git stash apply` (line 305-335); restore base is `<ref>` for `commit` kind, `<ref>^1` for `stash`; probes `checkpointCapturedUntracked` = `git cat-file -e <ref>^3^{commit}`; runs `git reset --hard <restoreBase>`; runs `git clean -fd` **only if `^3` exists** (the file-length comment at line 396-403: for 2-parent stashes and HEAD fallbacks, untracked files can't be reconstructed, so deleting them would be unrecoverable data loss; `clean -fd` also leaves `.gitignore`d paths alone and clearing before apply avoids "already exists" conflicts); finally, for stash kind, `git stash apply <ref>`.
- `findCheckpointForRun` (line 202) picks the newest checkpoint at-or-before a requested turn; `trimMessagesBeforeUserRun` (line 253) refuses to split a compaction-folded turn — the precedent Adoption 05 follows for "only fork at a whole-turn boundary".

## 3. Current state in codypendent (verified)

- **Worktrees** — `crates/daemon/src/worktrees.rs`: every writing (`Build`-mode) run gets a dedicated worktree outside the repository (`WorktreeManager::allocate`, line 257: `git worktree add -b codypendent/run-<short> <path> <base>` where `<base>` = `git rev-parse HEAD`), recorded as a `workspace_leases` row with `base_commit`. Release (`WorktreeManager::release`, line 400) protects unmerged work (patch artifact + retained directory). Every git call goes through `run_git(dir, args)` (line 770): a direct `tokio::process::Command` spawn with an explicit argument vector, never a shell string. There is currently **no** environment injection support in `run_git` — the temp-index trick needs one.
- **Run binding** — `crates/codypendentd/src/executor.rs`: `bind_run_worktree` (line 2911) allocates the lease for `run_writes_to_worktree(mode)` (Build) and returns the repository root itself for read-only modes; `RuntimeExecutor::execute` builds the `RunContext` with `operating_tree` as both read and write root (line 954-961) and drives `runtime.execute_run(&driver, ctx, token)` (line 1001). This is the daemon-side "start of a user turn": each `StartRun` **and** each `SubmitUserInput` continuation launches a fresh run (server dispatch at `crates/daemon/src/server.rs` line 2946-3043), each with its own worktree carved at HEAD.
- **Mid-run user turns** — steering. `crates/runtime/src/agent.rs`: `RunContext.steering: Option<mpsc::UnboundedReceiver<String>>` (line 1147), attached via `with_steering` (line 1213); `drain_steering` (line 3142) injects queued text at safe points (between nodes, line 2536; after tool completion, lines 2910/2935) and emits `EventBody::SteeringApplied`. A steering injection is the only way new user intent enters a live run — i.e. the intra-run "user turn" boundary.
- **Ledger** — `crates/daemon/src/ledger.rs`: `append_next_event` (line 185) atomically claims the next sequence and appends; `EventBody` (`crates/protocol/src/events.rs`) is `#[non_exhaustive]` with a `#[serde(other)] Unknown` fallback, so a new event variant degrades to a placeholder on older clients (RULE 1). Projections are maintained alongside appends (`append_run_state_changed`, line 220, is the pattern: one `BEGIN IMMEDIATE` transaction covering projection update + event insert).
- **Migrations** — `migrations/` at the repo root, embedded via `sqlx::migrate!("../../migrations")` in `crates/daemon/src/db.rs` line 26. Append-only and immutable once merged (`migrations/README.md`). Highest existing number: `0033_workflow_run_owner.sql`.
- **Approvals** — `crates/daemon/src/approvals.rs`: `ApprovalBroker::request(pool, session_id, run_id, action, risk, …)` persists a pending row, appends `ApprovalRequested`, and returns the id; the caller then `await_decision`s. `ProposedAction` (`crates/protocol/src/run.rs` line 87) is `#[non_exhaustive]`; the `PublishDocument` variant is the precedent for "a client command whose destructive effect is parked and only executes on approval".
- **Must not break:** worktree allocation/release semantics and their tests (`crates/daemon/src/worktrees.rs` tests, e.g. `unmerged_commit_is_protected_on_release`), the crash-consistent command write path (`crates/daemon/src/commands.rs`), old-ledger parseability (Phase 0 event bytes must parse forever).

## 4. Design

Port cline's mechanism with codypendent's turn structure:

- **Turn mapping.** In cline one session owns one workspace and `runCount` counts user messages across the session. In codypendent each user message launches a **run** and each writing run owns a **fresh worktree carved at HEAD** — so the session-level counter maps to `(run_id, ordinal)`:
  - **Ordinal 1** is created at run launch, in the daemon-side run driver (`RuntimeExecutor::execute`), immediately after `bind_run_worktree` and before the agent loop starts. For an isolated run the tree is clean at that instant, so ordinal 1 is almost always a `commit`-kind checkpoint recording `base_commit` — cheap, deterministic, and exactly the durable "state before this user turn" Adoption 05 forks from.
  - **Ordinals 2..n** are created at each steering application (the intra-run user turn): the runtime calls a new `TurnCheckpointer` seam from `drain_steering` **before** injecting the drained text, so the checkpoint captures the workspace as it stood when the user redirected the agent.
  - Read-only runs (Explore/Ask/Plan/Review — writes policy-denied) record **no** checkpoints: there is nothing to rewind and their operating tree is the user's shared checkout.
- **Ref namespace:** `refs/codypendent/checkpoints/<run-id>/<ordinal>` where `<run-id>` is the full lowercase hyphenated `RunId` UUID (valid in refnames) and `<ordinal>` the 1-based turn ordinal. Restore transactions use `refs/codypendent/restore-transactions/<uuid>`.
- **Durability:** each checkpoint appends `EventBody::CheckpointRecorded` to the session ledger AND inserts a `run_checkpoints` row (migration 0034). The row is the queryable authority (restore, fork); the event is the client-visible record. The `UNIQUE (run_id, ordinal)` constraint is the port of cline's `alreadyCheckpointed` guard: a recovered/re-driven run must never overwrite the pre-run snapshot with the mutated workspace — a conflicting insert means "already checkpointed, skip".
- **Restore:** `CommandBody::RestoreCheckpoint { run_id, checkpoint }` (Controller role). Refused while the run's projection state is `Running`/`Preparing` (`checkpoint.run-active`) — restoring under a live agent loop would race its writes. The daemon parks a **`ProposedAction::RestoreCheckpoint`** approval (Risk High — it discards work) via `ApprovalBroker::request` and spawns a task that `await_decision`s; on `Approved` it runs the transactional restore against the checkpoint's recorded `worktree_path`; on rejection/expiry nothing is touched. Outcome (success or rolled-back failure) is journaled as a `NoteAppended` so the operator sees it in the transcript.
- **Deviations from cline, and why:**
  1. Checkpoints are keyed `(run, ordinal)` instead of `(session, runCount)` — codypendent's per-run worktrees make the run the workspace-owning unit. Cline's compaction-surviving span counting is not needed because the ledger, not the message list, is the counting authority here.
  2. Creation lives in the daemon/runtime run driver, not an agent-hook layer — codypendent has no `AgentHooks`; the launch point and the steering safe points are the exact equivalents.
  3. The stash message marker is `codypendent checkpoint run=<run-id> turn=<n>` (own prefix, same legacy-detection role in `resolve kind`).
  4. If a retained worktree has been deleted from disk by the time a restore is approved, the restore is refused (`checkpoint.worktree-missing`) rather than resurrecting a tree — resurrection is Adoption 05's fork path.

## 5. Changes, file by file

No `Cargo.toml` changes in any crate (git is spawned; uuid/chrono/sqlx/tokio already present everywhere touched).

### 5.1 `crates/protocol/src/ids.rs`

Add the id newtype (used by the event, the command, and Adoption 05):

```rust
uuid_id!(CheckpointId);
```

Export from `lib.rs` alongside the other ids.

### 5.2 `crates/protocol/src/events.rs`

New `EventBody` variant (additive; old ledger bytes unaffected; older clients render `Unknown`):

```rust
    /// A filesystem checkpoint of the run's operating worktree was recorded
    /// (Adoption 04). `ordinal` is the 1-based user-turn ordinal within the
    /// run: 1 at launch, +1 per applied steering turn. `commit` is the
    /// checkpoint object's SHA; `kind` says how to restore it (`"stash"` needs
    /// `git stash apply`, `"commit"` is a plain reset target). The ref
    /// `refs/codypendent/checkpoints/<run_id>/<ordinal>` pins the object.
    CheckpointRecorded {
        run_id: RunId,
        checkpoint_id: CheckpointId,
        ordinal: u32,
        kind: CheckpointKind,
        commit: String,
        /// The commit the run's worktree was carved from — the "state before
        /// this turn" restore/fork target for a `commit`-kind checkpoint.
        base_commit: String,
    },
    /// A checkpoint restore finished (Adoption 04). `restored` is false when
    /// the transactional restore failed and was rolled back losslessly.
    CheckpointRestored {
        run_id: RunId,
        checkpoint_id: CheckpointId,
        restored: bool,
    },
```

`CheckpointKind` lives in `crates/protocol/src/run.rs`:

```rust
/// How a filesystem checkpoint is materialized (Adoption 04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum CheckpointKind {
    /// A stash-shaped snapshot commit; restore = reset to `^1` + `stash apply`.
    Stash,
    /// A plain commit (clean tree at capture time); restore = reset to it.
    Commit,
    #[serde(other)]
    Unknown,
}
```

Extend the round-trip test list in `events.rs` `mod tests` with both variants (the existing `round_trip` helper).

### 5.3 `crates/protocol/src/command.rs`

```rust
    /// Restore a run's operating worktree to a recorded filesystem checkpoint
    /// (Adoption 04). Controller-only and **approval-gated**: the daemon parks
    /// a `ProposedAction::RestoreCheckpoint` approval and touches nothing
    /// until a human approves it; the restore itself is transactional (the
    /// current state is captured behind a private ref first and re-applied on
    /// any failure). Refused `checkpoint.run-active` while the run is live,
    /// `checkpoint.not-found` for an unknown id, and
    /// `checkpoint.worktree-missing` when the recorded worktree no longer
    /// exists on disk.
    RestoreCheckpoint {
        run_id: RunId,
        checkpoint: CheckpointId,
    },
```

`named_resources` gets an arm (the wildcard-free match will not compile until it does — that is the point of that function):

```rust
    Self::RestoreCheckpoint { run_id, .. } => vec![NamedResource::Run(*run_id)],
```

### 5.4 `crates/protocol/src/run.rs`

New `ProposedAction` variant (rendered verbatim on the approval card):

```rust
    /// Restore a run's worktree to a recorded filesystem checkpoint
    /// (Adoption 04). Destructive for work done after the checkpoint, so it is
    /// always approval-gated; the card names exactly what is rewound.
    RestoreCheckpoint {
        /// The run whose worktree is rewound (string form of the RunId).
        run_id: String,
        /// The checkpoint's turn ordinal within the run.
        ordinal: u32,
        /// The worktree directory the reset/clean/apply will run in.
        worktree: String,
        /// The checkpoint commit being restored to.
        commit: String,
    },
```

### 5.5 `migrations/0035_run_checkpoints.sql`

```sql
-- Adoption 04: per-turn filesystem checkpoints of a run's operating worktree.
-- One row per (run, turn ordinal); the git object itself is pinned by
-- refs/codypendent/checkpoints/<run_id>/<ordinal> in the run's repository.
-- `kind` is 'stash' (reset --hard to the stash base, clean -fd ONLY when the
-- snapshot carries an untracked third parent, then stash apply) or 'commit'
-- (clean tree at capture: reset --hard alone restores it).
-- UNIQUE (run_id, ordinal) is the durable "already checkpointed" guard: a
-- recovered or re-driven run must never overwrite the pre-turn snapshot with
-- the now-mutated workspace (cline checkpoint-hooks.ts, beforeModel comment).
CREATE TABLE run_checkpoints (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    repository_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (run_id, ordinal)
);
CREATE INDEX idx_run_checkpoints_run ON run_checkpoints (run_id, ordinal);
```

(If Adoptions land out of order, renumber to the next free migration at implementation time — append-only.)

### 5.6 `crates/daemon/src/worktrees.rs` — the git mechanics

Extend `run_git` with an env-carrying sibling (same spawn discipline):

```rust
/// Like [`run_git`], with extra environment variables — used only for the
/// temp-index untracked capture, where GIT_INDEX_FILE must point at a private
/// index so the real one is never touched.
async fn run_git_env<S: AsRef<OsStr>>(
    dir: &Path,
    envs: &[(&str, &OsStr)],
    args: &[S],
) -> Result<String, WorktreeError> {
    let mut command = Command::new("git");
    command.current_dir(dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    for arg in args {
        command.arg(arg);
    }
    // ... identical output handling to run_git ...
}
```

New public types and functions (module section `// --- Checkpoints (Adoption 04) ---`):

```rust
/// One recorded checkpoint, mirroring a `run_checkpoints` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCheckpoint {
    pub id: CheckpointId,
    pub run_id: RunId,
    pub ordinal: u32,
    pub kind: CheckpointKind,
    pub commit_sha: String,
    pub base_commit: String,
    pub worktree_path: PathBuf,
    pub repository_path: PathBuf,
    pub created_at: DateTime<Utc>,
}

const CHECKPOINT_MESSAGE_PREFIX: &str = "codypendent checkpoint run=";

fn checkpoint_ref(run_id: RunId, ordinal: u32) -> String {
    format!("refs/codypendent/checkpoints/{run_id}/{ordinal}")
}
```

**`create_run_checkpoint`** — the creation sequence, executed with `worktree` as cwd. Literal git invocations, in order:

```rust
/// Snapshot `worktree` as checkpoint (run_id, ordinal). Returns None when the
/// row already exists (UNIQUE guard) — never overwrites an earlier snapshot.
pub async fn create_run_checkpoint(
    pool: &SqlitePool,
    repository: &Path,   // the main checkout (owns the ref namespace)
    worktree: &Path,     // the run's operating tree (git commands run here)
    run_id: RunId,
    ordinal: u32,
) -> Result<Option<RunCheckpoint>, WorktreeError>
```

Body, step by step (each `git` call via `run_git`/`run_git_env` in `worktree`):

1. `git rev-parse HEAD` → `base` (also the ordinal-1 base_commit).
2. `git stash create codypendent checkpoint run=<run_id> turn=<ordinal>` → `stash_ref` (may be empty: clean tracked tree). NOTE: the message is ONE argument.
3. Untracked parent:
   - `git ls-files --others --exclude-standard -z` → NUL-split file list. Empty → no untracked parent.
   - Else create a temp dir (`tempfile::tempdir()` — already a dependency of the daemon's dev tree; add `tempfile` to `[dependencies]` if only in dev-deps, or use `std::env::temp_dir` + uuid subdir), write `<tmp>/pathspec` as the NUL-joined list with a trailing NUL, then with `GIT_INDEX_FILE=<tmp>/index`:
     - `git add --force --pathspec-from-file <tmp>/pathspec --pathspec-file-nul`
     - `git write-tree` → `utree`
     - `git commit-tree <utree> -m "untracked files on codypendent checkpoint"` → `untracked_parent`
   - Remove the temp dir in all paths.
4. Combine:
   - `stash_ref` set, no `untracked_parent` → checkpoint commit = `stash_ref`, kind `Stash`.
   - `stash_ref` set, `untracked_parent` set →
     - `git rev-parse <stash_ref>^{tree}` → `tree`
     - `git rev-parse <stash_ref>^1` → `stash_base`
     - `git rev-parse <stash_ref>^2` → `index_parent`
     - `git commit-tree <tree> -p <stash_base> -p <index_parent> -p <untracked_parent> -m <message>` → checkpoint commit, kind `Stash`.
   - no `stash_ref`, `untracked_parent` set (clean tracked, new files) →
     - `git rev-parse HEAD^{tree}` → `head_tree`
     - `git commit-tree <head_tree> -p <base> -m "index on codypendent checkpoint"` → `index_parent`
     - `git commit-tree <head_tree> -p <base> -p <index_parent> -p <untracked_parent> -m <message>` → checkpoint commit, kind `Stash`.
   - neither → checkpoint commit = `base`, kind `Commit` (the HEAD fallback).
5. Pin: `git update-ref refs/codypendent/checkpoints/<run_id>/<ordinal> <commit>` — run in `worktree`; worktrees share the main repository's object store and ref namespace, so the ref is visible repo-wide.
6. Insert the `run_checkpoints` row with `id = CheckpointId::new()`. On a UNIQUE violation, delete nothing and return `Ok(None)` (already checkpointed — the recovery-path guard).

**`restore_checkpoint_transactional`** — the restore sequence, executed in the checkpoint's `worktree_path`:

```rust
pub async fn restore_checkpoint_transactional(
    checkpoint: &RunCheckpoint,
) -> Result<(), WorktreeError>
```

1. Preconditions: `git rev-parse --is-inside-work-tree` must print `true`; `git cat-file -e <commit>^{commit}` must succeed; for kind `Stash` additionally `git cat-file -e <commit>^1^{commit}` and `git cat-file -e <commit>^2^{commit}`.
2. **Begin transaction** (port of `beginWorktreeRestoreTransaction`):
   - `git rev-parse --verify HEAD` → `original_head`
   - `git rev-parse --verify --quiet refs/stash` → `previous_stash` (optional; failure = none)
   - `git stash push --include-untracked --message codypendent restore transaction <uuid>`
   - `git rev-parse --verify --quiet refs/stash` → `captured` — `has_snapshot` = captured exists and ≠ `previous_stash`
   - if `has_snapshot`:
     - `git update-ref refs/codypendent/restore-transactions/<uuid> <captured>`
     - `git stash drop stash@{0}`
     - on failure of either: attempt `git reset --hard <original_head>`; `git clean -fd`; `git stash apply --index <captured>`; then propagate the original error (both failing = propagate both).
3. **Apply** (port of `applyCheckpointToWorktree`):
   - `restore_base` = `<commit>` for kind `Commit`, `<commit>^1` for kind `Stash`; verify `git cat-file -e <restore_base>^{commit}`.
   - `captured_untracked` = `git cat-file -e <commit>^3^{commit}` succeeds.
   - `git reset --hard <restore_base>`
   - **only if** `captured_untracked`: `git clean -fd` — NEVER unconditionally (RULE 3 in §7).
   - kind `Stash`: `git stash apply <commit>`
4. **Commit** on success: `git update-ref -d refs/codypendent/restore-transactions/<uuid>` (failure swallowed — a leftover private ref is harmless and keeps the recovery object reachable).
5. **Rollback** on any step-3 failure: `git reset --hard <original_head>`; `git clean -fd`; if `has_snapshot`: `git stash apply --index refs/codypendent/restore-transactions/<uuid>`; `git update-ref -d refs/codypendent/restore-transactions/<uuid>`; then return the step-3 error wrapped.

Persistence helpers in the same file (following `fetch_lease`/`insert_lease` shape): `insert_checkpoint`, `fetch_checkpoint(pool, CheckpointId)`, `fetch_run_checkpoint(pool, RunId, ordinal)`, `launch_checkpoint(pool, RunId)` (ordinal 1), plus `delete_checkpoint_refs(repo, run_id)` mirroring cline's `deleteCheckpointRefs` (`git for-each-ref --format=%(refname) refs/codypendent/checkpoints/<run_id>/` then `git update-ref -d <ref>` per line, errors swallowed) for future retention work.

### 5.7 `crates/daemon/src/checkpoints.rs` (new module) — journaling + gated restore

Registered in `lib.rs`. Two entry points:

```rust
/// Create checkpoint (run, ordinal) in `worktree`, journal it, and publish.
/// Best-effort by contract: a checkpoint failure must never fail the run —
/// log and return Ok(None).
pub async fn record_checkpoint(
    pool: &SqlitePool,
    subscriptions: &SubscriptionHub,
    session_id: SessionId,
    repository: &Path,
    worktree: &Path,
    run_id: RunId,
    ordinal: u32,
) -> anyhow::Result<Option<RunCheckpoint>> {
    let Some(cp) = crate::worktrees::create_run_checkpoint(pool, repository, worktree, run_id, ordinal).await? else {
        return Ok(None);
    };
    let event = crate::ledger::append_next_event(
        pool, session_id, &Actor::System,
        &EventBody::CheckpointRecorded {
            run_id, checkpoint_id: cp.id, ordinal,
            kind: cp.kind, commit: cp.commit_sha.clone(),
            base_commit: cp.base_commit.clone(),
        },
        Utc::now(),
    ).await?;
    subscriptions.publish(session_id, event);
    Ok(Some(cp))
}

/// Handle an accepted `RestoreCheckpoint`: validate, park the approval, and
/// spawn the await-then-restore task. Returns the parked ApprovalId.
pub async fn request_restore(
    pool: &SqlitePool,
    approvals: &ApprovalBroker,
    subscriptions: &SubscriptionHub,
    session_id: SessionId,
    checkpoint: RunCheckpoint,
) -> Result<ApprovalId, CodypendentError>
```

`request_restore` validation, in order: run's projection state must not be `Preparing`/`Running` (`checkpoint.run-active`); `checkpoint.worktree_path.exists()` (`checkpoint.worktree-missing`). Then `approvals.request(pool, session_id, checkpoint.run_id, ProposedAction::RestoreCheckpoint { run_id: checkpoint.run_id.to_string(), ordinal: checkpoint.ordinal, worktree: …, commit: … }, Risk { level: RiskLevel::High, reasons: vec!["rewinds the worktree; work after the checkpoint is discarded".into()] }, …)`, then `tokio::spawn` a task that `await_decision`s; on `Approved` calls `restore_checkpoint_transactional` and appends `CheckpointRestored { restored: true/false }` + a human-readable `NoteAppended` on failure; on rejection appends nothing beyond the broker's own `ApprovalResolved`.

### 5.8 `crates/daemon/src/server.rs`

- In the command-dispatch match (connection level, alongside the `PublishDocument`-style intercepts): handle `CommandBody::RestoreCheckpoint { run_id, checkpoint }` — Controller role required; resolve the checkpoint row via `worktrees::fetch_checkpoint`, confirm `row.run_id == run_id` (`checkpoint.not-found` otherwise — one answer for absent and mismatched, no oracle), resolve the run's session via `projections::run_session`, then `checkpoints::request_restore`. Reply `Payload::CommandAccepted` (the approval id travels on the published `ApprovalRequested` event exactly as tool approvals do).

### 5.9 `crates/runtime/src/agent.rs` — the steering-turn seam

```rust
/// Daemon-side per-turn checkpoint hook (Adoption 04). Implemented by the
/// assembly executor; called by `drain_steering` BEFORE injecting drained
/// steering text, so the snapshot is "the workspace when the user redirected
/// the agent". Fire-and-forget contract: implementations must swallow their
/// own errors — a checkpoint failure never fails a run.
pub trait TurnCheckpointer: Send + Sync {
    fn checkpoint_turn(&self, ordinal: u32);
}
```

`RunContext` gains `pub checkpointer: Option<Arc<dyn TurnCheckpointer>>` (default `None`; builder `with_checkpointer`). `drain_steering` (line 3142) keeps a `turn_ordinal: u32` counter on the run (initialized 1) and, when at least one steering text was drained, increments it and calls `checkpointer.checkpoint_turn(ordinal)` **before** pushing the first drained `TurnItem::Steering`. The call site must not `.await` model work while holding anything — the implementation spawns.

### 5.10 `crates/codypendentd/src/executor.rs`

- In `RuntimeExecutor::execute`, immediately after the worktree binding (line 952) and only when `binding.lease.is_some()` (isolated writing run): `checkpoints::record_checkpoint(&self.pool, &self.subscriptions, launch.session_id, &launch.repository, &operating_tree, launch.run_id, 1).await` (log-and-continue on error).
- Build the `TurnCheckpointer` impl over the same fields and attach: 

```rust
struct ExecCheckpointer {
    pool: SqlitePool,
    subscriptions: SubscriptionHub,
    session_id: SessionId,
    run_id: RunId,
    repository: PathBuf,
    worktree: PathBuf,
}
impl TurnCheckpointer for ExecCheckpointer {
    fn checkpoint_turn(&self, ordinal: u32) {
        let this = /* clone fields */;
        tokio::spawn(async move {
            if let Err(error) = checkpoints::record_checkpoint(/* … */, ordinal).await {
                tracing::warn!(%error, "turn checkpoint failed");
            }
        });
    }
}
// only for isolated runs:
ctx = ctx.with_checkpointer(Arc::new(ExecCheckpointer { … }));
```

### 5.11 `crates/tui/src/reduce.rs` (display only)

Fold `EventBody::CheckpointRecorded` into the run: store `ordinal == 1` ids on a new `RunView.launch_checkpoint: Option<CheckpointId>` field (`crates/tui/src/state.rs`, next to `worktree`) — consumed by Adoption 05; render nothing (checkpoints are backstage). `CheckpointRestored` renders as a `TranscriptEntry::Note`.

## 6. Protocol & persistence

- **Command** (internally tagged, `type` field): `{"type":"RestoreCheckpoint","run_id":"<uuid>","checkpoint":"<uuid>"}`.
- **Events:**
  - `{"type":"CheckpointRecorded","run_id":"…","checkpoint_id":"…","ordinal":1,"kind":{"type":"Commit"},"commit":"<sha>","base_commit":"<sha>"}`
  - `{"type":"CheckpointRestored","run_id":"…","checkpoint_id":"…","restored":true}`
- **ProposedAction:** `{"type":"RestoreCheckpoint","run_id":"…","ordinal":2,"worktree":"/path","commit":"<sha>"}` — rendered verbatim on the standard approval card; resolved through the ordinary `ResolveApproval` command, `ApprovalScope::Once` semantics unchanged.
- **Migration:** `migrations/0035_run_checkpoints.sql` (§5.5), append-only.
- **Error codes:** `checkpoint.not-found`, `checkpoint.run-active`, `checkpoint.worktree-missing` (all non-retryable), following the `document.*`/`workflow.*` naming convention.

## 7. Acceptance criteria

RULES (MUST):

1. A `Build` run's launch records checkpoint ordinal 1 before the first model call; the ref `refs/codypendent/checkpoints/<run>/1` exists in the repository and `run_checkpoints` has the row.
2. A checkpoint of a dirty worktree with untracked files produces a **three-parent** commit: `git rev-parse <sha>^3` succeeds and `git stash apply <sha>` in a clean clone reproduces tracked changes; the untracked file's content is in `<sha>^3`'s tree.
3. Restore runs `git clean -fd` **only** when `<sha>^3^{commit}` exists. A restore of a 2-parent or `commit`-kind checkpoint leaves untracked files untouched.
4. No checkpoint ever appears in `git stash list`, `git branch -a`, or `git tag` output.
5. `RestoreCheckpoint` executes **nothing** before an approval is `Approved`; a rejected or expired approval leaves the worktree byte-identical.
6. A failed restore rolls back: after an injected failure mid-restore, the worktree content (tracked + untracked) equals its pre-restore state.
7. An ordinal collision (recovery re-driving a run) never overwrites: the original snapshot's SHA is unchanged after a second `create_run_checkpoint` call with the same `(run, ordinal)`.
8. Deny-wins policy, migrations append-only, no `unsafe`, no user data deleted anywhere in this adoption (house rules, `docs/docs/build/00-how-to-use-this-guide.md` §3).

RUN/EXPECT:

- RUN `cargo test -p codypendent-daemon checkpoint` → EXPECT all new worktrees/checkpoints tests pass.
- RUN (manual) start a Build run in a scratch repo, let it create a file, `git -C <repo> for-each-ref 'refs/codypendent/checkpoints/*'` → EXPECT one ref per user turn; `git stash list` → EXPECT empty.
- RUN `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` → EXPECT green.

## 8. Tests

Follow the existing real-git tempdir idiom in `crates/daemon/src/worktrees.rs::tests` (`init_repo`, `git(dir, args)`, `test_pool`).

`crates/daemon/src/worktrees.rs` (or `checkpoints.rs` for the journaled halves):

- `a_clean_worktree_checkpoints_as_head_commit` — allocate a worktree, checkpoint ordinal 1, assert kind `Commit`, `commit_sha == base_commit`, ref exists.
- `tracked_changes_checkpoint_as_a_stash_commit` — modify a tracked file, checkpoint, assert kind `Stash`, `git rev-parse <sha>^2` succeeds, worktree still dirty afterwards (creation never touches the tree).
- `untracked_files_become_a_third_parent` — clean tracked tree + one new file: assert `<sha>^3` resolves and its tree contains the file; the real index is unchanged (`git status --porcelain` identical before/after).
- `mixed_tracked_and_untracked_checkpoint_restores_both` — dirty + untracked → checkpoint → mutate everything + add another file → `restore_checkpoint_transactional` → assert tracked content, untracked content, and the disappearance of the post-checkpoint file.
- `restore_of_a_two_parent_stash_leaves_untracked_files_alone` — checkpoint with tracked changes only; create an untracked file after; restore; assert the untracked file survives (RULE 3).
- `a_failed_restore_rolls_back_to_the_pre_restore_state` — point the checkpoint at a deliberately corrupted sha (or delete the object) so apply fails; assert tree unchanged and `refs/codypendent/restore-transactions/*` cleaned up.
- `checkpoint_ordinal_is_never_overwritten` — RULE 7.
- `checkpoint_refs_are_invisible_to_stash_list` — RULE 4.

`crates/daemon/src/checkpoints.rs`:

- `record_checkpoint_appends_the_event_and_row_once` — event body fields match the row; a second call appends nothing.
- `restore_is_parked_behind_an_approval_and_a_rejection_restores_nothing`.
- `restore_refused_while_the_run_is_running` — seed a `Running` run, expect `checkpoint.run-active`.

`crates/protocol`:

- round-trips for `RestoreCheckpoint`, `CheckpointRecorded`, `CheckpointRestored`, `ProposedAction::RestoreCheckpoint` (existing `round_trip` helpers in `command.rs`/`events.rs`).

`crates/tui/src/reduce.rs` (pure-reducer style, no I/O):

- `checkpoint_recorded_ordinal_one_lands_on_the_run_view` — feed `Action::DaemonEvent(CheckpointRecorded{ordinal:1,…})`, assert `runs[0].launch_checkpoint == Some(id)`.
- `checkpoint_restored_renders_a_note`.

## 9. Gotchas

1. **Never delete unreconstructable untracked files.** `git clean -fd` is gated on `^3^{commit}` existing — cline's file-length comment (checkpoint-restore.ts line 396-403) exists because deleting untracked files a snapshot cannot bring back is unrecoverable data loss. Also: `clean -fd` deliberately leaves `.gitignore`d paths (build output, `.env`) alone — do not "upgrade" it to `-fdx`.
2. **`git stash create` output may be empty** (clean tracked tree) and its stdout carries a trailing newline — trim before use, and treat empty as "no stash", not an error (worktrees.rs `run_git` returns raw stdout).
3. **The temp index must be a fresh path** and `GIT_INDEX_FILE` must be set only on the three untracked-capture commands. Leaking it onto any other invocation silently corrupts subsequent staging.
4. **Pathspec via file, NUL-delimited** (`--pathspec-from-file`/`--pathspec-file-nul`): huge untracked sets overflow argv otherwise; also the only safe encoding for filenames with newlines.
5. **Ordinal survival across recovery**: the UNIQUE constraint, not any in-memory counter, is the guard (cline's `alreadyCheckpointed`). Restart recovery re-driving a run must find `Ok(None)` and move on — never re-snapshot the mutated tree over the pre-turn state.
6. **Kind resolution for legacy rows** must require BOTH the merge shape (≥2 parents) and the `codypendent checkpoint run=` message marker before choosing `stash` — feeding an ordinary merge commit to `git stash apply` corrupts the tree (checkpoint-restore.ts line 329-334).
7. **Refs live in the shared repo, commands run in the worktree.** `git update-ref`/`for-each-ref` executed inside a linked worktree operate on the shared ref store — that is relied on; do not "fix" it by running them in the main checkout with a different cwd discipline.
8. **`stash apply` (not `pop`)** on restore: the checkpoint ref must stay valid for repeat restores and for Adoption 05's forks.
9. **Restore-transaction cleanup failure must not fail a successful restore** — the private ref is harmless; cline swallows the `update-ref -d` error deliberately (checkpoint-restore.ts line 126-131).
10. **Checkpoint creation is best-effort for the run**: any failure logs and continues (a run must never die because a snapshot could not be taken), but restore failures are loud (the user asked for it).

## 10. Out of scope

- Checkpoint retention/GC policy (`delete_checkpoint_refs` is provided but nothing calls it automatically).
- Checkpoints for read-only-mode runs or for the shared repository root.
- A checkpoint picker UI (Adoption 05's backtrack overlay is the consumer; `/undo`-style single-key restore can follow).
- Message-transcript trimming on restore (cline's `trimMessagesToCheckpoint`): the ledger is immutable evidence; conversation rewind is Adoption 05's fork, never event deletion.
- Windows support (worktrees.rs is already unix-first; nothing here adds a new platform constraint).

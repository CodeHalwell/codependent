# Proposal for `crates/codypendentd/src/executor.rs` from **agent-delegation**

`executor.rs` is not in my ownership row. One small change there closes the last
part of the worker-worktree leak (F15.5) for the delegation flow specifically.

## What I fixed, and where it stops

`WorktreeManager::release` (mine, `crates/daemon/src/worktrees.rs`) now reclaims
the per-run `codypendent/run-<short>` branch, gated on
`git merge-base --is-ancestor <branch> HEAD` — the same proof `allocate` already
requires. Startup reconciliation additionally sweeps branches left by *earlier*
releases (released lease + no `branch_deleted_at` + worktree gone + ancestor of
HEAD), so an install that already accumulated refs is cleaned up. Migration 0028
adds `workspace_leases.branch_deleted_at`.

That covers every **clean** worktree — which is what the review measured (four
orphan `codypendent/run-*` branches after two runs whose stub agents did not edit
files).

It does **not** cover the delegation flow, and I do not want to overstate it.
A workflow implementer node leaves its worktree **dirty** by design: it edits
files and the daemon captures the diff. `release` then takes its protective path
— export a patch artifact, **retain** the directory — and a retained directory
has the branch checked out, so the branch must stay too. A fan-out of eight
workers therefore still leaves eight retained trees and eight refs per run. I
verified this rather than assumed it; the assertion is in
`workflow_exec.rs::patch_consolidate_combines_every_workers_patch_into_one`.

## The change

`release_run_worktree` (`crates/codypendentd/src/executor.rs:2871`) hardcodes
`force = false`:

```rust
pub(crate) async fn release_run_worktree(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    manager: &WorktreeManager,
    binding: &WorktreeBinding,
) {
    if let Some(lease_id) = binding.lease {
        if let Err(error) = manager.release(pool, artifacts, lease_id, false).await {
```

For a workflow agent node the "protect unmerged work" condition is already
satisfied **before** release: `capture_proposed_patch` has written the worktree's
full diff (including `git add -N`'d new files) into the content-addressed artifact
store and `post_proposed_patch` has put it on the run's board with full
attribution. The retained tree is a second copy of bytes that are already durable
and already reachable. Please add a variant that says so explicitly:

```rust
/// Release a run's worktree whose contents are ALREADY captured durably
/// elsewhere — a workflow agent node whose diff is a posted `proposed_patch`
/// artifact. `force` is safe here for exactly that reason, and only that
/// reason: the manager still exports a patch before removing anything, and
/// still refuses to remove when that export comes back empty.
pub(crate) async fn release_captured_run_worktree(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    manager: &WorktreeManager,
    binding: &WorktreeBinding,
) {
    if let Some(lease_id) = binding.lease {
        if let Err(error) = manager.release(pool, artifacts, lease_id, true).await {
            warn!(%lease_id, %error, "could not release the run's captured worktree");
        }
    }
}
```

…and a matching `WorktreeReleaseGuard::release_captured(self)` that calls it.
I will then call it from `workflow_exec.rs` on the agent-node path **only when
`proposed_patch.is_some()`** — i.e. only when the diff provably landed as an
artifact — and the branch reclamation I already wrote takes it from there.

The `release` side needs no change: the forced path still exports a patch first
and still refuses to remove the tree if that export comes back empty, so a
capture that silently produced nothing cannot destroy anything.

## Why not just do it in my own file

The `force` flag is not reachable from `workflow_exec.rs`: the binding is owned
by `WorktreeReleaseGuard`, and both the guard and `release_run_worktree` live in
`executor.rs`. Making `release_captured` a method on `WorktreeManager` alone
would not help — the guard is what holds the lease at the call site.

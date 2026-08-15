//! Git worktree manager (STEP 1.8).
//!
//! Every writing run is isolated in a dedicated Git worktree that lives
//! **outside** the repository working tree (a sibling `codypendent-worktrees/`
//! directory), on a per-run branch `codypendent/run-<short-run-id>`. Git remains
//! the authority; the `workspace_leases` table is a durable index over it
//! ([Chapter 04](../../../docs/docs/04-agent-runtime-and-workflows.md),
//! [STEP 1.8](../../../docs/docs/build/11-phase-1-persistent-agent-slice.md)).
//!
//! Three operations make up the contract:
//! - [`WorktreeManager::allocate`] mints a lease + branch + worktree.
//! - [`WorktreeManager::release`] tears one down, but **protects unmerged work**:
//!   if the branch has commits the base does not, or the working tree is dirty,
//!   it exports a patch artifact and retains the directory unless `force` is set.
//! - [`WorktreeManager::reconcile_on_startup`] compares lease rows against
//!   `git worktree list --porcelain` and marks inconsistencies `orphaned`. It
//!   never deletes anything on startup.
//!
//! Every `git` invocation is a direct process spawn with an explicit argument
//! list — never a shell string — so repository paths can never be interpreted as
//! shell syntax.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use codypendent_protocol::{ArtifactRef, CheckpointId, CheckpointKind, DataClassification, RunId};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::process::Command;
use uuid::Uuid;

use crate::artifacts::{ArtifactStore, Provenance};

/// How long an allocated lease is considered valid. Leases are advisory records
/// over Git; the TTL exists only so the `expires_at` column is populated and a
/// future reaper can find abandoned rows.
const LEASE_TTL_HOURS: i64 = 24;

/// The write mode a lease was granted under. Phase 1 only allocates `Write`
/// leases; `Read` exists so the enum mirrors the [Chapter 14] `WorkspaceLease`
/// contract and can round-trip a `read` row written by a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseMode {
    /// The single writable lease for a worktree.
    Write,
    /// A non-exclusive read lease (unused in Phase 1).
    Read,
}

impl LeaseMode {
    fn as_db(self) -> &'static str {
        match self {
            LeaseMode::Write => "write",
            LeaseMode::Read => "read",
        }
    }

    fn from_db(s: &str) -> Self {
        match s {
            "read" => LeaseMode::Read,
            _ => LeaseMode::Write,
        }
    }
}

/// The lifecycle state of a lease row, mirroring the `state` column
/// (`active | released | orphaned`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseState {
    /// The worktree is live and owned by its run.
    Active,
    /// The lease has been torn down (directory removed, or retained with work
    /// preserved as a patch artifact).
    Released,
    /// Reconciliation found the row inconsistent with Git; needs manual review.
    Orphaned,
}

impl LeaseState {
    fn as_db(self) -> &'static str {
        match self {
            LeaseState::Active => "active",
            LeaseState::Released => "released",
            LeaseState::Orphaned => "orphaned",
        }
    }

    fn from_db(s: &str) -> Self {
        match s {
            "released" => LeaseState::Released,
            "orphaned" => LeaseState::Orphaned,
            _ => LeaseState::Active,
        }
    }
}

/// A daemon-local mirror of the [Chapter 14] `WorkspaceLease` contract, one per
/// `workspace_leases` row. `id` is a daemon-local UUID (the protocol crate does
/// not define a `WorkspaceLeaseId` newtype yet); `owner_run_id` is the typed
/// [`RunId`] the lease belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLease {
    pub id: Uuid,
    pub repository_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub owner_run_id: RunId,
    pub mode: LeaseMode,
    pub state: LeaseState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
    /// When this lease's per-run branch was reclaimed, or `None` when it has not
    /// been (so a startup sweep knows which branches are still outstanding).
    pub branch_deleted_at: Option<DateTime<Utc>>,
}

/// What [`WorktreeManager::release`] did with a lease.
#[derive(Debug, Clone)]
pub struct ReleaseOutcome {
    /// The lease that was released (always ends in [`LeaseState::Released`]).
    pub lease_id: Uuid,
    /// The run the lease belonged to, so a caller can attribute what it reports
    /// to the run (and the session) the user is watching. Carried on the outcome
    /// because the release already read the lease row: an assembly that wants to
    /// TELL somebody a worktree was retained should not have to re-query for who
    /// to tell.
    pub owner_run_id: RunId,
    /// The retained (or removed) worktree directory, so a report names the path
    /// the user will actually find on disk.
    pub worktree_path: PathBuf,
    /// The per-run branch, likewise — `branch_deleted` says whether it is still
    /// there, and this says what it is called.
    pub branch: String,
    /// `true` when the worktree directory was retained because it held work
    /// that would otherwise be lost (and `force` was not set).
    pub preserved: bool,
    /// `true` when the worktree directory was removed from disk.
    pub worktree_removed: bool,
    /// `true` when the run's `codypendent/run-<short>` branch was reclaimed.
    /// `false` when it was retained because it holds commits `HEAD` does not —
    /// the branch is never deleted with unmerged work on it.
    pub branch_deleted: bool,
    /// The exported patch artifact, present whenever unmerged commits or dirty
    /// files were detected (the safety net for "protect unmerged work").
    pub patch: Option<ArtifactRef>,
    /// Number of commits on the branch that the base commit does not contain.
    pub unmerged_commits: usize,
    /// Whether the worktree had uncommitted changes.
    pub dirty: bool,
}

/// The result of [`WorktreeManager::reconcile_on_startup`].
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// Lease ids whose worktree directory was missing; marked [`LeaseState::Orphaned`].
    pub orphaned_leases: Vec<Uuid>,
    /// Worktree directories Git still tracks that have no lease row, flagged for
    /// manual cleanup. Never auto-deleted, and never auto-inserted (their owner
    /// run is unknown, and `owner_run_id` is a non-null foreign key).
    pub adopted_orphans: Vec<PathBuf>,
    /// Per-run branches reclaimed from earlier releases that removed a worktree
    /// but left its branch behind (every release before the reclaim landed).
    /// Only branches provably contained in `HEAD` are deleted.
    pub reclaimed_branches: Vec<String>,
}

/// A structured worktree-management error. Every variant is machine-branchable;
/// raw `sqlx`/`git` failures are wrapped, never surfaced verbatim to callers.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// `path` is not a Git repository (and none of its parent directories are
    /// either). Checked up front with `git rev-parse --is-inside-work-tree`
    /// before any operation that assumes Git is there — every writing run needs
    /// an isolated worktree (STEP 1.8), so there is no path forward without one
    /// either way, but this lets the user see actionable guidance instead of the
    /// raw `git` stderr a downstream `rev-parse HEAD` would otherwise surface.
    #[error(
        "{path} is not a git repository. Codypendent isolates each Build run in a git \
         worktree, so it needs one — open Codypendent inside a git repository, or run \
         `git init` in this folder first."
    )]
    NotAGitRepository { path: PathBuf },
    /// The computed worktree path would sit inside the repository working tree.
    /// Worktrees must live outside it (STEP 1.8 requirement 1).
    #[error("worktree path {worktree} would be nested inside repository {repository}")]
    NestedWorktree {
        repository: PathBuf,
        worktree: PathBuf,
    },
    /// A second writable lease was requested for a worktree that already has an
    /// active one (STEP 1.8 requirement 4). Distinct from a raw unique-constraint
    /// error so callers can branch on it.
    #[error("worktree {worktree_path} already has an active lease")]
    LeaseConflict { worktree_path: PathBuf },
    /// Re-allocation found a leftover run branch holding commits not reachable
    /// from HEAD. Deleting it would lose work, so the allocation refuses; the
    /// branch must be merged or removed deliberately.
    #[error("branch {branch} holds unmerged work; refusing to reuse it")]
    BranchHoldsWork { branch: String },
    /// No lease row exists for the given id.
    #[error("no workspace lease with id {lease_id}")]
    LeaseNotFound { lease_id: Uuid },
    /// A `git` invocation exited non-zero.
    #[error("`{command}` failed: {stderr}")]
    Git { command: String, stderr: String },
    /// A stored lease row could not be decoded (should never happen; the daemon
    /// wrote it).
    #[error("corrupt lease row: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Wraps an [`ArtifactStore`] failure during patch export.
    #[error(transparent)]
    Artifact(anyhow::Error),
}

/// Allocates, releases, and reconciles per-run Git worktrees over the
/// `workspace_leases` table.
///
/// The default layout places worktrees in `<repo>/../codypendent-worktrees/`.
/// [`WorktreeManager::with_base`] overrides the parent directory; it exists so a
/// test can point the base *inside* the repository and prove the nested-path
/// guard rejects it — production code always uses [`WorktreeManager::new`].
#[derive(Debug, Clone, Default)]
pub struct WorktreeManager {
    base_override: Option<PathBuf>,
}

impl WorktreeManager {
    /// A manager using the normative sibling-directory layout.
    pub fn new() -> Self {
        Self {
            base_override: None,
        }
    }

    /// A manager that creates worktrees directly under `base` (as
    /// `base/run-<short-id>`) instead of the sibling layout.
    pub fn with_base(base: PathBuf) -> Self {
        Self {
            base_override: Some(base),
        }
    }

    /// Create the branch `codypendent/run-<short>` at the repository's current
    /// HEAD and a worktree checked out to it, then persist an `active` write
    /// lease. The worktree path must resolve outside the repository working tree
    /// and must not already hold an active lease.
    pub async fn allocate(
        &self,
        pool: &SqlitePool,
        repository: &Path,
        run_id: RunId,
    ) -> Result<WorkspaceLease, WorktreeError> {
        self.allocate_at(pool, repository, run_id, None).await
    }

    /// Create the branch `codypendent/run-<short>` at the repository's current
    /// HEAD (or `base` when specified) and a worktree checked out to it, then
    /// persist an `active` write lease. The worktree path must resolve outside
    /// the repository working tree and must not already hold an active lease.
    pub async fn allocate_at(
        &self,
        pool: &SqlitePool,
        repository: &Path,
        run_id: RunId,
        base: Option<&str>,
    ) -> Result<WorkspaceLease, WorktreeError> {
        let repo = tokio::fs::canonicalize(repository).await?;

        // Fail fast with actionable guidance when `repo` is not a Git repository
        // at all (nor any parent directory), rather than letting the
        // `rev-parse HEAD` below leak raw `git` stderr ("fatal: not a git
        // repository (or any of the parent directories): .git") into the run's
        // error line. Every writing run needs an isolated worktree, so a
        // non-git directory cannot proceed either way — this only changes what
        // the user sees.
        if run_git(&repo, &["rev-parse", "--is-inside-work-tree"])
            .await
            .is_err()
        {
            return Err(WorktreeError::NotAGitRepository { path: repo });
        }

        let short = short_run_id(run_id);
        let branch = format!("codypendent/run-{short}");
        let worktree_path = self.worktree_path_for(&repo, &short)?;

        // Requirement 1: worktrees live outside the repository working tree.
        ensure_outside_repository(&repo, &worktree_path)?;

        // Requirement 4: at most one active lease per worktree path. Pre-check for
        // a clean error before touching Git (the UNIQUE index is the backstop).
        if active_lease_exists(pool, &worktree_path).await? {
            return Err(WorktreeError::LeaseConflict { worktree_path });
        }

        // A prior lease on this path that is no longer active (released or
        // orphaned) blocks re-allocation twice over: its row trips the
        // UNIQUE(worktree_path) index, and its surviving branch makes
        // `worktree add -b` refuse. Clear both, but only where provably
        // lossless — and in this order: verify the branch FIRST, because a
        // `BranchHoldsWork` refusal must leave the old lease row intact (it is
        // the only metadata tying a retained worktree/branch to its owner run;
        // deleting it before refusing would downgrade the next boot's
        // reconciliation to an unassociated-orphan report). The branch itself
        // is deleted only when every commit on it is reachable from HEAD; the
        // stale row goes only once the allocation can actually proceed.
        if run_git(
            &repo,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )
        .await
        .is_ok()
        {
            match run_git(&repo, &["merge-base", "--is-ancestor", &branch, "HEAD"]).await {
                Ok(_) => {
                    run_git(&repo, &["branch", "-D", &branch]).await?;
                }
                Err(_) => {
                    return Err(WorktreeError::BranchHoldsWork { branch });
                }
            }
        }
        sqlx::query("DELETE FROM workspace_leases WHERE worktree_path = ? AND state != 'active'")
            .bind(worktree_path.to_string_lossy().as_ref())
            .execute(pool)
            .await?;

        // Record the base commit, then create branch + worktree atomically. Using
        // `add -b <branch> <path> <base>` creates the branch at HEAD (== base) and
        // checks it out into the new worktree in one step, leaving no dangling
        // branch if the add fails.
        let base_commit = match base {
            Some(b) => {
                run_git(&repo, &["cat-file", "-e", &format!("{b}^{{commit}}")]).await?;
                b.to_string()
            }
            None => run_git(&repo, &["rev-parse", "HEAD"])
                .await?
                .trim()
                .to_string(),
        };
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let add_args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.clone().into(),
            worktree_path.clone().into_os_string(),
            base_commit.clone().into(),
        ];
        run_git(&repo, &add_args).await?;

        let now = Utc::now();
        let lease = WorkspaceLease {
            id: Uuid::now_v7(),
            repository_path: repo,
            worktree_path: worktree_path.clone(),
            branch,
            base_commit,
            owner_run_id: run_id,
            mode: LeaseMode::Write,
            state: LeaseState::Active,
            created_at: now,
            expires_at: now + Duration::hours(LEASE_TTL_HOURS),
            released_at: None,
            branch_deleted_at: None,
        };

        if let Err(e) = insert_lease(pool, &lease).await {
            // Losing the insert race must not leak the just-created worktree
            // on disk until a next-boot orphan sweep: it was created moments
            // ago at base == HEAD with no work in it, so tearing it down is
            // lossless. Best-effort — a failure here only re-creates the
            // pre-existing leak, it cannot lose work.
            let _ = run_git(
                &lease.repository_path,
                &[
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    OsString::from("--force"),
                    worktree_path.clone().into_os_string(),
                ],
            )
            .await;
            let _ = run_git(&lease.repository_path, &["branch", "-D", &lease.branch]).await;

            // Backstop for a lost race on the UNIQUE(worktree_path) index.
            if let WorktreeError::Database(sqlx::Error::Database(db)) = &e {
                if db.is_unique_violation() {
                    return Err(WorktreeError::LeaseConflict { worktree_path });
                }
            }
            return Err(e);
        }

        Ok(lease)
    }

    /// Tear down a lease, protecting work that is not yet in the repository.
    ///
    /// Reconciles against `git worktree list --porcelain`, then checks for
    /// unmerged commits (`git log <base>..<branch>`) and a dirty working tree
    /// (`git status --porcelain`). If either exists and `force` is false, the
    /// combined diff is exported as a patch artifact and the directory is
    /// **retained**; the lease is still marked `released`. Otherwise the worktree
    /// is removed. This is the "worktree cleanup protects unmerged work" exit
    /// criterion.
    pub async fn release(
        &self,
        pool: &SqlitePool,
        artifacts: &ArtifactStore,
        lease_id: Uuid,
        force: bool,
    ) -> Result<ReleaseOutcome, WorktreeError> {
        let lease = fetch_lease(pool, lease_id)
            .await?
            .ok_or(WorktreeError::LeaseNotFound { lease_id })?;
        let repo = &lease.repository_path;
        let worktree = &lease.worktree_path;

        // Reconcile with Git's own view before mutating anything.
        let registered = worktree_is_registered(repo, worktree).await?;
        let worktree_present = worktree.exists();

        // Unmerged commits: on the branch but not reachable from the base commit.
        let range = format!("{}..{}", lease.base_commit, lease.branch);
        let unmerged_commits = run_git(repo, &["log", &range, "--oneline"])
            .await?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();

        // Dirty working tree (tracked modifications, staged changes, untracked).
        let dirty = if worktree_present {
            !run_git(worktree, &["status", "--porcelain"])
                .await?
                .trim()
                .is_empty()
        } else {
            false
        };

        let has_work = unmerged_commits > 0 || dirty;

        if has_work && !force {
            // Protective path: export a patch and keep the directory.
            let patch = self.export_patch(pool, artifacts, &lease).await?;
            mark_released(pool, lease_id).await?;
            return Ok(ReleaseOutcome {
                lease_id,
                owner_run_id: lease.owner_run_id,
                worktree_path: lease.worktree_path.clone(),
                branch: lease.branch.clone(),
                preserved: true,
                worktree_removed: false,
                // The tree — and so the branch — is retained precisely because
                // it holds work.
                branch_deleted: false,
                patch: Some(patch),
                unmerged_commits,
                dirty,
            });
        }

        // If we are forcibly discarding real work, still export it first so it is
        // never lost, then remove the worktree. The export must SUCCEED and be
        // NON-EMPTY before anything is deleted: a failed or empty diff for a
        // worktree that provably has work means the safety patch did not capture
        // it (corrupt base commit, git failure), and force-removing anyway would
        // destroy the only copy. In that case the worktree is preserved instead.
        let patch = if has_work {
            let exported = self.export_patch(pool, artifacts, &lease).await?;
            if exported.byte_length == 0 {
                mark_released(pool, lease_id).await?;
                return Ok(ReleaseOutcome {
                    lease_id,
                    owner_run_id: lease.owner_run_id,
                    worktree_path: lease.worktree_path.clone(),
                    branch: lease.branch.clone(),
                    preserved: true,
                    worktree_removed: false,
                    branch_deleted: false,
                    patch: None,
                    unmerged_commits,
                    dirty,
                });
            }
            Some(exported)
        } else {
            None
        };

        let mut removed = false;
        if registered {
            let mut args: Vec<OsString> = vec!["worktree".into(), "remove".into()];
            if force {
                args.push("--force".into());
            }
            args.push(worktree.clone().into_os_string());
            run_git(repo, &args).await?;
            removed = true;
        } else if worktree_present {
            tokio::fs::remove_dir_all(worktree).await?;
            removed = true;
        }

        // Reclaim the per-run branch. Removing the worktree used to leave
        // `codypendent/run-<short>` behind forever: the only branch deletion is
        // `allocate`'s reclaim, which keys on a worktree path derived from a fresh
        // run id and so never matches again. Every writing run leaked one ref, and
        // a fan-out leaked one per worker.
        //
        // Gated on the SAME test `allocate` uses — every commit on the branch is
        // reachable from HEAD — so a branch that holds work is never deleted, only
        // reported. The forced path is allowed through because it has already
        // exported a verified non-empty patch artifact, so nothing is lost.
        let branch_deleted = if removed && (!has_work || force) {
            self.reclaim_branch(pool, repo, &lease.branch, lease_id)
                .await
        } else {
            false
        };

        mark_released(pool, lease_id).await?;
        Ok(ReleaseOutcome {
            lease_id,
            owner_run_id: lease.owner_run_id,
            worktree_path: lease.worktree_path.clone(),
            branch: lease.branch.clone(),
            preserved: false,
            worktree_removed: removed,
            branch_deleted,
            patch,
            unmerged_commits,
            dirty,
        })
    }

    /// Delete `branch` when — and only when — every commit on it is reachable
    /// from the repository's `HEAD`, stamping `branch_deleted_at` on the lease so
    /// the deletion is a durable fact rather than an inference.
    ///
    /// Best-effort by design: a branch that cannot be proven merged is left
    /// alone, and a `git` failure is logged rather than propagated — a lease must
    /// still be released even if its branch cannot be reclaimed, or the ref leak
    /// would be traded for a lease leak.
    async fn reclaim_branch(
        &self,
        pool: &SqlitePool,
        repo: &Path,
        branch: &str,
        lease_id: Uuid,
    ) -> bool {
        let branch_ref = format!("refs/heads/{branch}");
        if run_git(repo, &["rev-parse", "--verify", &branch_ref])
            .await
            .is_err()
        {
            return false; // already gone
        }
        if run_git(repo, &["merge-base", "--is-ancestor", branch, "HEAD"])
            .await
            .is_err()
        {
            tracing::info!(
                %branch,
                "worker branch holds commits HEAD does not contain; retained (its work is in the exported patch artifact)"
            );
            return false;
        }
        if let Err(error) = run_git(repo, &["branch", "-D", branch]).await {
            tracing::warn!(%branch, %error, "could not delete the worker branch");
            return false;
        }
        if let Err(error) =
            sqlx::query("UPDATE workspace_leases SET branch_deleted_at = ? WHERE id = ?")
                .bind(Utc::now().to_rfc3339())
                .bind(lease_id.to_string())
                .execute(pool)
                .await
        {
            // The branch is already gone; a missing stamp only costs the audit
            // trail, so it must not fail the release.
            tracing::warn!(%branch, %error, "deleted the worker branch but could not record it");
        }
        true
    }

    /// Reconcile lease rows against reality on daemon startup. Active leases whose
    /// worktree directory has vanished are marked `orphaned`; worktrees Git still
    /// tracks with no lease row are reported for manual cleanup. Nothing is ever
    /// deleted here.
    pub async fn reconcile_on_startup(
        &self,
        pool: &SqlitePool,
    ) -> Result<ReconcileReport, WorktreeError> {
        let leases = all_leases(pool).await?;
        let mut report = ReconcileReport::default();

        // Active rows whose directory is gone become orphaned.
        for lease in &leases {
            if lease.state == LeaseState::Active && !lease.worktree_path.exists() {
                mark_orphaned(pool, lease.id).await?;
                report.orphaned_leases.push(lease.id);
            }
        }

        // Adopt tracked worktrees that have no row. Group known repositories and
        // ask Git; a repository_path that is no longer a Git repo is skipped.
        let mut repos: Vec<PathBuf> = leases.iter().map(|l| l.repository_path.clone()).collect();
        repos.sort();
        repos.dedup();
        let known: Vec<PathBuf> = leases
            .iter()
            .map(|l| canonicalize_lenient(&l.worktree_path))
            .collect();

        for repo in repos {
            let Ok(listing) = run_git(&repo, &["worktree", "list", "--porcelain"]).await else {
                continue;
            };
            // Our per-run worktrees are the ones that live under the managed base
            // directory for this repository — identify them by *path*, not branch
            // name, so a detached-HEAD worktree (branch == None) is adopted too
            // instead of being silently skipped.
            let managed_base = self
                .managed_base_for(&repo)
                .map(|b| canonicalize_lenient(&b));
            for record in parse_worktree_list(&listing) {
                let canon = canonicalize_lenient(&record.path);
                let is_ours = managed_base
                    .as_ref()
                    .is_some_and(|base| canon.starts_with(base));
                if !is_ours {
                    continue;
                }
                if !known.contains(&canon) {
                    report.adopted_orphans.push(record.path);
                }
            }
        }

        // Reclaim branches left behind by earlier releases. A release before
        // this feature removed the worktree and left `codypendent/run-<short>`
        // in the user's repository forever, so an existing install carries one
        // ref per writing run it has ever done (the review found four after two
        // small workflow runs). A row qualifies only when ALL of these hold, so
        // nothing that could still be someone's work is touched:
        //
        //   * the lease is `released` (not active, not orphaned — an orphaned
        //     row means reality disagreed with the record, which a human reads);
        //   * `branch_deleted_at` is NULL (we have not already reclaimed it);
        //   * the worktree directory is GONE (a retained tree is retained
        //     precisely because it holds unmerged work, and its branch is
        //     checked out there anyway);
        //   * `git merge-base --is-ancestor <branch> HEAD` — the same proof
        //     `allocate` and `release` require before deleting a branch.
        //
        // This is the ONE thing startup reconciliation deletes, and it deletes
        // only refs it can prove HEAD already contains.
        for lease in &leases {
            if lease.state != LeaseState::Released
                || lease.branch_deleted_at.is_some()
                || lease.worktree_path.exists()
            {
                continue;
            }
            if self
                .reclaim_branch(pool, &lease.repository_path, &lease.branch, lease.id)
                .await
            {
                report.reclaimed_branches.push(lease.branch.clone());
            }
        }

        Ok(report)
    }

    /// The base directory this manager places `repo`'s worktrees under, matching
    /// [`worktree_path_for`](Self::worktree_path_for)'s layout: the override base
    /// if set, else `<repo>/../codypendent-worktrees/<repo-name>`. `None` when the
    /// repository path has no parent or final component.
    fn managed_base_for(&self, repo: &Path) -> Option<PathBuf> {
        if let Some(base) = &self.base_override {
            return Some(base.clone());
        }
        let parent = repo.parent()?;
        let repo_name = repo.file_name()?;
        Some(parent.join("codypendent-worktrees").join(repo_name))
    }

    /// Compute the worktree path for a run. Default layout is
    /// `<repo>/../codypendent-worktrees/<repo-name>/run-<short>`; an override base
    /// yields `<base>/run-<short>`.
    fn worktree_path_for(&self, repo: &Path, short: &str) -> Result<PathBuf, WorktreeError> {
        let leaf = format!("run-{short}");
        if let Some(base) = &self.base_override {
            return Ok(base.join(leaf));
        }
        let parent = repo
            .parent()
            .ok_or_else(|| WorktreeError::Corrupt("repository path has no parent".into()))?;
        let repo_name = repo.file_name().ok_or_else(|| {
            WorktreeError::Corrupt("repository path has no final component".into())
        })?;
        Ok(parent
            .join("codypendent-worktrees")
            .join(repo_name)
            .join(leaf))
    }

    /// Export the diff from the lease's base commit to the current worktree state
    /// (committed *and* uncommitted tracked changes) as a `text/x-diff` artifact.
    async fn export_patch(
        &self,
        pool: &SqlitePool,
        artifacts: &ArtifactStore,
        lease: &WorkspaceLease,
    ) -> Result<ArtifactRef, WorktreeError> {
        // `git diff <base>` in the worktree spans base -> working tree, capturing
        // both merged-into-branch commits and uncommitted edits in one patch.
        // `--binary` so binary file content survives (a plain diff records only
        // "Binary files differ" — useless for restoration). A diff FAILURE
        // propagates: swallowing it would store an empty "safety patch" and let
        // a force-release destroy the only copy of the work.
        let diff = if lease.worktree_path.exists() {
            // `git diff` omits *untracked* files, but a force-release that is
            // about to delete the worktree would then lose them silently. Mark
            // them intent-to-add first so they appear in the diff as additions
            // (the worktree is being torn down, so mutating its index is fine).
            let _ = run_git(&lease.worktree_path, &["add", "-A", "--intent-to-add"]).await;
            run_git(
                &lease.worktree_path,
                &["diff", "--binary", &lease.base_commit],
            )
            .await?
        } else {
            let range = format!("{}..{}", lease.base_commit, lease.branch);
            run_git(&lease.repository_path, &["diff", "--binary", &range]).await?
        };

        artifacts
            .put(
                pool,
                "text/x-diff",
                DataClassification::Internal,
                Provenance::system(format!("worktree-release:{}", lease.id)),
                diff.as_bytes(),
            )
            .await
            .map_err(WorktreeError::Artifact)
    }
}

/// A collision-resistant short id for a run: the **last** 12 hex characters of
/// the run id's UUIDv7. The high bits of a v7 UUID are a shared millisecond
/// clock — runs minted within ~65s share their leading hex digits — so the tail
/// (the random component) is used instead. The `codypendent/run-` prefix stays.
fn short_run_id(run_id: RunId) -> String {
    let simple = run_id.0.as_simple().to_string();
    simple[simple.len() - 12..].to_string()
}

/// Reject a worktree path that resolves inside the repository working tree.
fn ensure_outside_repository(repo: &Path, worktree: &Path) -> Result<(), WorktreeError> {
    let resolved = canonicalize_lenient(worktree);
    if resolved.starts_with(repo) {
        return Err(WorktreeError::NestedWorktree {
            repository: repo.to_path_buf(),
            worktree: worktree.to_path_buf(),
        });
    }
    Ok(())
}

/// Spawn `git` with an explicit argument vector (never a shell string) in `dir`,
/// returning stdout on success or a [`WorktreeError::Git`] on a non-zero exit.
async fn run_git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String, WorktreeError> {
    let mut command = Command::new("git");
    command.current_dir(dir);
    for arg in args {
        command.arg(arg);
    }
    let output = command.output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let printable: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();
        Err(WorktreeError::Git {
            command: format!("git {}", printable.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Like [`run_git`], with extra environment variables — used only for the
/// temp-index untracked capture, where `GIT_INDEX_FILE` must point at a private
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
    let output = command.output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let printable: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();
        Err(WorktreeError::Git {
            command: format!("git {}", printable.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// True if `worktree` appears in `git worktree list --porcelain` run from `repo`.
async fn worktree_is_registered(repo: &Path, worktree: &Path) -> Result<bool, WorktreeError> {
    let listing = run_git(repo, &["worktree", "list", "--porcelain"]).await?;
    let target = canonicalize_lenient(worktree);
    Ok(parse_worktree_list(&listing)
        .iter()
        .any(|r| canonicalize_lenient(&r.path) == target))
}

/// One record parsed from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeRecord {
    path: PathBuf,
    branch: Option<String>,
}

/// Parse the porcelain worktree listing into typed records. Records are
/// separated by blank lines; each begins with a `worktree <path>` line and may
/// carry a `branch refs/heads/<name>` line (absent for detached or bare entries).
fn parse_worktree_list(output: &str) -> Vec<WorktreeRecord> {
    let mut records = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;

    let mut flush = |path: &mut Option<PathBuf>, branch: &mut Option<String>| {
        if let Some(p) = path.take() {
            records.push(WorktreeRecord {
                path: p,
                branch: branch.take(),
            });
        } else {
            *branch = None;
        }
    };

    for line in output.lines() {
        if line.is_empty() {
            flush(&mut path, &mut branch);
        } else if let Some(rest) = line.strip_prefix("worktree ") {
            // A new record starts; flush any in-progress one first.
            flush(&mut path, &mut branch);
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.to_string());
        }
    }
    flush(&mut path, &mut branch);
    records
}

/// Canonicalize `path`, or if it does not exist yet, canonicalize the nearest
/// existing ancestor (resolving symlinks and `..` there) and re-append the
/// remainder, collapsing `.`/`..` lexically.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    let mut existing = path;
    while let Some(parent) = existing.parent() {
        if let Ok(base) = std::fs::canonicalize(parent) {
            let remainder = path.strip_prefix(parent).unwrap_or_else(|_| Path::new(""));
            let mut result = base;
            for component in remainder.components() {
                match component {
                    Component::ParentDir => {
                        result.pop();
                    }
                    Component::Normal(segment) => result.push(segment),
                    Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                }
            }
            return result;
        }
        existing = parent;
    }
    path.to_path_buf()
}

// --- Persistence -----------------------------------------------------------

type LeaseRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

const LEASE_COLUMNS: &str = "id, repository_path, worktree_path, branch, base_commit, \
     owner_run_id, mode, state, created_at, expires_at, released_at, branch_deleted_at";

async fn active_lease_exists(pool: &SqlitePool, worktree: &Path) -> Result<bool, WorktreeError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM workspace_leases WHERE worktree_path = ? AND state = 'active'",
    )
    .bind(worktree.to_string_lossy().as_ref())
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

async fn insert_lease(pool: &SqlitePool, lease: &WorkspaceLease) -> Result<(), WorktreeError> {
    sqlx::query(&format!(
        "INSERT INTO workspace_leases ({LEASE_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(lease.id.to_string())
    .bind(lease.repository_path.to_string_lossy().as_ref())
    .bind(lease.worktree_path.to_string_lossy().as_ref())
    .bind(&lease.branch)
    .bind(&lease.base_commit)
    .bind(lease.owner_run_id.to_string())
    .bind(lease.mode.as_db())
    .bind(lease.state.as_db())
    .bind(lease.created_at.to_rfc3339())
    .bind(lease.expires_at.to_rfc3339())
    .bind(lease.released_at.map(|t| t.to_rfc3339()))
    .bind(lease.branch_deleted_at.map(|t| t.to_rfc3339()))
    .execute(pool)
    .await?;
    Ok(())
}

async fn fetch_lease(
    pool: &SqlitePool,
    lease_id: Uuid,
) -> Result<Option<WorkspaceLease>, WorktreeError> {
    let row: Option<LeaseRow> = sqlx::query_as(&format!(
        "SELECT {LEASE_COLUMNS} FROM workspace_leases WHERE id = ?"
    ))
    .bind(lease_id.to_string())
    .fetch_optional(pool)
    .await?;
    row.map(lease_from_row).transpose()
}

async fn all_leases(pool: &SqlitePool) -> Result<Vec<WorkspaceLease>, WorktreeError> {
    let rows: Vec<LeaseRow> =
        sqlx::query_as(&format!("SELECT {LEASE_COLUMNS} FROM workspace_leases"))
            .fetch_all(pool)
            .await?;
    rows.into_iter().map(lease_from_row).collect()
}

async fn mark_released(pool: &SqlitePool, lease_id: Uuid) -> Result<(), WorktreeError> {
    sqlx::query("UPDATE workspace_leases SET state = 'released', released_at = ? WHERE id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(lease_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

async fn mark_orphaned(pool: &SqlitePool, lease_id: Uuid) -> Result<(), WorktreeError> {
    sqlx::query("UPDATE workspace_leases SET state = 'orphaned' WHERE id = ?")
        .bind(lease_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

fn lease_from_row(row: LeaseRow) -> Result<WorkspaceLease, WorktreeError> {
    let (
        id,
        repository_path,
        worktree_path,
        branch,
        base_commit,
        owner_run_id,
        mode,
        state,
        created_at,
        expires_at,
        released_at,
        branch_deleted_at,
    ) = row;
    Ok(WorkspaceLease {
        id: Uuid::from_str(&id).map_err(|e| WorktreeError::Corrupt(format!("id: {e}")))?,
        repository_path: PathBuf::from(repository_path),
        worktree_path: PathBuf::from(worktree_path),
        branch,
        base_commit,
        owner_run_id: RunId::from_str(&owner_run_id)
            .map_err(|e| WorktreeError::Corrupt(format!("owner_run_id: {e}")))?,
        mode: LeaseMode::from_db(&mode),
        state: LeaseState::from_db(&state),
        created_at: parse_ts(&created_at, "created_at")?,
        expires_at: parse_ts(&expires_at, "expires_at")?,
        released_at: released_at
            .map(|t| parse_ts(&t, "released_at"))
            .transpose()?,
        branch_deleted_at: branch_deleted_at
            .map(|t| parse_ts(&t, "branch_deleted_at"))
            .transpose()?,
    })
}

fn parse_ts(s: &str, field: &str) -> Result<DateTime<Utc>, WorktreeError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| WorktreeError::Corrupt(format!("{field}: {e}")))
}

// --- Checkpoints (Adoption 04) ---

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

pub const CHECKPOINT_MESSAGE_PREFIX: &str = "codypendent checkpoint run=";

pub fn checkpoint_ref(run_id: RunId, ordinal: u32) -> String {
    format!("refs/codypendent/checkpoints/{run_id}/{ordinal}")
}

/// Snapshot `worktree` as checkpoint (run_id, ordinal). Returns None when the
/// row already exists (UNIQUE guard) — never overwrites an earlier snapshot.
pub async fn create_run_checkpoint(
    pool: &SqlitePool,
    repository: &Path,
    worktree: &Path,
    run_id: RunId,
    ordinal: u32,
) -> Result<Option<RunCheckpoint>, WorktreeError> {
    // 1. Check if already checkpointed in DB (idempotent / recovery guard)
    if let Some(existing) = fetch_run_checkpoint(pool, run_id, ordinal).await? {
        return Ok(Some(existing));
    }

    // 2. Base commit
    let base = run_git(worktree, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_string();

    let message = format!("{CHECKPOINT_MESSAGE_PREFIX}{run_id} turn={ordinal}");

    // 3. Stash create
    let stash_out = run_git(worktree, &["stash", "create", &message])
        .await?
        .trim()
        .to_string();
    let stash_ref = if stash_out.is_empty() {
        None
    } else {
        Some(stash_out)
    };

    // 4. Untracked parent
    let untracked_out = run_git(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;

    let untracked_files: Vec<&str> = untracked_out
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect();

    let untracked_parent: Option<String> = if !untracked_files.is_empty() {
        let tmp_dir = tempfile::tempdir()?;
        let tmp_path = tmp_dir.path();
        let pathspec_file = tmp_path.join("pathspec");
        let index_file = tmp_path.join("index");

        let mut pathspec_bytes = Vec::new();
        for file in &untracked_files {
            pathspec_bytes.extend_from_slice(file.as_bytes());
            pathspec_bytes.push(0);
        }
        tokio::fs::write(&pathspec_file, &pathspec_bytes).await?;

        let index_os = index_file.as_os_str();
        let pathspec_os = pathspec_file.as_os_str();

        run_git_env(
            worktree,
            &[("GIT_INDEX_FILE", index_os)],
            &[
                OsStr::new("add"),
                OsStr::new("--force"),
                OsStr::new("--pathspec-from-file"),
                pathspec_os,
                OsStr::new("--pathspec-file-nul"),
            ],
        )
        .await?;

        let utree = run_git_env(worktree, &[("GIT_INDEX_FILE", index_os)], &["write-tree"])
            .await?
            .trim()
            .to_string();

        let uparent = run_git(
            worktree,
            &[
                "commit-tree",
                &utree,
                "-m",
                "untracked files on codypendent checkpoint",
            ],
        )
        .await?
        .trim()
        .to_string();

        Some(uparent)
    } else {
        None
    };

    // 5. Combine into final checkpoint commit
    let (checkpoint_commit, kind) = match (stash_ref, untracked_parent) {
        (Some(sref), None) => (sref, CheckpointKind::Stash),
        (Some(sref), Some(uparent)) => {
            let tree = run_git(worktree, &["rev-parse", &format!("{sref}^{{tree}}")])
                .await?
                .trim()
                .to_string();
            let stash_base = run_git(worktree, &["rev-parse", &format!("{sref}^1")])
                .await?
                .trim()
                .to_string();
            let index_parent = run_git(worktree, &["rev-parse", &format!("{sref}^2")])
                .await?
                .trim()
                .to_string();
            let final_commit = run_git(
                worktree,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &stash_base,
                    "-p",
                    &index_parent,
                    "-p",
                    &uparent,
                    "-m",
                    &message,
                ],
            )
            .await?
            .trim()
            .to_string();
            (final_commit, CheckpointKind::Stash)
        }
        (None, Some(uparent)) => {
            let head_tree = run_git(worktree, &["rev-parse", "HEAD^{tree}"])
                .await?
                .trim()
                .to_string();
            let index_parent = run_git(
                worktree,
                &[
                    "commit-tree",
                    &head_tree,
                    "-p",
                    &base,
                    "-m",
                    "index on codypendent checkpoint",
                ],
            )
            .await?
            .trim()
            .to_string();
            let final_commit = run_git(
                worktree,
                &[
                    "commit-tree",
                    &head_tree,
                    "-p",
                    &base,
                    "-p",
                    &index_parent,
                    "-p",
                    &uparent,
                    "-m",
                    &message,
                ],
            )
            .await?
            .trim()
            .to_string();
            (final_commit, CheckpointKind::Stash)
        }
        (None, None) => (base.clone(), CheckpointKind::Commit),
    };

    // 6. Pin under private ref in the worktree (shared with repo)
    let ref_name = checkpoint_ref(run_id, ordinal);
    run_git(worktree, &["update-ref", &ref_name, &checkpoint_commit]).await?;

    // 7. Insert DB row (with UNIQUE (run_id, ordinal) guard)
    let checkpoint = RunCheckpoint {
        id: CheckpointId::new(),
        run_id,
        ordinal,
        kind,
        commit_sha: checkpoint_commit,
        base_commit: base,
        worktree_path: worktree.to_path_buf(),
        repository_path: repository.to_path_buf(),
        created_at: Utc::now(),
    };

    insert_checkpoint(pool, &checkpoint).await?;
    Ok(Some(checkpoint))
}

/// Restore a worktree to a recorded checkpoint transactionally.
pub async fn restore_checkpoint_transactional(
    checkpoint: &RunCheckpoint,
) -> Result<(), WorktreeError> {
    let worktree = &checkpoint.worktree_path;
    let commit = &checkpoint.commit_sha;

    // 1. Preconditions
    let is_inside = run_git(worktree, &["rev-parse", "--is-inside-work-tree"])
        .await?
        .trim()
        .to_string();
    if is_inside != "true" {
        return Err(WorktreeError::NotAGitRepository {
            path: worktree.clone(),
        });
    }

    run_git(
        worktree,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .await?;

    if checkpoint.kind == CheckpointKind::Stash {
        run_git(
            worktree,
            &["cat-file", "-e", &format!("{commit}^1^{{commit}}")],
        )
        .await?;
        run_git(
            worktree,
            &["cat-file", "-e", &format!("{commit}^2^{{commit}}")],
        )
        .await?;
    }

    // 2. Begin transaction
    let original_head = run_git(worktree, &["rev-parse", "--verify", "HEAD"])
        .await?
        .trim()
        .to_string();
    let previous_stash = run_git(
        worktree,
        &["rev-parse", "--verify", "--quiet", "refs/stash"],
    )
    .await
    .ok()
    .map(|s| s.trim().to_string());

    let tx_uuid = Uuid::now_v7();
    let tx_ref = format!("refs/codypendent/restore-transactions/{tx_uuid}");
    let tx_msg = format!("codypendent restore transaction {tx_uuid}");

    let _ = run_git(
        worktree,
        &["stash", "push", "--include-untracked", "--message", &tx_msg],
    )
    .await;

    let captured = run_git(
        worktree,
        &["rev-parse", "--verify", "--quiet", "refs/stash"],
    )
    .await
    .ok()
    .map(|s| s.trim().to_string());

    let has_snapshot = match (&captured, &previous_stash) {
        (Some(c), Some(p)) => c != p,
        (Some(_), None) => true,
        _ => false,
    };

    if has_snapshot {
        if let Some(cap_sha) = &captured {
            if let Err(e) = run_git(worktree, &["update-ref", &tx_ref, cap_sha]).await {
                let _ = run_git(worktree, &["reset", "--hard", &original_head]).await;
                let _ = run_git(worktree, &["clean", "-fd"]).await;
                let _ = run_git(worktree, &["stash", "apply", "--index", cap_sha]).await;
                return Err(e);
            }
            if let Err(e) = run_git(worktree, &["stash", "drop", "stash@{0}"]).await {
                let _ = run_git(worktree, &["reset", "--hard", &original_head]).await;
                let _ = run_git(worktree, &["clean", "-fd"]).await;
                let _ = run_git(worktree, &["stash", "apply", "--index", cap_sha]).await;
                return Err(e);
            }
        }
    }

    // 3. Apply checkpoint
    let apply_res = async {
        let restore_base = match checkpoint.kind {
            CheckpointKind::Commit => commit.clone(),
            CheckpointKind::Stash => run_git(worktree, &["rev-parse", &format!("{commit}^1")])
                .await?
                .trim()
                .to_string(),
            CheckpointKind::Unknown | _ => commit.clone(),
        };

        run_git(
            worktree,
            &["cat-file", "-e", &format!("{restore_base}^{{commit}}")],
        )
        .await?;

        let captured_untracked = run_git(
            worktree,
            &["cat-file", "-e", &format!("{commit}^3^{{commit}}")],
        )
        .await
        .is_ok();

        run_git(worktree, &["reset", "--hard", &restore_base]).await?;

        // RULE 3: clean -fd ONLY if checkpoint captured untracked files!
        if captured_untracked {
            run_git(worktree, &["clean", "-fd"]).await?;
        }

        if checkpoint.kind == CheckpointKind::Stash {
            run_git(worktree, &["stash", "apply", commit]).await?;
        }

        Ok::<(), WorktreeError>(())
    }
    .await;

    // 4. Commit or Rollback
    match apply_res {
        Ok(()) => {
            let _ = run_git(worktree, &["update-ref", "-d", &tx_ref]).await;
            Ok(())
        }
        Err(apply_err) => {
            let _ = run_git(worktree, &["reset", "--hard", &original_head]).await;
            let _ = run_git(worktree, &["clean", "-fd"]).await;
            if has_snapshot {
                let _ = run_git(worktree, &["stash", "apply", "--index", &tx_ref]).await;
            }
            let _ = run_git(worktree, &["update-ref", "-d", &tx_ref]).await;
            Err(apply_err)
        }
    }
}

pub async fn insert_checkpoint(
    pool: &SqlitePool,
    checkpoint: &RunCheckpoint,
) -> Result<(), WorktreeError> {
    let id_str = checkpoint.id.to_string();
    let run_id_str = checkpoint.run_id.to_string();
    let kind_str = match checkpoint.kind {
        CheckpointKind::Stash => "stash",
        CheckpointKind::Commit => "commit",
        CheckpointKind::Unknown | _ => "unknown",
    };
    let worktree_str = checkpoint.worktree_path.to_string_lossy();
    let repo_str = checkpoint.repository_path.to_string_lossy();
    let created_at_str = checkpoint.created_at.to_rfc3339();

    sqlx::query(
        "INSERT OR IGNORE INTO run_checkpoints \
         (id, run_id, ordinal, kind, commit_sha, base_commit, worktree_path, repository_path, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_str)
    .bind(&run_id_str)
    .bind(checkpoint.ordinal as i64)
    .bind(kind_str)
    .bind(&checkpoint.commit_sha)
    .bind(&checkpoint.base_commit)
    .bind(&*worktree_str)
    .bind(&*repo_str)
    .bind(&created_at_str)
    .execute(pool)
    .await?;

    Ok(())
}

/// Apply a stash commit onto a freshly carved worktree (Adoption 05 fork replay).
pub async fn apply_stash(worktree: &Path, sha: &str) -> Result<(), WorktreeError> {
    run_git(worktree, &["stash", "apply", sha]).await?;
    Ok(())
}

type CheckpointDbRow = (
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
);

pub async fn fetch_checkpoint(
    pool: &SqlitePool,
    checkpoint_id: CheckpointId,
) -> Result<Option<RunCheckpoint>, WorktreeError> {
    let id_str = checkpoint_id.to_string();
    let row: Option<CheckpointDbRow> = sqlx::query_as(
        "SELECT id, run_id, ordinal, kind, commit_sha, base_commit, worktree_path, repository_path, created_at \
         FROM run_checkpoints WHERE id = ?",
    )
    .bind(&id_str)
    .fetch_optional(pool)
    .await?;

    row.map(decode_checkpoint_row).transpose()
}

pub async fn fetch_run_checkpoint(
    pool: &SqlitePool,
    run_id: RunId,
    ordinal: u32,
) -> Result<Option<RunCheckpoint>, WorktreeError> {
    let run_id_str = run_id.to_string();
    let row: Option<CheckpointDbRow> = sqlx::query_as(
        "SELECT id, run_id, ordinal, kind, commit_sha, base_commit, worktree_path, repository_path, created_at \
         FROM run_checkpoints WHERE run_id = ? AND ordinal = ?",
    )
    .bind(&run_id_str)
    .bind(ordinal as i64)
    .fetch_optional(pool)
    .await?;

    row.map(decode_checkpoint_row).transpose()
}

pub async fn launch_checkpoint(
    pool: &SqlitePool,
    run_id: RunId,
) -> Result<Option<RunCheckpoint>, WorktreeError> {
    fetch_run_checkpoint(pool, run_id, 1).await
}

pub async fn delete_checkpoint_refs(repository: &Path, run_id: RunId) -> Result<(), WorktreeError> {
    let prefix = format!("refs/codypendent/checkpoints/{run_id}/");
    let listing = run_git(
        repository,
        &["for-each-ref", "--format=%(refname)", &prefix],
    )
    .await
    .unwrap_or_default();

    for line in listing.lines() {
        let r = line.trim();
        if !r.is_empty() {
            let _ = run_git(repository, &["update-ref", "-d", r]).await;
        }
    }
    Ok(())
}

fn decode_checkpoint_row(
    (
        id,
        run_id,
        ordinal,
        kind,
        commit_sha,
        base_commit,
        worktree_path,
        repository_path,
        created_at,
    ): CheckpointDbRow,
) -> Result<RunCheckpoint, WorktreeError> {
    let checkpoint_id =
        CheckpointId::from_str(&id).map_err(|e| WorktreeError::Corrupt(e.to_string()))?;
    let run_id = RunId::from_str(&run_id).map_err(|e| WorktreeError::Corrupt(e.to_string()))?;
    let kind = match kind.as_str() {
        "stash" => CheckpointKind::Stash,
        "commit" => CheckpointKind::Commit,
        _ => CheckpointKind::Unknown,
    };
    let created_at = DateTime::parse_from_rfc3339(&created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| WorktreeError::Corrupt(e.to_string()))?;

    Ok(RunCheckpoint {
        id: checkpoint_id,
        run_id,
        ordinal: ordinal as u32,
        kind,
        commit_sha,
        base_commit,
        worktree_path: PathBuf::from(worktree_path),
        repository_path: PathBuf::from(repository_path),
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn test_pool(dir: &Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    /// Insert a session + run for `run_id` so a lease's `owner_run_id` foreign
    /// key resolves.
    async fn insert_run(pool: &SqlitePool, run_id: RunId) {
        let session_id = codypendent_protocol::SessionId::new();
        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(session_id.to_string())
            .bind("worktree-test")
            .bind(&now)
            .bind(&now)
            .execute(pool)
            .await
            .expect("insert session");

        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .bind("diagnose")
        .bind("Running")
        .bind("Build")
        .bind("hosted-default")
        .bind("{}")
        .execute(pool)
        .await
        .expect("insert run");
    }

    /// Insert a session + run with a fresh id, returning it.
    async fn seed_run(pool: &SqlitePool) -> RunId {
        let run_id = RunId::new();
        insert_run(pool, run_id).await;
        run_id
    }

    /// Run `git` synchronously in a test, asserting success.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Initialise a repo *inside* `parent` (so its sibling worktree tree is also
    /// under `parent` and cleaned up with the tempdir) and make an initial commit.
    fn init_repo(parent: &Path) -> PathBuf {
        let repo = parent.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@codypendent.dev"]);
        git(&repo, &["config", "user.name", "Codypendent Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        repo
    }

    async fn lease_state(pool: &SqlitePool, id: Uuid) -> LeaseState {
        let (state,): (String,) = sqlx::query_as("SELECT state FROM workspace_leases WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(pool)
            .await
            .expect("fetch state");
        LeaseState::from_db(&state)
    }

    #[tokio::test]
    async fn allocate_creates_branch_and_outside_worktree() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        assert_eq!(lease.state, LeaseState::Active);
        assert_eq!(lease.mode, LeaseMode::Write);
        assert!(lease.branch.starts_with("codypendent/run-"));
        assert!(lease.worktree_path.exists(), "worktree directory created");
        assert!(
            !lease.worktree_path.starts_with(&lease.repository_path),
            "worktree must live outside the repository tree"
        );
        assert!(!lease.base_commit.is_empty());
    }

    #[tokio::test]
    async fn allocate_against_non_git_directory_fails_with_actionable_guidance() {
        // The usability bug this guards: launching a Build run from a directory
        // that is not a Git repository must fail with a CLEAR, ACTIONABLE
        // message naming the path and pointing at `git init` — never the raw
        // `git rev-parse HEAD` stderr ("fatal: not a git repository...").
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let run_id = seed_run(&pool).await;

        // A plain directory, deliberately NOT `git init`-ed.
        let not_a_repo = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();

        let mgr = WorktreeManager::new();
        let err = mgr
            .allocate(&pool, &not_a_repo, run_id)
            .await
            .expect_err("a non-git directory must not allocate a worktree");

        assert!(
            matches!(err, WorktreeError::NotAGitRepository { .. }),
            "expected NotAGitRepository, got {err:?}"
        );

        let message = err.to_string();
        let canonical = std::fs::canonicalize(&not_a_repo).unwrap();
        assert!(
            message.contains(&canonical.display().to_string()),
            "message must name the path, got: {message}"
        );
        assert!(
            message.contains("git init"),
            "message must guide the user to `git init`, got: {message}"
        );
        assert!(
            !message.contains("rev-parse"),
            "message must not leak the raw git command, got: {message}"
        );
    }

    #[tokio::test]
    async fn unmerged_commit_is_protected_on_release() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        // Commit new work on the run branch, inside the worktree.
        let wt = &lease.worktree_path;
        std::fs::write(wt.join("feature.txt"), "new work\n").unwrap();
        git(wt, &["add", "."]);
        git(wt, &["commit", "-q", "-m", "unmerged feature"]);

        let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();

        assert!(outcome.unmerged_commits >= 1);
        assert!(outcome.preserved, "unmerged work must retain the directory");
        assert!(!outcome.worktree_removed);
        assert!(outcome.patch.is_some(), "a patch artifact must be exported");
        assert_eq!(lease_state(&pool, lease.id).await, LeaseState::Released);
        assert!(wt.exists(), "worktree directory must still exist");

        // The patch artifact row really exists in the store.
        let patch = outcome.patch.unwrap();
        assert!(store.verify(&pool, patch.id).await.unwrap());
    }

    #[tokio::test]
    async fn dirty_file_is_preserved_on_release() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        // Leave an uncommitted change to a tracked file in the worktree.
        let wt = &lease.worktree_path;
        std::fs::write(wt.join("README.md"), "hello\nlocal edit\n").unwrap();

        let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();

        assert_eq!(outcome.unmerged_commits, 0);
        assert!(outcome.dirty, "uncommitted change must be detected");
        assert!(outcome.preserved);
        assert!(!outcome.worktree_removed);
        assert!(outcome.patch.is_some());
        assert_eq!(lease_state(&pool, lease.id).await, LeaseState::Released);
        assert!(wt.exists(), "dirty worktree directory must be preserved");
    }

    #[tokio::test]
    async fn clean_release_removes_worktree() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = lease.worktree_path.clone();

        let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();

        assert!(!outcome.preserved);
        assert!(outcome.worktree_removed);
        assert!(outcome.patch.is_none());
        assert_eq!(lease_state(&pool, lease.id).await, LeaseState::Released);
        assert!(!wt.exists(), "clean worktree directory must be removed");
    }

    /// A repository's `git branch` list after a clean release. The leak this
    /// guards was verified in the review: four orphan `codypendent/run-*`
    /// branches after two small workflow runs, growing by one ref per writing
    /// run (and one per worker on a fan-out) forever.
    fn branches(repo: &Path) -> Vec<String> {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(["branch", "--format=%(refname:short)"])
            .output()
            .expect("spawn git");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect()
    }

    #[tokio::test]
    async fn a_clean_release_reclaims_the_worker_branch() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let store = ArtifactStore::new(dir.path().join("artifacts"));
        let before = branches(&repo);

        // Two workers, as a fan-out would allocate them.
        let mgr = WorktreeManager::new();
        let mut leases = Vec::new();
        for _ in 0..2 {
            let run_id = seed_run(&pool).await;
            leases.push(mgr.allocate(&pool, &repo, run_id).await.unwrap());
        }
        assert_eq!(
            branches(&repo).len(),
            before.len() + 2,
            "each worker takes a branch"
        );

        for lease in &leases {
            let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();
            assert!(outcome.branch_deleted, "a clean worker branch is reclaimed");
        }

        assert_eq!(
            branches(&repo),
            before,
            "no codypendent/run-* branch may survive a clean release"
        );
        // The reclamation is recorded, not merely inferred.
        for lease in &leases {
            let (stamp,): (Option<String>,) =
                sqlx::query_as("SELECT branch_deleted_at FROM workspace_leases WHERE id = ?")
                    .bind(lease.id.to_string())
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(stamp.is_some());
        }
    }

    /// The other half of the contract: a branch that holds commits `HEAD` does
    /// not is NEVER deleted. The worktree is retained too, so the user can still
    /// reach the work directly.
    /// Branches an OLDER build left behind — released lease, worktree gone, ref
    /// still there — are swept on startup. This is the only thing reconciliation
    /// deletes, and only when `HEAD` provably contains the branch.
    #[tokio::test]
    async fn startup_reconciliation_sweeps_branches_left_by_earlier_releases() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let store = ArtifactStore::new(dir.path().join("artifacts"));
        let before = branches(&repo);

        let mgr = WorktreeManager::new();
        let run_id = seed_run(&pool).await;
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        mgr.release(&pool, &store, lease.id, false).await.unwrap();

        // Recreate exactly the state an install upgraded from the leaking build
        // is in: the branch survives and the lease was never stamped.
        git(&repo, &["branch", &lease.branch]);
        sqlx::query("UPDATE workspace_leases SET branch_deleted_at = NULL WHERE id = ?")
            .bind(lease.id.to_string())
            .execute(&pool)
            .await
            .unwrap();
        assert!(branches(&repo).contains(&lease.branch));

        let report = mgr.reconcile_on_startup(&pool).await.unwrap();
        assert_eq!(report.reclaimed_branches, vec![lease.branch.clone()]);
        assert_eq!(branches(&repo), before);
    }

    /// …but a swept branch must still be provably merged. One that is not is
    /// reported nowhere and deleted never.
    #[tokio::test]
    async fn the_startup_sweep_never_deletes_an_unmerged_branch() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let run_id = seed_run(&pool).await;
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        // Commit on the run branch INSIDE its worktree, then remove the tree
        // behind the manager's back so only the branch (with real work) is left.
        let wt = &lease.worktree_path;
        std::fs::write(wt.join("worker.txt"), "worker output\n").unwrap();
        git(wt, &["add", "."]);
        git(wt, &["commit", "-q", "-m", "worker output"]);
        mgr.release(&pool, &store, lease.id, false).await.unwrap();
        assert!(wt.exists(), "unmerged work retains its worktree");
        git(
            &repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );

        let report = mgr.reconcile_on_startup(&pool).await.unwrap();
        assert!(report.reclaimed_branches.is_empty());
        assert!(
            branches(&repo).contains(&lease.branch),
            "a branch HEAD does not contain is never swept"
        );
    }

    #[tokio::test]
    async fn a_branch_holding_unmerged_work_is_never_reclaimed() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = &lease.worktree_path;
        std::fs::write(wt.join("feature.txt"), "worker output\n").unwrap();
        git(wt, &["add", "."]);
        git(wt, &["commit", "-q", "-m", "worker output"]);

        let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();
        assert!(outcome.preserved);
        assert!(!outcome.branch_deleted);
        assert!(
            branches(&repo).contains(&lease.branch),
            "unmerged work must keep its branch"
        );
    }

    #[tokio::test]
    async fn force_release_removes_worktree_with_unmerged_work() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = lease.worktree_path.clone();
        std::fs::write(wt.join("scratch.txt"), "throwaway\n").unwrap();

        let outcome = mgr.release(&pool, &store, lease.id, true).await.unwrap();

        assert!(
            outcome.worktree_removed,
            "force removes even dirty worktrees"
        );
        assert!(!outcome.preserved);
        assert_eq!(lease_state(&pool, lease.id).await, LeaseState::Released);
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn force_release_preserves_untracked_file_in_patch() {
        use tokio::io::AsyncReadExt;

        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = lease.worktree_path.clone();

        // The ONLY local change is a brand-new *untracked* file. `git diff <base>`
        // alone would omit it, so a force-release must intent-to-add it first.
        std::fs::write(wt.join("untracked.txt"), "precious untracked work\n").unwrap();

        let outcome = mgr.release(&pool, &store, lease.id, true).await.unwrap();

        assert!(outcome.dirty, "an untracked file makes the worktree dirty");
        assert!(outcome.worktree_removed, "force removes the worktree");
        let patch = outcome
            .patch
            .expect("force-discarding real work exports a safety patch");
        assert!(store.verify(&pool, patch.id).await.unwrap());

        // The exported patch actually contains the untracked file and its content.
        let mut file = store.open(&pool, patch.id).await.unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await.unwrap();
        let patch_text = String::from_utf8_lossy(&bytes);
        assert!(
            patch_text.contains("untracked.txt"),
            "patch must name the untracked file, got:\n{patch_text}"
        );
        assert!(
            patch_text.contains("precious untracked work"),
            "patch must carry the untracked content, got:\n{patch_text}"
        );
    }

    #[tokio::test]
    async fn stale_record_is_reconciled_to_orphaned() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let run_id = seed_run(&pool).await;

        // A lease row pointing at a directory that does not exist.
        let missing = dir.path().join("gone").join("run-deadbeef");
        let lease = WorkspaceLease {
            id: Uuid::now_v7(),
            repository_path: dir.path().join("not-a-repo"),
            worktree_path: missing.clone(),
            branch: "codypendent/run-deadbeef".to_string(),
            base_commit: "0".repeat(40),
            owner_run_id: run_id,
            mode: LeaseMode::Write,
            state: LeaseState::Active,
            created_at: Utc::now(),
            expires_at: Utc::now(),
            released_at: None,
            branch_deleted_at: None,
        };
        insert_lease(&pool, &lease).await.unwrap();

        let mgr = WorktreeManager::new();
        let report = mgr.reconcile_on_startup(&pool).await.unwrap();

        assert!(report.orphaned_leases.contains(&lease.id));
        assert_eq!(lease_state(&pool, lease.id).await, LeaseState::Orphaned);
        // Nothing was created or deleted.
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn simultaneous_allocations_get_distinct_worktrees() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());

        // Two genuinely distinct runs. The short id is the last 12 hex chars of
        // the run id — the v7 random tail — so distinct runs get distinct
        // worktrees even when their high (millisecond-clock) bits coincide. These
        // two share nothing relevant and differ in that tail (…0001 vs …0002).
        let run_a = RunId(Uuid::from_u128(0xaaaa_aaaa_0000_7000_8000_0000_0000_0001));
        let run_b = RunId(Uuid::from_u128(0xbbbb_bbbb_0000_7000_8000_0000_0000_0002));
        insert_run(&pool, run_a).await;
        insert_run(&pool, run_b).await;

        let mgr = WorktreeManager::new();
        let a = mgr.allocate(&pool, &repo, run_a).await.unwrap();
        let b = mgr.allocate(&pool, &repo, run_b).await.unwrap();

        assert_ne!(a.worktree_path, b.worktree_path);
        assert_ne!(a.branch, b.branch);
        assert_eq!(lease_state(&pool, a.id).await, LeaseState::Active);
        assert_eq!(lease_state(&pool, b.id).await, LeaseState::Active);
        assert!(a.worktree_path.exists() && b.worktree_path.exists());
    }

    #[tokio::test]
    async fn second_active_lease_for_same_worktree_conflicts() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;

        let mgr = WorktreeManager::new();
        mgr.allocate(&pool, &repo, run_id).await.unwrap();
        // The same run id maps to the same worktree path -> conflict.
        let err = mgr.allocate(&pool, &repo, run_id).await.unwrap_err();
        assert!(
            matches!(err, WorktreeError::LeaseConflict { .. }),
            "expected LeaseConflict, got {err:?}"
        );
    }

    #[tokio::test]
    async fn nested_worktree_path_is_rejected() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;

        // Force the base *inside* the repository working tree.
        let mgr = WorktreeManager::with_base(repo.join("inside-worktrees"));
        let err = mgr.allocate(&pool, &repo, run_id).await.unwrap_err();
        assert!(
            matches!(err, WorktreeError::NestedWorktree { .. }),
            "expected NestedWorktree, got {err:?}"
        );

        // The rejection happened before any row was written.
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn parse_worktree_list_extracts_records() {
        let sample = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                      worktree /wt/run-1234\nHEAD def\nbranch refs/heads/codypendent/run-1234\n\n\
                      worktree /wt/detached\nHEAD 999\ndetached\n";
        let records = parse_worktree_list(sample);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].path, PathBuf::from("/repo"));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            records[1].branch.as_deref(),
            Some("refs/heads/codypendent/run-1234")
        );
        assert_eq!(records[2].path, PathBuf::from("/wt/detached"));
        assert_eq!(records[2].branch, None);
    }

    #[tokio::test]
    async fn reallocate_after_clean_release_succeeds() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let first = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let outcome = mgr.release(&pool, &store, first.id, false).await.unwrap();
        assert!(outcome.worktree_removed);

        // The same run re-allocates the same path: the released lease row and
        // the workless leftover branch must not block it.
        let second = mgr
            .allocate(&pool, &repo, run_id)
            .await
            .expect("re-allocation after a clean release must succeed");
        assert_eq!(second.worktree_path, first.worktree_path);
        assert!(second.worktree_path.exists());
    }

    #[tokio::test]
    async fn reallocate_refuses_when_leftover_branch_holds_work() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let repo = init_repo(dir.path());
        let run_id = seed_run(&pool).await;
        let store = ArtifactStore::new(dir.path().join("artifacts"));

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        // Commit unmerged work on the run branch, then release (dir retained).
        let wt = &lease.worktree_path;
        std::fs::write(wt.join("feature.txt"), "unmerged\n").unwrap();
        git(wt, &["add", "."]);
        git(wt, &["commit", "-q", "-m", "unmerged feature"]);
        let outcome = mgr.release(&pool, &store, lease.id, false).await.unwrap();
        assert!(outcome.preserved);

        // Re-allocation must refuse rather than delete the branch's work.
        let err = mgr
            .allocate(&pool, &repo, run_id)
            .await
            .expect_err("a leftover branch with unmerged work must refuse re-allocation");
        assert!(
            matches!(err, WorktreeError::BranchHoldsWork { .. }),
            "unexpected error: {err:?}"
        );

        // The refusal must leave the released lease row in place: it is the
        // only metadata tying the retained worktree/branch to its owner run,
        // and deleting it before refusing would strand them as unassociated
        // orphans at the next boot's reconciliation.
        let (rows,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM workspace_leases WHERE worktree_path = ?")
                .bind(lease.worktree_path.to_string_lossy().as_ref())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows, 1,
            "the refused re-allocation must not delete the lease row"
        );
    }

    #[tokio::test]
    async fn checkpoint_clean_tree_records_commit_kind() {
        let root = tempdir().unwrap();
        let pool = test_pool(root.path()).await;
        let repo = init_repo(root.path());
        let run_id = seed_run(&pool).await;

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        let cp = create_run_checkpoint(&pool, &repo, &lease.worktree_path, run_id, 1)
            .await
            .unwrap()
            .expect("checkpoint created");

        assert_eq!(cp.kind, CheckpointKind::Commit);
        assert_eq!(cp.ordinal, 1);
        assert_eq!(cp.run_id, run_id);

        let fetched = fetch_checkpoint(&pool, cp.id)
            .await
            .unwrap()
            .expect("fetched");
        assert_eq!(fetched.commit_sha, cp.commit_sha);
        assert_eq!(fetched.kind, CheckpointKind::Commit);

        let launch = launch_checkpoint(&pool, run_id)
            .await
            .unwrap()
            .expect("launch cp");
        assert_eq!(launch.id, cp.id);
    }

    #[tokio::test]
    async fn checkpoint_dirty_tracked_records_stash_kind_and_restores() {
        let root = tempdir().unwrap();
        let pool = test_pool(root.path()).await;
        let repo = init_repo(root.path());
        let run_id = seed_run(&pool).await;

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = &lease.worktree_path;

        // Dirty tracked file
        std::fs::write(wt.join("hello.txt"), "modified content\n").unwrap();

        let cp = create_run_checkpoint(&pool, &repo, wt, run_id, 1)
            .await
            .unwrap()
            .expect("checkpoint created");

        assert_eq!(cp.kind, CheckpointKind::Stash);

        // Make another change
        std::fs::write(wt.join("hello.txt"), "subsequent change\n").unwrap();

        // Restore checkpoint
        restore_checkpoint_transactional(&cp).await.unwrap();

        // The file should be back to "modified content\n"
        let content = std::fs::read_to_string(wt.join("hello.txt")).unwrap();
        assert_eq!(content, "modified content\n");
    }

    #[tokio::test]
    async fn checkpoint_untracked_files_records_three_parent_stash_and_restores() {
        let root = tempdir().unwrap();
        let pool = test_pool(root.path()).await;
        let repo = init_repo(root.path());
        let run_id = seed_run(&pool).await;

        let mgr = WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();
        let wt = &lease.worktree_path;

        // Create an untracked file
        std::fs::write(wt.join("untracked.txt"), "untracked data\n").unwrap();

        let cp = create_run_checkpoint(&pool, &repo, wt, run_id, 1)
            .await
            .unwrap()
            .expect("checkpoint created");

        assert_eq!(cp.kind, CheckpointKind::Stash);

        // Verify 3 parents exist on this stash commit
        let p3 = run_git(
            wt,
            &["cat-file", "-e", &format!("{}^3^{{commit}}", cp.commit_sha)],
        )
        .await;
        assert!(p3.is_ok(), "untracked commit must be 3rd parent");

        // Mutate and add another untracked file
        std::fs::write(wt.join("untracked.txt"), "overwritten data\n").unwrap();
        std::fs::write(wt.join("untracked2.txt"), "extra file\n").unwrap();

        // Restore
        restore_checkpoint_transactional(&cp).await.unwrap();

        // Untracked.txt should be back to original, untracked2.txt cleaned away
        let content = std::fs::read_to_string(wt.join("untracked.txt")).unwrap();
        assert_eq!(content, "untracked data\n");
        assert!(!wt.join("untracked2.txt").exists());

        // Cleanup refs
        delete_checkpoint_refs(&repo, run_id).await.unwrap();
    }

    #[tokio::test]
    async fn allocate_at_carves_the_worktree_from_the_given_base() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let repo = init_repo(tmp.path());

        let commit1 = run_git(&repo, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .trim()
            .to_string();

        // Make a second commit on main
        std::fs::write(repo.join("file2.txt"), "commit2\n").unwrap();
        run_git(&repo, &["add", "file2.txt"]).await.unwrap();
        run_git(&repo, &["commit", "-m", "second commit"])
            .await
            .unwrap();

        let commit2 = run_git(&repo, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .trim()
            .to_string();
        assert_ne!(commit1, commit2);

        let manager = WorktreeManager::with_base(tmp.path().join("worktrees"));
        let run_id = seed_run(&pool).await;

        // Allocate at commit1
        let lease = manager
            .allocate_at(&pool, &repo, run_id, Some(&commit1))
            .await
            .unwrap();

        assert_eq!(lease.base_commit, commit1);
        // file2.txt should NOT exist in worktree carved at commit1
        assert!(!lease.worktree_path.join("file2.txt").exists());
        assert!(lease.worktree_path.join("README.md").exists());
    }

    #[tokio::test]
    async fn a_fork_run_reapplies_a_stash_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let repo = init_repo(tmp.path());
        let manager = WorktreeManager::with_base(tmp.path().join("worktrees"));

        let run1_id = seed_run(&pool).await;
        let lease1 = manager.allocate(&pool, &repo, run1_id).await.unwrap();
        let wt1 = &lease1.worktree_path;

        // Dirty modifications in run 1
        std::fs::write(wt1.join("README.md"), "run 1 modifications\n").unwrap();

        let cp = create_run_checkpoint(&pool, &repo, wt1, run1_id, 1)
            .await
            .unwrap()
            .expect("checkpoint");

        assert_eq!(cp.kind, CheckpointKind::Stash);

        // Fork run allocates at cp.base_commit and applies stash
        let run2_id = seed_run(&pool).await;
        let lease2 = manager
            .allocate_at(&pool, &repo, run2_id, Some(&cp.base_commit))
            .await
            .unwrap();
        let wt2 = &lease2.worktree_path;

        apply_stash(wt2, &cp.commit_sha).await.unwrap();

        let content = std::fs::read_to_string(wt2.join("README.md")).unwrap();
        assert_eq!(content, "run 1 modifications\n");
    }
}

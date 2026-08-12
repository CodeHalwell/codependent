//! The concrete [`DocumentPublisher`]: computes a document's publish plan,
//! durably parks its approval, and — only once approved — executes the
//! plan's Git action and records the publication (Phase 4 STEP 4.4, closing
//! the deferred "executing a `PublishPlan`" roadmap item).
//!
//! Like [`KnowledgeDocumentMutator`](crate::documents::KnowledgeDocumentMutator),
//! this lives in the assembly binary because it bridges the daemon (which
//! declares the [`DocumentPublisher`] seam) and `codypendent-knowledge` (which
//! owns `plan_publication`/`record_publication` and the document store) —
//! the daemon crate cannot name knowledge.
//!
//! ## The three targets (STEP 4.4.2)
//!
//! - [`RepositoryFile`](codypendent_knowledge::PublishTarget::RepositoryFile) —
//!   writes into the **primary working tree** and commits there in place (an
//!   approval-gated change set, attributed via the artifact store — "like
//!   agent writes").
//! - [`DocsBranchCommit`](codypendent_knowledge::PublishTarget::DocsBranchCommit) —
//!   commits onto a named docs branch through a **scratch worktree** created
//!   outside the primary checkout (`git worktree add`) so the user's checkout
//!   state is never touched, then the scratch worktree is removed (the branch
//!   itself is kept — unlike a per-run [`WorktreeManager`](codypendent_daemon::worktrees::WorktreeManager)
//!   branch, a docs branch is a persistent artifact, not throwaway).
//! - [`DocumentationPr`](codypendent_knowledge::PublishTarget::DocumentationPr) —
//!   the docs-branch flow, then `git push` and the Phase 3 GitHub write path
//!   (`github.create_draft_pull_request`, idempotent via the hidden-marker
//!   convention).
//!
//! ## Never blocks on the human decision
//!
//! [`publish`](DocumentPublisher::publish) mirrors
//! [`WorkflowStarter::start`](codypendent_daemon::workflows::WorkflowStarter::start)
//! (durably create, then drive in the background) rather than
//! [`DocumentMutator::apply_mutation`](codypendent_daemon::documents::DocumentMutator::apply_mutation)
//! (no approval step to await): it computes the plan, **parks the approval
//! before any write**, and returns as soon as it is recorded — spawning a
//! background task that awaits the resolution and, only on approval,
//! executes.
//!
//! A document has no `run_id`/`session_id` of its own (documents live outside
//! the session ledger), yet the shared [`ApprovalBroker`] — reused exactly as
//! a `GitHubMutation` parks — needs both to append its ledger events. An attached
//! caller's session is used when supplied, so its ordinary approval rail observes
//! the request; headless callers fall back to a fresh bookkeeping session. A run
//! is minted in either case (its objective is the plan's own Git-action sentence);
//! its state is flipped to
//! `Completed`/`Cancelled`/`Failed` once the decision is known so an operator
//! inspecting it sees a sensible outcome.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use codypendent_daemon::approvals::ApprovalBroker;
use codypendent_daemon::artifacts::{ArtifactStore, Provenance};
use codypendent_daemon::documents::{
    DocumentPublisher, PublishDocumentFuture, PublishDocumentRequest, PublishParked,
};
use codypendent_daemon::policy::Capability;
use codypendent_daemon::{ledger, projections};
use codypendent_integrations::github::model::NewPullRequest;
use codypendent_integrations::github::{GitHubApi, GitHubError, RepoId};
use codypendent_knowledge::{
    plan_publication, record_publication, DocStoreError, DocumentStatus, DocumentStore,
    PublishPlan, PublishTarget as KnowledgeTarget,
};
use codypendent_protocol::document::PublishTarget as WireTarget;
use codypendent_protocol::{
    AgentMode, ApprovalDecision, ApprovalId, CodypendentError, DataClassification, DocumentId,
    ProposedAction, Risk, RiskLevel, RunId, RunState, SessionId,
};
use sqlx::SqlitePool;
use tracing::warn;

/// The default base branch a documentation PR opens against when the checkout
/// is in a detached-HEAD state (no symbolic ref to read).
const FALLBACK_BASE_BRANCH: &str = "main";

/// Computes a `PublishDocument` plan, parks its approval, and (only once
/// approved) executes the plan's Git action against `repository_root`.
#[derive(Clone)]
pub struct KnowledgePublisher {
    pool: SqlitePool,
    approvals: ApprovalBroker,
    repository_root: PathBuf,
    artifacts: ArtifactStore,
    /// The GitHub client the documentation-PR target calls, if a personal-mode
    /// token was discovered at startup (mirrors [`RuntimeExecutor::github`]).
    /// `None` leaves that one target unavailable (`RepositoryFile`/
    /// `DocsBranchCommit` are unaffected).
    ///
    /// [`RuntimeExecutor::github`]: crate::executor::RuntimeExecutor
    github: Option<Arc<dyn GitHubApi>>,
}

impl KnowledgePublisher {
    /// Build a publisher over the daemon's pool, the shared approval broker
    /// (the SAME one the server resolves `ResolveApproval` against), the
    /// repository this daemon serves, and the artifact store.
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        approvals: ApprovalBroker,
        repository_root: PathBuf,
        artifacts: ArtifactStore,
    ) -> Self {
        Self {
            pool,
            approvals,
            repository_root,
            artifacts,
            github: None,
        }
    }

    /// Enable the documentation-PR target with a GitHub client.
    #[must_use]
    pub fn with_github(mut self, github: Arc<dyn GitHubApi>) -> Self {
        self.github = Some(github);
        self
    }

    /// Re-arm durable publication continuations after restart. Pending
    /// approvals are registered with the shared broker again; approvals which
    /// were resolved just before the crash continue from their stored plan.
    pub async fn recover_pending(&self) -> anyhow::Result<usize> {
        self.approvals.reload_pending(&self.pool).await?;

        let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT j.approval_id, j.run_id, j.document_id, j.plan_json, a.state \
             FROM document_publish_jobs j \
             LEFT JOIN approvals a ON a.id = j.approval_id \
             WHERE j.state IN ('pending', 'executing')",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut recovered = 0;
        for (approval, run, document, plan_json, approval_state) in rows {
            let approval_id = ApprovalId::from_str(&approval)?;
            let run_id = RunId::from_str(&run)?;
            let document_id = DocumentId::from_str(&document)?;
            let plan: PublishPlan = match serde_json::from_str(&plan_json) {
                Ok(plan) => plan,
                Err(error) => {
                    warn!(%approval_id, %error, "stored publish plan is corrupt");
                    set_publish_job_state(&self.pool, approval_id, "failed").await?;
                    projections::set_run_state(&self.pool, run_id, RunState::Failed).await?;
                    continue;
                }
            };

            match approval_state.as_deref() {
                Some("pending") => {
                    tokio::spawn(execute_after_decision(
                        self.pool.clone(),
                        self.approvals.clone(),
                        self.repository_root.clone(),
                        self.artifacts.clone(),
                        self.github.clone(),
                        approval_id,
                        run_id,
                        document_id,
                        plan,
                    ));
                    recovered += 1;
                }
                Some("approved") => {
                    tokio::spawn(execute_approved_plan(
                        self.pool.clone(),
                        self.repository_root.clone(),
                        self.artifacts.clone(),
                        self.github.clone(),
                        approval_id,
                        run_id,
                        document_id,
                        plan,
                    ));
                    recovered += 1;
                }
                Some("rejected" | "expired") => {
                    set_publish_job_state(&self.pool, approval_id, "cancelled").await?;
                    projections::set_run_state(&self.pool, run_id, RunState::Cancelled).await?;
                }
                Some(other) => {
                    warn!(%approval_id, state = other, "publish approval has invalid state");
                    set_publish_job_state(&self.pool, approval_id, "failed").await?;
                    projections::set_run_state(&self.pool, run_id, RunState::Failed).await?;
                }
                None => {
                    // A crash can occur after the continuation is persisted but
                    // before the approval transaction commits. The durable job
                    // makes that window detectable and terminal instead of
                    // leaving an unresolvable synthetic run forever.
                    warn!(%approval_id, "publish continuation has no approval row");
                    set_publish_job_state(&self.pool, approval_id, "failed").await?;
                    projections::set_run_state(&self.pool, run_id, RunState::Failed).await?;
                }
            }
        }
        Ok(recovered)
    }
}

impl DocumentPublisher for KnowledgePublisher {
    fn publish(&self, request: PublishDocumentRequest) -> PublishDocumentFuture<'_> {
        let pool = self.pool.clone();
        let approvals = self.approvals.clone();
        let repository_root = self.repository_root.clone();
        let artifacts = self.artifacts.clone();
        let github = self.github.clone();
        Box::pin(async move {
            let PublishDocumentRequest {
                document_id,
                target,
                session_id,
                ..
            } = request;

            let doc = DocumentStore::new()
                .snapshot_document(&pool, document_id)
                .await
                .map_err(map_store_error)?
                .ok_or_else(|| not_found(document_id))?;

            let domain_target = convert_target(target)?;
            // Fail closed before anything else: a `path`/`branch` that could
            // escape the repository or be misread as a `git` flag must never
            // reach a park (let alone an approval a human might wave through
            // without checking) or a filesystem/git call.
            validate_target(&domain_target)
                .map_err(|reason| CodypendentError::new("document.unsafe-target", reason, false))?;
            let plan = plan_publication(&doc, domain_target);
            let target_description = describe_target(&plan.target);
            let (risk, capabilities) = risk_and_capabilities(&plan.target);

            // A document has no run of its own. Mint one in the caller's attached
            // session when available so the approval event reaches that session's
            // subscribers; otherwise preserve the one-shot CLI's synthetic-session
            // fallback.
            let (session_id, run_id) =
                mint_publish_run(&pool, session_id, &doc.title, &plan.git_action)
                    .await
                    .map_err(internal_error)?;

            let action = ProposedAction::PublishDocument {
                document_id,
                target: target_description.clone(),
                changed_files: plan.changed_files.clone(),
                git_action: plan.git_action.clone(),
            };

            // Persist the continuation before the approval becomes visible.
            // If the process exits after either commit, startup can distinguish
            // a resumable approval from an incomplete request.
            let approval_id = ApprovalId::new();
            persist_publish_job(&pool, approval_id, run_id, document_id, &plan)
                .await
                .map_err(internal_error)?;
            if let Err(error) = approvals
                .request_with_id_and_reuse(
                    &pool,
                    approval_id,
                    session_id,
                    run_id,
                    action,
                    risk,
                    capabilities,
                    None,
                    true,
                )
                .await
            {
                let _ = set_publish_job_state(&pool, approval_id, "failed").await;
                let _ = projections::set_run_state(&pool, run_id, RunState::Failed).await;
                return Err(internal_error(error));
            }

            // Never block on the human decision: hand the plan to a background
            // task that awaits it, and reply now with exactly what was parked.
            tokio::spawn(execute_after_decision(
                pool,
                approvals,
                repository_root,
                artifacts,
                github,
                approval_id,
                run_id,
                document_id,
                plan.clone(),
            ));

            Ok(PublishParked {
                approval_id,
                target_description,
                changed_files: plan.changed_files,
                git_action: plan.git_action,
            })
        })
    }
}

/// Await `approval_id`'s resolution and, only on approval, execute the plan
/// and record the publication. Rejection (or the waiter ending without a
/// decision — e.g. a daemon restart before `reload_pending`) performs no
/// write. Every failure is logged and swallowed: nothing here has a client
/// still waiting on it (the seam already replied).
#[allow(clippy::too_many_arguments)]
async fn execute_after_decision(
    pool: SqlitePool,
    approvals: ApprovalBroker,
    repository_root: PathBuf,
    artifacts: ArtifactStore,
    github: Option<Arc<dyn GitHubApi>>,
    approval_id: ApprovalId,
    run_id: RunId,
    document_id: DocumentId,
    plan: PublishPlan,
) {
    let decision = match approvals.await_decision(approval_id).await {
        Ok(decision) => decision,
        Err(error) => {
            warn!(%approval_id, %error, "publish approval waiter ended without a decision");
            let _ = set_publish_job_state(&pool, approval_id, "failed").await;
            let _ = projections::set_run_state(&pool, run_id, RunState::Failed).await;
            return;
        }
    };

    if decision != ApprovalDecision::Approve {
        // Reject (or an unrecognised future decision): zero writes.
        let _ = set_publish_job_state(&pool, approval_id, "cancelled").await;
        let _ = projections::set_run_state(&pool, run_id, RunState::Cancelled).await;
        return;
    }

    execute_approved_plan(
        pool,
        repository_root,
        artifacts,
        github,
        approval_id,
        run_id,
        document_id,
        plan,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn execute_approved_plan(
    pool: SqlitePool,
    repository_root: PathBuf,
    artifacts: ArtifactStore,
    github: Option<Arc<dyn GitHubApi>>,
    approval_id: ApprovalId,
    run_id: RunId,
    document_id: DocumentId,
    plan: PublishPlan,
) {
    if let Err(error) = set_publish_job_state(&pool, approval_id, "executing").await {
        warn!(%approval_id, %error, "could not mark publish job executing");
        let _ = projections::set_run_state(&pool, run_id, RunState::Failed).await;
        return;
    }

    match execute_plan(
        &repository_root,
        &pool,
        &artifacts,
        github.as_deref(),
        &plan,
        document_id,
    )
    .await
    {
        Ok(git_commit) => {
            if let Err(error) =
                record_publication(&pool, document_id, &plan, git_commit.as_deref()).await
            {
                warn!(%document_id, %error, "publish executed but recording it failed");
                let _ = set_publish_job_state(&pool, approval_id, "failed").await;
                let _ = projections::set_run_state(&pool, run_id, RunState::Failed).await;
                return;
            }
            // The status lifecycle was decorative: nothing ever left `Draft`.
            // A publish that reached a commit is exactly the transition
            // `Published` describes, so record it — a metadata update that does
            // NOT bump the content revision, so pending suggestions keep their
            // anchors. Best-effort: the publication itself already committed,
            // and a status that lags is a display concern, not a lost write.
            if let Err(error) = DocumentStore::new()
                .set_status(&pool, document_id, DocumentStatus::Published)
                .await
            {
                warn!(%document_id, %error, "published, but the status stayed draft");
            }
            let _ = set_publish_job_state(&pool, approval_id, "completed").await;
            let _ = projections::set_run_state(&pool, run_id, RunState::Completed).await;
        }
        Err(error) => {
            warn!(%document_id, %error, "publish approved but execution failed");
            let _ = set_publish_job_state(&pool, approval_id, "failed").await;
            let _ = projections::set_run_state(&pool, run_id, RunState::Failed).await;
        }
    }
}

async fn persist_publish_job(
    pool: &SqlitePool,
    approval_id: ApprovalId,
    run_id: RunId,
    document_id: DocumentId,
    plan: &PublishPlan,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO document_publish_jobs \
         (approval_id, run_id, document_id, plan_json, state, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'pending', ?, ?)",
    )
    .bind(approval_id.to_string())
    .bind(run_id.to_string())
    .bind(document_id.to_string())
    .bind(serde_json::to_string(plan)?)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_publish_job_state(
    pool: &SqlitePool,
    approval_id: ApprovalId,
    state: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE document_publish_jobs SET state = ?, updated_at = ? WHERE approval_id = ?")
        .bind(state)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(approval_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Reject a `path` that could escape the repository or a `branch` that could
/// be misread as a `git` flag — defense in depth (deny-wins: the wire
/// target's fields are trusted enough to act on only once validated, both
/// here at parse-time and again defensively at [`execute_plan`]).
fn validate_target(target: &KnowledgeTarget) -> Result<(), String> {
    match target {
        KnowledgeTarget::RepositoryFile { path } => validate_path(path),
        KnowledgeTarget::DocsBranchCommit { branch, path }
        | KnowledgeTarget::DocumentationPr { branch, path, .. } => {
            validate_path(path)?;
            validate_branch(branch)
        }
    }
}

/// A repo-relative path: non-empty, not absolute, and no `..` component (so
/// `repo_root.join(path)` can never resolve outside `repo_root`).
fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path must not be empty".to_string());
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!(
            "path {path:?} must be repository-relative, not absolute"
        ));
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("path {path:?} must not contain `..`"));
    }
    Ok(())
}

/// A branch name: non-empty, no whitespace, and never starting with `-` (which
/// `git` could otherwise parse as a flag rather than a ref name in some
/// invocations).
fn validate_branch(branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(format!("branch {branch:?} is not a safe ref name"));
    }
    Ok(())
}

/// Execute an approved plan's Git action, returning the resulting commit-ish
/// (or `None` when a target genuinely has no single commit to report — none do
/// today, but the return stays optional to match [`record_publication`]'s
/// `git_commit: Option<&str>`).
async fn execute_plan(
    repository_root: &Path,
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    github: Option<&dyn GitHubApi>,
    plan: &PublishPlan,
    document_id: DocumentId,
) -> Result<Option<String>, PublishExecError> {
    validate_target(&plan.target).map_err(PublishExecError::UnsafeTarget)?;
    match &plan.target {
        KnowledgeTarget::RepositoryFile { path } => {
            let sha = write_and_commit(
                repository_root,
                pool,
                artifacts,
                document_id,
                path,
                &plan.rendered,
            )
            .await?;
            Ok(Some(sha))
        }
        KnowledgeTarget::DocsBranchCommit { branch, path } => {
            let sha = commit_on_docs_branch(repository_root, branch, path, &plan.rendered).await?;
            Ok(Some(sha))
        }
        KnowledgeTarget::DocumentationPr {
            branch,
            path,
            title,
        } => {
            let github = github.ok_or(PublishExecError::NoGitHubClient)?;
            let sha = commit_on_docs_branch(repository_root, branch, path, &plan.rendered).await?;
            run_git(
                repository_root,
                &["push", "origin", &format!("{branch}:{branch}")],
            )
            .await?;
            let repo = crate::executor::resolve_github_repo(repository_root)
                .await
                .ok_or(PublishExecError::NoGitHubRemote)?;
            let base = current_branch(repository_root)
                .await
                .unwrap_or_else(|| FALLBACK_BASE_BRANCH.to_string());
            open_documentation_pr(github, &repo, branch, &base, title, document_id).await?;
            Ok(Some(sha))
        }
    }
}

/// Write `rendered` into the **primary working tree** at `repo_root/path` and
/// commit it there (target 1: "write {path} in the working tree"). Recording
/// the rendered bytes as an artifact makes the write an attributable change
/// set — like an agent's own writes — independent of the commit; a failure
/// doing so is logged and swallowed (the commit is already the authoritative
/// record).
async fn write_and_commit(
    repo_root: &Path,
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    document_id: DocumentId,
    path: &str,
    rendered: &str,
) -> Result<String, PublishExecError> {
    let full_path = repo_root.join(path);
    if let Some(parent) = full_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&full_path, rendered.as_bytes()).await?;
    let sha = commit_path(repo_root, path, &format!("docs: publish {path}")).await?;

    if let Err(error) = artifacts
        .put(
            pool,
            "text/markdown",
            DataClassification::Internal,
            Provenance::system(format!("docs-publish:{document_id}")),
            rendered.as_bytes(),
        )
        .await
    {
        warn!(%document_id, %error, "publish committed but recording its artifact failed");
    }
    Ok(sha)
}

/// Commit `rendered` at `path` onto `branch` through a **scratch worktree**
/// created outside `repo_root`'s working tree (`git worktree add`), so the
/// primary checkout's `HEAD`/index/working files are never touched (target 2:
/// worktree-safe). The scratch worktree is always removed afterward — even on
/// a write/commit failure — but the branch itself is kept: unlike a per-run
/// [`WorktreeManager`](codypendent_daemon::worktrees::WorktreeManager) branch,
/// a docs branch is a persistent artifact a later publish reuses.
async fn commit_on_docs_branch(
    repo_root: &Path,
    branch: &str,
    path: &str,
    rendered: &str,
) -> Result<String, PublishExecError> {
    let scratch = scratch_worktree_path(repo_root)?;
    if let Some(parent) = scratch.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let scratch_str = scratch.to_string_lossy().into_owned();

    let branch_ref = format!("refs/heads/{branch}");
    let branch_exists = run_git(repo_root, &["rev-parse", "--verify", &branch_ref])
        .await
        .is_ok();
    if branch_exists {
        run_git(repo_root, &["worktree", "add", &scratch_str, branch]).await?;
    } else {
        run_git(
            repo_root,
            &["worktree", "add", "-b", branch, &scratch_str, "HEAD"],
        )
        .await?;
    }

    let result: Result<String, PublishExecError> = async {
        let full = scratch.join(path);
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full, rendered.as_bytes()).await?;
        commit_path(&scratch, path, &format!("docs: publish {path}")).await
    }
    .await;

    // Best-effort cleanup regardless of outcome: a failed publish must not
    // leak a registered worktree the next attempt trips over.
    let _ = run_git(repo_root, &["worktree", "remove", "--force", &scratch_str]).await;

    result
}

/// `git add` a path then commit it, but only when doing so actually stages a
/// change: publishing byte-identical content twice (STEP 4.4's determinism —
/// the same document revision always renders the same bytes) must be a no-op,
/// not a failing `git commit` ("nothing to commit"). Returns the resulting
/// commit sha, or the current `HEAD` when nothing changed.
async fn commit_path(dir: &Path, path: &str, message: &str) -> Result<String, PublishExecError> {
    run_git(dir, &["add", "--", path]).await?;
    // Scoped to `path`: for `DocsBranchCommit`/`DocumentationPr` this is a
    // fresh scratch worktree (clean index either way), but for
    // `RepositoryFile` `dir` IS the primary working tree, which may carry
    // unrelated staged changes at publish time. An unscoped
    // `--cached --name-only` would report non-empty (and later commit
    // everything staged) even when THIS path is unchanged — sweeping
    // unrelated work into the publish commit and defeating the
    // unchanged-content no-op. Scoping both the diff check and the commit
    // itself to `path` keeps the committed change set exactly what the
    // approval card showed (`changed_files = [path]`), never more, and never
    // less.
    let staged = run_git(dir, &["diff", "--cached", "--name-only", "--", path]).await?;
    if staged.trim().is_empty() {
        return Ok(run_git(dir, &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string());
    }
    run_git(dir, &["commit", "-q", "-m", message, "--", path]).await?;
    Ok(run_git(dir, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_string())
}

/// Open (or, via the hidden-marker convention, find) a draft documentation PR
/// through the Phase 3 GitHub write path (target 3). Split out from
/// [`execute_plan`] so the idempotency behavior — the same document+branch
/// always derives the same key, so a retried publish resolves to the existing
/// PR rather than opening a duplicate — is directly testable against a
/// GitHub double without needing a real `github.com`-resolvable remote.
async fn open_documentation_pr(
    github: &dyn GitHubApi,
    repo: &RepoId,
    branch: &str,
    base: &str,
    title: &str,
    document_id: DocumentId,
) -> Result<(), PublishExecError> {
    // Stable per (document, branch): a retried publish of the same document to
    // the same docs branch is one logical PR, however many times it runs.
    let idempotency_key = format!("docs-publish:{document_id}:{branch}");
    let request = NewPullRequest::draft(title.to_string(), branch.to_string(), base.to_string());
    github
        .create_draft_pull_request(repo, &request, &idempotency_key)
        .await?;
    Ok(())
}

/// A unique scratch-worktree path outside `repo_root`'s working tree, in the
/// same sibling `codypendent-worktrees/<repo-name>/` directory
/// [`WorktreeManager`](codypendent_daemon::worktrees::WorktreeManager) uses for
/// per-run worktrees (a distinct `docs-publish-` prefix keeps the two kinds
/// visually distinct on disk).
fn scratch_worktree_path(repo_root: &Path) -> Result<PathBuf, PublishExecError> {
    let parent = repo_root
        .parent()
        .ok_or_else(|| PublishExecError::Other("repository path has no parent".to_string()))?;
    let repo_name = repo_root.file_name().ok_or_else(|| {
        PublishExecError::Other("repository path has no final component".to_string())
    })?;
    let unique = uuid::Uuid::now_v7().simple().to_string();
    Ok(parent
        .join("codypendent-worktrees")
        .join(repo_name)
        .join(format!("docs-publish-{unique}")))
}

/// The checkout's current branch name (`git symbolic-ref --short HEAD`), or
/// `None` in a detached-HEAD state.
async fn current_branch(repo_root: &Path) -> Option<String> {
    let output = run_git(repo_root, &["symbolic-ref", "--short", "HEAD"])
        .await
        .ok()?;
    let trimmed = output.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Spawn `git` with an explicit argument vector (never a shell string) in
/// `dir`, returning stdout on success or a [`PublishExecError::Git`] on a
/// non-zero exit. Mirrors [`WorktreeManager`](codypendent_daemon::worktrees::WorktreeManager)'s
/// own internal `run_git` (a daemon-issued, not model-proposed, invocation).
async fn run_git<S: AsRef<std::ffi::OsStr>>(
    dir: &Path,
    args: &[S],
) -> Result<String, PublishExecError> {
    let mut command = tokio::process::Command::new("git");
    command.current_dir(dir);
    for arg in args {
        command.arg(arg);
    }
    // `git push` is the one network-touching invocation here; a terminal
    // prompt for missing credentials must never wedge the daemon's background
    // task waiting on stdin it can never receive.
    command.env("GIT_TERMINAL_PROMPT", "0");
    let output = command.output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let printable: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();
        Err(PublishExecError::Git {
            command: format!("git {}", printable.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Errors executing an approved plan. Never surfaced to a client (the seam
/// already replied before execution began) — logged at the call site instead.
#[derive(Debug, thiserror::Error)]
enum PublishExecError {
    #[error("`{command}` failed: {stderr}")]
    Git { command: String, stderr: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the documentation PR target requires a configured GitHub client")]
    NoGitHubClient,
    #[error(
        "could not resolve a GitHub owner/repo from the checkout's `origin` remote \
         (documentation PR target)"
    )]
    NoGitHubRemote,
    #[error(transparent)]
    GitHub(#[from] GitHubError),
    /// Defense in depth: [`validate_target`] rejected the plan's `path`/
    /// `branch` (should never trigger here — `publish` already rejects the
    /// same check before parking — but a future caller of `execute_plan`
    /// must not be able to skip it).
    #[error("unsafe publish target: {0}")]
    UnsafeTarget(String),
    #[error("{0}")]
    Other(String),
}

/// Create the fresh session + run a publish approval parks against (see the
/// module doc). `objective` is the plan's own Git-action sentence, so an
/// operator inspecting the run sees exactly what it will do.
async fn mint_publish_run(
    pool: &SqlitePool,
    session_id: Option<SessionId>,
    document_title: &str,
    objective: &str,
) -> anyhow::Result<(SessionId, RunId)> {
    let session_id = match session_id {
        Some(session_id) => session_id,
        None => {
            let session_id = SessionId::new();
            ledger::create_session(pool, session_id, &format!("docs publish: {document_title}"))
                .await?;
            session_id
        }
    };
    let run_id = RunId::new();
    projections::insert_run(
        pool,
        run_id,
        session_id,
        objective,
        AgentMode::Build,
        "docs-publish",
        "{}",
    )
    .await?;
    projections::set_run_state(pool, run_id, RunState::WaitingForApproval).await?;
    Ok((session_id, run_id))
}

/// Convert the wire [`WireTarget`] into the knowledge engine's domain
/// [`KnowledgeTarget`]. The only failure is an unrecognized target tag —
/// `Unknown`, or any future variant this build predates (`PublishTarget` is
/// `#[non_exhaustive]`, so the wildcard covers both).
fn convert_target(wire: WireTarget) -> Result<KnowledgeTarget, CodypendentError> {
    match wire {
        WireTarget::RepositoryFile { path } => Ok(KnowledgeTarget::RepositoryFile { path }),
        WireTarget::DocsBranchCommit { branch, path } => {
            Ok(KnowledgeTarget::DocsBranchCommit { branch, path })
        }
        WireTarget::DocumentationPr {
            branch,
            path,
            title,
        } => Ok(KnowledgeTarget::DocumentationPr {
            branch,
            path,
            title,
        }),
        _ => Err(CodypendentError::new(
            "protocol.unsupported-payload",
            "unsupported publish target".to_string(),
            false,
        )),
    }
}

/// A short human description of a target — the "target" STEP 4.4.2 requires
/// shown before approval, alongside `changed_files` and `git_action`.
fn describe_target(target: &KnowledgeTarget) -> String {
    match target {
        KnowledgeTarget::RepositoryFile { path } => format!("repository file {path}"),
        KnowledgeTarget::DocsBranchCommit { branch, path } => {
            format!("docs-branch commit {path} on {branch}")
        }
        KnowledgeTarget::DocumentationPr {
            branch,
            path,
            title,
        } => format!("documentation PR \"{title}\" ({path} on {branch})"),
    }
}

/// The [`Risk`] and [`Capability`] list shown on the approval card for a
/// target. A documentation PR additionally pushes to a remote and writes to
/// GitHub, so it is rated `High` with `GitPush` alongside `GitCommit`.
fn risk_and_capabilities(target: &KnowledgeTarget) -> (Risk, Vec<Capability>) {
    match target {
        KnowledgeTarget::RepositoryFile { path } => (
            Risk {
                level: RiskLevel::Medium,
                reasons: vec![format!("writes {path} in the working tree and commits it")],
            },
            vec![Capability::GitCommit],
        ),
        KnowledgeTarget::DocsBranchCommit { branch, path } => (
            Risk {
                level: RiskLevel::Medium,
                reasons: vec![format!("commits {path} on branch {branch}")],
            },
            vec![Capability::GitCommit],
        ),
        KnowledgeTarget::DocumentationPr { branch, path, .. } => (
            Risk {
                level: RiskLevel::High,
                reasons: vec![format!(
                    "commits {path} on branch {branch}, pushes it, and opens a GitHub pull request"
                )],
            },
            vec![Capability::GitCommit, Capability::GitPush],
        ),
    }
}

/// The `document.not-found` error (a document the client named does not
/// exist).
fn not_found(document_id: DocumentId) -> CodypendentError {
    CodypendentError::new(
        "document.not-found",
        format!("no document {document_id}"),
        false,
    )
}

/// Map a document-store failure to a structured error, mirroring
/// [`KnowledgeDocumentMutator`](crate::documents::KnowledgeDocumentMutator)'s
/// own mapping.
fn map_store_error(error: DocStoreError) -> CodypendentError {
    match error {
        DocStoreError::NoSuchDocument(id) => not_found(id),
        other => CodypendentError::new("document.apply-failed", other.to_string(), true),
    }
}

/// Map an infrastructure failure (approval broker, run bookkeeping) to a
/// structured, retryable error.
fn internal_error(error: impl std::fmt::Display) -> CodypendentError {
    CodypendentError::new("document.publish-failed", error.to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use codypendent_daemon::db;
    use codypendent_daemon::subscriptions::SubscriptionHub;
    use codypendent_integrations::github::idempotency;
    use codypendent_integrations::github::model::{
        CheckRun, NewCheckRun, PullRequest, ReviewComment, UpdatePullRequest,
    };
    use codypendent_knowledge::{
        BlockContent, DocumentAuthor, DocumentBlock, DocumentMetadata, NewDocument, Publication,
        Scope,
    };
    use codypendent_protocol::{ApprovalScope, ClientId, EventBody};

    async fn temp_pool(dir: &Path) -> SqlitePool {
        db::open_database(&dir.join("codypendent.db"))
            .await
            .expect("open database")
    }

    /// Run `git` synchronously, asserting success (test setup only).
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

    /// `git` synchronously, returning trimmed stdout (test assertions only).
    fn git_output(dir: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@codypendent.dev"]);
        git(dir, &["config", "user.name", "Codypendent Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
    }

    async fn seed_document(pool: &SqlitePool, title: &str) -> DocumentId {
        DocumentStore::new()
            .create(
                pool,
                NewDocument {
                    title: title.to_string(),
                    scope: Scope::System,
                    metadata: DocumentMetadata::default(),
                    blocks: vec![DocumentBlock::with_id(
                        "p",
                        BlockContent::Paragraph {
                            text: "hello world".to_string(),
                        },
                    )],
                },
                &DocumentAuthor::Integration {
                    integration: "test".to_string(),
                },
            )
            .await
            .expect("create document")
            .id
    }

    fn wire_target(path: &str) -> WireTarget {
        WireTarget::RepositoryFile {
            path: path.to_string(),
        }
    }

    fn publish_request(document_id: DocumentId, target: WireTarget) -> PublishDocumentRequest {
        PublishDocumentRequest {
            document_id,
            target,
            client_id: ClientId::new(),
            session_id: None,
        }
    }

    async fn resolve(
        pool: &SqlitePool,
        approvals: &ApprovalBroker,
        approval_id: codypendent_protocol::ApprovalId,
        decision: ApprovalDecision,
    ) {
        approvals
            .resolve(
                pool,
                approval_id,
                decision,
                ApprovalScope::Once,
                "tester".to_string(),
            )
            .await
            .expect("resolve approval");
    }

    /// Poll until at least `count` publication rows exist for `document_id`
    /// (the background execution task is fire-and-forget), or panic after a
    /// generous bound.
    async fn wait_for_publication_count(
        pool: &SqlitePool,
        document_id: DocumentId,
        count: usize,
    ) -> Vec<Publication> {
        for _ in 0..250 {
            let published = codypendent_knowledge::publications(pool, document_id)
                .await
                .expect("query publications");
            if published.len() >= count {
                return published;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {count} publication(s) on {document_id}");
    }

    /// Poll until the fire-and-forget continuation reaches its durable job
    /// state. A publication row is recorded before the job is marked
    /// completed, so observing that row alone is not a completion barrier.
    async fn wait_for_publish_job_state(
        pool: &SqlitePool,
        approval_id: ApprovalId,
        expected: &str,
    ) {
        let mut observed = None;
        for _ in 0..250 {
            observed = sqlx::query_scalar::<_, String>(
                "SELECT state FROM document_publish_jobs WHERE approval_id = ?",
            )
            .bind(approval_id.to_string())
            .fetch_optional(pool)
            .await
            .expect("query publish job state");
            if observed.as_deref() == Some(expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "timed out waiting for publish job {approval_id} to reach {expected}; observed {observed:?}"
        );
    }

    fn build_publisher(
        pool: SqlitePool,
        approvals: ApprovalBroker,
        repo: PathBuf,
        dir: &Path,
    ) -> KnowledgePublisher {
        KnowledgePublisher::new(
            pool,
            approvals,
            repo,
            ArtifactStore::new(dir.join("artifacts")),
        )
    }

    #[tokio::test]
    async fn attached_publish_parks_approval_in_the_callers_session() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Reachable approval").await;
        let session_id = SessionId::new();
        ledger::create_session(&pool, session_id, "workspace")
            .await
            .expect("create caller session");
        let subscriptions = SubscriptionHub::new();
        let mut live = subscriptions.subscribe(session_id);
        let approvals = ApprovalBroker::new().with_subscriptions(subscriptions);
        let publisher = build_publisher(pool.clone(), approvals, repo, dir.path());

        let parked = publisher
            .publish(PublishDocumentRequest {
                document_id,
                target: wire_target("docs/reachable.md"),
                client_id: ClientId::new(),
                session_id: Some(session_id),
            })
            .await
            .expect("publish parks an approval");

        let event = live.recv().await.expect("approval reaches caller session");
        assert!(matches!(
            event.body,
            EventBody::ApprovalRequested { approval_id, .. }
                if approval_id == parked.approval_id
        ));
        let run_session: String = sqlx::query_scalar(
            "SELECT r.session_id FROM approvals a JOIN runs r ON r.id = a.run_id WHERE a.id = ?",
        )
        .bind(parked.approval_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("publish run exists");
        assert_eq!(run_session, session_id.to_string());
    }

    #[tokio::test]
    async fn repository_file_publish_parks_approves_writes_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let parked = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .expect("publish parks an approval");

        assert_eq!(
            parked.changed_files,
            vec!["docs/architecture.md".to_string()]
        );
        assert!(parked.git_action.contains("docs/architecture.md"));
        assert!(parked.target_description.contains("docs/architecture.md"));

        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Approve,
        )
        .await;

        let published = wait_for_publication_count(&pool, document_id, 1).await;
        assert!(
            published[0].git_commit.is_some(),
            "a commit must be recorded"
        );
        assert_eq!(published[0].rendered_hash.len(), 64, "sha-256 hex digest");

        let written = std::fs::read_to_string(repo.join("docs/architecture.md")).unwrap();
        assert!(
            written.contains("hello world"),
            "the rendered content must be written verbatim"
        );

        let log = git_output(&repo, &["log", "--oneline"]);
        assert_eq!(
            log.lines().count(),
            2,
            "the publish must add exactly one commit: {log}"
        );
    }

    #[tokio::test]
    async fn pending_publish_is_rearmed_after_a_daemon_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;

        // Daemon instance one parks the request and disappears before a
        // decision. Its in-memory waiter is deliberately not used again.
        let old_publisher = build_publisher(
            pool.clone(),
            ApprovalBroker::new(),
            repo.clone(),
            dir.path(),
        );
        let parked = old_publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/recovered.md"),
            ))
            .await
            .expect("publish parks before restart");

        // A fresh broker/publisher pair models the restarted daemon. Recovery
        // reloads the durable approval and respawns the stored continuation.
        let recovered_approvals = ApprovalBroker::new();
        let recovered_publisher = build_publisher(
            pool.clone(),
            recovered_approvals.clone(),
            repo.clone(),
            dir.path(),
        );
        assert_eq!(recovered_publisher.recover_pending().await.unwrap(), 1);
        resolve(
            &pool,
            &recovered_approvals,
            parked.approval_id,
            ApprovalDecision::Approve,
        )
        .await;

        wait_for_publication_count(&pool, document_id, 1).await;
        assert!(repo.join("docs/recovered.md").exists());
        wait_for_publish_job_state(&pool, parked.approval_id, "completed").await;
    }

    #[tokio::test]
    async fn a_path_traversal_target_is_rejected_before_parking() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let error = publisher
            .publish(publish_request(document_id, wire_target("../outside.md")))
            .await
            .expect_err("a path escaping the repository must be rejected");
        assert_eq!(error.code, "document.unsafe-target");

        // Nothing was parked: no approval, no write, no publication.
        assert!(approvals.reload_pending(&pool).await.unwrap().is_empty());
        assert!(!dir.path().join("outside.md").exists());
        assert!(codypendent_knowledge::publications(&pool, document_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_absolute_path_target_is_rejected_before_parking() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let error = publisher
            .publish(publish_request(document_id, wire_target("/etc/passwd")))
            .await
            .expect_err("an absolute path must be rejected");
        assert_eq!(error.code, "document.unsafe-target");
    }

    #[tokio::test]
    async fn an_unsafe_branch_name_is_rejected_before_parking() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let error = publisher
            .publish(publish_request(
                document_id,
                WireTarget::DocsBranchCommit {
                    branch: "-x".to_string(),
                    path: "docs/architecture.md".to_string(),
                },
            ))
            .await
            .expect_err("a branch name that could be read as a git flag must be rejected");
        assert_eq!(error.code, "document.unsafe-target");
    }

    #[tokio::test]
    async fn rejected_publish_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());
        let head_before = git_output(&repo, &["rev-parse", "HEAD"]);

        let parked = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .expect("publish parks an approval");

        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Reject,
        )
        .await;

        // Give the (idle) background task a moment, then assert the repo and
        // the publication history are both untouched.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !repo.join("docs/architecture.md").exists(),
            "no file must land"
        );
        let published = codypendent_knowledge::publications(&pool, document_id)
            .await
            .unwrap();
        assert!(published.is_empty(), "no publication must be recorded");
        assert_eq!(git_output(&repo, &["rev-parse", "HEAD"]), head_before);
        let status = git_output(&repo, &["status", "--porcelain"]);
        assert!(
            status.is_empty(),
            "the repository must stay untouched: {status}"
        );
    }

    #[tokio::test]
    async fn republishing_unchanged_content_is_a_deterministic_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let first = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .unwrap();
        resolve(
            &pool,
            &approvals,
            first.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        let first_published = wait_for_publication_count(&pool, document_id, 1).await;
        let head_after_first = git_output(&repo, &["rev-parse", "HEAD"]);

        // Publish again: the document did not change, so `plan_publication` is
        // byte-for-byte the same plan (STEP 4.4 determinism) and the second
        // execution must not create a second commit.
        let second = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .unwrap();
        assert_eq!(second.git_action, first.git_action);
        assert_eq!(second.changed_files, first.changed_files);
        resolve(
            &pool,
            &approvals,
            second.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        let second_published = wait_for_publication_count(&pool, document_id, 2).await;
        let head_after_second = git_output(&repo, &["rev-parse", "HEAD"]);

        assert_eq!(
            head_after_first, head_after_second,
            "unchanged content must not create a second commit"
        );
        assert_eq!(
            first_published[0].rendered_hash, second_published[0].rendered_hash,
            "the render is deterministic across publishes"
        );
        // Newest-first: the second publish's row is index 0 this time.
        assert_eq!(
            second_published[0].git_commit,
            second_published[1].git_commit
        );
    }

    #[tokio::test]
    async fn repository_file_commit_does_not_sweep_unrelated_staged_changes() {
        // Regression test (review fix): `RepositoryFile` operates in the
        // PRIMARY working tree, which may carry unrelated staged changes at
        // publish time. The commit must contain exactly what the approval
        // card promised (`changed_files = [path]`) — never more.
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        // An unrelated file is staged BEFORE the publish runs — simulating a
        // user mid-way through unrelated work in their own checkout.
        std::fs::write(repo.join("secret.rs"), "let token = \"unrelated\";\n").unwrap();
        git(&repo, &["add", "secret.rs"]);

        let parked = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .expect("publish parks an approval");
        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        let published = wait_for_publication_count(&pool, document_id, 1).await;
        assert!(published[0].git_commit.is_some());

        // The commit contains ONLY the published path.
        let committed = git_output(
            &repo,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
        );
        let committed: Vec<&str> = committed.lines().collect();
        assert_eq!(
            committed,
            vec!["docs/architecture.md"],
            "the commit must contain only the published path, not unrelated staged work: {committed:?}"
        );

        // The unrelated file remains staged (uncommitted), exactly as the
        // user left it — the publish must not touch it at all.
        let status = git_output(&repo, &["status", "--porcelain"]);
        assert!(
            status.contains("secret.rs"),
            "the unrelated staged file must remain staged, untouched: {status}"
        );
        let head_files = git_output(&repo, &["show", "--name-only", "--format="]);
        assert!(
            !head_files.contains("secret.rs"),
            "secret.rs must never appear in the publish commit: {head_files}"
        );
    }

    #[tokio::test]
    async fn republish_no_op_holds_even_with_unrelated_staged_changes() {
        // Regression test (review fix): before the scoping fix, an unscoped
        // `git diff --cached --name-only` was non-empty whenever ANYTHING was
        // staged, defeating the unchanged-content no-op the moment the
        // primary checkout had unrelated staged work.
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let first = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .unwrap();
        resolve(
            &pool,
            &approvals,
            first.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        wait_for_publication_count(&pool, document_id, 1).await;
        let head_after_first = git_output(&repo, &["rev-parse", "HEAD"]);

        // Stage an unrelated file, THEN republish the SAME (unchanged) content.
        std::fs::write(repo.join("wip.rs"), "// work in progress\n").unwrap();
        git(&repo, &["add", "wip.rs"]);

        let second = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .unwrap();
        resolve(
            &pool,
            &approvals,
            second.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        wait_for_publication_count(&pool, document_id, 2).await;

        assert_eq!(
            git_output(&repo, &["rev-parse", "HEAD"]),
            head_after_first,
            "unchanged content must stay a no-op even with unrelated staged work present"
        );
        // The unrelated file is still staged, untouched by the no-op path.
        let status = git_output(&repo, &["status", "--porcelain"]);
        assert!(
            status.contains("wip.rs"),
            "the unrelated staged file must remain staged: {status}"
        );
    }

    #[tokio::test]
    async fn docs_branch_publish_never_touches_the_primary_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let head_before = git_output(&repo, &["rev-parse", "HEAD"]);
        let branch_before = git_output(&repo, &["symbolic-ref", "--short", "HEAD"]);

        let parked = publisher
            .publish(publish_request(
                document_id,
                WireTarget::DocsBranchCommit {
                    branch: "docs/publish".to_string(),
                    path: "docs/architecture.md".to_string(),
                },
            ))
            .await
            .expect("publish parks an approval");

        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        let published = wait_for_publication_count(&pool, document_id, 1).await;
        assert!(published[0].git_commit.is_some());

        // The primary checkout: same branch, same HEAD, clean status, and the
        // file never lands there.
        assert_eq!(git_output(&repo, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            git_output(&repo, &["symbolic-ref", "--short", "HEAD"]),
            branch_before
        );
        let status = git_output(&repo, &["status", "--porcelain"]);
        assert!(
            status.is_empty(),
            "primary checkout must stay clean: {status}"
        );
        assert!(
            !repo.join("docs/architecture.md").exists(),
            "the file must not land in the primary checkout"
        );

        // The docs branch itself really carries the commit.
        let branch_log = git_output(&repo, &["log", "docs/publish", "--oneline"]);
        assert!(branch_log.contains("publish"));

        // The scratch worktree was cleaned up (not left registered).
        let worktrees = git_output(&repo, &["worktree", "list"]);
        assert_eq!(
            worktrees.lines().count(),
            1,
            "only the primary worktree should remain: {worktrees}"
        );
    }

    /// The status lifecycle used to be decorative: publishing recorded a
    /// publication row but the document stayed `draft` forever. A publish that
    /// reaches a commit now transitions `Draft` -> `Published`, WITHOUT bumping
    /// the content revision (status is lifecycle state, not content, so pending
    /// suggestions keep their anchors).
    #[tokio::test]
    async fn a_successful_publish_moves_the_document_from_draft_to_published() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let before = DocumentStore::new()
            .snapshot_document(&pool, document_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.status, DocumentStatus::Draft);

        let parked = publisher
            .publish(publish_request(
                document_id,
                WireTarget::DocsBranchCommit {
                    branch: "docs/publish".to_string(),
                    path: "docs/architecture.md".to_string(),
                },
            ))
            .await
            .expect("publish parks an approval");
        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        wait_for_publication_count(&pool, document_id, 1).await;

        let after = DocumentStore::new()
            .snapshot_document(&pool, document_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, DocumentStatus::Published);
        assert_eq!(
            after.revision, before.revision,
            "a status transition is metadata, never a content revision"
        );
    }

    /// A publish that never executes (rejected at the approval) leaves the
    /// document a draft — the transition follows the WRITE, not the request.
    #[tokio::test]
    async fn a_rejected_publish_leaves_the_document_a_draft() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let parked = publisher
            .publish(publish_request(
                document_id,
                WireTarget::DocsBranchCommit {
                    branch: "docs/publish".to_string(),
                    path: "docs/architecture.md".to_string(),
                },
            ))
            .await
            .expect("publish parks an approval");
        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Reject,
        )
        .await;
        // Give the background task a beat to observe the rejection.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let after = DocumentStore::new()
            .snapshot_document(&pool, document_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, DocumentStatus::Draft);
        assert!(codypendent_knowledge::publications(&pool, document_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn docs_branch_commit_reuses_an_existing_branch_across_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());
        let target = || WireTarget::DocsBranchCommit {
            branch: "docs/publish".to_string(),
            path: "docs/architecture.md".to_string(),
        };

        let first = publisher
            .publish(publish_request(document_id, target()))
            .await
            .unwrap();
        resolve(
            &pool,
            &approvals,
            first.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        wait_for_publication_count(&pool, document_id, 1).await;

        // A second publish to the SAME branch must reuse it (not fail trying
        // to re-create it) and leave exactly one worktree registered again.
        let second = publisher
            .publish(publish_request(document_id, target()))
            .await
            .unwrap();
        resolve(
            &pool,
            &approvals,
            second.approval_id,
            ApprovalDecision::Approve,
        )
        .await;
        wait_for_publication_count(&pool, document_id, 2).await;

        let worktrees = git_output(&repo, &["worktree", "list"]);
        assert_eq!(
            worktrees.lines().count(),
            1,
            "no leaked worktree: {worktrees}"
        );
        // Two lines total: the repo's own "initial" commit plus the ONE
        // publish commit from the first publish — the second (unchanged)
        // publish must not add a third.
        let branch_log = git_output(&repo, &["log", "docs/publish", "--oneline"]);
        assert_eq!(
            branch_log.lines().count(),
            2,
            "unchanged content is a no-op the second time too: {branch_log}"
        );
    }

    #[tokio::test]
    async fn approval_card_content_carries_the_plan_verbatim() {
        // STEP 4.4.2: every publish displays target, changed files, and the
        // resulting Git action before approval — assert the parked approval's
        // own `ProposedAction::PublishDocument` carries exactly what the seam
        // returned to the client, unedited.
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let document_id = seed_document(&pool, "Architecture").await;
        let approvals = ApprovalBroker::new();
        let publisher = build_publisher(pool.clone(), approvals.clone(), repo.clone(), dir.path());

        let parked = publisher
            .publish(publish_request(
                document_id,
                wire_target("docs/architecture.md"),
            ))
            .await
            .unwrap();

        let pending = approvals.reload_pending(&pool).await.unwrap();
        let approval = pending
            .iter()
            .find(|p| p.approval_id == parked.approval_id)
            .expect("the parked approval reloads");
        match &approval.action {
            ProposedAction::PublishDocument {
                document_id: id,
                target,
                changed_files,
                git_action,
            } => {
                assert_eq!(*id, document_id);
                assert_eq!(*target, parked.target_description);
                assert_eq!(*changed_files, parked.changed_files);
                assert_eq!(*git_action, parked.git_action);
            }
            other => panic!("expected ProposedAction::PublishDocument, got {other:?}"),
        }

        // Clean up: reject so the test leaves no dangling waiter.
        resolve(
            &pool,
            &approvals,
            parked.approval_id,
            ApprovalDecision::Reject,
        )
        .await;
    }

    #[tokio::test]
    async fn documentation_pr_target_without_a_github_client_fails_before_touching_git() {
        let dir = tempfile::tempdir().unwrap();
        let pool = temp_pool(dir.path()).await;
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let artifacts = ArtifactStore::new(dir.path().join("artifacts"));
        let document_id = DocumentId::new();

        let plan = PublishPlan {
            target: KnowledgeTarget::DocumentationPr {
                branch: "docs/publish".to_string(),
                path: "docs/x.md".to_string(),
                title: "Publish: X".to_string(),
            },
            changed_files: vec!["docs/x.md".to_string()],
            git_action: "open documentation PR".to_string(),
            rendered: "# X\n".to_string(),
            rendered_hash: "deadbeef".to_string(),
            revision: 1,
        };

        let error = execute_plan(&repo, &pool, &artifacts, None, &plan, document_id)
            .await
            .expect_err("no github client must fail");
        assert!(matches!(error, PublishExecError::NoGitHubClient));

        // Nothing was touched: no branch, no worktree.
        let branches = git_output(&repo, &["branch", "--list"]);
        assert!(
            !branches.contains("docs/publish"),
            "no branch must be created: {branches}"
        );
        let worktrees = git_output(&repo, &["worktree", "list"]);
        assert_eq!(
            worktrees.lines().count(),
            1,
            "no worktree must be created: {worktrees}"
        );
    }

    #[tokio::test]
    async fn docs_branch_push_reaches_a_real_remote() {
        // Proves the exact invocation `execute_plan`'s PR target relies on
        // (`git push origin <branch>:<branch>`) actually lands the commit on a
        // remote, independent of GitHub-hostname resolution (see the module
        // test notes below for why the PR-open step is tested separately).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let bare = dir.path().join("origin.git");
        git(
            dir.path(),
            &["init", "--bare", "-q", bare.to_str().unwrap()],
        );
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);

        let sha = commit_on_docs_branch(&repo, "docs/publish", "docs/x.md", "# X\n")
            .await
            .expect("commit on docs branch");
        run_git(&repo, &["push", "origin", "docs/publish:docs/publish"])
            .await
            .expect("push to origin");

        let remote_branches = git_output(&bare, &["branch", "--list"]);
        assert!(
            remote_branches.contains("docs/publish"),
            "the branch must exist on the remote: {remote_branches}"
        );
        let remote_sha = git_output(&bare, &["rev-parse", "docs/publish"]);
        assert_eq!(
            remote_sha, sha,
            "the pushed commit must match what was committed"
        );
    }

    #[derive(Default)]
    struct FakeGitHub {
        prs: std::sync::Mutex<Vec<PullRequest>>,
    }

    fn unused_error() -> GitHubError {
        GitHubError::Api {
            status: 501,
            message: "not used in this test".to_string(),
        }
    }

    #[async_trait::async_trait]
    impl GitHubApi for FakeGitHub {
        async fn get_pull_request(
            &self,
            _repo: &RepoId,
            _number: u64,
        ) -> Result<PullRequest, GitHubError> {
            Err(unused_error())
        }

        async fn list_check_runs(
            &self,
            _repo: &RepoId,
            _git_ref: &str,
        ) -> Result<Vec<CheckRun>, GitHubError> {
            Ok(Vec::new())
        }

        async fn download_job_logs(
            &self,
            _repo: &RepoId,
            _job_id: u64,
        ) -> Result<Vec<u8>, GitHubError> {
            Ok(Vec::new())
        }

        async fn list_review_comments(
            &self,
            _repo: &RepoId,
            _number: u64,
        ) -> Result<Vec<ReviewComment>, GitHubError> {
            Ok(Vec::new())
        }

        async fn create_review_comment(
            &self,
            _repo: &RepoId,
            _number: u64,
            _body: &str,
            _idempotency_key: &str,
        ) -> Result<ReviewComment, GitHubError> {
            Err(unused_error())
        }

        async fn create_draft_pull_request(
            &self,
            _repo: &RepoId,
            req: &NewPullRequest,
            idempotency_key: &str,
        ) -> Result<PullRequest, GitHubError> {
            let mut prs = self.prs.lock().unwrap();
            if let Some(existing) = prs.iter().find(|pr| {
                pr.body
                    .as_deref()
                    .map(|body| idempotency::body_matches_key(body, idempotency_key))
                    .unwrap_or(false)
            }) {
                return Ok(existing.clone());
            }
            let number = prs.len() as u64 + 1;
            let body =
                idempotency::body_with_marker(req.body.as_deref().unwrap_or(""), idempotency_key);
            let pr = PullRequest {
                number,
                title: req.title.clone(),
                body: Some(body),
                state: "open".to_string(),
                draft: true,
                html_url: format!("https://github.com/octocat/hello-world/pull/{number}"),
                head: None,
                base: None,
            };
            prs.push(pr.clone());
            Ok(pr)
        }

        async fn update_pull_request(
            &self,
            _repo: &RepoId,
            _number: u64,
            _req: &UpdatePullRequest,
        ) -> Result<PullRequest, GitHubError> {
            Err(unused_error())
        }

        async fn create_check_run_summary(
            &self,
            _repo: &RepoId,
            _req: &NewCheckRun,
            _idempotency_key: &str,
        ) -> Result<CheckRun, GitHubError> {
            Err(unused_error())
        }
    }

    #[tokio::test]
    async fn documentation_pr_is_idempotent_on_a_retried_publish() {
        // "PR target against the GitHub double: idempotent (re-run finds the
        // marker)" — the same document+branch must resolve to the SAME PR on a
        // retried publish rather than opening a duplicate, exactly as
        // `github.create_draft_pull_request`'s hidden-marker convention
        // guarantees for a real retried command.
        let github = FakeGitHub::default();
        let repo = RepoId::new("octocat", "hello-world");
        let document_id = DocumentId::new();

        open_documentation_pr(
            &github,
            &repo,
            "docs/publish",
            "main",
            "Publish: X",
            document_id,
        )
        .await
        .expect("first open succeeds");
        assert_eq!(github.prs.lock().unwrap().len(), 1);

        // A retried publish of the SAME document to the SAME branch.
        open_documentation_pr(
            &github,
            &repo,
            "docs/publish",
            "main",
            "Publish: X",
            document_id,
        )
        .await
        .expect("retried open finds the marker");
        assert_eq!(
            github.prs.lock().unwrap().len(),
            1,
            "a retry must not open a second PR"
        );

        // A DIFFERENT document (or branch) is a distinct PR.
        open_documentation_pr(
            &github,
            &repo,
            "docs/publish",
            "main",
            "Publish: Y",
            DocumentId::new(),
        )
        .await
        .expect("a different document opens its own PR");
        assert_eq!(github.prs.lock().unwrap().len(), 2);
    }
}

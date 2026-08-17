//! Coordinated multi-repository campaign execution engine.
//!
//! A campaign is a COORDINATOR, never an authority. It aggregates the outcomes
//! of ordinary per-repository workflow runs created through
//! `WorkflowStore::create_run_idempotent_owned`.
//! It grants nothing: no shared worktree, no shared budget, no blanket approval,
//! and no shared secret lease.

use codypendent_workflow::compile::CompiledWorkflow;
use codypendent_workflow::store::{WorkflowRunAttribution, WorkflowStore};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

/// Kind of multi-repository campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CampaignKind {
    #[serde(rename = "api-migration")]
    ApiMigration,
    #[serde(rename = "schema-migration")]
    SchemaMigration,
    #[serde(rename = "dependency-upgrade")]
    DependencyUpgrade,
    #[serde(rename = "ownership-review")]
    OwnershipReview,
    #[serde(rename = "custom")]
    Custom,
}

impl CampaignKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiMigration => "api-migration",
            Self::SchemaMigration => "schema-migration",
            Self::DependencyUpgrade => "dependency-upgrade",
            Self::OwnershipReview => "ownership-review",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "api-migration" | "apimigration" => Self::ApiMigration,
            "schema-migration" | "schemamigration" => Self::SchemaMigration,
            "dependency-upgrade" | "dependencyupgrade" => Self::DependencyUpgrade,
            "ownership-review" | "ownershipreview" => Self::OwnershipReview,
            _ => Self::Custom,
        }
    }
}

impl fmt::Display for CampaignKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Overall lifecycle state of a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CampaignState {
    #[serde(rename = "planning")]
    Planning,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "partially-failed")]
    PartiallyFailed,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl CampaignState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Running => "running",
            Self::PartiallyFailed => "partially-failed",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "planning" => Self::Planning,
            "running" => Self::Running,
            "partially-failed" | "partiallyfailed" => Self::PartiallyFailed,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Planning,
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::PartiallyFailed
        )
    }
}

/// State of a target repository within a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CampaignRepositoryState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "denied")]
    Denied,
    #[serde(rename = "skipped")]
    Skipped,
}

impl CampaignRepositoryState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Skipped => "skipped",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "denied" => Self::Denied,
            "skipped" => Self::Skipped,
            _ => Self::Pending,
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Skipped
        )
    }
}

/// Approval mode per enrolled repository.
/// Blanket approval across repositories is strictly prohibited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CampaignApprovalMode {
    #[default]
    #[serde(rename = "per-effect")]
    PerEffect,
    #[serde(rename = "per-run")]
    PerRun,
}

impl CampaignApprovalMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerEffect => "per-effect",
            Self::PerRun => "per-run",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "per-run" | "perrun" => Self::PerRun,
            _ => Self::PerEffect,
        }
    }
}

/// Decision on a repository-scoped approval action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CampaignApprovalDecision {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "approved")]
    Approved,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "expired")]
    Expired,
}

impl CampaignApprovalDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

/// Multi-repo campaign record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    pub id: String,
    pub owner_uid: i64,
    pub title: String,
    pub kind: CampaignKind,
    pub workflow_id: String,
    pub idempotency_key: String,
    pub state: CampaignState,
    pub repository_count: i64,
    pub created_at: String,
    pub updated_at: String,
    pub terminal_at: Option<String>,
}

/// Target repository enrolled in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignRepository {
    pub campaign_id: String,
    pub repository_id: String,
    pub federated_id: String,
    pub worktree_path: Option<String>,
    pub budget_minor_units: Option<i64>,
    pub approval_mode: CampaignApprovalMode,
    pub state: CampaignRepositoryState,
    pub enrolled_at: String,
    pub terminal_at: Option<String>,
}

/// Specification for enrolling a repository in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetRepositorySpec {
    pub repository_id: String,
    pub federated_id: String,
    pub worktree_path: Option<String>,
    pub budget_minor_units: Option<i64>,
    pub approval_mode: CampaignApprovalMode,
}

/// Child run record tracking one workflow run attempt for a repository in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignRun {
    pub campaign_id: String,
    pub repository_id: String,
    pub run_id: String,
    pub attempt: i64,
    pub idempotency_key: String,
    pub state: String,
    pub created_at: String,
    pub terminal_at: Option<String>,
}

/// Approval record bound to a specific repository in a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignApproval {
    pub campaign_id: String,
    pub repository_id: String,
    pub approval_id: String,
    pub action_digest: String,
    pub decision: CampaignApprovalDecision,
    pub decided_at: Option<String>,
    pub decided_by_uid: Option<i64>,
}

/// Effect ledger entry recording an effect applied in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignEffect {
    pub id: String,
    pub campaign_id: String,
    pub repository_id: String,
    pub run_id: String,
    pub effect_kind: String,
    pub effect_digest: String,
    pub applied_at: String,
}

#[derive(Debug, Error)]
pub enum CampaignError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("workflow error: {0}")]
    Workflow(#[from] codypendent_workflow::store::WorkflowStoreError),
    #[error("campaign not found: {0}")]
    NotFound(String),
    #[error("invalid campaign state transition from {from:?} to {to:?}")]
    InvalidStateTransition {
        from: CampaignState,
        to: CampaignState,
    },
    #[error("repository {0} not enrolled in campaign {1}")]
    RepositoryNotEnrolled(String, String),
    #[error(
        "duplicate effect: effect {effect_digest} already applied in repository {repository_id}"
    )]
    DuplicateEffect {
        repository_id: String,
        effect_digest: String,
    },
}

/// Campaign coordinator and execution engine.
pub struct CampaignEngine;

impl CampaignEngine {
    /// Creates or adopts a campaign idempotently.
    ///
    /// If a campaign with `(owner_uid, idempotency_key)` already exists,
    /// returns the existing campaign and its enrolled repositories.
    pub async fn create_campaign_idempotent(
        pool: &SqlitePool,
        owner_uid: i64,
        title: &str,
        kind: CampaignKind,
        workflow_id: &str,
        idempotency_key: &str,
        target_repositories: &[TargetRepositorySpec],
    ) -> Result<(Campaign, Vec<CampaignRepository>), CampaignError> {
        let mut tx = pool.begin().await?;

        // Check for existing campaign
        let existing = sqlx::query(
            "SELECT id, owner_uid, title, kind, workflow_id, idempotency_key, state, \
                    repository_count, created_at, updated_at, terminal_at \
             FROM campaigns \
             WHERE owner_uid = ? AND idempotency_key = ?",
        )
        .bind(owner_uid)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(r) = existing {
            let id: String = r.get("id");
            let kind_str: String = r.get("kind");
            let state_str: String = r.get("state");

            let campaign = Campaign {
                id: id.clone(),
                owner_uid: r.get("owner_uid"),
                title: r.get("title"),
                kind: CampaignKind::parse(&kind_str),
                workflow_id: r.get("workflow_id"),
                idempotency_key: r.get("idempotency_key"),
                state: CampaignState::parse(&state_str),
                repository_count: r.get("repository_count"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                terminal_at: r.get("terminal_at"),
            };

            let repo_rows = sqlx::query(
                "SELECT campaign_id, repository_id, federated_id, worktree_path, \
                        budget_minor_units, approval_mode, state, enrolled_at, terminal_at \
                 FROM campaign_repositories \
                 WHERE campaign_id = ? \
                 ORDER BY enrolled_at ASC",
            )
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;

            let repos = repo_rows
                .into_iter()
                .map(|row| {
                    let mode_str: String = row.get("approval_mode");
                    let st_str: String = row.get("state");
                    CampaignRepository {
                        campaign_id: row.get("campaign_id"),
                        repository_id: row.get("repository_id"),
                        federated_id: row.get("federated_id"),
                        worktree_path: row.get("worktree_path"),
                        budget_minor_units: row.get("budget_minor_units"),
                        approval_mode: CampaignApprovalMode::parse(&mode_str),
                        state: CampaignRepositoryState::parse(&st_str),
                        enrolled_at: row.get("enrolled_at"),
                        terminal_at: row.get("terminal_at"),
                    }
                })
                .collect();

            tx.commit().await?;
            return Ok((campaign, repos));
        }

        let id = Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let repo_count = target_repositories.len() as i64;

        sqlx::query(
            "INSERT INTO campaigns \
             (id, owner_uid, title, kind, workflow_id, idempotency_key, state, repository_count, created_at, updated_at, terminal_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'planning', ?, ?, ?, NULL)",
        )
        .bind(&id)
        .bind(owner_uid)
        .bind(title)
        .bind(kind.as_str())
        .bind(workflow_id)
        .bind(idempotency_key)
        .bind(repo_count)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let mut repos = Vec::with_capacity(target_repositories.len());
        for target in target_repositories {
            sqlx::query(
                "INSERT INTO campaign_repositories \
                 (campaign_id, repository_id, federated_id, worktree_path, budget_minor_units, approval_mode, state, enrolled_at, terminal_at) \
                 VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, NULL)",
            )
            .bind(&id)
            .bind(&target.repository_id)
            .bind(&target.federated_id)
            .bind(&target.worktree_path)
            .bind(target.budget_minor_units)
            .bind(target.approval_mode.as_str())
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            repos.push(CampaignRepository {
                campaign_id: id.clone(),
                repository_id: target.repository_id.clone(),
                federated_id: target.federated_id.clone(),
                worktree_path: target.worktree_path.clone(),
                budget_minor_units: target.budget_minor_units,
                approval_mode: target.approval_mode,
                state: CampaignRepositoryState::Pending,
                enrolled_at: now.clone(),
                terminal_at: None,
            });
        }

        let campaign = Campaign {
            id,
            owner_uid,
            title: title.to_string(),
            kind,
            workflow_id: workflow_id.to_string(),
            idempotency_key: idempotency_key.to_string(),
            state: CampaignState::Planning,
            repository_count: repo_count,
            created_at: now.clone(),
            updated_at: now,
            terminal_at: None,
        };

        tx.commit().await?;
        Ok((campaign, repos))
    }

    /// Triggers workflow runs for each enrolled repository.
    ///
    /// Uses `WorkflowStore::create_run_idempotent_owned` to instantiate standard
    /// child workflow runs. N target repositories produce N distinct child runs
    /// in `campaign_runs` with distinct `run_id`s.
    pub async fn trigger_campaign_runs(
        pool: &SqlitePool,
        workflow_store: &WorkflowStore,
        compiled: &CompiledWorkflow,
        campaign_id: &str,
        inputs: &serde_json::Value,
        manifest_yaml: Option<&str>,
    ) -> Result<Vec<CampaignRun>, CampaignError> {
        let mut tx = pool.begin().await?;

        let camp_row = sqlx::query("SELECT owner_uid, state FROM campaigns WHERE id = ?")
            .bind(campaign_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| CampaignError::NotFound(campaign_id.to_string()))?;

        let owner_uid: i64 = camp_row.get("owner_uid");

        let repo_rows = sqlx::query(
            "SELECT repository_id, worktree_path, state \
             FROM campaign_repositories \
             WHERE campaign_id = ?",
        )
        .bind(campaign_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut runs = Vec::new();
        let now = chrono::Utc::now().to_rfc3339();

        for r in repo_rows {
            let repo_id: String = r.get("repository_id");
            let worktree_path: Option<String> = r.get("worktree_path");
            let repo_state_str: String = r.get("state");
            let repo_state = CampaignRepositoryState::parse(&repo_state_str);

            // Only trigger pending repositories
            if repo_state != CampaignRepositoryState::Pending {
                continue;
            }

            let attempt = 1i64;
            let run_idempotency_key =
                format!("campaign:{campaign_id}:repo:{repo_id}:attempt:{attempt}");

            // Create run via WorkflowStore::create_run_idempotent_owned
            let repo_ref = worktree_path.as_deref().unwrap_or(repo_id.as_str());
            let attribution = WorkflowRunAttribution {
                manifest: manifest_yaml,
                repository: Some(repo_ref),
                owner_uid: owner_uid as u32,
            };

            let run_id = workflow_store
                .create_run_idempotent_owned(
                    pool,
                    compiled,
                    &run_idempotency_key,
                    inputs,
                    attribution,
                )
                .await?;

            sqlx::query(
                "INSERT INTO campaign_runs \
                 (campaign_id, repository_id, run_id, attempt, idempotency_key, state, created_at, terminal_at) \
                 VALUES (?, ?, ?, ?, ?, 'running', ?, NULL)",
            )
            .bind(campaign_id)
            .bind(&repo_id)
            .bind(&run_id)
            .bind(attempt)
            .bind(&run_idempotency_key)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE campaign_repositories \
                 SET state = 'running' \
                 WHERE campaign_id = ? AND repository_id = ?",
            )
            .bind(campaign_id)
            .bind(&repo_id)
            .execute(&mut *tx)
            .await?;

            runs.push(CampaignRun {
                campaign_id: campaign_id.to_string(),
                repository_id: repo_id,
                run_id,
                attempt,
                idempotency_key: run_idempotency_key,
                state: "running".to_string(),
                created_at: now.clone(),
                terminal_at: None,
            });
        }

        sqlx::query(
            "UPDATE campaigns \
             SET state = 'running', updated_at = ? \
             WHERE id = ?",
        )
        .bind(&now)
        .bind(campaign_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(runs)
    }

    /// Records an approval decision for a specific repository.
    ///
    /// **CRITICAL**: Bound per repository: the SAME digest approved in
    /// repository A confers nothing in repository B. Blanket approval is prohibited.
    pub async fn record_approval(
        pool: &SqlitePool,
        campaign_id: &str,
        repository_id: &str,
        approval_id: &str,
        action_digest: &str,
        decision: CampaignApprovalDecision,
        decided_by_uid: i64,
    ) -> Result<CampaignApproval, CampaignError> {
        let now = chrono::Utc::now().to_rfc3339();

        let mut tx = pool.begin().await?;

        // Insert or update approval
        sqlx::query(
            "INSERT INTO campaign_approvals \
             (campaign_id, repository_id, approval_id, action_digest, decision, decided_at, decided_by_uid) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (campaign_id, repository_id, approval_id) DO UPDATE SET \
                decision = excluded.decision, \
                decided_at = excluded.decided_at, \
                decided_by_uid = excluded.decided_by_uid",
        )
        .bind(campaign_id)
        .bind(repository_id)
        .bind(approval_id)
        .bind(action_digest)
        .bind(decision.as_str())
        .bind(&now)
        .bind(decided_by_uid)
        .execute(&mut *tx)
        .await?;

        // If rejected, mark repository as denied
        if decision == CampaignApprovalDecision::Rejected {
            sqlx::query(
                "UPDATE campaign_repositories \
                 SET state = 'denied', terminal_at = ? \
                 WHERE campaign_id = ? AND repository_id = ?",
            )
            .bind(&now)
            .bind(campaign_id)
            .bind(repository_id)
            .execute(&mut *tx)
            .await?;

            // Update rollup
            Self::recalculate_campaign_rollup(&mut tx, campaign_id).await?;
        }

        tx.commit().await?;

        Ok(CampaignApproval {
            campaign_id: campaign_id.to_string(),
            repository_id: repository_id.to_string(),
            approval_id: approval_id.to_string(),
            action_digest: action_digest.to_string(),
            decision,
            decided_at: Some(now),
            decided_by_uid: Some(decided_by_uid),
        })
    }

    /// Records an applied effect in the effect ledger.
    ///
    /// Returns `Ok(true)` if effect was newly recorded, or `Ok(false)` if
    /// the effect was already applied in that repository (idempotent no-op).
    pub async fn apply_effect(
        pool: &SqlitePool,
        campaign_id: &str,
        repository_id: &str,
        run_id: &str,
        effect_kind: &str,
        effect_digest: &str,
    ) -> Result<bool, CampaignError> {
        let id = Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        let res = sqlx::query(
            "INSERT OR IGNORE INTO campaign_effects \
             (id, campaign_id, repository_id, run_id, effect_kind, effect_digest, applied_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(campaign_id)
        .bind(repository_id)
        .bind(run_id)
        .bind(effect_kind)
        .bind(effect_digest)
        .bind(&now)
        .execute(pool)
        .await?;

        Ok(res.rows_affected() > 0)
    }

    /// Records completion of a repository's run and updates rollups.
    pub async fn record_run_completion(
        pool: &SqlitePool,
        campaign_id: &str,
        repository_id: &str,
        run_id: &str,
        outcome: CampaignRepositoryState,
    ) -> Result<CampaignState, CampaignError> {
        let mut tx = pool.begin().await?;
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE campaign_runs \
             SET state = ?, terminal_at = ? \
             WHERE campaign_id = ? AND repository_id = ? AND run_id = ?",
        )
        .bind(outcome.as_str())
        .bind(&now)
        .bind(campaign_id)
        .bind(repository_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE campaign_repositories \
             SET state = ?, terminal_at = ? \
             WHERE campaign_id = ? AND repository_id = ?",
        )
        .bind(outcome.as_str())
        .bind(&now)
        .bind(campaign_id)
        .bind(repository_id)
        .execute(&mut *tx)
        .await?;

        let new_state = Self::recalculate_campaign_rollup(&mut tx, campaign_id).await?;
        tx.commit().await?;

        Ok(new_state)
    }

    /// Re-drives a campaign across partial failures.
    ///
    /// Only failed and denied repositories are retried. Already succeeded
    /// repositories are never re-run.
    pub async fn retry_campaign(
        pool: &SqlitePool,
        workflow_store: &WorkflowStore,
        compiled: &CompiledWorkflow,
        campaign_id: &str,
        inputs: &serde_json::Value,
        manifest_yaml: Option<&str>,
    ) -> Result<Vec<CampaignRun>, CampaignError> {
        let mut tx = pool.begin().await?;

        let camp_row = sqlx::query("SELECT owner_uid FROM campaigns WHERE id = ?")
            .bind(campaign_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| CampaignError::NotFound(campaign_id.to_string()))?;

        let owner_uid: i64 = camp_row.get("owner_uid");

        // Find failed or denied repositories
        let retry_repos = sqlx::query(
            "SELECT repository_id, worktree_path, state \
             FROM campaign_repositories \
             WHERE campaign_id = ? AND state IN ('failed', 'denied')",
        )
        .bind(campaign_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut new_runs = Vec::new();
        let now = chrono::Utc::now().to_rfc3339();

        for r in retry_repos {
            let repo_id: String = r.get("repository_id");
            let worktree_path: Option<String> = r.get("worktree_path");

            // Query highest attempt for this repo
            let max_attempt_row = sqlx::query(
                "SELECT COALESCE(MAX(attempt), 0) as max_attempt \
                 FROM campaign_runs \
                 WHERE campaign_id = ? AND repository_id = ?",
            )
            .bind(campaign_id)
            .bind(&repo_id)
            .fetch_one(&mut *tx)
            .await?;

            let last_attempt: i64 = max_attempt_row.get("max_attempt");
            let next_attempt = last_attempt + 1;

            let run_idempotency_key =
                format!("campaign:{campaign_id}:repo:{repo_id}:attempt:{next_attempt}");

            let repo_ref = worktree_path.as_deref().unwrap_or(repo_id.as_str());
            let attribution = WorkflowRunAttribution {
                manifest: manifest_yaml,
                repository: Some(repo_ref),
                owner_uid: owner_uid as u32,
            };

            let run_id = workflow_store
                .create_run_idempotent_owned(
                    pool,
                    compiled,
                    &run_idempotency_key,
                    inputs,
                    attribution,
                )
                .await?;

            sqlx::query(
                "INSERT INTO campaign_runs \
                 (campaign_id, repository_id, run_id, attempt, idempotency_key, state, created_at, terminal_at) \
                 VALUES (?, ?, ?, ?, ?, 'running', ?, NULL)",
            )
            .bind(campaign_id)
            .bind(&repo_id)
            .bind(&run_id)
            .bind(next_attempt)
            .bind(&run_idempotency_key)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE campaign_repositories \
                 SET state = 'running', terminal_at = NULL \
                 WHERE campaign_id = ? AND repository_id = ?",
            )
            .bind(campaign_id)
            .bind(&repo_id)
            .execute(&mut *tx)
            .await?;

            new_runs.push(CampaignRun {
                campaign_id: campaign_id.to_string(),
                repository_id: repo_id,
                run_id,
                attempt: next_attempt,
                idempotency_key: run_idempotency_key,
                state: "running".to_string(),
                created_at: now.clone(),
                terminal_at: None,
            });
        }

        sqlx::query(
            "UPDATE campaigns \
             SET state = 'running', terminal_at = NULL, updated_at = ? \
             WHERE id = ?",
        )
        .bind(&now)
        .bind(campaign_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(new_runs)
    }

    async fn recalculate_campaign_rollup(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        campaign_id: &str,
    ) -> Result<CampaignState, CampaignError> {
        let rows = sqlx::query("SELECT state FROM campaign_repositories WHERE campaign_id = ?")
            .bind(campaign_id)
            .fetch_all(&mut **tx)
            .await?;

        if rows.is_empty() {
            return Ok(CampaignState::Planning);
        }

        let mut any_running = false;
        let mut any_pending = false;
        let mut any_failed_or_denied = false;
        let mut all_succeeded_or_skipped = true;

        for r in rows {
            let s: String = r.get("state");
            let st = CampaignRepositoryState::parse(&s);
            match st {
                CampaignRepositoryState::Running => {
                    any_running = true;
                    all_succeeded_or_skipped = false;
                }
                CampaignRepositoryState::Pending => {
                    any_pending = true;
                    all_succeeded_or_skipped = false;
                }
                CampaignRepositoryState::Failed | CampaignRepositoryState::Denied => {
                    any_failed_or_denied = true;
                    all_succeeded_or_skipped = false;
                }
                CampaignRepositoryState::Succeeded | CampaignRepositoryState::Skipped => {}
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        let new_state = if any_running || any_pending {
            CampaignState::Running
        } else if all_succeeded_or_skipped {
            CampaignState::Completed
        } else if any_failed_or_denied {
            CampaignState::PartiallyFailed
        } else {
            CampaignState::Running
        };

        let terminal_at = if new_state.is_terminal() {
            Some(now.clone())
        } else {
            None
        };

        sqlx::query(
            "UPDATE campaigns \
             SET state = ?, updated_at = ?, terminal_at = ? \
             WHERE id = ?",
        )
        .bind(new_state.as_str())
        .bind(&now)
        .bind(terminal_at)
        .bind(campaign_id)
        .execute(&mut **tx)
        .await?;

        Ok(new_state)
    }
}

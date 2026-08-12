//! The concrete blackboard seams (Phase 5 STEP 5.3): the bridge between the
//! runtime's [`BlackboardChannel`] tool seam, the daemon's [`BlackboardReader`]
//! read seam, and `codypendent-workflow`'s authoritative
//! [`BlackboardStore`](codypendent_workflow::BlackboardStore).
//!
//! Like [`RuntimeExecutor`](crate::executor::RuntimeExecutor) and
//! [`KnowledgeDocumentMutator`](crate::documents::KnowledgeDocumentMutator), these
//! live in the assembly binary because it alone can name all three layers — the
//! runtime (which defines the tool seam), the daemon (which defines the read seam +
//! the fan-out hub), and the workflow crate (which owns the store). Two pieces:
//!
//! * [`AssemblyBlackboardChannel`] — implements the runtime's
//!   [`BlackboardChannel`]: an agent's `blackboard.post` / `blackboard.query`
//!   applies to the run's board through the `BlackboardStore` on the daemon's pool,
//!   and each posted (or superseded) artifact is fanned out to the run's
//!   subscribers over the daemon's [`BlackboardHub`] (persist-before-publish — the
//!   store commit happens first, then the hub publish).
//!
//! * [`WorkflowBlackboardReader`] — implements the daemon's [`BlackboardReader`]:
//!   projects a run's board (kind-filtered) into [`BlackboardItemView`]s for the
//!   `ReadBlackboard` command reply.
//!
//! Every workflow-store error is mapped to the seam's own structured error (the
//! runtime's [`BlackboardChannelError`] or a protocol [`CodypendentError`]) so no
//! caller branches on message text, and internals never leak.

use async_trait::async_trait;
use codypendent_daemon::blackboard::{
    BlackboardHub, BlackboardReadFuture, BlackboardReader, BlackboardWriteFuture, BlackboardWriter,
    BoardTarget, PostBlackboardRequest, ReadBlackboardRequest, UpdateBlackboardRequest,
};
use codypendent_protocol::{board_scope_id, BlackboardItemView, CodypendentError};
use codypendent_runtime::blackboard::{
    BlackboardChannel, BlackboardChannelError, BlackboardPost, TaskBoardChannel, TaskCardChange,
    TaskCardDraft,
};
use codypendent_workflow::{
    BlackboardError, BlackboardItem, BlackboardKind, BlackboardStore, BoardFields,
    NewBlackboardItem, WorkflowStore, DEFAULT_TASK_STATUS,
};
use sqlx::SqlitePool;

/// Project a stored workflow artifact into its wire/runtime view, carrying the run
/// id with it so a live delivery routes without the enclosing frame.
fn item_to_view(workflow_run_id: &str, item: BlackboardItem) -> BlackboardItemView {
    BlackboardItemView {
        id: item.id,
        workflow_run_id: workflow_run_id.to_string(),
        kind: item.kind.as_str().to_string(),
        payload: item.payload,
        author: item.author,
        confidence: item.confidence,
        evidence: item.evidence,
        revision: item.revision,
        superseded_by: item.superseded_by,
        board_scope: item.board.board_scope,
        status: item.board.status,
        assignee: item.board.assignee,
        ordinal: item.board.ordinal,
    }
}

/// Map a workflow-store error to the runtime tool seam's structured error, so the
/// agent sees a legible, correctable reason (an evidence-required refusal most of
/// all) rather than an opaque backend failure.
fn map_channel_error(error: BlackboardError) -> BlackboardChannelError {
    match error {
        BlackboardError::EvidenceRequired(kind) => {
            BlackboardChannelError::EvidenceRequired(kind.to_string())
        }
        BlackboardError::NotFound(id) => BlackboardChannelError::NotFound(id),
        BlackboardError::AlreadySuperseded(id) => BlackboardChannelError::AlreadySuperseded(id),
        // Database / serialization failures: surface a backend error without
        // leaking the underlying detail's structure to the model.
        other => BlackboardChannelError::Backend(other.to_string()),
    }
}

/// Parse a manifest-facing kind string for the channel seam, mapping an unknown
/// kind to the seam's structured error.
fn channel_kind(kind: &str) -> Result<BlackboardKind, BlackboardChannelError> {
    BlackboardKind::parse_kind(kind)
        .ok_or_else(|| BlackboardChannelError::UnknownKind(kind.to_string()))
}

/// Implements the runtime's [`BlackboardChannel`] over the workflow store + pool,
/// fanning each posted artifact out over the daemon's per-run hub. Cheap to clone
/// (a pool handle, a stateless store, and an `Arc`-backed hub).
#[derive(Clone)]
pub struct AssemblyBlackboardChannel {
    pool: SqlitePool,
    store: BlackboardStore,
    hub: BlackboardHub,
}

impl AssemblyBlackboardChannel {
    /// Build the channel over the daemon's pool and the run fan-out hub.
    #[must_use]
    pub fn new(pool: SqlitePool, hub: BlackboardHub) -> Self {
        Self {
            pool,
            store: BlackboardStore::new(),
            hub,
        }
    }
}

#[async_trait]
impl BlackboardChannel for AssemblyBlackboardChannel {
    async fn post(
        &self,
        workflow_run_id: &str,
        post: BlackboardPost,
    ) -> Result<BlackboardItemView, BlackboardChannelError> {
        let kind = channel_kind(&post.kind)?;
        let new = NewBlackboardItem {
            kind,
            payload: post.payload,
            author: post.author,
            confidence: post.confidence,
            evidence: post.evidence,
            // A workflow artifact is not a board card: no scope, column, assignee,
            // or ordinal. (A `task` posted through this path still defaults to the
            // `todo` column in the store.)
            board: BoardFields::default(),
        };
        // A post carrying `supersedes` is a correction (posted at the next revision,
        // stamping the old row in one transaction); otherwise a fresh artifact.
        let item = match post.supersedes {
            Some(old_id) => {
                self.store
                    .supersede(&self.pool, workflow_run_id, &old_id, new)
                    .await
            }
            None => self.store.post(&self.pool, workflow_run_id, new).await,
        }
        .map_err(map_channel_error)?;

        // Persist-before-publish: the store commit above happened; only now fan the
        // artifact out to the run's subscribers (best-effort — the store is the
        // durable record).
        let view = item_to_view(workflow_run_id, item);
        self.hub.publish(workflow_run_id, view.clone());
        Ok(view)
    }

    async fn query(
        &self,
        workflow_run_id: &str,
        kind: Option<String>,
        include_superseded: bool,
    ) -> Result<Vec<BlackboardItemView>, BlackboardChannelError> {
        let kind = kind.as_deref().map(channel_kind).transpose()?;
        let items = self
            .store
            .query(&self.pool, workflow_run_id, kind, include_superseded)
            .await
            .map_err(map_channel_error)?;
        Ok(items
            .into_iter()
            .map(|item| item_to_view(workflow_run_id, item))
            .collect())
    }
}

/// Implements the daemon's [`BlackboardReader`] over the workflow store + pool for
/// the `ReadBlackboard` command. Cheap to clone.
#[derive(Clone)]
pub struct WorkflowBlackboardReader {
    pool: SqlitePool,
    store: BlackboardStore,
}

impl WorkflowBlackboardReader {
    /// Build the reader over the daemon's pool.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            store: BlackboardStore::new(),
        }
    }
}

impl BlackboardReader for WorkflowBlackboardReader {
    fn read(&self, request: ReadBlackboardRequest) -> BlackboardReadFuture<'_> {
        let pool = self.pool.clone();
        let store = self.store;
        Box::pin(async move {
            let ReadBlackboardRequest {
                workflow_run_id,
                board_repository,
                kind,
                include_superseded,
                client_id: _,
            } = request;

            // A repository-board read re-points the SAME query at the synthetic
            // board run. Nothing is created: a repository never written to simply
            // has no rows, and an empty board is the truthful answer (the store's
            // FK only matters for writes).
            let workflow_run_id = match board_repository.as_deref() {
                Some(repository) => board_scope_id(repository),
                None => workflow_run_id,
            };

            // An explicit kind filter that names no known artifact kind is a client
            // error (a typo like `test-result`), rejected legibly rather than
            // silently returning an empty board.
            let kind = match kind.as_deref() {
                Some(k) => Some(BlackboardKind::parse_kind(k).ok_or_else(|| {
                    CodypendentError::new(
                        "workflow.unknown-blackboard-kind",
                        format!("`{k}` is not a known blackboard artifact kind"),
                        false,
                    )
                })?),
                None => None,
            };

            let items = store
                .query(&pool, &workflow_run_id, kind, include_superseded)
                .await
                .map_err(|error| {
                    CodypendentError::new(
                        "workflow.blackboard-read-failed",
                        format!("could not read the blackboard: {error}"),
                        true,
                    )
                })?;
            Ok(items
                .into_iter()
                .map(|item| item_to_view(&workflow_run_id, item))
                .collect())
        })
    }
}

// ---------------------------------------------------------------------------
// The task board (Phase B kanban, rubric 10)
// ---------------------------------------------------------------------------

/// Everything a board write needs, shared by the client seam
/// ([`AssemblyBoardWriter`]) and the agent seam (the `task.*` tools): resolve the
/// board, store the card under supersession discipline, publish it.
///
/// One implementation serves both so a human's move in the TUI and an agent's
/// `task.move` are the *same* write — same validation, same ordinal defaulting,
/// same fan-out — differing only in the attribution each caller supplies.
#[derive(Clone)]
struct BoardOps {
    pool: SqlitePool,
    store: BlackboardStore,
    hub: BlackboardHub,
}

impl BoardOps {
    fn new(pool: SqlitePool, hub: BlackboardHub) -> Self {
        Self {
            pool,
            store: BlackboardStore::new(),
            hub,
        }
    }

    /// Resolve a board target to the durable run id its items hang off, creating
    /// the synthetic board run on the first *write* to a repository board (a read
    /// never creates one). Returns the run id and the `board_scope` to stamp on
    /// the stored card (`None` for a real workflow run's board).
    async fn resolve_for_write(
        &self,
        target: &BoardTarget,
    ) -> Result<(String, Option<String>), BlackboardError> {
        match target {
            BoardTarget::WorkflowRun(run) => Ok((run.clone(), None)),
            BoardTarget::Repository(repository) => {
                let board_id = board_scope_id(repository);
                WorkflowStore::new()
                    .ensure_board_run(&self.pool, &board_id, repository)
                    .await
                    .map_err(|error| BlackboardError::BoardUnavailable(error.to_string()))?;
                Ok((board_id, Some(repository.clone())))
            }
        }
    }

    /// Store a new card and fan it out. `card.ordinal` absent appends to the end
    /// of the target column, so a caller that only knows "put this in `todo`"
    /// never has to compute a position.
    async fn post_card(
        &self,
        target: &BoardTarget,
        card: NewCard,
    ) -> Result<BlackboardItemView, BlackboardError> {
        let (run_id, board_scope) = self.resolve_for_write(target).await?;
        let status = match card.status {
            Some(status) => Some(codypendent_workflow::normalize_status(&status)?),
            None if card.kind == BlackboardKind::Task => Some(DEFAULT_TASK_STATUS.to_string()),
            None => None,
        };
        let ordinal = match (card.ordinal, status.as_deref()) {
            (Some(ordinal), _) => Some(ordinal),
            (None, Some(status)) => {
                Some(self.store.next_ordinal(&self.pool, &run_id, status).await?)
            }
            (None, None) => None,
        };
        let item = self
            .store
            .post(
                &self.pool,
                &run_id,
                NewBlackboardItem {
                    kind: card.kind,
                    payload: card.payload,
                    author: card.author,
                    confidence: card.confidence,
                    evidence: card.evidence,
                    board: BoardFields {
                        board_scope,
                        status,
                        assignee: card.assignee,
                        ordinal,
                    },
                },
            )
            .await?;
        Ok(self.publish(&run_id, item))
    }

    /// Supersede a live card with a revised one — a column move, a re-assignment,
    /// a re-order, or a payload edit. Absent fields carry the stored card's values
    /// forward, and the store's fork-proof supersession is what actually applies
    /// the change, so board history is preserved and two concurrent moves cannot
    /// both win.
    async fn update_card(
        &self,
        target: &BoardTarget,
        item_id: &str,
        change: CardChange,
    ) -> Result<BlackboardItemView, BlackboardError> {
        let (run_id, _) = self.resolve_for_write(target).await?;
        let old = self
            .store
            .get(&self.pool, &run_id, item_id)
            .await?
            .ok_or_else(|| BlackboardError::NotFound(item_id.to_string()))?;

        let moved_column = change
            .status
            .as_ref()
            .is_some_and(|status| Some(status.as_str()) != old.board.status.as_deref());
        let status = match change.status {
            Some(status) => Some(codypendent_workflow::normalize_status(&status)?),
            None => old.board.status.clone(),
        };
        // A card that changed column with no explicit position lands at the END of
        // its new column rather than keeping a stale index from the old one.
        let ordinal = match (change.ordinal, moved_column, status.as_deref()) {
            (Some(ordinal), _, _) => Some(ordinal),
            (None, true, Some(status)) => {
                Some(self.store.next_ordinal(&self.pool, &run_id, status).await?)
            }
            (None, _, _) => old.board.ordinal,
        };
        let item = self
            .store
            .supersede(
                &self.pool,
                &run_id,
                item_id,
                NewBlackboardItem {
                    kind: old.kind,
                    payload: merge_card_payload(old.payload, change.payload),
                    author: change.author.unwrap_or(old.author),
                    confidence: old.confidence,
                    evidence: old.evidence,
                    board: BoardFields {
                        board_scope: old.board.board_scope,
                        status,
                        assignee: change.assignee.or(old.board.assignee),
                        ordinal,
                    },
                },
            )
            .await?;
        Ok(self.publish(&run_id, item))
    }

    /// Read a board's live cards. Never creates the synthetic run — an unwritten
    /// board is simply empty.
    async fn list_cards(
        &self,
        target: &BoardTarget,
        kind: Option<BlackboardKind>,
    ) -> Result<Vec<BlackboardItemView>, BlackboardError> {
        let run_id = match target {
            BoardTarget::WorkflowRun(run) => run.clone(),
            BoardTarget::Repository(repository) => board_scope_id(repository),
        };
        let items = self.store.query(&self.pool, &run_id, kind, false).await?;
        Ok(items
            .into_iter()
            .map(|item| item_to_view(&run_id, item))
            .collect())
    }

    /// Persist-before-publish: the store commit has already happened; fan the
    /// stored card out to the board's live subscribers (the board run id doubles
    /// as the hub key, so a TUI board pane converges with no new machinery).
    fn publish(&self, run_id: &str, item: BlackboardItem) -> BlackboardItemView {
        let view = item_to_view(run_id, item);
        self.hub.publish(run_id, view.clone());
        view
    }
}

/// Fold an update's partial card body onto the stored one.
///
/// `task.update` sends only the fields it is changing, so treating the incoming
/// payload as a whole replacement silently drops the rest: renaming a card would
/// delete its description. Both sides are objects for every card this crate
/// writes, so a supplied key wins and an omitted key is carried forward. A
/// non-object payload (or an absent one) keeps the previous replace/keep
/// behaviour, since there are no fields to merge.
fn merge_card_payload(
    old: serde_json::Value,
    change: Option<serde_json::Value>,
) -> serde_json::Value {
    match change {
        // A pure move/assign carries the stored body forward untouched.
        None => old,
        Some(serde_json::Value::Object(patch)) => match old {
            serde_json::Value::Object(mut base) => {
                base.extend(patch);
                serde_json::Value::Object(base)
            }
            _ => serde_json::Value::Object(patch),
        },
        Some(replacement) => replacement,
    }
}

/// An artifact to place on a board. Bundled rather than passed as nine
/// positional arguments so the two call sites (a client's post, an agent's
/// `task.create`) read as the card they are storing.
#[derive(Debug, Clone)]
struct NewCard {
    kind: BlackboardKind,
    payload: serde_json::Value,
    author: serde_json::Value,
    confidence: Option<f64>,
    evidence: Vec<serde_json::Value>,
    status: Option<String>,
    assignee: Option<String>,
    ordinal: Option<i64>,
}

/// The fields an update may replace; everything else is carried forward from the
/// superseded card.
#[derive(Debug, Clone, Default)]
struct CardChange {
    status: Option<String>,
    assignee: Option<String>,
    ordinal: Option<i64>,
    payload: Option<serde_json::Value>,
    /// Replacement attribution — the *editor's*, so a card's latest revision names
    /// whoever last touched it. `None` keeps the original author.
    author: Option<serde_json::Value>,
}

/// Map a store error to the wire error a rejected client board write carries.
fn map_write_error(error: BlackboardError) -> CodypendentError {
    let (code, retryable) = match &error {
        BlackboardError::EvidenceRequired(_) => ("blackboard.evidence-required", false),
        BlackboardError::NotFound(_) => ("blackboard.item-not-found", false),
        BlackboardError::AlreadySuperseded(_) => ("blackboard.already-superseded", false),
        BlackboardError::InvalidStatus(_) => ("blackboard.invalid-status", false),
        _ => ("blackboard.write-failed", true),
    };
    CodypendentError::new(code, error.to_string(), retryable)
}

/// Implements the daemon's [`BlackboardWriter`] — a `Controller` client's
/// `PostBlackboardItem` / `UpdateBlackboardItem` (Phase B kanban). Cheap to clone.
#[derive(Clone)]
pub struct AssemblyBoardWriter {
    ops: BoardOps,
}

impl AssemblyBoardWriter {
    /// Build the writer over the daemon's pool and the board fan-out hub.
    #[must_use]
    pub fn new(pool: SqlitePool, hub: BlackboardHub) -> Self {
        Self {
            ops: BoardOps::new(pool, hub),
        }
    }
}

/// Attribution for a card written by a human operator over the socket. Built
/// **server-side** from the connection's client id — a client never supplies its
/// own identity, exactly as an agent's author is built from its run context.
fn operator_author(client_id: codypendent_protocol::ClientId) -> serde_json::Value {
    serde_json::json!({ "role": "operator", "client_id": client_id.to_string() })
}

impl BlackboardWriter for AssemblyBoardWriter {
    fn post(&self, request: PostBlackboardRequest) -> BlackboardWriteFuture<'_> {
        let ops = self.ops.clone();
        Box::pin(async move {
            let kind = BlackboardKind::parse_kind(&request.item.kind).ok_or_else(|| {
                CodypendentError::new(
                    "workflow.unknown-blackboard-kind",
                    format!(
                        "`{}` is not a known blackboard artifact kind",
                        request.item.kind
                    ),
                    false,
                )
            })?;
            ops.post_card(
                &request.target,
                NewCard {
                    kind,
                    payload: request.item.payload,
                    author: operator_author(request.client_id),
                    confidence: request.item.confidence,
                    evidence: request.item.evidence,
                    status: request.item.status,
                    assignee: request.item.assignee,
                    ordinal: request.item.ordinal,
                },
            )
            .await
            .map_err(map_write_error)
        })
    }

    fn update(&self, request: UpdateBlackboardRequest) -> BlackboardWriteFuture<'_> {
        let ops = self.ops.clone();
        Box::pin(async move {
            ops.update_card(
                &request.target,
                &request.item_id,
                CardChange {
                    status: request.status,
                    assignee: request.assignee,
                    ordinal: request.ordinal,
                    payload: request.payload,
                    author: Some(operator_author(request.client_id)),
                },
            )
            .await
            .map_err(map_write_error)
        })
    }
}

/// Implements the runtime's [`TaskBoardChannel`] — the `task.create` /
/// `task.update` / `task.move` / `task.list` tools an agent turns a feature
/// request into backlog cards with (rubric 10). It shares [`BoardOps`] with the
/// client writer above, so an agent-created card is indistinguishable in the store
/// from a human-created one apart from its attribution. Cheap to clone.
#[derive(Clone)]
pub struct AssemblyTaskBoardChannel {
    ops: BoardOps,
}

impl AssemblyTaskBoardChannel {
    /// Build the channel over the daemon's pool and the board fan-out hub.
    #[must_use]
    pub fn new(pool: SqlitePool, hub: BlackboardHub) -> Self {
        Self {
            ops: BoardOps::new(pool, hub),
        }
    }
}

#[async_trait]
impl TaskBoardChannel for AssemblyTaskBoardChannel {
    async fn create(
        &self,
        repository: &str,
        draft: TaskCardDraft,
    ) -> Result<BlackboardItemView, BlackboardChannelError> {
        self.ops
            .post_card(
                &BoardTarget::Repository(repository.to_string()),
                NewCard {
                    kind: BlackboardKind::Task,
                    payload: draft.payload,
                    author: draft.author,
                    confidence: None,
                    // A card is a plan, not a claim about the codebase, so it
                    // needs no evidence — `Task` is evidence-optional.
                    evidence: Vec::new(),
                    status: draft.status,
                    assignee: draft.assignee,
                    ordinal: draft.ordinal,
                },
            )
            .await
            .map_err(map_channel_error)
    }

    async fn update(
        &self,
        repository: &str,
        item_id: &str,
        change: TaskCardChange,
    ) -> Result<BlackboardItemView, BlackboardChannelError> {
        self.ops
            .update_card(
                &BoardTarget::Repository(repository.to_string()),
                item_id,
                CardChange {
                    status: change.status,
                    assignee: change.assignee,
                    ordinal: change.ordinal,
                    payload: change.payload,
                    author: Some(change.author),
                },
            )
            .await
            .map_err(map_channel_error)
    }

    async fn list(
        &self,
        repository: &str,
    ) -> Result<Vec<BlackboardItemView>, BlackboardChannelError> {
        self.ops
            .list_cards(
                &BoardTarget::Repository(repository.to_string()),
                Some(BlackboardKind::Task),
            )
            .await
            .map_err(map_channel_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::ClientId;
    use serde_json::json;

    /// A `task.update` that changes only the title must not blank the
    /// description (and vice versa): the tool sends just the fields it is
    /// changing, so the stored body is folded, not replaced.
    #[test]
    fn a_partial_card_update_keeps_the_fields_it_did_not_send() {
        let stored = json!({ "title": "flaky test", "description": "fails on CI only" });

        let renamed = merge_card_payload(stored.clone(), Some(json!({ "title": "flaky suite" })));
        assert_eq!(
            renamed,
            json!({ "title": "flaky suite", "description": "fails on CI only" }),
            "renaming a card must keep its description"
        );

        let redescribed =
            merge_card_payload(stored.clone(), Some(json!({ "description": "now green" })));
        assert_eq!(
            redescribed,
            json!({ "title": "flaky test", "description": "now green" }),
            "editing a description must keep the title"
        );

        // A pure move/assign sends no payload at all and carries the body over.
        assert_eq!(merge_card_payload(stored.clone(), None), stored);
    }

    /// A migrated pool on a tempfile (WAL needs a real file, not `:memory:`); the
    /// shared migrations create `blackboard_items` (0010). The returned `TempDir`
    /// must be kept alive for the pool's lifetime.
    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = codypendent_daemon::db::open_database(&dir.path().join("codypendent.db"))
            .await
            .expect("migrated pool");
        (dir, pool)
    }

    /// Seed a minimal `workflow_runs` row — `blackboard_items.workflow_run_id`
    /// references it (FK), so a post/query needs the run to exist.
    async fn seed_run(pool: &SqlitePool, id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO workflow_runs \
             (id, workflow_id, workflow_version, graph_signature, inputs_json, state, \
              created_at, updated_at) \
             VALUES (?, 'wf', 1, 'sig', 'null', 'running', ?, ?)",
        )
        .bind(id)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .expect("seed workflow run");
    }

    fn finding(with_evidence: bool) -> BlackboardPost {
        BlackboardPost {
            kind: "finding".to_string(),
            payload: json!({ "summary": "the parser drops trailing commas" }),
            author: json!({ "role": "investigator", "node_id": "diagnose" }),
            confidence: Some(0.8),
            evidence: if with_evidence {
                vec![json!({ "path": "src/parse.rs", "line": 42 })]
            } else {
                Vec::new()
            },
            supersedes: None,
        }
    }

    #[tokio::test]
    async fn post_lands_and_fans_out_to_subscribers() {
        let (_dir, pool) = temp_pool().await;
        let hub = BlackboardHub::new();
        let channel = AssemblyBlackboardChannel::new(pool.clone(), hub.clone());
        let run = "wfrun-post";
        seed_run(&pool, run).await;
        let mut rx = hub.subscribe(run);

        let posted = channel.post(run, finding(true)).await.expect("posts");
        assert_eq!(posted.kind, "finding");
        assert_eq!(posted.revision, 1);
        // The author is exactly what the runtime built (server-side).
        assert_eq!(posted.author["node_id"], "diagnose");

        // The subscriber receives the same artifact.
        let delivered = rx.recv().await.expect("delivered");
        assert_eq!(delivered.id, posted.id);
        assert_eq!(delivered.workflow_run_id, run);

        // And it is queryable on the live board.
        let live = channel
            .query(run, Some("finding".to_string()), false)
            .await
            .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, posted.id);
    }

    #[tokio::test]
    async fn evidence_required_refusal_is_structured_and_correctable() {
        let (_dir, pool) = temp_pool().await;
        let channel = AssemblyBlackboardChannel::new(pool.clone(), BlackboardHub::new());
        let run = "wfrun-evidence";
        seed_run(&pool, run).await;

        // A finding is claim-like: without evidence it is refused with the
        // correctable, structured error (not a backend error).
        let err = channel.post(run, finding(false)).await.unwrap_err();
        assert_eq!(err.code(), "blackboard.evidence-required");

        // Re-posting the same finding *with* evidence lands.
        let ok = channel
            .post(run, finding(true))
            .await
            .expect("second post lands");
        assert_eq!(ok.kind, "finding");
    }

    #[tokio::test]
    async fn supersede_publishes_the_new_revision() {
        let (_dir, pool) = temp_pool().await;
        let hub = BlackboardHub::new();
        let channel = AssemblyBlackboardChannel::new(pool.clone(), hub.clone());
        let run = "wfrun-supersede";
        seed_run(&pool, run).await;
        let mut rx = hub.subscribe(run);

        let first = channel.post(run, finding(true)).await.expect("first");
        let _ = rx.recv().await.expect("first delivered");

        let mut correction = finding(true);
        correction.supersedes = Some(first.id.clone());
        correction.payload = json!({ "summary": "corrected: it is only in nested arrays" });
        let second = channel.post(run, correction).await.expect("supersede");
        assert_eq!(second.revision, 2);

        let delivered = rx.recv().await.expect("supersession delivered");
        assert_eq!(delivered.id, second.id);
        assert_eq!(delivered.revision, 2);

        // The live board now shows only the correction.
        let live = channel.query(run, None, false).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, second.id);
    }

    #[tokio::test]
    async fn reader_projects_the_board_and_rejects_an_unknown_kind() {
        let (_dir, pool) = temp_pool().await;
        let channel = AssemblyBlackboardChannel::new(pool.clone(), BlackboardHub::new());
        let run = "wfrun-read";
        seed_run(&pool, run).await;
        channel.post(run, finding(true)).await.expect("seed");

        let reader = WorkflowBlackboardReader::new(pool);
        let items = reader
            .read(ReadBlackboardRequest {
                workflow_run_id: run.to_string(),
                board_repository: None,
                kind: None,
                include_superseded: false,
                client_id: ClientId::new(),
            })
            .await
            .expect("read");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "finding");

        // A typo'd kind filter is a legible rejection, not a silent empty board.
        let err = reader
            .read(ReadBlackboardRequest {
                workflow_run_id: run.to_string(),
                board_repository: None,
                kind: Some("test-result".to_string()),
                include_superseded: false,
                client_id: ClientId::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, "workflow.unknown-blackboard-kind");
    }

    // -----------------------------------------------------------------------
    // The repository task board (rubric 10)
    // -----------------------------------------------------------------------

    fn card(title: &str, status: Option<&str>) -> codypendent_protocol::BlackboardItemDraft {
        codypendent_protocol::BlackboardItemDraft {
            kind: "task".to_string(),
            payload: json!({ "title": title }),
            confidence: None,
            evidence: Vec::new(),
            status: status.map(str::to_string),
            assignee: None,
            ordinal: None,
        }
    }

    #[tokio::test]
    async fn a_repository_board_is_created_on_first_write_and_read_back() {
        // The board's synthetic run does not exist until something is written —
        // and a READ never creates it, so an untouched repository simply reads
        // empty rather than littering `workflow_runs`.
        let (_dir, pool) = temp_pool().await;
        let hub = BlackboardHub::new();
        let repository = "/home/user/project";
        let reader = WorkflowBlackboardReader::new(pool.clone());
        let empty = reader
            .read(ReadBlackboardRequest {
                workflow_run_id: String::new(),
                board_repository: Some(repository.to_string()),
                kind: None,
                include_superseded: false,
                client_id: ClientId::new(),
            })
            .await
            .expect("an unwritten board reads empty");
        assert!(empty.is_empty());
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "a read must not create the board run");

        let writer = AssemblyBoardWriter::new(pool.clone(), hub.clone());
        let mut live = hub.subscribe(board_scope_id(repository));
        let posted = writer
            .post(PostBlackboardRequest {
                target: BoardTarget::Repository(repository.to_string()),
                item: card("wire the DAG viewer", None),
                client_id: ClientId::new(),
            })
            .await
            .expect("post");
        // A `task` with no column lands in `todo`, at the end of that column,
        // stamped with the board it serves.
        assert_eq!(posted.status.as_deref(), Some("todo"));
        assert_eq!(posted.ordinal, Some(0));
        assert_eq!(posted.board_scope.as_deref(), Some(repository));
        assert_eq!(posted.workflow_run_id, board_scope_id(repository));
        // The author is built server-side — the client supplied none.
        assert_eq!(posted.author["role"], "operator");
        // …and the write fans out on the board's channel, which is what lets the
        // kanban pane converge without a re-read.
        assert_eq!(live.recv().await.expect("delivered").id, posted.id);

        let items = reader
            .read(ReadBlackboardRequest {
                workflow_run_id: String::new(),
                board_repository: Some(repository.to_string()),
                kind: Some("task".to_string()),
                include_superseded: false,
                client_id: ClientId::new(),
            })
            .await
            .expect("read the board");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, posted.id);
    }

    #[tokio::test]
    async fn a_move_supersedes_the_card_and_appends_it_to_the_target_column() {
        let (_dir, pool) = temp_pool().await;
        let repository = "/home/user/project";
        let writer = AssemblyBoardWriter::new(pool.clone(), BlackboardHub::new());
        let post = |draft| {
            let writer = writer.clone();
            async move {
                writer
                    .post(PostBlackboardRequest {
                        target: BoardTarget::Repository(repository.to_string()),
                        item: draft,
                        client_id: ClientId::new(),
                    })
                    .await
                    .expect("post")
            }
        };
        // Two cards already in `doing`, so the move must land AFTER them.
        post(card("first", Some("doing"))).await;
        post(card("second", Some("doing"))).await;
        let moving = post(card("third", Some("todo"))).await;

        let moved = writer
            .update(UpdateBlackboardRequest {
                target: BoardTarget::Repository(repository.to_string()),
                item_id: moving.id.clone(),
                status: Some("doing".to_string()),
                assignee: None,
                ordinal: None,
                payload: None,
                client_id: ClientId::new(),
            })
            .await
            .expect("move");
        assert_eq!(moved.status.as_deref(), Some("doing"));
        assert_eq!(moved.revision, 2, "a move is a supersession, not an edit");
        assert_ne!(moved.id, moving.id, "the replacement is a new row");
        assert_eq!(
            moved.ordinal,
            Some(2),
            "a moved card appends to its new column rather than keeping a stale index"
        );
        // The card's body survives a pure move — nothing named a payload.
        assert_eq!(moved.payload["title"], "third");

        // Moving the SAME (now superseded) card again is refused rather than
        // forking the chain.
        let err = writer
            .update(UpdateBlackboardRequest {
                target: BoardTarget::Repository(repository.to_string()),
                item_id: moving.id,
                status: Some("done".to_string()),
                assignee: None,
                ordinal: None,
                payload: None,
                client_id: ClientId::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, "blackboard.already-superseded");
    }

    #[tokio::test]
    async fn an_invalid_column_is_refused_legibly() {
        let (_dir, pool) = temp_pool().await;
        let writer = AssemblyBoardWriter::new(pool, BlackboardHub::new());
        let err = writer
            .post(PostBlackboardRequest {
                target: BoardTarget::Repository("/home/user/project".to_string()),
                item: card("bad column", Some("   ")),
                client_id: ClientId::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, "blackboard.invalid-status");
    }

    #[tokio::test]
    async fn an_agent_card_and_an_operator_card_share_one_board() {
        // The point of sharing `BoardOps`: a card the model creates through
        // `task.create` is the same durable row as one a human posts, differing
        // only in attribution — so the TUI shows both and either can move either.
        let (_dir, pool) = temp_pool().await;
        let hub = BlackboardHub::new();
        let repository = "/home/user/project";
        let agent = AssemblyTaskBoardChannel::new(pool.clone(), hub.clone());
        let human = AssemblyBoardWriter::new(pool.clone(), hub);

        let by_agent = agent
            .create(
                repository,
                codypendent_runtime::blackboard::TaskCardDraft {
                    payload: json!({ "title": "split the parser" }),
                    author: json!({ "role": "agent", "run_id": "r1" }),
                    status: None,
                    assignee: None,
                    ordinal: None,
                },
            )
            .await
            .expect("agent card");
        human
            .post(PostBlackboardRequest {
                target: BoardTarget::Repository(repository.to_string()),
                item: card("write the spec", Some("review")),
                client_id: ClientId::new(),
            })
            .await
            .expect("operator card");

        let board = agent.list(repository).await.expect("list");
        assert_eq!(board.len(), 2);
        assert!(board.iter().any(|c| c.author["role"] == "agent"));
        assert!(board.iter().any(|c| c.author["role"] == "operator"));

        // And a human can move the agent's card: one board, one write path.
        let moved = human
            .update(UpdateBlackboardRequest {
                target: BoardTarget::Repository(repository.to_string()),
                item_id: by_agent.id,
                status: Some("done".to_string()),
                assignee: Some("dana".to_string()),
                ordinal: None,
                payload: None,
                client_id: ClientId::new(),
            })
            .await
            .expect("human moves the agent's card");
        assert_eq!(moved.status.as_deref(), Some("done"));
        assert_eq!(moved.assignee.as_deref(), Some("dana"));
    }
}

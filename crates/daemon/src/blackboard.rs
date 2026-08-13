//! Blackboard transport: per-run artifact fan-out + the board-read seam
//! (Phase 5 STEP 5.3 client surface).
//!
//! Two pieces live here, both daemon-owned but neither depending on the workflow
//! crate (the daemon must not — `codypendent-workflow` owns the authoritative
//! `BlackboardStore`, and depending on it would invert the layering the executor
//! seam exists to avoid):
//!
//! * [`BlackboardHub`] — a per-*workflow-run* [`tokio::sync::broadcast`] fan-out,
//!   mirroring [`crate::documents::DocumentHub`] but keyed by the durable
//!   workflow-run id and carrying a [`BlackboardItemView`]. The workflow executor
//!   publishes each posted (or superseded) artifact here as it lands, and the
//!   server subscribes a client's `Subscription::Blackboard` forwarder to it. It is
//!   **not** a source of truth — the workflow crate's `BlackboardStore` is; a
//!   missed delivery is harmless, because a subscriber's baseline comes from the
//!   read command and each item is merged idempotently by id.
//!
//! * [`BlackboardReader`] — the dependency-inversion seam for *reading* a run's
//!   board. The daemon declares what it needs (project a run's board, kind-filtered,
//!   into [`BlackboardItemView`]s); the `codypendentd` assembly implements it over
//!   `codypendent-workflow`'s `BlackboardStore` on the daemon's pool, exactly as it
//!   implements [`WorkflowStarter`](crate::workflows::WorkflowStarter) and
//!   [`DocumentMutator`](crate::documents::DocumentMutator). Like the document
//!   seams it is **request/reply**: the server awaits the projected items so it can
//!   reply `BlackboardItems`, so the method returns a boxed future (no `async-trait`
//!   dependency). The default-`None`
//!   [`RunExecutor::blackboard_reader`](crate::executor::RunExecutor::blackboard_reader)
//!   leaves it unwired — the lib-only / test server then rejects `ReadBlackboard`
//!   with `workflow.transport-unavailable`, exactly as `StartWorkflow` is without a
//!   starter.
//!
//! * [`BlackboardWriter`] — the mirror-image seam for *writing* a board from a
//!   client (Phase B kanban). Originally there was deliberately no post seam here
//!   (only the workflow executor wrote the board). The repository **task board**
//!   needs one: a human moves a card in the TUI, and no agent run is involved. It
//!   is gated at the server to the `Controller` role, so an Observer stays
//!   read-only and an agent still writes only through its `blackboard.*`/`task.*`
//!   tools.
//!
//! A **repository task board** rides the same three pieces unchanged: it is stored
//! as a synthetic workflow run whose id is `codypendent_protocol::board_scope_id`
//! (`board:<canonical repo>`), so the hub keys it like any run (`board:<repo>`
//! feeds), the reader projects it like any run's board, and a client subscribes to
//! it with an ordinary `Subscription::Blackboard { workflow_run_id: board_id }`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use codypendent_protocol::{BlackboardItemView, ClientId, CodypendentError};
use tokio::sync::broadcast;

/// Per-run channel depth. A run's board advances far slower than a session's
/// event stream (a handful of typed artifacts per node, not a streaming token
/// feed), so a modest buffer bounds memory; a receiver that still falls behind is
/// signalled `Lagged` and simply re-reads the board (idempotent merge by id),
/// never stalling the publisher.
const CHANNEL_CAPACITY: usize = 256;

/// An in-memory, per-workflow-run blackboard fan-out shared by every clone (an
/// [`Arc`]), so the executor's post path (publisher) and each `Blackboard`
/// forwarder (subscriber) see the same channels.
#[derive(Debug, Clone, Default)]
pub struct BlackboardHub {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<BlackboardItemView>>>>,
}

impl BlackboardHub {
    /// An empty hub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to a run's live board stream, creating the channel lazily if this
    /// is the first subscriber. The returned receiver observes only items published
    /// *after* this call — a subscriber gets its baseline from the read command,
    /// then converges via the live stream (merges are idempotent by id, so a small
    /// overlap or gap self-heals).
    pub fn subscribe(
        &self,
        workflow_run_id: impl Into<String>,
    ) -> broadcast::Receiver<BlackboardItemView> {
        self.lock()
            .entry(workflow_run_id.into())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Publish one posted (or superseded) artifact to a run's subscribers.
    /// Best-effort: no channel (no subscribers ever) or all receivers dropped
    /// discards it silently — the workflow `BlackboardStore` remains the durable
    /// record.
    pub fn publish(&self, workflow_run_id: &str, item: BlackboardItemView) {
        if let Some(sender) = self.lock().get(workflow_run_id) {
            let _ = sender.send(item);
        }
    }

    /// Number of runs with a live channel (subscribed at least once).
    #[must_use]
    pub fn run_count(&self) -> usize {
        self.lock().len()
    }

    /// Drop channels whose last receiver has detached, so a long-lived daemon's hub
    /// does not retain one channel per workflow run ever subscribed.
    pub fn prune_idle(&self) {
        self.lock().retain(|_, sender| sender.receiver_count() > 0);
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, broadcast::Sender<BlackboardItemView>>> {
        // Held only for map lookups/inserts, never across an await, so poisoning
        // indicates a bug elsewhere; surface it loudly.
        self.channels.lock().expect("blackboard hub mutex poisoned")
    }
}

/// A client's request to read a workflow run's blackboard.
#[derive(Debug, Clone)]
pub struct ReadBlackboardRequest {
    /// The durable workflow-run id whose board to read. Ignored when
    /// [`board_repository`](Self::board_repository) is set — the assembly then
    /// resolves the synthetic board run instead.
    pub workflow_run_id: String,
    /// Read a **repository task board** (kanban) rather than a workflow run's
    /// board. The canonical repository root; the assembly resolves it to the
    /// synthetic board run `codypendent_protocol::board_scope_id` names. A board
    /// never written to reads empty (a read creates nothing).
    pub board_repository: Option<String>,
    /// A blackboard artifact kind to filter by, or all kinds when `None`.
    pub kind: Option<String>,
    /// Include superseded revisions too; `false` returns only the live board.
    pub include_superseded: bool,
    /// The identity of the reading client (for attribution / audit).
    pub client_id: ClientId,
}

/// Which durable board a client write targets — the daemon-side mirror of the
/// wire [`BlackboardScope`](codypendent_protocol::BlackboardScope), with the
/// `Unknown` (a newer client's scope) already rejected at the server edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardTarget {
    /// A durable workflow run's board.
    WorkflowRun(String),
    /// A repository's task board — the assembly creates the synthetic board run
    /// on first write.
    Repository(String),
}

/// A client's request to post a new artifact onto a board (`PostBlackboardItem`).
///
/// The `author` is **not** here: the assembly builds it from
/// [`client_id`](Self::client_id) and the role the daemon recorded at attach,
/// exactly as the workflow executor builds an agent's author — a client never
/// supplies its own attribution.
#[derive(Debug, Clone)]
pub struct PostBlackboardRequest {
    /// The board to write.
    pub target: BoardTarget,
    /// The artifact to store (kind, payload, evidence, board fields).
    pub item: codypendent_protocol::BlackboardItemDraft,
    /// The identity of the writing client (server-built attribution).
    pub client_id: ClientId,
}

/// A client's request to supersede an existing board item
/// (`UpdateBlackboardItem`): a column move, a re-assignment, a re-order, or a
/// payload edit. Absent fields carry the stored item's values forward.
#[derive(Debug, Clone)]
pub struct UpdateBlackboardRequest {
    /// The board holding the item.
    pub target: BoardTarget,
    /// The live item to supersede.
    pub item_id: String,
    /// The new column, when moving.
    pub status: Option<String>,
    /// The new assignee, when re-assigning.
    pub assignee: Option<String>,
    /// The new within-column position; absent appends to the target column.
    pub ordinal: Option<i64>,
    /// A replacement payload, when editing the card body.
    pub payload: Option<serde_json::Value>,
    /// The identity of the writing client (server-built attribution).
    pub client_id: ClientId,
}

/// The future a [`BlackboardWriter`] returns: the stored (or superseding) item to
/// reply with, or a structured [`CodypendentError`] the server rejects with.
pub type BlackboardWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BlackboardItemView, CodypendentError>> + Send + 'a>>;

/// The daemon's seam for *writing* a board from an accepted `PostBlackboardItem`
/// / `UpdateBlackboardItem` (Phase B kanban).
///
/// The mirror of [`BlackboardReader`]: the daemon declares what it needs, the
/// `codypendentd` assembly implements it over `codypendent-workflow`'s
/// `BlackboardStore` on the daemon's pool (creating the synthetic board run on
/// first repository-board write) and publishes the stored item to the
/// [`BlackboardHub`] so live subscribers converge. The default-`None`
/// [`RunExecutor::blackboard_writer`](crate::executor::RunExecutor::blackboard_writer)
/// leaves it unwired — the lib-only / test server then rejects both commands with
/// `workflow.transport-unavailable`.
///
/// The server gates both commands on the
/// [`Controller`](codypendent_protocol::ClientRole::Controller) role *before*
/// reaching this seam, so an implementation never sees an Observer's write.
pub trait BlackboardWriter: Send + Sync {
    /// Store a new artifact on `request`'s board, returning its projected view.
    fn post(&self, request: PostBlackboardRequest) -> BlackboardWriteFuture<'_>;

    /// Supersede `request`'s item with a revised one, returning the replacement.
    fn update(&self, request: UpdateBlackboardRequest) -> BlackboardWriteFuture<'_>;
}

/// The future a [`BlackboardReader`] returns: the projected board items to reply
/// with, or a structured [`CodypendentError`] the server rejects with. Boxed so
/// the trait stays object-safe without an `async-trait` dependency (matching the
/// [`DocumentMutator`](crate::documents::DocumentMutator) seam).
pub type BlackboardReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<BlackboardItemView>, CodypendentError>> + Send + 'a>>;

/// A request for one board item's full revision lineage
/// (`codypendent_workflow::BlackboardStore::history`): every superseded
/// ancestor and the live head, oldest first. Mirrors [`ReadBlackboardRequest`]'s
/// two ways to name a board (a durable run, or a repository's synthetic board)
/// but names one item within it rather than the whole board.
///
/// Not wired to a wire command yet (see [`BlackboardReader::history`]'s docs) —
/// this is the daemon-side seam a `ReadBlackboardHistory` command (or an
/// `item_id` added to `ReadBlackboard`) would call once one exists.
#[derive(Debug, Clone)]
pub struct BlackboardHistoryRequest {
    /// The durable workflow-run id whose item to trace. Ignored when
    /// [`board_repository`](Self::board_repository) is set.
    pub workflow_run_id: String,
    /// Trace an item on a **repository task board** rather than a workflow
    /// run's board, exactly as [`ReadBlackboardRequest::board_repository`].
    pub board_repository: Option<String>,
    /// The item whose lineage to walk (any revision's id resolves the whole
    /// chain, not only the live head's).
    pub item_id: String,
    /// The identity of the reading client (for attribution / audit).
    pub client_id: ClientId,
}

/// The daemon's seam for *reading* a durable run's blackboard from an accepted
/// `ReadBlackboard` command.
///
/// Implemented by the assembly over `codypendent-workflow`'s `BlackboardStore`
/// (`query`, kind-filtered) on the daemon's pool, and injected alongside the
/// [`RunExecutor`](crate::executor::RunExecutor). The default-`None`
/// [`RunExecutor::blackboard_reader`](crate::executor::RunExecutor::blackboard_reader)
/// leaves it unwired — the lib-only / test server then rejects `ReadBlackboard`
/// with `workflow.transport-unavailable`.
pub trait BlackboardReader: Send + Sync {
    /// Project `request`'s run board (kind-filtered) into
    /// [`BlackboardItemView`]s. A store failure is surfaced verbatim to the client
    /// as a `CommandRejected`; an unknown run yields an empty board (its own board
    /// is simply empty), never an error.
    fn read(&self, request: ReadBlackboardRequest) -> BlackboardReadFuture<'_>;

    /// Project one item's full supersession lineage, oldest revision first —
    /// the read half of `codypendent_workflow::BlackboardStore::history`, which
    /// every board move (a column drag, a `task.move`) already feeds and which
    /// had no caller anywhere in the product before this method existed. A
    /// board move is a supersession, so the audit trail ("who moved this, and
    /// when, and what did it say before") already lives durably in the store —
    /// this is the seam that lets a client actually ask for it.
    ///
    /// Returns the SAME [`BlackboardReadFuture`] shape as
    /// [`read`](Self::read): reuses [`BlackboardItemView`] for each revision
    /// rather than inventing a parallel type, since a history entry IS a
    /// stored item, just not the live one. An id absent from the named board
    /// yields an empty vector (mirrors `BlackboardStore::history`'s own
    /// "empty ⇒ not found" contract), never an error.
    fn history(&self, request: BlackboardHistoryRequest) -> BlackboardReadFuture<'_>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(workflow_run_id: &str, id: &str, kind: &str) -> BlackboardItemView {
        BlackboardItemView {
            id: id.to_string(),
            workflow_run_id: workflow_run_id.to_string(),
            kind: kind.to_string(),
            payload: json!({ "note": id }),
            author: json!({ "node_id": "n1" }),
            confidence: None,
            evidence: Vec::new(),
            revision: 1,
            superseded_by: None,
            board_scope: None,
            status: None,
            assignee: None,
            ordinal: None,
        }
    }

    #[tokio::test]
    async fn subscriber_receives_published_items_in_order() {
        let hub = BlackboardHub::new();
        let run = "wfrun-1";
        let mut rx = hub.subscribe(run);

        hub.publish(run, item(run, "a", "finding"));
        hub.publish(run, item(run, "b", "decision"));

        assert_eq!(rx.recv().await.unwrap().id, "a");
        assert_eq!(rx.recv().await.unwrap().id, "b");
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_a_silent_noop() {
        let hub = BlackboardHub::new();
        hub.publish("wfrun-void", item("wfrun-void", "a", "finding"));
        assert_eq!(hub.run_count(), 0);
    }

    #[tokio::test]
    async fn channels_are_isolated_per_run() {
        let hub = BlackboardHub::new();
        let mut rx_a = hub.subscribe("wfrun-a");
        let _rx_b = hub.subscribe("wfrun-b");

        hub.publish("wfrun-a", item("wfrun-a", "x", "finding"));
        assert_eq!(rx_a.recv().await.unwrap().id, "x");
        assert_eq!(hub.run_count(), 2);
    }

    #[test]
    fn prune_drops_channels_whose_receivers_detached() {
        let hub = BlackboardHub::new();
        {
            let _rx = hub.subscribe("wfrun-1");
            assert_eq!(hub.run_count(), 1);
        }
        hub.prune_idle();
        assert_eq!(hub.run_count(), 0);
    }
}

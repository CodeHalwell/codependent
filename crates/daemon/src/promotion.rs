//! Promotion seam (dependency inversion), Phase 7 STEP 7.5.
//!
//! `ProposePromotion`/`AdvancePromotion`/`ApprovePromotion`/`RollbackPromotion`
//! commands create and drive a candidate that lives in its own durable store
//! *outside* the session ledger — so, like `StartWorkflow`, they are
//! intercepted at the connection level and applied through a seam the daemon
//! declares and the `codypendentd` assembly fills (only the assembly can name
//! `codypendent-eval` and reach the pool). The default-`None`
//! [`ServerState::promotion`](crate::server::ServerState::promotion) leaves it
//! unwired — the lib-only / test server then rejects every promotion command
//! with `promotion.transport-unavailable`, exactly as an executor-less run
//! stays `Queued` and a mutator-less server rejects `MutateDocument`.
//!
//! # The human-approval gate lives HERE, not in the seam implementation
//!
//! [`ApprovePromotionRequest::approver`] is constructed by `crates/daemon/src/server.rs`
//! from the *connection's authenticated role* — never from a client-supplied
//! wire field (`CommandBody::ApprovePromotion` carries only a `candidate_id`;
//! there is no way for a caller to submit an `Actor` at all). Over this
//! local-first socket, a `Controller`-role connection **is** the human
//! operator (the same mapping `crate::commands::apply_resolve_approval` already
//! uses to attribute `resolved_by`), so the server maps `Controller` →
//! `Actor::Human { user_id: UserId(client_id) }` and refuses every other role
//! before the seam is ever called — an agent or system actor can never reach
//! this path (the daemon has no notion of "connect as an agent"; `Actor::Agent`
//! is only ever constructed by the runtime executor attributing its OWN
//! actions, never by a socket command). [`codypendent_eval::promote::Candidate::approve`]
//! then enforces the invariant a second, structural time regardless of what
//! the daemon does — ADR-010's belt and suspenders.

use std::future::Future;
use std::pin::Pin;

use codypendent_protocol::{Actor, ClientId, CodypendentError, PromotionAction};

/// A client's request to draft a new promotion candidate.
#[derive(Debug, Clone)]
pub struct ProposePromotionRequest {
    /// The wire name of an `ArtifactKind` (e.g. `"skill"`, `"router"`). The
    /// seam implementation parses this; an unrecognized kind is rejected
    /// rather than guessed at.
    pub kind: String,
    pub name: String,
    pub version: u32,
    pub requires_permission_review: bool,
    /// The command's idempotency key: a duplicate `ProposePromotion` delivery
    /// (a client retrying after a lost acknowledgement) carries the same key,
    /// so the seam drafts the candidate idempotently — the same key resolves
    /// to the same candidate rather than a second one.
    pub idempotency_key: String,
    /// The identity of the proposing client, for attribution (a draft's
    /// author need not be human — see `Candidate::draft`).
    pub client_id: ClientId,
}

/// A client's request to advance a candidate through regression/shadow/canary.
#[derive(Debug, Clone)]
pub struct AdvancePromotionRequest {
    pub candidate_id: String,
    pub action: PromotionAction,
    pub client_id: ClientId,
}

/// A client's submission of the eval evidence a candidate's regression gate
/// will consume.
///
/// This exists because the alternative was worse: `codypendent eval run
/// --candidate-id` used to open the daemon's own SQLite file and `INSERT` the
/// `eval_suite_reports` row itself, so the "durable evidence" the gate later
/// read had never crossed an authenticated boundary — anyone able to write that
/// file could hand-author an all-passing report and clear the gate for any
/// candidate. Routing it through the socket makes the daemon the only writer of
/// its own evidence table, and lets it re-derive the candidate's artifact
/// identity rather than accept the caller's.
///
/// It does NOT make the daemon the *producer* of the measurements — the cases
/// still execute in the client. What it removes is the unauthenticated write
/// path; see the implementation for the checks that survive.
#[derive(Debug, Clone)]
pub struct SubmitEvalEvidenceRequest {
    pub candidate_id: String,
    /// The suite that ran; the regression gate consumes `core`.
    pub suite: String,
    /// The routing policy the suite ran under, or `daemon-default`.
    pub routing_policy: String,
    /// A serialized `codypendent_eval::SuiteReport`. Parsed and validated by
    /// the implementation — never stored verbatim on trust.
    pub report_json: String,
    pub client_id: ClientId,
}

/// A client's request to approve (and thereby promote) a candidate. `approver`
/// is constructed by `server.rs` from the connection's role — see the module
/// doc — never taken from the wire.
#[derive(Debug, Clone)]
pub struct ApprovePromotionRequest {
    pub candidate_id: String,
    pub approver: Actor,
    pub client_id: ClientId,
}

/// A client's request to manually roll back a promoted candidate. `actor` is
/// likewise constructed by `server.rs` from the connection's role.
#[derive(Debug, Clone)]
pub struct RollbackPromotionRequest {
    pub candidate_id: String,
    pub actor: Actor,
    pub client_id: ClientId,
}

/// The future [`PromotionGateway::propose`] returns: the new candidate id, or
/// a structured [`CodypendentError`] the server rejects with. Boxed so the
/// trait stays object-safe without an `async-trait` dependency (matching the
/// [`WorkflowStarter`](crate::workflows::WorkflowStarter) seam).
pub type PromotionProposeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<String, CodypendentError>> + Send + 'a>>;

/// The future [`PromotionGateway`]'s other methods return: the synchronous
/// outcome, or a structured [`CodypendentError`]. Matches
/// [`PromotionProposeFuture`].
pub type PromotionActionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), CodypendentError>> + Send + 'a>>;

/// The daemon's seam for the promotion pipeline (Phase 7 STEP 7.5).
///
/// Implemented by the assembly over `codypendent-eval::PromotionStore` on the
/// daemon's pool, and injected into [`crate::server::ServerState::promotion`].
/// Every method surfaces the underlying state-machine error verbatim as a
/// `CommandRejected` (an illegal transition, a non-human approver, an
/// unobserved canary trying to finish) — nothing here ever coerces a refusal
/// into a success.
pub trait PromotionGateway: Send + Sync {
    /// Draft a candidate, returning its new id.
    fn propose(&self, request: ProposePromotionRequest) -> PromotionProposeFuture<'_>;
    /// Advance a candidate through one legal transition.
    fn advance(&self, request: AdvancePromotionRequest) -> PromotionActionFuture<'_>;
    /// Persist the eval evidence a later `RunRegression` will read. The
    /// implementation re-derives the candidate's artifact kind/name/version
    /// from its own store and refuses evidence that does not parse or carries
    /// no cases — see [`SubmitEvalEvidenceRequest`].
    fn submit_eval_evidence(&self, request: SubmitEvalEvidenceRequest)
        -> PromotionActionFuture<'_>;
    /// Approve (and promote + activate) a candidate. Refused unless
    /// `request.approver` is `Actor::Human` — enforced by
    /// `codypendent_eval::promote::Candidate::approve` itself, not merely by
    /// the caller's discipline.
    fn approve(&self, request: ApprovePromotionRequest) -> PromotionActionFuture<'_>;
    /// Manually roll back a promoted candidate, attributing `request.actor`.
    fn rollback(&self, request: RollbackPromotionRequest) -> PromotionActionFuture<'_>;
}

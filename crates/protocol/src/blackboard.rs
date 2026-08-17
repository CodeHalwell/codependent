//! Blackboard wire types (Phase 5 STEP 5.3): the client-facing projection of a
//! workflow run's typed artifact board.
//!
//! Agents in a multi-agent workflow communicate **only** via blackboard artifacts
//! and declared node outputs (Chapter 04). The authoritative board lives in
//! `codypendent-workflow`'s `BlackboardStore`; this crate carries the *view* of one
//! stored artifact across the wire — the shape the daemon's read command
//! ([`CommandBody::ReadBlackboard`](crate::command::CommandBody::ReadBlackboard))
//! returns and the per-run subscription
//! ([`Subscription::Blackboard`](crate::handshake::Subscription::Blackboard))
//! delivers.
//!
//! Payload, author, and evidence ride as opaque JSON [`Value`]s so the protocol
//! stays decoupled from the workflow domain types — a client renders them, never
//! branches structurally on them (and treats them as evidence, not instructions).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The stable board id (and blackboard hub key) of a repository's task board.
///
/// A repository board is stored as a **synthetic workflow run** whose id is this
/// string, so every existing board mechanism — the `blackboard_items` FK, the
/// `ReadBlackboard` command, and the per-run `Subscription::Blackboard` fan-out —
/// serves it unchanged (`board:<repo>` feeds via the existing hub pattern). Pure
/// string formatting (the canonical repository path is already unique), so the
/// daemon, the assembly, and clients derive identical ids with no shared state.
/// Callers pass the **canonicalized** repository root; this does no I/O.
#[must_use]
pub fn board_scope_id(repository: &str) -> String {
    format!("board:{repository}")
}

/// Which durable board a client-side blackboard write targets: a workflow run's
/// board, or a repository's task board (the kanban surface). A wire enum with an
/// [`Unknown`](BlackboardScope::Unknown) fallback so a scope from a newer client
/// is rejected structurally rather than failing the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum BlackboardScope {
    /// A durable workflow run's board.
    WorkflowRun { workflow_run_id: String },
    /// A repository's task board — resolved server-side to the synthetic board
    /// run [`board_scope_id`] names, created on first use. `repository` is the
    /// canonical filesystem root, exactly as `StartRun.repository` carries it.
    RepositoryBoard { repository: String },
    #[serde(other)]
    Unknown,
}

/// A client-authored blackboard artifact before it is stored (Phase B kanban —
/// the write half `PostBlackboardItem` carries). The author is **not** here: the
/// daemon builds it server-side from the issuing connection, exactly as the
/// workflow executor builds an agent's author — a client never supplies its own
/// attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BlackboardItemDraft {
    /// The typed artifact kind (`task` for board cards; any store kind is legal).
    pub kind: String,
    /// The artifact body (opaque JSON — for a `task`, conventionally
    /// `{ "title": …, "description": … }`).
    pub payload: Value,
    /// The author's confidence in `[0, 1]`, if given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Evidence references grounding the artifact. Claim-like kinds require at
    /// least one (the store enforces it); a `task` needs none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Value>,
    /// The board column (`todo` / `doing` / `review` / `done`, or a validated
    /// free string). Defaults server-side to `todo` for a `task`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Who the card is assigned to, if anyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// The card's position within its column (lower sorts first). Appended to
    /// the end of the column when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<i64>,
}

/// One stored blackboard artifact, projected for a client.
///
/// A read-command reply carries a `Vec` of these (the run's board, kind-filtered);
/// a subscription delivers one as each post/supersede lands. The `workflow_run_id`
/// travels with the item so a client routes a live
/// [`Payload::BlackboardPosted`](crate::envelope::Payload::BlackboardPosted) to the
/// right board without consulting the enclosing frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BlackboardItemView {
    /// The artifact's stable id (a UUIDv7 string).
    pub id: String,
    /// The workflow run whose board holds it.
    pub workflow_run_id: String,
    /// The typed artifact kind (`finding`, `decision`, `hypothesis`, …), as the
    /// manifest-facing string the `BlackboardStore` records.
    pub kind: String,
    /// The artifact body (opaque JSON — a client renders it).
    pub payload: Value,
    /// Who produced it — the daemon builds this server-side from the authoring
    /// node's run context (`{role, run_id, node_id, workflow_run_id}`), never from
    /// model-supplied identity.
    pub author: Value,
    /// The author's self-reported confidence in `[0, 1]`, if given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Evidence references grounding the artifact (opaque JSON). Claim-like kinds
    /// require at least one; the store enforces it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Value>,
    /// The artifact's revision within its supersession chain (1 for an original).
    pub revision: u32,
    /// The id of the item that superseded this one, if any — a live item has
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// The repository this item's board serves, when the item lives on a
    /// repository task board rather than a real workflow run (its
    /// `workflow_run_id` is then the synthetic [`board_scope_id`]). Additive: an
    /// older daemon sends none and every field below parses back defaulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_scope: Option<String>,
    /// The board column (`todo` / `doing` / `review` / `done`, or a validated
    /// free string), when this item is a board card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Who the card is assigned to, if anyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// The card's position within its column (lower sorts first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view() -> BlackboardItemView {
        BlackboardItemView {
            id: "0192-item".to_string(),
            workflow_run_id: "wfrun-abc".to_string(),
            kind: "finding".to_string(),
            payload: json!({ "summary": "the parser drops trailing commas" }),
            author: json!({ "role": "investigator", "node_id": "diagnose" }),
            confidence: Some(0.8),
            evidence: vec![json!({ "path": "src/parse.rs", "line": 42 })],
            revision: 1,
            superseded_by: None,
            board_scope: None,
            status: None,
            assignee: None,
            ordinal: None,
        }
    }

    #[test]
    fn blackboard_item_view_round_trips() {
        let original = view();
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: BlackboardItemView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn absent_optionals_are_skipped_and_default_back() {
        // A live item with no confidence and no evidence omits both keys, and such
        // a payload reparses with them defaulted (an older client that sends none
        // still round-trips).
        let mut item = view();
        item.confidence = None;
        item.evidence = Vec::new();
        item.superseded_by = None;
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(!json.contains("confidence"), "confidence skipped: {json}");
        assert!(!json.contains("evidence"), "evidence skipped: {json}");
        assert!(
            !json.contains("superseded_by"),
            "superseded_by skipped: {json}"
        );
        // The board fields are additive: absent for a plain workflow artifact
        // (an older daemon's exact shape) and defaulted on reparse.
        assert!(!json.contains("board_scope"), "board_scope skipped: {json}");
        assert!(!json.contains("status"), "status skipped: {json}");
        assert!(!json.contains("assignee"), "assignee skipped: {json}");
        assert!(!json.contains("ordinal"), "ordinal skipped: {json}");
        let parsed: BlackboardItemView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, item);
    }

    #[test]
    fn a_board_card_round_trips_its_board_fields() {
        let mut card = view();
        card.kind = "task".to_string();
        card.workflow_run_id = board_scope_id("/home/user/project");
        card.board_scope = Some("/home/user/project".to_string());
        card.status = Some("doing".to_string());
        card.assignee = Some("dana".to_string());
        card.ordinal = Some(3);
        let json = serde_json::to_string(&card).expect("serialize");
        let parsed: BlackboardItemView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, card);
    }

    #[test]
    fn board_scope_id_is_stable_and_prefixed() {
        // The synthetic run id doubles as the subscription hub key, so its shape
        // is a wire contract: a fixed prefix plus the canonical repository path.
        assert_eq!(
            board_scope_id("/home/user/project"),
            "board:/home/user/project"
        );
    }

    #[test]
    fn blackboard_scope_round_trips_and_tolerates_unknown() {
        for scope in [
            BlackboardScope::WorkflowRun {
                workflow_run_id: "wfrun-abc".to_string(),
            },
            BlackboardScope::RepositoryBoard {
                repository: "/home/user/project".to_string(),
            },
        ] {
            let json = serde_json::to_string(&scope).expect("serialize");
            assert_eq!(
                serde_json::from_str::<BlackboardScope>(&json).expect("deserialize"),
                scope
            );
        }
        let future = json!({ "type": "OrganizationBoard", "org": "acme" });
        assert!(matches!(
            serde_json::from_value::<BlackboardScope>(future).expect("unknown scope parses"),
            BlackboardScope::Unknown
        ));
    }

    #[test]
    fn item_draft_omits_absent_optionals_and_defaults_back() {
        let draft = BlackboardItemDraft {
            kind: "task".to_string(),
            payload: json!({ "title": "wire the DAG viewer" }),
            confidence: None,
            evidence: Vec::new(),
            status: None,
            assignee: None,
            ordinal: None,
        };
        let json = serde_json::to_string(&draft).expect("serialize");
        assert!(!json.contains("confidence"), "confidence skipped: {json}");
        assert!(!json.contains("status"), "status skipped: {json}");
        assert!(!json.contains("ordinal"), "ordinal skipped: {json}");
        let parsed: BlackboardItemDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, draft);
    }
}

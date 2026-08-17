//! Durable, owner-scoped inbox wire contracts.
//!
//! Ownership is derived from the authenticated repository grant by the daemon.
//! Consequently none of the client-authored query or mutation types contains an
//! owner identity.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{
    ApprovalId, InboxEntryId, PluginId, QuestionId, RepositoryId, RunId, SessionId, WorkflowId,
};
use crate::session::PageCursor;

/// The human work or notification represented by an inbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum InboxEntryKind {
    ApprovalRequest,
    AgentQuestion,
    RunCompleted,
    RunFailed,
    BudgetWarning,
    WorkflowBlocked,
    PluginPermissionChanged,
    RunnerFailed,
    #[serde(other)]
    Unknown,
}

/// Read/lifecycle state of a durable inbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum InboxEntryState {
    #[default]
    Unread,
    Acknowledged,
    Dismissed,
    Resolved,
    #[serde(other)]
    Unknown,
}

/// Durable source identity from which the daemon derives the deduplication key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum InboxSourceIdentity {
    Approval {
        approval_id: ApprovalId,
    },
    Question {
        question_id: QuestionId,
    },
    Run {
        run_id: RunId,
    },
    Budget {
        budget_id: String,
    },
    Workflow {
        workflow_id: WorkflowId,
    },
    Plugin {
        plugin_id: PluginId,
    },
    Runner {
        runner_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Stable provenance used by the repository to deduplicate an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InboxSource {
    pub identity: InboxSourceIdentity,
    /// Stable within an owner. Replaying the same source must reuse this key.
    pub dedup_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
}

/// A typed navigation target. Clients never need to interpret an arbitrary URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum InboxDeepLink {
    Approval {
        approval_id: ApprovalId,
    },
    Question {
        question_id: QuestionId,
    },
    Session {
        session_id: SessionId,
    },
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    Workflow {
        workflow_id: WorkflowId,
    },
    Plugin {
        plugin_id: PluginId,
    },
    Repository {
        repository_id: RepositoryId,
    },
    #[serde(other)]
    Unknown,
}

/// Repository-authorized client projection of an inbox row.
///
/// There is intentionally no `owner_id`: authorization and owner scoping are
/// repository concerns and cannot be selected or asserted by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InboxEntry {
    pub id: InboxEntryId,
    pub repository_id: RepositoryId,
    pub kind: InboxEntryKind,
    #[serde(default)]
    pub state: InboxEntryState,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    pub source: InboxSource,
    pub deep_link: InboxDeepLink,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<DateTime<Utc>>,
    /// Set only when the authoritative source operation resolves. Inbox
    /// acknowledgement and dismissal never decide an approval or question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Explicit name for the authorized row projection.
pub type InboxEntryProjection = InboxEntry;

/// A cursor page returned by an inbox list operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InboxPage {
    pub items: Vec<InboxEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Optional list restrictions. An empty value means all authorized entries.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InboxListFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<InboxEntryKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<InboxEntryState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repository_ids: Vec<RepositoryId>,
}

/// Cursor-based inbox list request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InboxListQuery {
    #[serde(default)]
    pub filters: InboxListFilters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Idempotent state change requested for an inbox entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum InboxMutation {
    Acknowledge {
        entry_id: InboxEntryId,
    },
    Dismiss {
        entry_id: InboxEntryId,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additive_defaults_and_owner_is_not_on_the_query_wire() {
        let query: InboxListQuery = serde_json::from_str("{}").expect("default query");
        assert_eq!(query, InboxListQuery::default());
        let json = serde_json::to_value(query).expect("serialize query");
        assert!(json.get("owner_id").is_none());

        let state: InboxEntryState =
            serde_json::from_value(serde_json::json!({ "type": "Unread" })).expect("state");
        assert_eq!(state, InboxEntryState::Unread);
    }

    #[test]
    fn unknown_kinds_links_states_and_mutations_are_safe() {
        let future = serde_json::json!({ "type": "AddedLater", "extra": true });
        assert_eq!(
            serde_json::from_value::<InboxEntryKind>(future.clone()).expect("kind"),
            InboxEntryKind::Unknown
        );
        assert_eq!(
            serde_json::from_value::<InboxEntryState>(future.clone()).expect("state"),
            InboxEntryState::Unknown
        );
        assert_eq!(
            serde_json::from_value::<InboxDeepLink>(future.clone()).expect("link"),
            InboxDeepLink::Unknown
        );
        assert_eq!(
            serde_json::from_value::<InboxMutation>(future).expect("mutation"),
            InboxMutation::Unknown
        );
    }

    #[test]
    fn list_query_preserves_an_opaque_cursor() {
        let value = serde_json::json!({
            "cursor": "opaque:do-not-interpret",
            "limit": 25,
            "filters": { "states": [{ "type": "Unread" }] }
        });
        let query: InboxListQuery = serde_json::from_value(value.clone()).expect("query");
        assert_eq!(serde_json::to_value(query).expect("serialize"), value);
    }

    #[cfg(feature = "schema-export")]
    #[test]
    fn public_contracts_generate_json_schema() {
        let _ = schemars::schema_for!(InboxEntry);
        let _ = schemars::schema_for!(InboxListQuery);
        let _ = schemars::schema_for!(InboxMutation);
    }
}

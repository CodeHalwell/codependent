//! Session-library, history, lifecycle, and editor-action wire contracts.
//!
//! Cursors are deliberately opaque to clients. Optional fields are additive:
//! absent values deserialize to their defaults and are omitted when encoded.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::events::SessionEvent;
use crate::ide::{Diagnostic, IdeContextUpdate};
use crate::ids::{ArtifactId, ModelId, RepositoryId, RunId, SessionId, WorkflowId, WorkspaceId};
use crate::run::RunState;

/// An opaque continuation token issued by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PageCursor(pub String);

/// One stable page of results. `next_cursor` is absent at the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Summary shown by session pickers and the Session Library.
///
/// The first six fields are the original v0.9 contract. Everything after them
/// is additive so historical payloads remain valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub state: String,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub internal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_state: Option<RunState>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Filters applied together by the ranked session search service.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionSearchFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_ids: Vec<WorkflowId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<ModelId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repository_ids: Vec<RepositoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_before: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_states: Vec<RunState>,
}

/// The indexed material responsible for a search hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionSearchSource {
    Title,
    Transcript,
    ToolObservation,
    Patch,
    Artifact,
    ChangedPath,
    Symbol,
    #[serde(other)]
    Unknown,
}

/// Authorization scope in which a search hit was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionSearchScope {
    Session,
    Repository,
    Workspace,
    User,
    #[serde(other)]
    Unknown,
}

/// Stable navigation target for a session-library result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionDeepLink {
    Session {
        session_id: SessionId,
    },
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    Event {
        session_id: SessionId,
        sequence: u64,
    },
    Artifact {
        session_id: SessionId,
        artifact_id: ArtifactId,
    },
    Path {
        session_id: SessionId,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<u32>,
    },
    Symbol {
        session_id: SessionId,
        symbol: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// A ranked search hit with a durable identity and navigable target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionSearchResult {
    pub session: SessionSummary,
    pub source: SessionSearchSource,
    pub scope: SessionSearchScope,
    pub stable_identity: String,
    pub deep_link: SessionDeepLink,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// Request for ranked session search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionSearchQuery {
    pub query: String,
    #[serde(default)]
    pub filters: SessionSearchFilters,
    #[serde(default)]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}

/// Cursor-paged ranked search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionSearchPage {
    pub items: Vec<SessionSearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Cursor-paged durable session history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionHistoryPage {
    pub items: Vec<SessionEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Portable session export formats understood by clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionExportFormat {
    Json,
    Markdown,
    #[serde(other)]
    Unknown,
}

/// Controls bounded data included in a session export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SessionExportOptions {
    pub format: SessionExportFormat,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_artifacts: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_internal_sessions: bool,
}

/// Retention behavior requested by a session deletion. The daemon remains the
/// policy authority and may reject a mode rather than weakening retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionDeletionMode {
    #[default]
    RetentionPolicy,
    TombstoneOnly,
    #[serde(other)]
    Unknown,
}

/// A lifecycle mutation. The containing command supplies the idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionLifecycleAction {
    Rename {
        title: String,
    },
    Pin,
    Unpin,
    Archive,
    Restore,
    Delete {
        #[serde(default)]
        mode: SessionDeletionMode,
    },
    Export {
        options: SessionExportOptions,
    },
    #[serde(other)]
    Unknown,
}

/// An ordinary run entry point contributed by an editor client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum EditorNativeAction {
    FixSelection,
    ExplainSelection,
    ReviewCurrentFile,
    GenerateTestsForSelection,
    FixDiagnostic {
        diagnostic: Diagnostic,
    },
    #[serde(other)]
    Unknown,
}

/// Current editor state attached to an editor-native action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EditorActionContext {
    pub ide: IdeContextUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Diagnostic>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_session_summary_deserializes_and_additions_are_omitted() {
        let old = json!({
            "session_id": SessionId::new(), "workspace_id": null,
            "title": "old", "state": "open",
            "updated_at": "2026-08-16T10:00:00Z",
            "created_at": "2026-08-16T09:00:00Z"
        });
        let summary: SessionSummary = serde_json::from_value(old.clone()).expect("old summary");
        assert!(!summary.internal && !summary.pinned);
        assert_eq!(summary.workspace, None);
        assert_eq!(serde_json::to_value(summary).expect("serialize"), old);
    }

    #[test]
    fn opaque_cursor_round_trips() {
        let page = CursorPage {
            items: vec!["hit".to_string()],
            next_cursor: Some(PageCursor("not-an-offset".into())),
        };
        let json = serde_json::to_string(&page).expect("serialize");
        assert_eq!(
            serde_json::from_str::<CursorPage<String>>(&json).expect("parse"),
            page
        );
    }

    #[test]
    fn unknown_tags_degrade_safely() {
        assert_eq!(
            serde_json::from_value::<EditorNativeAction>(json!({"type":"FutureAction"}))
                .expect("parse"),
            EditorNativeAction::Unknown
        );
        assert_eq!(
            serde_json::from_value::<SessionSearchSource>(json!({"type":"FutureSource"}))
                .expect("parse"),
            SessionSearchSource::Unknown
        );
    }

    #[test]
    fn defaults_are_omitted() {
        let options = SessionExportOptions {
            format: SessionExportFormat::Json,
            include_artifacts: false,
            include_internal_sessions: false,
        };
        assert_eq!(
            serde_json::to_value(options).expect("serialize"),
            json!({"format":{"type":"Json"}})
        );
        assert_eq!(
            serde_json::to_value(SessionSearchFilters::default()).expect("serialize"),
            json!({})
        );
    }
}

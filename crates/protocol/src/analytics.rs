//! Usage and quality analytics wire contracts.
//!
//! Measurements are deliberately optional: an absent value means the producer
//! did not measure that dimension and must never be interpreted as zero.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactRef;
use crate::session::PageCursor;

/// Inclusive start and exclusive end of an analytics query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsTimeRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

/// Completion outcome used both as a filter and an aggregate dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsCompletion {
    Successful,
    Failed,
    Cancelled,
    Incomplete,
    #[serde(other)]
    Unknown,
}

/// Dimensions by which observations may be grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsGrouping {
    Model,
    Provider,
    Repository,
    Workflow,
    TaskClass,
    Time,
    Completion,
    Route,
    #[serde(other)]
    Unknown,
}

/// Optional restrictions on observations. Empty lists do not restrict.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<AnalyticsTimeRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completions: Vec<AnalyticsCompletion>,
}

/// A bounded, cursor-paged aggregate query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsQuery {
    #[serde(default)]
    pub filters: AnalyticsFilters,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<AnalyticsGrouping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    /// Requested page size. The server applies its own upper bound; zero means
    /// the server default.
    #[serde(default)]
    pub limit: u32,
}

/// Number of observations for which a dimension was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MeasurementCoverage {
    pub measured: u64,
    pub total: u64,
}

/// Coverage is explicit per nullable metric, making partial aggregates visible.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsDimensionCoverage {
    pub input_tokens: MeasurementCoverage,
    pub output_tokens: MeasurementCoverage,
    pub cached_tokens: MeasurementCoverage,
    pub reasoning_tokens: MeasurementCoverage,
    pub cost: MeasurementCoverage,
    pub latency: MeasurementCoverage,
    pub grader_score: MeasurementCoverage,
    pub cost_per_successful_task: MeasurementCoverage,
    pub retry_count: MeasurementCoverage,
    pub escalation_count: MeasurementCoverage,
    pub completion_count: MeasurementCoverage,
}

/// Aggregate values for a grouping bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Measured USD cost in millionths of a dollar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grader_score_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_successful_task_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_count: Option<u64>,
    #[serde(default)]
    pub coverage: AnalyticsDimensionCoverage,
}

/// A result bucket. Dimension keys correspond in order to `query.group_by`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBucket {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
    pub metrics: AnalyticsMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsPage {
    pub items: Vec<AnalyticsBucket>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Supported analytics export encodings. The request must choose explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsExportFormat {
    Json,
    Csv,
    #[serde(other)]
    Unknown,
}

/// Request for a server-bounded export of an analytics query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsExportRequest {
    pub query: AnalyticsQuery,
    pub format: AnalyticsExportFormat,
    /// Requested row ceiling. The server may impose a smaller ceiling; zero
    /// selects the server default.
    #[serde(default)]
    pub max_rows: u32,
}

/// Metadata for a completed export. Bulk JSON/CSV bytes live in the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsExportResult {
    pub format: AnalyticsExportFormat,
    pub artifact: ArtifactRef,
    pub row_count: u64,
    #[serde(default)]
    pub truncated: bool,
    pub generated_at: DateTime<Utc>,
}

// --- Spend / usage budgets (migration 0043 `analytics_budgets`) --------------

/// The measured dimension a budget threshold applies to.
///
/// Deliberately a CLOSED set of *measured* dimensions, matching the migration's
/// `CHECK (dimension IN (...))`. A budget over an unmeasured dimension would
/// have to read `NULL` as `0` to decide anything, which is exactly the
/// zero-coercion the measurement contract forbids — so those dimensions are not
/// expressible here at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsBudgetDimension {
    CostMicros,
    InputTokens,
    OutputTokens,
    LatencyMs,
    /// A dimension this build does not know. Fails closed: the daemon refuses
    /// such a budget rather than guessing a column.
    #[serde(other)]
    Unknown,
}

/// The rolling window a budget's threshold is measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsBudgetWindow {
    Day,
    Week,
    Month,
    #[serde(other)]
    Unknown,
}

/// What a budget is scoped to. `Owner` covers everything the principal runs;
/// the others narrow to one repository, workflow, or model.
///
/// The storage layer splits this into `(scope, scope_value)` because 0043 does
/// — `scope_value` is `''` for `Owner` — but the wire contract keeps the value
/// attached to the scope that gives it meaning, so a repository id can never be
/// paired with `scope = 'model'`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsBudgetScope {
    /// Every observation owned by the principal.
    Owner,
    Repository {
        repository_id: String,
    },
    Workflow {
        workflow_id: String,
    },
    Model {
        model_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// A budget as a client asks for it. The owner is never on the wire: it is the
/// connection's kernel-derived principal, exactly like every other owner-scoped
/// store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBudgetDraft {
    pub scope: AnalyticsBudgetScope,
    pub dimension: AnalyticsBudgetDimension,
    pub window: AnalyticsBudgetWindow,
    /// Strictly positive (0043 `CHECK (threshold > 0)`): a zero threshold would
    /// alert on the first measured observation forever.
    pub threshold: u64,
    #[serde(default = "budget_enabled_default")]
    pub enabled: bool,
}

fn budget_enabled_default() -> bool {
    true
}

/// A stored budget with its server-assigned identity and timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBudget {
    /// Opaque server-minted id. A plain `String` rather than a UUID newtype
    /// because 0043 declares `id TEXT PRIMARY KEY` with no format constraint
    /// and rows predating this command may carry any text.
    pub id: String,
    #[serde(flatten)]
    pub definition: AnalyticsBudgetDraft,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A sparse update. An absent field is unchanged; scope, dimension and window
/// are immutable because they are the row's UNIQUE identity — changing one is a
/// delete plus a create, and pretending otherwise would silently collide.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBudgetPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Optional narrowing of a budget listing. There is no cursor and no total:
/// budgets are bounded per owner by 0043's UNIQUE constraint, and a count is
/// the kind of aggregate that leaks volume, so the server caps the page and
/// says so with `truncated` instead.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBudgetQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Requested row ceiling; zero selects the server default. The server may
    /// impose a smaller ceiling.
    #[serde(default)]
    pub limit: u32,
}

/// One page of budgets owned by the requesting principal.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AnalyticsBudgetPage {
    pub items: Vec<AnalyticsBudget>,
    /// The server's ceiling cut the listing short. Honest truncation rather
    /// than a total the caller could not otherwise see.
    #[serde(default)]
    pub truncated: bool,
}

/// Normalized budget CRUD. The containing command provides idempotency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalyticsBudgetRequest {
    Create {
        budget: AnalyticsBudgetDraft,
    },
    Get {
        id: String,
    },
    List {
        query: AnalyticsBudgetQuery,
    },
    Update {
        id: String,
        patch: AnalyticsBudgetPatch,
    },
    Delete {
        id: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_measurement_is_distinct_from_measured_zero() {
        let missing: AnalyticsMetrics = serde_json::from_value(json!({})).expect("defaults");
        assert_eq!(missing.input_tokens, None);
        assert_eq!(missing.cost_micros, None);

        let zero: AnalyticsMetrics = serde_json::from_value(json!({
            "input_tokens": 0,
            "cost_micros": 0
        }))
        .expect("measured zeros");
        assert_eq!(zero.input_tokens, Some(0));
        assert_eq!(zero.cost_micros, Some(0));
        let encoded = serde_json::to_value(zero).expect("serialize");
        assert_eq!(encoded["input_tokens"], 0);
        assert_eq!(encoded["cost_micros"], 0);
    }

    #[test]
    fn query_and_metric_defaults_are_safe_and_bounded_by_server() {
        let query: AnalyticsQuery = serde_json::from_value(json!({})).expect("query defaults");
        assert_eq!(query, AnalyticsQuery::default());
        assert_eq!(query.limit, 0);

        let metrics: AnalyticsMetrics = serde_json::from_value(json!({})).expect("metric defaults");
        assert_eq!(metrics.retry_count, None);
        assert_eq!(metrics.coverage, AnalyticsDimensionCoverage::default());
    }

    #[test]
    fn unknown_enum_variants_fall_back() {
        let grouping: AnalyticsGrouping =
            serde_json::from_value(json!({ "type": "organization" })).expect("grouping");
        let completion: AnalyticsCompletion =
            serde_json::from_value(json!({ "type": "timed_out" })).expect("completion");
        let format: AnalyticsExportFormat =
            serde_json::from_value(json!({ "type": "parquet" })).expect("format");
        assert_eq!(grouping, AnalyticsGrouping::Unknown);
        assert_eq!(completion, AnalyticsCompletion::Unknown);
        assert_eq!(format, AnalyticsExportFormat::Unknown);
    }

    #[test]
    fn unknown_budget_request_dimension_and_scope_fall_back() {
        let request: AnalyticsBudgetRequest =
            serde_json::from_value(json!({ "type": "rotate", "id": "b-1" })).expect("request");
        let dimension: AnalyticsBudgetDimension =
            serde_json::from_value(json!({ "type": "cached_tokens" })).expect("dimension");
        let window: AnalyticsBudgetWindow =
            serde_json::from_value(json!({ "type": "quarter" })).expect("window");
        let scope: AnalyticsBudgetScope =
            serde_json::from_value(json!({ "type": "organization", "organization_id": "o-1" }))
                .expect("scope");
        assert_eq!(request, AnalyticsBudgetRequest::Unknown);
        assert_eq!(dimension, AnalyticsBudgetDimension::Unknown);
        assert_eq!(window, AnalyticsBudgetWindow::Unknown);
        assert_eq!(scope, AnalyticsBudgetScope::Unknown);
    }

    #[test]
    fn budget_draft_defaults_enabled_and_keeps_scope_value_with_its_scope() {
        let draft: AnalyticsBudgetDraft = serde_json::from_value(json!({
            "scope": { "type": "repository", "repository_id": "/repo" },
            "dimension": { "type": "cost_micros" },
            "window": { "type": "day" },
            "threshold": 5000
        }))
        .expect("draft");
        assert!(draft.enabled);
        assert_eq!(
            draft.scope,
            AnalyticsBudgetScope::Repository {
                repository_id: "/repo".to_string()
            }
        );
        // A sparse patch never carries scope/dimension/window: they are the
        // row's UNIQUE identity and are not updatable.
        let patch = AnalyticsBudgetPatch {
            enabled: Some(false),
            ..Default::default()
        };
        let encoded = serde_json::to_value(&patch).expect("serialize patch");
        assert_eq!(encoded, json!({ "enabled": false }));
    }
}

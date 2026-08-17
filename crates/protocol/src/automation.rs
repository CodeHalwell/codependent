//! Wire contracts for trigger and schedule automation bindings.
//!
//! Bindings select a versioned workflow and describe when it may be invoked.
//! [`InvocationPolicy`] deliberately lives here rather than in a workflow
//! definition: the same workflow can be bound to several sources with different
//! operational and approval policies. Webhook configuration contains only stable
//! endpoint/key references; secret material never crosses this contract.

use crate::ids::{AutomationBindingId, RepositoryId, WorkflowId};
use crate::session::PageCursor;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The event or schedule that can invoke a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TriggerSource {
    Cron {
        expression: String,
        timezone: String,
    },
    OneTime {
        at: DateTime<Utc>,
    },
    GitHubWebhook {
        endpoint_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installation_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
    },
    SignedWebhook {
        endpoint_id: String,
        signature: WebhookSignatureScheme,
        /// Reference to daemon-owned secret material, never the secret itself.
        signing_key_ref: String,
    },
    CiFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        workflows: Vec<String>,
    },
    RepositoryChange,
    CodeGraphChange,
    DependencyAlert {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ecosystems: Vec<String>,
    },
    Manual,
    Api,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WebhookSignatureScheme {
    #[default]
    HmacSha256,
    Ed25519,
    #[serde(other)]
    Unknown,
}

/// Common source filters. Values are public event metadata, never credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TriggerFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DeduplicationPolicy {
    /// Names of normalized event fields which form the identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_fields: Vec<String>,
    #[serde(default = "default_dedup_window_seconds")]
    pub window_seconds: u64,
}

impl Default for DeduplicationPolicy {
    fn default() -> Self {
        Self {
            identity_fields: Vec::new(),
            window_seconds: default_dedup_window_seconds(),
        }
    }
}

const fn default_dedup_window_seconds() -> u64 {
    86_400
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConcurrencyPolicy {
    #[default]
    Allow,
    Skip,
    Queue,
    Replace,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TriggerRetryPolicy {
    #[serde(default)]
    pub max_attempts: u32,
    #[serde(default = "default_retry_delay_seconds")]
    pub initial_delay_seconds: u64,
    #[serde(default = "default_retry_multiplier")]
    pub backoff_multiplier: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delay_seconds: Option<u64>,
}

impl Default for TriggerRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 0,
            initial_delay_seconds: default_retry_delay_seconds(),
            backoff_multiplier: default_retry_multiplier(),
            max_delay_seconds: None,
        }
    }
}

const fn default_retry_delay_seconds() -> u64 {
    30
}
const fn default_retry_multiplier() -> u32 {
    2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MissedRunPolicy {
    #[default]
    Skip,
    RunOnce,
    CatchUp {
        max_occurrences: u32,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BudgetCeiling {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutomationApprovalMode {
    #[default]
    Inherit,
    AlwaysRequire,
    PolicyDriven,
    Preapproved {
        approval_receipt: String,
    },
    #[serde(other)]
    Unknown,
}

/// Per-binding invocation controls, independent of the workflow definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InvocationPolicy {
    #[serde(default)]
    pub deduplication: DeduplicationPolicy,
    #[serde(default)]
    pub concurrency: ConcurrencyPolicy,
    #[serde(default)]
    pub retry: TriggerRetryPolicy,
    #[serde(default)]
    pub missed_run: MissedRunPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ceiling: Option<BudgetCeiling>,
    #[serde(default)]
    pub approval_mode: AutomationApprovalMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutomationBindingDraft {
    pub name: String,
    pub source: TriggerSource,
    pub workflow_id: WorkflowId,
    pub workflow_version: String,
    pub repository_id: RepositoryId,
    #[serde(default)]
    pub filters: TriggerFilters,
    #[serde(default)]
    pub invocation: InvocationPolicy,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

const fn default_enabled() -> bool {
    true
}

/// Sparse update. Nested policy values are replaced as a normalized unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutomationBindingPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<TriggerSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<TriggerFilters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<InvocationPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutomationBinding {
    pub id: AutomationBindingId,
    #[serde(flatten)]
    pub definition: AutomationBindingDraft,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutomationBindingQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<WorkflowId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutomationBindingPage {
    pub items: Vec<AutomationBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
}

/// Normalized CRUD requests. The containing command provides idempotency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutomationBindingRequest {
    Create {
        binding: AutomationBindingDraft,
    },
    Get {
        id: AutomationBindingId,
    },
    List {
        query: AutomationBindingQuery,
    },
    Update {
        id: AutomationBindingId,
        patch: AutomationBindingPatch,
    },
    Delete {
        id: AutomationBindingId,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn draft_defaults_are_additive_and_secret_free() {
        let workflow_id = WorkflowId::new();
        let repository_id = RepositoryId::new();
        let draft: AutomationBindingDraft = serde_json::from_value(json!({
            "name": "nightly health",
            "source": { "type": "cron", "expression": "0 2 * * *", "timezone": "UTC" },
            "workflow_id": workflow_id,
            "workflow_version": "v3",
            "repository_id": repository_id
        }))
        .expect("deserialize minimal draft");
        assert!(draft.enabled);
        assert_eq!(draft.invocation, InvocationPolicy::default());
        assert_eq!(draft.filters, TriggerFilters::default());
    }

    #[test]
    fn signed_webhook_round_trip_contains_reference_not_secret() {
        let source = TriggerSource::SignedWebhook {
            endpoint_id: "hook_release".into(),
            signature: WebhookSignatureScheme::HmacSha256,
            signing_key_ref: "keyring://automation/hook_release".into(),
        };
        let value = serde_json::to_value(&source).expect("serialize");
        assert_eq!(
            value["signing_key_ref"],
            "keyring://automation/hook_release"
        );
        assert!(value.get("secret").is_none());
        assert_eq!(
            serde_json::from_value::<TriggerSource>(value).expect("deserialize"),
            source
        );
    }

    #[test]
    fn unknown_source_and_request_are_forward_compatible() {
        let source: TriggerSource =
            serde_json::from_value(json!({"type":"future_bus","topic":"x"}))
                .expect("unknown source");
        let request: AutomationBindingRequest =
            serde_json::from_value(json!({"type":"rotate","id":"x"})).expect("unknown request");
        assert!(matches!(source, TriggerSource::Unknown));
        assert!(matches!(request, AutomationBindingRequest::Unknown));
    }

    #[test]
    fn sparse_patch_omits_unchanged_fields() {
        let patch = AutomationBindingPatch {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(patch).expect("serialize"),
            json!({"enabled": false})
        );
    }
}

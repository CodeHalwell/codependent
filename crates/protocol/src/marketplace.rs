//! Wire contracts for the Codypendent marketplace.

use serde::{Deserialize, Serialize};

/// A public package/install metadata view returned by marketplace commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MarketplacePackageView {
    pub id: String,
    pub publisher_id: String,
    pub kind: String,
    pub display_name: String,
    pub summary: String,
    pub latest_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_scope: Option<String>,
}

//! Stable, opaque keyset pagination models.
//!
//! Pagination uses opaque keyset cursors bound to query hashes to prevent tampering
//! and existence oracle probing across tenants.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CursorDecodeError {
    #[error("invalid cursor format: expected base64-encoded token")]
    InvalidEncoding,
    #[error("invalid cursor layout: missing expected component")]
    InvalidLayout,
}

/// An opaque, stable keyset pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PageCursor(pub String);

impl PageCursor {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Encode a keyset cursor containing a query hash, sort value, and unique row ID.
    #[must_use]
    pub fn encode_keyset(query_hash: &str, sort_key: &str, row_id: &str) -> Self {
        let raw = format!("{query_hash}:{sort_key}:{row_id}");
        let encoded = hex::encode(raw);
        Self(encoded)
    }

    /// Decode a keyset cursor into its (query_hash, sort_key, row_id) components.
    ///
    /// The sort key is taken as everything *between* the first and last separator, because
    /// the commonest sort key is an RFC3339 timestamp and that contains colons. Splitting
    /// left-to-right into three parts truncated `2026-08-17T00:00:00Z` to `2026-08-17T00`
    /// and folded the rest into the row ID. The query hash is hex and the row ID is a UUID,
    /// so neither end can itself contain a separator.
    pub fn decode_keyset(&self) -> Result<(String, String, String), CursorDecodeError> {
        let bytes = hex::decode(&self.0).map_err(|_| CursorDecodeError::InvalidEncoding)?;
        let raw = String::from_utf8(bytes).map_err(|_| CursorDecodeError::InvalidEncoding)?;
        let (query_hash, rest) = raw
            .split_once(':')
            .ok_or(CursorDecodeError::InvalidLayout)?;
        let (sort_key, row_id) = rest
            .rsplit_once(':')
            .ok_or(CursorDecodeError::InvalidLayout)?;
        Ok((
            query_hash.to_string(),
            sort_key.to_string(),
            row_id.to_string(),
        ))
    }
}

impl fmt::Display for PageCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PageCursor {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

/// Standard page query request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Standard paginated response envelope.
///
/// The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default
/// `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the
/// generated TypeScript — a name no client author would type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema-export", schemars(rename = "{T}Page"))]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<PageCursor>,
    pub has_more: bool,
    /// Bounded count computed strictly inside the authorized set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_count: Option<u64>,
}

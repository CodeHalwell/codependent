//! Client-facing memory inspection, correction, and forgetting (Chapter 06's
//! right to inspect, edit, and delete what the fabric remembers).
//!
//! # Why these types are projections, not mirrors
//!
//! The authoritative memory record lives in `codypendent-knowledge`
//! (`MemoryRecord`, `Scope`, `EvidenceRef`). Protocol is a leaf crate and must
//! not grow a second copy of that type graph — a copy is exactly the thing that
//! drifts, and the golden-vector guard exists because this codebase has already
//! paid for one such duplication (the S1 bug). So [`MemoryView`] carries only
//! what a client renders, a scope arrives as the two scalars the store already
//! indexes ([`MemoryScope`]), and evidence is addressed by *index* rather than
//! by a wire copy of `EvidenceRef`.
//!
//! # Scope is named by tier, never by key
//!
//! [`ForgetMemoryScope`](crate::command::CommandBody::ForgetMemoryScope) takes a
//! [`MemoryScopeTier`], not a scope key. The daemon maps the tier onto one of
//! the scopes the caller can already see, so there is no way to *name* a scope
//! outside that set — a bulk delete cannot be aimed at another repository's
//! memories even by a caller that guesses its id.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::artifact::DataClassification;
use crate::events::SessionEvent;
use crate::ids::MemoryId;

/// A memory's scope as the two scalars the store indexes (`scope_tier` /
/// `scope_key`). `key` is absent for the keyless `system` tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MemoryScope {
    pub tier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Which of the caller's *visible* scopes a bulk forget targets. Deliberately
/// not a scope key: see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum MemoryScopeTier {
    /// Built-in, daemon-authored memories.
    System,
    /// The local operator's own user scope.
    User,
    /// The repository named by the command.
    Repository,
    #[serde(other)]
    Unknown,
}

/// One memory as a client sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MemoryView {
    pub id: MemoryId,
    pub scope: MemoryScope,
    /// The memory class as its stored wire name (`semantic`, `episodic`, …).
    pub class: String,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_value: Option<serde_json::Value>,
    pub confidence: f32,
    pub observed_at: DateTime<Utc>,
    pub sensitivity: DataClassification,
    /// The memories this one replaced, when it is itself a correction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<MemoryId>,
    /// One human-legible label per evidence ref, in the SAME order
    /// [`OpenMemoryEvidence`](crate::command::CommandBody::OpenMemoryEvidence)'s
    /// `evidence_index` addresses them — so "show me where this came from" is a
    /// position in this list, and a client never has to reconstruct an
    /// `EvidenceRef` it cannot type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

/// The content behind one of a memory's evidence refs — Chapter 06's "every
/// retrieved memory opens its source", fetched rather than merely named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum MemoryEvidence {
    /// The session-ledger events the ref names, inclusive of both ends.
    Events { events: Vec<SessionEvent> },
    /// The stored artifact's bytes. Base64 for the same reason `PutArtifact`
    /// uses it: JSON framing has no byte-string scalar.
    Artifact {
        media_type: String,
        bytes_base64: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_view_round_trips_and_omits_its_empty_optionals() {
        let view = MemoryView {
            id: MemoryId::new(),
            scope: MemoryScope {
                tier: "repository".to_string(),
                key: Some("abc".to_string()),
            },
            class: "semantic".to_string(),
            statement: "the parser is generated".to_string(),
            structured_value: None,
            confidence: 0.8,
            observed_at: Utc::now(),
            sensitivity: DataClassification::Internal,
            supersedes: Vec::new(),
            evidence: vec!["events 1..3 of session 2000…".to_string()],
        };
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(!json.contains("structured_value"));
        assert!(!json.contains("supersedes"));
        let parsed: MemoryView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, view);
    }

    /// A tier this build does not know must land on `Unknown` rather than
    /// erroring the frame (RULE 1) — and `Unknown` must never be mistaken for a
    /// real tier by the daemon's mapping.
    #[test]
    fn an_unknown_scope_tier_deserializes_to_unknown() {
        let parsed: MemoryScopeTier =
            serde_json::from_str(r#"{"type":"Galaxy"}"#).expect("future tiers must parse");
        assert_eq!(parsed, MemoryScopeTier::Unknown);
    }
}

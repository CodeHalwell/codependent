//! Artifact references and data classification.
//!
//! Events never inline bulk content (Chapter 03): anything larger than a small
//! inline value is written to the content-addressed store and referenced by an
//! [`ArtifactRef`]. A client streams or pages the underlying blob separately.
//! The reference carries the blob's classification so display, export, and
//! model-routing checks can be made without opening it.

use serde::{Deserialize, Serialize};

use crate::ids::ArtifactId;

/// A pointer to a stored artifact plus the metadata needed to handle it safely.
///
/// `id` and `sha256` are deliberately independent: identical bytes dedup to one
/// blob (keyed by `sha256`) but every occurrence is its own `ArtifactRef` with
/// its own id and `sensitivity` (Chapter 14 / STEP 1.4). Classification checks
/// always read the ref in hand, never a row looked up by hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ArtifactRef {
    pub id: ArtifactId,
    /// IANA media type, e.g. `text/plain` or `application/json`.
    pub media_type: String,
    pub byte_length: u64,
    /// Lowercase hex SHA-256 of the blob's bytes (the content address).
    pub sha256: String,
    pub sensitivity: DataClassification,
}

/// How sensitive an artifact's contents are.
///
/// Ordered least to most restrictive; higher classifications gate model
/// routing, export, and display. A wire enum, so it is internally tagged and
/// carries an [`DataClassification::Unknown`] fallback for forward
/// compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum DataClassification {
    Public,
    Internal,
    Confidential,
    Secret,
    /// A classification a newer peer defined that this build does not know.
    /// Treated as at least as restrictive as `Secret` by receivers.
    #[serde(other)]
    Unknown,
}

impl DataClassification {
    /// A restrictiveness rank, least (0) to most. `Unknown` ranks above `Secret`
    /// so an unrecognized (newer) classification is treated as at least as
    /// restrictive as the strictest one this build knows. Use this to compare two
    /// classifications for gating checks (routing, export, off-device transfer).
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            DataClassification::Public => 0,
            DataClassification::Internal => 1,
            DataClassification::Confidential => 2,
            DataClassification::Secret => 3,
            DataClassification::Unknown => 4,
        }
    }

    /// Whether data at this classification may leave the device given a policy
    /// that permits everything up to and including `max_off_device`. More
    /// restrictive data than the policy allows stays local.
    ///
    /// An `Unknown` CEILING authorizes nothing. `Unknown` ranking strictest is
    /// right for data being classified — it keeps an unrecognized tag local —
    /// but the very same rank on the POLICY side made it the most permissive
    /// ceiling there is, because every named class is `<= 4`. Federation parses
    /// this column leniently (`store.rs`, `_ => Unknown`), so a single corrupt
    /// or newer-build policy row silently raised the publication bound to
    /// "publish Secret off-device". A value this build cannot read is not
    /// permission; it is the absence of one.
    ///
    /// The data side needs no separate guard: `Unknown` data against any named
    /// ceiling is already `4 <= 3`, which is false.
    #[must_use]
    pub fn allowed_off_device(self, max_off_device: DataClassification) -> bool {
        if matches!(max_off_device, DataClassification::Unknown) {
            return false;
        }
        self.rank() <= max_off_device.rank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ref() -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 4096,
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            sensitivity: DataClassification::Internal,
        }
    }

    #[test]
    fn artifact_ref_round_trips() {
        let original = sample_ref();
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: ArtifactRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn data_classification_round_trips() {
        for original in [
            DataClassification::Public,
            DataClassification::Internal,
            DataClassification::Confidential,
            DataClassification::Secret,
        ] {
            let json = serde_json::to_string(&original).expect("serialize");
            let parsed: DataClassification = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(original, parsed);
        }
    }

    #[test]
    fn unknown_classification_tag_deserializes_to_unknown() {
        let parsed: DataClassification =
            serde_json::from_value(serde_json::json!({ "type": "TopSecretFromTheFuture" }))
                .expect("unknown tag must parse, not error");
        assert!(matches!(parsed, DataClassification::Unknown));
    }

    /// A ceiling this build cannot read authorizes nothing.
    ///
    /// `Unknown` ranks strictest so unrecognized DATA stays local — correct —
    /// but the same rank made it the most permissive CEILING, admitting every
    /// named class including `Secret`. Federation maps any unrecognized policy
    /// string to `Unknown` (`federation/src/store.rs`), so one corrupt row
    /// lifted the publication bound entirely.
    #[test]
    fn an_unknown_ceiling_authorizes_nothing() {
        for data in [
            DataClassification::Public,
            DataClassification::Internal,
            DataClassification::Confidential,
            DataClassification::Secret,
            DataClassification::Unknown,
        ] {
            assert!(
                !data.allowed_off_device(DataClassification::Unknown),
                "{data:?} must not leave the device under an unreadable ceiling"
            );
        }

        // Named ceilings are untouched: this must fail CLOSED, not shut.
        assert!(DataClassification::Public.allowed_off_device(DataClassification::Internal));
        assert!(DataClassification::Internal.allowed_off_device(DataClassification::Internal));
        assert!(!DataClassification::Secret.allowed_off_device(DataClassification::Internal));
        // Unknown DATA still stays local against a named ceiling.
        assert!(!DataClassification::Unknown.allowed_off_device(DataClassification::Secret));
    }
}

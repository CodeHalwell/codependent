//! The LLM-extractor seam (M3a): a trait `harvest_memories` calls alongside
//! the pure heuristic/agent-tool producers, plus the zero-model-deps default.
//!
//! This module holds ONLY the trait + its input shape + the default
//! no-op implementation — no model client, no HTTP, no `agent-framework-rs`
//! dependency. The model-backed `LlmFactExtractor` lives in `crates/runtime`
//! (ADR-009: the only crate allowed to depend on `agent-framework-rs`) and
//! implements this trait from the outside, so `crates/knowledge` never learns
//! about models.
//!
//! `FactExtractor::extract` returns `Vec<CandidateMemory>`, never `Result`:
//! the never-fail contract is structural, not merely a convention harvest
//! happens to honor — an implementor CANNOT propagate an error even if it
//! wanted to, so a slow/broken/misconfigured model can never fail a run.

use chrono::{DateTime, Utc};
use codypendent_protocol::{ArtifactRef, DataClassification, RunId};

use crate::memory::CandidateMemory;
use crate::types::{Revision, Scope};

/// Bounded context handed to a [`FactExtractor`]: everything it needs to
/// distil a finished run into candidate facts, and nothing it would need to
/// reach back into the ledger/artifact store for. The impl (not this struct)
/// is responsible for bounding `transcript_excerpt` to its own input budget.
pub struct ExtractionInput<'a> {
    /// The run's objective, as recorded on the chronicle (`chronicle["objective"]`).
    pub objective: &'a str,
    /// The parsed `RunCompleted` chronicle artifact.
    pub chronicle: &'a serde_json::Value,
    /// A cheap join of the ledger's note/tool-observation texts, bounded by
    /// the implementation (tail kept) — never by the caller.
    pub transcript_excerpt: &'a str,
    /// The scope candidates are extracted under (session scope at harvest
    /// time; the caller re-anchors to repository scope afterward).
    pub scope: &'a Scope,
    /// The chronicle artifact each returned candidate should cite as
    /// provenance, so extraction never needs a session id to have evidence.
    pub chronicle_ref: &'a ArtifactRef,
    /// The run these facts were extracted from.
    pub run_id: RunId,
    /// When the extraction is happening (stamped onto each candidate).
    pub observed_at: DateTime<Utc>,
    /// The ledger revision each candidate is valid from.
    pub valid_from: Revision,
    /// The chronicle artifact's sensitivity, carried onto each candidate so
    /// `curate`'s redaction gate sees the right classification.
    pub sensitivity: DataClassification,
}

/// Distils bounded run context into candidate facts. Implementors MUST NOT
/// error the caller — any internal failure (model unavailable, timeout,
/// malformed response, ...) is swallowed and yields `vec![]`, exactly like a
/// heuristic extractor finding nothing. `harvest_memories` calls this
/// best-effort alongside the pure heuristic/agent-tool producers; a slow or
/// broken extractor degrades to "contributes nothing," never to "fails the
/// run."
#[async_trait::async_trait]
pub trait FactExtractor: Send + Sync {
    /// Best-effort: distil bounded run context into candidate facts. MUST NOT
    /// error the caller — returns `vec![]` on ANY internal failure.
    async fn extract(&self, input: ExtractionInput<'_>) -> Vec<CandidateMemory>;
}

/// The default extractor when no model is configured (or none is needed):
/// produces nothing. Zero model deps, so `crates/knowledge` never depends on
/// `agent-framework-rs`; `harvest_memories` behaves exactly as it did before
/// M3a when this is the injected extractor.
pub struct NoopExtractor;

#[async_trait::async_trait]
impl FactExtractor for NoopExtractor {
    async fn extract(&self, _input: ExtractionInput<'_>) -> Vec<CandidateMemory> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{ArtifactId, RunId};

    fn minimal_input<'a>(
        chronicle: &'a serde_json::Value,
        scope: &'a Scope,
        chronicle_ref: &'a ArtifactRef,
    ) -> ExtractionInput<'a> {
        ExtractionInput {
            objective: "fix the guard",
            chronicle,
            transcript_excerpt: "",
            scope,
            chronicle_ref,
            run_id: RunId::new(),
            observed_at: Utc::now(),
            valid_from: Revision::sequence(1),
            sensitivity: DataClassification::Internal,
        }
    }

    /// M3a: `NoopExtractor` is the seam's default — it contributes nothing,
    /// for any input, so injecting it leaves `harvest_memories` behaving
    /// exactly as before M3a.
    #[tokio::test]
    async fn noop_extractor_returns_empty_for_any_input() {
        let chronicle = serde_json::json!({"objective": "fix the guard"});
        let scope = Scope::System;
        let chronicle_ref = ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 0,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };

        let candidates = NoopExtractor
            .extract(minimal_input(&chronicle, &scope, &chronicle_ref))
            .await;

        assert!(candidates.is_empty());
    }

    /// Object-safety: `FactExtractor` must be usable as `&dyn FactExtractor`
    /// (the shape `harvest_with` is injected with) — this only needs to
    /// compile.
    #[tokio::test]
    async fn fact_extractor_is_object_safe() {
        let chronicle = serde_json::json!({});
        let scope = Scope::System;
        let chronicle_ref = ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 0,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };
        let extractor: &dyn FactExtractor = &NoopExtractor;
        let candidates = extractor
            .extract(minimal_input(&chronicle, &scope, &chronicle_ref))
            .await;
        assert!(candidates.is_empty());
    }
}

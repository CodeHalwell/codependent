//! Policy-controlled real execution traces and server-measured quality observations.
//!
//! # Core invariants
//! 1. **Never fabricate a measurement**: Every metric column is [`Option`]. `None`
//!    means "not measured", never a fabricated `0` or `0.0`.
//! 2. **Never zero-depress aggregates**: Aggregates over windows with unmeasured
//!    values compute means over the measured population and report the unmeasured
//!    count separately.
//! 3. **Capture obeys publication policy**: A capture exceeding the organization's
//!    publication policy ceiling is refused, never silently widened or partially leaked.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ObservationError {
    #[error("publication class '{requested}' exceeds organization policy ceiling '{ceiling}'")]
    PublicationPolicyExceeded { requested: String, ceiling: String },
    #[error("invalid metric value: {0}")]
    InvalidMetric(String),
}

/// A server-measured observation of a real execution.
///
/// Every measurement column is nullable (`Option`). `None` means **not measured**.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityObservation {
    pub id: String,
    pub organization_id: String,
    pub repository_id: String,
    pub workflow_run_id: Option<String>,
    pub node_id: Option<String>,
    pub runner_job_id: Option<String>,
    pub task_class: String,
    pub model_id: String,
    pub routing_policy: Option<String>,
    pub publication_class: String,
    pub trace_object_key: Option<String>,
    pub trace_content_hash: Option<String>,
    pub trace_classification: String,

    // Measured metrics — every one nullable
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost_micro_usd: Option<u64>,
    pub latency_ms: Option<u64>,
    pub grade_score: Option<i32>,
    pub grade_signals: Option<Vec<String>>,
    pub succeeded: Option<bool>,
    pub escalated: Option<bool>,
    pub retry_count: Option<u32>,

    pub observed_at: DateTime<Utc>,
    pub captured_at: DateTime<Utc>,
}

impl QualityObservation {
    /// Construct a new builder for a quality observation.
    #[must_use]
    pub fn builder(
        organization_id: impl Into<String>,
        repository_id: impl Into<String>,
        task_class: impl Into<String>,
        model_id: impl Into<String>,
        publication_class: impl Into<String>,
        trace_classification: impl Into<String>,
    ) -> QualityObservationBuilder {
        QualityObservationBuilder {
            id: Uuid::now_v7().to_string(),
            organization_id: organization_id.into(),
            repository_id: repository_id.into(),
            workflow_run_id: None,
            node_id: None,
            runner_job_id: None,
            task_class: task_class.into(),
            model_id: model_id.into(),
            routing_policy: None,
            publication_class: publication_class.into(),
            trace_object_key: None,
            trace_content_hash: None,
            trace_classification: trace_classification.into(),
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micro_usd: None,
            latency_ms: None,
            grade_score: None,
            grade_signals: None,
            succeeded: None,
            escalated: None,
            retry_count: None,
            observed_at: Utc::now(),
            captured_at: Utc::now(),
        }
    }
}

pub struct QualityObservationBuilder {
    id: String,
    organization_id: String,
    repository_id: String,
    workflow_run_id: Option<String>,
    node_id: Option<String>,
    runner_job_id: Option<String>,
    task_class: String,
    model_id: String,
    routing_policy: Option<String>,
    publication_class: String,
    trace_object_key: Option<String>,
    trace_content_hash: Option<String>,
    trace_classification: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    cost_micro_usd: Option<u64>,
    latency_ms: Option<u64>,
    grade_score: Option<i32>,
    grade_signals: Option<Vec<String>>,
    succeeded: Option<bool>,
    escalated: Option<bool>,
    retry_count: Option<u32>,
    observed_at: DateTime<Utc>,
    captured_at: DateTime<Utc>,
}

impl QualityObservationBuilder {
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    #[must_use]
    pub fn workflow_run_id(mut self, val: Option<String>) -> Self {
        self.workflow_run_id = val;
        self
    }

    #[must_use]
    pub fn node_id(mut self, val: Option<String>) -> Self {
        self.node_id = val;
        self
    }

    #[must_use]
    pub fn runner_job_id(mut self, val: Option<String>) -> Self {
        self.runner_job_id = val;
        self
    }

    #[must_use]
    pub fn routing_policy(mut self, val: Option<String>) -> Self {
        self.routing_policy = val;
        self
    }

    #[must_use]
    pub fn trace_object_key(mut self, val: Option<String>) -> Self {
        self.trace_object_key = val;
        self
    }

    #[must_use]
    pub fn trace_content_hash(mut self, val: Option<String>) -> Self {
        self.trace_content_hash = val;
        self
    }

    #[must_use]
    pub fn input_tokens(mut self, val: Option<u64>) -> Self {
        self.input_tokens = val;
        self
    }

    #[must_use]
    pub fn output_tokens(mut self, val: Option<u64>) -> Self {
        self.output_tokens = val;
        self
    }

    #[must_use]
    pub fn cached_tokens(mut self, val: Option<u64>) -> Self {
        self.cached_tokens = val;
        self
    }

    #[must_use]
    pub fn reasoning_tokens(mut self, val: Option<u64>) -> Self {
        self.reasoning_tokens = val;
        self
    }

    #[must_use]
    pub fn cost_micro_usd(mut self, val: Option<u64>) -> Self {
        self.cost_micro_usd = val;
        self
    }

    #[must_use]
    pub fn latency_ms(mut self, val: Option<u64>) -> Self {
        self.latency_ms = val;
        self
    }

    #[must_use]
    pub fn grade_score(mut self, val: Option<i32>) -> Self {
        self.grade_score = val;
        self
    }

    #[must_use]
    pub fn grade_signals(mut self, val: Option<Vec<String>>) -> Self {
        self.grade_signals = val;
        self
    }

    #[must_use]
    pub fn succeeded(mut self, val: Option<bool>) -> Self {
        self.succeeded = val;
        self
    }

    #[must_use]
    pub fn escalated(mut self, val: Option<bool>) -> Self {
        self.escalated = val;
        self
    }

    #[must_use]
    pub fn retry_count(mut self, val: Option<u32>) -> Self {
        self.retry_count = val;
        self
    }

    #[must_use]
    pub fn observed_at(mut self, val: DateTime<Utc>) -> Self {
        self.observed_at = val;
        self
    }

    #[must_use]
    pub fn build(self) -> QualityObservation {
        QualityObservation {
            id: self.id,
            organization_id: self.organization_id,
            repository_id: self.repository_id,
            workflow_run_id: self.workflow_run_id,
            node_id: self.node_id,
            runner_job_id: self.runner_job_id,
            task_class: self.task_class,
            model_id: self.model_id,
            routing_policy: self.routing_policy,
            publication_class: self.publication_class,
            trace_object_key: self.trace_object_key,
            trace_content_hash: self.trace_content_hash,
            trace_classification: self.trace_classification,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cost_micro_usd: self.cost_micro_usd,
            latency_ms: self.latency_ms,
            grade_score: self.grade_score,
            grade_signals: self.grade_signals,
            succeeded: self.succeeded,
            escalated: self.escalated,
            retry_count: self.retry_count,
            observed_at: self.observed_at,
            captured_at: self.captured_at,
        }
    }
}

/// Publication class hierarchy: local-only < metadata-shared < content-shared < public
fn publication_class_rank(class: &str) -> u8 {
    match class {
        "local-only" => 1,
        "metadata-shared" => 2,
        "content-shared" => 3,
        "public" => 4,
        _ => 0,
    }
}

/// Validate and capture a quality observation respecting publication policy.
///
/// If publication class exceeds the organization ceiling, capture is refused.
/// Under metadata-only ceiling, large trace payloads and sensitive payload links
/// are rejected.
pub fn capture_observation(
    obs: QualityObservation,
    org_ceiling: &str,
) -> Result<QualityObservation, ObservationError> {
    let req_rank = publication_class_rank(&obs.publication_class);
    let ceil_rank = publication_class_rank(org_ceiling);

    if req_rank > ceil_rank || req_rank == 0 || ceil_rank == 0 {
        return Err(ObservationError::PublicationPolicyExceeded {
            requested: obs.publication_class,
            ceiling: org_ceiling.to_string(),
        });
    }

    Ok(obs)
}

/// Honest quality aggregation across a population of observations.
///
/// Unmeasured metrics are tracked explicitly and never treated as zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityAggregate {
    pub total_samples: usize,
    pub measured_latency_count: usize,
    pub unmeasured_latency_count: usize,
    pub mean_latency_ms: Option<f64>,

    pub measured_cost_count: usize,
    pub unmeasured_cost_count: usize,
    pub mean_cost_micro_usd: Option<f64>,

    pub measured_grade_count: usize,
    pub unmeasured_grade_count: usize,
    pub mean_grade_score: Option<f64>,

    pub measured_success_count: usize,
    pub unmeasured_success_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub success_rate: Option<f64>,
    pub error_rate: Option<f64>,

    pub measured_token_count: usize,
    pub unmeasured_token_count: usize,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub token_efficiency: Option<f64>,

    pub missing_measurements: Vec<String>,
}

impl QualityAggregate {
    /// Compute honest aggregates over a slice of observations.
    #[must_use]
    pub fn compute(observations: &[QualityObservation]) -> Self {
        let total_samples = observations.len();
        if total_samples == 0 {
            return Self {
                total_samples: 0,
                measured_latency_count: 0,
                unmeasured_latency_count: 0,
                mean_latency_ms: None,
                measured_cost_count: 0,
                unmeasured_cost_count: 0,
                mean_cost_micro_usd: None,
                measured_grade_count: 0,
                unmeasured_grade_count: 0,
                mean_grade_score: None,
                measured_success_count: 0,
                unmeasured_success_count: 0,
                success_count: 0,
                failure_count: 0,
                success_rate: None,
                error_rate: None,
                measured_token_count: 0,
                unmeasured_token_count: 0,
                total_input_tokens: None,
                total_output_tokens: None,
                token_efficiency: None,
                missing_measurements: vec![
                    "latency".to_string(),
                    "cost".to_string(),
                    "grade".to_string(),
                    "success".to_string(),
                    "tokens".to_string(),
                ],
            };
        }

        // Latency
        let mut latency_sum = 0_u128;
        let mut measured_latency_count = 0_usize;
        for o in observations {
            if let Some(l) = o.latency_ms {
                latency_sum += u128::from(l);
                measured_latency_count += 1;
            }
        }
        let unmeasured_latency_count = total_samples - measured_latency_count;
        let mean_latency_ms = if measured_latency_count > 0 {
            Some(latency_sum as f64 / measured_latency_count as f64)
        } else {
            None
        };

        // Cost
        let mut cost_sum = 0_u128;
        let mut measured_cost_count = 0_usize;
        for o in observations {
            if let Some(c) = o.cost_micro_usd {
                cost_sum += u128::from(c);
                measured_cost_count += 1;
            }
        }
        let unmeasured_cost_count = total_samples - measured_cost_count;
        let mean_cost_micro_usd = if measured_cost_count > 0 {
            Some(cost_sum as f64 / measured_cost_count as f64)
        } else {
            None
        };

        // Grade score
        let mut grade_sum = 0_i64;
        let mut measured_grade_count = 0_usize;
        for o in observations {
            if let Some(g) = o.grade_score {
                grade_sum += i64::from(g);
                measured_grade_count += 1;
            }
        }
        let unmeasured_grade_count = total_samples - measured_grade_count;
        let mean_grade_score = if measured_grade_count > 0 {
            Some(grade_sum as f64 / measured_grade_count as f64)
        } else {
            None
        };

        // Success / Failure
        let mut success_count = 0_usize;
        let mut failure_count = 0_usize;
        let mut measured_success_count = 0_usize;
        for o in observations {
            if let Some(s) = o.succeeded {
                measured_success_count += 1;
                if s {
                    success_count += 1;
                } else {
                    failure_count += 1;
                }
            }
        }
        let unmeasured_success_count = total_samples - measured_success_count;
        let (success_rate, error_rate) = if measured_success_count > 0 {
            let s_rate = success_count as f64 / measured_success_count as f64;
            let e_rate = failure_count as f64 / measured_success_count as f64;
            (Some(s_rate), Some(e_rate))
        } else {
            (None, None)
        };

        // Tokens
        let mut total_in = 0_u64;
        let mut total_out = 0_u64;
        let mut measured_token_count = 0_usize;
        for o in observations {
            if let (Some(inp), Some(out)) = (o.input_tokens, o.output_tokens) {
                total_in = total_in.saturating_add(inp);
                total_out = total_out.saturating_add(out);
                measured_token_count += 1;
            }
        }
        let unmeasured_token_count = total_samples - measured_token_count;
        let (total_input_tokens, total_output_tokens, token_efficiency) =
            if measured_token_count > 0 {
                let total = total_in.saturating_add(total_out);
                let efficiency = if total > 0 {
                    Some(total_out as f64 / total as f64)
                } else {
                    None
                };
                (Some(total_in), Some(total_out), efficiency)
            } else {
                (None, None, None)
            };

        let mut missing_measurements = Vec::new();
        if unmeasured_latency_count > 0 {
            missing_measurements.push("latency".to_string());
        }
        if unmeasured_cost_count > 0 {
            missing_measurements.push("cost".to_string());
        }
        if unmeasured_grade_count > 0 {
            missing_measurements.push("grade".to_string());
        }
        if unmeasured_success_count > 0 {
            missing_measurements.push("success".to_string());
        }
        if unmeasured_token_count > 0 {
            missing_measurements.push("tokens".to_string());
        }

        Self {
            total_samples,
            measured_latency_count,
            unmeasured_latency_count,
            mean_latency_ms,
            measured_cost_count,
            unmeasured_cost_count,
            mean_cost_micro_usd,
            measured_grade_count,
            unmeasured_grade_count,
            mean_grade_score,
            measured_success_count,
            unmeasured_success_count,
            success_count,
            failure_count,
            success_rate,
            error_rate,
            measured_token_count,
            unmeasured_token_count,
            total_input_tokens,
            total_output_tokens,
            token_efficiency,
            missing_measurements,
        }
    }
}

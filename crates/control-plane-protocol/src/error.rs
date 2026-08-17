//! Standardized error models for the Control Plane API.
//!
//! Enforces uniform non-disclosure error shapes (§5.3): unauthorized and absent resources
//! return identical 404 not-found responses to eliminate existence oracles.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Standard wire error response returned by all Control Plane REST APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ControlPlaneError {
    /// Categorical error type (e.g. "not_found", "unauthorized", "validation_error", "conflict").
    #[serde(rename = "type")]
    pub error_type: String,
    /// Resource name if applicable (e.g. "repository", "session", "organization").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Human-readable error message.
    pub message: String,
    /// Optional machine-readable sub-code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Structured details for validation or debugging (sanitized of sensitive data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl ControlPlaneError {
    /// Generic Not Found error. Used uniformly for absent and unauthorized resources.
    #[must_use]
    pub fn not_found(resource: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_type: "not_found".to_string(),
            resource: Some(resource.into()),
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Unauthorized error (no valid credentials provided).
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            error_type: "unauthorized".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Forbidden error (authenticated principal lacks necessary privileges).
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            error_type: "forbidden".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Request validation error.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            error_type: "validation_error".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Conflict error (e.g. state precondition or idempotency mismatch).
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            error_type: "conflict".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Rate limited error.
    #[must_use]
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            error_type: "rate_limited".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }

    /// Revoked entity error (e.g. revoked daemon pairing or token).
    #[must_use]
    pub fn revoked(reason: impl Into<String>) -> Self {
        Self {
            error_type: "revoked".to_string(),
            resource: None,
            message: reason.into(),
            code: Some("REVOKED".to_string()),
            detail: None,
        }
    }

    /// Internal service error.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            error_type: "internal_error".to_string(),
            resource: None,
            message: message.into(),
            code: None,
            detail: None,
        }
    }
}

impl fmt::Display for ControlPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref res) = self.resource {
            write!(f, "{}: {} - {}", self.error_type, res, self.message)
        } else {
            write!(f, "{}: {}", self.error_type, self.message)
        }
    }
}

impl std::error::Error for ControlPlaneError {}

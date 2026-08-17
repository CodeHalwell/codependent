use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlPlaneError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {resource} - {message}")]
    Forbidden { resource: String, message: String },

    #[error("not found: {resource} - {message}")]
    NotFound { resource: String, message: String },

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("unprocessable entity: {0}")]
    UnprocessableEntity(String),

    #[error("idempotency conflict: {0}")]
    IdempotencyConflict(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("internal server error: {0}")]
    Internal(String),
}

impl ControlPlaneError {
    pub fn not_found(resource: impl Into<String>, message: impl Into<String>) -> Self {
        Self::NotFound {
            resource: resource.into(),
            message: message.into(),
        }
    }

    pub fn forbidden(resource: impl Into<String>, message: impl Into<String>) -> Self {
        // Design §5.3: Map inaccessible/forbidden resources to uniform NotFound (404)
        // to prevent tenant existence probing across organizational boundaries.
        Self::NotFound {
            resource: resource.into(),
            message: message.into(),
        }
    }
}

impl IntoResponse for ControlPlaneError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ControlPlaneError::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse {
                    r#type: "unauthorized".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::Forbidden { resource, message }
            | ControlPlaneError::NotFound { resource, message } => (
                StatusCode::NOT_FOUND,
                ErrorResponse {
                    r#type: "not_found".to_string(),
                    resource: Some(resource),
                    message,
                },
            ),
            ControlPlaneError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                ErrorResponse {
                    r#type: "bad_request".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::Conflict(msg) => (
                StatusCode::CONFLICT,
                ErrorResponse {
                    r#type: "conflict".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::UnprocessableEntity(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorResponse {
                    r#type: "unprocessable_entity".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::IdempotencyConflict(msg) => (
                StatusCode::CONFLICT,
                ErrorResponse {
                    r#type: "idempotency_conflict".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::Storage(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    r#type: "storage_error".to_string(),
                    resource: None,
                    message: msg,
                },
            ),
            ControlPlaneError::Database(msg) => {
                tracing::error!(database_error = %msg, "Database error encountered");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse {
                        r#type: "database_error".to_string(),
                        resource: None,
                        message: "internal database operation failed".to_string(),
                    },
                )
            }
            ControlPlaneError::Internal(msg) => {
                tracing::error!(internal_error = %msg, "Internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse {
                        r#type: "internal_error".to_string(),
                        resource: None,
                        message: "internal server error".to_string(),
                    },
                )
            }
        };

        (status, Json(body)).into_response()
    }
}

/// PostgreSQL `unique_violation`.
pub(crate) const SQLSTATE_UNIQUE_VIOLATION: &str = "23505";
/// PostgreSQL `foreign_key_violation`.
pub(crate) const SQLSTATE_FOREIGN_KEY_VIOLATION: &str = "23503";

/// Tables where a distinguishable "already exists" answer is an existence
/// oracle: the caller supplies the conflicting key itself, so a 409 proves that
/// *some other principal* already owns that key.
///
/// `user_identities` is unique on `(provider, issuer, subject)` — a tuple the
/// caller types into the link request. Answering 409 tells an attacker which
/// GitHub/OIDC accounts are already registered. Every such violation collapses
/// to the byte-identical refusal an unauthorized or absent record produces
/// (design §5.3: unauthorized and absent are indistinguishable).
const EXISTENCE_ORACLE_TABLES: &[&str] = &["user_identities"];

fn is_existence_oracle_table(table: Option<&str>) -> bool {
    table.is_some_and(|t| EXISTENCE_ORACLE_TABLES.contains(&t))
}

/// The single refusal returned for anything to do with an identity link that
/// the caller is not entitled to complete — absent, not permitted, or already
/// claimed by someone else. Kept in one place so the three cases cannot drift
/// apart into a distinguishable response.
pub fn identity_link_refused() -> ControlPlaneError {
    ControlPlaneError::NotFound {
        resource: "identity".to_string(),
        message: "identity cannot be linked".to_string(),
    }
}

impl From<sqlx::Error> for ControlPlaneError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => ControlPlaneError::NotFound {
                resource: "record".to_string(),
                message: "resource not found".to_string(),
            },
            sqlx::Error::Database(db_err) => {
                // Design §5.4: sanitize database errors so constraint names and
                // foreign keys do not leak schema or tenant details.
                //
                // Classification is by SQLSTATE, never by the driver's message
                // text: `message()` is localized by the server's `lc_messages`,
                // so substring matching on English words silently degrades every
                // constraint violation to a 500 on a non-English server.
                let code = db_err.code();
                match code.as_deref() {
                    Some(SQLSTATE_UNIQUE_VIOLATION) => {
                        if is_existence_oracle_table(db_err.table()) {
                            identity_link_refused()
                        } else {
                            ControlPlaneError::Conflict(
                                "resource already exists or conflicts".to_string(),
                            )
                        }
                    }
                    Some(SQLSTATE_FOREIGN_KEY_VIOLATION) => ControlPlaneError::NotFound {
                        resource: "referenced_resource".to_string(),
                        message: "referenced entity does not exist".to_string(),
                    },
                    // Fail closed: anything unclassified is an internal fault,
                    // not a client-shaped answer.
                    _ => ControlPlaneError::Database(db_err.message().to_string()),
                }
            }
            other => ControlPlaneError::Database(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existence_oracle_tables_are_matched_exactly() {
        assert!(is_existence_oracle_table(Some("user_identities")));
        assert!(!is_existence_oracle_table(Some("user_identities_backup")));
        assert!(!is_existence_oracle_table(Some("organizations")));
        assert!(!is_existence_oracle_table(None));
    }

    #[test]
    fn identity_link_refusal_is_byte_identical_to_an_absent_identity() {
        // The route layer refuses an unauthorized link with `forbidden`, which
        // collapses to NotFound. A unique violation must produce the same bytes,
        // or the difference is the oracle.
        let absent = ControlPlaneError::forbidden("identity", "identity cannot be linked");
        let claimed = identity_link_refused();
        match (absent, claimed) {
            (
                ControlPlaneError::NotFound {
                    resource: a_res,
                    message: a_msg,
                },
                ControlPlaneError::NotFound {
                    resource: c_res,
                    message: c_msg,
                },
            ) => {
                assert_eq!(a_res, c_res);
                assert_eq!(a_msg, c_msg);
            }
            other => panic!("identity refusals must both be NotFound, got {other:?}"),
        }
    }
}

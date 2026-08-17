use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Serialize)]
pub struct ReadyResponse {
    pub status: &'static str,
    pub database: bool,
    pub storage: bool,
}

pub async fn healthz() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let db_ready = state.store.is_ready().await;
    let storage_ready = true;

    if db_ready && storage_ready {
        (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ready",
                database: db_ready,
                storage: storage_ready,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "not_ready",
                database: db_ready,
                storage: storage_ready,
            }),
        )
    }
}

pub async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "crate": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

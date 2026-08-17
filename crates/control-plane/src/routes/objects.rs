use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::{AuthPrincipal, Principal},
    authz::{authorize_organization_action, Action},
    error::ControlPlaneError,
    state::AppState,
    store::PublishedObject,
};

#[derive(Debug, Deserialize)]
pub struct UploadObjectParams {
    pub repository_id: Option<Uuid>,
    pub media_type: Option<String>,
    pub class: Option<String>,
    pub expected_hash: Option<String>,
}

pub async fn upload_object(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PublishedObject>, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::UploadObject,
    )
    .await?;

    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let mut hasher = Sha256::new();
    hasher.update(&body);
    let calculated_hash = hasher.finalize().to_vec();
    let calculated_hex = hex::encode(&calculated_hash);

    // If client supplied expected hash (e.g. in header), verify it
    if let Some(expected) = headers
        .get("x-content-sha256")
        .and_then(|v| v.to_str().ok())
    {
        if !expected.eq_ignore_ascii_case(&calculated_hex) {
            return Err(ControlPlaneError::BadRequest(format!(
                "content hash mismatch: expected {expected}, calculated {calculated_hex}"
            )));
        }
    }

    let storage_key = format!("{org_id}/{calculated_hex}");
    state
        .storage
        .put_object(&storage_key, &body, &media_type)
        .await?;

    let now = Utc::now();
    let obj_id = Uuid::now_v7();

    let daemon_id = match principal {
        Principal::Daemon { daemon_id, .. } => Some(daemon_id),
        _ => None,
    };

    let published = PublishedObject {
        id: obj_id,
        organization_id: org_id,
        repository_id: None,
        content_hash: calculated_hash,
        byte_length: body.len() as i64,
        media_type,
        class: "metadata-shared".to_string(),
        encryption: "none".to_string(),
        state: "available".to_string(),
        uploaded_by_daemon: daemon_id,
        created_at: now,
    };

    let recorded = state.store.record_published_object(published).await?;

    Ok(Json(recorded))
}

#[derive(Debug, Deserialize)]
pub struct PresignRequest {
    pub key: String,
    pub method: String,
    pub expiry_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct PresignResponse {
    pub url: String,
    pub key: String,
    pub method: String,
}

pub async fn presign_object_url(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    Json(req): Json<PresignRequest>,
) -> Result<Json<PresignResponse>, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::DownloadObject,
    )
    .await?;

    let expiry_secs = req.expiry_secs.unwrap_or(3600);

    // The organization prefix is the only thing keeping one tenant's presigned
    // URLs out of another's bucket space, and `key` is request input. A relative
    // segment escapes that prefix ("../other-org/..."), so any key that is not a
    // plain relative path is refused rather than normalised.
    let requested_key = req.key.trim_start_matches('/');
    let key_is_traversable = requested_key.is_empty()
        || requested_key.contains('\\')
        || requested_key
            .split('/')
            .any(|segment| segment == ".." || segment == ".");
    if key_is_traversable {
        return Err(ControlPlaneError::BadRequest(
            "invalid object key".to_string(),
        ));
    }

    let scoped_key = format!("{org_id}/{requested_key}");

    let url = state
        .storage
        .generate_presigned_url(&scoped_key, &req.method, expiry_secs)
        .await?;

    Ok(Json(PresignResponse {
        url,
        key: scoped_key,
        method: req.method,
    }))
}

pub async fn download_object(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path((org_id, hash_hex)): Path<(Uuid, String)>,
    headers: HeaderMap,
) -> Result<Response, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::DownloadObject,
    )
    .await?;

    let hash_bytes = hex::decode(&hash_hex)
        .map_err(|_| ControlPlaneError::BadRequest("invalid hex hash".to_string()))?;

    // Check DB metadata
    let obj_meta = state
        .store
        .get_published_object(org_id, &hash_bytes)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

    if obj_meta.state != "available" {
        return Err(ControlPlaneError::not_found(
            "object",
            "object not available",
        ));
    }

    // Parse Range header if present
    let range = headers
        .get(header::RANGE)
        .and_then(|r| r.to_str().ok())
        .and_then(|r| {
            if let Some(bytes_range) = r.strip_prefix("bytes=") {
                let mut parts = bytes_range.split('-');
                let start: u64 = parts.next()?.parse().ok()?;
                let end: u64 = parts.next()?.parse().ok()?;
                Some((start, end))
            } else {
                None
            }
        });

    let storage_key = format!("{org_id}/{hash_hex}");
    let obj_data = state.storage.get_object(&storage_key, range).await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        obj_meta
            .media_type
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        obj_data.data.len().to_string().parse().unwrap(),
    );
    response_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    response_headers.insert("etag", format!("\"{}\"", hash_hex).parse().unwrap());

    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    Ok((status, response_headers, obj_data.data).into_response())
}

pub async fn get_object_metadata(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path((org_id, hash_hex)): Path<(Uuid, String)>,
) -> Result<Json<PublishedObject>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    let hash_bytes = hex::decode(&hash_hex)
        .map_err(|_| ControlPlaneError::BadRequest("invalid hex hash".to_string()))?;

    let obj = state
        .store
        .get_published_object(org_id, &hash_bytes)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

    if obj.state != "available" {
        return Err(ControlPlaneError::not_found("object", "object not found"));
    }

    Ok(Json(obj))
}

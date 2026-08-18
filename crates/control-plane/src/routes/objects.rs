//! Content-addressed object upload, download and metadata.
//!
//! Every response body here is the protocol's
//! [`WirePublishedObject`](codypendent_control_plane_protocol::object_storage::PublishedObject),
//! never the stored row. The row carries `content_hash` as `Vec<u8>`, which
//! serde renders as a JSON **array of integers** where the protocol and every
//! client expect a 64-character hex string, and carries `class`, `state` and
//! `encryption` as free-form text where the protocol has closed enums. Handing
//! the row straight to `Json` published all four divergences.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use codypendent_control_plane_protocol::{
    ids::{DaemonId, OrganizationId, PublishedObjectId, RepositoryId},
    object_storage::{ObjectEncryption, ObjectState, PublishedObject as WirePublishedObject},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    audit::digest_from_bytes,
    auth::{AuthPrincipal, Principal},
    authz::{authorize_organization_action, parse_publication_class, Action},
    error::ControlPlaneError,
    state::AppState,
    store::PublishedObject,
};

/// Decode a lifecycle state stored as text. An unrecognized value is
/// [`ObjectState::Unknown`], which [`ObjectState::is_readable`] refuses — never
/// a guess at the closest named state.
fn parse_object_state(raw: &str) -> ObjectState {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .unwrap_or(ObjectState::Unknown)
}

/// Decode an encryption mode stored as text. An unrecognized mode is
/// [`ObjectEncryption::Unknown`]: this build cannot say how the bytes are
/// wrapped, so it must not serve them.
fn parse_object_encryption(raw: &str) -> ObjectEncryption {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .unwrap_or(ObjectEncryption::Unknown)
}

/// Project a stored object row onto the wire type.
///
/// A negative `byte_length` is a corrupted row, not a zero-length object: it is
/// refused rather than coerced into a measurement that was never taken.
fn object_to_wire(row: PublishedObject) -> Result<WirePublishedObject, ControlPlaneError> {
    let byte_length = u64::try_from(row.byte_length).map_err(|_| {
        ControlPlaneError::Internal("stored object byte length is negative".to_string())
    })?;

    Ok(WirePublishedObject {
        id: PublishedObjectId::from_uuid(row.id),
        organization_id: OrganizationId::from_uuid(row.organization_id),
        repository_id: row.repository_id.map(RepositoryId::from_uuid),
        content_hash: digest_from_bytes(&row.content_hash)?,
        byte_length,
        media_type: row.media_type,
        class: parse_publication_class(&row.class),
        encryption: parse_object_encryption(&row.encryption),
        state: parse_object_state(&row.state),
        uploaded_by_daemon: row.uploaded_by_daemon.map(DaemonId::from_uuid),
        created_at: row.created_at,
    })
}

/// Canonical lowercase hex form of a content address taken from the request
/// path, or `None` when it is not a SHA-256 digest at all.
///
/// The same string addresses both the metadata row and the storage key, so it is
/// re-rendered from the decoded bytes: an uppercase digest previously matched the
/// row and then missed the object.
fn canonical_content_hash(raw: &str) -> Option<(Vec<u8>, String)> {
    let bytes = hex::decode(raw).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let canonical = hex::encode(&bytes);
    Some((bytes, canonical))
}

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
) -> Result<Json<WirePublishedObject>, ControlPlaneError> {
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

    Ok(Json(object_to_wire(recorded)?))
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

    let (hash_bytes, hash_hex) = canonical_content_hash(&hash_hex)
        .ok_or_else(|| ControlPlaneError::BadRequest("invalid object hash".to_string()))?;

    // Check DB metadata
    let obj_meta = state
        .store
        .get_published_object(org_id, &hash_bytes)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

    // An object still uploading, tombstoned, or in a state this build cannot
    // name is served exactly like one that does not exist. `is_readable` admits
    // `Available` only, so `Unknown` fails closed.
    if !parse_object_state(&obj_meta.state).is_readable() {
        return Err(ControlPlaneError::not_found("object", "object not found"));
    }

    // The bytes are only servable when this build knows how they are wrapped. An
    // unrecognized mode must not be handed out as though it were plaintext.
    if parse_object_encryption(&obj_meta.encryption) == ObjectEncryption::Unknown {
        return Err(ControlPlaneError::not_found("object", "object not found"));
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
) -> Result<Json<WirePublishedObject>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    let (hash_bytes, _) = canonical_content_hash(&hash_hex)
        .ok_or_else(|| ControlPlaneError::BadRequest("invalid object hash".to_string()))?;

    let obj = state
        .store
        .get_published_object(org_id, &hash_bytes)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

    if !parse_object_state(&obj.state).is_readable() {
        return Err(ControlPlaneError::not_found("object", "object not found"));
    }

    Ok(Json(object_to_wire(obj)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_control_plane_protocol::publication::PublicationClass;

    fn row(state: &str, encryption: &str, class: &str, byte_length: i64) -> PublishedObject {
        PublishedObject {
            id: Uuid::now_v7(),
            organization_id: Uuid::now_v7(),
            repository_id: None,
            content_hash: Sha256::digest(b"object").to_vec(),
            byte_length,
            media_type: "text/plain".to_string(),
            class: class.to_string(),
            encryption: encryption.to_string(),
            state: state.to_string(),
            uploaded_by_daemon: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_content_hash_is_published_as_hex_never_as_an_array_of_integers() {
        let wire = object_to_wire(row("available", "none", "metadata-shared", 6))
            .expect("a well-formed row must project");
        let json = serde_json::to_value(&wire).expect("the wire type must serialize");
        let hash = json
            .get("content_hash")
            .and_then(|h| h.as_str())
            .expect("content_hash must be a JSON string");
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hex::encode(Sha256::digest(b"object")));
        assert_eq!(json.get("class").unwrap(), "metadata-shared");
        assert_eq!(json.get("state").unwrap(), "available");
    }

    #[test]
    fn a_corrupt_row_is_refused_rather_than_measured_as_zero() {
        assert!(object_to_wire(row("available", "none", "metadata-shared", -1)).is_err());

        let mut short = row("available", "none", "metadata-shared", 6);
        short.content_hash = vec![0u8; 16];
        assert!(object_to_wire(short).is_err());
    }

    #[test]
    fn unrecognized_enum_values_never_decode_to_a_named_one() {
        let wire = object_to_wire(row("quarantined", "aes-siv", "galaxy-shared", 6))
            .expect("an unrecognized tag is projected, not refused");
        assert_eq!(wire.state, ObjectState::Unknown);
        assert!(!wire.state.is_readable(), "Unknown must never be served");
        assert_eq!(wire.encryption, ObjectEncryption::Unknown);
        assert_eq!(wire.class, PublicationClass::Unknown);
        assert!(
            !wire.class.allows_off_device(),
            "an unrecognized class must rank most-restrictive"
        );
    }

    #[test]
    fn only_a_full_sha256_addresses_an_object_and_it_is_canonicalized() {
        let hex_hash = hex::encode(Sha256::digest(b"object"));
        let (bytes, canonical) =
            canonical_content_hash(&hex_hash.to_uppercase()).expect("uppercase hex addresses");
        assert_eq!(canonical, hex_hash, "the storage key must be lowercase");
        assert_eq!(bytes.len(), 32);

        for bad in ["", "zz", &"ab".repeat(8), &"ab".repeat(64)] {
            assert!(
                canonical_content_hash(bad).is_none(),
                "{bad} is not a content address"
            );
        }
    }
}

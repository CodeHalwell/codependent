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
    // The verb decides what the URL's holder can DO, so the gate must follow
    // the verb: a PUT presign is an upload and takes the same Contributor
    // gate as `upload_object` — gating it as a read handed every Observer a
    // write into the org's bucket space, bypassing `upload_object`'s
    // content-hash check as well. Any other verb has no sibling route whose
    // gate it could mirror, so it is refused rather than mapped to the
    // weakest one.
    let action = match req.method.as_str() {
        "GET" => Action::DownloadObject,
        "PUT" => Action::UploadObject,
        _ => {
            return Err(ControlPlaneError::BadRequest(
                "presign method must be GET or PUT".to_string(),
            ));
        }
    };
    authorize_organization_action(state.store.as_ref(), &principal, org_id, action).await?;

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

    // The whole object is fetched even for a range request: a slice cannot be
    // checked against a whole-object digest, and serving unverified bytes from
    // a content-addressed store gives up the only guarantee it offers. The
    // range is cut out of the verified whole below.
    let storage_key = format!("{org_id}/{hash_hex}");
    let obj_data = state.storage.get_object(&storage_key, None).await?;

    let served = verify_and_slice(&hash_bytes, obj_data.data, range)?;

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
        served.body.len().to_string().parse().unwrap(),
    );
    response_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    response_headers.insert("etag", format!("\"{}\"", hash_hex).parse().unwrap());
    if let Some(content_range) = &served.content_range {
        response_headers.insert(
            header::CONTENT_RANGE,
            content_range
                .parse()
                .map_err(|_| ControlPlaneError::Internal("invalid content range".to_string()))?,
        );
    }

    Ok((served.status, response_headers, served.body).into_response())
}

/// What `download_object` is about to put on the wire.
struct ServedBytes {
    body: Vec<u8>,
    status: StatusCode,
    /// `Some` only for a 206, where RFC 9110 requires `Content-Range`.
    content_range: Option<String>,
}

/// Check the stored bytes against the address they are being served under, then
/// cut the requested range out of the verified whole.
///
/// The store is content-addressed: the digest in the request path *is* the
/// object's identity, so bytes that do not hash to it are not the object,
/// whatever the backend returned. A mismatch is server-side corruption or a
/// substituted object, never a client error and never something to serve
/// anyway. It reports as `Internal` rather than not-found because the caller is
/// authorized and the row exists — nothing about another tenant's namespace is
/// disclosed either way.
fn verify_and_slice(
    expected_hash: &[u8],
    data: Vec<u8>,
    range: Option<(u64, u64)>,
) -> Result<ServedBytes, ControlPlaneError> {
    let served_hash = Sha256::digest(&data);
    if served_hash.as_slice() != expected_hash {
        return Err(ControlPlaneError::Internal(
            "stored object failed content-hash verification".to_string(),
        ));
    }

    let total_len = data.len() as u64;
    let Some((start, end)) = range else {
        return Ok(ServedBytes {
            body: data,
            status: StatusCode::OK,
            content_range: None,
        });
    };

    if start >= total_len || start > end {
        return Err(ControlPlaneError::BadRequest(
            "invalid byte range requested".to_string(),
        ));
    }
    let end = end.min(total_len - 1);
    let body = data[start as usize..=end as usize].to_vec();

    Ok(ServedBytes {
        body,
        status: StatusCode::PARTIAL_CONTENT,
        content_range: Some(format!("bytes {start}-{end}/{total_len}")),
    })
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

    /// The point of a content-addressed store: bytes that do not hash to the
    /// address they were requested under are refused, not served. The driver
    /// used to compute this digest and throw it away.
    #[test]
    fn bytes_that_do_not_match_the_address_are_never_served() {
        let addressed = Sha256::digest(b"the real object").to_vec();

        let err = verify_and_slice(&addressed, b"substituted bytes".to_vec(), None)
            .err()
            .expect("a hash mismatch must refuse the download");
        assert!(matches!(err, ControlPlaneError::Internal(_)));

        // Truncation is a mismatch too, including the empty body a broken
        // backend is happiest to return.
        assert!(verify_and_slice(&addressed, b"the real objec".to_vec(), None).is_err());
        assert!(verify_and_slice(&addressed, Vec::new(), None).is_err());

        // And a range request cannot be used to dodge the check.
        assert!(verify_and_slice(&addressed, b"substituted bytes".to_vec(), Some((0, 3))).is_err());
    }

    /// Matching bytes are served whole, and a range is cut out of the verified
    /// whole with the `Content-Range` a 206 is required to carry.
    #[test]
    fn a_verified_object_serves_whole_and_by_range() {
        let body = b"Hello, codypendent".to_vec();
        let addressed = Sha256::digest(&body).to_vec();

        let whole = verify_and_slice(&addressed, body.clone(), None).expect("verified");
        assert_eq!(whole.body, body);
        assert_eq!(whole.status, StatusCode::OK);
        assert!(whole.content_range.is_none());

        let part = verify_and_slice(&addressed, body.clone(), Some((0, 4))).expect("verified");
        assert_eq!(part.body, b"Hello");
        assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.content_range.as_deref(), Some("bytes 0-4/18"));

        // An end past the last byte clamps rather than panicking.
        let tail = verify_and_slice(&addressed, body.clone(), Some((13, 9_999))).expect("verified");
        assert_eq!(tail.body, b"ndent");
        assert_eq!(tail.content_range.as_deref(), Some("bytes 13-17/18"));

        for bad in [(18, 20), (5, 4)] {
            let err = verify_and_slice(&addressed, body.clone(), Some(bad))
                .err()
                .expect("an unsatisfiable range is refused");
            assert!(matches!(err, ControlPlaneError::BadRequest(_)), "{bad:?}");
        }
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

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ControlPlaneError;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetadata {
    pub content_hash: Vec<u8>,
    pub byte_length: u64,
    pub media_type: String,
    pub class: String,
    pub encryption: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct ObjectData {
    pub metadata: ObjectMetadata,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait ObjectStorageDriver: Send + Sync {
    async fn put_object(
        &self,
        key: &str,
        data: &[u8],
        media_type: &str,
    ) -> Result<ObjectMetadata, ControlPlaneError>;

    async fn get_object(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
    ) -> Result<ObjectData, ControlPlaneError>;

    async fn delete_object(&self, key: &str) -> Result<(), ControlPlaneError>;

    async fn generate_presigned_url(
        &self,
        key: &str,
        method: &str,
        expiry_secs: u64,
    ) -> Result<String, ControlPlaneError>;

    async fn head_object(&self, key: &str) -> Result<ObjectMetadata, ControlPlaneError>;
}

// -----------------------------------------------------------------------------
// In-Memory Storage Driver (default for tests & offline operation)
// -----------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct MemoryStorageDriver {
    objects: RwLock<HashMap<String, (ObjectMetadata, Vec<u8>)>>,
}

impl MemoryStorageDriver {
    pub fn new() -> Self {
        Self {
            objects: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ObjectStorageDriver for MemoryStorageDriver {
    async fn put_object(
        &self,
        key: &str,
        data: &[u8],
        media_type: &str,
    ) -> Result<ObjectMetadata, ControlPlaneError> {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let content_hash = hasher.finalize().to_vec();

        let metadata = ObjectMetadata {
            content_hash,
            byte_length: data.len() as u64,
            media_type: media_type.to_string(),
            class: "metadata-shared".to_string(),
            encryption: "none".to_string(),
            state: "available".to_string(),
        };

        let mut store = self.objects.write().map_err(|e| {
            ControlPlaneError::Storage(format!("Memory storage write lock poisoned: {e}"))
        })?;

        store.insert(key.to_string(), (metadata.clone(), data.to_vec()));
        Ok(metadata)
    }

    async fn get_object(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
    ) -> Result<ObjectData, ControlPlaneError> {
        let store = self.objects.read().map_err(|e| {
            ControlPlaneError::Storage(format!("Memory storage read lock poisoned: {e}"))
        })?;

        let (metadata, data) = store
            .get(key)
            .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

        let sliced_data = match range {
            Some((start, end)) => {
                let start = start as usize;
                let end = (end as usize + 1).min(data.len());
                if start >= data.len() || start >= end {
                    return Err(ControlPlaneError::BadRequest(
                        "invalid byte range requested".to_string(),
                    ));
                }
                data[start..end].to_vec()
            }
            None => data.clone(),
        };

        Ok(ObjectData {
            metadata: metadata.clone(),
            data: sliced_data,
        })
    }

    async fn delete_object(&self, key: &str) -> Result<(), ControlPlaneError> {
        let mut store = self.objects.write().map_err(|e| {
            ControlPlaneError::Storage(format!("Memory storage write lock poisoned: {e}"))
        })?;

        store.remove(key);
        Ok(())
    }

    async fn generate_presigned_url(
        &self,
        key: &str,
        method: &str,
        expiry_secs: u64,
    ) -> Result<String, ControlPlaneError> {
        let expires_at = Utc::now().timestamp() + expiry_secs as i64;
        Ok(format!(
            "http://memory-storage.local/{key}?method={method}&expires={expires_at}"
        ))
    }

    async fn head_object(&self, key: &str) -> Result<ObjectMetadata, ControlPlaneError> {
        let store = self.objects.read().map_err(|e| {
            ControlPlaneError::Storage(format!("Memory storage read lock poisoned: {e}"))
        })?;

        let (metadata, _) = store
            .get(key)
            .ok_or_else(|| ControlPlaneError::not_found("object", "object not found"))?;

        Ok(metadata.clone())
    }
}

// -----------------------------------------------------------------------------
// S3 / MinIO Storage Driver (AWS SigV4)
// -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct S3StorageDriver {
    client: reqwest::Client,
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    use_path_style: bool,
}

impl S3StorageDriver {
    pub fn new(
        endpoint: Option<String>,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        use_path_style: bool,
    ) -> Self {
        let endpoint = endpoint.unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            bucket,
            region,
            access_key,
            secret_key,
            use_path_style,
        }
    }

    /// Build the object URL with an AWS-canonical path.
    ///
    /// The key is SigV4 URI-encoded here rather than left to `Url::parse`,
    /// which uses the WHATWG path escape set and leaves several reserved
    /// characters raw (`+ , : @ = & ! $ ' ( ) * ;`). S3 and MinIO both
    /// recompute the canonical URI by re-encoding
    /// the decoded key with the AWS rule, so a raw `+` in the request line was
    /// signed as `+` and verified as `%2B` — SignatureDoesNotMatch. Encoding
    /// once, here, makes the wire path and the canonical URI the same string.
    ///
    fn object_url(&self, key: &str) -> String {
        let key = aws_uri_encode(key.trim_start_matches('/'), false);
        if self.use_path_style {
            format!(
                "{}/{}/{}",
                self.endpoint,
                aws_uri_encode(&self.bucket, true),
                key
            )
        } else {
            format!("{}/{}", self.endpoint, key)
        }
    }

    /// Derive the SigV4 signing key for `date` (`yyyymmdd`).
    fn signing_key(&self, date: &str) -> Result<Vec<u8>, ControlPlaneError> {
        let k_secret = format!("AWS4{}", self.secret_key);
        let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes())?;
        let k_region = hmac_sha256(&k_date, self.region.as_bytes())?;
        let k_service = hmac_sha256(&k_region, b"s3")?;
        hmac_sha256(&k_service, b"aws4_request")
    }

    fn sign_string(&self, string_to_sign: &str, date: &str) -> Result<String, ControlPlaneError> {
        let k_signing = self.signing_key(date)?;
        Ok(hex::encode(hmac_sha256(
            &k_signing,
            string_to_sign.as_bytes(),
        )?))
    }

    fn sign_request(
        &self,
        method: &str,
        url: &reqwest::Url,
        payload_hash: &str,
        headers: &mut HeaderMap,
    ) -> Result<(), ControlPlaneError> {
        let datetime = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        self.sign_request_at(method, url, payload_hash, headers, &datetime)
    }

    /// SigV4 header authorization at a caller-supplied `yyyymmddThhmmssZ`
    /// timestamp. Split out from [`Self::sign_request`] so the canonical
    /// request can be pinned against published vectors instead of `Utc::now`.
    fn sign_request_at(
        &self,
        method: &str,
        url: &reqwest::Url,
        payload_hash: &str,
        headers: &mut HeaderMap,
        datetime: &str,
    ) -> Result<(), ControlPlaneError> {
        let date = &datetime[0..8];

        headers.insert(
            "x-amz-date",
            datetime
                .parse()
                .map_err(|e| ControlPlaneError::Storage(format!("invalid x-amz-date: {e}")))?,
        );
        headers.insert(
            "x-amz-content-sha256",
            payload_hash.parse().map_err(|e| {
                ControlPlaneError::Storage(format!("invalid x-amz-content-sha256: {e}"))
            })?,
        );

        // `object_url` already emits the AWS-canonical path, so this is the
        // canonical URI as-is; re-encoding it here would double-escape `%`.
        let canonical_uri = url.path();
        let canonical_query = url.query().unwrap_or("");

        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            canonical_host(url),
            payload_hash,
            datetime
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, canonical_uri, canonical_query, canonical_headers, signed_headers, payload_hash
        );

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let canonical_req_hash = hex::encode(hasher.finalize());

        let credential_scope = format!("{}/{}/s3/aws4_request", date, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime, credential_scope, canonical_req_hash
        );

        let signature = self.sign_string(&string_to_sign, date)?;

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, credential_scope, signed_headers, signature
        );

        headers.insert(
            "authorization",
            authorization.parse().map_err(|e| {
                ControlPlaneError::Storage(format!("invalid authorization header: {e}"))
            })?,
        );
        Ok(())
    }

    /// SigV4 query (presigned) authorization at a caller-supplied timestamp.
    fn presigned_url_at(
        &self,
        key: &str,
        method: &str,
        expiry_secs: u64,
        datetime: &str,
    ) -> Result<String, ControlPlaneError> {
        let date = &datetime[0..8];
        let credential_scope = format!("{}/{}/s3/aws4_request", date, self.region);

        let url_str = self.object_url(key);
        let mut url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let host = canonical_host(&url);
        // A lowercase method would be signed verbatim and rejected; the
        // canonical request carries the uppercase verb.
        let method = method.to_uppercase();

        // The canonical query is built by hand rather than with
        // `query_pairs_mut`, whose form-urlencoded escape set differs from the
        // SigV4 one (`+` for space, `%7E` for `~`, `*` left raw). The server
        // recomputes the canonical query with the SigV4 rule, so anything
        // form-encoded here fails to verify.
        let mut params: Vec<(String, String)> = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            (
                "X-Amz-Credential",
                format!("{}/{}", self.access_key, credential_scope),
            ),
            ("X-Amz-Date", datetime.to_string()),
            ("X-Amz-Expires", expiry_secs.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (aws_uri_encode(k, true), aws_uri_encode(&v, true)))
        .collect();
        // Sorted by encoded name, then encoded value.
        params.sort();

        let canonical_query = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_request = format!(
            "{}\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
            method,
            url.path(),
            canonical_query,
            host
        );

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let canonical_req_hash = hex::encode(hasher.finalize());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime, credential_scope, canonical_req_hash
        );

        let signature = self.sign_string(&string_to_sign, date)?;

        // Every byte here is unreserved or an uppercase percent-escape, so the
        // query escape set of `set_query` leaves the signed string untouched.
        url.set_query(Some(&format!(
            "{canonical_query}&X-Amz-Signature={signature}"
        )));

        Ok(url.to_string())
    }
}

/// SigV4 URI encoding: every byte outside the RFC 3986 unreserved set
/// (`A-Za-z0-9-._~`) becomes an uppercase percent-escape. `/` is preserved only
/// in path position (`encode_slash == false`).
fn aws_uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            b'/' if !encode_slash => out.push('/'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The host exactly as it goes on the wire.
///
/// hyper derives the `Host` header from the URI authority and omits the
/// scheme's default port; `Url::port` returns `None` in precisely that case, so
/// the two agree. Signing the bare `host_str` instead dropped `:9000` from
/// every MinIO canonical request and failed every one of them with
/// SignatureDoesNotMatch.
fn canonical_host(url: &reqwest::Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, ControlPlaneError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| ControlPlaneError::Storage(format!("HMAC key error: {e}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

#[async_trait]
impl ObjectStorageDriver for S3StorageDriver {
    async fn put_object(
        &self,
        key: &str,
        data: &[u8],
        media_type: &str,
    ) -> Result<ObjectMetadata, ControlPlaneError> {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let content_hash = hasher.finalize().to_vec();
        let payload_hash = hex::encode(&content_hash);

        let url_str = self.object_url(key);
        let url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            media_type
                .parse()
                .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
        );

        self.sign_request("PUT", &url, &payload_hash, &mut headers)?;

        let response = self
            .client
            .put(url)
            .headers(headers)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| ControlPlaneError::Storage(format!("S3 PUT failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ControlPlaneError::Storage(format!(
                "S3 PUT returned status {status}: {body}"
            )));
        }

        Ok(ObjectMetadata {
            content_hash,
            byte_length: data.len() as u64,
            media_type: media_type.to_string(),
            class: "metadata-shared".to_string(),
            encryption: "none".to_string(),
            state: "available".to_string(),
        })
    }

    async fn get_object(
        &self,
        key: &str,
        range: Option<(u64, u64)>,
    ) -> Result<ObjectData, ControlPlaneError> {
        let url_str = self.object_url(key);
        let url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let payload_hash = hex::encode(Sha256::digest([]));
        let mut headers = HeaderMap::new();
        if let Some((start, end)) = range {
            headers.insert(
                reqwest::header::RANGE,
                format!("bytes={start}-{end}").parse().unwrap(),
            );
        }

        self.sign_request("GET", &url, &payload_hash, &mut headers)?;

        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ControlPlaneError::Storage(format!("S3 GET failed: {e}")))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ControlPlaneError::not_found("object", "object not found"));
        }

        if !response.status().is_success() {
            return Err(ControlPlaneError::Storage(format!(
                "S3 GET returned error status: {}",
                response.status()
            )));
        }

        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ControlPlaneError::Storage(format!("Failed to read S3 body: {e}")))?;

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let content_hash = hasher.finalize().to_vec();

        Ok(ObjectData {
            metadata: ObjectMetadata {
                content_hash,
                byte_length: bytes.len() as u64,
                media_type,
                class: "metadata-shared".to_string(),
                encryption: "none".to_string(),
                state: "available".to_string(),
            },
            data: bytes.to_vec(),
        })
    }

    async fn delete_object(&self, key: &str) -> Result<(), ControlPlaneError> {
        let url_str = self.object_url(key);
        let url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let payload_hash = hex::encode(Sha256::digest([]));
        let mut headers = HeaderMap::new();
        self.sign_request("DELETE", &url, &payload_hash, &mut headers)?;

        let response = self
            .client
            .delete(url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ControlPlaneError::Storage(format!("S3 DELETE failed: {e}")))?;

        if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(ControlPlaneError::Storage(format!(
                "S3 DELETE returned error status: {}",
                response.status()
            )));
        }

        Ok(())
    }

    async fn generate_presigned_url(
        &self,
        key: &str,
        method: &str,
        expiry_secs: u64,
    ) -> Result<String, ControlPlaneError> {
        let datetime = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        self.presigned_url_at(key, method, expiry_secs, &datetime)
    }

    async fn head_object(&self, key: &str) -> Result<ObjectMetadata, ControlPlaneError> {
        let url_str = self.object_url(key);
        let url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let payload_hash = hex::encode(Sha256::digest([]));
        let mut headers = HeaderMap::new();
        self.sign_request("HEAD", &url, &payload_hash, &mut headers)?;

        let response = self
            .client
            .head(url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ControlPlaneError::Storage(format!("S3 HEAD failed: {e}")))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ControlPlaneError::not_found("object", "object not found"));
        }

        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let length = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(ObjectMetadata {
            content_hash: vec![],
            byte_length: length,
            media_type,
            class: "metadata-shared".to_string(),
            encryption: "none".to_string(),
            state: "available".to_string(),
        })
    }
}

#[cfg(test)]
mod sigv4_tests {
    use super::*;

    /// Expected values in this module were produced by botocore 1.43.73
    /// (`botocore.auth.S3SigV4Auth` / `S3SigV4QueryAuth`, service `s3`), an
    /// independent implementation of the SigV4 spec, at the fixed timestamp
    /// below. They are cross-implementation vectors, not values recorded from
    /// this driver.
    const TS: &str = "20240517T120000Z";
    const ACCESS_KEY: &str = "minioadmin";
    const SECRET_KEY: &str = "minioadminsecret";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn minio() -> S3StorageDriver {
        S3StorageDriver::new(
            Some("http://127.0.0.1:9000".to_string()),
            "codypendent".to_string(),
            "us-east-1".to_string(),
            ACCESS_KEY.to_string(),
            SECRET_KEY.to_string(),
            true,
        )
    }

    fn aws() -> S3StorageDriver {
        S3StorageDriver::new(
            Some("https://s3.eu-west-2.amazonaws.com".to_string()),
            "codypendent".to_string(),
            "eu-west-2".to_string(),
            ACCESS_KEY.to_string(),
            SECRET_KEY.to_string(),
            true,
        )
    }

    fn signature_of(
        driver: &S3StorageDriver,
        method: &str,
        key: &str,
        payload_hash: &str,
    ) -> String {
        let url = reqwest::Url::parse(&driver.object_url(key)).expect("driver URL must parse");
        let mut headers = HeaderMap::new();
        driver
            .sign_request_at(method, &url, payload_hash, &mut headers, TS)
            .expect("signing must succeed");
        headers
            .get("authorization")
            .expect("authorization header")
            .to_str()
            .expect("ascii")
            .to_string()
    }

    /// The MinIO case the port bug broke: the canonical `Host` is
    /// `127.0.0.1:9000`, exactly what hyper puts on the wire. Signing the bare
    /// `host_str` yields a different signature and MinIO answers
    /// SignatureDoesNotMatch to every request.
    #[test]
    fn a_minio_endpoint_on_a_nondefault_port_signs_the_port() {
        let auth = signature_of(
            &minio(),
            "GET",
            "11111111-1111-1111-1111-111111111111/deadbeef",
            EMPTY_SHA256,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=minioadmin/20240517/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=dc1d03d8a168102448c2941645fddcbc66e25811d007c396dfae700b9578f2c5"
        );
    }

    /// The mirror case: an https endpoint on the scheme's default port must
    /// sign the bare host, never `host:443`.
    #[test]
    fn a_default_port_endpoint_signs_the_bare_host() {
        let payload_hash = hex::encode(Sha256::digest(b"codypendent object bytes"));
        assert_eq!(
            payload_hash,
            "c93c5922272792b17b4e69cbedecd6939f20700e480ff841c9c04a90bdc431cc"
        );
        let auth = signature_of(&aws(), "PUT", "org/obj", &payload_hash);
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=minioadmin/20240517/eu-west-2/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=6ce4e9d1317b2e3061cd025de6a2505128d6db6d8e76782783d225377443bf51"
        );
    }

    /// A key carrying characters the WHATWG path escape set leaves raw. The
    /// request line and the canonical URI must both read `/…/a%2Bb%20c`; the
    /// vector is the signature over that canonical URI.
    #[test]
    fn a_key_with_reserved_characters_is_sigv4_uri_encoded() {
        let driver = minio();
        assert_eq!(
            driver.object_url("org/a+b c"),
            "http://127.0.0.1:9000/codypendent/org/a%2Bb%20c"
        );
        let auth = signature_of(&driver, "GET", "org/a+b c", EMPTY_SHA256);
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=minioadmin/20240517/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=e54ca40704a6bc890ffc3b801800f12b43e8de5116ddfd02ac0e9678618e18d4"
        );
    }

    /// Presigned URL against MinIO: SigV4-encoded, sorted canonical query and a
    /// port-bearing signed host.
    #[test]
    fn a_presigned_url_matches_the_reference_query_signature() {
        let url = minio()
            .presigned_url_at("org/obj.bin", "GET", 900, TS)
            .expect("presign must succeed");
        assert_eq!(
            url,
            "http://127.0.0.1:9000/codypendent/org/obj.bin\
             ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=minioadmin%2F20240517%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20240517T120000Z\
             &X-Amz-Expires=900\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=ea54718773fe59b7ee9ae23e6bef0919d23b71bd155ea107b25e3f5f5d2cb27a"
        );
    }

    /// The same for a PUT presign on a default-port https endpoint, and proof
    /// that a lowercase verb is signed as the canonical uppercase one rather
    /// than verbatim.
    #[test]
    fn a_presigned_put_signs_the_uppercase_verb() {
        let expected = "https://s3.eu-west-2.amazonaws.com/codypendent/org/obj.bin\
             ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=minioadmin%2F20240517%2Feu-west-2%2Fs3%2Faws4_request\
             &X-Amz-Date=20240517T120000Z\
             &X-Amz-Expires=3600\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=60b23fddaba285c3aac72619d0067100406b115e671d0b346eff4a72be91227a";
        let driver = aws();
        assert_eq!(
            driver
                .presigned_url_at("org/obj.bin", "PUT", 3600, TS)
                .unwrap(),
            expected
        );
        assert_eq!(
            driver
                .presigned_url_at("org/obj.bin", "put", 3600, TS)
                .unwrap(),
            expected
        );
    }

    /// The canonical query must use the SigV4 escape set, not the
    /// form-urlencoded one. This credential inverts the two: `~` is unreserved
    /// and must stay raw where `query_pairs_mut` writes `%7E`, and `*` must be
    /// escaped where `query_pairs_mut` leaves it raw.
    #[test]
    fn the_canonical_query_uses_the_sigv4_escape_set_not_form_encoding() {
        let driver = S3StorageDriver::new(
            Some("http://127.0.0.1:9000".to_string()),
            "codypendent".to_string(),
            "us-east-1".to_string(),
            "key~star*x".to_string(),
            SECRET_KEY.to_string(),
            true,
        );
        let url = driver
            .presigned_url_at("org/obj.bin", "GET", 900, TS)
            .expect("presign must succeed");
        assert_eq!(
            url,
            "http://127.0.0.1:9000/codypendent/org/obj.bin\
             ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=key~star%2Ax%2F20240517%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20240517T120000Z\
             &X-Amz-Expires=900\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=375116378aac8224b699ee669c4948b4f285ee420ee7254578ed2a0c4e41ccde"
        );
    }

    /// The encoding rule itself, against `botocore.utils.percent_encode`
    /// (`safe='-._~'`, plus `/` in path position).
    #[test]
    fn sigv4_uri_encoding_matches_the_reference_escape_set() {
        for (raw, path, query) in [
            ("org/obj.bin", "org/obj.bin", "org%2Fobj.bin"),
            ("a+b", "a%2Bb", "a%2Bb"),
            ("a b", "a%20b", "a%20b"),
            ("a~b", "a~b", "a~b"),
            ("a*b", "a%2Ab", "a%2Ab"),
            ("a%b", "a%25b", "a%25b"),
            ("a,b=c:d@e", "a%2Cb%3Dc%3Ad%40e", "a%2Cb%3Dc%3Ad%40e"),
            (
                "quote'()!$&;",
                "quote%27%28%29%21%24%26%3B",
                "quote%27%28%29%21%24%26%3B",
            ),
            ("ünïcode", "%C3%BCn%C3%AFcode", "%C3%BCn%C3%AFcode"),
        ] {
            assert_eq!(aws_uri_encode(raw, false), path, "path form of {raw}");
            assert_eq!(aws_uri_encode(raw, true), query, "query form of {raw}");
        }
    }

    /// `Url::port` and hyper agree on when a port appears in `Host`.
    #[test]
    fn the_canonical_host_tracks_the_wire_host() {
        for (url, expected) in [
            ("http://127.0.0.1:9000/b/k", "127.0.0.1:9000"),
            ("https://minio.internal:9000/b/k", "minio.internal:9000"),
            (
                "https://s3.eu-west-2.amazonaws.com/b/k",
                "s3.eu-west-2.amazonaws.com",
            ),
            (
                "https://s3.eu-west-2.amazonaws.com:443/b/k",
                "s3.eu-west-2.amazonaws.com",
            ),
            ("http://minio.internal:80/b/k", "minio.internal"),
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert_eq!(canonical_host(&parsed), expected, "{url}");
        }
    }
}

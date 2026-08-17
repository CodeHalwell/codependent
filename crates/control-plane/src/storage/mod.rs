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

    fn object_url(&self, key: &str) -> String {
        let key = key.trim_start_matches('/');
        if self.use_path_style {
            format!("{}/{}/{}", self.endpoint, self.bucket, key)
        } else {
            format!("{}/{}", self.endpoint, key)
        }
    }

    fn sign_request(
        &self,
        method: &str,
        url: &reqwest::Url,
        payload_hash: &str,
        headers: &mut HeaderMap,
    ) -> Result<(), ControlPlaneError> {
        let datetime = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let date = &datetime[0..8];

        headers.insert("x-amz-date", datetime.parse().unwrap());
        headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());

        let canonical_uri = url.path();
        let canonical_query = url.query().unwrap_or("");

        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            url.host_str().unwrap_or_default(),
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

        // Derive signing key
        let k_secret = format!("AWS4{}", self.secret_key);
        let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes())?;
        let k_region = hmac_sha256(&k_date, self.region.as_bytes())?;
        let k_service = hmac_sha256(&k_region, b"s3")?;
        let k_signing = hmac_sha256(&k_service, b"aws4_request")?;

        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes())?);

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, credential_scope, signed_headers, signature
        );

        headers.insert("authorization", authorization.parse().unwrap());
        Ok(())
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
        let date = &datetime[0..8];
        let credential_scope = format!("{}/{}/s3/aws4_request", date, self.region);

        let url_str = self.object_url(key);
        let mut url = reqwest::Url::parse(&url_str)
            .map_err(|e| ControlPlaneError::Storage(format!("Invalid S3 URL: {e}")))?;

        let host = url.host_str().unwrap_or_default().to_string();

        url.query_pairs_mut()
            .append_pair("X-Amz-Algorithm", "AWS4-HMAC-SHA256")
            .append_pair(
                "X-Amz-Credential",
                &format!("{}/{}", self.access_key, credential_scope),
            )
            .append_pair("X-Amz-Date", &datetime)
            .append_pair("X-Amz-Expires", &expiry_secs.to_string())
            .append_pair("X-Amz-SignedHeaders", "host");

        let canonical_query = url.query().unwrap_or_default();
        let canonical_uri = url.path();
        let canonical_headers = format!("host:{host}\n");
        let signed_headers = "host";

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\nUNSIGNED-PAYLOAD",
            method, canonical_uri, canonical_query, canonical_headers, signed_headers
        );

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let canonical_req_hash = hex::encode(hasher.finalize());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime, credential_scope, canonical_req_hash
        );

        let k_secret = format!("AWS4{}", self.secret_key);
        let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes())?;
        let k_region = hmac_sha256(&k_date, self.region.as_bytes())?;
        let k_service = hmac_sha256(&k_region, b"s3")?;
        let k_signing = hmac_sha256(&k_service, b"aws4_request")?;

        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes())?);

        url.query_pairs_mut()
            .append_pair("X-Amz-Signature", &signature);

        Ok(url.to_string())
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

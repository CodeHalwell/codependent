//! Ingestion orchestration: verify → dedup → normalize.
//!
//! [`WebhookIngestor::ingest`] enforces the ordering that makes ingestion safe:
//! signature verification happens **before** the body is parsed, normalization
//! succeeds before any replay key is consumed, and both delivery-id and signed
//! content replays are suppressed before any event is produced. Workflows are
//! triggered solely through an injected [`WebhookEventSink`] (Task 4.2).

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::store::DeliveryStore;
use super::{normalize, verify, WebhookError};

/// The GitHub headers ingestion depends on, extracted from the request.
#[derive(Debug, Clone, Default)]
pub struct DeliveryHeaders {
    /// The `X-Hub-Signature-256` value (`sha256=<hex>`), if present.
    pub signature: Option<String>,
    /// The `X-GitHub-Event` value.
    pub event_type: String,
    /// The `X-GitHub-Delivery` GUID.
    pub delivery_id: String,
    /// Optional endpoint ID from the URL path (`/webhooks/<endpoint_id>`).
    pub endpoint_id: Option<String>,
}

/// Trait for dispatching accepted, verified webhook deliveries.
#[async_trait::async_trait]
pub trait WebhookEventSink: Send + Sync {
    /// Handle an accepted delivery. Invoked ONLY after signature verification,
    /// normalization, and atomic replay reservation have all succeeded.
    async fn on_event(
        &self,
        endpoint_id: &str,
        delivery_id: &str,
        event_type: &str,
        event: &normalize::NormalizedEvent,
        raw_body: &[u8],
    ) -> Result<(), WebhookError>;
}

/// Configuration for an inbound webhook endpoint.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    pub endpoint_id: String,
    pub scheme: String,
    pub signing_key_ref: String,
    pub body_limit_bytes: usize,
    pub replay_window_seconds: u64,
}

/// Resolves endpoint configuration and signing key references.
#[async_trait::async_trait]
pub trait EndpointResolver: Send + Sync {
    /// Look up endpoint metadata by its `endpoint_id`.
    async fn resolve_endpoint(
        &self,
        endpoint_id: &str,
    ) -> Result<Option<EndpointConfig>, WebhookError>;
}

/// Resolves a `signing_key_ref` (e.g. `env:VAR_NAME` or literal string) to raw key bytes.
pub fn resolve_signing_key(signing_key_ref: &str) -> Option<Vec<u8>> {
    if let Some(var_name) = signing_key_ref.strip_prefix("env:") {
        std::env::var(var_name).ok().map(|s| s.into_bytes())
    } else if let Some(raw) = signing_key_ref.strip_prefix("raw:") {
        Some(raw.as_bytes().to_vec())
    } else if let Ok(val) = std::env::var(signing_key_ref) {
        Some(val.into_bytes())
    } else if !signing_key_ref.is_empty() {
        Some(signing_key_ref.as_bytes().to_vec())
    } else {
        None
    }
}

/// In-memory endpoint resolver for tests.
#[derive(Default, Clone)]
pub struct InMemoryEndpointResolver {
    endpoints: Arc<tokio::sync::RwLock<std::collections::HashMap<String, EndpointConfig>>>,
}

impl InMemoryEndpointResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, endpoint: EndpointConfig) {
        self.endpoints
            .write()
            .await
            .insert(endpoint.endpoint_id.clone(), endpoint);
    }
}

#[async_trait::async_trait]
impl EndpointResolver for InMemoryEndpointResolver {
    async fn resolve_endpoint(
        &self,
        endpoint_id: &str,
    ) -> Result<Option<EndpointConfig>, WebhookError> {
        Ok(self.endpoints.read().await.get(endpoint_id).cloned())
    }
}

/// Orchestrates verification, replay dedup, and normalization for webhook endpoints.
pub struct WebhookIngestor {
    store: Arc<dyn DeliveryStore>,
    /// Fallback default HMAC secret (e.g. from static config).
    default_secret: Option<Vec<u8>>,
    /// Optional sink for accepted deliveries.
    sink: Option<Arc<dyn WebhookEventSink>>,
    /// Optional resolver for per-endpoint signing keys and body limits.
    endpoint_resolver: Option<Arc<dyn EndpointResolver>>,
}

/// The outcome of ingesting one delivery.
#[derive(Debug, Clone, PartialEq)]
pub enum IngestOutcome {
    /// A secret is configured but the request carried no signature.
    SignatureMissing,
    /// The signature was present but did not verify.
    SignatureInvalid,
    /// The endpoint is not configured or disabled (refused identically to invalid signature).
    EndpointUnknown,
    /// The delivery GUID was already recorded; no event is produced.
    Duplicate,
    /// A fresh, authenticated delivery normalized into an event.
    Accepted { event: normalize::NormalizedEvent },
}

impl WebhookIngestor {
    /// Build an ingestor over `store`. `secret` is the default HMAC secret;
    /// `sink` receives accepted events.
    pub fn new(
        store: Arc<dyn DeliveryStore>,
        secret: Option<Vec<u8>>,
        sink: Option<Arc<dyn WebhookEventSink>>,
    ) -> Self {
        Self {
            store,
            default_secret: secret,
            sink,
            endpoint_resolver: None,
        }
    }

    /// Attach a per-endpoint configuration and key resolver.
    pub fn with_endpoint_resolver(mut self, resolver: Arc<dyn EndpointResolver>) -> Self {
        self.endpoint_resolver = Some(resolver);
        self
    }

    /// Return the maximum body bytes allowed for the given endpoint.
    pub async fn max_body_bytes(&self, endpoint_id: &str) -> usize {
        const DEFAULT_BODY_LIMIT: usize = 8 * 1024 * 1024;
        if let Some(resolver) = &self.endpoint_resolver {
            if let Ok(Some(config)) = resolver.resolve_endpoint(endpoint_id).await {
                return config.body_limit_bytes.min(DEFAULT_BODY_LIMIT);
            }
        }
        DEFAULT_BODY_LIMIT
    }

    /// Ingest one delivery using endpoint from headers (or `"default"`).
    pub async fn ingest(
        &self,
        headers: &DeliveryHeaders,
        body: &[u8],
    ) -> Result<IngestOutcome, WebhookError> {
        let endpoint_id = headers.endpoint_id.as_deref().unwrap_or("default");
        self.ingest_for_endpoint(endpoint_id, headers, body).await
    }

    /// Ingest one delivery for a specific `endpoint_id`.
    ///
    /// Verification runs before the body is parsed; deduplication runs before
    /// any event is dispatched to the sink.
    pub async fn ingest_for_endpoint(
        &self,
        endpoint_id: &str,
        headers: &DeliveryHeaders,
        body: &[u8],
    ) -> Result<IngestOutcome, WebhookError> {
        // 0. A delivery id is the dedup key: an empty one must never reach the
        // store, where it would be recorded once and then mark every later
        // id-less delivery a `Duplicate`.
        if headers.delivery_id.is_empty() || headers.event_type.is_empty() {
            return Err(WebhookError::Malformed(
                "missing X-GitHub-Delivery / X-GitHub-Event".to_string(),
            ));
        }

        // 1. Resolve signing key for endpoint.
        let secret: Vec<u8> = if let Some(resolver) = &self.endpoint_resolver {
            match resolver.resolve_endpoint(endpoint_id).await? {
                Some(config) => match resolve_signing_key(&config.signing_key_ref) {
                    Some(key) if !key.is_empty() => key,
                    _ => {
                        return Err(WebhookError::Config(
                            "signing key reference could not be resolved".into(),
                        ));
                    }
                },
                None => {
                    // Unknown endpoint: check if default secret is set and endpoint is "default"
                    if endpoint_id == "default" {
                        if let Some(sec) = self.default_secret.as_ref().filter(|s| !s.is_empty()) {
                            sec.clone()
                        } else {
                            return Err(WebhookError::Config(
                                "webhook secret is not configured".into(),
                            ));
                        }
                    } else {
                        return Ok(IngestOutcome::EndpointUnknown);
                    }
                }
            }
        } else {
            match self.default_secret.as_ref().filter(|s| !s.is_empty()) {
                Some(sec) => sec.clone(),
                None => {
                    return Err(WebhookError::Config(
                        "webhook secret is not configured".into(),
                    ));
                }
            }
        };

        let Some(signature) = &headers.signature else {
            return Ok(IngestOutcome::SignatureMissing);
        };
        if !verify::verify_signature(&secret, body, signature) {
            return Ok(IngestOutcome::SignatureInvalid);
        }

        // 2. Normalize only after authentication but before consuming a replay
        // key. Malformed payloads therefore cannot burn a delivery GUID.
        let event = normalize::normalize(&headers.event_type, body)?;

        // 3. Atomically reserve both replay identities: the delivery GUID and a
        // signature fingerprint that rejects the same authenticated content
        // under a forged new delivery/event header.
        let replay_key = format!("body-sha256:{}", hex::encode(Sha256::digest(signature)));
        if !self
            .store
            .reserve_if_new(&headers.delivery_id, &headers.event_type, &replay_key)
            .await?
        {
            return Ok(IngestOutcome::Duplicate);
        }

        // 4. Dispatch to the event sink if configured.
        if let Some(sink) = &self.sink {
            sink.on_event(
                endpoint_id,
                &headers.delivery_id,
                &headers.event_type,
                &event,
                body,
            )
            .await?;
        }

        Ok(IngestOutcome::Accepted { event })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::store::InMemoryDeliveryStore;
    use crate::webhook::verify::sign;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pull_request_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": "opened",
            "pull_request": { "number": 7 },
            "repository": { "full_name": "octocat/hello-world" }
        }))
        .expect("serialize fixture")
    }

    fn headers(delivery_id: &str, signature: Option<String>) -> DeliveryHeaders {
        DeliveryHeaders {
            signature,
            event_type: "pull_request".to_string(),
            delivery_id: delivery_id.to_string(),
            endpoint_id: None,
        }
    }

    struct CountingSink {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl WebhookEventSink for CountingSink {
        async fn on_event(
            &self,
            _endpoint_id: &str,
            _delivery_id: &str,
            _event_type: &str,
            _event: &normalize::NormalizedEvent,
            _raw_body: &[u8],
        ) -> Result<(), WebhookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn missing_secret_fails_closed() {
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, None, None);
        let error = ingestor
            .ingest(&headers("d1", None), &pull_request_body())
            .await
            .expect_err("unsigned mode must not exist");
        assert!(matches!(error, WebhookError::Config(_)));
    }

    #[tokio::test]
    async fn missing_signature_when_secret_set() {
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(b"topsecret".to_vec()), None);
        let outcome = ingestor
            .ingest(&headers("d1", None), &pull_request_body())
            .await
            .expect("ingest");
        assert_eq!(outcome, IngestOutcome::SignatureMissing);
    }

    #[tokio::test]
    async fn bad_signature_rejected() {
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(b"topsecret".to_vec()), None);
        let body = pull_request_body();
        let forged = sign(b"a different secret", &body);
        let outcome = ingestor
            .ingest(&headers("d1", Some(forged)), &body)
            .await
            .expect("ingest");
        assert_eq!(outcome, IngestOutcome::SignatureInvalid);
    }

    #[tokio::test]
    async fn forged_signature_on_unparseable_body_rejected_before_parse() {
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(b"topsecret".to_vec()), None);
        let body = b"this is not json {{{";
        let forged = sign(b"a different secret", body);
        let outcome = ingestor
            .ingest(&headers("d1", Some(forged)), body)
            .await
            .expect("ingest");
        assert_eq!(outcome, IngestOutcome::SignatureInvalid);
    }

    #[tokio::test]
    async fn empty_delivery_id_is_malformed_not_recorded() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(Arc::clone(&store) as _, Some(secret.to_vec()), None);
        let body = pull_request_body();
        let signature = sign(secret, &body);
        let err = ingestor
            .ingest(&headers("", Some(signature.clone())), &body)
            .await
            .expect_err("an empty delivery id must be malformed");
        assert!(matches!(err, WebhookError::Malformed(_)));
        let outcome = ingestor
            .ingest(&headers("d-real", Some(signature)), &body)
            .await
            .expect("ingest");
        assert!(matches!(outcome, IngestOutcome::Accepted { .. }));
    }

    #[tokio::test]
    async fn replay_is_duplicate() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(secret.to_vec()), None);
        let body = pull_request_body();
        let signature = sign(secret, &body);
        let first = ingestor
            .ingest(&headers("dup", Some(signature.clone())), &body)
            .await
            .expect("ingest");
        assert!(matches!(first, IngestOutcome::Accepted { .. }));
        let second = ingestor
            .ingest(&headers("dup", Some(signature)), &body)
            .await
            .expect("ingest");
        assert_eq!(second, IngestOutcome::Duplicate);
    }

    #[tokio::test]
    async fn a_signed_body_replayed_under_a_new_delivery_id_is_duplicate() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(secret.to_vec()), None);
        let body = pull_request_body();
        let signature = sign(secret, &body);
        let first = ingestor
            .ingest(&headers("delivery-1", Some(signature.clone())), &body)
            .await
            .unwrap();
        assert!(matches!(first, IngestOutcome::Accepted { .. }));
        let replay = ingestor
            .ingest(&headers("forged-new-id", Some(signature)), &body)
            .await
            .unwrap();
        assert_eq!(replay, IngestOutcome::Duplicate);
    }

    #[tokio::test]
    async fn malformed_payload_does_not_burn_the_delivery_id() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(secret.to_vec()), None);
        let malformed = b"not json";
        let error = ingestor
            .ingest(
                &headers("retry-me", Some(sign(secret, malformed))),
                malformed,
            )
            .await
            .expect_err("malformed payload is rejected");
        assert!(matches!(error, WebhookError::Malformed(_)));

        let valid = pull_request_body();
        let retry = ingestor
            .ingest(&headers("retry-me", Some(sign(secret, &valid))), &valid)
            .await
            .unwrap();
        assert!(matches!(retry, IngestOutcome::Accepted { .. }));
    }

    #[tokio::test]
    async fn accepted_delivery_invokes_sink_when_present() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let sink = Arc::new(CountingSink {
            calls: AtomicUsize::new(0),
        });
        let ingestor =
            WebhookIngestor::new(store, Some(secret.to_vec()), Some(Arc::clone(&sink) as _));
        let body = pull_request_body();
        let outcome = ingestor
            .ingest(&headers("d1", Some(sign(secret, &body))), &body)
            .await
            .expect("ingest");
        assert!(matches!(outcome, IngestOutcome::Accepted { .. }));
        assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn per_endpoint_resolution_works() {
        let resolver = Arc::new(InMemoryEndpointResolver::new());
        resolver
            .register(EndpointConfig {
                endpoint_id: "ep-alpha".to_string(),
                scheme: "hmac_sha256".to_string(),
                signing_key_ref: "raw:alpha-secret".to_string(),
                body_limit_bytes: 1048576,
                replay_window_seconds: 300,
            })
            .await;

        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, None, None).with_endpoint_resolver(resolver);

        let body = pull_request_body();
        let sig = sign(b"alpha-secret", &body);
        let mut hdrs = headers("d-alpha", Some(sig));
        hdrs.endpoint_id = Some("ep-alpha".to_string());

        let outcome = ingestor.ingest(&hdrs, &body).await.expect("ingest");
        assert!(matches!(outcome, IngestOutcome::Accepted { .. }));

        // Unknown endpoint returns EndpointUnknown
        let mut hdrs_unknown = headers("d-unknown", Some(sign(b"alpha-secret", &body)));
        hdrs_unknown.endpoint_id = Some("ep-unknown".to_string());
        let outcome_unknown = ingestor.ingest(&hdrs_unknown, &body).await.expect("ingest");
        assert_eq!(outcome_unknown, IngestOutcome::EndpointUnknown);
    }
}

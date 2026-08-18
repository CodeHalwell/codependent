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
    /// The URL path segment this endpoint answers on: `POST /webhooks/<id>`.
    pub endpoint_id: String,
    /// The declared signature scheme. Only [`SUPPORTED_SIGNATURE_SCHEME`] is
    /// verifiable here; any other value makes every delivery to this endpoint
    /// refuse (see [`WebhookIngestor::ingest_for_endpoint`]).
    pub scheme: String,
    /// An opaque REFERENCE to key material, never key material itself — the
    /// forms [`resolve_signing_key`] accepts. A reference that does not resolve
    /// refuses the delivery; it is never used as a key.
    pub signing_key_ref: String,
    /// The per-endpoint body ceiling, clamped by the listener's own hard
    /// ceiling. Governs ingestion through [`WebhookIngestor::max_body_bytes`].
    pub body_limit_bytes: usize,
    /// NOT ENFORCED as a time window, and deliberately so — see the module
    /// documentation. Nothing in this build compares a delivery timestamp
    /// against it, because an HMAC-SHA256 GitHub delivery signs only the body:
    /// any timestamp header would be attacker-supplied, and refusing on an
    /// unsigned timestamp is theatre. Replay is instead suppressed by the
    /// permanent content-fingerprint reservation in
    /// [`super::store::DeliveryStore::reserve_if_new`], which is strictly
    /// stronger than a window as long as nothing prunes `webhook_deliveries`
    /// (nothing does — there is no retention sweep in this repository).
    ///
    /// The only scheme for which the window would be load-bearing (`ed25519`,
    /// whose contract carries a signed timestamp) is refused outright rather
    /// than served with an unenforced window.
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

/// The only signature scheme this ingestor can actually verify — the one
/// [`super::verify::verify_signature`] implements. `automation_endpoints.scheme`
/// also admits `ed25519` (migration 0044), and no code here verifies it; an
/// endpoint declaring anything but this is REFUSED rather than verified with the
/// wrong algorithm under the same key reference.
pub const SUPPORTED_SIGNATURE_SCHEME: &str = "hmac_sha256";

/// The body ceiling applied when NO endpoint row governs the request: the
/// `automation_endpoints.body_limit_bytes` default from migration 0044
/// (1 MiB), not the listener's 8 MiB hard ceiling.
///
/// This exists so an UNREGISTERED endpoint can never be more permissive than a
/// registered one. Before it, `/webhooks/default` (and `/webhook`) fell back to
/// the global secret with the full 8 MiB allowance while a registered endpoint
/// got 1 MiB — registering an endpoint made ingestion *stricter*, which is
/// backwards. An operator who needs a larger body registers an endpoint and
/// says so explicitly (`codypendent webhook endpoint add --body-limit-bytes`).
pub const UNREGISTERED_BODY_LIMIT_BYTES: usize = 1_048_576;

/// Whether this ingestor can verify `scheme` at all.
///
/// Fail-closed on purpose: an unrecognized scheme is not "probably HMAC".
fn scheme_is_supported(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case(SUPPORTED_SIGNATURE_SCHEME)
}

/// Resolve a `signing_key_ref` to raw key bytes, or `None` when it names
/// something this build cannot resolve.
///
/// Only two forms resolve:
///
/// - `env:NAME` — the value of environment variable `NAME`.
/// - `raw:VALUE` — `VALUE` itself, an explicit opt-in for tests and for a
///   loopback endpoint whose key is genuinely inline.
///
/// Everything else — including the `keyring://…` / `secret://…` broker forms
/// migration 0044 documents but nothing in this build resolves — returns
/// `None`, and the caller refuses the delivery.
///
/// The previous implementation ended in two fallbacks: a bare env-var lookup,
/// and then "use the reference string itself as the key". That second one was a
/// fail-OPEN: `automation_endpoints.signing_key_ref` is a public column ("read
/// by ordinary queries, dumped in support bundles, and shown in CLI output"), so
/// an unresolvable `secret://webhooks/github` ref silently became an HMAC key
/// that anyone who can read the table — or a support bundle — can forge with.
pub fn resolve_signing_key(signing_key_ref: &str) -> Option<Vec<u8>> {
    if let Some(var_name) = signing_key_ref.strip_prefix("env:") {
        if var_name.is_empty() {
            return None;
        }
        let value = std::env::var(var_name).ok()?;
        (!value.is_empty()).then(|| value.into_bytes())
    } else if let Some(raw) = signing_key_ref.strip_prefix("raw:") {
        (!raw.is_empty()).then(|| raw.as_bytes().to_vec())
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

    /// The maximum body bytes allowed for `endpoint_id`.
    ///
    /// A registered endpoint gets its own `body_limit_bytes` (clamped by the
    /// listener's hard ceiling). Anything else — an unregistered endpoint, an
    /// endpoint whose scheme this build cannot verify, or a resolver that
    /// errored — gets [`UNREGISTERED_BODY_LIMIT_BYTES`], the *tighter* value, so
    /// that an unregistered endpoint is never more permissive than a registered
    /// one.
    pub async fn max_body_bytes(&self, endpoint_id: &str) -> usize {
        /// The listener's own hard ceiling, mirroring
        /// `super::server::MAX_BODY_BYTES` and the migration's CHECK.
        const HARD_CEILING: usize = 8 * 1024 * 1024;
        if let Some(resolver) = &self.endpoint_resolver {
            if let Ok(Some(config)) = resolver.resolve_endpoint(endpoint_id).await {
                if scheme_is_supported(&config.scheme) {
                    return config.body_limit_bytes.min(HARD_CEILING);
                }
            }
        }
        UNREGISTERED_BODY_LIMIT_BYTES
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
                Some(config) => {
                    // A scheme this build cannot verify must never be verified
                    // with the algorithm it *can* — that would accept an
                    // HMAC-SHA256 signature over a key reference registered for
                    // ed25519. Refused exactly as an unknown endpoint is, so the
                    // wire cannot tell the two apart.
                    if !scheme_is_supported(&config.scheme) {
                        tracing::warn!(
                            endpoint_id,
                            scheme = %config.scheme,
                            "webhook endpoint declares a signature scheme this build cannot \
                             verify; every delivery to it is refused"
                        );
                        return Ok(IngestOutcome::EndpointUnknown);
                    }
                    match resolve_signing_key(&config.signing_key_ref) {
                        Some(key) if !key.is_empty() => key,
                        _ => {
                            // A registered-but-unresolvable endpoint refuses
                            // identically to an absent one: returning a distinct
                            // error here would answer "does this endpoint
                            // exist?" for an unauthenticated caller (the server
                            // maps `Config` to 500 and `EndpointUnknown` to
                            // 401). The reason is logged, not answered.
                            tracing::warn!(
                                endpoint_id,
                                "webhook endpoint's signing_key_ref does not resolve to key \
                                 material; every delivery to it is refused"
                            );
                            return Ok(IngestOutcome::EndpointUnknown);
                        }
                    }
                }
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

        // 4. Dispatch to the event sink if configured. A dispatch that FAILS
        //    releases the reservation it just took: the caller answers 5xx, the
        //    sender redelivers the same GUID, and a reservation kept for an event
        //    that was never produced would answer that retry `Duplicate` — a 200
        //    for a delivery nothing ever acted on. The event would be lost
        //    permanently, with no signal anywhere.
        //
        //    Releasing only on the error path keeps replay protection intact: a
        //    delivery that WAS dispatched keeps both keys forever, so a replayed
        //    body — under its own GUID or a forged one — is still refused. The
        //    cost is that a sink which failed *after* acting sees its event again
        //    on the retry, so delivery is at-least-once for a failing sink rather
        //    than at-most-once; a retrying sender already requires that of its
        //    consumers, and losing the event outright is the worse failure.
        if let Some(sink) = &self.sink {
            if let Err(error) = sink
                .on_event(
                    endpoint_id,
                    &headers.delivery_id,
                    &headers.event_type,
                    &event,
                    body,
                )
                .await
            {
                // A release that itself fails leaves the identity consumed —
                // fail closed, and say so loudly, because that delivery now
                // needs a manual redrive.
                if let Err(release_error) =
                    self.store.release(&headers.delivery_id, &replay_key).await
                {
                    tracing::error!(
                        delivery_id = %headers.delivery_id,
                        %release_error,
                        "webhook dispatch failed AND its replay reservation could not be \
                         released; the sender's retry will be answered as a duplicate and this \
                         delivery will not be processed"
                    );
                }
                return Err(error);
            }
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

    /// A sink that fails once, then succeeds — the transient-failure shape a
    /// retrying sender exists for.
    struct FlakySink {
        calls: AtomicUsize,
        fail_first: usize,
    }

    #[async_trait::async_trait]
    impl WebhookEventSink for FlakySink {
        async fn on_event(
            &self,
            _endpoint_id: &str,
            _delivery_id: &str,
            _event_type: &str,
            _event: &normalize::NormalizedEvent,
            _raw_body: &[u8],
        ) -> Result<(), WebhookError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.fail_first {
                return Err(WebhookError::Config("sink is down".to_string()));
            }
            Ok(())
        }
    }

    /// A dispatch that FAILED must not consume the delivery's replay identity.
    ///
    /// Reserving before dispatch and keeping the reservation on failure lost the
    /// event permanently: the first attempt 5xx'd, GitHub redelivered the same
    /// GUID, and dedup answered `Duplicate` (a 200) without ever dispatching —
    /// no event, no error, nothing to alert on. The retry must dispatch and be
    /// `Accepted`; reverting the release makes both assertions fail.
    #[tokio::test]
    async fn a_failed_dispatch_does_not_consume_the_delivery_and_the_retry_is_processed() {
        let secret = b"topsecret";
        let store = Arc::new(InMemoryDeliveryStore::default());
        let sink = Arc::new(FlakySink {
            calls: AtomicUsize::new(0),
            fail_first: 1,
        });
        let ingestor = WebhookIngestor::new(
            Arc::clone(&store) as _,
            Some(secret.to_vec()),
            Some(Arc::clone(&sink) as _),
        );
        let body = pull_request_body();
        let signature = sign(secret, &body);

        let failed = ingestor
            .ingest(&headers("flaky-1", Some(signature.clone())), &body)
            .await
            .expect_err("a failing sink surfaces as an error the caller answers 5xx to");
        assert!(matches!(failed, WebhookError::Config(_)));

        // The sender retries the SAME delivery: it must be dispatched, not
        // silently acknowledged as a duplicate of an event that never happened.
        let retried = ingestor
            .ingest(&headers("flaky-1", Some(signature.clone())), &body)
            .await
            .expect("the retry is processed");
        assert!(
            matches!(retried, IngestOutcome::Accepted { .. }),
            "the retry of an undispatched delivery is accepted, got {retried:?}"
        );
        assert_eq!(
            sink.calls.load(Ordering::SeqCst),
            2,
            "the retry actually reached the sink"
        );

        // ...and the replay protection the reservation exists for is intact: now
        // that the delivery HAS been dispatched, a third copy is refused, under
        // its own GUID and under a forged one.
        assert_eq!(
            ingestor
                .ingest(&headers("flaky-1", Some(signature.clone())), &body)
                .await
                .unwrap(),
            IngestOutcome::Duplicate
        );
        assert_eq!(
            ingestor
                .ingest(&headers("forged-id", Some(signature)), &body)
                .await
                .unwrap(),
            IngestOutcome::Duplicate
        );
        assert_eq!(
            sink.calls.load(Ordering::SeqCst),
            2,
            "no replay reached the sink"
        );
    }

    /// A `signing_key_ref` this build cannot resolve must resolve to NOTHING —
    /// never to the reference string itself. The reference is a public column
    /// (it is shown by `codypendent webhook endpoint list` and lands in support
    /// bundles), so using it as key material hands out a forgeable secret.
    #[test]
    fn an_unresolvable_key_reference_yields_no_key() {
        assert_eq!(resolve_signing_key("raw:abc"), Some(b"abc".to_vec()));
        assert_eq!(resolve_signing_key("secret://webhooks/github"), None);
        assert_eq!(resolve_signing_key("keyring://codypendent/hook"), None);
        assert_eq!(
            resolve_signing_key("PATH"),
            None,
            "a bare name is not a key"
        );
        assert_eq!(resolve_signing_key(""), None);
        assert_eq!(resolve_signing_key("raw:"), None);
        assert_eq!(resolve_signing_key("env:"), None);
        assert_eq!(
            resolve_signing_key("env:CODYPENDENT_NO_SUCH_WEBHOOK_KEY_VAR"),
            None
        );
    }

    /// An endpoint registered for a scheme this build cannot verify must refuse
    /// every delivery — NOT fall through to HMAC-SHA256 under the same key
    /// reference — and must refuse indistinguishably from an absent endpoint.
    #[tokio::test]
    async fn an_unverifiable_scheme_refuses_like_an_absent_endpoint() {
        let resolver = Arc::new(InMemoryEndpointResolver::new());
        resolver
            .register(EndpointConfig {
                endpoint_id: "ep-ed25519".to_string(),
                scheme: "ed25519".to_string(),
                signing_key_ref: "raw:alpha-secret".to_string(),
                body_limit_bytes: 8 * 1024 * 1024,
                replay_window_seconds: 300,
            })
            .await;
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, None, None).with_endpoint_resolver(resolver);

        let body = pull_request_body();
        let mut hdrs = headers("d-ed", Some(sign(b"alpha-secret", &body)));
        hdrs.endpoint_id = Some("ep-ed25519".to_string());
        assert_eq!(
            ingestor.ingest(&hdrs, &body).await.expect("ingest"),
            IngestOutcome::EndpointUnknown
        );

        let mut absent = headers("d-absent", Some(sign(b"alpha-secret", &body)));
        absent.endpoint_id = Some("ep-absent".to_string());
        assert_eq!(
            ingestor.ingest(&absent, &body).await.expect("ingest"),
            IngestOutcome::EndpointUnknown,
            "an unverifiable endpoint and an absent one must be indistinguishable"
        );

        // …and it does not get the generous unregistered allowance either.
        assert_eq!(
            ingestor.max_body_bytes("ep-ed25519").await,
            UNREGISTERED_BODY_LIMIT_BYTES
        );
    }

    /// A registered endpoint whose key reference does not resolve refuses the
    /// same way an absent endpoint does — the status code must not answer
    /// "does this endpoint exist?" — and never verifies against the reference.
    #[tokio::test]
    async fn an_unresolvable_endpoint_key_refuses_like_an_absent_endpoint() {
        let resolver = Arc::new(InMemoryEndpointResolver::new());
        resolver
            .register(EndpointConfig {
                endpoint_id: "ep-broker".to_string(),
                scheme: "hmac_sha256".to_string(),
                signing_key_ref: "secret://webhooks/github".to_string(),
                body_limit_bytes: 1024,
                replay_window_seconds: 300,
            })
            .await;
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, None, None).with_endpoint_resolver(resolver);

        let body = pull_request_body();
        // Signed with the reference string itself: the old fallback would have
        // accepted this.
        let mut hdrs = headers("d-broker", Some(sign(b"secret://webhooks/github", &body)));
        hdrs.endpoint_id = Some("ep-broker".to_string());
        assert_eq!(
            ingestor.ingest(&hdrs, &body).await.expect("ingest"),
            IngestOutcome::EndpointUnknown
        );
    }

    /// The property this pass exists to hold: NOT registering an endpoint must
    /// never buy a bigger allowance than registering one.
    #[tokio::test]
    async fn an_unregistered_endpoint_is_never_more_permissive_than_a_registered_one() {
        let resolver = Arc::new(InMemoryEndpointResolver::new());
        resolver
            .register(EndpointConfig {
                endpoint_id: "ep-registered".to_string(),
                // The migration's own default for a registered endpoint.
                body_limit_bytes: 1_048_576,
                scheme: "hmac_sha256".to_string(),
                signing_key_ref: "raw:alpha-secret".to_string(),
                replay_window_seconds: 300,
            })
            .await;
        let store = Arc::new(InMemoryDeliveryStore::default());
        let ingestor = WebhookIngestor::new(store, Some(b"global-secret".to_vec()), None)
            .with_endpoint_resolver(resolver);

        let registered = ingestor.max_body_bytes("ep-registered").await;
        assert!(
            ingestor.max_body_bytes("default").await <= registered,
            "the global-secret fallback must not out-rank a registered endpoint"
        );
        assert!(ingestor.max_body_bytes("ep-absent").await <= registered);
        assert_eq!(
            ingestor.max_body_bytes("default").await,
            UNREGISTERED_BODY_LIMIT_BYTES
        );
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

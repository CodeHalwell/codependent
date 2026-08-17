//! Integration tests for webhook ingestion (Phase 3 STEP 3.3, Milestone 4 Task 4.2).
//!
//! Covers the security-critical invariants: a forged or unsigned delivery is
//! rejected before any event is produced; a redelivered GUID is idempotent;
//! path routing correctly routes to `/webhooks/<endpoint_id>`; per-endpoint
//! secret references and body limits are enforced; and accepted deliveries
//! are dispatched to the injected `WebhookEventSink`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use codypendent_integrations::webhook::ingest::{
    DeliveryHeaders, EndpointConfig, InMemoryEndpointResolver, IngestOutcome, WebhookEventSink,
    WebhookIngestor,
};
use codypendent_integrations::webhook::normalize::NormalizedEvent;
use codypendent_integrations::webhook::server;
use codypendent_integrations::webhook::store::{
    DeliveryStore, InMemoryDeliveryStore, SqliteDeliveryStore,
};
use codypendent_integrations::webhook::verify::sign;
use codypendent_integrations::webhook::WebhookError;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A small, valid `pull_request` payload.
fn pull_request_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "opened",
        "pull_request": { "number": 7 },
        "repository": { "full_name": "octocat/hello-world" }
    }))
    .expect("serialize fixture")
}

/// Open a migrated SQLite pool under a fresh tempdir.
async fn temp_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("webhooks.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("open pool");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    (dir, pool)
}

struct TestSink {
    calls: AtomicUsize,
    last_endpoint: tokio::sync::Mutex<Option<String>>,
    last_delivery_id: tokio::sync::Mutex<Option<String>>,
}

impl TestSink {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last_endpoint: tokio::sync::Mutex::new(None),
            last_delivery_id: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl WebhookEventSink for TestSink {
    async fn on_event(
        &self,
        endpoint_id: &str,
        delivery_id: &str,
        _event_type: &str,
        _event: &NormalizedEvent,
        _raw_body: &[u8],
    ) -> Result<(), WebhookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_endpoint.lock().await = Some(endpoint_id.to_string());
        *self.last_delivery_id.lock().await = Some(delivery_id.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn forged_signature_rejected() {
    let store = Arc::new(InMemoryDeliveryStore::default());
    let ingestor = WebhookIngestor::new(store, Some(b"correct-secret".to_vec()), None);
    let body = pull_request_body();

    // Signature computed under the WRONG secret.
    let forged = sign(b"wrong-secret", &body);
    let headers = DeliveryHeaders {
        signature: Some(forged),
        event_type: "pull_request".to_string(),
        delivery_id: "guid-forged".to_string(),
        endpoint_id: None,
    };
    let outcome = ingestor.ingest(&headers, &body).await.expect("ingest");
    assert_eq!(outcome, IngestOutcome::SignatureInvalid);

    // No signature at all when a secret is configured.
    let headers = DeliveryHeaders {
        signature: None,
        event_type: "pull_request".to_string(),
        delivery_id: "guid-unsigned".to_string(),
        endpoint_id: None,
    };
    let outcome = ingestor.ingest(&headers, &body).await.expect("ingest");
    assert_eq!(outcome, IngestOutcome::SignatureMissing);
}

#[tokio::test]
async fn replay_is_idempotent_sqlite() {
    let secret = b"sqlite-secret";
    let (_dir, pool) = temp_pool().await;
    let store = Arc::new(SqliteDeliveryStore::new(pool.clone()));
    let ingestor = WebhookIngestor::new(store, Some(secret.to_vec()), None);
    let body = pull_request_body();
    let headers = DeliveryHeaders {
        signature: Some(sign(secret, &body)),
        event_type: "pull_request".to_string(),
        delivery_id: "same-guid".to_string(),
        endpoint_id: None,
    };

    let first = ingestor.ingest(&headers, &body).await.expect("ingest");
    assert!(matches!(first, IngestOutcome::Accepted { .. }));

    let second = ingestor.ingest(&headers, &body).await.expect("ingest");
    assert_eq!(second, IngestOutcome::Duplicate);

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries WHERE delivery_id = ?")
            .bind("same-guid")
            .fetch_one(&pool)
            .await
            .expect("count rows");
    assert_eq!(count, 1);
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_deliveries")
        .fetch_one(&pool)
        .await
        .expect("count all replay keys");
    assert_eq!(total, 2, "the GUID and body fingerprint commit together");
}

#[tokio::test]
async fn duplicate_fingerprint_does_not_burn_a_fresh_guid_sqlite() {
    let (_dir, pool) = temp_pool().await;
    let store = SqliteDeliveryStore::new(pool);
    assert!(store
        .reserve_if_new("guid-1", "push", "body-1")
        .await
        .unwrap());
    assert!(!store
        .reserve_if_new("guid-2", "push", "body-1")
        .await
        .unwrap());
    assert!(
        store
            .reserve_if_new("guid-2", "push", "body-2")
            .await
            .unwrap(),
        "a rejected replay must insert neither key, leaving its fresh GUID reusable"
    );
}

#[tokio::test]
async fn delivery_accepted_and_invokes_sink() {
    let secret = b"policy-secret";
    let store = Arc::new(InMemoryDeliveryStore::default());
    let sink = Arc::new(TestSink::new());
    let ingestor = WebhookIngestor::new(store, Some(secret.to_vec()), Some(Arc::clone(&sink) as _));
    let body = pull_request_body();
    let headers = DeliveryHeaders {
        signature: Some(sign(secret, &body)),
        event_type: "pull_request".to_string(),
        delivery_id: "guid-policy".to_string(),
        endpoint_id: Some("ep-test".to_string()),
    };
    let outcome = ingestor.ingest(&headers, &body).await.expect("ingest");
    match outcome {
        IngestOutcome::Accepted { event } => {
            assert_eq!(
                event,
                NormalizedEvent::PullRequest {
                    action: "opened".to_string(),
                    number: 7,
                    repository: "octocat/hello-world".to_string(),
                }
            );
            assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
            assert_eq!(sink.last_endpoint.lock().await.as_deref(), Some("ep-test"));
            assert_eq!(
                sink.last_delivery_id.lock().await.as_deref(),
                Some("guid-policy")
            );
        }
        other => panic!("expected accepted, got {other:?}"),
    }
}

#[tokio::test]
async fn end_to_end_loopback() {
    let secret = b"loopback-secret".to_vec();
    let store = Arc::new(InMemoryDeliveryStore::default());
    let sink = Arc::new(TestSink::new());
    let ingestor = Arc::new(WebhookIngestor::new(
        store,
        Some(secret.clone()),
        Some(Arc::clone(&sink) as _),
    ));

    let listener = server::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = server::serve(listener, ingestor).await;
    });

    let body = pull_request_body();

    // A valid, signed delivery to /webhooks/my-endpoint is accepted (202).
    let signature = sign(&secret, &body);
    let status = send_post(
        addr,
        "/webhooks/my-endpoint",
        &signature,
        "guid-valid",
        &body,
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        sink.last_endpoint.lock().await.as_deref(),
        Some("my-endpoint")
    );

    // Legacy /webhook path defaults to "default" endpoint.
    let status = send_post(addr, "/webhook", &signature, "guid-valid-2", &body).await;
    assert_eq!(status, 202);
    assert_eq!(sink.calls.load(Ordering::SeqCst), 2);
    assert_eq!(sink.last_endpoint.lock().await.as_deref(), Some("default"));

    // A forged delivery is rejected (401).
    let forged = sign(b"not-the-secret", &body);
    let status = send_post(addr, "/webhooks/my-endpoint", &forged, "guid-bad", &body).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn per_endpoint_secrets_and_limits_over_http() {
    let resolver = Arc::new(InMemoryEndpointResolver::new());
    resolver
        .register(EndpointConfig {
            endpoint_id: "ep-small".to_string(),
            scheme: "hmac_sha256".to_string(),
            signing_key_ref: "raw:small-secret".to_string(),
            body_limit_bytes: 64, // 64 bytes max
            replay_window_seconds: 300,
        })
        .await;
    resolver
        .register(EndpointConfig {
            endpoint_id: "ep-large".to_string(),
            scheme: "hmac_sha256".to_string(),
            signing_key_ref: "raw:large-secret".to_string(),
            body_limit_bytes: 1048576,
            replay_window_seconds: 300,
        })
        .await;

    let store = Arc::new(InMemoryDeliveryStore::default());
    let ingestor =
        Arc::new(WebhookIngestor::new(store, None, None).with_endpoint_resolver(resolver));

    let listener = server::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = server::serve(listener, ingestor).await;
    });

    let body = pull_request_body(); // ~80 bytes

    // 1. Delivery to unknown endpoint returns 401 (not distinguished from bad signature).
    let sig = sign(b"large-secret", &body);
    let status = send_post(addr, "/webhooks/ep-unknown", &sig, "d-unk", &body).await;
    assert_eq!(status, 401);

    // 2. Delivery to ep-large with large-secret succeeds (202).
    let status = send_post(addr, "/webhooks/ep-large", &sig, "d-large", &body).await;
    assert_eq!(status, 202);

    // 3. Delivery to ep-small exceeds body limit (64 bytes) -> returns 413 Payload Too Large.
    let small_sig = sign(b"small-secret", &body);
    let status = send_post(addr, "/webhooks/ep-small", &small_sig, "d-small", &body).await;
    assert_eq!(status, 413);
}

/// Send a raw HTTP POST and return the numeric status code.
async fn send_post(
    addr: std::net::SocketAddr,
    path: &str,
    signature: &str,
    delivery_id: &str,
    body: &[u8],
) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Length: {}\r\n\
         X-Hub-Signature-256: {}\r\n\
         X-GitHub-Event: pull_request\r\n\
         X-GitHub-Delivery: {}\r\n\
         Connection: close\r\n\r\n",
        body.len(),
        signature,
        delivery_id,
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    stream.write_all(body).await.expect("write body");
    stream.flush().await.expect("flush");

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let text = String::from_utf8_lossy(&response);
    // Status line: "HTTP/1.1 <code> <reason>".
    text.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

//! Integration tests for the Tavily web-search client (PR C1), driven against
//! a `wiremock` mock of the search API.
//!
//! The focus is the contract the runtime tool relies on: the request shape
//! (`query` / `max_results` / `include_answer` plus the bearer key), the
//! tolerant response decode, and non-2xx responses mapping to a typed error
//! whose `Display` NEVER contains the key.

use codypendent_integrations::search::{SearchApi, SearchError, TavilyClient, TavilyKey};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENTINEL_KEY: &str = "tvly-SEKRET";

fn client(server: &MockServer) -> TavilyClient {
    TavilyClient::new(server.uri(), TavilyKey::new(SENTINEL_KEY)).expect("build client")
}

#[tokio::test]
async fn search_returns_the_answer_and_results() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/search"))
        // The request carries the bearer key and the documented body shape.
        .and(header("authorization", format!("Bearer {SENTINEL_KEY}")))
        .and(body_partial_json(serde_json::json!({
            "query": "rust async runtime",
            "max_results": 5,
            "include_answer": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "answer": "Tokio is the de-facto standard async runtime.",
            "results": [
                {
                    "title": "Tokio",
                    "url": "https://tokio.rs",
                    "content": "A runtime for writing reliable async applications.",
                    "score": 0.99
                },
                {
                    "title": "async-std",
                    "url": "https://async.rs",
                    "content": "An async version of the Rust standard library."
                    // no `score`: optional fields are tolerated
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = client(&server)
        .search("rust async runtime", 5)
        .await
        .expect("search succeeds");

    assert_eq!(
        outcome.answer.as_deref(),
        Some("Tokio is the de-facto standard async runtime.")
    );
    assert_eq!(outcome.results.len(), 2);
    assert_eq!(outcome.results[0].title, "Tokio");
    assert_eq!(outcome.results[0].url, "https://tokio.rs");
    assert_eq!(
        outcome.results[0].content,
        "A runtime for writing reliable async applications."
    );
    assert_eq!(outcome.results[1].title, "async-std");
}

#[tokio::test]
async fn search_tolerates_a_sparse_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            // no `answer`; a result with only a URL
            "results": [{ "url": "https://example.test" }]
        })))
        .mount(&server)
        .await;

    let outcome = client(&server).search("q", 3).await.expect("decodes");
    assert!(outcome.answer.is_none());
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.results[0].url, "https://example.test");
    assert!(outcome.results[0].title.is_empty());
    assert!(outcome.results[0].content.is_empty());
}

#[tokio::test]
async fn non_2xx_maps_to_an_api_error_that_never_contains_the_key() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "detail": { "error": "invalid API key" }
        })))
        .mount(&server)
        .await;

    let error = client(&server)
        .search("q", 5)
        .await
        .expect_err("a 401 is an error");
    match &error {
        SearchError::Api { status, message } => {
            assert_eq!(*status, 401);
            assert!(message.contains("invalid API key"), "{message}");
        }
        other => panic!("expected SearchError::Api, got {other:?}"),
    }
    let rendered = error.to_string();
    assert!(
        !rendered.contains(SENTINEL_KEY),
        "the key must never appear in an error: {rendered}"
    );
}

#[tokio::test]
async fn malformed_json_is_a_decode_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string("this is not json"))
        .mount(&server)
        .await;

    let error = client(&server)
        .search("q", 5)
        .await
        .expect_err("malformed JSON is an error");
    assert!(
        matches!(error, SearchError::Decode(_)),
        "expected SearchError::Decode, got {error:?}"
    );
    assert!(
        !error.to_string().contains(SENTINEL_KEY),
        "the key must never appear in an error"
    );
}

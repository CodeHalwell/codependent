//! Integration tests for the Hugging Face Hub discovery client (Unsloth local
//! models), driven against a `wiremock` mock of the Hub's `api/models`
//! surface. Response fixtures below are trimmed, faithful reproductions of
//! shapes captured live against the real Hub while building this module
//! (`unsloth/Qwen3-32B-GGUF` and friends) — not invented schemas.

use codypendent_integrations::unsloth::{HfCatalogApi, HfError, HfHubClient};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> HfHubClient {
    HfHubClient::new(server.uri()).expect("build client")
}

#[tokio::test]
async fn list_gguf_repos_parses_the_hub_listing_and_sends_the_documented_query() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("author", "unsloth"))
        .and(query_param("filter", "gguf"))
        .and(query_param("sort", "downloads"))
        .and(query_param("direction", "-1"))
        .and(query_param("limit", "10"))
        .and(query_param("full", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "_id": "688b451a53e70a07b0669a7c",
                "id": "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
                "author": "unsloth",
                "gated": false,
                "lastModified": "2026-01-30T06:29:38.000Z",
                "likes": 891,
                "private": false,
                "sha": "b17cb02dd882d5b6ab62fc777ad2995f19668350",
                "downloads": 6575381,
                "tags": ["transformers", "gguf", "unsloth"],
                "siblings": [{"rfilename": ".gitattributes"}]
            },
            {
                "id": "unsloth/Qwen3.5-4B-GGUF",
                "downloads": 1106071,
                "likes": 365
                // no lastModified: an older/partial response shape is tolerated
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let repos = client(&server)
        .list_gguf_repos("unsloth", 10)
        .await
        .expect("list succeeds");

    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].id, "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF");
    assert_eq!(repos[0].downloads, 6_575_381);
    assert_eq!(repos[0].likes, 891);
    assert_eq!(
        repos[0].updated_at.as_deref(),
        Some("2026-01-30T06:29:38.000Z")
    );
    assert_eq!(repos[1].id, "unsloth/Qwen3.5-4B-GGUF");
    assert_eq!(
        repos[1].updated_at, None,
        "a missing lastModified is None, never fabricated"
    );
}

#[tokio::test]
async fn list_gguf_repos_clamps_an_absurd_limit_instead_of_requesting_it_unbounded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let repos = client(&server)
        .list_gguf_repos("unsloth", 999_999)
        .await
        .expect("list succeeds");
    assert!(repos.is_empty());
}

#[tokio::test]
async fn list_quant_variants_groups_a_flat_and_nested_split_tree() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/models/unsloth/Qwen3-32B-GGUF/tree/main"))
        .and(query_param("recursive", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"type": "directory", "oid": "x", "size": 0, "path": "BF16"},
            {
                "type": "file", "oid": "a", "path": "BF16/Qwen3-32B-BF16-00001-of-00002.gguf",
                "size": 49_871_764_512_u64
            },
            {
                "type": "file", "oid": "b", "path": "BF16/Qwen3-32B-BF16-00002-of-00002.gguf",
                "size": 15_659_811_424_u64
            },
            {"type": "file", "oid": "c", "path": "Qwen3-32B-Q4_K_M.gguf", "size": 19_762_150_048_u64},
            {
                "type": "file", "oid": "d", "path": "Qwen3-32B-UD-Q4_K_XL.gguf",
                "size": 20_021_713_568_u64
            },
            {"type": "file", "oid": "e", "path": "README.md", "size": 4200}
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let variants = client(&server)
        .list_quant_variants("unsloth/Qwen3-32B-GGUF")
        .await
        .expect("tree fetch succeeds");

    let labels: Vec<&str> = variants.iter().map(|v| v.quant.as_str()).collect();
    assert!(labels.contains(&"Q4_K_M"));
    assert!(labels.contains(&"UD-Q4_K_XL"));
    assert!(labels.contains(&"BF16"));
    assert!(!labels.iter().any(|l| l.contains("README")));

    let bf16 = variants.iter().find(|v| v.quant == "BF16").unwrap();
    assert_eq!(bf16.files.len(), 2, "both split parts are present");
    assert_eq!(
        bf16.total_size_bytes,
        49_871_764_512 + 15_659_811_424,
        "the combined size is the sum of every split part"
    );

    // Smallest total size first (RAM-fit browsing order).
    let sizes: Vec<u64> = variants.iter().map(|v| v.total_size_bytes).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable();
    assert_eq!(sizes, sorted);
}

#[tokio::test]
async fn repo_metadata_reads_the_optional_gguf_context_length() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/unsloth/Qwen3-32B-GGUF"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "unsloth/Qwen3-32B-GGUF",
            "gguf": {
                "total": 32_762_123_456_u64,
                "architecture": "qwen3",
                "context_length": 40960
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let metadata = client(&server)
        .repo_metadata("unsloth/Qwen3-32B-GGUF")
        .await
        .expect("metadata fetch succeeds");
    assert_eq!(metadata.context_length, Some(40960));
}

#[tokio::test]
async fn repo_metadata_omits_context_length_when_the_repo_has_no_gguf_block() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/some-org/no-gguf-metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "some-org/no-gguf-metadata"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let metadata = client(&server)
        .repo_metadata("some-org/no-gguf-metadata")
        .await
        .expect("metadata fetch succeeds");
    assert_eq!(
        metadata.context_length, None,
        "an absent gguf block must never fabricate a context length"
    );
}

#[tokio::test]
async fn a_non_2xx_status_is_a_typed_api_error_naming_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/unsloth/does-not-exist/tree/main"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Repository not found"))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .list_quant_variants("unsloth/does-not-exist")
        .await
        .expect_err("a 404 must be a typed error, not a panic");
    match error {
        HfError::Api {
            status,
            url,
            message,
        } => {
            assert_eq!(status, 404);
            assert!(url.contains("unsloth/does-not-exist"));
            assert!(message.contains("not found") || message.contains("Not Found"));
        }
        other => panic!("expected HfError::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn a_malformed_body_is_a_typed_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .list_gguf_repos("unsloth", 10)
        .await
        .expect_err("malformed JSON must be a typed error, not a panic");
    assert!(matches!(error, HfError::Decode(_)));
}

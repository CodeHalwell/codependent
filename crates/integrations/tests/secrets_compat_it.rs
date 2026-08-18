//! Compatibility tests verifying that integration credentials use brokered
//! references (`env:<NAME>`), resolve secrets at call time, and never expose
//! secret material in Debug representations, logs, or error messages.

use codypendent_integrations::github::{GitHubError, GitHubToken};
use codypendent_integrations::mcp::load_mcp_config;
use codypendent_integrations::search::{SearchError, TavilyKey};
use codypendent_integrations::webhook::config::WebhooksConfig;

const TEST_SECRET: &str = "super-secret-token-12345";
const ROTATED_SECRET: &str = "rotated-secret-token-67890";

#[tokio::test]
async fn integration_credentials_resolve_through_the_broker() {
    let gh_var = "CODYPENDENT_TEST_SECRETS_COMPAT_GH_TOKEN";
    let tvly_var = "CODYPENDENT_TEST_SECRETS_COMPAT_TVLY_KEY";

    std::env::set_var(gh_var, TEST_SECRET);
    std::env::set_var(tvly_var, TEST_SECRET);

    // 1. GitHubToken with env: reference resolves at call time
    let gh_token = GitHubToken::from_reference(format!("env:{gh_var}"));
    assert_eq!(gh_token.reference(), Some(format!("env:{gh_var}").as_str()));
    assert_eq!(gh_token.expose(), TEST_SECRET);

    // 2. TavilyKey with env: reference resolves at call time
    let tvly_key = TavilyKey::from_reference(format!("env:{tvly_var}"));
    assert_eq!(
        tvly_key.reference(),
        Some(format!("env:{tvly_var}").as_str())
    );
    assert_eq!(tvly_key.expose(), TEST_SECRET);

    // 3. Dynamic rotation: updating the environment updates the resolved value at call time
    std::env::set_var(gh_var, ROTATED_SECRET);
    std::env::set_var(tvly_var, ROTATED_SECRET);
    assert_eq!(gh_token.expose(), ROTATED_SECRET);
    assert_eq!(tvly_key.expose(), ROTATED_SECRET);

    std::env::remove_var(gh_var);
    std::env::remove_var(tvly_var);
}

#[test]
fn secrets_are_never_exposed_in_debug_formatting() {
    let gh_literal = GitHubToken::new(TEST_SECRET);
    let gh_ref = GitHubToken::from_reference("env:GITHUB_TOKEN");
    let tvly_literal = TavilyKey::new(TEST_SECRET);
    let tvly_ref = TavilyKey::from_reference("env:TAVILY_API_KEY");
    let webhook_cfg = WebhooksConfig {
        enabled: true,
        listen_addr: "127.0.0.1:8765".into(),
        secret: Some(TEST_SECRET.into()),
        automation_dispatch: false,
    };

    let gh_lit_dbg = format!("{gh_literal:?}");
    let gh_ref_dbg = format!("{gh_ref:?}");
    let tvly_lit_dbg = format!("{tvly_literal:?}");
    let tvly_ref_dbg = format!("{tvly_ref:?}");
    let wh_dbg = format!("{webhook_cfg:?}");

    for (name, dbg) in [
        ("GitHubToken literal", &gh_lit_dbg),
        ("GitHubToken reference", &gh_ref_dbg),
        ("TavilyKey literal", &tvly_lit_dbg),
        ("TavilyKey reference", &tvly_ref_dbg),
        ("WebhooksConfig", &wh_dbg),
    ] {
        assert!(
            !dbg.contains(TEST_SECRET),
            "{name} leaked secret in Debug: {dbg}"
        );
        assert!(
            dbg.contains("redacted"),
            "{name} missing <redacted> in Debug: {dbg}"
        );
    }
}

#[test]
fn error_messages_never_expose_secret_values() {
    let missing_gh = GitHubError::MissingToken("GITHUB_TOKEN".into());
    let missing_tvly = SearchError::MissingKey("TAVILY_API_KEY".into());

    let gh_err = missing_gh.to_string();
    let tvly_err = missing_tvly.to_string();

    assert!(gh_err.contains("GITHUB_TOKEN"));
    assert!(!gh_err.contains(TEST_SECRET));

    assert!(tvly_err.contains("TAVILY_API_KEY"));
    assert!(!tvly_err.contains(TEST_SECRET));
}

#[test]
fn mcp_config_parses_env_references_without_literal_secrets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("mcp.toml");
    std::fs::write(
        &config_path,
        r#"
[[server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = [["GITHUB_TOKEN", "env:GITHUB_TOKEN"], ["CUSTOM_VAR", "env:MY_CUSTOM_SECRET"]]
"#,
    )
    .expect("write mcp.toml");

    let config = load_mcp_config(&config_path).expect("loads config");
    assert_eq!(config.servers.len(), 1);
    let server = &config.servers[0];
    assert_eq!(server.name, "github");
    assert_eq!(
        server.env,
        vec![
            ("GITHUB_TOKEN".to_string(), "env:GITHUB_TOKEN".to_string()),
            ("CUSTOM_VAR".to_string(), "env:MY_CUSTOM_SECRET".to_string())
        ]
    );
}

// NOTE: the counterpart assertion — that `JsonRpcClient::spawn` resolves an
// `env:` reference into the child's environment — lives in
// `crates/integrations/src/mcp/jsonrpc.rs`. `JsonRpcClient` is deliberately
// private (the module exposes only `McpBridge`), so that check belongs inside
// the crate rather than widening the public API for a test.

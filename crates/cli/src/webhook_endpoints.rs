//! `codypendent webhook endpoint …` — the writer for `automation_endpoints`.
//!
//! Migration 0044 gave every inbound webhook endpoint its own signing key
//! reference, body ceiling and replay window, and
//! [`SqliteDeliveryStore::resolve_endpoint`] reads them on every delivery. The
//! table had exactly ONE reader and **no writer anywhere in the repository**:
//! no INSERT existed in any crate, migration or fixture, so `resolve_endpoint`
//! returned `None` for every request that ever reached the listener and the
//! per-endpoint controls governed nothing. This module is that writer.
//!
//! It is an offline maintenance command in the shape of
//! [`crate::commands::models_bench`]: it opens the migrated database directly
//! (WAL + `busy_timeout` make the concurrent open with a live daemon safe)
//! because it writes a table the running daemon only reads. No restart is
//! needed — `resolve_endpoint` is a per-request SELECT, so the next delivery
//! after `add` is already governed by the new row.
//!
//! Two things this command refuses to do:
//!
//! - **Persist key material.** `signing_key_ref` is a public column (read by
//!   ordinary queries, dumped in support bundles, printed by `list`), so only
//!   an `env:NAME` reference is written — the NAME, never the value.
//! - **Register an unreachable id.** The id is round-tripped through the
//!   listener's own [`parse_endpoint_id`] before it is stored, so a row can
//!   never exist for a path the HTTP surface would not route to it.

use std::path::Path;

use anyhow::{bail, Context};
use codypendent_integrations::webhook::parse_endpoint_id;
use codypendent_protocol::discovery::RuntimePaths;

/// The listener's hard ceiling and the migration's `CHECK` upper bound.
const MAX_BODY_LIMIT_BYTES: i64 = 8 * 1024 * 1024;
/// The migration's own default, and the ceiling an UNREGISTERED endpoint gets
/// (`webhook::UNREGISTERED_BODY_LIMIT_BYTES`), so `add` with no explicit limit
/// changes only *who holds the key*, never how much is allowed through.
const DEFAULT_BODY_LIMIT_BYTES: i64 = 1_048_576;
/// The migration's default replay window. Stored for the audit trail and for a
/// future scheme that can enforce it; see [`EndpointConfig::replay_window_seconds`]
/// for why nothing enforces it today.
///
/// [`EndpointConfig::replay_window_seconds`]: codypendent_integrations::webhook::EndpointConfig::replay_window_seconds
const DEFAULT_REPLAY_WINDOW_SECONDS: i64 = 300;

/// Open the same database the daemon serves from.
async fn open(paths: &RuntimePaths) -> anyhow::Result<sqlx::SqlitePool> {
    std::fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("creating {}", paths.data_dir.display()))?;
    codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .context("opening the database to register a webhook endpoint")
}

/// The uid of the user invoking this command, read from a file this process
/// just created in the data directory.
///
/// A file's owner is the effective uid of the process that created it, so this
/// is exact — and it needs no `libc`/`unsafe` (the workspace denies
/// `unsafe_code`) and no new dependency. It is the same technique
/// `codypendentd`'s `daemon_uid` and the server's `daemon_uid_from_socket` use.
/// `automation_endpoints.owner_uid` must be a real uid: it is the audit answer
/// to "who registered this endpoint", and it scopes `list`/`rotate`/`disable`.
fn invoking_uid(data_dir: &Path) -> anyhow::Result<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let probe = tempfile::Builder::new()
            .prefix(".codypendent-uid-probe")
            .tempfile_in(data_dir)
            .with_context(|| format!("creating a uid probe file in {}", data_dir.display()))?;
        let uid = probe
            .as_file()
            .metadata()
            .context("reading the uid probe file's owner")?
            .uid();
        Ok(uid)
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        bail!("webhook endpoints are owned by a Unix uid; this platform has none")
    }
}

/// Validate an endpoint id against the listener that will have to route it.
///
/// The id is a URL path segment, so it is checked against the real router
/// ([`parse_endpoint_id`]) rather than against a guess about it: if
/// `/webhooks/<id>` does not parse back to exactly `<id>`, the row would be
/// unreachable and is refused. `default` is allowed on purpose — registering it
/// is how an operator upgrades the legacy `/webhook` path from the global
/// `webhooks.toml` secret to a governed endpoint.
pub fn validate_endpoint_id(endpoint_id: &str) -> anyhow::Result<()> {
    if endpoint_id.is_empty() || endpoint_id.len() > 64 {
        bail!("endpoint id must be 1-64 characters");
    }
    if !endpoint_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!(
            "endpoint id `{endpoint_id}` must use only ASCII letters, digits, `-`, `_` and `.` — \
             it is a URL path segment"
        );
    }
    match parse_endpoint_id(&format!("/webhooks/{endpoint_id}")) {
        Some(routed) if routed == endpoint_id => Ok(()),
        _ => bail!(
            "the listener would not route POST /webhooks/{endpoint_id} to `{endpoint_id}`; \
             refusing to register an endpoint no delivery can reach"
        ),
    }
}

/// Build the stored reference for an environment-variable-held key.
///
/// Only the NAME is stored. The form is one of exactly two the ingestor's own
/// `resolve_signing_key` accepts, so this command cannot write a reference the
/// verifier would refuse (which would fail every delivery closed — correctly,
/// but for a reason the operator never asked for).
pub fn key_ref_from_env_name(name: &str) -> anyhow::Result<String> {
    if name.is_empty() {
        bail!("--key-env needs an environment variable NAME");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.starts_with(|c: char| c.is_ascii_digit())
    {
        bail!("`{name}` is not a usable environment variable name");
    }
    // `env:NAME` is one of exactly two forms `resolve_signing_key` accepts (the
    // other, `raw:VALUE`, is key material and is deliberately unreachable from
    // this command). The unit test below pins the round trip.
    Ok(format!("env:{name}"))
}

/// Validate a per-endpoint body ceiling against the migration's `CHECK`.
pub fn validate_body_limit(bytes: i64) -> anyhow::Result<i64> {
    if bytes <= 0 || bytes > MAX_BODY_LIMIT_BYTES {
        bail!("--body-limit-bytes must be between 1 and {MAX_BODY_LIMIT_BYTES}");
    }
    Ok(bytes)
}

/// Validate a replay window against the migration's `CHECK`.
pub fn validate_replay_window(seconds: i64) -> anyhow::Result<i64> {
    if seconds <= 0 {
        bail!("--replay-window-seconds must be positive");
    }
    Ok(seconds)
}

/// `codypendent webhook endpoint add <id> --key-env NAME`.
pub async fn add(
    paths: &RuntimePaths,
    endpoint_id: &str,
    key_env: &str,
    body_limit_bytes: Option<i64>,
    replay_window_seconds: Option<i64>,
) -> anyhow::Result<()> {
    validate_endpoint_id(endpoint_id)?;
    let signing_key_ref = key_ref_from_env_name(key_env)?;
    let body_limit = validate_body_limit(body_limit_bytes.unwrap_or(DEFAULT_BODY_LIMIT_BYTES))?;
    let replay_window =
        validate_replay_window(replay_window_seconds.unwrap_or(DEFAULT_REPLAY_WINDOW_SECONDS))?;

    let owner_uid = invoking_uid(&paths.data_dir)?;
    let pool = open(paths).await?;
    insert_endpoint(
        &pool,
        endpoint_id,
        owner_uid,
        &signing_key_ref,
        body_limit,
        replay_window,
    )
    .await?;

    println!(
        "registered webhook endpoint `{endpoint_id}` (owner uid {owner_uid})\n  \
         path: POST /webhooks/{endpoint_id}\n  \
         scheme: {scheme}\n  \
         signing key: {signing_key_ref} (the NAME is stored; the value never is)\n  \
         body limit: {body_limit} bytes\n  \
         replay window: {replay_window} s (recorded, NOT enforced as a time window — \
         replays are suppressed by permanent delivery/content fingerprints)",
        scheme = codypendent_integrations::webhook::SUPPORTED_SIGNATURE_SCHEME,
    );
    if std::env::var(key_env)
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        eprintln!(
            "webhook endpoint add: WARNING — `{key_env}` is not set in this shell. It must be set \
             in the DAEMON's environment, or every delivery to `{endpoint_id}` is refused (401, \
             indistinguishable from an unknown endpoint) and the reason is logged to daemon.log."
        );
    }
    pool.close().await;
    Ok(())
}

/// `codypendent webhook endpoint list`.
pub async fn list(paths: &RuntimePaths) -> anyhow::Result<()> {
    let owner_uid = invoking_uid(&paths.data_dir)?;
    let pool = open(paths).await?;
    let rows = select_endpoints(&pool, owner_uid).await?;
    if rows.is_empty() {
        println!(
            "no webhook endpoints registered for uid {owner_uid}. Deliveries to \
             /webhooks/<id> are refused; only /webhook and /webhooks/default are served, under \
             the webhooks.toml secret and a {DEFAULT_BODY_LIMIT_BYTES}-byte body ceiling."
        );
    }
    for row in rows {
        let state = match (row.disabled_at.as_deref(), row.rotated_at.as_deref()) {
            (Some(at), _) => format!("disabled {at}"),
            (None, Some(at)) => format!("active (key rotated {at})"),
            (None, None) => "active".to_string(),
        };
        println!(
            "{id}\t{state}\tscheme={scheme}\tkey={key}\tbody_limit={body}\treplay_window={window}s\tcreated={created}",
            id = row.endpoint_id,
            scheme = row.scheme,
            key = row.signing_key_ref,
            body = row.body_limit_bytes,
            window = row.replay_window_seconds,
            created = row.created_at,
        );
    }
    pool.close().await;
    Ok(())
}

/// `codypendent webhook endpoint rotate <id> --key-env NAME`.
pub async fn rotate(paths: &RuntimePaths, endpoint_id: &str, key_env: &str) -> anyhow::Result<()> {
    validate_endpoint_id(endpoint_id)?;
    let signing_key_ref = key_ref_from_env_name(key_env)?;
    let owner_uid = invoking_uid(&paths.data_dir)?;
    let pool = open(paths).await?;
    let changed = rotate_endpoint(&pool, endpoint_id, owner_uid, &signing_key_ref).await?;
    pool.close().await;
    if !changed {
        bail!("no such webhook endpoint: `{endpoint_id}`");
    }
    println!("rotated `{endpoint_id}` to {signing_key_ref}");
    Ok(())
}

/// `codypendent webhook endpoint disable <id>`.
pub async fn disable(paths: &RuntimePaths, endpoint_id: &str) -> anyhow::Result<()> {
    validate_endpoint_id(endpoint_id)?;
    let owner_uid = invoking_uid(&paths.data_dir)?;
    let pool = open(paths).await?;
    let changed = disable_endpoint(&pool, endpoint_id, owner_uid).await?;
    pool.close().await;
    if !changed {
        bail!("no such webhook endpoint: `{endpoint_id}`");
    }
    println!(
        "disabled `{endpoint_id}`. Deliveries to POST /webhooks/{endpoint_id} are now refused \
         (401, indistinguishable from an endpoint that never existed)."
    );
    Ok(())
}

/// One `automation_endpoints` row, as `list` prints it.
#[derive(Debug, sqlx::FromRow)]
pub struct EndpointRow {
    pub endpoint_id: String,
    pub scheme: String,
    pub signing_key_ref: String,
    pub body_limit_bytes: i64,
    pub replay_window_seconds: i64,
    pub created_at: String,
    pub rotated_at: Option<String>,
    pub disabled_at: Option<String>,
}

/// The INSERT, split out so a test drives it against a temp migrated database.
pub async fn insert_endpoint(
    pool: &sqlx::SqlitePool,
    endpoint_id: &str,
    owner_uid: u32,
    signing_key_ref: &str,
    body_limit_bytes: i64,
    replay_window_seconds: i64,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO automation_endpoints \
         (endpoint_id, owner_uid, scheme, signing_key_ref, body_limit_bytes, \
          replay_window_seconds, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(endpoint_id)
    .bind(i64::from(owner_uid))
    .bind(codypendent_integrations::webhook::SUPPORTED_SIGNATURE_SCHEME)
    .bind(signing_key_ref)
    .bind(body_limit_bytes)
    .bind(replay_window_seconds)
    .bind(&now)
    .execute(pool)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            bail!(
                "webhook endpoint `{endpoint_id}` is already registered; use \
                 `codypendent webhook endpoint rotate` to change its key or `disable` to retire it"
            )
        }
        Err(error) => Err(anyhow::Error::new(error).context("registering the webhook endpoint")),
    }
}

/// The owner-scoped listing.
pub async fn select_endpoints(
    pool: &sqlx::SqlitePool,
    owner_uid: u32,
) -> anyhow::Result<Vec<EndpointRow>> {
    sqlx::query_as::<_, EndpointRow>(
        "SELECT endpoint_id, scheme, signing_key_ref, body_limit_bytes, replay_window_seconds, \
                created_at, rotated_at, disabled_at \
         FROM automation_endpoints WHERE owner_uid = ? ORDER BY endpoint_id",
    )
    .bind(i64::from(owner_uid))
    .fetch_all(pool)
    .await
    .context("listing webhook endpoints")
}

/// Rotate the key reference. Returns `false` when the row does not exist OR
/// belongs to another uid — the caller reports both as "no such endpoint", so
/// this is not an oracle for another user's endpoint ids.
pub async fn rotate_endpoint(
    pool: &sqlx::SqlitePool,
    endpoint_id: &str,
    owner_uid: u32,
    signing_key_ref: &str,
) -> anyhow::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE automation_endpoints SET signing_key_ref = ?, rotated_at = ? \
         WHERE endpoint_id = ? AND owner_uid = ?",
    )
    .bind(signing_key_ref)
    .bind(&now)
    .bind(endpoint_id)
    .bind(i64::from(owner_uid))
    .execute(pool)
    .await
    .context("rotating the webhook endpoint key reference")?;
    Ok(result.rows_affected() > 0)
}

/// Disable an endpoint without deleting it (history stays resolvable for audit,
/// which is why the migration has `disabled_at` rather than a DELETE).
pub async fn disable_endpoint(
    pool: &sqlx::SqlitePool,
    endpoint_id: &str,
    owner_uid: u32,
) -> anyhow::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE automation_endpoints SET disabled_at = ? \
         WHERE endpoint_id = ? AND owner_uid = ? AND disabled_at IS NULL",
    )
    .bind(&now)
    .bind(endpoint_id)
    .bind(i64::from(owner_uid))
    .execute(pool)
    .await
    .context("disabling the webhook endpoint")?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_integrations::webhook::{
        resolve_signing_key, EndpointResolver, SqliteDeliveryStore,
    };

    async fn temp_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = codypendent_daemon::db::open_database(&dir.path().join("codypendent.db"))
            .await
            .expect("open a migrated database");
        (dir, pool)
    }

    #[test]
    fn an_unroutable_endpoint_id_is_refused() {
        assert!(validate_endpoint_id("gh-main").is_ok());
        assert!(validate_endpoint_id("default").is_ok());
        assert!(validate_endpoint_id("").is_err());
        assert!(validate_endpoint_id("a/b").is_err());
        assert!(validate_endpoint_id("a?b").is_err());
        assert!(validate_endpoint_id("a b").is_err());
        assert!(validate_endpoint_id(&"x".repeat(65)).is_err());
    }

    /// The writer stores a REFERENCE, never key material, and only in a form
    /// the ingestor's own resolver accepts.
    #[test]
    fn only_an_env_reference_is_written() {
        assert_eq!(
            key_ref_from_env_name("GH_WEBHOOK_SECRET").unwrap(),
            "env:GH_WEBHOOK_SECRET"
        );
        assert!(key_ref_from_env_name("").is_err());
        assert!(key_ref_from_env_name("has space").is_err());
        assert!(key_ref_from_env_name("1LEADING_DIGIT").is_err());

        // The form round-trips through the verifier's resolver.
        std::env::set_var("CODYPENDENT_TEST_WEBHOOK_KEY", "s3cret");
        let reference = key_ref_from_env_name("CODYPENDENT_TEST_WEBHOOK_KEY").unwrap();
        assert_eq!(resolve_signing_key(&reference), Some(b"s3cret".to_vec()));
        std::env::remove_var("CODYPENDENT_TEST_WEBHOOK_KEY");
    }

    #[test]
    fn limits_are_checked_before_sqlite_checks_them() {
        assert!(validate_body_limit(0).is_err());
        assert!(validate_body_limit(-1).is_err());
        assert!(validate_body_limit(MAX_BODY_LIMIT_BYTES).is_ok());
        assert!(validate_body_limit(MAX_BODY_LIMIT_BYTES + 1).is_err());
        assert!(validate_replay_window(0).is_err());
        assert!(validate_replay_window(1).is_ok());
    }

    /// The reachability proof: a row this command writes is the row the
    /// daemon's OWN resolver reads on the ingest path. This test drives
    /// `SqliteDeliveryStore::resolve_endpoint` — the exact resolver
    /// `maybe_start_webhook_listener` attaches — and would fail if the INSERT
    /// were reverted, because before it the table had no writer at all.
    #[tokio::test]
    async fn a_registered_endpoint_is_what_the_daemons_resolver_reads() {
        let (_dir, pool) = temp_pool().await;
        insert_endpoint(&pool, "gh-main", 501, "env:GH_HOOK", 4096, 300)
            .await
            .expect("register");

        let resolver = SqliteDeliveryStore::new(pool.clone());
        let resolved = resolver
            .resolve_endpoint("gh-main")
            .await
            .expect("resolve")
            .expect("the registered endpoint resolves");
        assert_eq!(resolved.endpoint_id, "gh-main");
        assert_eq!(resolved.signing_key_ref, "env:GH_HOOK");
        assert_eq!(resolved.body_limit_bytes, 4096);
        assert_eq!(
            resolved.scheme,
            codypendent_integrations::webhook::SUPPORTED_SIGNATURE_SCHEME
        );

        // Disabling it takes the endpoint back out of the resolver's answer —
        // the delivery is refused exactly as an unknown endpoint is.
        assert!(disable_endpoint(&pool, "gh-main", 501)
            .await
            .expect("disable"));
        assert!(resolver
            .resolve_endpoint("gh-main")
            .await
            .expect("resolve")
            .is_none());
    }

    /// Send a raw HTTP POST to the listener and return the status code.
    async fn post(
        addr: std::net::SocketAddr,
        path: &str,
        signature: &str,
        delivery_id: &str,
        body: &[u8],
    ) -> u16 {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {len}\r\n\
             X-Hub-Signature-256: {signature}\r\nX-GitHub-Event: pull_request\r\n\
             X-GitHub-Delivery: {delivery_id}\r\nConnection: close\r\n\r\n",
            len = body.len(),
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        // A refusal (413) is written and the socket closed WITHOUT reading the
        // body, so a large write can legitimately be reset mid-flight. That is
        // the server behaving correctly; the status line is the assertion.
        let _ = stream.write_all(body).await;
        let _ = stream.flush().await;
        let mut response = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    response.extend_from_slice(&chunk[..read]);
                    if response.windows(2).any(|pair| pair == b"\r\n") {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&response)
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    /// End to end over real HTTP, in the daemon's own assembly: a row written by
    /// THIS command's writer, resolved by `SqliteDeliveryStore` as
    /// `maybe_start_webhook_listener` attaches it, decides whether a signed
    /// delivery is accepted — and its `body_limit_bytes` decides how much is let
    /// in. Revert the INSERT and every assertion below flips to 401: before this
    /// writer existed there was no way to reach the 202 at all.
    #[tokio::test]
    async fn a_registered_endpoint_governs_a_real_delivery_over_http() {
        use codypendent_integrations::webhook::{
            sign, DeliveryStore, SqliteDeliveryStore, WebhookIngestor,
        };
        use std::sync::Arc;

        const KEY_VAR: &str = "CODYPENDENT_E2E_WEBHOOK_KEY";
        const KEY: &[u8] = b"e2e-shared-secret";
        std::env::set_var(KEY_VAR, String::from_utf8_lossy(KEY).to_string());

        let (_dir, pool) = temp_pool().await;
        insert_endpoint(&pool, "gh-e2e", 501, &format!("env:{KEY_VAR}"), 4096, 300)
            .await
            .expect("register");

        let store = SqliteDeliveryStore::new(pool.clone());
        let ingestor = Arc::new(
            WebhookIngestor::new(
                Arc::new(store.clone()) as Arc<dyn DeliveryStore>,
                None,
                None,
            )
            .with_endpoint_resolver(Arc::new(store)),
        );
        let listener = codypendent_integrations::webhook::server::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = codypendent_integrations::webhook::server::serve(listener, ingestor).await;
        });

        let body = serde_json::to_vec(&serde_json::json!({
            "action": "opened",
            "pull_request": { "number": 7 },
            "repository": { "full_name": "octocat/hello-world" }
        }))
        .expect("fixture");

        // The registered endpoint, signed with the key its reference names.
        assert_eq!(
            post(addr, "/webhooks/gh-e2e", &sign(KEY, &body), "d-1", &body).await,
            202
        );
        // The same signature to an id nobody registered is refused.
        assert_eq!(
            post(addr, "/webhooks/gh-other", &sign(KEY, &body), "d-2", &body).await,
            401
        );
        // The endpoint's own body ceiling governs: 4096 bytes, so this is refused
        // before it is read.
        let big = serde_json::to_vec(&serde_json::json!({
            "action": "opened",
            "pull_request": { "number": 7 },
            "repository": { "full_name": "octocat/hello-world" },
            "padding": "x".repeat(5000),
        }))
        .expect("fixture");
        assert_eq!(
            post(addr, "/webhooks/gh-e2e", &sign(KEY, &big), "d-3", &big).await,
            413
        );
        // Retiring it takes the 202 away again.
        assert!(disable_endpoint(&pool, "gh-e2e", 501)
            .await
            .expect("disable"));
        let fresh = serde_json::to_vec(&serde_json::json!({
            "action": "closed",
            "pull_request": { "number": 8 },
            "repository": { "full_name": "octocat/hello-world" }
        }))
        .expect("fixture");
        assert_eq!(
            post(addr, "/webhooks/gh-e2e", &sign(KEY, &fresh), "d-4", &fresh).await,
            401
        );
        std::env::remove_var(KEY_VAR);
    }

    #[tokio::test]
    async fn rotation_and_disable_are_owner_scoped_and_registration_is_unique() {
        let (_dir, pool) = temp_pool().await;
        insert_endpoint(&pool, "gh-main", 501, "env:GH_HOOK", 4096, 300)
            .await
            .expect("register");
        assert!(
            insert_endpoint(&pool, "gh-main", 501, "env:GH_HOOK", 4096, 300)
                .await
                .is_err(),
            "a second registration of the same id must be refused, not silently overwrite the key"
        );

        assert!(
            !rotate_endpoint(&pool, "gh-main", 502, "env:OTHER")
                .await
                .expect("rotate"),
            "another uid must not rotate this endpoint's key"
        );
        assert!(
            !disable_endpoint(&pool, "gh-main", 502)
                .await
                .expect("disable"),
            "another uid must not disable this endpoint"
        );
        assert!(rotate_endpoint(&pool, "gh-main", 501, "env:NEW")
            .await
            .expect("rotate"));

        let rows = select_endpoints(&pool, 501).await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].signing_key_ref, "env:NEW");
        assert!(rows[0].rotated_at.is_some());
        assert!(
            select_endpoints(&pool, 502).await.expect("list").is_empty(),
            "another uid sees nothing, so the listing is not an existence oracle"
        );
    }
}

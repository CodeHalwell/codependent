//! `codypendent-codypendentd` — the persistent Codypendent daemon, exposed both
//! as the standalone `codypendentd` binary (`src/main.rs`) and as a library so
//! the single `codypendent` binary can run the SAME daemon in-process via the
//! hidden `codypendent __daemon` subcommand. Installing/updating `codypendent`
//! therefore always updates the daemon — no separate binary on disk to go stale
//! (the client/daemon version-skew bug this crate's library form eliminates).
//!
//! This is the composition root. It depends on BOTH `codypendent-daemon` (the
//! server + persistence) and `codypendent-runtime` (the agent loop) — which the
//! daemon crate itself cannot, because the runtime depends on the daemon (a
//! cycle). [`run_daemon`] performs the daemon startup exactly as the old
//! `main.rs` did (paths already resolved, db, boot, recovery), then constructs a
//! [`RuntimeExecutor`] that drives the runtime agent loop and injects it into
//! the server.

mod blackboard;
mod documents;
mod executor;
mod promotion;
mod publish;
mod routing;
mod scan;
mod session_history;
mod workflow_exec;
mod workflows;

use std::path::PathBuf;
use std::sync::Arc;

use codypendent_daemon::{db, instance, recovery, server};
use codypendent_protocol::discovery::RuntimePaths;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::executor::RuntimeExecutor;

/// Install the daemon's tracing subscriber: a formatted layer to stderr, its
/// filter taken from the environment (`RUST_LOG`/`EnvFilter`) and defaulting to
/// `info`. Both the standalone `codypendentd` binary and `codypendent __daemon`
/// call this FIRST — the daemon's stderr is what the CLI redirects to
/// `daemon.log`, so this preserves the daemon's logging behavior byte-for-byte.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Run the Codypendent daemon to shutdown. `paths` must already be resolved and
/// have had `ensure_directories()` called (the caller — the `codypendentd`
/// binary or `codypendent __daemon` — does that, "as today"). Returns `Ok(())`
/// on graceful shutdown.
pub async fn run_daemon(paths: RuntimePaths) -> anyhow::Result<()> {
    let database_path = paths.data_dir.join("codypendent.db");

    // Claim single-instance exclusivity FIRST — before touching any shared
    // state. Recovery fails live runs, the relaunch spawns workers, and the
    // scan wipes/rebuilds the code graph; if a second daemon ran those against
    // a live daemon's database before discovering the socket was taken, it
    // would corrupt in-flight runs (contradictory terminal events, double
    // execution). Binding the socket is the mutex; losers exit here.
    let listener = server::acquire_socket(&paths).await?;

    let pool = db::open_database(&database_path).await?;
    let boot = instance::record_boot(&pool).await?;
    info!(
        instance = %boot.instance_id,
        boot_count = boot.boot_count,
        database = %database_path.display(),
        "codypendentd starting"
    );

    // Reconcile state a previous process may have left mid-flight — after the
    // exclusivity claim, before serving, so no client observes a half-recovered
    // run (STEP 1.14).
    let report = recovery::recover_on_startup(&pool, &paths).await?;
    info!(
        swept_tmp = report.swept_tmp,
        orphaned_leases = report.orphaned_leases.len(),
        reconciled_effects = report.reconciled_effects,
        failed_runs = report.failed_runs.len(),
        expired_approvals = report.expired_approvals.len(),
        "startup recovery complete"
    );

    // Register the built-in tools into the governed registry (STEP 2.2 — Phase-1
    // tools "now registered with metadata"). Idempotent: `register_builtins`
    // upserts by identity and reuses ids, so this is safe on every boot and is
    // what gives retrieval and the Skill Studio a populated registry from the
    // first start. A failure here is logged but never blocks the daemon.
    match codypendent_knowledge::register_builtins(&pool).await {
        Ok(()) => info!("built-in tools registered in the knowledge registry"),
        Err(error) => warn!(%error, "failed to register built-in tools"),
    }

    // Derive the process's fallback repository identity without warming the code
    // graph synchronously. Session attach and run launch schedule valid Git
    // checkouts in the background; startup must never walk an arbitrary daemon
    // working directory before it can serve clients.
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repository = scan::repository_id_for(&workdir);

    // Register the operator's installed skill packages, so retrieval has
    // something to disclose beyond the built-ins. `register_package` previously
    // had no production caller at all: a package on disk reached the registry
    // only from a test. Both well-known roots are probed on every boot —
    // `<data_dir>/skills/` (what `codypendent skill add` installs into) and the
    // startup checkout's `.codypendent/skills/` (packages committed alongside
    // the code they serve) — and an absent root is a clean no-op. Idempotent
    // like `register_builtins` above: identity is reused, so re-scanning every
    // boot re-verifies each package's content hash rather than duplicating it.
    scan_installed_skills(&pool, &paths.data_dir, &workdir, repository).await;

    // The executor owns the shared event fan-out + approval broker the server
    // binds to (`RunExecutor::collaborators`), and drives each accepted run
    // through the runtime agent loop. `workdir` is the daemon's startup root,
    // used both as the per-run worktree-binding fallback / node repository (T5,
    // the 4th `new` arg) and as the document-publish root (Phase 4 STEP 4.4 —
    // a document has no per-command repository field the way `StartRun` does,
    // so publication uses this same startup root, as the code-graph scan does).
    let mut executor =
        RuntimeExecutor::new(pool.clone(), paths.clone(), repository, workdir.clone())
            .with_repository_root(workdir);

    // Personal-mode GitHub (Phase 3 STEP 3.2): discover a token from `gh auth
    // token` or `GITHUB_TOKEN` and enable the `github.*` tools. Absent (the
    // common case in CI/headless), the tools stay disabled and the daemon runs
    // exactly as before. The token is a secret — only whether one was found is
    // ever logged, never its value.
    match codypendent_integrations::github::GitHubToken::discover().await {
        Ok(token) => {
            match codypendent_integrations::github::RestGitHubClient::new(
                "https://api.github.com",
                token,
            ) {
                Ok(client) => {
                    executor = executor.with_github(Arc::new(client));
                    info!("github personal-mode client enabled");
                }
                Err(error) => {
                    warn!(%error, "could not build the github client; github tools disabled")
                }
            }
        }
        Err(_) => info!("no github token found; github tools disabled"),
    }

    // Web search resolves its key per call. A TUI key update therefore applies
    // instantly without restarting this daemon; a missing key fails the tool
    // locally before any network request and remains secret-safe.
    executor = executor.with_search(Arc::new(
        codypendent_integrations::search::ReloadingTavilyClient::new(
            paths.data_dir.clone(),
            "https://api.tavily.com",
        ),
    ));
    info!("tavily web search adapter enabled (credentials resolve per call)");

    // MCP client (PR B): load the operator-declared server list from
    // `<config_dir>/mcp.toml` (sibling to `policy.toml`) and hand the registry
    // to the executor, so single-agent runs AND workflow agent nodes are
    // offered the `mcp.<server>.<tool>` tools their warm servers provide.
    //
    // A MALFORMED file is deliberately NOT fatal: MCP is an optional feature,
    // so a typo in its config must not kill the daemon. The load error is
    // legible (it names the path and the reason) and is logged loudly here,
    // and it stays visible via `codypendent mcp list`; the daemon simply
    // continues with NO MCP servers. An absent file — or one declaring zero
    // servers — builds no registry at all.
    match codypendent_integrations::mcp::load_mcp_config(&paths.global_mcp_path()) {
        Ok(config) if !config.servers.is_empty() => {
            let server_count = config.servers.len();
            let registry = Arc::new(codypendent_integrations::mcp::McpRegistry::new(config));
            executor = executor.with_mcp(registry.clone());
            // Fire-and-forget pre-warm (the code-graph scan precedent): a
            // server that won't start is logged inside `warm_all`, never
            // fatal — its tools stay unoffered until a lazy spawn succeeds.
            tokio::spawn(async move { registry.warm_all().await });
            info!(
                servers = server_count,
                "mcp registry enabled; warming servers in the background"
            );
        }
        Ok(_) => info!("no mcp servers configured; mcp tools disabled"),
        Err(error) => {
            error!(%error, "malformed mcp config; continuing with NO mcp servers");
        }
    }

    let executor = Arc::new(executor);

    // Re-launch any run left `Queued` by a crash between `StartRun`'s commit and
    // its fire-and-forget spawn — recovery's live-state sweep does not cover
    // `Queued`, so those runs would otherwise be stuck with no worker.
    match executor.relaunch_queued_runs().await {
        Ok(0) => {}
        Ok(n) => info!(
            relaunched = n,
            "re-launched queued runs orphaned by a prior crash"
        ),
        Err(error) => warn!(%error, "could not re-launch queued runs at startup"),
    }

    // Resume any durable workflow run left non-terminal by a crash (Phase 5 STEP
    // 5.2): recompile each from its stored manifest and drive it onward from where
    // it stopped. Paused runs are left for an explicit resume. Fire-and-forget, so
    // a slow workflow never stalls the socket server below; a run that cannot be
    // recompiled is a no-op logged by its drive task.
    match executor.recover_workflows().await {
        Ok(0) => {}
        Ok(n) => info!(recovered = n, "resumed incomplete workflow runs"),
        Err(error) => warn!(%error, "could not resume workflow runs at startup"),
    }

    // Re-arm approval-gated document publication from its durable plan. Generic
    // recovery deliberately leaves these synthetic runs live; this step runs
    // after GitHub/MCP adapter assembly so every publication target has the same
    // capabilities it had before the restart.
    match executor.recover_document_publications().await {
        Ok(0) => {}
        Ok(n) => info!(recovered = n, "resumed document publication jobs"),
        Err(error) => warn!(%error, "could not resume document publication jobs"),
    }

    // Optionally open the GitHub webhook listener (Phase 3 STEP 3.3). It is
    // disabled unless `<data_dir>/webhooks.toml` sets `enabled = true`, and even
    // then binds loopback by default. Deliveries are verified, deduplicated by
    // their `X-GitHub-Delivery` GUID, and normalized; they never trigger
    // workflows here (that requires explicit policy, wired in a later phase). The
    // listener runs concurrently with the blocking socket server below.
    maybe_start_webhook_listener(&paths, &pool).await;

    server::run_with_executor_on(listener, pool, paths, boot, Some(executor)).await
}

/// Register every skill package installed under the two well-known roots into
/// the governed registry, so the retrieval funnel can disclose them.
///
/// Both roots are probed on every boot: `<data_dir>/skills/` (the operator's
/// global installs, where `codypendent skill add` copies a validated package)
/// and `<workdir>/.codypendent/skills/` (packages a checkout commits alongside
/// the code they serve). `repository` anchors any package declaring
/// repository tier, matching the identity the executor attributes a run's
/// context to.
///
/// Best-effort throughout, like the code-graph warm-up: a broken package is
/// logged with its reason and skipped, never fatal and never blocking its
/// siblings. Registration is idempotent — a re-scan reuses the existing
/// identity's id and only flags a content change — so this is safe every boot.
async fn scan_installed_skills(
    pool: &sqlx::SqlitePool,
    data_dir: &std::path::Path,
    workdir: &std::path::Path,
    repository: codypendent_protocol::RepositoryId,
) {
    use codypendent_knowledge::{repository_skills_root, scan_skill_root, user_skills_root};

    let roots = [
        ("user", user_skills_root(data_dir)),
        ("repository", repository_skills_root(workdir)),
    ];
    let mut registered = 0usize;
    for (label, root) in roots {
        let outcome = scan_skill_root(pool, &root, repository).await;
        registered += outcome.registered.len();
        for (dir, reason) in outcome.failures {
            warn!(root = label, package = %dir.display(), %reason, "skill package not registered");
        }
    }
    if registered > 0 {
        info!(skills = registered, "installed skill packages registered");
    }
}

/// Start the webhook listener if `<data_dir>/webhooks.toml` enables it. Any
/// failure is logged and never blocks daemon startup — the webhook endpoint is
/// an optional, opt-in surface.
async fn maybe_start_webhook_listener(paths: &RuntimePaths, pool: &sqlx::SqlitePool) {
    use codypendent_integrations::webhook::{config, SqliteDeliveryStore, WebhookIngestor};

    let config_path = paths.data_dir.join("webhooks.toml");
    let webhooks = match config::load(&config_path) {
        Ok(Some(webhooks)) if webhooks.enabled => webhooks,
        Ok(_) => return, // absent or disabled — the default
        Err(error) => {
            warn!(%error, "failed to load webhooks configuration; listener not started");
            return;
        }
    };

    // The secret never reaches a log line: only its presence is reported.
    let secret = webhooks
        .secret
        .as_ref()
        .map(|value| value.as_bytes().to_vec());
    let store = Arc::new(SqliteDeliveryStore::new(pool.clone()));
    // Deliveries never trigger workflows in this phase (default-deny policy).
    let ingestor = Arc::new(WebhookIngestor::new(store, secret, false));

    match codypendent_integrations::webhook::server::bind(&webhooks.listen_addr).await {
        Ok(listener) => {
            info!(
                addr = %webhooks.listen_addr,
                signed = webhooks.secret.is_some(),
                "webhook listener enabled"
            );
            tokio::spawn(async move {
                if let Err(error) =
                    codypendent_integrations::webhook::server::serve(listener, ingestor).await
                {
                    warn!(%error, "webhook listener stopped");
                }
            });
        }
        Err(error) => warn!(
            %error,
            addr = %webhooks.listen_addr,
            "could not bind the webhook listener"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use codypendent_knowledge::{
        repository_skills_root, user_skills_root, Registry, RegistryItemKind, Scope,
    };
    use codypendent_protocol::RepositoryId;

    /// Write a minimal valid skill package at `dir`.
    fn write_package(dir: &std::path::Path, id: &str, scope: &str) {
        std::fs::create_dir_all(dir).expect("package dir");
        std::fs::write(
            dir.join("skill.toml"),
            format!(
                "schema_version = 1\n\
                 id = \"{id}\"\n\
                 name = \"Test Skill\"\n\
                 version = \"0.1.0\"\n\
                 scope = \"{scope}\"\n\
                 status = \"active\"\n\
                 description = \"A test skill.\"\n\
                 \n\
                 [entrypoints]\n\
                 instructions = \"SKILL.md\"\n\
                 \n\
                 [trust]\n\
                 publisher = \"local-user\"\n\
                 signature_required = false\n"
            ),
        )
        .expect("write skill.toml");
        std::fs::write(dir.join("SKILL.md"), "# Test\n").expect("write SKILL.md");
    }

    /// The startup scan is the production ingestion path the 2026-08-11 review
    /// found missing entirely (`register_package` was reachable only from
    /// tests): both well-known roots must register, a broken package must not
    /// block its siblings, and a second boot must not duplicate a row.
    #[tokio::test]
    async fn startup_registers_both_skill_roots_idempotently() {
        let data = tempfile::tempdir().expect("data dir");
        let workdir = tempfile::tempdir().expect("workdir");
        let pool = codypendent_daemon::db::open_database(&data.path().join("codypendent.db"))
            .await
            .expect("open db");
        let repository = RepositoryId::new();

        write_package(
            &user_skills_root(data.path()).join("global"),
            "test.global",
            "user",
        );
        write_package(
            &repository_skills_root(workdir.path()).join("local"),
            "test.local",
            "repository",
        );
        // A package whose declared entrypoint is missing: skipped with a
        // reason, never fatal to its siblings or the boot.
        let broken = user_skills_root(data.path()).join("broken");
        write_package(&broken, "test.broken", "user");
        std::fs::remove_file(broken.join("SKILL.md")).expect("break the package");

        scan_installed_skills(&pool, data.path(), workdir.path(), repository).await;

        let skills: Vec<_> = Registry::new()
            .list(&pool)
            .await
            .expect("list registry")
            .into_iter()
            .filter(|item| item.kind == RegistryItemKind::Skill)
            .collect();
        assert_eq!(skills.len(), 2, "both good packages register: {skills:?}");
        let global = skills
            .iter()
            .find(|item| item.name == "test.global")
            .expect("the data-dir package registered");
        assert_eq!(global.scope, codypendent_knowledge::local_user_scope());
        let local = skills
            .iter()
            .find(|item| item.name == "test.local")
            .expect("the repo-local package registered");
        assert_eq!(
            local.scope,
            Scope::Repository(repository),
            "a repository-tier package anchors to the daemon's repository"
        );

        // A second boot re-verifies rather than duplicating.
        let ids: Vec<_> = skills.iter().map(|item| item.id).collect();
        scan_installed_skills(&pool, data.path(), workdir.path(), repository).await;
        let after: Vec<_> = Registry::new()
            .list(&pool)
            .await
            .expect("list registry")
            .into_iter()
            .filter(|item| item.kind == RegistryItemKind::Skill)
            .collect();
        assert_eq!(after.len(), 2, "a re-scan must not duplicate rows");
        for item in &after {
            assert!(ids.contains(&item.id), "identity survives a re-scan");
        }
    }

    /// Absent roots — the common case on a fresh install — are a clean no-op,
    /// so the scan can run unconditionally on every boot.
    #[tokio::test]
    async fn startup_with_no_installed_skills_is_a_no_op() {
        let data = tempfile::tempdir().expect("data dir");
        let workdir = tempfile::tempdir().expect("workdir");
        let pool = codypendent_daemon::db::open_database(&data.path().join("codypendent.db"))
            .await
            .expect("open db");

        scan_installed_skills(&pool, data.path(), workdir.path(), RepositoryId::new()).await;

        let skills = Registry::new()
            .list(&pool)
            .await
            .expect("list registry")
            .into_iter()
            .filter(|item| item.kind == RegistryItemKind::Skill)
            .count();
        assert_eq!(skills, 0);
    }
}

//! The assembly side of automation: what makes a stored binding actually FIRE.
//!
//! `codypendent-daemon` owns the scheduler ([`AutomationScheduler`]) but cannot
//! own its inputs — it can name neither `codypendent-workflow`'s source registry
//! (workflow resolution lives in this assembly) nor [`crate::scan`]'s
//! repository-identity derivation. Those cross the
//! [`AutomationEnvironment`] seam from here, exactly as `WorkflowStarter` does.
//!
//! Two production callers are wired in [`crate::run_daemon`]:
//!
//! * [`start_automation_scheduler`] spawns the tick loop, so a cron/one-time
//!   binding whose `next_fire_at` has come is claimed, reserved and started.
//! * [`AutomationWebhookSink`] is the `WebhookEventSink` the ingestor was
//!   built for and never had: a verified, deduplicated GitHub delivery is fanned
//!   out to every enabled binding on that endpoint.
//!
//! The sink is **policy-gated**, not implicitly on: `webhooks.toml` must set
//! `automation_dispatch = true`. `None` was a deliberate default-deny, so
//! turning it on is an explicit operator act, and the flag is separate from
//! `enabled` (which only opens the listener).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use codypendent_daemon::automation::workflow_id_for_manifest_name;
use codypendent_daemon::automation_scheduler::{
    AutomationEnvironment, AutomationScheduler, TriggerEvent, TICK_INTERVAL,
};
use codypendent_daemon::executor::RunExecutor;
use codypendent_integrations::webhook::{
    normalize::NormalizedEvent, WebhookError, WebhookEventSink,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{CodypendentError, DaemonInstanceId, RepositoryId, WorkflowId};
use codypendent_workflow::source::BUILTIN_WORKFLOW_MANIFESTS;
use codypendent_workflow::{parse_definition, WorkflowScope};
use tracing::{info, warn};

use crate::executor::RuntimeExecutor;

/// Build the scheduler and spawn its tick loop, returning it so the webhook sink
/// can share the SAME instance (one lease holder, one dispatcher).
///
/// Returns `None` — with a warning and no loop — when the daemon cannot satisfy
/// a precondition for firing anything:
///
/// * no [`WorkflowStarter`](codypendent_daemon::workflows::WorkflowStarter): a
///   scheduler that claims occurrences it cannot start would consume them and
///   drop the work.
/// * the daemon's own uid is unreadable: ownership could not then be checked,
///   and the only alternative — running a binding as whoever the daemon happens
///   to be — is the privilege escalation `owner_is_resolvable` exists to stop.
///
/// Never fatal to startup, mirroring `retrieval::spawn_index_maintenance`.
pub(crate) fn start_automation_scheduler(
    pool: &sqlx::SqlitePool,
    paths: &RuntimePaths,
    instance_id: DaemonInstanceId,
    executor: &Arc<RuntimeExecutor>,
) -> Option<AutomationScheduler> {
    let Some(starter) = executor.workflow_starter() else {
        warn!("automation scheduler not started: no workflow starter is wired");
        return None;
    };
    let daemon_uid = match daemon_uid(paths) {
        Some(uid) => uid,
        None => {
            warn!("automation scheduler not started: the daemon's own uid is unreadable");
            return None;
        }
    };

    let environment = Arc::new(DaemonAutomationEnvironment {
        user_workflow_dir: paths.data_dir.join("workflows"),
        daemon_uid,
    });
    let scheduler = AutomationScheduler::new(pool.clone(), instance_id, starter, environment);
    scheduler.clone().spawn(TICK_INTERVAL);
    Some(scheduler)
}

/// The daemon process's own uid, read from the socket inode it has already
/// bound. A file's owner is the effective uid of the process that created it, so
/// this is exact — and it needs no `libc`/`unsafe` (the workspace denies
/// `unsafe_code`) and no new dependency. This mirrors the server's own
/// `daemon_uid_from_socket`, which is private to that module and runs later.
fn daemon_uid(paths: &RuntimePaths) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(&paths.socket_path)
        .map(|metadata| metadata.uid())
        .ok()
}

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

/// The production [`AutomationEnvironment`].
struct DaemonAutomationEnvironment {
    /// `<data_dir>/workflows`, the same per-user source
    /// `WorkflowConductorHost::with_workflow_source_dir` is given.
    user_workflow_dir: PathBuf,
    /// The uid this daemon runs as.
    daemon_uid: u32,
}

impl AutomationEnvironment for DaemonAutomationEnvironment {
    /// Invert `workflow_id_for_manifest_name` over the sources a start would
    /// actually resolve from, and refuse unless the winning manifest is at the
    /// version the binding pinned.
    ///
    /// The binding stores a `WorkflowId` UUID while `StartWorkflowRequest` takes
    /// a manifest NAME, and no `workflow_definitions` table maps one to the
    /// other; the id is therefore derivable from the name, and the map is
    /// inverted by hashing the tens of manifests in scope rather than by a
    /// lookup table with no writer.
    fn resolve_workflow(
        &self,
        workflow_id: WorkflowId,
        workflow_version: &str,
        repository_path: &Path,
    ) -> Result<String, CodypendentError> {
        let pinned: u32 = workflow_version.trim().parse().map_err(|_| {
            refuse(
                "automation.workflow-version-invalid",
                format!(
                    "binding pins workflow version '{workflow_version}', which is not a version"
                ),
            )
        })?;

        let repository_dir = repository_path.join(".codypendent").join("workflows");
        let mut matches: Vec<(WorkflowScope, u32, String)> = Vec::new();
        for (scope, manifest) in manifests_in_scope(&self.user_workflow_dir, &repository_dir) {
            let Ok(definition) = parse_definition(&manifest) else {
                continue;
            };
            if workflow_id_for_manifest_name(&definition.id) == workflow_id {
                matches.push((scope, definition.version, definition.id));
            }
        }

        // The precedence a start will apply: repository over user over built-in,
        // then the higher version (`WorkflowSourceRegistry::resolve`). Picking
        // the same winner here is what makes the version check meaningful — the
        // pin must match what would RUN, not merely what exists somewhere.
        let Some((_, version, name)) = matches
            .into_iter()
            .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
        else {
            return Err(refuse(
                "workflow.unknown-workflow",
                "the binding's workflow is not defined by any source in scope",
            ));
        };
        if version != pinned {
            return Err(refuse(
                "automation.workflow-version-mismatch",
                format!(
                    "binding pins workflow version {pinned} but version {version} is what would run"
                ),
            ));
        }
        Ok(name)
    }

    /// Re-derive the checkout's identity, so a moved or renamed repository is
    /// detected instead of silently running the automation against whatever now
    /// occupies the old path. `None` when the path is not a readable directory —
    /// `scan::repository_id_for` hashes a path whether or not it exists, so the
    /// existence check has to happen here.
    fn repository_id_for(&self, path: &Path) -> Option<RepositoryId> {
        path.is_dir().then(|| crate::scan::repository_id_for(path))
    }

    /// Whether this daemon may act as `uid`.
    ///
    /// The daemon never changes uid: every run it starts executes as the process
    /// owner. So a binding owned by any other uid CANNOT be honoured — running it
    /// anyway would execute one user's automation with another's authority, which
    /// is exactly the fallback-to-`daemon_uid` escalation this check exists to
    /// prevent. Refusing is the only sound answer, and the receipt records
    /// `automation.owner-unresolvable` so an operator can see why.
    fn owner_is_resolvable(&self, uid: u32) -> bool {
        uid == self.daemon_uid
    }
}

fn refuse(code: &'static str, message: impl Into<String>) -> CodypendentError {
    CodypendentError::new(code, message.into(), false)
}

/// Every manifest text a start could resolve from, tagged with its scope: the
/// embedded built-ins, then the per-user directory, then the repository's own —
/// the same three sources `WorkflowSourceRegistry::load` gathers. An unreadable
/// directory or file contributes nothing (a broken sibling never hides a healthy
/// workflow).
fn manifests_in_scope(user_dir: &Path, repository_dir: &Path) -> Vec<(WorkflowScope, String)> {
    let mut out: Vec<(WorkflowScope, String)> = BUILTIN_WORKFLOW_MANIFESTS
        .iter()
        .map(|manifest| (WorkflowScope::BuiltIn, (*manifest).to_string()))
        .collect();
    for (scope, dir) in [
        (WorkflowScope::User, user_dir),
        (WorkflowScope::Repository, repository_dir),
    ] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("yaml" | "yml")
                )
            })
            .collect();
        files.sort();
        for path in files {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                out.push((scope, raw));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The webhook sink
// ---------------------------------------------------------------------------

/// The `WebhookEventSink` that turns a verified delivery into workflow runs.
///
/// Attached only when `webhooks.toml` sets `automation_dispatch = true`.
pub(crate) struct AutomationWebhookSink {
    scheduler: AutomationScheduler,
}

impl AutomationWebhookSink {
    pub(crate) fn new(scheduler: AutomationScheduler) -> Self {
        Self { scheduler }
    }
}

#[async_trait::async_trait]
impl WebhookEventSink for AutomationWebhookSink {
    /// Fan the delivery out to the endpoint's bindings.
    ///
    /// Always `Ok`: a sink error propagates out of `WebhookIngestor::ingest` and
    /// fails the whole delivery, which would make the provider retry a delivery
    /// that was correctly filtered or correctly denied. Per-binding outcomes are
    /// durable in `automation_attempts`, which is where they belong.
    async fn on_event(
        &self,
        endpoint_id: &str,
        delivery_id: &str,
        event_type: &str,
        event: &NormalizedEvent,
        _raw_body: &[u8],
    ) -> Result<(), WebhookError> {
        let trigger = trigger_event(delivery_id, event_type, event);
        let dispatched = self
            .scheduler
            .dispatch_endpoint_event(endpoint_id, &trigger, Utc::now())
            .await;
        if dispatched > 0 {
            info!(
                endpoint = endpoint_id,
                event = event_type,
                bindings = dispatched,
                "automation dispatched a webhook delivery"
            );
        }
        Ok(())
    }
}

/// Flatten a [`NormalizedEvent`] into the string fields a binding's
/// `TriggerFilters` and `DeduplicationPolicy.identity_fields` name.
///
/// `content_fingerprint` stays `None`: ingestion computes its replay fingerprint
/// over the signature and does not hand it to the sink, and recording a
/// differently-derived digest in a column documented as that one would be a
/// measurement that was never taken.
fn trigger_event(delivery_id: &str, event_type: &str, event: &NormalizedEvent) -> TriggerEvent {
    let mut trigger = TriggerEvent {
        event_type: event_type.to_string(),
        delivery_id: (!delivery_id.is_empty()).then(|| delivery_id.to_string()),
        content_fingerprint: None,
        ..TriggerEvent::default()
    };
    let mut put = |key: &str, value: &str| {
        trigger.fields.insert(key.to_string(), value.to_string());
    };
    match event {
        NormalizedEvent::Ping => {}
        NormalizedEvent::PullRequest {
            action,
            number,
            repository,
        } => {
            put("action", action);
            put("number", &number.to_string());
            put("repository", repository);
        }
        NormalizedEvent::CheckRun {
            action,
            name,
            status,
            repository,
        } => {
            put("action", action);
            put("check_name", name);
            put("status", status);
            put("repository", repository);
        }
        NormalizedEvent::Push {
            git_ref,
            repository,
        } => {
            put("git_ref", git_ref);
            // The bare branch name too, so a `branches` filter written the way an
            // operator says it (`main`) matches a fully-qualified ref.
            if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
                put("branch", branch);
            }
            put("repository", repository);
        }
        // An event type the normalizer does not model carries no fields. It still
        // reaches the bindings on this endpoint: one with no filters may want it,
        // and one with filters will not match (fail closed).
        NormalizedEvent::Other { event_type } => put("event_type", event_type),
    }
    trigger
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_event_carries_both_the_ref_and_the_bare_branch() {
        let event = NormalizedEvent::Push {
            git_ref: "refs/heads/main".into(),
            repository: "octocat/hello-world".into(),
        };
        let trigger = trigger_event("d-1", "push", &event);
        assert_eq!(trigger.delivery_id.as_deref(), Some("d-1"));
        assert_eq!(
            trigger.fields.get("git_ref").map(String::as_str),
            Some("refs/heads/main")
        );
        assert_eq!(
            trigger.fields.get("branch").map(String::as_str),
            Some("main")
        );
        assert_eq!(
            trigger.fields.get("repository").map(String::as_str),
            Some("octocat/hello-world")
        );
        assert!(
            trigger.content_fingerprint.is_none(),
            "the signature digest is not exposed to the sink; it must stay absent"
        );
    }

    #[test]
    fn a_binding_owned_by_another_uid_is_never_resolvable() {
        let environment = DaemonAutomationEnvironment {
            user_workflow_dir: PathBuf::from("/nonexistent"),
            daemon_uid: 501,
        };
        assert!(environment.owner_is_resolvable(501));
        assert!(
            !environment.owner_is_resolvable(502),
            "the daemon never changes uid, so another owner's binding must refuse"
        );
    }

    #[test]
    fn a_missing_checkout_has_no_repository_identity() {
        let environment = DaemonAutomationEnvironment {
            user_workflow_dir: PathBuf::from("/nonexistent"),
            daemon_uid: 501,
        };
        assert!(environment
            .repository_id_for(Path::new("/nonexistent/checkout"))
            .is_none());
    }

    /// The built-in `repair-github-check` is resolvable from its derived id at
    /// its own version, and refuses at any other — the version pin is enforced,
    /// which `resolve_named_workflow` alone does not do.
    #[test]
    fn a_builtin_workflow_resolves_by_its_derived_id_at_the_pinned_version() {
        let environment = DaemonAutomationEnvironment {
            user_workflow_dir: PathBuf::from("/nonexistent"),
            daemon_uid: 501,
        };
        let definition = parse_definition(codypendent_workflow::REPAIR_GITHUB_CHECK_MANIFEST)
            .expect("the built-in manifest parses");
        let id = workflow_id_for_manifest_name(&definition.id);
        let resolved = environment
            .resolve_workflow(
                id,
                &definition.version.to_string(),
                Path::new("/nonexistent/checkout"),
            )
            .expect("the built-in resolves");
        assert_eq!(resolved, definition.id);

        let error = environment
            .resolve_workflow(
                id,
                &(definition.version + 1).to_string(),
                Path::new("/nonexistent/checkout"),
            )
            .expect_err("a version that would not run must refuse");
        assert_eq!(error.code, "automation.workflow-version-mismatch");
    }

    /// The whole webhook path, end to end through the PRODUCTION types: a
    /// verified delivery reaches [`AutomationWebhookSink::on_event`], which fans
    /// it out through the real [`AutomationScheduler`] to a binding created by
    /// the real `create_binding`, and a workflow run is started exactly once —
    /// a redelivery of the same GUID reserves nothing further.
    #[tokio::test]
    async fn a_verified_delivery_starts_a_run_once_through_the_real_sink() {
        use codypendent_daemon::workflows::{StartWorkflowRequest, WorkflowStarter};
        use codypendent_protocol::{
            AutomationBindingDraft, InvocationPolicy, TriggerFilters, TriggerSource,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingStarter {
            starts: AtomicUsize,
        }
        impl WorkflowStarter for CountingStarter {
            fn start(
                &self,
                request: StartWorkflowRequest,
            ) -> codypendent_daemon::workflows::WorkflowStartFuture<'_> {
                Box::pin(async move {
                    self.starts.fetch_add(1, Ordering::SeqCst);
                    Ok(format!("wfrun-{}", request.idempotency_key.len()))
                })
            }
        }

        const OWNER_UID: u32 = 4242;
        let checkout = tempfile::tempdir().expect("tempdir");
        let data = tempfile::tempdir().expect("tempdir");
        let repository_id = crate::scan::repository_id_for(checkout.path());
        let pool = codypendent_daemon::db::open_database(&data.path().join("automation.db"))
            .await
            .expect("open database");

        // `create_binding` takes the repository path from the owner's most recent
        // session, exactly as a real client's binding does.
        sqlx::query(
            "INSERT INTO sessions (id, title, state, created_at, updated_at, repository, \
             repository_id, owner_uid) VALUES (?, 'seed', 'active', ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .bind(checkout.path().display().to_string())
        .bind(repository_id.to_string())
        .bind(i64::from(OWNER_UID))
        .execute(&pool)
        .await
        .expect("seed session");

        let definition = parse_definition(codypendent_workflow::REPAIR_GITHUB_CHECK_MANIFEST)
            .expect("built-in manifest parses");
        codypendent_daemon::automation::create_binding(
            &pool,
            codypendent_daemon::principal::PeerPrincipal::from_uid(OWNER_UID),
            AutomationBindingDraft {
                name: "on-push".into(),
                source: TriggerSource::GitHubWebhook {
                    endpoint_id: "gh-1".into(),
                    installation_id: None,
                    events: vec!["push".into()],
                },
                workflow_id: workflow_id_for_manifest_name(&definition.id),
                workflow_version: definition.version.to_string(),
                repository_id,
                filters: TriggerFilters::default(),
                invocation: InvocationPolicy::default(),
                enabled: true,
            },
        )
        .await
        .expect("create binding");

        let starter = Arc::new(CountingStarter {
            starts: AtomicUsize::new(0),
        });
        let scheduler = AutomationScheduler::new(
            pool.clone(),
            DaemonInstanceId::new(),
            starter.clone(),
            Arc::new(DaemonAutomationEnvironment {
                user_workflow_dir: data.path().join("workflows"),
                daemon_uid: OWNER_UID,
            }),
        );
        let sink = AutomationWebhookSink::new(scheduler);
        let event = NormalizedEvent::Push {
            git_ref: "refs/heads/main".into(),
            repository: "octocat/hello-world".into(),
        };

        sink.on_event("gh-1", "delivery-1", "push", &event, b"{}")
            .await
            .expect("the sink never fails a delivery");
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1, "the run started");

        // The receipt is the durable proof, and its attempt records what happened.
        let (state, run_id): (String, Option<String>) = sqlx::query_as(
            "SELECT state, workflow_run_id FROM automation_receipts WHERE delivery_id = ?",
        )
        .bind("delivery-1")
        .fetch_one(&pool)
        .await
        .expect("a receipt was written");
        assert_eq!(state, "dispatched");
        assert!(run_id.is_some(), "the run id is recorded on the receipt");
        let outcome: String = sqlx::query_scalar(
            "SELECT a.outcome FROM automation_attempts a \
             JOIN automation_receipts r ON r.id = a.receipt_id WHERE r.delivery_id = ?",
        )
        .bind("delivery-1")
        .fetch_one(&pool)
        .await
        .expect("an attempt was written");
        assert_eq!(outcome, "started");

        // A redelivery of the same GUID reserves nothing and starts nothing.
        sink.on_event("gh-1", "delivery-1", "push", &event, b"{}")
            .await
            .expect("the sink never fails a delivery");
        assert_eq!(
            starter.starts.load(Ordering::SeqCst),
            1,
            "the reservation's UNIQUE (binding_id, dedup_key) admits exactly one firing"
        );
    }

    #[test]
    fn an_unknown_workflow_id_refuses_rather_than_picking_one() {
        let environment = DaemonAutomationEnvironment {
            user_workflow_dir: PathBuf::from("/nonexistent"),
            daemon_uid: 501,
        };
        let error = environment
            .resolve_workflow(
                workflow_id_for_manifest_name("no-such-workflow"),
                "1",
                Path::new("/nonexistent/checkout"),
            )
            .expect_err("must refuse");
        assert_eq!(error.code, "workflow.unknown-workflow");
    }
}

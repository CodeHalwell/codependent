//! Verified UI-worker lifecycle injection and broker bridge.
//!
//! Package discovery deliberately lives outside this module: callers may only
//! supply already verified [`UiWorkerLaunch`] values minted by the sandbox
//! lifecycle. This keeps a missing plugin database from becoming an execution
//! shortcut while still giving the daemon a real product integration seam.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use codypendent_protocol::SessionId;
use codypendent_sandbox::UiTarget;
use codypendent_ui_host::{
    UiWorker, UiWorkerError, UiWorkerLaunch, UiWorkerSignal, UiWorkerSupervisor,
};
use tokio::sync::{broadcast, mpsc, watch};

use crate::poison::lock_recovering;
use crate::remote_ui::{
    RemoteUiBroker, UiBrokerDispatch, UiBrokerFrame, UiBrokerTarget, UiMediatedAction,
    UiMediatedCancellation, UiMediatedSubscription, UiMediatedUnsubscription, UiProducerHandle,
};

const MAX_ACTIVE_WORKERS: usize = 64;
const MAX_ACTIVE_WORKERS_PER_SESSION: usize = 16;
const MAX_ACTIVE_WORKERS_PER_PLUGIN: usize = 16;
const MAX_ACTIVE_WORKER_MEMORY_MB: u64 = 8 * 1024;
type EnsureEpoch = (SessionId, Option<UiTarget>);

/// Trust-preserving source of launch descriptors. Implementations must build
/// these through `UiWorkerLaunch::from_installed`; raw paths are never accepted.
pub trait VerifiedUiLaunchSource: Send + Sync {
    fn launches_for(&self, session_id: SessionId) -> Vec<UiWorkerLaunch>;
}

/// Work that must run through daemon policy/projection stores before a result
/// can be delivered point-to-point to a worker.
#[derive(Debug, Clone)]
pub enum UiWorkerRequest {
    Action {
        session_id: SessionId,
        action: UiMediatedAction,
    },
    Subscription {
        session_id: SessionId,
        subscription: UiMediatedSubscription,
    },
    Unsubscription {
        session_id: SessionId,
        unsubscription: UiMediatedUnsubscription,
    },
    Cancellation {
        session_id: SessionId,
        cancellation: UiMediatedCancellation,
    },
    /// Teardown signal used to cancel daemon-side live projection readers when
    /// the owning worker exits or its session detaches.
    ProducerStopped {
        session_id: SessionId,
        producer: UiProducerHandle,
    },
}

/// Owns one supervised process per verified plugin/session/target tuple.
pub struct RemoteUiWorkerService {
    supervisor: Arc<UiWorkerSupervisor>,
    launches: Arc<dyn VerifiedUiLaunchSource>,
    active: Arc<Mutex<HashMap<ActiveWorkerKey, ActiveWorker>>>,
    ensured: Arc<Mutex<HashSet<EnsureEpoch>>>,
}

struct ActiveWorker {
    cancellation: watch::Sender<bool>,
    memory_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ActiveWorkerKey {
    session_id: SessionId,
    plugin_id: String,
    target: UiTarget,
}

impl std::fmt::Debug for RemoteUiWorkerService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteUiWorkerService")
            .field("active", &lock_recovering(&self.active).len())
            .finish_non_exhaustive()
    }
}

impl RemoteUiWorkerService {
    #[must_use]
    pub fn new(
        supervisor: Arc<UiWorkerSupervisor>,
        launches: Arc<dyn VerifiedUiLaunchSource>,
    ) -> Self {
        Self {
            supervisor,
            launches,
            active: Arc::new(Mutex::new(HashMap::new())),
            ensured: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    #[must_use]
    pub fn supervisor(&self) -> Arc<UiWorkerSupervisor> {
        Arc::clone(&self.supervisor)
    }

    /// Start all verified UI components enabled for a newly attached session.
    /// Duplicate attachment is idempotent. Launch/handshake runs in supervised
    /// tasks so a slow component never blocks the daemon socket loop.
    pub fn ensure_session(
        &self,
        session_id: SessionId,
        broker: RemoteUiBroker,
        requests: mpsc::Sender<UiWorkerRequest>,
    ) -> usize {
        self.ensure_session_filtered(session_id, None, broker, requests)
    }

    /// Start the shared worker plus only the entrypoint matching an attached
    /// renderer. A mixed terminal/web session therefore runs each target once
    /// and the broker can route documents without cross-target execution.
    pub fn ensure_session_target(
        &self,
        session_id: SessionId,
        target: UiTarget,
        broker: RemoteUiBroker,
        requests: mpsc::Sender<UiWorkerRequest>,
    ) -> usize {
        self.ensure_session_filtered(session_id, Some(target), broker, requests)
    }

    fn ensure_session_filtered(
        &self,
        session_id: SessionId,
        target: Option<UiTarget>,
        broker: RemoteUiBroker,
        requests: mpsc::Sender<UiWorkerRequest>,
    ) -> usize {
        let epoch = (session_id, target);
        if !lock_recovering(&self.ensured).insert(epoch) {
            return 0;
        }
        let launches = self.launches.launches_for(session_id);
        let mut started = 0;
        for launch in launches {
            if target.is_some_and(|target| {
                !matches!(launch.target(), UiTarget::Shared) && launch.target() != target
            }) {
                continue;
            }
            let key = ActiveWorkerKey {
                session_id,
                plugin_id: launch.plugin_id().to_owned(),
                target: launch.target(),
            };
            let (cancel_tx, cancel_rx) = watch::channel(false);
            {
                let mut active = lock_recovering(&self.active);
                if active.contains_key(&key) {
                    continue;
                }
                if let Some(reason) = worker_quota_denial(&active, &key, launch.memory_limit_mb()) {
                    tracing::warn!(
                        %session_id,
                        plugin = launch.plugin_id(),
                        target = ?launch.target(),
                        reason,
                        "Remote UI worker unavailable because aggregate admission quota is full"
                    );
                    continue;
                }
                active.insert(
                    key.clone(),
                    ActiveWorker {
                        cancellation: cancel_tx.clone(),
                        memory_mb: launch.memory_limit_mb(),
                    },
                );
            }
            started += 1;
            let supervisor = Arc::clone(&self.supervisor);
            let active = Arc::clone(&self.active);
            let ensured = Arc::clone(&self.ensured);
            let broker = broker.clone();
            let requests = requests.clone();
            tokio::spawn(async move {
                let result = run_worker(
                    supervisor,
                    launch,
                    session_id,
                    broker.clone(),
                    requests,
                    cancel_rx,
                )
                .await;
                if let Err(error) = result {
                    tracing::warn!(%session_id, error = %error, "verified Remote UI worker stopped");
                }
                let mut active = lock_recovering(&active);
                // A detach followed immediately by a fresh attach may already
                // have installed a new generation under the same tuple.  The
                // old task must not remove that replacement.
                if active
                    .get(&key)
                    .is_some_and(|current| current.cancellation.same_channel(&cancel_tx))
                {
                    active.remove(&key);
                    lock_recovering(&ensured).remove(&epoch);
                }
            });
        }
        if started == 0 {
            lock_recovering(&self.ensured).remove(&epoch);
        }
        started
    }

    /// Stop every worker owned by a session once its final renderer detaches.
    /// The bridge performs an orderly worker shutdown and producer disposal;
    /// cancellation is idempotent and generation-safe across fast re-attaches.
    pub fn stop_session(&self, session_id: SessionId) -> usize {
        let mut active = lock_recovering(&self.active);
        let keys = active
            .keys()
            .filter(|key| key.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            if let Some(worker) = active.remove(key) {
                let _ = worker.cancellation.send(true);
            }
        }
        lock_recovering(&self.ensured).retain(|(owned_session, _)| *owned_session != session_id);
        keys.len()
    }

    /// Stop only the worker for a renderer target that no longer has a
    /// consumer. Shared workers remain live until the final renderer detaches.
    pub fn stop_session_target(&self, session_id: SessionId, target: UiTarget) -> usize {
        let mut active = lock_recovering(&self.active);
        let keys = active
            .keys()
            .filter(|key| key.session_id == session_id && key.target == target)
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            if let Some(worker) = active.remove(key) {
                let _ = worker.cancellation.send(true);
            }
        }
        lock_recovering(&self.ensured).remove(&(session_id, Some(target)));
        keys.len()
    }

    /// Stop all active generations for a plugin after update/revoke. New
    /// verified launches are minted from persistence rather than reusing stale
    /// descriptors.
    pub fn stop_plugin(&self, plugin_id: &str) -> Vec<(SessionId, UiTarget)> {
        let mut active = lock_recovering(&self.active);
        let keys = active
            .keys()
            .filter(|key| key.plugin_id == plugin_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            if let Some(worker) = active.remove(key) {
                let _ = worker.cancellation.send(true);
            }
        }
        lock_recovering(&self.ensured).clear();
        keys.into_iter()
            .map(|key| (key.session_id, key.target))
            .collect()
    }

    #[must_use]
    pub fn active_count(&self, session_id: SessionId) -> usize {
        lock_recovering(&self.active)
            .keys()
            .filter(|key| key.session_id == session_id)
            .count()
    }

    /// Stop all worker generations during daemon shutdown.
    pub fn shutdown(&self) -> usize {
        let mut active = lock_recovering(&self.active);
        let count = active.len();
        for (_, worker) in active.drain() {
            let _ = worker.cancellation.send(true);
        }
        lock_recovering(&self.ensured).clear();
        count
    }
}

fn worker_quota_denial(
    active: &HashMap<ActiveWorkerKey, ActiveWorker>,
    candidate: &ActiveWorkerKey,
    memory_mb: u64,
) -> Option<&'static str> {
    if active.len() >= MAX_ACTIVE_WORKERS {
        return Some("global worker count");
    }
    if active
        .keys()
        .filter(|key| key.session_id == candidate.session_id)
        .count()
        >= MAX_ACTIVE_WORKERS_PER_SESSION
    {
        return Some("session worker count");
    }
    if active
        .keys()
        .filter(|key| key.plugin_id == candidate.plugin_id)
        .count()
        >= MAX_ACTIVE_WORKERS_PER_PLUGIN
    {
        return Some("plugin worker count");
    }
    if active
        .values()
        .map(|worker| worker.memory_mb)
        .sum::<u64>()
        .saturating_add(memory_mb)
        > MAX_ACTIVE_WORKER_MEMORY_MB
    {
        return Some("aggregate declared memory");
    }
    None
}

async fn run_worker(
    supervisor: Arc<UiWorkerSupervisor>,
    launch: UiWorkerLaunch,
    session_id: SessionId,
    broker: RemoteUiBroker,
    requests: mpsc::Sender<UiWorkerRequest>,
    mut cancellation: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut first_launch = true;
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        let mut offer = broker.producer_offer();
        offer.client = match launch.target() {
            UiTarget::Terminal => codypendent_protocol::UiClientKind::from("terminal"),
            UiTarget::Web => codypendent_protocol::UiClientKind::from("web"),
            UiTarget::Shared => codypendent_protocol::UiClientKind::from("shared"),
        };
        let launch_future = async {
            let circuit_key = format!(
                "{}:{session_id}:{:?}:{}",
                launch.plugin_id(),
                launch.target(),
                launch.generation_key()
            );
            if first_launch {
                supervisor
                    .launch_instance(launch.clone(), offer.clone(), circuit_key)
                    .await
            } else {
                supervisor
                    .restart_instance(launch.clone(), offer.clone(), circuit_key)
                    .await
            }
        };
        let launch_result = tokio::select! {
            result = launch_future => result,
            _ = cancellation.changed() => return Ok(()),
        };
        let mut worker = match launch_result {
            Ok(worker) => worker,
            Err(UiWorkerError::CircuitOpen { remaining, .. })
            | Err(UiWorkerError::RestartBackoff { remaining, .. }) => {
                tokio::select! {
                    () = tokio::time::sleep(remaining) => continue,
                    _ = cancellation.changed() => return Ok(()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        first_launch = false;
        let selection = worker.selection().ok_or_else(|| {
            UiWorkerError::Handshake("missing handshake selection after launch".into())
        })?;
        let producer = broker.register_verified_producer(session_id, &launch, selection)?;
        let mut receiver = broker.subscribe_producer(session_id, &producer)?.receiver;
        let (result, cancelled) = tokio::select! {
            result = bridge_loop(
                &mut worker,
                session_id,
                &broker,
                &producer,
                &requests,
                &mut receiver,
            ) => (result, false),
            _ = cancellation.changed() => (Ok(()), true),
        };
        let _ = broker.dispose_producer(session_id, &producer);
        let _ = requests
            .send(UiWorkerRequest::ProducerStopped {
                session_id,
                producer: producer.clone(),
            })
            .await;
        if cancelled {
            let _ = worker.shutdown().await;
            return Ok(());
        }
        match result {
            Ok(()) => {
                let _ = worker.shutdown().await;
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    %session_id,
                    plugin = launch.plugin_id(),
                    error = %error,
                    "Remote UI worker crashed; supervisor will restart after backoff"
                );
                worker.fail_and_cancel().await;
            }
        }
    }
}

async fn bridge_loop(
    worker: &mut UiWorker,
    session_id: SessionId,
    broker: &RemoteUiBroker,
    producer: &UiProducerHandle,
    requests: &mpsc::Sender<UiWorkerRequest>,
    receiver: &mut broadcast::Receiver<UiBrokerFrame>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            signal = worker.next_signal() => {
                match signal? {
                    UiWorkerSignal::Message(message) => {
                        let dispatch = broker.handle_producer(session_id, producer, *message)?;
                        forward_dispatch(worker, session_id, requests, dispatch).await?;
                    }
                    UiWorkerSignal::ResyncRequested { document_id: Some(document_id), revision, reason } => {
                        worker.request_resync(&document_id, revision, reason.unwrap_or_else(|| "worker requested resync".to_owned())).await?;
                    }
                    UiWorkerSignal::Heartbeat | UiWorkerSignal::Reloaded | UiWorkerSignal::ResyncRequested { document_id: None, .. } => {}
                }
            }
            frame = receiver.recv() => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        anyhow::bail!("Remote UI producer fan-out lagged; worker must restart for a clean baseline")
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                if !matches!(&frame.target, UiBrokerTarget::Producer(target) if target == producer) {
                    continue;
                }
                forward_to_worker(worker, frame.message).await?;
            }
        }
    }
}

async fn forward_dispatch(
    worker: &mut UiWorker,
    session_id: SessionId,
    requests: &mpsc::Sender<UiWorkerRequest>,
    dispatch: UiBrokerDispatch,
) -> anyhow::Result<()> {
    for direct in dispatch.direct {
        forward_to_worker(worker, direct).await?;
    }
    for action in dispatch.actions {
        requests
            .send(UiWorkerRequest::Action { session_id, action })
            .await
            .map_err(|_| anyhow::anyhow!("Remote UI daemon action mediator stopped"))?;
    }
    for subscription in dispatch.subscriptions {
        requests
            .send(UiWorkerRequest::Subscription {
                session_id,
                subscription,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Remote UI daemon projection mediator stopped"))?;
    }
    for unsubscription in dispatch.unsubscriptions {
        requests
            .send(UiWorkerRequest::Unsubscription {
                session_id,
                unsubscription,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Remote UI daemon projection mediator stopped"))?;
    }
    for cancellation in dispatch.cancellations {
        requests
            .send(UiWorkerRequest::Cancellation {
                session_id,
                cancellation,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Remote UI daemon action mediator stopped"))?;
    }
    Ok(())
}

async fn forward_to_worker(
    worker: &mut UiWorker,
    message: codypendent_protocol::UiWireMessage,
) -> anyhow::Result<()> {
    match message.kind.as_str() {
        "event" => {
            worker
                .send_event(message.event.expect("broker event"))
                .await?
        }
        "projection" => {
            worker
                .send_projection(message.projection.expect("broker projection"))
                .await?
        }
        "actionResult" => {
            worker
                .send_action_result(message.action_result.expect("broker result"))
                .await?
        }
        "cancelAction" => {
            worker
                .send_action_cancellation(message.cancellation.expect("broker cancellation"))
                .await?
        }
        "viewport" => {
            worker
                .update_viewport(message.viewport.expect("broker viewport"))
                .await?
        }
        "resync" => {
            let request = message.resync.expect("broker resync");
            worker
                .request_resync(
                    &request.document_id,
                    request.known_revision,
                    "broker resync",
                )
                .await?;
        }
        other => anyhow::bail!("broker emitted unsupported producer message {other:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(memory_mb: u64) -> ActiveWorker {
        let (cancellation, _) = watch::channel(false);
        ActiveWorker {
            cancellation,
            memory_mb,
        }
    }

    /// No launch source is needed: this proves the *registry* half of the
    /// service survives a poisoned mutex, which is what decides whether running
    /// workers can still be cancelled.
    fn service() -> RemoteUiWorkerService {
        struct NoLaunches;
        impl VerifiedUiLaunchSource for NoLaunches {
            fn launches_for(&self, _session_id: SessionId) -> Vec<UiWorkerLaunch> {
                Vec::new()
            }
        }
        let supervisor = Arc::new(
            UiWorkerSupervisor::new(
                Arc::new(codypendent_sandbox::RefusingSandbox),
                codypendent_ui_host::UiWorkerConfig::default(),
            )
            .expect("default config validates"),
        );
        RemoteUiWorkerService::new(supervisor, Arc::new(NoLaunches))
    }

    /// Poison the active-worker registry the only way it can be poisoned — a
    /// panic while holding it — and prove teardown still fires every worker's
    /// cancellation. With `.expect(...)` back in place, `shutdown` panics and
    /// the workers it should have stopped run until the daemon dies.
    #[test]
    fn shutdown_still_cancels_workers_after_the_registry_is_poisoned() {
        let service = service();
        let session = SessionId::new();
        let (cancellation, cancelled) = watch::channel(false);
        lock_recovering(&service.active).insert(
            ActiveWorkerKey {
                session_id: session,
                plugin_id: "stuck".into(),
                target: UiTarget::Terminal,
            },
            ActiveWorker {
                cancellation,
                memory_mb: 1,
            },
        );

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = service.active.lock().expect("fresh mutex");
            panic!("a holder panicked");
        }));
        assert!(service.active.is_poisoned());

        assert_eq!(service.active_count(session), 1);
        assert_eq!(service.shutdown(), 1);
        assert!(*cancelled.borrow(), "the worker was told to stop");
        assert_eq!(service.active_count(session), 0);
    }

    #[test]
    fn attach_storm_is_bounded_by_session_plugin_global_and_memory_quotas() {
        let session = SessionId::new();
        let mut active = HashMap::new();
        for index in 0..MAX_ACTIVE_WORKERS_PER_SESSION {
            active.insert(
                ActiveWorkerKey {
                    session_id: session,
                    plugin_id: format!("plugin-{index}"),
                    target: UiTarget::Terminal,
                },
                entry(1),
            );
        }
        assert_eq!(
            worker_quota_denial(
                &active,
                &ActiveWorkerKey {
                    session_id: session,
                    plugin_id: "overflow".into(),
                    target: UiTarget::Web,
                },
                1,
            ),
            Some("session worker count")
        );

        active.clear();
        for _index in 0..MAX_ACTIVE_WORKERS_PER_PLUGIN {
            active.insert(
                ActiveWorkerKey {
                    session_id: SessionId::new(),
                    plugin_id: "same-plugin".into(),
                    target: UiTarget::Terminal,
                },
                entry(1),
            );
        }
        assert_eq!(
            worker_quota_denial(
                &active,
                &ActiveWorkerKey {
                    session_id: SessionId::new(),
                    plugin_id: "same-plugin".into(),
                    target: UiTarget::Web,
                },
                1,
            ),
            Some("plugin worker count")
        );

        active.clear();
        for index in 0..MAX_ACTIVE_WORKERS {
            active.insert(
                ActiveWorkerKey {
                    session_id: SessionId::new(),
                    plugin_id: format!("global-{index}"),
                    target: UiTarget::Terminal,
                },
                entry(1),
            );
        }
        assert_eq!(
            worker_quota_denial(
                &active,
                &ActiveWorkerKey {
                    session_id: SessionId::new(),
                    plugin_id: "global-overflow".into(),
                    target: UiTarget::Terminal,
                },
                1,
            ),
            Some("global worker count")
        );

        active.clear();
        active.insert(
            ActiveWorkerKey {
                session_id: SessionId::new(),
                plugin_id: "large".into(),
                target: UiTarget::Terminal,
            },
            entry(MAX_ACTIVE_WORKER_MEMORY_MB),
        );
        assert_eq!(
            worker_quota_denial(
                &active,
                &ActiveWorkerKey {
                    session_id: SessionId::new(),
                    plugin_id: "one-more".into(),
                    target: UiTarget::Terminal,
                },
                1,
            ),
            Some("aggregate declared memory")
        );
    }
}

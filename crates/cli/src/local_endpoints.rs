//! Which local model servers are answering right now (P12).
//!
//! The provider catalog knows where Ollama, LM Studio and vLLM listen by
//! default, and the runtime already owns a TCP connect probe
//! ([`TcpConnectProbe`]) — but nothing asked the question before the operator
//! did. First-run setup listed three servers as if any of them might be
//! running, and `doctor` said nothing about local servers at all unless a
//! model was already configured against one.
//!
//! This module asks, for every provider the catalog marks `local`: does a
//! TCP connect to its base URL succeed? Loopback only, a connect only (no
//! HTTP, no model call), every candidate concurrently, bounded by
//! [`PROBE_TIMEOUT`]. A hosted endpoint is never touched before the operator
//! chooses it. The answer is the TUI's [`LocalEndpoint`] projection, the same
//! shape `doctor` reports from.

use std::time::Duration;

use codypendent_protocol::discovery::RuntimePaths;
use codypendent_providers::{Catalog, Protocol};
use codypendent_runtime::models::{
    authority_from_base_url, is_local_base_url, ConnectivityProbe, TcpConnectProbe,
};
use codypendent_tui::state::LocalEndpoint;

/// How long one probe waits. A refused connection returns in microseconds;
/// the timeout only matters for a port something is black-holing, and it
/// bounds what boot can lose to the probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// The probe boot and `doctor` use.
#[must_use]
pub fn default_probe() -> TcpConnectProbe {
    TcpConnectProbe {
        timeout: PROBE_TIMEOUT,
    }
}

/// Probe every local provider in the catalog the CLI would show — the
/// built-ins layered with the user's `providers.toml`, or the built-ins alone
/// when that file does not parse (the picker already warned about it).
pub async fn probe_catalog_defaults(paths: &RuntimePaths) -> Vec<LocalEndpoint> {
    let catalog = Catalog::load_with_user_overrides(&paths.data_dir.join("providers.toml"))
        .unwrap_or_else(|_| Catalog::builtin());
    probe_local_endpoints(&catalog, default_probe()).await
}

/// Probe every provider `catalog` marks `local` (ACP agents excepted — they
/// are launched, not connected to), concurrently, and answer in catalog
/// order. A provider with no base URL, or one whose base URL has no host,
/// is skipped: there is nothing to try.
///
/// The `local` flag alone is NOT enough to earn a connection. It comes from
/// `providers.toml`, which a person edits, so an override marking a LAN or
/// remote host `local = true` had the TUI's boot probe and `doctor` open a
/// socket to it — outbound traffic from merely starting the client, which is
/// what this module's loopback-only promise exists to prevent. The parsed host
/// has to agree.
pub async fn probe_local_endpoints(
    catalog: &Catalog,
    probe: TcpConnectProbe,
) -> Vec<LocalEndpoint> {
    let mut probes = tokio::task::JoinSet::new();
    for (index, provider) in catalog
        .providers()
        .filter(|provider| provider.local && !matches!(provider.protocol, Protocol::Acp))
        .enumerate()
    {
        let Some(base_url) = provider.base_url.clone() else {
            continue;
        };
        let Ok(authority) = authority_from_base_url(&base_url) else {
            continue;
        };
        if !is_local_base_url(&base_url) {
            continue;
        }
        let provider_id = provider.id.clone();
        let probe = probe.clone();
        probes.spawn(async move {
            let reachable = probe.check(&base_url).await.is_ok();
            (
                index,
                LocalEndpoint {
                    provider_id,
                    authority,
                    reachable,
                },
            )
        });
    }
    let mut results = Vec::with_capacity(probes.len());
    while let Some(joined) = probes.join_next().await {
        if let Ok(result) = joined {
            results.push(result);
        }
    }
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, endpoint)| endpoint).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_providers::{AuthMethod, Provider};
    use std::collections::BTreeMap;

    fn provider(id: &str, local: bool, base_url: &str) -> Provider {
        Provider {
            id: id.to_owned(),
            name: id.to_owned(),
            protocol: Protocol::OpenAiChat,
            base_url: Some(base_url.to_owned()),
            auth: vec![AuthMethod::None],
            extra_headers: BTreeMap::new(),
            query_params: BTreeMap::new(),
            local,
        }
    }

    /// A `local = true` override on a REMOTE host earns no connection.
    ///
    /// The flag comes from `providers.toml`, which a person edits. Trusting it
    /// meant merely launching the TUI, or running `doctor`, opened a socket to
    /// whatever host the file named — including one whose name merely contains
    /// a loopback word. Nothing is probed and nothing is reported.
    #[tokio::test]
    async fn a_remote_host_marked_local_is_never_contacted() {
        // Bound and dropped: a connect to this port fails fast rather than
        // hanging, so a probe that DID run would still finish the test.
        let unused = {
            let taken = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            taken.local_addr().expect("addr").port()
        };
        let catalog = Catalog::from_providers(vec![
            provider("lan", true, &format!("http://192.168.1.10:{unused}/v1")),
            provider("public", true, "https://models.example.invalid/v1"),
            provider("looks-local", true, "https://localhost.example.invalid/v1"),
            provider(
                "path-only",
                true,
                &format!("https://example.invalid:{unused}/localhost"),
            ),
        ]);
        let found = probe_local_endpoints(&catalog, default_probe()).await;
        assert!(
            found.is_empty(),
            "a remote host was probed on the strength of its flag: {found:?}"
        );
    }

    /// A real listener answers; a port nothing listens on does not; a hosted
    /// provider is never tried at all. Real sockets, so the connect-refused
    /// path is the OS's, not a mock's.
    #[tokio::test]
    async fn a_listening_port_answers_and_a_closed_one_does_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let open = listener.local_addr().expect("addr").port();
        let closed = {
            let taken = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            taken.local_addr().expect("addr").port()
        };
        let catalog = Catalog::from_providers(vec![
            provider("up", true, &format!("http://127.0.0.1:{open}/v1")),
            provider("down", true, &format!("http://127.0.0.1:{closed}/v1")),
            provider("hosted", false, "https://api.example.invalid/v1"),
            Provider {
                base_url: None,
                ..provider("no-url", true, "")
            },
        ]);

        let endpoints = probe_local_endpoints(&catalog, default_probe()).await;
        // The catalog iterates by id, so that is "catalog order" here.
        assert_eq!(
            endpoints
                .iter()
                .map(|endpoint| endpoint.provider_id.as_str())
                .collect::<Vec<_>>(),
            vec!["down", "up"],
            "catalog order, hosted providers untouched, a provider with no base URL skipped"
        );
        let by_id = |id: &str| {
            endpoints
                .iter()
                .find(|endpoint| endpoint.provider_id == id)
                .expect(id)
        };
        assert!(by_id("up").reachable);
        assert_eq!(by_id("up").authority, format!("127.0.0.1:{open}"));
        assert!(!by_id("down").reachable);
        assert_eq!(by_id("down").authority, format!("127.0.0.1:{closed}"));
        drop(listener);
    }
}

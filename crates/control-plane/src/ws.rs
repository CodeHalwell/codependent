use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{AuthPrincipal, Principal},
    authz::{authorize_repository_action, Action},
    error::ControlPlaneError,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Refused, never honoured. A bearer token in the query string is written to
    /// access logs by `TraceLayer` and by every intermediary proxy. Credentials
    /// belong in the `Authorization` header.
    pub token: Option<String>,
    pub organization_id: Option<Uuid>,
    pub repository_id: Option<Uuid>,
    pub stream: Option<String>,
    pub last_event_id: Option<i64>,
}

/// `ws` is taken as an `Option` deliberately: `WebSocketUpgrade`'s own rejection
/// runs before this function body, and a malformed handshake must not be able to
/// pre-empt (or mask) the credential and authorization checks below. The upgrade
/// is required, just last.
pub async fn ws_handler(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Query(query): Query<WsQuery>,
    ws: Option<WebSocketUpgrade>,
) -> Result<impl IntoResponse, ControlPlaneError> {
    if query.token.is_some() {
        return Err(ControlPlaneError::BadRequest(
            "credentials must be sent in the Authorization header, not the query string"
                .to_string(),
        ));
    }

    // The subscription names its own tenant and repository; nothing is inferred
    // from the principal (the previous code fell back to the caller's first
    // organization, which silently subscribed them to a tenant they never asked
    // for). Both are then authorized before the upgrade completes.
    let org_id = query
        .organization_id
        .ok_or_else(|| ControlPlaneError::BadRequest("organization_id is required".to_string()))?;
    let repo_id = query
        .repository_id
        .ok_or_else(|| ControlPlaneError::BadRequest("repository_id is required".to_string()))?;

    authorize_repository_action(
        state.store.as_ref(),
        &principal,
        org_id,
        repo_id,
        Action::Read,
    )
    .await?;

    // Only once the subscription is authorized does the handshake itself matter.
    let ws =
        ws.ok_or_else(|| ControlPlaneError::BadRequest("websocket upgrade required".to_string()))?;

    let stream_name = query.stream.unwrap_or_else(|| "sync".to_string());
    let last_id = query.last_event_id.unwrap_or(0);

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(
            socket,
            state,
            principal,
            org_id,
            repo_id,
            stream_name,
            last_id,
        )
    }))
}

/// Re-evaluates the subscription's authorization. Connections outlive grants:
/// authorizing once at upgrade let a revoked grant keep receiving events for the
/// life of the socket.
async fn still_authorized(
    state: &AppState,
    principal: &Principal,
    org_id: Uuid,
    repo_id: Uuid,
) -> bool {
    authorize_repository_action(
        state.store.as_ref(),
        principal,
        org_id,
        repo_id,
        Action::Read,
    )
    .await
    .is_ok()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    principal: Principal,
    org_id: Uuid,
    repo_id: Uuid,
    stream_name: String,
    last_id: i64,
) {
    let (mut sender, mut receiver) = socket.split();

    // 1. Replay historical missed events, scoped to the one authorized
    //    repository. Re-checked immediately before the batch is delivered.
    if !still_authorized(&state, &principal, org_id, repo_id).await {
        return;
    }

    match state
        .store
        .query_stream_events(org_id, Some(repo_id), &stream_name, last_id, 100)
        .await
    {
        Ok(historical) => {
            for event in historical {
                let msg_text = match serde_json::to_string(&event) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if sender.send(Message::Text(msg_text)).await.is_err() {
                    return;
                }
            }
        }
        // Fail closed: a store error is not evidence that delivery is permitted.
        Err(_) => return,
    }

    // 2. Subscribe to live broadcast channel
    let mut rx = state.events_tx.subscribe();

    tokio::select! {
        _ = async {
            while let Ok(msg) = rx.recv().await {
                // Events with no repository cannot be proved in scope for this
                // subscription, so they are not delivered.
                if msg.organization_id != org_id
                    || msg.stream != stream_name
                    || msg.repository_id != Some(repo_id)
                {
                    continue;
                }

                // Re-authorize per delivery batch so a grant revoked mid-connection
                // stops delivery instead of streaming until the client disconnects.
                if !still_authorized(&state, &principal, org_id, repo_id).await {
                    break;
                }

                if let Ok(json_str) = serde_json::to_string(&msg) {
                    if sender.send(Message::Text(json_str)).await.is_err() {
                        break;
                    }
                }
            }
        } => {},
        _ = async {
            while let Some(Ok(msg)) = receiver.next().await {
                if let Message::Close(_) = msg {
                    break;
                }
            }
        } => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::{
        config::ControlPlaneConfig,
        storage::MemoryStorageDriver,
        store::{memory::MemoryStore, Organization, Repository, RoleGrant, Store},
    };

    const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

    /// A subscription authorized once at upgrade must not keep delivering after
    /// the grant behind it stops being valid. This drives the predicate the live
    /// loop consults before every delivery batch, across the moment the grant
    /// lapses.
    #[tokio::test]
    async fn authorization_is_re_evaluated_after_upgrade() {
        let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
            .expect("test signing secret must be accepted");
        let store = Arc::new(MemoryStore::new());
        let state = AppState::new(config, store.clone(), Arc::new(MemoryStorageDriver::new()));

        let org_id = Uuid::now_v7();
        let repo_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        let now = chrono::Utc::now();

        store
            .create_organization(Organization {
                id: org_id,
                slug: "acme".to_string(),
                display_name: "Acme".to_string(),
                max_publication_class: "content-shared".to_string(),
                max_classification: "internal".to_string(),
                data_residency: None,
                retention_days: None,
                policy_version: 1,
                created_at: now,
            })
            .await
            .unwrap();

        store
            .create_repository(Repository {
                id: repo_id,
                organization_id: org_id,
                federated_id: "a".repeat(64),
                display_name: "Repo".to_string(),
                max_publication_class: "content-shared".to_string(),
                max_classification: "internal".to_string(),
                policy_version: 1,
                created_at: now,
            })
            .await
            .unwrap();

        // A grant that lapses shortly after the connection is established.
        store
            .create_role_grant(RoleGrant {
                id: Uuid::now_v7(),
                organization_id: org_id,
                user_id: Some(user_id),
                team_id: None,
                repository_id: Some(repo_id),
                role: "observer".to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: now,
                expires_at: Some(now + chrono::Duration::milliseconds(300)),
                revoked_at: None,
            })
            .await
            .unwrap();

        let principal = Principal::User {
            id: user_id,
            email: None,
            display_name: "Subscriber".to_string(),
        };

        assert!(
            still_authorized(&state, &principal, org_id, repo_id).await,
            "a live grant must authorize delivery"
        );

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        assert!(
            !still_authorized(&state, &principal, org_id, repo_id).await,
            "delivery must stop once the grant is no longer valid"
        );
    }

    /// A principal with no grant at all is refused, so the predicate cannot be
    /// satisfied merely by holding an open socket.
    #[tokio::test]
    async fn a_principal_without_a_grant_is_never_authorized() {
        let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
            .expect("test signing secret must be accepted");
        let store = Arc::new(MemoryStore::new());
        let state = AppState::new(config, store.clone(), Arc::new(MemoryStorageDriver::new()));

        let principal = Principal::User {
            id: Uuid::now_v7(),
            email: None,
            display_name: "Stranger".to_string(),
        };

        assert!(!still_authorized(&state, &principal, Uuid::now_v7(), Uuid::now_v7()).await);
    }
}

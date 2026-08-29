use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    audit::AuditRecord,
    error::{identity_link_refused, ControlPlaneError},
    store::*,
};

type SharedSessionTombstoneKey = (Uuid, Uuid, Uuid, String);

#[derive(Default)]
pub struct MemoryStore {
    users: RwLock<HashMap<Uuid, User>>,
    user_identities: RwLock<HashMap<(String, String, String), UserIdentity>>,
    refresh_tokens: RwLock<HashMap<Vec<u8>, UserRefreshToken>>,
    organizations: RwLock<HashMap<Uuid, Organization>>,
    memberships: RwLock<HashMap<(Uuid, Uuid), Membership>>,
    repositories: RwLock<HashMap<Uuid, Repository>>,
    role_grants: RwLock<HashMap<Uuid, RoleGrant>>,
    daemons: RwLock<HashMap<Uuid, Daemon>>,
    pairing_challenges: RwLock<HashMap<Vec<u8>, PairingChallenge>>,
    workload_credentials: RwLock<HashMap<Vec<u8>, WorkloadCredential>>,
    shared_sessions: RwLock<HashMap<Uuid, SharedSession>>,
    shared_session_tombstones: RwLock<HashMap<SharedSessionTombstoneKey, DateTime<Utc>>>,
    sync_receipts: RwLock<HashMap<(Uuid, i64), SyncReceipt>>,
    tombstones: RwLock<HashMap<Uuid, Tombstone>>,
    idempotency_records: RwLock<HashMap<(String, Uuid, String), IdempotencyRecord>>,
    stream_events: RwLock<Vec<StreamEvent>>,
    published_objects: RwLock<HashMap<(Uuid, Vec<u8>), PublishedObject>>,
    audit_records: RwLock<HashMap<Uuid, Vec<AuditRecord>>>, // Keyed by organization_id
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn refresh_descendants(
    tokens: &HashMap<Vec<u8>, UserRefreshToken>,
    root_id: Uuid,
) -> HashSet<Uuid> {
    let mut chain = HashSet::from([root_id]);
    loop {
        let before = chain.len();
        for token in tokens.values() {
            if token
                .rotated_from
                .is_some_and(|parent| chain.contains(&parent))
            {
                chain.insert(token.id);
            }
        }
        if chain.len() == before {
            return chain;
        }
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn is_ready(&self) -> bool {
        true
    }

    async fn create_user(&self, user: User) -> Result<User, ControlPlaneError> {
        let mut users = self.users.write().unwrap();
        users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn get_user(&self, id: Uuid) -> Result<Option<User>, ControlPlaneError> {
        let users = self.users.read().unwrap();
        Ok(users.get(&id).cloned())
    }

    async fn create_user_identity(
        &self,
        identity: UserIdentity,
    ) -> Result<UserIdentity, ControlPlaneError> {
        let mut identities = self.user_identities.write().unwrap();
        let key = (
            identity.provider.clone(),
            identity.issuer.clone(),
            identity.subject.clone(),
        );
        if identities.contains_key(&key) {
            // Same refusal the PostgreSQL store gives for a unique violation on
            // `(provider, issuer, subject)`: telling the caller the identity is
            // taken tells them whose it is not.
            return Err(identity_link_refused());
        }
        identities.insert(key, identity.clone());
        Ok(identity)
    }

    async fn find_user_identity(
        &self,
        provider: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<UserIdentity>, ControlPlaneError> {
        let identities = self.user_identities.read().unwrap();
        let key = (
            provider.to_string(),
            issuer.to_string(),
            subject.to_string(),
        );
        Ok(identities.get(&key).cloned())
    }

    async fn save_refresh_token(&self, token: UserRefreshToken) -> Result<(), ControlPlaneError> {
        let mut tokens = self.refresh_tokens.write().unwrap();
        tokens.insert(token.token_hash.clone(), token);
        Ok(())
    }

    async fn lookup_refresh_token(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<UserRefreshToken>, ControlPlaneError> {
        let tokens = self.refresh_tokens.read().unwrap();
        Ok(tokens.get(token_hash).cloned())
    }

    async fn revoke_refresh_token(&self, id: Uuid) -> Result<(), ControlPlaneError> {
        let mut tokens = self.refresh_tokens.write().unwrap();
        for token in tokens.values_mut() {
            if token.id == id {
                token.revoked_at = Some(Utc::now());
            }
        }
        Ok(())
    }

    async fn revoke_refresh_token_chain(&self, token_hash: &[u8]) -> Result<(), ControlPlaneError> {
        let mut tokens = self.refresh_tokens.write().unwrap();
        if let Some(root_id) = tokens.get(token_hash).map(|token| token.id) {
            let chain = refresh_descendants(&tokens, root_id);
            let now = Utc::now();
            for token in tokens
                .values_mut()
                .filter(|token| chain.contains(&token.id))
            {
                token.revoked_at = Some(now);
            }
        }
        Ok(())
    }

    async fn rotate_refresh_token(
        &self,
        rotation: RefreshRotation,
    ) -> Result<RefreshRotationOutcome, ControlPlaneError> {
        // Keep the authoritative user and token set locked for the complete
        // decision. Two concurrent uses of one token therefore cannot both
        // observe it as active and mint descendants.
        let users = self.users.read().unwrap();
        let mut tokens = self.refresh_tokens.write().unwrap();

        let Some(old) = tokens.get(&rotation.old_token_hash).cloned() else {
            return Ok(RefreshRotationOutcome::Invalid);
        };

        if old.revoked_at.is_some() {
            let chain = refresh_descendants(&tokens, old.id);
            for token in tokens
                .values_mut()
                .filter(|token| chain.contains(&token.id))
            {
                token.revoked_at = Some(rotation.issued_at);
            }
            return Ok(RefreshRotationOutcome::ReuseDetected);
        }
        if old.expires_at <= rotation.issued_at {
            return Ok(RefreshRotationOutcome::Expired);
        }

        let Some(user) = users.get(&old.user_id).cloned() else {
            return Ok(RefreshRotationOutcome::Invalid);
        };
        if user.state != "active" {
            for token in tokens
                .values_mut()
                .filter(|token| token.user_id == old.user_id)
            {
                token.revoked_at = Some(rotation.issued_at);
            }
            return Ok(RefreshRotationOutcome::InactiveUser);
        }

        // The old row was cloned while this write lock was held, so it is still
        // the same active row here.
        if let Some(old_mut) = tokens.get_mut(&rotation.old_token_hash) {
            old_mut.revoked_at = Some(rotation.issued_at);
        }
        tokens.insert(
            rotation.new_token_hash.clone(),
            UserRefreshToken {
                id: rotation.new_id,
                user_id: old.user_id,
                token_hash: rotation.new_token_hash,
                rotated_from: Some(old.id),
                issued_at: rotation.issued_at,
                expires_at: rotation.expires_at,
                revoked_at: None,
                user_agent_digest: rotation.user_agent_digest,
            },
        );

        Ok(RefreshRotationOutcome::Rotated(user))
    }

    async fn create_organization(
        &self,
        org: Organization,
    ) -> Result<Organization, ControlPlaneError> {
        let mut orgs = self.organizations.write().unwrap();
        // Check case-insensitive unique slug
        let lower_slug = org.slug.to_lowercase();
        if orgs.values().any(|o| o.slug.to_lowercase() == lower_slug) {
            return Err(ControlPlaneError::Conflict(
                "organization slug already exists".to_string(),
            ));
        }
        orgs.insert(org.id, org.clone());
        Ok(org)
    }

    async fn get_organization(&self, id: Uuid) -> Result<Option<Organization>, ControlPlaneError> {
        let orgs = self.organizations.read().unwrap();
        Ok(orgs.get(&id).cloned())
    }

    async fn get_organization_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<Organization>, ControlPlaneError> {
        let orgs = self.organizations.read().unwrap();
        let lower = slug.to_lowercase();
        Ok(orgs
            .values()
            .find(|o| o.slug.to_lowercase() == lower)
            .cloned())
    }

    async fn list_user_organizations(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<Organization>, ControlPlaneError> {
        let memberships = self.memberships.read().unwrap();
        let orgs = self.organizations.read().unwrap();
        let user_org_ids: Vec<Uuid> = memberships
            .values()
            .filter(|m| m.user_id == user_id && m.state == "active")
            .map(|m| m.organization_id)
            .collect();

        Ok(orgs
            .values()
            .filter(|o| user_org_ids.contains(&o.id))
            .cloned()
            .collect())
    }

    async fn add_membership(&self, membership: Membership) -> Result<(), ControlPlaneError> {
        let mut memberships = self.memberships.write().unwrap();
        let key = (membership.organization_id, membership.user_id);
        memberships.insert(key, membership);
        Ok(())
    }

    async fn get_membership(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Membership>, ControlPlaneError> {
        let memberships = self.memberships.read().unwrap();
        Ok(memberships.get(&(org_id, user_id)).cloned())
    }

    async fn create_role_grant(&self, grant: RoleGrant) -> Result<RoleGrant, ControlPlaneError> {
        let mut grants = self.role_grants.write().unwrap();
        grants.insert(grant.id, grant.clone());
        Ok(grant)
    }

    async fn list_user_grants(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<RoleGrant>, ControlPlaneError> {
        let grants = self.role_grants.read().unwrap();
        let now = Utc::now();
        Ok(grants
            .values()
            .filter(|g| {
                g.organization_id == org_id
                    && g.user_id == Some(user_id)
                    && g.revoked_at.is_none()
                    && g.expires_at.is_none_or(|exp| exp > now)
            })
            .cloned()
            .collect())
    }

    async fn create_repository(&self, repo: Repository) -> Result<Repository, ControlPlaneError> {
        let mut repos = self.repositories.write().unwrap();
        // Check uniqueness of (organization_id, federated_id)
        if repos.values().any(|r| {
            r.organization_id == repo.organization_id && r.federated_id == repo.federated_id
        }) {
            return Err(ControlPlaneError::Conflict(
                "repository already registered in this organization".to_string(),
            ));
        }
        repos.insert(repo.id, repo.clone());
        Ok(repo)
    }

    async fn get_repository(&self, id: Uuid) -> Result<Option<Repository>, ControlPlaneError> {
        let repos = self.repositories.read().unwrap();
        Ok(repos.get(&id).cloned())
    }

    async fn get_repository_in_org(
        &self,
        org_id: Uuid,
        repo_id: Uuid,
    ) -> Result<Option<Repository>, ControlPlaneError> {
        let repos = self.repositories.read().unwrap();
        Ok(repos
            .get(&repo_id)
            .filter(|r| r.organization_id == org_id)
            .cloned())
    }

    async fn find_repository_by_federated_id(
        &self,
        org_id: Uuid,
        federated_id: &str,
    ) -> Result<Option<Repository>, ControlPlaneError> {
        let repos = self.repositories.read().unwrap();
        Ok(repos
            .values()
            .find(|r| r.organization_id == org_id && r.federated_id == federated_id)
            .cloned())
    }

    async fn list_authorized_repositories(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<Repository>, ControlPlaneError> {
        let grants = self.list_user_grants(org_id, user_id).await?;
        let repos = self.repositories.read().unwrap();

        let has_org_wide_grant = grants.iter().any(|g| g.repository_id.is_none());
        if has_org_wide_grant {
            return Ok(repos
                .values()
                .filter(|r| r.organization_id == org_id)
                .cloned()
                .collect());
        }

        let specific_repo_ids: Vec<Uuid> = grants.iter().filter_map(|g| g.repository_id).collect();
        Ok(repos
            .values()
            .filter(|r| r.organization_id == org_id && specific_repo_ids.contains(&r.id))
            .cloned()
            .collect())
    }

    async fn create_pairing_challenge(
        &self,
        challenge: PairingChallenge,
    ) -> Result<(), ControlPlaneError> {
        let mut challenges = self.pairing_challenges.write().unwrap();
        challenges.insert(challenge.code_hash.clone(), challenge);
        Ok(())
    }

    async fn complete_pairing(
        &self,
        code_hash: &[u8],
        completion: PairingCompletion,
    ) -> Result<Option<PairingChallenge>, ControlPlaneError> {
        // These locks cover the whole state transition. Memory inserts are
        // infallible, so after validation either all three records become
        // visible before the locks are released or none do.
        let users = self.users.read().unwrap();
        let organizations = self.organizations.read().unwrap();
        let memberships = self.memberships.read().unwrap();
        let mut challenges = self.pairing_challenges.write().unwrap();
        let Some(challenge) = challenges.get_mut(code_hash) else {
            return Ok(None);
        };
        if challenge.consumed_at.is_some() || challenge.expires_at <= completion.completed_at {
            return Ok(None);
        }
        if users
            .get(&challenge.initiated_by)
            .is_none_or(|user| user.state != "active")
            || memberships
                .get(&(challenge.organization_id, challenge.initiated_by))
                .is_none_or(|membership| membership.state != "active")
        {
            return Ok(None);
        }

        let scope = super::validated_pairing_scope(challenge, &completion)?;
        if organizations
            .get(&challenge.organization_id)
            .is_none_or(|organization| {
                !super::pairing_scope_fits_organization(&scope, organization)
            })
        {
            return Ok(None);
        }
        let daemon = Daemon {
            id: completion.daemon_id,
            organization_id: challenge.organization_id,
            paired_by: challenge.initiated_by,
            display_name: completion.display_name,
            consent_manifest_hash: completion.consent_manifest_hash,
            max_publication_class: scope.max_publication_class.as_str().to_string(),
            accepts_remote_approvals: scope.accepts_remote_approvals,
            accepts_runner_dispatch: scope.accepts_runner_dispatch,
            state: "active".to_string(),
            paired_at: Some(completion.completed_at),
            revoked_at: None,
            last_seen_at: Some(completion.completed_at),
            created_at: completion.completed_at,
        };
        let credential = WorkloadCredential {
            id: completion.credential_id,
            daemon_id: completion.daemon_id,
            audience: completion.credential_audience,
            purpose: completion.credential_purpose,
            token_hash: completion.credential_token_hash,
            rotated_from: None,
            issued_at: completion.completed_at,
            expires_at: completion.credential_expires_at,
            revoked_at: None,
        };

        let mut daemons = self.daemons.write().unwrap();
        let mut credentials = self.workload_credentials.write().unwrap();
        daemons.insert(daemon.id, daemon);
        credentials.insert(credential.token_hash.clone(), credential);
        challenge.consumed_at = Some(completion.completed_at);
        challenge.daemon_id = Some(completion.daemon_id);
        Ok(Some(challenge.clone()))
    }

    async fn register_daemon(&self, daemon: Daemon) -> Result<Daemon, ControlPlaneError> {
        let mut daemons = self.daemons.write().unwrap();
        daemons.insert(daemon.id, daemon.clone());
        Ok(daemon)
    }

    async fn get_daemon(&self, daemon_id: Uuid) -> Result<Option<Daemon>, ControlPlaneError> {
        let daemons = self.daemons.read().unwrap();
        Ok(daemons.get(&daemon_id).cloned())
    }

    async fn update_daemon_state(
        &self,
        daemon_id: Uuid,
        state: &str,
    ) -> Result<(), ControlPlaneError> {
        let mut daemons = self.daemons.write().unwrap();
        if let Some(daemon) = daemons.get_mut(&daemon_id) {
            daemon.state = state.to_string();
            if state == "revoked" {
                daemon.revoked_at = Some(Utc::now());
            }
        }
        Ok(())
    }

    async fn save_workload_credential(
        &self,
        cred: WorkloadCredential,
    ) -> Result<(), ControlPlaneError> {
        let mut creds = self.workload_credentials.write().unwrap();
        creds.insert(cred.token_hash.clone(), cred);
        Ok(())
    }

    async fn lookup_workload_credential(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<WorkloadCredential>, ControlPlaneError> {
        let creds = self.workload_credentials.read().unwrap();
        let now = Utc::now();
        if let Some(cred) = creds.get(token_hash) {
            if cred.revoked_at.is_none() && cred.expires_at > now {
                return Ok(Some(cred.clone()));
            }
        }
        Ok(None)
    }

    async fn upsert_shared_session(
        &self,
        session: SharedSession,
    ) -> Result<SharedSession, ControlPlaneError> {
        let mut sessions = self.shared_sessions.write().unwrap();
        // Check if there is an existing session for daemon_id and remote_session_key
        if let Some(existing) = sessions.values_mut().find(|s| {
            s.daemon_id == session.daemon_id && s.remote_session_key == session.remote_session_key
        }) {
            existing.class = session.class;
            existing.title = session.title;
            existing.state = session.state;
            existing.last_activity_at = session.last_activity_at;
            existing.updated_at = Utc::now();
            return Ok(existing.clone());
        }

        sessions.insert(session.id, session.clone());
        Ok(session)
    }

    async fn list_shared_sessions(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SharedSession>, ControlPlaneError> {
        let sessions = self.shared_sessions.read().unwrap();
        let mut list: Vec<SharedSession> = sessions
            .values()
            .filter(|s| {
                s.organization_id == org_id && repo_id.is_none_or(|rid| s.repository_id == rid)
            })
            .filter(|s| s.tombstoned_at.is_none())
            .cloned()
            .collect();

        list.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        list.truncate(limit);
        Ok(list)
    }

    async fn record_sync_receipt(&self, receipt: SyncReceipt) -> Result<bool, ControlPlaneError> {
        let mut receipts = self.sync_receipts.write().unwrap();
        let daemon_id = receipt.daemon_id;
        let key = (daemon_id, receipt.daemon_sequence);
        if receipts.contains_key(&key) {
            return Ok(false); // Already recorded (idempotent replay)
        }
        receipts.insert(key, receipt);
        Ok(true)
    }

    async fn apply_sync_delta(
        &self,
        application: SyncDeltaApplication,
    ) -> Result<SyncDeltaOutcome, ControlPlaneError> {
        let SyncDeltaApplication {
            receipt,
            projection,
            event,
        } = application;

        // Every lock this unit of work needs, taken before anything is written
        // and held across all of it — the in-memory stand-in for the
        // transaction the PostgreSQL store opens. There is no `.await` between
        // them, so no other task interleaves, and nothing here can fail
        // partway.
        //
        // Acquired in the order the fields are declared on `MemoryStore`, which
        // is the order every multi-lock method in this file uses; a second
        // ordering is how two of them would deadlock against each other.
        let mut sessions = self.shared_sessions.write().unwrap();
        let mut session_tombstones = self.shared_session_tombstones.write().unwrap();
        let mut receipts = self.sync_receipts.write().unwrap();
        let mut tombstones = self.tombstones.write().unwrap();
        let mut events = self.stream_events.write().unwrap();

        let key = (receipt.daemon_id, receipt.daemon_sequence);
        if receipts.contains_key(&key) {
            return Ok(SyncDeltaOutcome::Duplicate);
        }
        receipts.insert(key, receipt);

        match projection {
            SyncProjection::None => {}
            SyncProjection::SharedSession(mut session) => {
                // A session key is never reusable after its deletion. Delivery
                // can be sparse or out of order (for example after a local
                // policy narrows an older queued summary), so a tombstone that
                // arrived first must dominate a later summary instead of
                // letting that summary resurrect a visible session.
                let tombstone_key = (
                    session.organization_id,
                    session.repository_id,
                    session.daemon_id,
                    session.remote_session_key.clone(),
                );
                if let Some(tombstoned_at) = session_tombstones.get(&tombstone_key).copied() {
                    session.tombstoned_at = Some(tombstoned_at);
                    session.updated_at = session.updated_at.max(tombstoned_at);
                }
                // Upsert on (daemon_id, remote_session_key), matching the
                // PostgreSQL store's ON CONFLICT target rather than the map key.
                let existing = sessions.iter().find_map(|(id, s)| {
                    (s.daemon_id == session.daemon_id
                        && s.remote_session_key == session.remote_session_key)
                        .then_some(*id)
                });
                match existing {
                    Some(id) => {
                        let stored = sessions.get_mut(&id).expect("just located");
                        stored.class = session.class.clone();
                        stored.title = session.title.clone();
                        stored.state = session.state.clone();
                        stored.last_activity_at = session.last_activity_at;
                        stored.updated_at = session.updated_at;
                    }
                    None => {
                        sessions.insert(session.id, *session);
                    }
                }
            }
            SyncProjection::Tombstone {
                record: tombstone,
                shared_session,
            } => {
                if let Some(target) = shared_session {
                    let tombstone_key = (
                        target.organization_id,
                        target.repository_id,
                        target.daemon_id,
                        target.remote_session_key.clone(),
                    );
                    session_tombstones
                        .entry(tombstone_key)
                        .and_modify(|stored| *stored = (*stored).max(target.tombstoned_at))
                        .or_insert(target.tombstoned_at);
                    if let Some(stored) = sessions.values_mut().find(|session| {
                        session.organization_id == target.organization_id
                            && session.repository_id == target.repository_id
                            && session.daemon_id == target.daemon_id
                            && session.remote_session_key == target.remote_session_key
                    }) {
                        stored.tombstoned_at = Some(target.tombstoned_at);
                        stored.updated_at = target.tombstoned_at;
                    }
                }
                let duplicate = tombstones.values().any(|t| {
                    t.organization_id == tombstone.organization_id
                        && t.subject_kind == tombstone.subject_kind
                        && t.subject_key == tombstone.subject_key
                        && t.created_at == tombstone.created_at
                });
                if !duplicate {
                    tombstones.insert(tombstone.id, *tombstone);
                }
            }
        }

        let mut appended = event;
        appended.id = i64::try_from(events.len() + 1).unwrap_or(i64::MAX);
        events.push(appended.clone());

        Ok(SyncDeltaOutcome::Applied(Box::new(appended)))
    }

    async fn get_sync_receipt(
        &self,
        daemon_id: Uuid,
        daemon_sequence: i64,
    ) -> Result<Option<SyncReceipt>, ControlPlaneError> {
        let receipts = self.sync_receipts.read().unwrap();
        Ok(receipts.get(&(daemon_id, daemon_sequence)).cloned())
    }

    async fn latest_sync_sequence(
        &self,
        daemon_id: Uuid,
    ) -> Result<Option<i64>, ControlPlaneError> {
        let receipts = self.sync_receipts.read().unwrap();
        Ok(receipts
            .keys()
            .filter(|(id, _)| *id == daemon_id)
            .map(|(_, seq)| *seq)
            .max())
    }

    async fn create_tombstone(&self, tombstone: Tombstone) -> Result<(), ControlPlaneError> {
        let mut tombstones = self.tombstones.write().unwrap();
        tombstones.insert(tombstone.id, tombstone);
        Ok(())
    }

    async fn list_tombstones(
        &self,
        org_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<Tombstone>, ControlPlaneError> {
        let tombstones = self.tombstones.read().unwrap();
        Ok(tombstones
            .values()
            .filter(|t| t.organization_id == org_id && t.created_at >= since)
            .cloned()
            .collect())
    }

    async fn get_idempotency_record(
        &self,
        principal_kind: &str,
        principal_id: Uuid,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, ControlPlaneError> {
        let records = self.idempotency_records.read().unwrap();
        let k = (principal_kind.to_string(), principal_id, key.to_string());
        let now = Utc::now();
        // Matches the PostgreSQL store's `expires_at > now()` predicate. Without
        // it the two stores disagree about whether a stale response is still
        // replayable.
        Ok(records.get(&k).filter(|r| r.expires_at > now).cloned())
    }

    async fn save_idempotency_record(
        &self,
        record: IdempotencyRecord,
    ) -> Result<bool, ControlPlaneError> {
        let mut records = self.idempotency_records.write().unwrap();
        let k = (
            record.principal_kind.clone(),
            record.principal_id,
            record.key.clone(),
        );
        let now = Utc::now();
        // First writer wins, mirroring `ON CONFLICT DO NOTHING`. An expired entry
        // is not a live entry, so it may be replaced.
        if records
            .get(&k)
            .is_some_and(|existing| existing.expires_at > now)
        {
            return Ok(false);
        }
        records.insert(k, record);
        Ok(true)
    }

    async fn append_stream_event(
        &self,
        mut event: StreamEvent,
    ) -> Result<StreamEvent, ControlPlaneError> {
        let mut events = self.stream_events.write().unwrap();
        let next_id = (events.len() as i64) + 1;
        event.id = next_id;
        events.push(event.clone());
        Ok(event)
    }

    async fn query_stream_events(
        &self,
        org_id: Uuid,
        repo_id: Option<Uuid>,
        stream: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, ControlPlaneError> {
        let events = self.stream_events.read().unwrap();
        let mut filtered: Vec<StreamEvent> = events
            .iter()
            .filter(|e| {
                e.organization_id == org_id
                    && repo_id.is_none_or(|repository_id| e.repository_id == Some(repository_id))
            })
            .filter(|e| e.stream == stream && e.id > after_id)
            .cloned()
            .collect();

        filtered.truncate(limit);
        Ok(filtered)
    }

    async fn record_published_object(
        &self,
        obj: PublishedObject,
    ) -> Result<PublishedObject, ControlPlaneError> {
        let mut objects = self.published_objects.write().unwrap();
        let key = (obj.organization_id, obj.content_hash.clone());
        objects.insert(key, obj.clone());
        Ok(obj)
    }

    async fn get_published_object(
        &self,
        org_id: Uuid,
        content_hash: &[u8],
    ) -> Result<Option<PublishedObject>, ControlPlaneError> {
        let objects = self.published_objects.read().unwrap();
        let key = (org_id, content_hash.to_vec());
        Ok(objects.get(&key).cloned())
    }

    async fn update_object_state(&self, id: Uuid, state: &str) -> Result<(), ControlPlaneError> {
        let mut objects = self.published_objects.write().unwrap();
        for obj in objects.values_mut() {
            if obj.id == id {
                obj.state = state.to_string();
            }
        }
        Ok(())
    }

    async fn append_audit_record(
        &self,
        mut record: AuditRecord,
    ) -> Result<AuditRecord, ControlPlaneError> {
        // The write guard is held across the read of the tail and the push, so
        // concurrent appends serialize — the same guarantee the PostgreSQL store
        // gets from a per-organization advisory lock inside a transaction.
        let mut audit_map = self.audit_records.write().unwrap();
        let org_chain = audit_map
            .entry(record.organization_id.as_uuid())
            .or_default();

        let tail = org_chain.last();
        let prev_hash = tail.map(|r| r.record_hash.clone());
        record.prev_hash = prev_hash.clone();
        // Same normalization the PostgreSQL store applies, so both stores hash
        // and order identical input identically.
        record.occurred_at =
            chain_ordered_timestamp(record.occurred_at, tail.map(|r| r.occurred_at));

        // The protocol's hash function, the only one there is.
        record.record_hash = AuditRecord::compute_hash(
            &record.organization_id,
            record.actor_kind,
            record.actor_id.as_deref(),
            &record.action,
            &record.target_kind,
            &record.target_id,
            &record.action_digest,
            record.correlation_id.as_ref(),
            prev_hash.as_ref(),
            &record.detail,
            &record.occurred_at,
        );

        org_chain.push(record.clone());
        Ok(record)
    }

    async fn get_latest_audit_record(
        &self,
        org_id: Uuid,
    ) -> Result<Option<AuditRecord>, ControlPlaneError> {
        let audit_map = self.audit_records.read().unwrap();
        Ok(audit_map
            .get(&org_id)
            .and_then(|chain| chain.last().cloned()))
    }

    async fn list_audit_records(
        &self,
        org_id: Uuid,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, ControlPlaneError> {
        let audit_map = self.audit_records.read().unwrap();
        if let Some(chain) = audit_map.get(&org_id) {
            let mut list = chain.clone();
            list.reverse();
            list.truncate(limit);
            Ok(list)
        } else {
            Ok(vec![])
        }
    }
}

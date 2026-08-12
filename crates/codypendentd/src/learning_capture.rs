//! Conservative bridge from a completed run into the curated learning ledger.
//!
//! This intentionally does **not** reuse the legacy memory harvest. That path
//! accepts broad heuristic and model-extracted observations; the governed
//! learning ledger is narrower. It receives only:
//!
//! * an explicit preference/correction authored in the run's user objective;
//! * a small allow-list of local verification commands that actually succeeded.
//!
//! Raw tool output, artifacts, assistant prose, web content, and logs are never
//! opened here. The generic persistence helper still fails closed through
//! `LearningStore`, so a future untrusted candidate can only be proposed.

use std::collections::HashSet;

use chrono::{Duration, Utc};
use codypendent_knowledge::{
    ActivationIntent, CaptureOutcome, LearningContent, LearningError, LearningProvenance,
    LearningScope, LearningState, LearningStore, NewLearning,
};
use codypendent_protocol::{
    Actor, EventBody, LearningId, RepositoryId, RunDisposition, RunId, SessionEvent, SessionId,
    ToolOutcome, UserId,
};
use sqlx::SqlitePool;

/// IDs safe to project to clients after one learning pass. Content and
/// provenance deliberately stay behind the governed store's query boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LearningCaptureReport {
    pub(crate) proposed_ids: Vec<LearningId>,
    pub(crate) activated_ids: Vec<LearningId>,
}

impl LearningCaptureReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.proposed_ids.is_empty() && self.activated_ids.is_empty()
    }
}

/// Capture the narrow set of durable learnings supported by the daemon today.
/// Failed/cancelled/incomplete runs produce nothing, and replay is idempotent
/// because `LearningStore` deduplicates normalized content inside each scope.
pub(crate) async fn capture_completed_run(
    pool: &SqlitePool,
    events: &[SessionEvent],
    session_id: SessionId,
    run_id: RunId,
    repository: RepositoryId,
) -> Result<LearningCaptureReport, LearningError> {
    if !events.iter().any(|event| {
        matches!(
            &event.body,
            EventBody::RunCompleted {
                run_id: completed,
                disposition: RunDisposition::Completed { .. },
                ..
            } if *completed == run_id
        )
    }) {
        return Ok(LearningCaptureReport::default());
    }

    let mut candidates = Vec::new();
    if let Some(candidate) = direct_user_candidate(events, run_id, repository) {
        candidates.push(candidate);
    }
    candidates.extend(verified_command_candidates(
        events, session_id, run_id, repository,
    ));
    capture_candidates(pool, candidates).await
}

/// Persist candidates through one policy gate. Kept separate from extraction so
/// every future producer gets the same activation, dedupe, and secret rules.
async fn capture_candidates(
    pool: &SqlitePool,
    candidates: impl IntoIterator<Item = NewLearning>,
) -> Result<LearningCaptureReport, LearningError> {
    let store = LearningStore::new();
    let mut report = LearningCaptureReport::default();
    for candidate in candidates {
        match store.capture(pool, candidate).await? {
            CaptureOutcome::Stored(record) | CaptureOutcome::Conflict { record, .. } => {
                match record.state {
                    LearningState::Active => report.activated_ids.push(record.id),
                    LearningState::Proposed => report.proposed_ids.push(record.id),
                    // Capture never creates a rejected row.
                    LearningState::Rejected => {}
                }
            }
            CaptureOutcome::Duplicate { .. } | CaptureOutcome::PolicyRejected { .. } => {}
        }
    }
    Ok(report)
}

/// Recognize only explicit durable-language forms. Ordinary requests such as
/// "fix the tests", greetings, and completion acknowledgements are not facts.
fn direct_user_candidate(
    events: &[SessionEvent],
    run_id: RunId,
    repository: RepositoryId,
) -> Option<NewLearning> {
    let (client, objective) =
        events
            .iter()
            .find_map(|event| match (&event.actor, &event.body) {
                (
                    Actor::Client { client_id },
                    EventBody::RunStarted {
                        run_id: started,
                        objective,
                        ..
                    },
                ) if *started == run_id => Some((*client_id, objective.as_str())),
                _ => None,
            })?;
    let statement = objective.split_whitespace().collect::<Vec<_>>().join(" ");
    if statement.is_empty() || statement.len() > 480 {
        return None;
    }
    let lower = statement.to_ascii_lowercase();
    let is_preference = [
        "i prefer ",
        "my preference is ",
        "please always ",
        "always use ",
        "remember that ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    let is_correction = ["correction:", "no, use ", "instead, use "]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || (lower.starts_with("actually,")
            && [
                " use ", " prefer ", " don't ", " do not ", " must ", " should ",
            ]
            .iter()
            .any(|needle| lower.contains(needle)))
        || lower.contains(" instead of ");
    if !is_preference && !is_correction {
        return None;
    }

    let repository_scoped = is_correction
        || [
            "this repo",
            "this repository",
            "this project",
            "the codebase",
            "for this repo",
            "for this project",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
    let author = UserId(client.to_string());
    Some(NewLearning {
        scope: if repository_scoped {
            LearningScope::Repository(repository)
        } else {
            // The daemon is per-user. Keep the stable user scope used by global
            // skills while retaining the exact client identity in provenance.
            LearningScope::User(UserId("local".to_owned()))
        },
        content: LearningContent::Fact {
            statement,
            structured_value: None,
        },
        conflict_key: None,
        provenance: vec![LearningProvenance::UserStatement { user: author }],
        confidence: 0.95,
        expires_at: None,
        activation: ActivationIntent::ActivateIfTrusted,
    })
}

/// Learn only that a canonical, locally executed check passed. Labels are used
/// solely to select an allow-listed constant; arbitrary label text is never
/// persisted. The short expiry reflects that repository health changes.
fn verified_command_candidates(
    events: &[SessionEvent],
    session_id: SessionId,
    run_id: RunId,
    repository: RepositoryId,
) -> Vec<NewLearning> {
    let mut pending: Option<&'static str> = None;
    let mut observed = HashSet::new();
    let mut candidates = Vec::new();
    for event in events {
        match &event.body {
            EventBody::ToolStarted {
                run_id: started,
                tool,
                label,
                ..
            } if *started == run_id && tool == "shell.run" => {
                pending = label.as_deref().and_then(canonical_verification_command);
            }
            EventBody::ToolCompleted {
                run_id: completed,
                tool,
                outcome,
                ..
            } if *completed == run_id && tool == "shell.run" => {
                let verified = pending.take();
                if matches!(outcome, ToolOutcome::Succeeded) {
                    if let Some(command) = verified.filter(|command| observed.insert(*command)) {
                        candidates.push(NewLearning {
                            scope: LearningScope::Repository(repository),
                            content: LearningContent::Fact {
                                statement: format!(
                                    "Repository verification command `{command}` completed successfully."
                                ),
                                structured_value: Some(serde_json::json!({"command": command})),
                            },
                            conflict_key: Some(format!("verification:{command}")),
                            provenance: vec![LearningProvenance::SuccessfulCommand {
                                session: session_id,
                                command_summary: command.to_owned(),
                            }],
                            confidence: 0.9,
                            expires_at: Some(Utc::now() + Duration::days(30)),
                            activation: ActivationIntent::ActivateIfTrusted,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    candidates
}

fn canonical_verification_command(label: &str) -> Option<&'static str> {
    let lower = label.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.contains([';', '|', '&', '>', '<', '$', '`', '\n', '\r']) {
        return None;
    }
    let words = lower.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["cargo", "test", ..] => Some("cargo test"),
        ["cargo", "nextest", "run", ..] => Some("cargo nextest run"),
        ["npm", "test", ..] => Some("npm test"),
        ["npm", "run", "test", ..] => Some("npm run test"),
        ["pnpm", "test", ..] => Some("pnpm test"),
        ["yarn", "test", ..] => Some("yarn test"),
        ["pytest", ..] => Some("pytest"),
        ["python", "-m", "pytest", ..] | ["python3", "-m", "pytest", ..] => {
            Some("python -m pytest")
        }
        ["go", "test", ..] => Some("go test"),
        ["swift", "test", ..] => Some("swift test"),
        ["mix", "test", ..] => Some("mix test"),
        ["dotnet", "test", ..] => Some("dotnet test"),
        ["make", "test", ..] => Some("make test"),
        ["just", "test", ..] => Some("just test"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::db;
    use codypendent_protocol::{
        AgentMode, ArtifactId, ArtifactRef, ClientId, DataClassification, RunDisposition,
    };

    fn event(sequence: u64, actor: Actor, body: EventBody) -> SessionEvent {
        SessionEvent {
            sequence,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor,
            body,
        }
    }

    fn completed_events(run_id: RunId, objective: &str) -> Vec<SessionEvent> {
        vec![
            event(
                1,
                Actor::Client {
                    client_id: ClientId::new(),
                },
                EventBody::RunStarted {
                    run_id,
                    objective: objective.to_owned(),
                    mode: AgentMode::Build,
                },
            ),
            event(
                2,
                Actor::System,
                EventBody::RunCompleted {
                    run_id,
                    disposition: RunDisposition::Completed {
                        summary: Some("done".to_owned()),
                    },
                    chronicle: ArtifactRef {
                        id: ArtifactId::new(),
                        media_type: "application/json".to_owned(),
                        byte_length: 0,
                        sha256: "0".repeat(64),
                        sensitivity: DataClassification::Internal,
                    },
                },
            ),
        ]
    }

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = db::open_database(&dir.path().join("test.db"))
            .await
            .expect("database");
        (dir, pool)
    }

    #[tokio::test]
    async fn greetings_and_generic_completions_yield_zero() {
        let (_dir, pool) = test_pool().await;
        for objective in ["Hello", "What can I help you with?", "Done"] {
            let run = RunId::new();
            let report = capture_completed_run(
                &pool,
                &completed_events(run, objective),
                SessionId::new(),
                run,
                RepositoryId::new(),
            )
            .await
            .expect("capture");
            assert!(report.is_empty(), "unexpected learning for {objective:?}");
        }
    }

    #[tokio::test]
    async fn verified_user_correction_yields_one_active_fact() {
        let (_dir, pool) = test_pool().await;
        let run = RunId::new();
        let repository = RepositoryId::new();
        let report = capture_completed_run(
            &pool,
            &completed_events(run, "Actually, use cargo nextest instead of cargo test."),
            SessionId::new(),
            run,
            repository,
        )
        .await
        .expect("capture");
        assert_eq!(report.activated_ids.len(), 1);
        assert!(report.proposed_ids.is_empty());
        let record = LearningStore::new()
            .get(&pool, report.activated_ids[0])
            .await
            .expect("get")
            .expect("record");
        assert_eq!(record.scope, LearningScope::Repository(repository));
        assert_eq!(record.state, LearningState::Active);
    }

    #[tokio::test]
    async fn untrusted_tool_output_can_only_remain_proposed() {
        let (_dir, pool) = test_pool().await;
        let report = capture_candidates(
            &pool,
            [NewLearning {
                scope: LearningScope::Repository(RepositoryId::new()),
                content: LearningContent::Fact {
                    statement: "The repository convention uses cargo nextest.".to_owned(),
                    structured_value: None,
                },
                conflict_key: None,
                provenance: vec![LearningProvenance::ToolOutput {
                    tool: "shell.run".to_owned(),
                }],
                confidence: 0.99,
                expires_at: None,
                activation: ActivationIntent::ActivateIfTrusted,
            }],
        )
        .await
        .expect("capture");
        assert_eq!(report.proposed_ids.len(), 1);
        assert!(report.activated_ids.is_empty());
    }

    #[tokio::test]
    async fn rerunning_capture_is_idempotent() {
        let (_dir, pool) = test_pool().await;
        let run = RunId::new();
        let events = completed_events(run, "I prefer concise progress updates.");
        let session = SessionId::new();
        let repository = RepositoryId::new();
        let first = capture_completed_run(&pool, &events, session, run, repository)
            .await
            .expect("first");
        let second = capture_completed_run(&pool, &events, session, run, repository)
            .await
            .expect("second");
        assert_eq!(first.activated_ids.len(), 1);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn successful_allowlisted_check_is_active_without_copying_its_label() {
        let (_dir, pool) = test_pool().await;
        let run = RunId::new();
        let mut events = completed_events(run, "Run the tests");
        events.insert(
            1,
            event(
                2,
                Actor::System,
                EventBody::ToolStarted {
                    run_id: run,
                    tool: "shell.run".to_owned(),
                    args_digest: "digest".to_owned(),
                    label: Some("cargo test --workspace".to_owned()),
                },
            ),
        );
        events.insert(
            2,
            event(
                3,
                Actor::System,
                EventBody::ToolCompleted {
                    run_id: run,
                    tool: "shell.run".to_owned(),
                    outcome: ToolOutcome::Succeeded,
                    artifact: None,
                },
            ),
        );
        let report =
            capture_completed_run(&pool, &events, SessionId::new(), run, RepositoryId::new())
                .await
                .expect("capture");
        assert_eq!(report.activated_ids.len(), 1);
        let record = LearningStore::new()
            .get(&pool, report.activated_ids[0])
            .await
            .expect("get")
            .expect("record");
        assert_eq!(
            record.content.summary(),
            "Repository verification command `cargo test` completed successfully."
        );
    }
}

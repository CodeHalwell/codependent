//! Persisted, multi-provider agent councils.
//!
//! A council is deliberately composed from ordinary model profiles. Each member
//! receives an independent, read-only daemon session, so native models and ACP
//! agents use the exact same durable execution path as a normal run. Their
//! bounded responses are then supplied to a separately pinned chair model for
//! synthesis. No provider-specific shortcut or hidden credential store exists.

use std::collections::{BTreeSet, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    AgentMode, ClientRole, CommandBody, EventBody, MessageId, ModelId, Payload, RunDisposition,
    RunId, RunState, SessionId, Subscription, WorkspaceId,
};
use codypendent_runtime::models::load_models;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::commands::{ensure_daemon, expect_catchup};
use crate::connection::Connection;

const SCHEMA_VERSION: u32 = 1;
const MAX_COUNCILS: usize = 64;
const MAX_MEMBERS: usize = 8;
const MAX_ROUNDS: u8 = 3;
const MAX_OBJECTIVE_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_DOSSIER_BYTES: usize = 384 * 1024;
const MEMBER_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CouncilMember {
    pub model: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CouncilDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub chair: String,
    #[serde(default = "default_rounds")]
    pub rounds: u8,
    pub members: Vec<CouncilMember>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CouncilFile {
    #[serde(default = "schema_version")]
    schema_version: u32,
    #[serde(default, rename = "council")]
    councils: Vec<CouncilDefinition>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberOutcome {
    model: String,
    role: String,
    session_id: SessionId,
    run_id: RunId,
    response: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CouncilOutcome {
    council: String,
    objective: String,
    rounds: u8,
    members: Vec<MemberOutcome>,
    chair: MemberOutcome,
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

fn default_rounds() -> u8 {
    1
}

fn council_path(paths: &RuntimePaths) -> PathBuf {
    paths.config_dir.join("councils.toml")
}

pub fn parse_member(value: &str) -> anyhow::Result<CouncilMember> {
    let (model, role) = value
        .split_once('=')
        .map_or((value, "member"), |(model, role)| (model, role));
    let model = model.trim();
    let role = role.trim();
    if model.is_empty() || model.len() > 128 || contains_unsafe_control(model) {
        bail!("council member model must contain 1..=128 safe characters");
    }
    if role.is_empty() || role.len() > 80 || contains_unsafe_control(role) {
        bail!("council member role must contain 1..=80 safe characters");
    }
    Ok(CouncilMember {
        model: model.to_owned(),
        role: role.to_owned(),
    })
}

pub fn create(
    paths: &RuntimePaths,
    name: String,
    members: Vec<String>,
    chair: String,
    rounds: u8,
    description: Option<String>,
) -> anyhow::Result<()> {
    let members = members
        .iter()
        .map(|value| parse_member(value))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let definition = CouncilDefinition {
        name,
        description: description.unwrap_or_default().trim().to_owned(),
        chair: chair.trim().to_owned(),
        rounds,
        members,
    };
    validate_definition(paths, &definition)?;

    let path = council_path(paths);
    let mut file = load_file(&path)?;
    if file
        .councils
        .iter()
        .any(|council| council.name == definition.name)
    {
        bail!(
            "council `{}` already exists; remove it first to replace its membership",
            definition.name
        );
    }
    if file.councils.len() >= MAX_COUNCILS {
        bail!("at most {MAX_COUNCILS} councils may be configured");
    }
    file.councils.push(definition.clone());
    file.councils.sort_by(|a, b| a.name.cmp(&b.name));
    save_file(&path, &file)?;
    println!(
        "created council `{}` with {} members; chair `{}`; {} round(s)",
        definition.name,
        definition.members.len(),
        definition.chair,
        definition.rounds
    );
    Ok(())
}

pub fn list(paths: &RuntimePaths, json: bool) -> anyhow::Result<()> {
    let file = load_file(&council_path(paths))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&file.councils)?);
        return Ok(());
    }
    if file.councils.is_empty() {
        println!("no councils configured; run `codypendent council create --help`");
        return Ok(());
    }
    println!("{:<24} {:<8} {:<24} MEMBERS", "COUNCIL", "ROUNDS", "CHAIR");
    for council in file.councils {
        let members = council
            .members
            .iter()
            .map(|member| format!("{}={}", member.model, member.role))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{:<24} {:<8} {:<24} {}",
            council.name, council.rounds, council.chair, members
        );
    }
    Ok(())
}

pub fn show(paths: &RuntimePaths, name: &str, json: bool) -> anyhow::Result<()> {
    let definition = find(paths, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&definition)?);
        return Ok(());
    }
    println!("Council: {}", definition.name);
    if !definition.description.is_empty() {
        println!("Purpose: {}", definition.description);
    }
    println!("Chair: {}", definition.chair);
    println!("Rounds: {}", definition.rounds);
    println!("Members:");
    for member in definition.members {
        println!("  - {} · {}", member.model, member.role);
    }
    Ok(())
}

pub fn remove(paths: &RuntimePaths, name: &str) -> anyhow::Result<()> {
    let path = council_path(paths);
    let mut file = load_file(&path)?;
    let before = file.councils.len();
    file.councils.retain(|council| council.name != name);
    if file.councils.len() == before {
        bail!("council `{name}` is not configured");
    }
    save_file(&path, &file)?;
    println!("removed council `{name}`");
    Ok(())
}

pub async fn run(
    paths: &RuntimePaths,
    name: &str,
    objective: String,
    repository: PathBuf,
    json: bool,
) -> anyhow::Result<()> {
    validate_objective(&objective)?;
    let definition = find(paths, name)?;
    validate_definition(paths, &definition)?;
    let repository = repository
        .canonicalize()
        .with_context(|| format!("invalid repository {}", repository.display()))?;
    if !repository.is_dir() {
        bail!("repository {} is not a directory", repository.display());
    }
    ensure_daemon(paths).await?;

    let repo = repository.to_string_lossy().into_owned();
    let mut latest = deliberate_round(paths, &definition, &objective, &repo, None, 1).await?;
    for round in 2..=definition.rounds {
        let dossier = dossier(&latest)?;
        latest =
            deliberate_round(paths, &definition, &objective, &repo, Some(&dossier), round).await?;
    }
    if latest.len() < 2 {
        bail!("council quorum failed: fewer than two members completed");
    }

    let dossier = dossier(&latest)?;
    let chair_prompt = synthesis_prompt(&definition, &objective, &dossier);
    eprintln!(
        "codypendent: council `{}` asking chair `{}` to synthesize",
        definition.name, definition.chair
    );
    let chair = run_pinned(
        paths.clone(),
        definition.chair.clone(),
        "chair".to_string(),
        chair_prompt,
        repo,
    )
    .await
    .with_context(|| format!("council chair `{}` failed", definition.chair))?;

    let outcome = CouncilOutcome {
        council: definition.name,
        objective,
        rounds: definition.rounds,
        members: latest,
        chair,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        let safe_response = codypendent_tui::sanitize_accessible_text(&outcome.chair.response);
        println!("Council `{}` · final synthesis", outcome.council);
        println!("{}", safe_response.trim());
        println!("\nParticipants:");
        for member in &outcome.members {
            println!(
                "  - {} · {} · session {} · run {}",
                member.model, member.role, member.session_id, member.run_id
            );
        }
        println!(
            "  - {} · chair · session {} · run {}",
            outcome.chair.model, outcome.chair.session_id, outcome.chair.run_id
        );
    }
    Ok(())
}

async fn deliberate_round(
    paths: &RuntimePaths,
    definition: &CouncilDefinition,
    objective: &str,
    repository: &str,
    prior: Option<&str>,
    round: u8,
) -> anyhow::Result<Vec<MemberOutcome>> {
    eprintln!(
        "codypendent: council `{}` round {round}/{} · launching {} members",
        definition.name,
        definition.rounds,
        definition.members.len()
    );
    let mut tasks = JoinSet::new();
    for member in &definition.members {
        let prompt = member_prompt(definition, member, objective, prior, round);
        tasks.spawn(run_pinned(
            paths.clone(),
            member.model.clone(),
            member.role.clone(),
            prompt,
            repository.to_owned(),
        ));
    }

    let mut successes = Vec::new();
    let mut failures = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(outcome)) => {
                eprintln!(
                    "codypendent: council round {round} · {} ({}) completed",
                    outcome.role, outcome.model
                );
                successes.push(outcome);
            }
            Ok(Err(error)) => failures.push(error.to_string()),
            Err(error) => failures.push(format!("member task failed: {error}")),
        }
    }
    successes.sort_by(|a, b| a.model.cmp(&b.model).then(a.role.cmp(&b.role)));
    if successes.len() < 2 {
        bail!(
            "council round {round} failed quorum ({} of {} completed): {}",
            successes.len(),
            definition.members.len(),
            failures.join("; ")
        );
    }
    if !failures.is_empty() {
        eprintln!(
            "codypendent: council round {round} continuing with quorum; {} member(s) failed: {}",
            failures.len(),
            failures.join("; ")
        );
    }
    Ok(successes)
}

async fn run_pinned(
    paths: RuntimePaths,
    model: String,
    role: String,
    prompt: String,
    repository: String,
) -> anyhow::Result<MemberOutcome> {
    let mut conn = Connection::connect(&paths.socket_path).await?;
    conn.handshake("codypendent-council", env!("CARGO_PKG_VERSION"), None)
        .await?;
    let create = conn
        .send_command(CommandBody::CreateSession {
            workspace: WorkspaceId::new(),
            title: bounded(&format!("Council · {role} · {model}"), 256),
            repository: Some(repository.clone()),
        })
        .await?;
    let session_id = match create.payload {
        Payload::CommandAccepted { .. } => create
            .session_id
            .ok_or_else(|| anyhow!("CreateSession omitted session_id"))?,
        Payload::CommandRejected(error) => bail!("CreateSession: {}", error.message),
        other => bail!("unexpected CreateSession reply: {other:?}"),
    };
    let attach = conn
        .send_command(CommandBody::AttachSession {
            session_id,
            last_seen_sequence: None,
            subscriptions: vec![Subscription::SessionSummary, Subscription::AgentActivity],
            requested_role: ClientRole::Controller,
            repository: Some(repository.clone()),
        })
        .await?;
    let _ = expect_catchup(attach)?;
    let start = conn
        .send_command(CommandBody::StartRun {
            session_id,
            objective: prompt,
            mode: AgentMode::Ask,
            repository: Some(repository),
            model: Some(ModelId(model.clone())),
        })
        .await?;
    let run_id = match start.payload {
        Payload::CommandAccepted {
            created_run: Some(run_id),
            ..
        } => run_id,
        Payload::CommandRejected(error) => bail!("model `{model}`: {}", error.message),
        other => bail!("model `{model}` returned unexpected StartRun reply: {other:?}"),
    };

    let collect = collect_run(&mut conn, run_id);
    let response = match tokio::time::timeout(MEMBER_TIMEOUT, collect).await {
        Ok(result) => result?,
        Err(_) => {
            let _ = conn.send_command(CommandBody::CancelRun { run_id }).await;
            bail!(
                "model `{model}` timed out after {} seconds",
                MEMBER_TIMEOUT.as_secs()
            );
        }
    };
    if response.trim().is_empty() {
        bail!("model `{model}` completed without a text response");
    }
    Ok(MemberOutcome {
        model,
        role,
        session_id,
        run_id,
        response,
    })
}

async fn collect_run(conn: &mut Connection, run_id: RunId) -> anyhow::Result<String> {
    let mut response = String::new();
    loop {
        let envelope = conn
            .next_envelope()
            .await?
            .ok_or_else(|| anyhow!("daemon closed before run {run_id} completed"))?;
        let Payload::Event(event) = envelope.payload else {
            continue;
        };
        match event.body {
            EventBody::ModelStreamDelta { run_id: own, text } if own == run_id => {
                append_bounded(&mut response, &text, MAX_RESPONSE_BYTES);
            }
            EventBody::RunCompleted {
                run_id: own,
                disposition,
                ..
            } if own == run_id => match disposition {
                RunDisposition::Completed { .. } => return Ok(response),
                other => bail!("run {run_id} did not complete successfully: {other:?}"),
            },
            EventBody::RunStateChanged { run_id: own, state } if own == run_id => match state {
                RunState::Failed | RunState::Cancelled => {
                    bail!("run {run_id} entered terminal state {state:?}")
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn member_prompt(
    definition: &CouncilDefinition,
    member: &CouncilMember,
    objective: &str,
    prior: Option<&str>,
    round: u8,
) -> String {
    let (source, context) = prior.map_or_else(
        || ("the request", String::new()),
        |dossier| {
            (
                "the request and prior council reports",
                format!(
                    "\nThis is deliberation round {round}. Review the prior round below. Identify errors, resolve disagreements, and improve your recommendation without merely echoing it.\n\n{dossier}\n"
                ),
            )
        },
    );
    bounded(
        &format!(
            "You are the {role} on the `{name}` agent council. Work independently and critically. Do not invoke tools or modify files; reason only from {source}. State assumptions, evidence, risks, disagreements, and a concrete recommendation.\n\nCouncil objective:\n{objective}\n{context}",
            role = member.role,
            name = definition.name,
        ),
        MAX_DOSSIER_BYTES,
    )
}

fn synthesis_prompt(definition: &CouncilDefinition, objective: &str, dossier: &str) -> String {
    bounded(
        &format!(
            "You are the chair of the `{}` agent council. Synthesize the independent member reports into one decision-quality answer to the objective. Preserve material dissent and uncertainty; do not decide by majority vote alone. Reconcile conflicts using evidence, call out unresolved risks, and end with a concrete recommendation and next actions. Do not invoke tools or modify files.\n\nObjective:\n{}\n\nCouncil reports:\n{}",
            definition.name, objective, dossier
        ),
        MAX_DOSSIER_BYTES,
    )
}

fn dossier(outcomes: &[MemberOutcome]) -> anyhow::Result<String> {
    let mut value = String::new();
    for outcome in outcomes {
        let section = format!(
            "## {} ({})\n{}\n\n",
            outcome.role,
            outcome.model,
            outcome.response.trim()
        );
        append_bounded(&mut value, &section, MAX_DOSSIER_BYTES);
        if value.len() >= MAX_DOSSIER_BYTES {
            break;
        }
    }
    if value.trim().is_empty() {
        bail!("council produced no usable reports");
    }
    Ok(value)
}

fn validate_definition(paths: &RuntimePaths, definition: &CouncilDefinition) -> anyhow::Result<()> {
    if definition.name.is_empty()
        || definition.name.len() > 64
        || !definition
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("council name must match [A-Za-z0-9._-] and contain 1..=64 bytes");
    }
    if definition.description.len() > 1024 || contains_unsafe_control(&definition.description) {
        bail!("council description must contain at most 1024 safe bytes");
    }
    if !(2..=MAX_MEMBERS).contains(&definition.members.len()) {
        bail!("a council requires 2..={MAX_MEMBERS} members");
    }
    if !(1..=MAX_ROUNDS).contains(&definition.rounds) {
        bail!("council rounds must be 1..={MAX_ROUNDS}");
    }
    if definition.chair.is_empty()
        || definition.chair.len() > 128
        || contains_unsafe_control(&definition.chair)
    {
        bail!("council chair must name a configured model profile");
    }
    let mut unique = HashSet::new();
    for member in &definition.members {
        let parsed = parse_member(&format!("{}={}", member.model, member.role))?;
        if !unique.insert(parsed.model) {
            bail!("council member model profiles must be unique");
        }
    }
    let configured = configured_model_ids(paths)?;
    for model in unique.iter().chain(std::iter::once(&definition.chair)) {
        if !configured.contains(model) {
            bail!(
                "model profile `{model}` is not configured; add/connect it before creating the council"
            );
        }
    }
    Ok(())
}

fn configured_model_ids(paths: &RuntimePaths) -> anyhow::Result<BTreeSet<String>> {
    let path = paths.data_dir.join("models.toml");
    let models = load_models(&path).with_context(|| format!("loading {}", path.display()))?;
    Ok(models.into_iter().map(|model| model.id.0).collect())
}

fn validate_objective(objective: &str) -> anyhow::Result<()> {
    if objective.trim().is_empty()
        || objective.len() > MAX_OBJECTIVE_BYTES
        || objective.contains('\0')
    {
        bail!("council objective must contain 1..={MAX_OBJECTIVE_BYTES} bytes and no NUL");
    }
    Ok(())
}

fn contains_unsafe_control(value: &str) -> bool {
    value.chars().any(|character| character.is_control())
}

fn find(paths: &RuntimePaths, name: &str) -> anyhow::Result<CouncilDefinition> {
    load_file(&council_path(paths))?
        .councils
        .into_iter()
        .find(|council| council.name == name)
        .ok_or_else(|| anyhow!("council `{name}` is not configured"))
}

fn load_file(path: &Path) -> anyhow::Result<CouncilFile> {
    if !path.exists() {
        return Ok(CouncilFile {
            schema_version: SCHEMA_VERSION,
            councils: Vec::new(),
        });
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > 1024 * 1024 {
        bail!(
            "{} exceeds the 1 MiB council configuration limit",
            path.display()
        );
    }
    let text =
        std::str::from_utf8(&bytes).with_context(|| format!("{} is not UTF-8", path.display()))?;
    let file: CouncilFile =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    if file.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported councils.toml schema {}; expected {SCHEMA_VERSION}",
            file.schema_version
        );
    }
    if file.councils.len() > MAX_COUNCILS {
        bail!("councils.toml contains more than {MAX_COUNCILS} councils");
    }
    Ok(file)
}

fn save_file(path: &Path, file: &CouncilFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(file).context("serializing councils.toml")?;
    let tmp = path.with_extension(format!("toml.tmp.{}", MessageId::new()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut output = options
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    output.write_all(body.as_bytes())?;
    output.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn append_bounded(target: &mut String, value: &str, max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(target.len());
    if remaining == 0 {
        return;
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= remaining)
        .last()
        .unwrap_or(0);
    if value.len() <= remaining {
        target.push_str(value);
    } else if end > 0 {
        target.push_str(&value[..end]);
    }
}

fn bounded(value: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    append_bounded(&mut result, value, max_bytes);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, RuntimePaths) {
        let directory = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(directory.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).expect("data");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            r#"
[[model]]
id = "claude"
provider = "acp"
model = "claude-acp@1.0.0"

[[model]]
id = "codex"
provider = "acp"
model = "codex-acp@1.0.0"

[[model]]
id = "chair"
provider = "openai-compatible"
base_url = "http://localhost/v1"
model = "chair-model"
"#,
        )
        .expect("models");
        (directory, paths)
    }

    #[test]
    fn member_parser_preserves_model_ids_and_roles() {
        assert_eq!(
            parse_member("acp/claude=security reviewer").expect("member"),
            CouncilMember {
                model: "acp/claude".to_string(),
                role: "security reviewer".to_string()
            }
        );
        assert_eq!(
            parse_member("ollama/qwen:32b").expect("default role").model,
            "ollama/qwen:32b"
        );
    }

    #[test]
    fn persisted_council_is_private_typed_and_round_trips() {
        let (_directory, paths) = paths();
        create(
            &paths,
            "review-board".to_string(),
            vec!["claude=architect".to_string(), "codex=critic".to_string()],
            "chair".to_string(),
            2,
            Some("Independent review".to_string()),
        )
        .expect("create");
        let restored = find(&paths, "review-board").expect("restore");
        assert_eq!(restored.rounds, 2);
        assert_eq!(restored.members.len(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(council_path(&paths))
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn validation_requires_distinct_configured_models_and_bounded_rounds() {
        let (_directory, paths) = paths();
        let mut definition = CouncilDefinition {
            name: "c".to_string(),
            description: String::new(),
            chair: "chair".to_string(),
            rounds: 1,
            members: vec![
                parse_member("claude=one").expect("one"),
                parse_member("claude=two").expect("two"),
            ],
        };
        assert!(validate_definition(&paths, &definition).is_err());
        definition.members[1] = parse_member("codex=two").expect("two");
        definition.rounds = MAX_ROUNDS + 1;
        assert!(validate_definition(&paths, &definition).is_err());
        definition.rounds = 1;
        assert!(validate_definition(&paths, &definition).is_ok());
    }

    #[test]
    fn prompts_preserve_roles_dissent_and_are_bounded() {
        let definition = CouncilDefinition {
            name: "architecture".to_string(),
            description: String::new(),
            chair: "chair".to_string(),
            rounds: 2,
            members: vec![
                parse_member("claude=security").expect("one"),
                parse_member("codex=delivery").expect("two"),
            ],
        };
        let prompt = member_prompt(
            &definition,
            &definition.members[0],
            "Choose a design",
            Some("prior disagreement"),
            2,
        );
        assert!(prompt.contains("security"));
        assert!(prompt.contains("prior disagreement"));
        assert!(prompt.contains("request and prior council reports"));
        let first_round = member_prompt(
            &definition,
            &definition.members[0],
            "Choose a design",
            None,
            1,
        );
        assert!(first_round.contains("reason only from the request"));
        assert!(!first_round.contains("transcript"));
        let synthesis = synthesis_prompt(&definition, "Choose a design", "reports");
        assert!(synthesis.contains("Preserve material dissent"));
        assert!(synthesis.len() <= MAX_DOSSIER_BYTES);
    }

    #[test]
    fn unicode_bounds_never_split_codepoints() {
        assert_eq!(bounded("ab🦀cd", 5), "ab");
        assert_eq!(bounded("ab🦀cd", 6), "ab🦀");
    }
}

//! The retrieval wiring the assembly owns (rubric 9): building the configured
//! embedding model, keeping the persisted vectors fresh, and serving the
//! agent-callable `skills.search` tool.
//!
//! Three seams meet here, and only here — knowledge defines them and stays
//! model-free (ADR-009), the runtime implements the provider half, and this
//! crate (the composition root, the only one that holds BOTH the pool and the
//! runtime) binds them together:
//!
//! 1. [`build_embedder`] turns `models.toml`'s `[embedding]` entry into a
//!    `SemanticEmbedder`. Absent or misconfigured ⇒ `None`, and every consumer
//!    keeps the offline hashing embedder — today's behavior exactly.
//! 2. [`spawn_index_maintenance`] is the indexer worker `lib.rs` promised and
//!    never had: a startup reconcile plus a periodic
//!    [`drain_outbox`](codypendent_knowledge::drain_outbox) that finally
//!    CONSUMES the `index_outbox` (its `unprocessed`/`mark_processed` pair had
//!    zero production callers, so the table grew without bound).
//! 3. [`PoolRegistrySearch`] backs the runtime's `skills.search` tool over the
//!    same funnel `assemble_context` uses, and reads a disclosed skill's
//!    `SKILL.md` from its registered package directory.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use codypendent_knowledge::{
    drain_outbox, reconcile_embeddings, retrieve, semantic_indexes, CapabilityRequest, Registry,
    RegistryItem, RegistryItemKind, RetrievalConfig, RetrievalQuery, RiskClass, Scope,
    SemanticEmbedder, ToolCard,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_runtime::models::{load_model_extras, RetrievalSettings};
use codypendent_runtime::tools::{
    RegistryCard, RegistrySearch, RegistrySearchOutcome, RegistrySearchRequest, SkillDocument,
    SEARCH_CARD_LIMIT,
};
use codypendent_runtime::HttpEmbedder;
use sqlx::SqlitePool;
use tracing::{info, warn};

/// How many outbox rows one drain pass claims. Bounded so a backlog is worked
/// off over several passes instead of one long transaction-heavy burst.
const DRAIN_BATCH: i64 = 256;

/// How often the background drain runs. Registry writes are rare (a skill
/// install, a builtin re-registration), so a minute of staleness in a DERIVED
/// index costs nothing — and `semantic_indexes` embeds any still-missing item
/// inline at assembly time anyway, so retrieval is never actually stale.
const DRAIN_INTERVAL: Duration = Duration::from_secs(60);

/// Build the configured embedding model, or `None` when none is configured.
///
/// `None` is the normal, supported state: retrieval then uses the offline
/// hashing embedder exactly as it did before embeddings existed. A PRESENT but
/// broken entry (unknown provider, blank URL) is warned about loudly and also
/// yields `None` — a typo must degrade retrieval, never fail daemon startup.
#[must_use]
pub fn build_embedder(paths: &RuntimePaths) -> Option<Arc<dyn SemanticEmbedder>> {
    let extras = match load_model_extras(&paths.data_dir.join("models.toml")) {
        Ok(extras) => extras,
        Err(error) => {
            warn!(%error, "could not read models.toml retrieval settings; using the offline embedder");
            return None;
        }
    };
    let config = extras.embedding?;
    match HttpEmbedder::from_config(&config) {
        Ok(embedder) => {
            info!(
                model = %config.model,
                endpoint = %embedder.endpoint(),
                "semantic embeddings enabled for retrieval"
            );
            Some(Arc::new(embedder))
        }
        Err(error) => {
            warn!(%error, "invalid [embedding] entry; retrieval keeps the offline hashing embedder");
            None
        }
    }
}

/// The `[retrieval]` tuning (today: `mcp_top_k`). An unreadable/malformed file
/// yields the defaults — the same degrade-don't-fail stance as
/// [`build_embedder`].
#[must_use]
pub fn retrieval_settings(paths: &RuntimePaths) -> RetrievalSettings {
    load_model_extras(&paths.data_dir.join("models.toml"))
        .map(|extras| extras.retrieval)
        .unwrap_or_default()
}

/// Start the index-maintenance job: one reconcile pass, then a drain every
/// [`DRAIN_INTERVAL`], for the life of the process.
///
/// The reconcile exists because the outbox is a queue, not a log: rows written
/// while no embedding model was configured are consumed by a drain that had
/// nothing to persist, so a later `[embedding]` entry would strand every
/// existing item. A full-scan pass at startup (and thus after any config change,
/// which requires a restart) backfills them.
///
/// Fire-and-forget, like the code-graph warm-up: a failure is logged and the
/// next pass retries. Nothing here is on any request path — `assemble_context`
/// embeds whatever is still missing inline — so a wedged job degrades cost, not
/// correctness.
pub fn spawn_index_maintenance(pool: SqlitePool, embedder: Option<Arc<dyn SemanticEmbedder>>) {
    tokio::spawn(async move {
        if let Some(embedder) = &embedder {
            match reconcile_embeddings(&pool, embedder.as_ref()).await {
                Ok(0) => info!("registry embeddings already current"),
                Ok(written) => info!(written, "backfilled registry embeddings at startup"),
                Err(error) => {
                    warn!(%error, "could not reconcile registry embeddings; retrieval degrades to the offline embedder")
                }
            }
        }
        loop {
            tokio::time::sleep(DRAIN_INTERVAL).await;
            match drain_outbox(&pool, embedder.as_deref(), DRAIN_BATCH).await {
                Ok(report) if report.processed == 0 => {}
                Ok(report) => info!(
                    processed = report.processed,
                    embedded = report.embedded,
                    deleted = report.deleted,
                    deferred = report.deferred_kinds,
                    "drained the knowledge index outbox"
                ),
                Err(error) => warn!(%error, "index outbox drain failed; retrying next pass"),
            }
        }
    });
}

/// The `skills.search` backend: the retrieval funnel over the knowledge pool,
/// plus the package read that turns a disclosed skill card into its procedure.
#[derive(Clone)]
pub struct PoolRegistrySearch {
    pool: SqlitePool,
    embedder: Option<Arc<dyn SemanticEmbedder>>,
}

impl PoolRegistrySearch {
    /// Bind a search backend to the knowledge pool and (optionally) the
    /// configured embedding model — the same pair `assemble_context` uses, so an
    /// agent's own search ranks by exactly what the system-assembled context
    /// ranked by.
    #[must_use]
    pub fn new(pool: SqlitePool, embedder: Option<Arc<dyn SemanticEmbedder>>) -> Self {
        Self { pool, embedder }
    }

    /// Read a registered skill's `SKILL.md`.
    ///
    /// The package directory comes from the item's own `Provenance::Package`
    /// row and the file name from its validated manifest — never from the model,
    /// so this can only ever open a file inside a package the operator
    /// registered (`load_package` already rejected escaping entrypoints at
    /// registration). Returns `Err(note)` with a legible reason the tool renders
    /// rather than swallowing.
    fn read_skill_document(item: &RegistryItem) -> Result<SkillDocument, String> {
        let codypendent_knowledge::Provenance::Package { path } = &item.provenance else {
            return Err(format!(
                "`{}` is a built-in, not a skill package with a written procedure",
                item.name
            ));
        };
        let dir = PathBuf::from(path);
        let manifest_text = std::fs::read_to_string(dir.join("skill.toml"))
            .map_err(|error| format!("could not read `{}`'s manifest: {error}", item.name))?;
        let manifest: codypendent_knowledge::SkillManifest = toml::from_str(&manifest_text)
            .map_err(|error| format!("could not parse `{}`'s manifest: {error}", item.name))?;
        let instructions = manifest
            .entrypoints
            .instructions
            .ok_or_else(|| format!("`{}` declares no instructions entrypoint", item.name))?;
        let content = std::fs::read_to_string(dir.join(&instructions))
            .map_err(|error| format!("could not read `{}`: {error}", instructions))?;
        Ok(SkillDocument {
            name: item.name.clone(),
            content,
        })
    }
}

/// Project a disclosed card + its authoritative item into the tool's card shape.
/// Permissions come from the ITEM (the card carries only name/summary/risk), so
/// the model sees what selecting the item would actually cost.
fn to_registry_card(card: &ToolCard, item: Option<&RegistryItem>) -> RegistryCard {
    RegistryCard {
        name: card.name.clone(),
        kind: match card.kind {
            RegistryItemKind::Tool => "tool",
            RegistryItemKind::Skill => "skill",
            RegistryItemKind::Plugin => "plugin",
            RegistryItemKind::Hook => "hook",
            RegistryItemKind::Command => "command",
        }
        .to_string(),
        summary: card.summary.clone(),
        permissions: item
            .map(|item| item.permissions.iter().map(render_permission).collect())
            .unwrap_or_default(),
    }
}

/// One declared capability as `kind:target` — the shape the approval card and
/// the policy engine speak, so what the model reads here matches what it will
/// later be asked to approve.
fn render_permission(request: &CapabilityRequest) -> String {
    match request {
        CapabilityRequest::FilesystemRead(target) => format!("filesystem-read:{target}"),
        CapabilityRequest::FilesystemWrite(target) => format!("filesystem-write:{target}"),
        CapabilityRequest::Command(target) => format!("command:{target}"),
        CapabilityRequest::Network(target) => format!("network:{target}"),
        CapabilityRequest::Secret(target) => format!("secret:{target}"),
    }
}

#[async_trait]
impl RegistrySearch for PoolRegistrySearch {
    async fn search(
        &self,
        request: RegistrySearchRequest<'_>,
    ) -> Result<RegistrySearchOutcome, String> {
        let repository = crate::scan::repository_id_for(request.repository);
        let items = Registry::new()
            .list(&self.pool)
            .await
            .map_err(|error| format!("could not read the registry: {error}"))?;
        let (indexes, query_vector) =
            semantic_indexes(&self.pool, &items, self.embedder.as_deref(), request.query)
                .await
                .map_err(|error| format!("could not build the retrieval indexes: {error}"))?;

        // The SAME visibility and risk ceiling `assemble_context` applies: an
        // agent-initiated search can never see more than the system-assembled
        // context would have disclosed for this repository.
        let mut query = RetrievalQuery::new(
            request.query,
            vec![Scope::System, Scope::Repository(repository)],
            RiskClass::Medium,
        );
        query.query_vector = query_vector;
        let config = RetrievalConfig {
            disclose_tools_max: SEARCH_CARD_LIMIT,
            ..RetrievalConfig::default()
        };
        let result = retrieve(&items, &indexes, &query, &config)
            .map_err(|error| format!("retrieval failed: {error}"))?;

        let by_id: std::collections::HashMap<_, _> =
            items.iter().map(|item| (item.id, item)).collect();
        let cards: Vec<RegistryCard> = result
            .skills
            .iter()
            .chain(result.tools.iter())
            .map(|card| to_registry_card(card, by_id.get(&card.id).copied()))
            .collect();

        // `open` resolves against the DISCLOSED skills only, so the tool can
        // never be used to read a package the funnel's hard filters just
        // excluded (a deprecated, out-of-scope, or over-risk skill).
        let (document, open_note) = match request.open {
            None => (None, None),
            Some(name) => {
                let disclosed = result
                    .skills
                    .iter()
                    .find(|card| card.name == name)
                    .and_then(|card| by_id.get(&card.id).copied());
                match disclosed {
                    None => (
                        None,
                        Some(format!(
                            "`{name}` is not among the skills this search disclosed"
                        )),
                    ),
                    Some(item) => match Self::read_skill_document(item) {
                        Ok(document) => (Some(document), None),
                        Err(note) => (None, Some(note)),
                    },
                }
            }
        };

        Ok(RegistrySearchOutcome {
            cards,
            document,
            open_note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_knowledge::db;
    use std::path::Path;

    /// A paths bundle whose `data_dir` is `dir`; the rest are unused by the
    /// retrieval readers, which only ever open `<data_dir>/models.toml`.
    fn paths_at(dir: &Path) -> RuntimePaths {
        RuntimePaths {
            data_dir: dir.to_path_buf(),
            config_dir: dir.to_path_buf(),
            run_dir: dir.to_path_buf(),
            socket_path: dir.join("sock"),
            pid_path: dir.join("pid"),
            log_dir: dir.to_path_buf(),
        }
    }

    /// A minimal on-disk skill package: `skill.toml` + the `SKILL.md` its
    /// `[entrypoints]` declares.
    fn write_package(dir: &Path, name: &str, description: &str, procedure: &str) {
        std::fs::create_dir_all(dir).expect("package dir");
        std::fs::write(
            dir.join("skill.toml"),
            format!(
                r#"
schema_version = 1
id = "{name}"
name = "Fix CI"
version = "1.0.0"
scope = "system"
status = "active"
description = "{description}"
intents = ["fix a red continuous integration build"]

[entrypoints]
instructions = "SKILL.md"

[permissions]
commands = ["cargo"]

[trust]
publisher = "local-user"
"#
            ),
        )
        .expect("write manifest");
        std::fs::write(dir.join("SKILL.md"), procedure).expect("write procedure");
    }

    /// The `skills.search` seam end to end over a real pool: the funnel
    /// discloses the registered skill for a matching query, its card carries the
    /// declared permissions, and `open` returns the package's `SKILL.md`.
    #[tokio::test]
    async fn search_discloses_a_registered_skill_and_opens_its_procedure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = db::open(&tmp.path().join("codypendent.db"))
            .await
            .expect("pool");
        codypendent_knowledge::register_builtins(&pool)
            .await
            .expect("builtins");

        let package = tmp.path().join("fix-ci");
        write_package(
            &package,
            "rust.fix-ci",
            "diagnose and fix a failing continuous integration build",
            "1. read the failing job log\n2. reproduce locally with cargo test\n",
        );
        Registry::new()
            .register_package(&pool, &package, Scope::System)
            .await
            .expect("registers");

        let search = PoolRegistrySearch::new(pool.clone(), None);
        let outcome = search
            .search(RegistrySearchRequest {
                query: "the continuous integration build is failing",
                open: Some("rust.fix-ci"),
                repository: tmp.path(),
            })
            .await
            .expect("searches");

        let skill = outcome
            .cards
            .iter()
            .find(|card| card.name == "rust.fix-ci")
            .unwrap_or_else(|| panic!("the skill must be disclosed: {:?}", outcome.cards));
        assert_eq!(skill.kind, "skill");
        assert_eq!(
            skill.permissions,
            vec!["command:cargo".to_string()],
            "the card carries what selecting the skill would cost"
        );
        let document = outcome.document.expect("open returns the procedure");
        assert_eq!(document.name, "rust.fix-ci");
        assert!(document
            .content
            .contains("reproduce locally with cargo test"));
        assert_eq!(outcome.open_note, None);
    }

    /// `open` resolves ONLY against the skills this search disclosed, so it can
    /// never be used to read a package the hard filters excluded — and the
    /// refusal is reported rather than silently dropped.
    #[tokio::test]
    async fn open_is_limited_to_disclosed_skills_and_says_why_when_it_is_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = db::open(&tmp.path().join("codypendent.db"))
            .await
            .expect("pool");
        codypendent_knowledge::register_builtins(&pool)
            .await
            .expect("builtins");

        let search = PoolRegistrySearch::new(pool, None);
        let outcome = search
            .search(RegistrySearchRequest {
                query: "read a file",
                open: Some("rust.fix-ci"),
                repository: tmp.path(),
            })
            .await
            .expect("searches");
        assert!(outcome.document.is_none());
        assert!(
            outcome
                .open_note
                .as_deref()
                .is_some_and(|note| note.contains("not among the skills")),
            "an undisclosed name is reported: {:?}",
            outcome.open_note
        );
        assert!(
            !outcome.cards.is_empty(),
            "the cards themselves are still returned"
        );
    }

    /// A built-in has no package on disk, so `open` on one is a legible note
    /// rather than a filesystem error.
    #[tokio::test]
    async fn opening_a_builtin_is_a_legible_note() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = db::open(&tmp.path().join("codypendent.db"))
            .await
            .expect("pool");
        codypendent_knowledge::register_builtins(&pool)
            .await
            .expect("builtins");
        let item = Registry::new()
            .list(&pool)
            .await
            .expect("list")
            .into_iter()
            .find(|item| item.name == "workspace.read_file")
            .expect("a builtin exists");
        assert!(PoolRegistrySearch::read_skill_document(&item)
            .expect_err("a builtin ships no procedure")
            .contains("built-in"));
    }

    /// With no `[embedding]` entry there is no embedder — the supported default,
    /// under which retrieval keeps the offline hashing model.
    #[test]
    fn no_embedding_entry_yields_no_embedder_and_default_tuning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = paths_at(tmp.path());
        assert!(build_embedder(&paths).is_none());
        assert_eq!(
            retrieval_settings(&paths),
            RetrievalSettings::default(),
            "an absent models.toml yields the default tuning, never a failure"
        );

        // A configured entry builds an embedder without touching the network.
        std::fs::write(
            tmp.path().join("models.toml"),
            "[embedding]\nbase_url = \"http://localhost:11434/v1\"\nmodel = \"nomic-embed-text\"\n\n[retrieval]\nmcp_top_k = 5\n",
        )
        .expect("write models.toml");
        let embedder = build_embedder(&paths).expect("builds from a valid entry");
        assert_eq!(embedder.model(), "nomic-embed-text");
        assert_eq!(retrieval_settings(&paths).mcp_top_k, 5);
    }
}

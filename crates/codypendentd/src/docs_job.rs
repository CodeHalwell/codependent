//! Document creation and the `/update-docs` maintenance sweep — the two
//! knowledge-backed document seams the assembly injects alongside
//! [`KnowledgeDocumentMutator`](crate::documents::KnowledgeDocumentMutator).
//!
//! * [`KnowledgeDocumentCreator`] implements the daemon's
//!   [`DocumentCreator`] seam over `DocumentStore::create` plus the Markdown
//!   importer. Before it existed, `DocumentStore::create` had no production
//!   caller at all: the Docs Studio browsed a set nothing could populate.
//!
//! * [`KnowledgeDocumentMaintainer`] implements [`DocumentMaintainer`] — the
//!   `/update-docs` glue over the (previously unwired) STEP 4.6 staleness
//!   engine. One sweep, per document:
//!     1. `detect_staleness` diffs the links **already stored** on the document
//!        (the baseline resolved by an earlier sweep) against the live code
//!        graph;
//!     2. each finding is drafted as a **Maintain-mode suggestion** via
//!        `as_suggestion` and proposed — never a direct edit, so a human still
//!        accepts it in the review rail; then
//!     3. `resolve_links` + `set_links` re-anchor the baseline to the current
//!        graph, so the same change is reported once rather than on every sweep.
//!
//!   The first sweep over a document therefore only establishes the baseline
//!   (nothing was resolved before, so nothing can be stale) — which is exactly
//!   the state the fabric shipped in: `set_links` never ran, so `links_json`
//!   was always `[]`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use codypendent_daemon::documents::{
    DocsCheckFuture, DocsCheckReport, DocsCheckRequest, DocumentCreateFuture,
    DocumentCreateRequest, DocumentCreator, DocumentMaintainer,
};
use codypendent_daemon::ledger;
use codypendent_daemon::subscriptions::SubscriptionHub;
use codypendent_knowledge::docs::staleness::{detect_staleness, resolve_links};
use codypendent_knowledge::{
    codegraph, import_markdown, DocumentAuthor, DocumentMetadata, DocumentStore, NewDocument,
    Scope, SuggestionStore,
};
use codypendent_protocol::{Actor, CodypendentError, EventBody, OrganizationId, UserId};
use sqlx::SqlitePool;
use tracing::warn;

use crate::scan::{head_revision, repository_id_for};

/// The `integration` name the maintenance sweep attributes its suggestions to.
/// Not a `Human` (no person typed it) and not an `Agent` (no run, model, or
/// policy version backs it) — the sweep is a daemon job, and the attribution
/// says so.
const MAINTENANCE_INTEGRATION: &str = "update-docs";

/// Creates collaborative documents for `CreateDocument`. Cheap to clone.
#[derive(Clone)]
pub struct KnowledgeDocumentCreator {
    pool: SqlitePool,
    /// The daemon's startup checkout, used when a request names no repository.
    startup_root: PathBuf,
}

impl KnowledgeDocumentCreator {
    /// Build a creator over the daemon's pool and startup repository root.
    #[must_use]
    pub fn new(pool: SqlitePool, startup_root: PathBuf) -> Self {
        Self { pool, startup_root }
    }
}

impl DocumentCreator for KnowledgeDocumentCreator {
    fn create(&self, request: DocumentCreateRequest) -> DocumentCreateFuture<'_> {
        let pool = self.pool.clone();
        let startup_root = self.startup_root.clone();
        Box::pin(async move {
            let DocumentCreateRequest {
                title,
                scope,
                repository,
                initial_markdown,
                client_id,
            } = request;

            let title = title.trim().to_string();
            if title.is_empty() {
                return Err(CodypendentError::new(
                    "document.invalid-title",
                    "a document title must not be blank".to_string(),
                    false,
                ));
            }
            let root = repository.map(PathBuf::from).unwrap_or(startup_root);
            let scope = parse_scope(scope.as_deref(), &root)?;

            // Import the seed Markdown into typed blocks; absent seeds an empty
            // document (the TUI/CLI then edits it block by block).
            let blocks = initial_markdown
                .as_deref()
                .map(|markdown| import_markdown(&title, markdown))
                .unwrap_or_default();

            // A `CreateDocument` command is a human client action, attributed
            // exactly as `MutateDocument` attributes its edits.
            let author = DocumentAuthor::Human {
                user: UserId(client_id.to_string()),
            };
            let metadata = DocumentMetadata {
                created_by: Some(author.clone()),
                ..DocumentMetadata::default()
            };
            let doc = DocumentStore::new()
                .create(
                    &pool,
                    NewDocument {
                        title,
                        scope,
                        metadata,
                        blocks,
                    },
                    &author,
                )
                .await
                .map_err(|error| {
                    CodypendentError::new("document.create-failed", error.to_string(), true)
                })?;
            Ok(doc.id)
        })
    }
}

/// Parse the wire scope string. `None` means the default: the repository the
/// checkout at `root` identifies, so a created document lives with the code it
/// documents. An unrecognized value is rejected rather than guessed at.
fn parse_scope(scope: Option<&str>, root: &Path) -> Result<Scope, CodypendentError> {
    match scope.map(str::trim) {
        None | Some("") | Some("repository") => Ok(Scope::Repository(repository_id_for(root))),
        Some("system") => Ok(Scope::System),
        Some(other) => {
            if let Some(id) = other.strip_prefix("organization:") {
                return id
                    .trim()
                    .parse::<OrganizationId>()
                    .map(Scope::Organization)
                    .map_err(|error| {
                        CodypendentError::new(
                            "document.invalid-scope",
                            format!("organization scope id is not a uuid: {error}"),
                            false,
                        )
                    });
            }
            Err(CodypendentError::new(
                "document.invalid-scope",
                format!(
                    "unknown document scope {other:?}; expected \"repository\", \"system\", or \
                     \"organization:<uuid>\""
                ),
                false,
            ))
        }
    }
}

/// Runs the `/update-docs` staleness sweep for `CheckDocuments`, and after a
/// code-graph rescan. Cheap to clone.
#[derive(Clone)]
pub struct KnowledgeDocumentMaintainer {
    pool: SqlitePool,
    startup_root: PathBuf,
    /// The daemon's event fan-out, so a sweep asked to report into a session
    /// reaches an attached client live (append-then-publish).
    subscriptions: SubscriptionHub,
}

impl KnowledgeDocumentMaintainer {
    /// Build a maintainer over the daemon's pool, startup root, and fan-out.
    #[must_use]
    pub fn new(pool: SqlitePool, startup_root: PathBuf, subscriptions: SubscriptionHub) -> Self {
        Self {
            pool,
            startup_root,
            subscriptions,
        }
    }
}

impl DocumentMaintainer for KnowledgeDocumentMaintainer {
    fn check(&self, request: DocsCheckRequest) -> DocsCheckFuture<'_> {
        let pool = self.pool.clone();
        let startup_root = self.startup_root.clone();
        let subscriptions = self.subscriptions.clone();
        Box::pin(async move {
            let DocsCheckRequest {
                repository,
                session_id,
                client_id,
            } = request;
            let root = repository.map(PathBuf::from).unwrap_or(startup_root);
            let report = run_docs_check(&pool, &root).await.map_err(|error| {
                CodypendentError::new("document.check-failed", error.to_string(), true)
            })?;

            // Surface the counts into the named session's ledger so the finding
            // reaches the active conversation, not just this command's reply.
            // Best-effort: the sweep itself already committed, so a ledger
            // failure must not turn a completed check into an error.
            if let (Some(session_id), true) = (session_id, report.stale_findings > 0) {
                let text = format!(
                    "/update-docs: {} document(s) checked, {} link(s) resolved, {} stale \
                     finding(s), {} suggestion(s) filed for review (requested by {client_id})",
                    report.documents_checked,
                    report.links_resolved,
                    report.stale_findings,
                    report.suggestions_filed,
                );
                match ledger::append_next_event(
                    &pool,
                    session_id,
                    &Actor::System,
                    &EventBody::NoteAppended { text, run_id: None },
                    Utc::now(),
                )
                .await
                {
                    // Persist-before-publish, exactly as the executor's notes do.
                    Ok(event) => subscriptions.publish(session_id, event),
                    Err(error) => warn!(%error, "docs check note could not be appended"),
                }
            }
            Ok(report)
        })
    }
}

/// Run one documentation staleness sweep over every stored document, against
/// the code graph of the checkout at `root`. See the module docs for the
/// detect → propose → re-anchor ordering (and why the first sweep only
/// establishes the baseline).
///
/// A per-document failure is logged and skipped: one corrupt or concurrently
/// edited document must not abort the whole sweep.
pub async fn run_docs_check(pool: &SqlitePool, root: &Path) -> anyhow::Result<DocsCheckReport> {
    let repository = repository_id_for(root);
    let revision = head_revision(root);
    let current = codegraph::symbol_snapshot(pool, repository).await?;
    let author = DocumentAuthor::Integration {
        integration: MAINTENANCE_INTEGRATION.to_string(),
    };
    let store = DocumentStore::new();
    let suggestions = SuggestionStore::new();

    let mut report = DocsCheckReport::default();
    for summary in store.list_all(pool).await? {
        let Some(mut doc) = store.load(pool, summary.id).await? else {
            continue;
        };
        report.documents_checked += 1;
        let blocks = doc.blocks()?;

        // 1. Diff the STORED baseline against the live graph, before it is
        //    overwritten below.
        let findings = detect_staleness(doc.id, &doc.links, &current, &revision);
        report.stale_findings += findings.len() as u64;

        // 2. File each finding as a Maintain-mode suggestion (never an edit).
        let pending = suggestions.pending(pool, doc.id).await?;
        for finding in &findings {
            let Some(new) = finding.as_suggestion(author.clone(), &blocks, doc.revision) else {
                continue;
            };
            // A repeated sweep must not stack identical warnings on a block a
            // reviewer has not got to yet.
            if pending
                .iter()
                .any(|p| p.block_id == new.block_id && p.replacement == new.replacement)
            {
                continue;
            }
            match suggestions.propose(pool, doc.id, new).await {
                Ok(_) => report.suggestions_filed += 1,
                Err(error) => warn!(document = %doc.id, %error, "staleness suggestion refused"),
            }
        }

        // 3. Re-anchor the baseline to the current graph so the same change is
        //    reported once. Skipped when the document references no symbols and
        //    had none stored, so an unrelated document is never rewritten.
        let links = resolve_links(pool, repository, &blocks, &revision).await?;
        report.links_resolved += links.iter().filter(|l| l.resolved.is_some()).count() as u64;
        if !links.is_empty() || !doc.links.is_empty() {
            if let Err(error) = store.set_links(pool, &mut doc, links).await {
                warn!(document = %doc.id, %error, "staleness link baseline not persisted");
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::db;
    use codypendent_knowledge::{BlockContent, DocumentBlock};
    use codypendent_protocol::ClientId;

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::open_database(&tmp.path().join("codypendent.db"))
            .await
            .expect("open db");
        (tmp, pool)
    }

    #[test]
    fn scope_strings_parse_or_are_refused() {
        let root = Path::new("/tmp");
        assert!(matches!(
            parse_scope(None, root).unwrap(),
            Scope::Repository(_)
        ));
        assert!(matches!(
            parse_scope(Some("repository"), root).unwrap(),
            Scope::Repository(_)
        ));
        assert_eq!(parse_scope(Some("system"), root).unwrap(), Scope::System);
        let org = OrganizationId::new();
        assert_eq!(
            parse_scope(Some(&format!("organization:{org}")), root).unwrap(),
            Scope::Organization(org)
        );
        // Unknown scope and a malformed organization id are both refused, never
        // silently downgraded to the default.
        assert_eq!(
            parse_scope(Some("everyone"), root).unwrap_err().code,
            "document.invalid-scope"
        );
        assert_eq!(
            parse_scope(Some("organization:nope"), root)
                .unwrap_err()
                .code,
            "document.invalid-scope"
        );
    }

    #[tokio::test]
    async fn create_imports_markdown_into_typed_blocks() {
        let (tmp, pool) = temp_pool().await;
        let creator = KnowledgeDocumentCreator::new(pool.clone(), tmp.path().to_path_buf());
        let id = creator
            .create(DocumentCreateRequest {
                title: "Runbook".into(),
                scope: Some("system".into()),
                repository: None,
                initial_markdown: Some(
                    "# Runbook\n\nIntro.\n\n## Steps\n\n```sh\nmake\n```\n".into(),
                ),
                client_id: ClientId::new(),
            })
            .await
            .expect("create");

        let doc = DocumentStore::new()
            .snapshot_document(&pool, id)
            .await
            .unwrap()
            .expect("document exists");
        assert_eq!(doc.title, "Runbook");
        assert_eq!(doc.revision, 1);
        // The leading `# Runbook` repeats the title and is dropped by the import.
        assert_eq!(
            doc.blocks
                .iter()
                .map(|b| b.content.clone())
                .collect::<Vec<_>>(),
            vec![
                BlockContent::Paragraph {
                    text: "Intro.".into()
                },
                BlockContent::Heading {
                    level: 2,
                    text: "Steps".into()
                },
                BlockContent::Code {
                    language: Some("sh".into()),
                    text: "make".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn create_refuses_a_blank_title() {
        let (tmp, pool) = temp_pool().await;
        let creator = KnowledgeDocumentCreator::new(pool, tmp.path().to_path_buf());
        let error = creator
            .create(DocumentCreateRequest {
                title: "   ".into(),
                scope: None,
                repository: None,
                initial_markdown: None,
                client_id: ClientId::new(),
            })
            .await
            .expect_err("a blank title is refused");
        assert_eq!(error.code, "document.invalid-title");
    }

    /// The `/update-docs` sweep end to end over the real staleness engine: the
    /// first pass establishes the link baseline, a signature change then makes
    /// the second pass file a Maintain suggestion, and a third pass is quiet
    /// (the baseline was re-anchored) rather than stacking duplicates.
    #[tokio::test]
    async fn the_sweep_files_a_suggestion_once_per_signature_change() {
        const V1: &str = "pub fn charge_customer(amount: u32) -> bool { true }";
        const V2: &str = "pub fn charge_customer(amount: u32, currency: String) -> bool { true }";

        let (tmp, pool) = temp_pool().await;
        let root = tmp.path();
        let repository = repository_id_for(root);
        let revision = head_revision(root);
        codegraph::upsert_file_graph(&pool, repository, &revision, "src/payments.rs", V1)
            .await
            .unwrap();

        let store = DocumentStore::new();
        let doc = store
            .create(
                &pool,
                NewDocument {
                    title: "Payments".into(),
                    scope: Scope::Repository(repository),
                    metadata: DocumentMetadata::default(),
                    blocks: vec![DocumentBlock::with_id(
                        "intro",
                        BlockContent::Paragraph {
                            text: "See {{ symbol:charge_customer }} for the charge path.".into(),
                        },
                    )],
                },
                &DocumentAuthor::Integration {
                    integration: "test".into(),
                },
            )
            .await
            .unwrap();

        // First sweep: baseline only — nothing was resolved before, so nothing
        // can be stale.
        let first = run_docs_check(&pool, root).await.unwrap();
        assert_eq!(first.documents_checked, 1);
        assert_eq!(first.links_resolved, 1);
        assert_eq!(first.stale_findings, 0);
        assert_eq!(first.suggestions_filed, 0);

        // The symbol's signature changes.
        codegraph::upsert_file_graph(&pool, repository, &revision, "src/payments.rs", V2)
            .await
            .unwrap();

        let second = run_docs_check(&pool, root).await.unwrap();
        assert_eq!(second.stale_findings, 1);
        assert_eq!(second.suggestions_filed, 1);
        let pending = SuggestionStore::new().pending(&pool, doc.id).await.unwrap();
        assert_eq!(pending.len(), 1, "a review-rail suggestion, not an edit");
        assert!(matches!(
            &pending[0].author,
            DocumentAuthor::Integration { integration } if integration == MAINTENANCE_INTEGRATION
        ));
        assert!(pending[0]
            .rationale
            .as_deref()
            .is_some_and(|r| r.contains("charge_customer")));

        // Third sweep: the baseline was re-anchored, so the same change is not
        // reported (or re-filed) again.
        let third = run_docs_check(&pool, root).await.unwrap();
        assert_eq!(third.stale_findings, 0);
        assert_eq!(third.suggestions_filed, 0);
        assert_eq!(
            SuggestionStore::new()
                .pending(&pool, doc.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

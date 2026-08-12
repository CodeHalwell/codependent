//! The assembly's [`DocsChannel`]: the agent `docs.*` tools over the knowledge
//! fabric's document engine (rubric #4, doc writer).
//!
//! This is the composition the runtime cannot do itself — it may not name
//! `codypendent-knowledge` or `sqlx` — and it is deliberately the SAME engine
//! path a human client's `MutateDocument` takes: [`apply_mutation`], gated by
//! [`CollaborationMode::default_for_scope`]. That is what makes agent authoring
//! mergeable: an organization-scope document defaults to `Suggest`, so an agent
//! edit lands as a **pending suggestion** in the Docs Studio review rail rather
//! than as a silent content change, attributed to
//! `DocumentAuthor::Agent { run_id, model, policy_version }`.
//!
//! Two things an agent deliberately CANNOT reach here:
//!
//! * **publication** — writing a document to Git stays behind the separate
//!   approval-gated `PublishDocument` pipeline; and
//! * **resolution** — accepting or rejecting a suggestion is a client-role
//!   decision (Approver/Controller), never an agent's, so no `docs.*` tool
//!   maps to `AcceptSuggestion`/`RejectSuggestion`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use codypendent_knowledge::docs::apply::{apply_mutation, ApplyError, MutationEffect};
use codypendent_knowledge::{
    import_markdown, render_document, CollaborationMode, DocStoreError, DocumentAuthor,
    DocumentMetadata, DocumentStore, NewDocument, Scope,
};
use codypendent_protocol::document::{DocumentMutation, SuggestionInput};
use codypendent_protocol::{DocumentId, ModelId};
use codypendent_runtime::docs::{
    DocsAuthor, DocsChannel, DocsChannelError, DocsCreate, DocsEdit, DocsSuggest, DocsWriteEffect,
};
use sqlx::SqlitePool;

use crate::docs_job::parse_scope;
use crate::scan::repository_id_for;

/// The agent-facing document channel. Cheap to clone (a pool handle plus the
/// daemon's startup checkout, used when a run names no repository of its own).
#[derive(Clone)]
pub struct AssemblyDocsChannel {
    pool: SqlitePool,
    startup_root: PathBuf,
}

impl AssemblyDocsChannel {
    /// Build a channel over the daemon's pool and startup repository root.
    #[must_use]
    pub fn new(pool: SqlitePool, startup_root: PathBuf) -> Self {
        Self { pool, startup_root }
    }

    /// The repository root a run's document access is scoped to: the run's own
    /// checkout when it named one, else this daemon's startup root.
    fn root(&self, repository: &str) -> PathBuf {
        if repository.is_empty() {
            self.startup_root.clone()
        } else {
            PathBuf::from(repository)
        }
    }

    /// Load the document and the collaboration mode its scope implies — the gate
    /// every agent write passes through.
    ///
    /// `repository` is the run's checkout. Scope isolation is enforced HERE and
    /// not only in [`list`](Self::list): a caller holding a document id from
    /// another repository would otherwise reach it by naming it directly,
    /// turning the listing's scope filter into a discovery inconvenience rather
    /// than a boundary. A document outside the run's scope is reported as
    /// `NotFound` — the same answer as an id that does not exist, so the id
    /// space of other repositories cannot be probed.
    async fn load_mode(
        &self,
        document_id: DocumentId,
        repository: &str,
    ) -> Result<CollaborationMode, DocsChannelError> {
        let scope = DocumentStore::new()
            .scope(&self.pool, document_id)
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| DocsChannelError::NotFound(document_id.to_string()))?;
        if !self.scope_is_visible(&scope, repository) {
            return Err(DocsChannelError::NotFound(document_id.to_string()));
        }
        Ok(CollaborationMode::default_for_scope(&scope))
    }

    /// Whether a document in `scope` is reachable from the run's `repository` —
    /// the same set [`list`](Self::list) shows: this repository, plus the
    /// genuinely global scopes that are not repository-partitioned.
    fn scope_is_visible(&self, scope: &Scope, repository: &str) -> bool {
        match scope {
            Scope::Repository(id) => *id == repository_id_for(&self.root(repository)),
            // Not repository-partitioned: an org/system doc is shared by design,
            // and the Docs Studio lists System alongside the repository's own.
            Scope::System | Scope::Organization(_) | Scope::User(_) => true,
            // Narrower-than-repository scopes belong to one workspace, branch,
            // session or task; an agent addressing them by id from a plain run
            // has no scope to check against, so they stay out of reach.
            _ => false,
        }
    }

    /// Apply `mutation` through the knowledge engine under `document_id`'s mode,
    /// attributed to `author`, and describe what it did.
    async fn apply(
        &self,
        author: &DocsAuthor,
        repository: &str,
        document_id: DocumentId,
        mutation: DocumentMutation,
    ) -> Result<DocsWriteEffect, DocsChannelError> {
        let mode = self.load_mode(document_id, repository).await?;
        let outcome = apply_mutation(
            &self.pool,
            document_id,
            &mutation,
            mode,
            &agent_author(author),
        )
        .await
        .map_err(map_apply_error)?;
        Ok(match outcome.effect {
            MutationEffect::Suggested(suggestion_id) => {
                DocsWriteEffect::Suggested { suggestion_id }
            }
            // Accept/reject are unreachable from an agent tool (see the module
            // docs); an applied edit reports the document's new revision.
            _ => DocsWriteEffect::Applied {
                revision: outcome.sync.revision,
            },
        })
    }
}

#[async_trait]
impl DocsChannel for AssemblyDocsChannel {
    async fn create(
        &self,
        author: &DocsAuthor,
        request: DocsCreate,
        repository: &str,
    ) -> Result<String, DocsChannelError> {
        let title = request.title.trim().to_string();
        if title.is_empty() {
            return Err(DocsChannelError::Invalid(
                "a document title must not be blank".to_string(),
            ));
        }
        let root = self.root(repository);
        let scope = parse_scope(request.scope.as_deref(), &root)
            .map_err(|error| DocsChannelError::Invalid(error.message))?;
        let blocks = request
            .markdown
            .as_deref()
            .map(|markdown| import_markdown(&title, markdown))
            .unwrap_or_default();

        let author = agent_author(author);
        let metadata = DocumentMetadata {
            created_by: Some(author.clone()),
            ..DocumentMetadata::default()
        };
        let doc = DocumentStore::new()
            .create(
                &self.pool,
                NewDocument {
                    title,
                    scope,
                    metadata,
                    blocks,
                },
                &author,
            )
            .await
            .map_err(map_store_error)?;
        Ok(doc.id.to_string())
    }

    async fn read(
        &self,
        document_id: Option<&str>,
        repository: &str,
    ) -> Result<String, DocsChannelError> {
        let store = DocumentStore::new();
        let Some(document_id) = document_id else {
            return self.list(&store, &self.root(repository)).await;
        };
        let id = parse_document_id(document_id)?;
        // Scope-gate the direct-id read exactly as a write is gated, so naming a
        // document from another repository is no more revealing than naming one
        // that does not exist.
        self.load_mode(id, repository).await?;
        let document = store
            .snapshot_document(&self.pool, id)
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| DocsChannelError::NotFound(document_id.to_string()))?;

        // The rendered Markdown alone is not actionable: `docs.edit`/`docs.suggest`
        // target a BLOCK ID, which the Markdown does not carry. So the block
        // index rides alongside it.
        let mut out = format!(
            "# {} ({}) — {} r{}\n\n## blocks\n",
            document.title,
            document.id,
            document.status.as_str(),
            document.revision
        );
        for block in &document.blocks {
            let text = block.content_text().replace('\n', " ");
            let preview: String = text.chars().take(80).collect();
            out.push_str(&format!("- {} — {preview}\n", block.id));
        }
        out.push_str("\n## rendered\n\n");
        out.push_str(&render_document(&document.title, &document.blocks));
        Ok(out)
    }

    async fn edit(
        &self,
        author: &DocsAuthor,
        repository: &str,
        request: DocsEdit,
    ) -> Result<DocsWriteEffect, DocsChannelError> {
        let id = parse_document_id(&request.document_id)?;
        // A full replace of the block's text. The current length is read from the
        // authoritative document rather than trusted from the model, so the edit
        // covers exactly what is there — and, in a suggest-disposition mode, the
        // resulting suggestion's anchor is the real current text.
        let current = self.block_text(id, &request.block_id).await?;
        self.apply(
            author,
            repository,
            id,
            DocumentMutation::EditText {
                block_id: request.block_id,
                position: 0,
                delete_len: current.chars().count() as u32,
                insert: request.text,
            },
        )
        .await
    }

    async fn suggest(
        &self,
        author: &DocsAuthor,
        repository: &str,
        request: DocsSuggest,
    ) -> Result<DocsWriteEffect, DocsChannelError> {
        let id = parse_document_id(&request.document_id)?;
        self.apply(
            author,
            repository,
            id,
            DocumentMutation::Annotate {
                suggestion: SuggestionInput {
                    block_id: request.block_id,
                    range_start: request.range_start,
                    range_end: request.range_end,
                    replacement: request.replacement,
                    rationale: request.rationale,
                },
            },
        )
        .await
    }
}

impl AssemblyDocsChannel {
    /// The documents visible from `root` (its repository scope plus system),
    /// newest first — the same isolation the Docs Studio's tree rail enforces.
    async fn list(&self, store: &DocumentStore, root: &Path) -> Result<String, DocsChannelError> {
        let scopes = [Scope::Repository(repository_id_for(root)), Scope::System];
        let summaries = store
            .list(&self.pool, &scopes)
            .await
            .map_err(map_store_error)?;
        if summaries.is_empty() {
            return Ok("No documents yet. Use docs.create to draft one.".to_string());
        }
        let mut out = String::from("documents (newest first):\n");
        for summary in &summaries {
            out.push_str(&format!(
                "- {} — {} [{}] r{}\n",
                summary.id,
                summary.title,
                summary.status.as_str(),
                summary.revision
            ));
        }
        Ok(out)
    }

    /// The current text of `block_id`, or a correctable error naming the block.
    async fn block_text(
        &self,
        document_id: DocumentId,
        block_id: &str,
    ) -> Result<String, DocsChannelError> {
        let doc = DocumentStore::new()
            .load(&self.pool, document_id)
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| DocsChannelError::NotFound(document_id.to_string()))?;
        let blocks = doc.blocks().map_err(map_store_error)?;
        blocks
            .iter()
            .find(|block| block.id == block_id)
            .map(|block| block.content_text().to_string())
            .ok_or_else(|| {
                DocsChannelError::Invalid(format!(
                    "no block {block_id} in document {document_id} — use docs.read for its blocks"
                ))
            })
    }
}

/// The knowledge author for an agent write: the traceability triple the
/// attribution schema was built for, never a model-supplied identity.
fn agent_author(author: &DocsAuthor) -> DocumentAuthor {
    DocumentAuthor::Agent {
        run_id: author.run_id,
        model: ModelId(author.model.clone()),
        policy_version: author.policy_version.clone(),
    }
}

fn parse_document_id(raw: &str) -> Result<DocumentId, DocsChannelError> {
    raw.trim()
        .parse::<DocumentId>()
        .map_err(|_| DocsChannelError::NotFound(raw.to_string()))
}

fn map_store_error(error: DocStoreError) -> DocsChannelError {
    match error {
        DocStoreError::NoSuchDocument(id) => DocsChannelError::NotFound(id.to_string()),
        DocStoreError::SuggestionRangeDrifted(_) | DocStoreError::StaleRevision { .. } => {
            DocsChannelError::Drifted(error.to_string())
        }
        other => DocsChannelError::Backend(other.to_string()),
    }
}

fn map_apply_error(error: ApplyError) -> DocsChannelError {
    match error {
        ApplyError::NoSuchDocument(id) => DocsChannelError::NotFound(id.to_string()),
        ApplyError::Denied { .. } => DocsChannelError::ModeDenied(error.to_string()),
        ApplyError::InvalidContent(_) | ApplyError::Unsupported => {
            DocsChannelError::Invalid(error.to_string())
        }
        ApplyError::Store(store) => map_store_error(store),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::db;
    use codypendent_knowledge::{BlockContent, DocumentBlock, SuggestionStore};
    use codypendent_protocol::{OrganizationId, RunId};

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = db::open_database(&tmp.path().join("codypendent.db"))
            .await
            .expect("open db");
        (tmp, pool)
    }

    fn author() -> DocsAuthor {
        DocsAuthor {
            run_id: RunId::new(),
            model: "gpt-5.1-codex".to_string(),
            policy_version: "v1".to_string(),
        }
    }

    async fn seed(pool: &SqlitePool, scope: Scope) -> DocumentId {
        DocumentStore::new()
            .create(
                pool,
                NewDocument {
                    title: "Doc".into(),
                    scope,
                    metadata: DocumentMetadata::default(),
                    blocks: vec![DocumentBlock::with_id(
                        "p",
                        BlockContent::Paragraph {
                            text: "hello world".into(),
                        },
                    )],
                },
                &DocumentAuthor::Integration {
                    integration: "seed".into(),
                },
            )
            .await
            .expect("seed")
            .id
    }

    /// Scope isolation is a boundary, not just a listing filter: holding a
    /// document id from ANOTHER repository must not grant a read or a write.
    /// Both answer `NotFound`, so the other repository's id space cannot be
    /// probed by the shape of the error.
    #[tokio::test]
    async fn a_document_from_another_repository_is_not_reachable_by_id() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        // Seeded into a DIFFERENT checkout's repository scope.
        let other = tmp.path().join("other-checkout");
        std::fs::create_dir_all(&other).expect("other checkout");
        let doc = seed(&pool, Scope::Repository(repository_id_for(&other))).await;

        let read = channel.read(Some(&doc.to_string()), "").await;
        assert!(
            matches!(read, Err(DocsChannelError::NotFound(_))),
            "a cross-repository read must report NotFound, got {read:?}"
        );

        let write = channel
            .edit(
                &author(),
                "",
                DocsEdit {
                    document_id: doc.to_string(),
                    block_id: "p".into(),
                    text: "trespass".into(),
                },
            )
            .await;
        assert!(
            matches!(write, Err(DocsChannelError::NotFound(_))),
            "a cross-repository write must report NotFound, got {write:?}"
        );
    }

    /// THE safety property: in an organization-scope document (Suggest by
    /// default) an agent `docs.edit` does not change content — it files a
    /// reviewable suggestion attributed to the agent.
    #[tokio::test]
    async fn an_agent_edit_to_an_org_document_lands_as_a_suggestion() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        let doc = seed(&pool, Scope::Organization(OrganizationId::new())).await;
        let author = author();

        let effect = channel
            .edit(
                &author,
                "",
                DocsEdit {
                    document_id: doc.to_string(),
                    block_id: "p".into(),
                    text: "hello, reviewed world".into(),
                },
            )
            .await
            .expect("the edit is accepted as a proposal");
        let suggestion_id = match effect {
            DocsWriteEffect::Suggested { suggestion_id } => suggestion_id,
            other => panic!("an org document must route agent edits to review, got {other:?}"),
        };

        // Content is UNCHANGED until a human accepts.
        let snapshot = DocumentStore::new()
            .snapshot_document(&pool, doc)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.blocks[0].content_text(), "hello world");
        assert_eq!(snapshot.revision, 1);

        // The suggestion is on the review rail, attributed to the agent run.
        let pending = SuggestionStore::new().pending(&pool, doc).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, suggestion_id);
        assert_eq!(pending[0].replacement, "hello, reviewed world");
        match &pending[0].author {
            DocumentAuthor::Agent {
                run_id,
                model,
                policy_version,
            } => {
                assert_eq!(*run_id, author.run_id);
                assert_eq!(model.0, "gpt-5.1-codex");
                assert_eq!(policy_version, "v1");
            }
            other => panic!("expected agent attribution, got {other:?}"),
        }
    }

    /// A system-scope document defaults to Edit, so the same call applies
    /// directly — the mode, not the tool, decides.
    #[tokio::test]
    async fn an_agent_edit_to_a_system_document_applies_directly() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        let doc = seed(&pool, Scope::System).await;

        let effect = channel
            .edit(
                &author(),
                "",
                DocsEdit {
                    document_id: doc.to_string(),
                    block_id: "p".into(),
                    text: "replaced".into(),
                },
            )
            .await
            .expect("edit");
        assert!(matches!(effect, DocsWriteEffect::Applied { revision: 2 }));
        let snapshot = DocumentStore::new()
            .snapshot_document(&pool, doc)
            .await
            .unwrap()
            .unwrap();
        // A FULL replace: the old text is gone, not prepended to.
        assert_eq!(snapshot.blocks[0].content_text(), "replaced");
    }

    /// `docs.suggest` proposes even in a direct-edit mode — an agent that wants
    /// review can always ask for it.
    #[tokio::test]
    async fn suggest_proposes_even_in_edit_mode() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        let doc = seed(&pool, Scope::System).await;

        let effect = channel
            .suggest(
                &author(),
                "",
                DocsSuggest {
                    document_id: doc.to_string(),
                    block_id: "p".into(),
                    range_start: 0,
                    range_end: 5,
                    replacement: "HELLO".into(),
                    rationale: Some("match the code".into()),
                },
            )
            .await
            .expect("suggest");
        assert!(matches!(effect, DocsWriteEffect::Suggested { .. }));
        let pending = SuggestionStore::new().pending(&pool, doc).await.unwrap();
        assert_eq!(pending[0].original, "hello");
        assert_eq!(pending[0].rationale.as_deref(), Some("match the code"));
    }

    #[tokio::test]
    async fn create_read_round_trips_through_the_agent_channel() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        let root = tmp.path().to_string_lossy().into_owned();

        let id = channel
            .create(
                &author(),
                DocsCreate {
                    title: "Runbook".into(),
                    scope: Some("system".into()),
                    markdown: Some("# Runbook\n\nCharging notes.\n".into()),
                },
                &root,
            )
            .await
            .expect("create");

        let rendered = channel.read(Some(&id), &root).await.expect("read");
        assert!(rendered.contains("Runbook"), "{rendered}");
        assert!(rendered.contains("Charging notes."), "{rendered}");
        assert!(rendered.contains("## blocks"), "block index: {rendered}");

        let listing = channel.read(None, &root).await.expect("list");
        assert!(listing.contains(&id), "{listing}");

        // A bad id is a correctable not-found, never a backend error.
        let error = channel.read(Some("not-a-uuid"), &root).await.unwrap_err();
        assert_eq!(error.code(), "docs.not-found");
    }

    #[tokio::test]
    async fn editing_an_unknown_block_is_a_correctable_error() {
        let (tmp, pool) = temp_pool().await;
        let channel = AssemblyDocsChannel::new(pool.clone(), tmp.path().to_path_buf());
        let doc = seed(&pool, Scope::System).await;
        let error = channel
            .edit(
                &author(),
                "",
                DocsEdit {
                    document_id: doc.to_string(),
                    block_id: "nope".into(),
                    text: "x".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "docs.invalid-request");
        assert!(error.to_string().contains("docs.read"), "{error}");
    }
}

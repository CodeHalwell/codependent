//! The rubric #4 doc-writer proof: a **scripted agent run drafts documentation
//! through the real `docs.*` tools**, its change lands as a *reviewable
//! suggestion* (never a silent edit), and a **human accept applies it**.
//!
//! This is the whole vertical over production seams, not mocks:
//!
//! * the real [`FrameworkAgentRuntime`] agent loop, driven by a
//!   [`ScriptedDriver`] (no live model, no HTTP);
//! * the assembly's real `AssemblyDocsChannel` — the same `apply_mutation` +
//!   collaboration-mode gate a human client's `MutateDocument` passes through;
//!   and
//! * the assembly's real `KnowledgeDocumentMutator` for the human accept,
//!   including its edit-lease requirement.
//!
//! The safety story this pins: the document is **organization-scope**, which
//! defaults to `CollaborationMode::Suggest`, so the agent's `docs.edit` changes
//! nothing until a human accepts it. That is why giving an agent document tools
//! is mergeable.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use codypendent_codypendentd::docs_channel::AssemblyDocsChannel;
use codypendent_codypendentd::documents::KnowledgeDocumentMutator;
use codypendent_daemon::approvals::ApprovalBroker;
use codypendent_daemon::artifacts::{ArtifactStore, Provenance};
use codypendent_daemon::db::open_database;
use codypendent_daemon::documents::{
    DocumentLeaseRequest, DocumentLeaser, DocumentMutationRequest, DocumentMutator,
};
use codypendent_daemon::policy::PolicyEngine;
use codypendent_daemon::subscriptions::SubscriptionHub;
use codypendent_daemon::{ledger, projections};
use codypendent_knowledge::{
    BlockContent, CollaborationMode, DocumentAuthor, DocumentBlock, DocumentMetadata,
    DocumentStore, NewDocument, Scope, SuggestionStore,
};
use codypendent_protocol::{
    Actor, AgentMode, ClientId, DataClassification, DocumentMutation, EventBody, OrganizationId,
    RunDisposition, RunId, SessionEvent, SessionId,
};
use codypendent_runtime::agent::{
    ApprovalRequest, CancellationToken, FrameworkAgentRuntime, ModelStep, RunContext, RunJournal,
    ScriptedDriver,
};
use codypendent_runtime::models::ModelRegistry;
use codypendent_runtime::tools::{ArtifactSink, ClosureSink};
use serde_json::json;

/// An [`ArtifactSink`] over a store + pool (the pool's type is unnameable in a
/// function signature here, so this must be a macro — as in `agent_it.rs`).
macro_rules! store_sink {
    ($store:expr, $pool:expr) => {{
        let store = $store.clone();
        let pool = $pool.clone();
        ClosureSink(move |media: String, prov: Provenance, bytes: Vec<u8>| {
            let store = store.clone();
            let pool = pool.clone();
            async move {
                store
                    .put(&pool, &media, DataClassification::Internal, prov, &bytes)
                    .await
            }
        })
    }};
}

/// A [`RunJournal`] over the ledger/projections/broker (mirrors `agent_it.rs`).
macro_rules! run_journal {
    ($pool:expr, $broker:expr) => {{
        let persist_pool = $pool.clone();
        let approve_pool = $pool.clone();
        let state_pool = $pool.clone();
        let approve_broker = $broker.clone();
        RunJournal::new(
            move |session: SessionId, actor: Actor, body: EventBody| {
                let pool = persist_pool.clone();
                async move {
                    if let EventBody::RunStateChanged { run_id, state } = &body {
                        projections::set_run_state(&pool, *run_id, *state).await?;
                    }
                    let sequence = ledger::next_sequence(&pool, session).await?;
                    let event = SessionEvent {
                        sequence,
                        occurred_at: Utc::now(),
                        causation_id: None,
                        correlation_id: None,
                        actor,
                        body,
                    };
                    ledger::append_event(&pool, session, &event).await?;
                    Ok(event)
                }
            },
            move |req: ApprovalRequest| {
                let pool = approve_pool.clone();
                let broker = approve_broker.clone();
                async move {
                    let id = broker
                        .request(
                            &pool,
                            req.session_id,
                            req.run_id,
                            req.repository.as_deref(),
                            req.action,
                            req.risk,
                            req.capabilities,
                            None,
                        )
                        .await?;
                    Ok(id)
                }
            },
        )
        .with_state_reader(move |run_id| {
            let pool = state_pool.clone();
            async move { projections::load_run_state(&pool, run_id).await }
        })
    }};
}

#[tokio::test]
async fn a_scripted_agent_drafts_a_doc_edit_that_a_human_then_accepts() {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = tmp.path().to_path_buf();
    let pool = open_database(&root.join("codypendent.db")).await.unwrap();
    let store = ArtifactStore::new(root.join("artifacts"));
    let broker = ApprovalBroker::new();
    let hub = SubscriptionHub::new();

    // An ORGANIZATION-scope document: suggest-by-default, so an agent may only
    // propose.
    let scope = Scope::Organization(OrganizationId::new());
    assert_eq!(
        CollaborationMode::default_for_scope(&scope),
        CollaborationMode::Suggest
    );
    let document = DocumentStore::new()
        .create(
            &pool,
            NewDocument {
                title: "Payments Runbook".into(),
                scope,
                metadata: DocumentMetadata::default(),
                blocks: vec![DocumentBlock::with_id(
                    "intro",
                    BlockContent::Paragraph {
                        text: "charge_customer takes an amount.".into(),
                    },
                )],
            },
            &DocumentAuthor::Human {
                user: codypendent_protocol::UserId("seed".into()),
            },
        )
        .await
        .expect("seed the document")
        .id;

    // The agent loop, wired with the real assembly document channel.
    let session = SessionId::new();
    let run = RunId::new();
    ledger::create_session(&pool, session, "docs-agent-it")
        .await
        .unwrap();
    projections::insert_run(
        &pool,
        run,
        session,
        "document the charge path",
        AgentMode::Build,
        "hosted",
        "{}",
    )
    .await
    .unwrap();

    let journal = run_journal!(pool, broker);
    let sink: Box<dyn ArtifactSink> = Box::new(store_sink!(store, pool));
    let runtime = FrameworkAgentRuntime::new(
        ModelRegistry::new(Vec::new()),
        PolicyEngine::with_defaults(),
        broker.clone(),
        hub.clone(),
        journal,
        sink,
    )
    .with_docs(Arc::new(AssemblyDocsChannel::new(
        pool.clone(),
        root.clone(),
    )));

    // The `docs.*` tools really are offered to a plain (non-workflow) run.
    let ctx = RunContext::new(
        session,
        run,
        "document the charge path",
        AgentMode::Build,
        root.clone(),
        root.clone(),
    );
    let offered = runtime.offered_tool_names(&ctx);
    for tool in ["docs.create", "docs.read", "docs.edit", "docs.suggest"] {
        assert!(offered.iter().any(|n| n == tool), "{tool} not offered");
    }
    // ...and are ADVERTISED, not merely offered. Asserting only on the offered
    // set (which is all this test used to do) missed the whole defect: the four
    // tools were dispatchable and driven successfully by the `ScriptedDriver`
    // below, while `static_tool_definitions()` had no entry for any of them — so
    // the intersection in `advertised_tool_definitions` dropped all four and a
    // REAL model was never shown a document tool. A scripted driver cannot catch
    // that, because it calls tools by name without reading the catalog.
    let advertised: Vec<String> = runtime
        .advertised_tool_definitions(&ctx)
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    for tool in ["docs.create", "docs.read", "docs.edit", "docs.suggest"] {
        assert!(
            advertised.iter().any(|n| n == tool),
            "{tool} is dispatchable but never shown to the model: {advertised:?}"
        );
    }

    // The script: read the document (to learn its block ids, as a real agent
    // would), then rewrite the intro block.
    let driver = ScriptedDriver::new(vec![
        ModelStep::CallTool {
            tool: "docs.read".to_string(),
            args: json!({ "document_id": document.to_string() }),
        },
        ModelStep::CallTool {
            tool: "docs.edit".to_string(),
            args: json!({
                "document_id": document.to_string(),
                "block_id": "intro",
                "text": "charge_customer takes an amount and a currency.",
            }),
        },
        ModelStep::Finish {
            summary: "documented".to_string(),
        },
    ]);

    let outcome = runtime
        .execute_run(&driver, ctx, CancellationToken::never())
        .await
        .expect("the run drives to completion");
    assert!(
        matches!(outcome.disposition, RunDisposition::Completed { .. }),
        "the run completed: {:?}",
        outcome.disposition
    );

    // The agent's edit did NOT change the document: it is a pending suggestion,
    // attributed to the agent run.
    let before = DocumentStore::new()
        .snapshot_document(&pool, document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.blocks[0].content_text(),
        "charge_customer takes an amount.",
        "an org-scope document must not be edited directly by an agent"
    );
    assert_eq!(before.revision, 1);

    let pending = SuggestionStore::new()
        .pending(&pool, document)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the agent's edit is on the review rail");
    let suggestion = &pending[0];
    assert_eq!(
        suggestion.replacement,
        "charge_customer takes an amount and a currency."
    );
    match &suggestion.author {
        DocumentAuthor::Agent { run_id, model, .. } => {
            assert_eq!(*run_id, run, "attributed to this run");
            assert_eq!(model.0, "scripted", "and to the model that wrote it");
        }
        other => panic!("expected agent attribution, got {other:?}"),
    }

    // A HUMAN accepts it through the production client seam — lease first, as
    // the Docs Studio does, then `MutateDocument::AcceptSuggestion`.
    let mutator = KnowledgeDocumentMutator::new(pool.clone());
    let reviewer = ClientId::new();
    mutator
        .acquire(DocumentLeaseRequest {
            document_id: document,
            block_id: Some("intro".to_string()),
            ttl: None,
            client_id: reviewer,
        })
        .await
        .expect("the reviewer leases the target block");
    mutator
        .apply_mutation(DocumentMutationRequest {
            document_id: document,
            mutation: DocumentMutation::AcceptSuggestion {
                suggestion_id: suggestion.id.clone(),
            },
            client_id: reviewer,
        })
        .await
        .expect("the human accept applies the agent's proposal");

    // Only NOW does the document carry the agent's sentence.
    let after = DocumentStore::new()
        .snapshot_document(&pool, document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.blocks[0].content_text(),
        "charge_customer takes an amount and a currency."
    );
    assert_eq!(after.revision, 2);
    assert!(SuggestionStore::new()
        .pending(&pool, document)
        .await
        .unwrap()
        .is_empty());

    // The attribution log records both hands: the agent proposed, the human
    // resolved.
    let authorship = DocumentStore::new()
        .authorship(&pool, document)
        .await
        .unwrap();
    assert!(
        authorship
            .iter()
            .any(|record| matches!(record.author, DocumentAuthor::Human { .. })),
        "the accept is attributed to the human reviewer"
    );

    pool.close().await;
}

//! STEP 4.4 deterministic render + Git publication: the same document revision
//! renders byte-identical Markdown, the publish plan shows target/changed
//! files/Git action before approval, and a publication records the
//! (document revision ↔ git commit) pairing.

use codypendent_knowledge::db;
use codypendent_knowledge::docs::model::{
    BlockContent, ChecklistItem, DocumentAuthor, DocumentBlock, DocumentMetadata,
};
use codypendent_knowledge::docs::render::{
    pending_pull_request_publications, plan_publication, publications, record_publication,
    record_pull_request_merge, render_document, PublishTarget, PullRequestHandle,
};
use codypendent_knowledge::docs::store::{DocumentStore, NewDocument};
use codypendent_knowledge::Scope;
use codypendent_protocol::UserId;

async fn temp_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    (tmp, pool)
}

fn sample() -> Vec<DocumentBlock> {
    vec![
        DocumentBlock::with_id(
            "h",
            BlockContent::Heading {
                level: 2,
                text: "Payment Service".into(),
            },
        ),
        DocumentBlock::with_id(
            "p",
            BlockContent::Paragraph {
                text: "Charges customers.".into(),
            },
        ),
        DocumentBlock::with_id(
            "c",
            BlockContent::Code {
                language: Some("rust".into()),
                text: "fn charge() {}".into(),
            },
        ),
        DocumentBlock::with_id(
            "t",
            BlockContent::Table {
                rows: vec![
                    vec!["Field".into(), "Type".into()],
                    vec!["amount".into(), "u64".into()],
                ],
            },
        ),
        DocumentBlock::with_id(
            "cl",
            BlockContent::Checklist {
                items: vec![ChecklistItem {
                    text: "retry".into(),
                    checked: true,
                }],
            },
        ),
        DocumentBlock::with_id(
            "sym",
            BlockContent::EmbeddedSymbol {
                symbol: "payments::charge_customer".into(),
            },
        ),
    ]
}

#[test]
fn render_is_deterministic() {
    let blocks = sample();
    let a = render_document("Runbook", &blocks);
    let b = render_document("Runbook", &blocks);
    assert_eq!(
        a, b,
        "the same revision must render byte-identical Markdown"
    );
}

#[test]
fn render_covers_block_kinds_and_keeps_symbol_markers() {
    let md = render_document("Runbook", &sample());
    assert!(md.starts_with("# Runbook\n"));
    assert!(md.contains("## Payment Service"));
    assert!(md.contains("```rust\nfn charge() {}\n```"));
    assert!(md.contains("| Field | Type |"));
    assert!(md.contains("| --- | --- |"));
    assert!(md.contains("- [x] retry"));
    // The symbol embed keeps its marker verbatim so staleness can resolve it.
    assert!(md.contains("{{ symbol:payments::charge_customer }}"));
}

#[tokio::test]
async fn publish_plan_shows_target_changed_files_and_git_action() {
    let (_tmp, pool) = temp_pool().await;
    let store = DocumentStore::new();
    let author = DocumentAuthor::Human {
        user: UserId("dev".into()),
    };
    let doc = store
        .create(
            &pool,
            NewDocument {
                title: "Runbook".into(),
                scope: Scope::Repository(codypendent_protocol::RepositoryId::new()),
                metadata: DocumentMetadata::default(),
                blocks: sample(),
            },
            &author,
        )
        .await
        .unwrap();
    let full = store
        .snapshot_document(&pool, doc.id)
        .await
        .unwrap()
        .unwrap();

    let plan = plan_publication(
        &full,
        PublishTarget::DocumentationPr {
            branch: "docs/payment-runbook".into(),
            path: "docs/payment-runbook.md".into(),
            title: "Update payment runbook".into(),
        },
    );
    assert_eq!(
        plan.changed_files,
        vec!["docs/payment-runbook.md".to_string()]
    );
    assert!(plan.git_action.contains("documentation PR"));
    assert!(plan.git_action.contains("docs/payment-runbook.md"));
    assert_eq!(plan.revision, 1);
    // The plan renders exactly what would be committed.
    assert_eq!(plan.rendered, render_document("Runbook", &full.blocks));
    assert_eq!(plan.rendered_hash.len(), 64);
}

#[tokio::test]
async fn publishing_records_revision_to_commit_pairing() {
    let (_tmp, pool) = temp_pool().await;
    let store = DocumentStore::new();
    let author = DocumentAuthor::Human {
        user: UserId("dev".into()),
    };
    let doc = store
        .create(
            &pool,
            NewDocument {
                title: "Runbook".into(),
                scope: Scope::System,
                metadata: DocumentMetadata::default(),
                blocks: sample(),
            },
            &author,
        )
        .await
        .unwrap();
    let full = store
        .snapshot_document(&pool, doc.id)
        .await
        .unwrap()
        .unwrap();
    let plan = plan_publication(
        &full,
        PublishTarget::RepositoryFile {
            path: "docs/runbook.md".into(),
        },
    );

    let published = record_publication(&pool, doc.id, &plan, Some("abc123"), None)
        .await
        .unwrap();
    assert_eq!(published.revision, 1);
    assert_eq!(published.git_commit.as_deref(), Some("abc123"));
    assert_eq!(published.pr_number, None, "not a PR target");

    let history = publications(&pool, doc.id).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].revision, 1);
    assert_eq!(history[0].git_commit.as_deref(), Some("abc123"));
    assert_eq!(history[0].rendered_hash, plan.rendered_hash);
}

/// 2026-08-13 review F9: a `DocumentationPr` publication persists the opened
/// PR's handle (previously discarded outright — nothing was ever stored to
/// poll), and a later poll's merge status reflects back onto that SAME
/// document's row — never a different document's, even one that opened a PR
/// with the same number against a different repository.
#[tokio::test]
async fn pull_request_handle_persists_and_merge_status_reflects_back_to_the_right_document() {
    let (_tmp, pool) = temp_pool().await;
    let store = DocumentStore::new();
    let author = DocumentAuthor::Human {
        user: UserId("dev".into()),
    };
    let make_doc = |title: &str| NewDocument {
        title: title.to_string(),
        scope: Scope::System,
        metadata: DocumentMetadata::default(),
        blocks: sample(),
    };
    let doc_a = store.create(&pool, make_doc("A"), &author).await.unwrap();
    let doc_b = store.create(&pool, make_doc("B"), &author).await.unwrap();
    let full_a = store
        .snapshot_document(&pool, doc_a.id)
        .await
        .unwrap()
        .unwrap();
    let full_b = store
        .snapshot_document(&pool, doc_b.id)
        .await
        .unwrap()
        .unwrap();
    let plan_a = plan_publication(
        &full_a,
        PublishTarget::DocumentationPr {
            branch: "docs/a".into(),
            path: "docs/a.md".into(),
            title: "A".into(),
        },
    );
    let plan_b = plan_publication(
        &full_b,
        PublishTarget::DocumentationPr {
            branch: "docs/b".into(),
            path: "docs/b.md".into(),
            title: "B".into(),
        },
    );

    // Both documents happen to open PR number 42 (against different
    // repositories in reality; the store has no notion of "repository" here,
    // which is exactly why the update must be scoped by document_id too).
    let handle = PullRequestHandle {
        number: 42,
        url: "https://github.com/octocat/hello-world/pull/42".to_string(),
    };
    let published_a = record_publication(&pool, doc_a.id, &plan_a, Some("sha-a"), Some(&handle))
        .await
        .unwrap();
    record_publication(&pool, doc_b.id, &plan_b, Some("sha-b"), Some(&handle))
        .await
        .unwrap();
    assert_eq!(published_a.pr_number, Some(42));
    assert_eq!(published_a.pr_url.as_deref(), Some(handle.url.as_str()));
    assert!(!published_a.pr_merged);

    // Both are pending before any poll.
    let pending = pending_pull_request_publications(&pool).await.unwrap();
    assert_eq!(pending.len(), 2);

    // Only document A's PR is reported merged.
    let updated = record_pull_request_merge(
        &pool,
        doc_a.id,
        42,
        true,
        Some("2026-08-13T00:00:00Z"),
        Some("mergedsha"),
    )
    .await
    .unwrap();
    assert_eq!(updated, 1, "exactly document A's row must be touched");

    let history_a = publications(&pool, doc_a.id).await.unwrap();
    assert!(history_a[0].pr_merged);
    assert_eq!(
        history_a[0].pr_merged_at.as_deref(),
        Some("2026-08-13T00:00:00Z")
    );
    assert_eq!(
        history_a[0].pr_merge_commit_sha.as_deref(),
        Some("mergedsha")
    );

    // Document B's identically-numbered PR is untouched.
    let history_b = publications(&pool, doc_b.id).await.unwrap();
    assert!(
        !history_b[0].pr_merged,
        "a different document's same-numbered PR must not be affected"
    );

    // The sync poll list now excludes A (merged) but still includes B.
    let pending_after = pending_pull_request_publications(&pool).await.unwrap();
    assert_eq!(pending_after.len(), 1);
    assert_eq!(pending_after[0].document_id, doc_b.id);
}

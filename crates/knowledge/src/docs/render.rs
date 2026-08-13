//! Deterministic Markdown rendering and Git publication (STEP 4.4).
//!
//! Git is the **reviewed snapshot store**, not the collaboration algorithm. A
//! document revision renders to Markdown by a total, order-preserving function:
//! the same block list always renders byte-identical output (exit criterion 2,
//! "snapshot is reproducible"). Publication produces a [`PublishPlan`] — target,
//! changed files, and the resulting Git action — shown before approval, and, once
//! committed, records the `(document revision ↔ git commit)` pairing so staleness
//! (STEP 4.6) can compare a published document against the live graph.

use chrono::Utc;
use codypendent_protocol::DocumentId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::model::{BlockContent, DocumentBlock, KnowledgeDocument};
use super::store::DocStoreError;

/// Render a document (title + blocks) to Markdown. Deterministic and total.
#[must_use]
pub fn render_document(title: &str, blocks: &[DocumentBlock]) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(title);
    out.push('\n');
    for block in blocks {
        out.push('\n');
        render_block(&block.content, &mut out);
        out.push('\n');
    }
    out
}

/// Render one block. Stable per block kind; embed blocks keep their
/// `{{ kind:target }}` marker verbatim so the staleness engine (STEP 4.6) can
/// still resolve them in the published text.
fn render_block(content: &BlockContent, out: &mut String) {
    match content {
        BlockContent::Heading { level, text } => {
            let hashes = "#".repeat((*level).clamp(1, 6) as usize);
            out.push_str(&hashes);
            out.push(' ');
            // A heading is one Markdown line by definition: an embedded newline
            // would end the heading early and leak the remainder as body text,
            // so it is folded to a space (deterministic, still one line).
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(line);
            }
            out.push('\n');
        }
        BlockContent::Paragraph { text } => {
            out.push_str(text);
            out.push('\n');
        }
        BlockContent::Code { language, text } => {
            out.push_str("```");
            out.push_str(language.as_deref().unwrap_or(""));
            out.push('\n');
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        BlockContent::Diagram { format, source } => {
            out.push_str("```");
            out.push_str(format);
            out.push('\n');
            out.push_str(source);
            if !source.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        BlockContent::Table { rows } => render_table(rows, out),
        BlockContent::Callout { kind, text } => {
            // GitHub-style alert blockquote — deterministic, uppercased kind.
            // EVERY line of the text is quoted: an unquoted second line would
            // fall out of the blockquote in the published Markdown.
            out.push_str("> [!");
            out.push_str(&kind.to_uppercase());
            out.push_str("]\n");
            for line in text.split('\n') {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        BlockContent::Checklist { items } => {
            for item in items {
                out.push_str(if item.checked { "- [x] " } else { "- [ ] " });
                out.push_str(&item.text);
                out.push('\n');
            }
        }
        BlockContent::Query { query } => {
            out.push_str("```query\n");
            out.push_str(query);
            out.push('\n');
            out.push_str("```\n");
        }
        BlockContent::EmbeddedFile { path } => {
            out.push('[');
            out.push_str(path);
            out.push_str("](");
            out.push_str(path);
            out.push_str(")\n");
        }
        BlockContent::EmbeddedSymbol { symbol } => {
            out.push_str("{{ symbol:");
            out.push_str(symbol);
            out.push_str(" }}\n");
        }
        BlockContent::EmbeddedWorkflow { workflow } => {
            out.push_str("{{ workflow:");
            out.push_str(workflow);
            out.push_str(" }}\n");
        }
        BlockContent::EmbeddedSkill { skill } => {
            out.push_str("{{ skill:");
            out.push_str(skill);
            out.push_str(" }}\n");
        }
    }
}

/// Render a table with the first row as the header (GitHub Markdown). Cell
/// text is escaped so a literal `|` cannot open a phantom column (and the
/// escaping backslash itself is escaped first, keeping the mapping invertible);
/// a newline inside a cell is folded to a space — a Markdown table row is one
/// line by definition.
fn render_table(rows: &[Vec<String>], out: &mut String) {
    if rows.is_empty() {
        return;
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let escape_cell = |cell: &str| {
        cell.replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('\n', " ")
    };
    let write_row = |out: &mut String, row: &[String]| {
        out.push('|');
        for c in 0..columns {
            out.push(' ');
            out.push_str(
                &row.get(c)
                    .map(String::as_str)
                    .map(escape_cell)
                    .unwrap_or_default(),
            );
            out.push_str(" |");
        }
        out.push('\n');
    };
    write_row(out, &rows[0]);
    out.push('|');
    for _ in 0..columns {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &rows[1..] {
        write_row(out, row);
    }
}

/// Where a publication writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PublishTarget {
    /// Write the rendered Markdown to a repository file (via an approval-gated
    /// change set on the working tree).
    RepositoryFile { path: String },
    /// Commit the rendered Markdown to a dedicated docs branch.
    DocsBranchCommit { branch: String, path: String },
    /// Open a documentation pull request via the Phase 3 GitHub write path.
    DocumentationPr {
        branch: String,
        path: String,
        title: String,
    },
}

impl PublishTarget {
    /// The repo-relative file this target writes.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            PublishTarget::RepositoryFile { path }
            | PublishTarget::DocsBranchCommit { path, .. }
            | PublishTarget::DocumentationPr { path, .. } => path,
        }
    }

    /// A human description of the resulting Git action, shown before approval.
    #[must_use]
    pub fn git_action(&self) -> String {
        match self {
            PublishTarget::RepositoryFile { path } => {
                format!("write {path} in the working tree (approval-gated change set)")
            }
            PublishTarget::DocsBranchCommit { branch, path } => {
                format!("commit {path} on branch {branch}")
            }
            PublishTarget::DocumentationPr {
                branch,
                path,
                title,
            } => format!("open documentation PR \"{title}\" ({path} on {branch})"),
        }
    }
}

/// The plan a publish produces and shows before any approval: the target, the
/// files it changes, the Git action, and the exact rendered bytes (with their
/// hash, which is also what a published-snapshot row records).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishPlan {
    pub target: PublishTarget,
    pub changed_files: Vec<String>,
    pub git_action: String,
    pub rendered: String,
    pub rendered_hash: String,
    /// The document revision this plan renders.
    pub revision: u64,
}

/// Build the publication plan for `doc` to `target`. Pure — no side effects, no
/// approval; the caller displays it, gates it, then executes the Git action.
#[must_use]
pub fn plan_publication(doc: &KnowledgeDocument, target: PublishTarget) -> PublishPlan {
    let rendered = render_document(&doc.title, &doc.blocks);
    let rendered_hash = hex::encode(Sha256::digest(rendered.as_bytes()));
    let git_action = target.git_action();
    let changed_files = vec![target.path().to_string()];
    PublishPlan {
        target,
        changed_files,
        git_action,
        rendered,
        rendered_hash,
        revision: doc.revision,
    }
}

/// A minimal, GitHub-API-agnostic handle to an opened pull request — just
/// enough to persist and later poll. This crate depends only on
/// `codypendent-protocol` (see the crate root docs), so it cannot name
/// `codypendent-integrations::github::model::PullRequest` directly; the
/// daemon assembly (`codypendentd::publish`) converts between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestHandle {
    pub number: u64,
    pub url: String,
}

/// A recorded publication: which document revision was published, to where, at
/// which commit, and the hash of the rendered Markdown. For a
/// `DocumentationPr` target, also the opened PR's number/URL and — once
/// polled back via [`record_pull_request_merge`] — whether/when it merged
/// (2026-08-13 review F9: until now, the PR `create_draft_pull_request`
/// returned was discarded outright — `.await?; Ok(())` — so nothing was ever
/// persisted to poll, and no schema column existed to poll into).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    pub id: String,
    pub document_id: DocumentId,
    pub revision: u64,
    pub target: String,
    pub git_commit: Option<String>,
    pub rendered_hash: String,
    /// The opened PR's number, for a `DocumentationPr` target; `None` for the
    /// other two targets, which never open a PR.
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// Whether GitHub has reported this PR merged, as of the last poll.
    pub pr_merged: bool,
    pub pr_merged_at: Option<String>,
    pub pr_merge_commit_sha: Option<String>,
}

/// Record a completed publication (`plan` executed, producing `git_commit`),
/// storing the `(document revision ↔ git commit)` pairing staleness compares,
/// and — for a `DocumentationPr` target — the opened PR's handle so its merge
/// status can later be polled and reflected back (see
/// [`record_pull_request_merge`]).
pub async fn record_publication(
    pool: &SqlitePool,
    document_id: DocumentId,
    plan: &PublishPlan,
    git_commit: Option<&str>,
    pull_request: Option<&PullRequestHandle>,
) -> Result<Publication, DocStoreError> {
    let id = Uuid::now_v7().to_string();
    let target = plan.git_action.clone();
    sqlx::query(
        "INSERT INTO document_publications \
         (id, document_id, revision, target, git_commit, rendered_hash, published_at, \
          pr_number, pr_url, pr_merged, pr_merged_at, pr_merge_commit_sha) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL)",
    )
    .bind(&id)
    .bind(document_id.to_string())
    .bind(plan.revision as i64)
    .bind(&target)
    .bind(git_commit)
    .bind(&plan.rendered_hash)
    .bind(Utc::now().to_rfc3339())
    .bind(pull_request.map(|pr| pr.number as i64))
    .bind(pull_request.map(|pr| pr.url.clone()))
    .execute(pool)
    .await?;
    Ok(Publication {
        id,
        document_id,
        revision: plan.revision,
        target,
        git_commit: git_commit.map(str::to_string),
        rendered_hash: plan.rendered_hash.clone(),
        pr_number: pull_request.map(|pr| pr.number),
        pr_url: pull_request.map(|pr| pr.url.clone()),
        pr_merged: false,
        pr_merged_at: None,
        pr_merge_commit_sha: None,
    })
}

/// The publication history for a document, newest first.
pub async fn publications(
    pool: &SqlitePool,
    document_id: DocumentId,
) -> Result<Vec<Publication>, DocStoreError> {
    let rows = sqlx::query(
        "SELECT id, revision, target, git_commit, rendered_hash, pr_number, pr_url, pr_merged, \
         pr_merged_at, pr_merge_commit_sha FROM document_publications \
         WHERE document_id = ? ORDER BY published_at DESC, id DESC",
    )
    .bind(document_id.to_string())
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| decode_publication(row, document_id))
        .collect()
}

/// Every publication row that opened a pull request whose merge status has
/// not yet been observed as merged — the poll list a merge-status sync walks
/// (2026-08-13 review F9). Spans every document, newest first.
pub async fn pending_pull_request_publications(
    pool: &SqlitePool,
) -> Result<Vec<Publication>, DocStoreError> {
    let rows = sqlx::query(
        "SELECT id, document_id, revision, target, git_commit, rendered_hash, pr_number, pr_url, \
         pr_merged, pr_merged_at, pr_merge_commit_sha FROM document_publications \
         WHERE pr_number IS NOT NULL AND pr_merged = 0 \
         ORDER BY published_at DESC, id DESC",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let document_id: DocumentId = row
                .get::<String, _>("document_id")
                .parse()
                .map_err(|e: uuid::Error| DocStoreError::Corrupt(e.to_string()))?;
            decode_publication(row, document_id)
        })
        .collect()
}

/// Reflect a polled GitHub merge status back onto every publication row that
/// opened `pr_number` for `document_id` — the read-back half of F9 that
/// `open_documentation_pr` alone could never provide (opening a PR and it
/// later merging are separated by however long review takes). Idempotent:
/// re-polling an already-merged PR is a harmless no-op update. Returns how
/// many rows were updated (0 for a PR number never recorded against this
/// document).
pub async fn record_pull_request_merge(
    pool: &SqlitePool,
    document_id: DocumentId,
    pr_number: u64,
    merged: bool,
    merged_at: Option<&str>,
    merge_commit_sha: Option<&str>,
) -> Result<u64, DocStoreError> {
    let result = sqlx::query(
        "UPDATE document_publications SET pr_merged = ?, pr_merged_at = ?, pr_merge_commit_sha = ? \
         WHERE document_id = ? AND pr_number = ?",
    )
    .bind(merged)
    .bind(merged_at)
    .bind(merge_commit_sha)
    .bind(document_id.to_string())
    .bind(pr_number as i64)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Decode one `document_publications` row (the column set [`publications`]
/// and [`pending_pull_request_publications`] both select) into a
/// [`Publication`], anchored to the caller-supplied `document_id`.
fn decode_publication(
    row: &sqlx::sqlite::SqliteRow,
    document_id: DocumentId,
) -> Result<Publication, DocStoreError> {
    Ok(Publication {
        id: row.get("id"),
        document_id,
        revision: row.get::<i64, _>("revision") as u64,
        target: row.get("target"),
        git_commit: row.get("git_commit"),
        rendered_hash: row.get("rendered_hash"),
        pr_number: row.get::<Option<i64>, _>("pr_number").map(|n| n as u64),
        pr_url: row.get("pr_url"),
        pr_merged: row.get("pr_merged"),
        pr_merged_at: row.get("pr_merged_at"),
        pr_merge_commit_sha: row.get("pr_merge_commit_sha"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(content: BlockContent) -> DocumentBlock {
        DocumentBlock::with_id("b", content)
    }

    /// Every line of a multi-line callout is quoted — an unquoted second line
    /// would fall out of the blockquote in published Markdown. This changes the
    /// published bytes for multi-line callouts INTENTIONALLY (the old render
    /// quoted only the first line); the output stays deterministic.
    #[test]
    fn multi_line_callout_quotes_every_line() {
        let callout = || {
            block(BlockContent::Callout {
                kind: "warning".into(),
                text: "first line\nsecond line".into(),
            })
        };
        let md = render_document("Doc", &[callout()]);
        assert!(md.contains("> [!WARNING]\n> first line\n> second line\n"));
        // Deterministic: a second render is byte-identical.
        assert_eq!(md, render_document("Doc", &[callout()]));
    }

    /// A literal `|` in a table cell is escaped so it cannot open a phantom
    /// column, the escaping backslash is itself escaped (invertible), and a
    /// newline inside a cell folds to a space (a table row is one line).
    #[test]
    fn table_cells_escape_pipes_and_backslashes() {
        let md = render_document(
            "Doc",
            &[block(BlockContent::Table {
                rows: vec![
                    vec!["Name".into(), "Pattern".into()],
                    vec!["or".into(), "a|b".into()],
                    vec!["esc".into(), "a\\b".into()],
                    vec!["nl".into(), "a\nb".into()],
                ],
            })],
        );
        assert!(md.contains("| or | a\\|b |"), "pipe escaped: {md}");
        assert!(md.contains("| esc | a\\\\b |"), "backslash escaped: {md}");
        assert!(md.contains("| nl | a b |"), "newline folded: {md}");
    }

    /// A newline inside a heading folds to a space — the heading stays one
    /// Markdown line rather than leaking its tail as body text.
    #[test]
    fn heading_newline_is_guarded() {
        let md = render_document(
            "Doc",
            &[block(BlockContent::Heading {
                level: 2,
                text: "Payments\nService".into(),
            })],
        );
        assert!(md.contains("## Payments Service\n"), "folded: {md}");
        assert!(
            !md.contains("## Payments\nService"),
            "no split heading: {md}"
        );
    }
}

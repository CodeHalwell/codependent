-- Documentation-PR merge readback (2026-08-13 review, memory-docs vertical,
-- F9). Before this migration, `document_publications` had no PR column at
-- all — `open_documentation_pr` discarded the `PullRequest` GitHub returned
-- (`.await?; Ok(())`), so a documentation PR's number/URL were never
-- persisted, leaving nothing for a later merge-status poll to key off. These
-- columns are the missing stored handle plus the polled-back merge state;
-- NULL/0 for the two targets that never open a PR (`RepositoryFile`,
-- `DocsBranchCommit`), and for every row published before this migration.
ALTER TABLE document_publications ADD COLUMN pr_number INTEGER;
ALTER TABLE document_publications ADD COLUMN pr_url TEXT;
ALTER TABLE document_publications ADD COLUMN pr_merged INTEGER NOT NULL DEFAULT 0;
ALTER TABLE document_publications ADD COLUMN pr_merged_at TEXT;
ALTER TABLE document_publications ADD COLUMN pr_merge_commit_sha TEXT;

-- The merge-status sync's poll list (`pending_pull_request_publications`)
-- filters exactly this shape: an opened PR not yet observed merged.
CREATE INDEX idx_document_publications_pending_pr
    ON document_publications(pr_number)
    WHERE pr_number IS NOT NULL AND pr_merged = 0;

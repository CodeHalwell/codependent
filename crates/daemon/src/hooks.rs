//! Hook discovery and registration (adoption 08): filesystem → `hooks` table.
//! Discovery is NOT activation (migration 0027 rule 1): rows land `pending`
//! and dispatch only after `codypendent hook approve` binds a content hash.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use codypendent_sandbox::hook::{
    parse_hook, validate_event_set, HookEvent, HookKind, HookScope, HookSpec,
};
use sqlx::SqlitePool;
use uuid::Uuid;

/// `<data_dir>/hooks/` — the operator's own hooks (the skills `user_skills_root`
/// convention; NOT `~/.codypendent`, which is not a path this product uses).
#[must_use]
pub fn user_hooks_root(data_dir: &Path) -> PathBuf {
    data_dir.join("hooks")
}

/// `<repo>/.codypendent/hooks/` — repository-committed, untrusted.
#[must_use]
pub fn repository_hooks_root(repository_root: &Path) -> PathBuf {
    repository_root.join(".codypendent").join("hooks")
}

/// Outcome of scanning one hook root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookScanOutcome {
    /// Hook IDs that were registered (or updated with unchanged content hash).
    pub registered: Vec<String>,
    /// Hook IDs whose content hash changed and were reset to 'pending'.
    pub reset_to_pending: Vec<String>,
    /// Non-fatal per-file or root-level failures.
    pub failures: Vec<(PathBuf, String)>,
}

/// A row from the `hooks` table.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HookRecord {
    pub id: String,
    pub registry_item_id: Option<String>,
    pub hook_id: String,
    pub name: String,
    pub scope_kind: String,
    pub scope_key: String,
    pub event: String,
    pub kind: String,
    pub priority: i64,
    pub source_path: String,
    pub content_hash: String,
    pub spec_json: String,
    pub approval_state: String,
    pub approved_content_hash: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Scan one root as one scope. Accepts `<root>/*.toml` and `<root>/*/hook.toml`
/// (a package dir may bundle the scripts its `program`/`args` reference).
/// Deterministic: entries sorted by path before parsing; `validate_event_set`
/// runs per (scope, event) over everything the root yielded, so a hostile
/// fan-out or duplicate id fails the whole ROOT (fail-closed), not one file.
pub async fn scan_hook_root(
    pool: &SqlitePool,
    root: &Path,
    scope: HookScope,
    scope_key: &str,
) -> HookScanOutcome {
    let mut outcome = HookScanOutcome::default();

    if !root.exists() || !root.is_dir() {
        return outcome;
    }

    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(err) => {
            outcome
                .failures
                .push((root.to_path_buf(), format!("reading directory: {err}")));
            return outcome;
        }
    };

    let mut candidate_paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path.extension().is_some_and(|ext| ext == "toml") {
                candidate_paths.push(path);
            }
        } else if path.is_dir() {
            let nested = path.join("hook.toml");
            if nested.is_file() {
                candidate_paths.push(nested);
            }
        }
    }

    // Deterministic ordering across enumeration order.
    candidate_paths.sort();

    struct Candidate {
        path: PathBuf,
        content_hash: String,
        spec: HookSpec,
    }

    let mut candidates = Vec::new();
    for path in candidate_paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                outcome
                    .failures
                    .push((path.clone(), format!("reading file: {err}")));
                continue;
            }
        };
        let content_hash = HookSpec::content_digest(&raw);
        match parse_hook(&raw, scope) {
            Ok(spec) => {
                candidates.push(Candidate {
                    path,
                    content_hash,
                    spec,
                });
            }
            Err(err) => {
                outcome
                    .failures
                    .push((path.clone(), format!("parsing hook: {err}")));
            }
        }
    }

    // Group specs by event to run validate_event_set.
    let mut by_event: BTreeMap<HookEvent, Vec<HookSpec>> = BTreeMap::new();
    for c in &candidates {
        by_event
            .entry(c.spec.event)
            .or_default()
            .push(c.spec.clone());
    }

    for (event, specs) in by_event {
        if let Err(err) = validate_event_set(event, &specs) {
            outcome.failures.push((
                root.to_path_buf(),
                format!(
                    "event set validation failed for event `{}`: {err}",
                    event.as_str()
                ),
            ));
            // Fail the entire root on hostile fan-out or duplicate id.
            return outcome;
        }
    }

    // Perform database operations in a transaction.
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            outcome.failures.push((
                root.to_path_buf(),
                format!("beginning database transaction: {err}"),
            ));
            return outcome;
        }
    };

    let mut scanned_source_paths = BTreeSet::new();

    for c in candidates {
        let source_path_str = c.path.to_string_lossy().to_string();
        scanned_source_paths.insert(source_path_str.clone());

        let existing: Option<(String, String, Option<String>)> = match sqlx::query_as(
            "SELECT content_hash, approval_state, approved_content_hash FROM hooks WHERE scope_kind = ? AND scope_key = ? AND hook_id = ?",
        )
        .bind(scope.as_str())
        .bind(scope_key)
        .bind(&c.spec.id)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(opt) => opt,
            Err(err) => {
                outcome.failures.push((c.path.clone(), format!("querying existing hook: {err}")));
                continue;
            }
        };

        let now = Utc::now().to_rfc3339();
        let id = Uuid::now_v7().to_string();
        let spec_json = match serde_json::to_string(&c.spec) {
            Ok(json) => json,
            Err(err) => {
                outcome
                    .failures
                    .push((c.path.clone(), format!("serializing spec: {err}")));
                continue;
            }
        };

        let kind_str = match c.spec.kind {
            HookKind::Observe => "observe",
            HookKind::Validate => "validate",
            HookKind::Mutate => "mutate",
        };

        let res = sqlx::query(
            r#"
INSERT INTO hooks (id, hook_id, name, scope_kind, scope_key, event, kind, priority,
                   source_path, content_hash, spec_json, approval_state, created_at, updated_at)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)
ON CONFLICT (scope_kind, scope_key, hook_id) DO UPDATE SET
    name = excluded.name, event = excluded.event, kind = excluded.kind,
    priority = excluded.priority, source_path = excluded.source_path,
    content_hash = excluded.content_hash, spec_json = excluded.spec_json,
    updated_at = excluded.updated_at,
    approval_state = CASE
        WHEN hooks.approved_content_hash = excluded.content_hash THEN hooks.approval_state
        ELSE 'pending'
    END
"#,
        )
        .bind(&id)
        .bind(&c.spec.id)
        .bind(&c.spec.name)
        .bind(scope.as_str())
        .bind(scope_key)
        .bind(c.spec.event.as_str())
        .bind(kind_str)
        .bind(i64::from(c.spec.priority))
        .bind(&source_path_str)
        .bind(&c.content_hash)
        .bind(&spec_json)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await;

        if let Err(err) = res {
            outcome
                .failures
                .push((c.path.clone(), format!("upserting hook: {err}")));
            continue;
        }

        if let Some((old_hash, _approval_state, _approved_hash)) = existing {
            if old_hash != c.content_hash {
                outcome.reset_to_pending.push(c.spec.id);
            } else {
                outcome.registered.push(c.spec.id);
            }
        } else {
            outcome.registered.push(c.spec.id);
        }
    }

    // Delete rows whose source_path no longer exists in this root.
    let existing_rows: Result<Vec<(String, String)>, _> =
        sqlx::query_as("SELECT id, source_path FROM hooks WHERE scope_kind = ? AND scope_key = ?")
            .bind(scope.as_str())
            .bind(scope_key)
            .fetch_all(&mut *tx)
            .await;

    if let Ok(rows) = existing_rows {
        for (row_id, source_path) in rows {
            if !scanned_source_paths.contains(&source_path) {
                let _ = sqlx::query("DELETE FROM hooks WHERE id = ?")
                    .bind(row_id)
                    .execute(&mut *tx)
                    .await;
            }
        }
    }

    if let Err(err) = tx.commit().await {
        outcome
            .failures
            .push((root.to_path_buf(), format!("committing transaction: {err}")));
    }

    outcome
}

/// Scan both user and repository hook roots.
pub async fn scan_installed_hooks(
    pool: &SqlitePool,
    data_dir: &Path,
    repository_root: Option<&Path>,
) -> Vec<HookScanOutcome> {
    let mut outcomes = Vec::new();
    let user_root = user_hooks_root(data_dir);
    outcomes.push(scan_hook_root(pool, &user_root, HookScope::User, "").await);

    if let Some(repo) = repository_root {
        let repo_root = repository_hooks_root(repo);
        let repo_key = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        let repo_key_str = repo_key.to_string_lossy().to_string();
        outcomes.push(scan_hook_root(pool, &repo_root, HookScope::Repository, &repo_key_str).await);
    }
    outcomes
}

/// List all hooks across all scopes.
pub async fn list_hooks(pool: &SqlitePool) -> Result<Vec<HookRecord>, sqlx::Error> {
    sqlx::query_as::<_, HookRecord>(
        "SELECT id, registry_item_id, hook_id, name, scope_kind, scope_key, event, kind, priority, \
         source_path, content_hash, spec_json, approval_state, approved_content_hash, approved_by, \
         approved_at, created_at, updated_at FROM hooks ORDER BY priority, id",
    )
    .fetch_all(pool)
    .await
}

/// Find a hook record by `hook_id` slug or database `id`.
pub async fn get_hook(pool: &SqlitePool, hook_id: &str) -> Result<Option<HookRecord>, sqlx::Error> {
    sqlx::query_as::<_, HookRecord>(
        "SELECT id, registry_item_id, hook_id, name, scope_kind, scope_key, event, kind, priority, \
         source_path, content_hash, spec_json, approval_state, approved_content_hash, approved_by, \
         approved_at, created_at, updated_at FROM hooks WHERE hook_id = ? OR id = ?",
    )
    .bind(hook_id)
    .bind(hook_id)
    .fetch_optional(pool)
    .await
}

/// Approve a hook by `hook_id` or database `id`.
/// Sets `approval_state = 'approved'`, `approved_content_hash = content_hash`, `approved_by = user`, `approved_at = now`.
pub async fn approve_hook(
    pool: &SqlitePool,
    hook_id: &str,
    user: &str,
) -> Result<bool, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    let res = sqlx::query(
        "UPDATE hooks SET approval_state = 'approved', approved_content_hash = content_hash, \
         approved_by = ?, approved_at = ?, updated_at = ? WHERE (hook_id = ? OR id = ?) AND approval_state != 'approved'",
    )
    .bind(user)
    .bind(&now)
    .bind(&now)
    .bind(hook_id)
    .bind(hook_id)
    .execute(pool)
    .await?;

    Ok(res.rows_affected() > 0)
}

/// Reject a hook by `hook_id` or database `id`.
pub async fn reject_hook(
    pool: &SqlitePool,
    hook_id: &str,
    user: &str,
) -> Result<bool, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    let res = sqlx::query(
        "UPDATE hooks SET approval_state = 'rejected', \
         approved_by = ?, approved_at = ?, updated_at = ? WHERE hook_id = ? OR id = ?",
    )
    .bind(user)
    .bind(&now)
    .bind(&now)
    .bind(hook_id)
    .bind(hook_id)
    .execute(pool)
    .await?;

    Ok(res.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_hook_toml(id: &str, scope: &str, event: &str) -> String {
        format!(
            r#"
schema_version = 1
id = "{id}"
name = "Sample {id}"
scope = "{scope}"
event = "{event}"
kind = "validate"
priority = 100

[runtime]
type = "command"
program = "/bin/sh"
args = ["-c", "exit 0"]
timeout_seconds = 30

[policy]
failure = "block"
requires_approval = false
network = "deny"
"#
        )
    }

    #[tokio::test]
    async fn scan_registers_pending_never_approved() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let hook_file = dir.path().join("test.toml");
        std::fs::write(
            &hook_file,
            sample_hook_toml("test.hook", "user", "tool.pre"),
        )
        .unwrap();

        let outcome = scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert_eq!(outcome.registered, vec!["test.hook"]);
        assert!(outcome.failures.is_empty());

        let record = get_hook(&pool, "test.hook").await.unwrap().unwrap();
        assert_eq!(record.approval_state, "pending");
        assert!(record.approved_content_hash.is_none());
    }

    #[tokio::test]
    async fn changed_hash_resets_to_pending_and_stops_dispatch() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let hook_file = dir.path().join("test.toml");
        std::fs::write(
            &hook_file,
            sample_hook_toml("test.hook", "user", "tool.pre"),
        )
        .unwrap();

        let outcome = scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert_eq!(outcome.registered, vec!["test.hook"]);

        // Approve it
        let approved = approve_hook(&pool, "test.hook", "operator").await.unwrap();
        assert!(approved);
        let record = get_hook(&pool, "test.hook").await.unwrap().unwrap();
        assert_eq!(record.approval_state, "approved");
        assert_eq!(
            record.approved_content_hash,
            Some(record.content_hash.clone())
        );

        // Modify file content
        let mut modified = sample_hook_toml("test.hook", "user", "tool.pre");
        modified.push_str("\n# comment\n");
        std::fs::write(&hook_file, modified).unwrap();

        // Rescan
        let outcome2 = scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert_eq!(outcome2.reset_to_pending, vec!["test.hook"]);

        let record2 = get_hook(&pool, "test.hook").await.unwrap().unwrap();
        assert_eq!(record2.approval_state, "pending");
        // approved_content_hash is the old hash, which no longer matches current content_hash
        assert_ne!(Some(record2.content_hash), record2.approved_content_hash);
    }

    #[tokio::test]
    async fn scope_mismatch_is_refused_at_scan() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let hook_file = dir.path().join("test.toml");
        // Claim user scope but discovered in repository scope
        std::fs::write(
            &hook_file,
            sample_hook_toml("test.hook", "user", "tool.pre"),
        )
        .unwrap();

        let outcome =
            scan_hook_root(&pool, dir.path(), HookScope::Repository, "/path/to/repo").await;
        assert!(outcome.registered.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].1.contains("cannot claim a tier"));

        let record = get_hook(&pool, "test.hook").await.unwrap();
        assert!(record.is_none());
    }

    #[tokio::test]
    async fn removed_file_row_is_deleted() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();
        let hook_file = dir.path().join("test.toml");
        std::fs::write(
            &hook_file,
            sample_hook_toml("test.hook", "user", "tool.pre"),
        )
        .unwrap();

        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert!(get_hook(&pool, "test.hook").await.unwrap().is_some());

        // Remove the file and rescan
        std::fs::remove_file(&hook_file).unwrap();
        scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert!(get_hook(&pool, "test.hook").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn hostile_fanout_fails_the_root() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();

        // Create 33 hooks for tool.pre
        for i in 0..33 {
            let hook_file = dir.path().join(format!("test_{i}.toml"));
            std::fs::write(
                &hook_file,
                sample_hook_toml(&format!("test.hook.{i}"), "user", "tool.pre"),
            )
            .unwrap();
        }

        let outcome = scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert!(outcome.registered.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].1.contains("above the 32 ceiling"));

        let list = list_hooks(&pool).await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn scan_is_deterministic_across_enumeration_order() {
        let dir = tempdir().unwrap();
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .unwrap();

        let hook_b = dir.path().join("b.toml");
        let hook_a = dir.path().join("a.toml");
        std::fs::write(&hook_b, sample_hook_toml("hook.b", "user", "tool.pre")).unwrap();
        std::fs::write(&hook_a, sample_hook_toml("hook.a", "user", "tool.pre")).unwrap();

        let outcome = scan_hook_root(&pool, dir.path(), HookScope::User, "").await;
        assert_eq!(outcome.registered, vec!["hook.a", "hook.b"]);
    }
}

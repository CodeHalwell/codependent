# Adoption 07 — Arity-learned "always allow" + command scanning

**Effort:** M · **Depends on:** nothing · **Reference:** reference-repos/opencode/packages/opencode/src/permission/arity.ts, src/tool/shell.ts (the `collect`/`argPath`/`pathArgs` scan, ~lines 29–415), src/permission/index.ts (`always` patterns, auto-resolve of satisfied pendings); reference-repos/codex/codex-rs/execpolicy/src/amend.rs (persist-a-rule)
**Ported from:** opencode + codex · **Status:** ⬜ not started

## 1. Summary

Today "approve for the rest of the run" (`ApprovalScope::Run`, the TUI's `A` key) reuses an approval only for a **byte-identical** action digest — `cargo test` approved does nothing for `cargo test -p codypendent-daemon`. This adoption makes "always allow" learn the *human-understandable command* instead: an arity dictionary ("`git` = 2 tokens, `npm run` = 3") computes the prefix a command belongs to, so approving `git checkout main` with the new **Pattern** scope auto-approves every later `git checkout <anything>` in the run — and the approval card shows the exact rule it is about to create. A second new scope, **Repository**, persists the learned rule durably (codex's `amend.rs` idea, adapted from a rule file to an append-only table), so it survives the run. Alongside, `eval_command` gains **argument scanning**: path-like arguments of file-manipulating commands are resolved and classified against the run's path scopes — a command touching a denied path is denied outright, and one touching paths outside every granted root is escalated to a fresh, non-reusable, non-learnable approval whose card names the external directories. Deny-wins is only ever strengthened: patterns substitute solely for the human's "yes" on actions the policy engine has *already, freshly* classified `RequireApproval`; a `Deny` never reaches any matching.

## 2. Reference implementation

All paths relative to `reference-repos/opencode/packages/opencode/src/` unless noted.

**The arity dictionary** (`permission/arity.ts`): a flat map `prefix string → token count`. Rules (from the generation prompt embedded in the file): flags never count as tokens, only subcommands; longest matching prefix wins; a longer prefix is listed only when its arity differs from what the shorter one implies (`git: 2` covers `git checkout`; `npm run: 3` overrides `npm: 2`). `prefix(tokens)` walks candidate prefixes from the full token list down to length 1, joins with spaces, looks each up, and on the first hit returns `tokens.slice(0, arity)`; with no hit it returns `tokens.slice(0, 1)` (just the program — the conservative default). Examples: `touch foo.txt → [touch]`; `git checkout main → [git, checkout]`; `npm run dev → [npm, run, dev]`.

**How the pattern is used** (`tool/shell.ts` `collect`, ~line 392–411): for every parsed sub-command, opencode adds the literal command source to `scan.patterns` (what must be allowed *now*) and `BashArity.prefix(tokens).join(" ") + " *"` to `scan.always` (the rule an "always" reply will create). `permission/index.ts::reply` (`"always"` branch, ~lines 145–166) appends each request's **pre-computed** `always` patterns as `{permission, pattern, action: allow}` rules into the session's approved ruleset, then walks the other pending requests and auto-resolves any the new rules now satisfy. `evaluate` is last-match-wins wildcard matching; the TUI's permission card "always shows the rule it will create".

**Command scanning** (`tool/shell.ts`): tree-sitter (bash + PowerShell WASM grammars) parses the raw shell string into sub-commands (`commands()`/`parts()`); for each sub-command whose head is in the `FILES` set (`cd/rm/cp/mv/mkdir/touch/chmod/chown/cat` + PowerShell aliases + `cmd.exe` names), `pathArgs` extracts non-flag arguments, `argPath` unquotes, expands `~`/`$env:`/provider prefixes, refuses dynamic text (`$(`, backticks, leading glob), trims a glob suffix to its literal prefix (`prefix()` at line 181), resolves against cwd, and any resolved path **outside the instance's roots** contributes its directory to `scan.dirs`; those become an `external_directory` permission ask with concrete `dir/*` globs (`ask()` ~line 263). Commands whose head is in `CWD` (`cd`, `pushd`, …) are excluded from the shell-permission patterns themselves.

**Persisting a rule** (reference-repos/codex/codex-rs/execpolicy/src/amend.rs): `blocking_append_allow_prefix_rule(policy_path, prefix)` refuses an empty prefix, serializes each token with `serde_json::to_string` (so quoting is unambiguous), formats `prefix_rule(pattern=[…], decision="allow")`, and appends one line to the policy file under an advisory file lock. The shape to keep: **"always allow" persists a real, inspectable, individually-revocable rule** — never a mutation of an opaque blob.

## 3. Current state in codypendent (verified)

- **The shell tool is structured, not a shell string.** `crates/runtime/src/tools/shell.rs::CommandRequest { program: PathBuf, args: Vec<String>, cwd, environment, timeout }` — "Arguments, passed verbatim — never re-parsed by a shell." There is **no bash text anywhere on this path**, so the reference's tree-sitter machinery has no job here (see §4, Decision 1).
- **Policy evaluation** (`crates/daemon/src/policy/mod.rs::eval_command`): mode overlay → exact-match program allow-list (`CommandScope::allows_program`, deliberately never basename — `crates/daemon/src/policy/scope.rs` ~line 180) → git special-casing (mutating/networked subcommand vs mode; force-push and branch-delete dispositions via `command_disposition`) → the built-in default `require(Capability::CommandExecute(scope), "policy.command-requires-approval")` for every allow-listed program. `require()` sets `approval_reusable: true`; `require_once()` (used by `ApprovalAction::AlwaysApproval` dispositions) sets it false. `Decision::Deny` short-circuits in `agent.rs::run_tool` **before** any approval request exists, so no reuse path can ever see a denied action.
- **Approval reuse** (`crates/daemon/src/approvals.rs`): `request_with_reuse(.., allow_run_reuse)` consults `run_scoped_match(pool, run_id, digest)` — rows `WHERE run_id = ? AND scope = 'run' AND state = 'approved'` whose stored `action_json` hashes to the same `action_digest` (hex SHA-256 of the action's canonical JSON). Auto-approval still writes an `approved` row (`resolved_by = 'auto:run-scope'`) plus both ledger events. `resolve_in_tx` stamps the human-chosen scope into `approvals.scope`.
- **`ApprovalScope::Pattern` and `::Repository` already exist on the wire** (`crates/protocol/src/run.rs`) and in the DB mapping (`approvals.rs::scope_to_db` → `'pattern'` / `'repository'`) — but **nothing sends or matches them**: `crates/tui/src/input.rs::map_approval_key` offers only `a` = Once, `A` = Run, `r` = Reject, and `run_scoped_match` queries only `scope = 'run'`. They are reserved names this adoption gives semantics to.
- **The approval request path** threads `ApprovalRequest { session_id, run_id, action, risk, capabilities, allow_run_reuse }` from `agent.rs::run_tool` through the pool-erased `RunJournal` closure (`crates/codypendentd/src/executor.rs::run_journal`) into `broker.request_with_reuse`. The runtime knows the run's repository (`EvalContext.repository` via `eval_ctx(run)`), but the broker currently does not.
- **Path scopes**: `PolicyEngine::file_read_scope/file_write_scope(ctx)` build canonicalized `PathScope`s (deny-wins, component-wise containment, lenient canonicalization for not-yet-existing leaves — `crates/daemon/src/policy/scope.rs`). `eval_command` does **not** currently look at a command's path arguments at all: `cat $HOME/.aws/credentials` with an approved `cat` reaches the human as an ordinary command approval with no path warning.
- **Environment is a first-class smuggling concern**: `ProposedAction::ExecuteCommand.environment` exists precisely so the approver sees the complete child environment (`crates/protocol/src/run.rs` doc comment).
- **Must not break**: exact-digest `Run`-scope reuse (existing behavior and tests `run_scoped_resolution_auto_approves_identical_proposal`); `AlwaysApproval` dispositions bypassing all reuse (`allow_run_reuse = false`); the deny-before-approval ordering in `run_tool`; migrations append-only (latest `migrations/0033_workflow_run_owner.sql`).

## 4. Design

**Decision 1 — no tree-sitter, and no shell lexer at all.** The reference parses raw bash because its shell tool accepts raw bash. Codypendent's `shell.run` receives `program` + `args` already tokenized by the model and executed without a shell, so pipelines, substitutions, and quoting *cannot occur* on this path — the only way to smuggle one is via an interpreter (`sh -c "…"`), which the built-in allow-list excludes and which this spec makes structurally unlearnable (RULE 5). Porting tree-sitter (two WASM grammars + a runtime) or even `shell-words` would add a dependency to parse text that never exists. Zero new dependencies.

**Decision 2 — patterns gate the *approval*, never the *policy*.** A learned rule is consulted only inside `ApprovalBroker` reuse, i.e. only for actions the policy engine has just returned `RequireApproval` + `approval_reusable: true` for. `Deny` never reaches the broker (structural, `run_tool` ordering), `AlwaysApproval` never reaches reuse (`allow_run_reuse = false`), and a policy file change immediately affects every future evaluation regardless of learned rules. This is how "always allow persists real rules" composes with deny-wins without weakening it.

**Decision 3 — persistence is a table, not a rule file.** Codex appends DSL lines to a policy file under advisory locks; codypendent's durable state lives in SQLite behind append-only migrations (ADR-003), and a rule file would create a second policy source the merge invariant doesn't govern. `approval_rules` rows are inspectable (`codypendent approvals rules`) and individually revocable (`revoked_at`), keeping amend.rs's real property.

**Decision 4 — scanning strengthens, never widens.** Path-argument scanning can only move an outcome toward stricter: `RequireApproval(reusable)` → `Deny` (path under `fs_deny`) or → `RequireApproval(non-reusable, non-learnable)` (path outside all roots). An in-scope path changes nothing.

The four moving parts:

1. `crates/daemon/src/policy/arity.rs` — the dictionary + `command_prefix()` (faithful port of `arity.ts`, including "raw token slice, flags not skipped at match time"). `command_pattern(action) -> Option<String>` renders `git checkout *`, refusing interpreters, empty programs, path-bearing programs, and actions with a non-empty environment.
2. `ApprovalBroker` — `resolve_in_tx` computes and stores the pattern when scope is `Pattern`/`Repository` (new `approvals.pattern` column; `Repository` additionally inserts an `approval_rules` row); `request_with_reuse` matches, in order: exact run digest (unchanged) → run-scoped pattern → repository rules. Auto-approvals record which rule fired (`resolved_by = 'auto:pattern'` / `'auto:repo-rule:<id>'`).
3. `eval_command` — argument scanning against the read/write scopes.
4. TUI + CLI — `p`/`P` keys on command approval cards showing the literal rule; `codypendent approvals rules list|revoke`.

Deviations from the reference: opencode auto-resolves *other pending* requests the new rule satisfies; codypendent's agent loop parks at most one approval per run at a time, so that sweep is dropped (noted in §10). opencode's `external_directory` is a separate permission type with its own "always" globs; codypendent folds the external-path finding into the existing command approval as a stricter disposition instead of adding a second permission vocabulary (the path scopes in `policy.toml` remain the sole authority over paths).

## 5. Changes, file by file

### 5.1 `crates/daemon/src/policy/arity.rs` (new)

```rust
//! The command-arity dictionary (adoption 07), ported from opencode's
//! `permission/arity.ts`: how many leading tokens make a shell command
//! "human-understandable", so an "always allow" learns `git checkout *`
//! rather than the literal invocation.
//!
//! Port-faithful semantics: the lookup slices RAW tokens — flags are excluded
//! from the dictionary's counts by construction, not skipped at match time —
//! and the longest listed prefix wins. Unknown program ⇒ the program token
//! alone (the conservative default).

/// `(prefix, arity)` pairs, sorted by prefix for the binary search below.
/// Verbatim port of opencode's generated dictionary (entries whose programs
/// can never pass codypendent's allow-list are still kept: the dictionary is
/// data about commands, the allow-list is policy about them).
const ARITY: &[(&str, usize)] = &[
    ("aws", 3), ("az", 3), ("bazel", 2), ("brew", 2), ("bun", 2), ("bun run", 3),
    ("bun x", 3), ("cargo", 2), ("cargo add", 3), ("cargo run", 3), ("cat", 1),
    ("cd", 1), ("cdk", 2), ("cf", 2), ("chmod", 1), ("chown", 1), ("cmake", 2),
    ("composer", 2), ("consul", 2), ("consul kv", 3), ("cp", 1), ("crictl", 2),
    ("deno", 2), ("deno task", 3), ("docker", 2), ("docker builder", 3),
    ("docker compose", 3), ("docker container", 3), ("docker image", 3),
    ("docker network", 3), ("docker volume", 3), ("doctl", 3), ("echo", 1),
    ("eksctl", 2), ("eksctl create", 3), ("env", 1), ("export", 1),
    ("firebase", 2), ("flyctl", 2), ("gcloud", 3), ("gh", 3), ("git", 2),
    ("git config", 3), ("git remote", 3), ("git stash", 3), ("go", 2),
    ("gradle", 2), ("grep", 1), ("helm", 2), ("heroku", 2), ("hugo", 2),
    ("ip", 2), ("ip addr", 3), ("ip link", 3), ("ip netns", 3), ("ip route", 3),
    ("kill", 1), ("killall", 1), ("kind", 2), ("kind create", 3),
    ("kubectl", 2), ("kubectl kustomize", 3), ("kubectl rollout", 3),
    ("kustomize", 2), ("ln", 1), ("ls", 1), ("make", 2), ("mc", 2),
    ("mc admin", 3), ("minikube", 2), ("mkdir", 1), ("mongosh", 2), ("mv", 1),
    ("mvn", 2), ("mysql", 2), ("ng", 2), ("npm", 2), ("npm exec", 3),
    ("npm init", 3), ("npm run", 3), ("npm view", 3), ("nvm", 2), ("nx", 2),
    ("openssl", 2), ("openssl req", 3), ("openssl x509", 3), ("pip", 2),
    ("pipenv", 2), ("pnpm", 2), ("pnpm dlx", 3), ("pnpm exec", 3),
    ("pnpm run", 3), ("podman", 2), ("podman container", 3),
    ("podman image", 3), ("poetry", 2), ("ps", 1), ("psql", 2), ("pulumi", 2),
    ("pulumi stack", 3), ("pwd", 1), ("pyenv", 2), ("python", 2), ("rake", 2),
    ("rbenv", 2), ("redis-cli", 2), ("rm", 1), ("rmdir", 1), ("rustup", 2),
    ("serverless", 2), ("sfdx", 3), ("skaffold", 2), ("sleep", 1), ("sls", 2),
    ("source", 1), ("sst", 2), ("swift", 2), ("systemctl", 2), ("tail", 1),
    ("terraform", 2), ("terraform workspace", 3), ("tmux", 2), ("touch", 1),
    ("turbo", 2), ("ufw", 2), ("unset", 1), ("vault", 2), ("vault auth", 3),
    ("vault kv", 3), ("vercel", 2), ("volta", 2), ("which", 1), ("wp", 2),
    ("yarn", 2), ("yarn dlx", 3), ("yarn run", 3),
];

fn lookup(prefix: &str) -> Option<usize> {
    ARITY
        .binary_search_by(|(p, _)| p.cmp(&prefix))
        .ok()
        .map(|i| ARITY[i].1)
}

/// The human-understandable prefix of `tokens` (port of `BashArity.prefix`):
/// longest listed prefix wins; unknown ⇒ the first token; empty ⇒ empty.
#[must_use]
pub fn command_prefix(tokens: &[String]) -> Vec<String> {
    for len in (1..=tokens.len()).rev() {
        let candidate = tokens[..len].join(" ");
        if let Some(arity) = lookup(&candidate) {
            return tokens[..arity.min(tokens.len())].to_vec();
        }
    }
    tokens.first().cloned().into_iter().collect()
}

/// Programs whose arguments ARE programs/scripts: a learned prefix would be a
/// blank check (`sh *`, `python *`), so these never produce a pattern. Closed
/// list; extending it only ever narrows.
pub const UNLEARNABLE_PROGRAMS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish", "ksh", "pwsh", "powershell",
    "python", "python3", "node", "deno", "bun", "ruby", "perl", "php",
    "env", "xargs", "sudo", "doas", "nice", "nohup", "time", "timeout", "eval",
];

/// The `always allow` pattern for an ExecuteCommand, or `None` when learning
/// is refused. Refusals (each a RULE in §7):
/// - program not a bare name (path separators ⇒ pinned binary, no pattern);
/// - program in [`UNLEARNABLE_PROGRAMS`];
/// - non-empty `environment` (a pattern learned from a clean env must never
///   auto-approve a call that adds variables — the `ExecuteCommand.environment`
///   smuggling channel);
/// - empty token list.
#[must_use]
pub fn command_pattern(program: &str, args: &[String], environment: &[(String, String)])
    -> Option<String>
{
    if program.is_empty()
        || program.contains('/')
        || program.contains('\\')
        || !environment.is_empty()
        || UNLEARNABLE_PROGRAMS.contains(&program)
    {
        return None;
    }
    let mut tokens = Vec::with_capacity(args.len() + 1);
    tokens.push(program.to_string());
    tokens.extend(args.iter().cloned());
    let prefix = command_prefix(&tokens);
    Some(format!("{} *", prefix.join(" ")))
}

/// Whether `pattern` (from [`command_pattern`]) covers this invocation:
/// the pattern's tokens (sans the trailing `*`) must equal the invocation's
/// leading tokens exactly. No globbing inside tokens — `*` is only ever the
/// whole tail.
#[must_use]
pub fn pattern_matches(pattern: &str, program: &str, args: &[String]) -> bool {
    let Some(head) = pattern.strip_suffix(" *") else { return false; };
    let want: Vec<&str> = head.split(' ').collect();
    let mut have = vec![program];
    have.extend(args.iter().map(String::as_str));
    have.len() >= want.len() && have[..want.len()] == want[..]
}
```

Register in `crates/daemon/src/policy/mod.rs` (`mod arity;` + `pub use arity::{command_pattern, command_prefix, pattern_matches};`) and mention the dictionary is sorted (add a unit test asserting `ARITY.windows(2).all(|w| w[0].0 < w[1].0)` so the binary search stays sound as entries are added).

### 5.2 `migrations/0038_approval_patterns.sql` (append-only; renumber to the next free number when landing)

```sql
-- Adoption 07: arity-learned approval patterns.
--
-- `approvals.pattern` records the rule a Pattern/Repository-scoped resolution
-- created (e.g. 'git checkout *'), computed server-side from the approved
-- action at resolve time — never wire-supplied. NULL for once/run scopes.
ALTER TABLE approvals ADD COLUMN pattern TEXT;

-- Durable, per-repository learned rules ("always allow persists real rules",
-- codex execpolicy/amend.rs adapted to the house append-only-SQLite shape).
-- Rows are never updated except to stamp revoked_at; revocation is a tombstone
-- so the audit trail keeps what was in force when.
CREATE TABLE approval_rules (
    id TEXT PRIMARY KEY,
    repository TEXT NOT NULL,             -- canonical repository root
    kind TEXT NOT NULL,                   -- 'command-prefix' (closed set, v1)
    pattern TEXT NOT NULL,                -- e.g. 'git checkout *'
    created_from_approval TEXT REFERENCES approvals(id),
    created_by TEXT NOT NULL,             -- principal uid, as approvals.resolved_by
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT
);

CREATE INDEX idx_approval_rules_lookup
    ON approval_rules(repository, kind, revoked_at);
```

### 5.3 `crates/daemon/src/approvals.rs`

**`ApprovalRequest` plumbing:** `request`/`request_with_reuse`/`request_with_id_and_reuse` gain a `repository: Option<String>` parameter (canonical run repository root; `None` disables repository-rule matching). Threaded from `crates/runtime/src/agent.rs::ApprovalRequest` (new field `pub repository: Option<String>`, filled from the run context in `run_tool`) through `crates/codypendentd/src/executor.rs::run_journal`.

**Matching** — extend the reuse check (only reached when `allow_run_reuse`, i.e. never for `AlwaysApproval` dispositions):

```rust
        let auto_approve = if allow_run_reuse {
            if self.run_scoped_match(pool, run_id, &digest).await? {
                Some(AutoApproval::RunDigest)
            } else if let Some(rule) = self.pattern_match(pool, run_id, repository.as_deref(), &action).await? {
                Some(rule)
            } else {
                None
            }
        } else {
            None
        };
```

```rust
/// Which reuse rule auto-approved a request — recorded verbatim in
/// `approvals.resolved_by` so the audit trail names the authority.
enum AutoApproval {
    /// Byte-identical action already Run-approved (existing behavior).
    RunDigest,                       // resolved_by = "auto:run-scope"
    /// A Pattern-scoped approval in this run covers the prefix.
    RunPattern { pattern: String },  // resolved_by = "auto:pattern:<pattern>"
    /// A persisted repository rule covers the prefix.
    RepositoryRule { rule_id: String, pattern: String }, // "auto:repo-rule:<id>"
}
```

```rust
    /// Prefix-pattern reuse for ExecuteCommand actions ONLY. Both the learned
    /// pattern and the candidate must be learnable (`command_pattern` returns
    /// Some for the candidate) — an interpreter call or an env-carrying call
    /// can never be covered, even by a rule that would textually match.
    async fn pattern_match(
        &self,
        pool: &SqlitePool,
        run_id: RunId,
        repository: Option<&str>,
        action: &ProposedAction,
    ) -> Result<Option<AutoApproval>, ApprovalError> {
        let ProposedAction::ExecuteCommand { program, args, environment, .. } = action else {
            return Ok(None);
        };
        // Candidate must itself be learnable — this re-checks the environment
        // and interpreter rules on the CANDIDATE, not just on the learned side.
        if crate::policy::command_pattern(program, args, environment).is_none() {
            return Ok(None);
        }
        // (1) run-scoped Pattern approvals.
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT pattern FROM approvals \
             WHERE run_id = ? AND scope = 'pattern' AND state = 'approved' \
               AND pattern IS NOT NULL",
        )
        .bind(run_id.to_string())
        .fetch_all(pool)
        .await?;
        for (pattern,) in rows {
            if crate::policy::pattern_matches(&pattern, program, args) {
                return Ok(Some(AutoApproval::RunPattern { pattern }));
            }
        }
        // (2) persisted repository rules.
        if let Some(repository) = repository {
            let rules: Vec<(String, String)> = sqlx::query_as(
                "SELECT id, pattern FROM approval_rules \
                 WHERE repository = ? AND kind = 'command-prefix' AND revoked_at IS NULL",
            )
            .bind(repository)
            .fetch_all(pool)
            .await?;
            for (rule_id, pattern) in rules {
                if crate::policy::pattern_matches(&pattern, program, args) {
                    return Ok(Some(AutoApproval::RepositoryRule { rule_id, pattern }));
                }
            }
        }
        Ok(None)
    }
```

**`resolve_in_tx`** — when `decision == Approve` and `scope` is `Pattern` or `Repository`: load the row's `action_json`, compute `command_pattern(...)`; if `None`, **fail the resolve** with a new `ApprovalError::PatternUnavailable` ("this action cannot be generalized to a rule — approve it Once or for the Run instead") rather than silently downgrading; if `Some(pattern)`, `UPDATE approvals SET pattern = ?` in the same statement that stamps state/scope. For `Repository` scope additionally (same transaction):

```rust
            sqlx::query(
                "INSERT INTO approval_rules \
                 (id, repository, kind, pattern, created_from_approval, created_by, created_at) \
                 VALUES (?, ?, 'command-prefix', ?, ?, ?, ?)",
            )
            // repository = the run's repository, resolved via the runs row —
            // add it to the resolve-time SELECT (see gotcha 5).
```

The `Risk.reasons` surfaced to clients on an auto-approval keep working unchanged; the new `resolved_by` strings make pattern hits distinguishable in the ledger.

### 5.4 `crates/daemon/src/policy/mod.rs` — argument scanning in `eval_command`

After the allow-list check and git special-cases, before the final `require(...)`:

```rust
        // Adoption 07: scan path-like arguments of file-manipulating commands
        // against the run's path scopes. Strictly narrowing: an in-scope path
        // changes nothing; a denied path denies; an out-of-scope path forces a
        // fresh, non-reusable approval that NAMES the external directories.
        if let Some(hit) = scan_command_paths(program, args, self, ctx) {
            match hit {
                PathScanHit::Denied(path) => {
                    return self.deny(PolicyReason::new(
                        "policy.command-path-denied",
                        format!(
                            "`{program}` targets `{path}`, which is on the filesystem deny list",
                        ),
                    ));
                }
                PathScanHit::External(dirs) => {
                    return self.require_once(
                        Capability::CommandExecute(scope),
                        PolicyReason::new(
                            "policy.command-touches-external-paths",
                            format!(
                                "`{program}` touches paths outside the granted roots: {}",
                                dirs.join(", ")
                            ),
                        ),
                    );
                }
            }
        }
```

```rust
/// File-manipulating commands whose arguments are worth resolving as paths —
/// the opencode FILES set minus shells/PowerShell (no such invocations here).
const PATH_SCANNED_PROGRAMS: &[&str] = &[
    "cd", "rm", "cp", "mv", "mkdir", "rmdir", "touch", "chmod", "chown",
    "cat", "head", "tail", "ln",
];

enum PathScanHit {
    Denied(String),
    External(Vec<String>),
}

/// Port of shell.ts `argPath`/`pathArgs`/`prefix`, simplified for structured
/// args: skip `-`-flags (and chmod's `+x` modes); expand a leading `~`; trim a
/// glob tail to its literal prefix (a leading glob char ⇒ skip the arg);
/// resolve against `ctx.worktree`; classify with the union of the read and
/// write scopes (deny list included via the scopes themselves).
fn scan_command_paths(
    program: &str,
    args: &[String],
    engine: &PolicyEngine,
    ctx: &EvalContext,
) -> Option<PathScanHit> {
    if !PATH_SCANNED_PROGRAMS.contains(&program) {
        return None;
    }
    let read = engine.file_read_scope(ctx);
    let write = engine.file_write_scope(ctx);
    let mut external: std::collections::BTreeSet<String> = Default::default();
    for arg in args {
        if arg.starts_with('-') || (program == "chmod" && arg.starts_with('+')) {
            continue;
        }
        let Some(text) = glob_literal_prefix(arg) else { continue };
        let expanded = expand_home(text); // `~`/`$HOME` only — the same names
                                          // expand_roots already honors
        let resolved = ctx.worktree.join(expanded); // absolute paths win join()
        match (read.classify(&resolved), write.classify(&resolved)) {
            (ScopeVerdict::Denied, _) | (_, ScopeVerdict::Denied) => {
                return Some(PathScanHit::Denied(resolved.display().to_string()));
            }
            (ScopeVerdict::OutsideRoots, ScopeVerdict::OutsideRoots) => {
                let dir = if resolved.is_dir() { resolved.clone() }
                          else { resolved.parent().map(Path::to_path_buf).unwrap_or(resolved.clone()) };
                external.insert(dir.display().to_string());
            }
            _ => {}
        }
    }
    (!external.is_empty()).then(|| PathScanHit::External(external.into_iter().collect()))
}
```

(`glob_literal_prefix` ports shell.ts `prefix()`: first of `?*[` — at index 0 ⇒ `None`, else the slice before it; no match ⇒ the whole arg.)

### 5.5 `crates/runtime/src/agent.rs`

- `ApprovalRequest` gains `pub repository: Option<String>`; `run_tool`'s `RequireApproval` arm fills it with the run's canonical repository (the same value `eval_ctx(run)` carries as `EvalContext.repository`, rendered `to_string_lossy`).
- No other loop change: `Deny` still short-circuits before the broker, so scanning/pattern interplay is inherited for free.

### 5.6 `crates/codypendentd/src/executor.rs`

`run_journal`'s approval closure passes `req.repository` through to `request_with_reuse` (one added argument).

### 5.7 TUI — `crates/tui/src/input.rs`, `state.rs`, `reduce.rs`, `render.rs`

- `state.rs`: `PendingApproval` gains `pub learnable_pattern: Option<String>` — computed client-side by a re-export of the pure `command_pattern` logic? **No** — the TUI speaks only protocol types (its Cargo.toml rule). Instead the daemon computes it: `EventBody::ApprovalRequested` gains an additive field
  `#[serde(default, skip_serializing_if = "Option::is_none")] pub pattern: Option<String>` — the rule a Pattern/Repository resolution *would* create, stamped by the broker at request time. Old clients ignore it; old events deserialize with `None`.
- `input.rs::map_approval_key`:

```rust
        KeyCode::Char('p') => Action::Approve(ApprovalScope::Pattern),
        KeyCode::Char('P') => Action::Approve(ApprovalScope::Repository),
```

- `reduce.rs::resolve_focused`: unchanged (scope passes through). Guard: if the focused approval's `pattern` is `None`, `p`/`P` are no-ops (the card shows why).
- `render.rs` approval card, for `ExecuteCommand` actions: below the existing risk lines, when `pattern` is `Some`:
  `p  always allow  git checkout *  (this run)`
  `P  always allow  git checkout *  (persist for this repository)`
  and when `None`: `always-allow unavailable for this command (interpreter, pinned path, or custom environment)`. **The literal rule string must be shown** — that is the opencode taste this port exists to keep.
- Footer key hints for `InputMode::Approval` updated accordingly.

### 5.8 CLI — `crates/cli/src/main.rs` + `commands.rs`

```
codypendent approvals rules            # list active + revoked repository rules
codypendent approvals rules revoke <id>
```

List renders `id · repository · pattern · created_by · created_at · (revoked)`. Revoke stamps `revoked_at`/`revoked_by` (never deletes). Both go straight at the SQLite pool the way `skill_add` uses `RuntimePaths` (`crates/cli/src/commands.rs::skill_add` is the shape to copy).

### 5.9 `Cargo.toml`

No additions anywhere. (Decision 1: the port deliberately needs no lexer crate.)

## 6. Protocol & persistence

- **Wire**: `EventBody::ApprovalRequested` gains the additive optional `pattern` field (§5.7). `ApprovalScope::Pattern`/`::Repository` change from reserved to meaningful — no serde change (they already round-trip). `ApprovalRequest` (runtime-internal, not wire) gains `repository`.
- **Persistence**: migration `0038_approval_patterns.sql` — `approvals.pattern` column + `approval_rules` table (§5.2). Append-only; no existing row is rewritten.
- **Ledger**: no new event kinds. Auto-approvals keep emitting `ApprovalRequested` + `ApprovalResolved{Approve}` with `Actor::System`; the authority is recorded in `approvals.resolved_by` (`auto:run-scope` | `auto:pattern:<pattern>` | `auto:repo-rule:<id>`).
- **Reason codes** (stable, machine-branchable): `policy.command-path-denied`, `policy.command-touches-external-paths`; approval error `approval.pattern-unavailable` (surfaced when a Pattern/Repository resolve targets an unlearnable action).

## 7. Acceptance criteria

Numbered RULES first — each MUST hold and each has a test in §8:

1. A learned pattern is consulted **only after** the policy engine freshly returns `RequireApproval` with `approval_reusable: true` for the candidate action. `Deny` outcomes never reach pattern matching (structural: the broker is not called), and `AlwaysApproval` dispositions never match (`allow_run_reuse = false`).
2. Pattern learning and pattern matching both refuse actions with a non-empty `environment`.
3. Pattern learning and matching both refuse interpreter/multiplexer programs (`UNLEARNABLE_PROGRAMS`) and path-bearing programs.
4. `Pattern`-scoped rules die with the run; `Repository` rules apply only to their exact canonical repository and only while `revoked_at IS NULL`.
5. A Pattern/Repository resolve of an unlearnable action fails legibly (`approval.pattern-unavailable`); it is never silently treated as `Once`.
6. Path scanning only ever narrows: it can produce `Deny` or a stricter `require_once`, never an `Allow`.

Checkable outcomes:

7. `command_prefix` reproduces the reference examples: `["touch","foo.txt"] → ["touch"]`, `["git","checkout","main"] → ["git","checkout"]`, `["npm","run","dev"] → ["npm","run","dev"]`, `["python","script.py"] → ["python"]` (unknown-arity conservative default is program-only; the *learnability* refusal for python is separate).
8. Resolving `git checkout main` with scope `Pattern` then requesting `git checkout feature/x` in the same run auto-approves (`resolved_by = "auto:pattern:git checkout *"`, both ledger events present); requesting `git push origin main` still parks.
9. Resolving with scope `Repository` inserts an `approval_rules` row; a **new run** in the same repository auto-approves `git checkout other`; a run in a different repository parks; after `codypendent approvals rules revoke <id>`, the next request parks.
10. `cat ../../../etc/passwd` (program allow-listed, path outside roots) yields `RequireApproval` with reason code `policy.command-touches-external-paths` naming the directory and `approval_reusable == false` — so even after a human approves it, a later identical request parks again (the `require_once` disposition disables both digest and pattern reuse via `allow_run_reuse = false`).
11. `cat $WORKTREE/.git/config`-style deny-listed target (e.g. `~/.ssh/id_rsa` under the default deny list) yields `Deny` with `policy.command-path-denied`, and no approval row is created.
12. The TUI card for a learnable `ExecuteCommand` shows the literal rule for both `p` and `P`; for `sh -c …` it shows the unavailability line and `p`/`P` do nothing.
13. Existing behavior unchanged: the `run_scoped_resolution_auto_approves_identical_proposal` test still passes verbatim; `A` (Run scope) still requires byte-identical repeats.
14. RUN `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace` — EXPECT green.

## 8. Tests

- `crates/daemon/src/policy/arity.rs`:
  `dictionary_is_sorted_for_binary_search`, `prefix_matches_reference_examples` (criterion 7),
  `longest_prefix_wins` (`git stash pop → git stash`), `unknown_program_slices_one`,
  `pattern_refuses_interpreters_paths_and_env` (RULES 2–3),
  `pattern_matches_is_token_exact` (`"git checkout *"` matches `git checkout -b x`, does not match `git checkout` alone? — it does: `have.len() >= want.len()` with have==want ⇒ prefix with no tail still matches, assert that explicitly; and `"git checkout *"` never matches `git checkoutfoo`).
- `crates/daemon/src/approvals.rs` (extend the existing suite with the same `seed_session_run` fixture):
  `pattern_scoped_resolution_auto_approves_prefix_match` (criterion 8),
  `pattern_never_matches_env_carrying_candidate` (RULE 2, candidate side),
  `always_approval_disposition_skips_pattern_reuse` (RULE 1, via `allow_run_reuse=false`),
  `repository_rule_persists_across_runs_and_respects_revocation` (criterion 9),
  `pattern_resolve_of_unlearnable_action_fails` (RULE 5),
  `pattern_column_is_stamped_at_resolve_time`.
- `crates/daemon/src/policy/mod.rs`:
  `scanned_external_path_forces_fresh_approval` (criterion 10),
  `scanned_denied_path_denies` (criterion 11),
  `in_scope_paths_change_nothing`, `flags_and_glob_leading_args_are_skipped`,
  `scanning_applies_only_to_path_scanned_programs` (`cargo test target/` untouched).
- `crates/tui/src/reduce.rs` + `input.rs`:
  `p_and_shift_p_emit_pattern_and_repository_scopes`,
  `pattern_keys_are_noops_without_a_learnable_pattern`,
  render snapshot: the card shows the literal `git checkout *` line.
- `crates/runtime` (agent.rs): `approval_request_carries_the_run_repository`.

## 9. Gotchas

1. **The reference slices RAW tokens** — `prefix()` in arity.ts does not skip flags at match time (flags are excluded only from the dictionary's *counts*). `git -C /x checkout main` therefore learns `git -C *` — ugly but faithful, and strictly narrower than learning `git checkout *` from a flag-bearing call. Do not "fix" this by skipping flags: `npm run --silent dev` would then learn `npm run dev *`… from tokens the user never reviewed in that order.
2. **Check learnability on BOTH sides.** Matching a stored `git checkout *` against a candidate must re-run `command_pattern` on the candidate: a candidate with `environment` set or an absolute-path program must never be covered, even though `pattern_matches` alone would say yes. §5.3's `pattern_match` does this first — keep it first.
3. **`resolved_by` strings are audit surface** — `auto:run-scope` already exists and the TUI/ledger consumers treat `resolved_by` as opaque text; keep the new values prefix-stable (`auto:pattern:`, `auto:repo-rule:`) so log grepping works.
4. **`approvals.pattern` is computed server-side at resolve time** from the stored `action_json` — never accept a pattern from the wire, or a client could learn a broader rule than the card displayed.
5. **`resolve_in_tx` doesn't currently know the repository** — its SELECT joins `runs` for `session_id` only. The `Repository`-scope insert needs the run's repository; extend that same SELECT rather than issuing a second read outside the transaction. If the runs row has no repository recorded (older rows), fail the `Repository` resolve legibly instead of inserting a rule with an empty key that would match nothing (or worse, everything).
6. **Canonicalize the repository key once** — `approval_rules.repository` must be the same canonical string on write (resolve) and read (request). Use the same canonical root the runtime passes in `ApprovalRequest.repository`; do not `fs::canonicalize` twice in places that may disagree about symlinks.
7. **`cd` is in `PATH_SCANNED_PROGRAMS` but produces no learnable value** — opencode's `CWD` set excludes `cd` from shell *patterns* while still scanning its path. Here `cd` isn't even a real command for a no-shell executor, but models propose it; scanning it still yields the external-path warning while the arity dictionary (`cd: 1`) would happily learn `cd *`. RULE 3's interpreter list doesn't cover it — rely on the fact that `cd` is not on the program allow-list (it is a shell builtin, `which cd` fails), so it deny-short-circuits before any learning. Add a test pinning that assumption.
8. **Windows paths**: `command_pattern` rejects `\` in programs, and the path scan resolves with `Path::join` (absolute wins) — no cygpath/`$env:` handling; this port is Unix-shaped like the rest of the executor (Seatbelt/bwrap). Don't cargo-cult the PowerShell branches of shell.ts.
9. **Auto-resolve of other pendings is deliberately dropped** — opencode sweeps its pending map when a new rule lands; codypendent's loop parks one approval per run, so there is nothing to sweep. If parallel tools ever land, revisit (§10).
10. **The additive `ApprovalRequested.pattern` field must be `#[serde(default)]`** — replayed ledgers contain the old shape; a non-defaulted field breaks catch-up on every pre-existing session.

## 10. Out of scope

- Learning for any action other than `ExecuteCommand` (no path patterns, no network patterns — the policy file remains the only authority there).
- A user-editable arity dictionary (config overlay) — the constant is code; extend it by PR.
- opencode's separate `external_directory` permission vocabulary and its "always" globs — external paths surface as stricter command approvals here.
- Auto-resolving other pending approvals when a rule lands (single-parked-approval loop; see gotcha 9).
- Persisted rules at user/global scope (`~` rules that apply across repositories) — repository scope only in v1.
- Codex-style network rules (`blocking_append_network_rule`) — no per-host learned network rules; `[network] allow` in policy.toml is the only widening path.
- Shell-string parsing of any kind (tree-sitter, `shell-words`) — see §4 Decision 1.

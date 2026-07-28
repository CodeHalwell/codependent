# Wire up policy files — design

Branch: `claude/policy-files` (from `main` @ 1c31625)
Status: draft for review — security-sensitive; the trust model (§Decision 1) must be confirmed before planning.

## Problem

`PolicyEngine::load` exists and is fully implemented, but it has **zero non-test callers**. The executor always builds the engine from the built-in defaults:

```rust
// crates/codypendentd/src/executor.rs:465-469
let policy = if self.github.is_some() {
    PolicyEngine::with_defaults_allowing_network([GITHUB_API_ENDPOINT.to_string()])
} else {
    PolicyEngine::with_defaults()
};
```

So a user who writes `~/…/codypendent/policy.toml` or a repo-local `.codypendent/policy.toml` to adjust tool/path/command permissions gets a **silent no-op**: a documented config surface (`docs/specs/policy.toml`, parsed and validated by `RawPolicy`) that changes nothing. That is an honesty gap — the config *looks* live but is dead.

**Concrete pain (the reason this matters):** the built-in shell allow-list is `cargo git rg rustfmt` plus a curated read-only set (`ls cat head … uniq`). A user running local Python repos finds `repository.test` / `shell.run` deny `pytest` with `policy.program-not-allowlisted`, and there is no way to add it — because the only path that could add it (a policy file) is never read. Wiring policy files must let that user **widen** their own allow-list to include `pytest`.

Two latent defects compound the wiring (prior review item 13 + D3):

- **(a) The merge can only narrow.** `MergedPolicy::apply_overlay` is narrow-only for every security field: `shell_allowed_programs` uses `intersect_exact` (test `shell_allow_list_only_narrows` proves adding `npm` is dropped), roots `intersect_roots`, network `intersect_exact`, git `more_restrictive`. Even if `load` were wired today, **no file could ever add `pytest`** — the union is never taken. Widening requires a new, trust-gated merge path.
- **(b) D3 — merge-time containment runs on raw, un-collapsed strings.** `intersect_roots` → `raw_within` compares *unexpanded* strings with `Path::starts_with` and never collapses `..`. `raw_within("$WORKTREE/../etc", "$WORKTREE")` is **true** (components `$WORKTREE`, `..`, `etc` start with `$WORKTREE`), so `$WORKTREE/../etc` survives the "narrower-than-worktree" intersection — then at eval time `build_path_scope` → `canonicalize_lenient` collapses the `..` and the effective allowed root becomes `<parent-of-worktree>/etc`, **outside** the worktree. Eval-time `PathScope::classify` already canonicalizes candidate paths correctly; the hole is purely in the *merge-time* string check that decides which roots are kept.

## Goals

- Wire `PolicyEngine::load` into `RuntimeExecutor::execute` so policy files actually take effect.
- Let a **trusted** source widen the shell allow-list (so the user can add `pytest`), read scope, network allow-list, and relax approval dispositions.
- Keep an **untrusted** source (a repo the agent may be reviewing) unable to grant the agent *any* additional power — narrow-only, exactly as `apply_overlay` is today.
- Fix D3: merge-time root containment must classify path-normalized (`..`/`.` collapsed) strings, so a `..`-escaping root can never survive an intersection.
- Malformed / unknown-key policy → a legible error surfaced to the user; never a silent ignore and never a silent widen.

## Non-goals

- No new protocol / wire messages. Policy is daemon-internal config (see §Protocol).
- No enforcement of the parsed-but-unread sections (`scope`, `data`, `plugins`, `memory`) — they stay validated-only, as today.
- No interactive "trust this repo?" prompt (that is Option B, rejected below). No per-run policy override via the client.
- No change to the eval-time capability model, `ModeOverlay`, or the approval gate.

## Verified code map (confirmed against branch)

- `crates/daemon/src/policy/mod.rs:207-223` — `PolicyEngine::load(repo_policy: Option<&Path>, config_policy: Option<&Path>)`. Applies `config_policy` then `repo_policy` over `builtin_defaults`, **both via `apply_overlay`** (both narrow-only today). Takes explicit paths for testability. Zero non-test callers.
- `crates/daemon/src/policy/mod.rs:182-198` — `with_defaults` / `with_defaults_allowing_network`.
- `crates/daemon/src/policy/mod.rs:537-602` — `build_path_scope` / `expand_vars`: eval-time expansion + `canonicalize_lenient` of every root/deny. **Correct** — D3 is not here.
- `crates/daemon/src/policy/config.rs:83-200` — `MergedPolicy::builtin_defaults` (the shell allow-list lives at 100-127) + `apply_overlay` (narrow-only merge).
- `crates/daemon/src/policy/config.rs:208-255` — `intersect_roots`, `intersect_exact`, `union_in_place`, **`raw_within` (the D3 bug — no `..` collapse)**.
- `crates/daemon/src/policy/scope.rs:126-186` — `CommandScope::allows_program` (exact-match, never basename), `canonicalize_lenient`, `is_within` (eval-time, component-wise, correct).
- `crates/codypendentd/src/executor.rs:465-478` — the wiring seam. `launch.repository` is the run's repository root; `self.paths: RuntimePaths` carries `data_dir`. `execute` returns `Result<_, String>`, so a load error maps to a legible run-failure string.
- `crates/protocol/src/discovery.rs:44-113` — `RuntimePaths { data_dir, … }`. Built from `directories::ProjectDirs::from("", "", "codypendent")`. Exposes `data_dir` only — **no `config_dir` accessor yet** (see Decision 3).
- `docs/specs/policy.toml` — the documented sample; every section already parses via `RawPolicy` (`deny_unknown_fields` throughout).

## Decision 1 (THE key security decision) — trust model: global widens, repo-local narrows-only

**Confirm this first.** A `.codypendent/policy.toml` that lives *inside* a repository can be attacker-controlled: the agent's read-only modes (`Explore`/`Review`) operate on repositories it may not trust, and a malicious repo could ship a `policy.toml` that allow-lists `bash`/`curl`, widens `fs_write` to `$HOME`, or flips `network.default = "allow"`. If a repo-local file could widen, cloning a hostile repo would hand the agent more power **before a human sees a line of it**.

Options considered:

- **Option A (recommended, chosen): trust follows origin.** A **global** user config (in the user's own config dir — their machine, their file) is *trusted* and may **widen or narrow**. A **repo-local** `.codypendent/policy.toml` is *untrusted* and may **only narrow** — never add a program, root, endpoint, or relax an approval. An untrusted repo therefore can only ever *reduce* the agent's authority, never grant it.
- **Option B: repo-local may widen behind a one-time trust prompt.** Rejected for now: it needs a client-facing trust-store + UI + a wire message (violates the no-protocol-change goal), and it invites blind "yes" fatigue. Can be layered on later without reworking Option A.
- **Option C: repo-local widens freely.** Rejected — directly insecure for the core use case (reviewing untrusted code).

**Why A is right for this codebase:** it maps trust to *who authored the file*, which is exactly the property that matters. The user's `pytest` need is satisfied entirely by the global config (their own machine). Nothing about A blocks a repo from *tightening* itself (a locked-down repo can still forbid `git push`), which is the legitimate repo-local use case.

The merge order becomes: `builtin_defaults` → **trusted global overlay (widen-or-narrow)** → **untrusted repo overlay (narrow-only)**. Because the repo layer is applied last and can only narrow, the repo can always claw back anything the global widened, but never exceed it.

## Decision 2 (confirm) — how far may a trusted global widen? (write scope)

The `pytest` use case needs only **shell allow-list** widening. But design item 3 asks to "widen/adjust path scopes", and the constraint names "worktree confinement" as a floor. These pull in different directions, so pick a lane:

- **2a (recommended): trusted global may widen `shell`, `network`, `fs_read`, and relax `git`/approval dispositions — but `fs_write` roots stay narrow-only even from the global config.** Rationale: writes are the highest-blast-radius capability; the worktree-confinement floor (`fs_write = $WORKTREE`) is the single invariant that guarantees a Build run cannot scribble outside its isolated worktree. Keeping write-scope narrow-only removes an entire class of "my own config accidentally let the agent write my home dir" footguns while fully solving the stated problem. `fs_read` widening is low-risk and occasionally useful (read a sibling vendored dir).
- **2b: trusted global may also widen `fs_write`.** More flexible, strictly more dangerous; defensible because it is the user's own trusted file, but it is not needed for the `pytest` goal.

This spec designs **2a**. If the reviewer wants 2b, only the `fs_write` branch of `apply_trusted_overlay` changes (union instead of intersect) — the structure is identical.

Independent of 2a/2b: **the approval gate is the real floor, and it is untouched.** `eval_command` returns `RequireApproval` for *every* allow-listed program — widening the allow-list can only move a program from `Deny` to `RequireApproval`, **never to auto-run**. So even a global config that adds `python`/`bash` cannot make the agent execute them without a human approval. The "no interpreters auto-run" floor holds structurally, not by allow-list curation.

## Design

### 1. Split the merge into a trusted and an untrusted path

Replace the single `apply_overlay` with two entry points in `config.rs` (keep the field-by-field structure):

- `apply_untrusted_overlay(&mut self, raw)` — **exactly today's `apply_overlay`**, unchanged (narrow-only): `intersect_exact` for shell/network, `intersect_roots` for `fs_read`/`fs_write`, `union` for `fs_deny`, `more_restrictive` for git, ratchet for `network.default`. Rename-in-place; the existing tests carry over verbatim.
- `apply_trusted_overlay(&mut self, raw)` — widen-or-narrow:

  | Field | Trusted merge | Direction |
  |---|---|---|
  | `shell_allowed_programs` | `union_in_place(&mut self.shell, raw.allowed)` | **widen** (adds `pytest`) |
  | `network_allow` | `union_in_place` | **widen** |
  | `network_default` | overlay value **replaces** (may set `Allow`) | either |
  | `fs_read` | `union_roots` (normalized, D3-safe) | **widen** |
  | `fs_write` | `intersect_roots` (narrow-only — Decision 2a) | narrow |
  | `fs_deny` | `union_in_place` | widen the *denials* (still tightening) |
  | `git_*` | overlay value **replaces** (may relax to `Allow`) | either |
  | `shell_interpreter_requires_approval` | overlay value **replaces** | either |
  | `shell_maximum_seconds` | overlay value **replaces** | either |
  | `schema_version` | replaces (as today) | n/a |

  A field the overlay omits (`None`) is left untouched, as today. `union_roots` is a new helper: append normalized overlay roots not already present (the widening dual of `intersect_roots`), passing each through the D3 normalizer below.

`builtin_defaults`, `PathScope`, `CommandScope`, the eval-time code, and `ModeOverlay` are **unchanged**.

### 2. `PolicyEngine::load` — carry the trust distinction

Change `load` so the two layers apply through the two paths:

```rust
pub fn load(
    repo_policy: Option<&Path>,     // untrusted → apply_untrusted_overlay
    global_policy: Option<&Path>,   // trusted   → apply_trusted_overlay
) -> Result<Self, PolicyLoadError> {
    let mut merged = MergedPolicy::builtin_defaults();
    if let Some(p) = global_policy {
        if let Some(raw) = config::load_layer(p)? { merged.apply_trusted_overlay(&raw); }
    }
    if let Some(p) = repo_policy {
        if let Some(raw) = config::load_layer(p)? { merged.apply_untrusted_overlay(&raw); }
    }
    Ok(Self::from_merged(merged))
}
```

Signature/argument *names* change (trust is now explicit) but the shape — two `Option<&Path>`, explicit paths for testability, `Ok(None)` on a missing file, `Err` on malformed — is preserved. The `config_policy` parameter is renamed `global_policy` to make the trust boundary legible at the call site.

**GitHub endpoint admission.** Today `with_defaults_allowing_network([GITHUB_API_ENDPOINT])` folds the endpoint into `network_allow`. After wiring, keep that fold *after* the merge so it composes with a loaded policy. Cleanest seam: a small post-load method `PolicyEngine::admitting_network(self, endpoints)` (or have the executor build the `MergedPolicy` and add endpoints before `from_merged`). Either keeps the invariant that admitting the endpoint grants nothing on its own — every GitHub write still returns `RequireApproval`.

### 3. D3 fix — normalize before merge-time containment

Add a lexical normalizer used by `intersect_roots`, the new `union_roots`, and `raw_within` in `config.rs`:

```rust
/// Collapse `.` and `..` in a raw (unexpanded) policy path string, treating a
/// leading `$REPOSITORY` / `$WORKTREE` / `$HOME` as an opaque, immovable
/// component. Returns `None` if `..` would pop above that leading anchor
/// (an escape) — such a root is DROPPED from allow lists (fail-closed) and,
/// for the trusted widen path, logged.
fn normalize_raw(raw: &str) -> Option<String>;
```

- `intersect_roots` / `union_roots` normalize both operands before `raw_within`; a root that normalizes to `None` (escapes its anchor) is dropped. So `$WORKTREE/../etc` no longer counts as "within `$WORKTREE`" and never becomes an effective root.
- This is *merge-time* defence-in-depth. Eval-time `PathScope::classify` already canonicalizes candidate paths correctly (verified) — D3 is closed at the layer where the effective root set is *chosen*, before it ever reaches eval.
- We deliberately do **not** filesystem-canonicalize at merge time: strings are still unexpanded (`$WORKTREE` is unknown until `EvalContext`), so a lexical collapse with opaque anchors is the correct and only sound operation here. The real filesystem/symlink resolution stays at eval time where the context exists.

### 4. Executor wiring (the seam at executor.rs:465)

Replace the `with_defaults` branch with a `load` call:

```rust
let repo_policy   = launch.repository.join(".codypendent").join("policy.toml"); // untrusted
let global_policy = self.paths.global_policy_path();                            // trusted
let mut policy = PolicyEngine::load(Some(&repo_policy), Some(&global_policy))
    .map_err(|e| format!("policy configuration error: {e}"))?; // legible; never silent
if self.github.is_some() {
    policy = policy.admitting_network([GITHUB_API_ENDPOINT.to_string()]);
}
```

- **Repo-local path:** `<launch.repository>/.codypendent/policy.toml` — the run's repository root (untrusted).
- **Global path:** see Decision 3. Missing files are `Ok(None)` (skipped), so a user with no policy files behaves exactly as today (built-in defaults).

### Decision 3 (confirm) — where the trusted global file lives

`RuntimePaths` currently exposes only `data_dir` (from `ProjectDirs::data_dir()`). Two options:

- **3a (recommended): add `RuntimePaths::global_policy_path()` returning `<config_dir>/policy.toml`, where `config_dir = ProjectDirs::from("", "", "codypendent").config_dir()`.** Matches the `config.rs` module doc ("`<config_dir>/codypendent/policy.toml`, the User layer"), matches the XDG convention (config, not data, holds user config), and is honest about intent. One-time cost: thread a `config_dir` field through `RuntimePaths::with_socket` / `from_data_dir`, honoring a `CODYPENDENT_CONFIG_DIR` override for tests (mirrors the existing `CODYPENDENT_DATA_DIR`).
- **3b: put it under the existing `data_dir` (`<data_dir>/policy.toml`).** Zero new plumbing, but conflates user-editable config with daemon runtime state and drifts from the documented location.

Design **3a**.

### 5. Honesty / errors

- **Malformed or unknown-key policy (either layer):** `load` already returns `PolicyLoadError::{Read,Parse}`. The executor maps it to a legible run-failure string (`"policy configuration error: failed to parse policy file <path>: <toml error>"`) — the run does **not** start. Rationale: a narrowing layer that fails to parse must **not** fall back to defaults, because defaults are *weaker* than the author intended (fail-open on a security narrowing is the exact class of bug we are closing). Failing the run legibly is the only fail-closed option.

  Decision 4 (minor, confirm): alternative is warn-and-fall-back-to-defaults for the *global* (trusted, widening) file only — a malformed global just means "no widening", which is safe. Recommended: still fail legibly for both, for one predictable rule and to surface the user's typo instead of silently ignoring their `pytest` line. A silently-ignored malformed global would reproduce the original honesty bug in miniature.

- **Never a silent widen from an untrusted source:** structurally guaranteed — the repo layer only ever goes through `apply_untrusted_overlay`, which has no widening branch.

## Data flow (after wiring)

```
RuntimeExecutor::execute
  └─ PolicyEngine::load(Some(repo/.codypendent/policy.toml),   // untrusted, narrow-only
                        Some(config_dir/policy.toml))          // trusted,   widen-or-narrow
       ├─ builtin_defaults()
       ├─ apply_trusted_overlay(global)   → union shell/read/network, replace git/default, intersect write
       └─ apply_untrusted_overlay(repo)   → intersect/ratchet only (can claw back, never grant)
     → .admitting_network([GITHUB_API_ENDPOINT]) if github configured
     → FrameworkAgentRuntime::new(policy, …)   // unchanged downstream
```

## Protocol

**No wire change.** Policy files are read daemon-side; `MergedPolicy` already serializes only to derive the internal `PolicyVersion` hash. `PolicyDecision` (with its `policy_version`) already crosses the wire and its *schema* is unchanged — only the *value* of the hash differs when a policy file is present, which existing clients already treat as an opaque string. Flag: none.

## Testing

Unit (`config.rs`):
- **Widen:** `apply_trusted_overlay` with `[shell] allowed_programs = ["pytest"]` → merged allow-list **contains** `pytest` **and** retains the built-in set (union, not replace).
- **Narrow-only untrusted:** `apply_untrusted_overlay` with the same input → `pytest` **absent** (regression-guards the existing `shell_allow_list_only_narrows`).
- **Trust source:** `load(Some(repo_with_pytest), None)` → no `pytest`; `load(None, Some(global_with_pytest))` → `pytest` present. Proves origin, not content, decides.
- **D3:** `intersect_roots`/`union_roots` drop `$WORKTREE/../etc` (normalizes to escape → `None`); a healthy `$WORKTREE/src` survives. Property test: every merged root, once normalized, is anchored at its variable and contains no residual `..`.
- **Network / git widen:** trusted overlay may set `network.default = "allow"` and `git.commit = "allow"`; untrusted overlay of the same is a no-op (ratchet holds).
- **fs_write floor (Decision 2a):** trusted overlay listing `$HOME` in `fs_write` does **not** widen the write scope.

Unit (`mod.rs`):
- **End-to-end widen:** engine from `load(None, Some(global_with_pytest))` → `evaluate(ExecuteCommand{program:"pytest"})` returns `RequireApproval` (not `Deny`) — and `python` still `Deny`s if not listed; an *added* `python` returns `RequireApproval`, never auto-run.
- **Malformed:** `load(Some(bad_toml), None)` and `load(None, Some(bad_toml))` both return `Err(PolicyLoadError::Parse)`; unknown-key too.
- **Missing files:** `load(Some(nonexistent), Some(nonexistent))` == `with_defaults()` (same `policy_version`).

Executor:
- The `execute` seam maps a `PolicyLoadError` to a legible run-failure string and does not start the run.

## Constraints (satisfied)

- **Untrusted repo-local can never widen** — enforced structurally (repo layer → `apply_untrusted_overlay`, no widen branch). ✔
- **Path containment classifies normalized paths (D3)** — `normalize_raw` collapses `..`/`.` before merge-time containment; eval-time canonicalization unchanged. ✔
- **Malformed fails legibly, no silent no-op** — `PolicyLoadError` surfaced as a run-failure string; no fall-back-to-defaults for a narrowing layer. ✔
- **Read-only shell floor + core safety** — approval gate untouched: every allow-listed program still `RequireApproval` (no auto-run); worktree confinement kept as a floor (Decision 2a keeps `fs_write` narrow-only). ✔
- **No protocol/wire change** — daemon-internal config. ✔
- **Testable** — widen/narrow, trust-source, canonicalization, malformed all have unit seams. ✔

## Components (seed the plan's tasks)

1. **`config.rs` — split the merge.** Rename `apply_overlay` → `apply_untrusted_overlay` (behavior unchanged); add `apply_trusted_overlay` + `union_roots` helper. Port existing merge tests; add widen tests.
2. **`config.rs` — D3 normalizer.** Add `normalize_raw`; route `intersect_roots`, `union_roots`, `raw_within` through it; add escape-drop + property tests.
3. **`mod.rs` — `PolicyEngine::load` trust wiring + `admitting_network`.** Rename `config_policy` → `global_policy`; apply through the two paths; add the post-load network-admission method; add load/trust/malformed/end-to-end tests.
4. **`discovery.rs` — `RuntimePaths::global_policy_path()`** (Decision 3a): thread a `config_dir` (with `CODYPENDENT_CONFIG_DIR` override); accessor returns `<config_dir>/policy.toml`.
5. **`executor.rs` — wire the seam** (~465): build both paths, call `load`, map error to a legible run failure, re-admit the GitHub endpoint when configured.
6. **Docs follow-up (out of this spec's commit):** note the two policy-file locations + the trust model in the user guide / `docs/specs/policy.toml` header. Not touched here.

## Open decisions for the reviewer

1. **Trust model (Decision 1) — the #1 confirm.** Global config widens-or-narrows; repo-local narrows-only. Confirm A over B/C.
2. **Write-scope widening (Decision 2).** Recommended **2a**: even a trusted global keeps `fs_write` narrow-only (worktree-confinement floor); widening covers `shell`/`network`/`fs_read`/`git` only — which fully solves the `pytest` need. Confirm, or opt into 2b.
3. **Global file location (Decision 3).** `config_dir/policy.toml` (3a, recommended) vs `data_dir/policy.toml` (3b).
4. **Malformed-file behavior (Decision 4).** Fail the run legibly for both layers (recommended) vs warn-and-default for the trusted global only.

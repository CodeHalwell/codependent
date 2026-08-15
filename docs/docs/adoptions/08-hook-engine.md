# Adoption 08 — Hook discovery + execution engine

**Effort:** M · **Depends on:** nothing · **Reference:** reference-repos/cline/sdk/packages/core/src/hooks/{hook-file-config.ts, hook-file-hooks.ts, subprocess-runner.ts}, sdk/packages/shared/src/hooks/{contracts.ts, events.ts}, shared/src/storage/paths.ts (`resolveHooksConfigSearchPaths`); reference-repos/codex/codex-rs/hooks/src/{lib.rs, types.rs} (typed event taxonomy + outcomes), engine/ + output_spill.rs (bounded output)
**Ported from:** cline + codex · **Status:** ⬜ not started

## 1. Summary

`crates/sandbox/src/hook.rs` already ships the hook engine's *decision core* — the `hook.toml` parser, the verdict lattice (`Deny` absorbing, conflicting rewrites deny), and the `Unapproved<ToolCall>` / `PolicyReentry` type wall that makes "a hook cannot escalate privilege" a compile-time property. Its own module doc states, correctly, that **nothing else exists**: no discovery, no registration, no dispatch, no execution. This adoption builds those four missing layers, adopting cline's *operational* conventions — workspace + global hook directories, JSON payload on the subprocess's stdin, a `HOOK_CONTROL`-tagged JSON control line on stdout, hooks disabled on unattended paths — **adapted to the existing `hook.toml`/verdict semantics, never the reverse**: hooks stay declared manifests bound to a closed event set, inert on discovery until a human approves their content hash (migration 0027's rule), executed through the OS sandbox executor, and their rewrites still cannot run without `Unapproved::reenter`.

## 2. Reference implementation

**cline — file conventions** (`sdk/packages/core/src/hooks/hook-file-config.ts`, `shared/src/storage/paths.ts`): hooks are executables named after events (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `PreCompact`, `TaskStart/Resume/Cancel/Complete/Error`, `SessionShutdown`), any of a closed extension set (`""/.sh/.bash/.zsh/.js/.mjs/.cjs/.ts/.mts/.cts/.py/.ps1`), discovered across ordered roots — global (`~/.cline/hooks` + a Documents dir) then workspace (`<ws>/.clinerules/hooks`, `<ws>/.cline/hooks`) — deduped by absolute path, sorted for determinism. `inferHookCommand` (hook-file-hooks.ts ~line 300) resolves the interpreter from shebang or extension.

**cline — subprocess protocol** (`subprocess-runner.ts`): the full event payload is written to the child's **stdin** as one JSON document (with careful EPIPE/EOF-tolerant stdin handling and listeners installed before yielding); stdout and stderr are captured; on close, `parseStdout` takes the **last line starting `HOOK_CONTROL\t`** (so scripts can freely print logs) — or, if none, the whole trimmed stdout — and JSON-parses it. Timeout ⇒ SIGKILL with `timedOut: true`. The control shape (`shared/src/hooks/contracts.ts`):
`HookControl { cancel?, review?, context?, overrideInput?, systemPrompt?, appendMessages?, replaceMessages? }` — `beforeToolResultFromControl` maps `cancel: true → stop` and `overrideInput → replacement input` (hook-file-hooks.ts). Payloads carry `clineVersion`, `hookName`, `timestamp`, `taskId`, `workspaceRoots`, `workspaceInfo` (git branch/commit "so hook scripts … without running git themselves"), and per-event blocks (`preToolUse: {toolName, parameters}`, `postToolUse: {…, result, success, executionTimeMs}`). Async/lifecycle hooks run detached (`detached: true`, stdout ignored); the blocking `tool_call` hook has `timeoutMs: options.toolCallTimeoutMs ?? 120000`.

**cline — the unattended rule**: "Yolo disables hooks ('no side effects unattended')" — every auto-approve path re-derives its safety posture and switches hooks off.

**codex — typed taxonomy and outcomes** (`codex-rs/hooks/src/lib.rs`): eleven event names (`PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop`), nine with matchers; **typed outcome per event** (`PreToolUseOutcome`, `PermissionRequestDecision` — a hook can decide a permission before the UI, `PreCompactOutcome::Stopped` aborts compaction); `HookResult::{Success, FailedContinue, FailedAbort}` distinguishes "this hook failed, keep going" from "abort the operation" (types.rs); stable snake_case wire shapes pinned by serialization tests (`hook_payload_serializes_stable_wire_shape`); `output_spill.rs` spills oversized hook output to disk so a chatty hook cannot blow the transcript. The lesson kept: events are a **closed enum with per-event outcome types**, never stringly-typed — which codypendent's `HookEvent`/`HookVerdict` already are.

## 3. Current state in codypendent (verified)

- **`crates/sandbox/src/hook.rs` (read end-to-end; authoritative):**
  - `HookSpec` parses `hook.toml` (`docs/specs/hook.toml` is the shipped shape) with `deny_unknown_fields`; closed `HookScope` (`user` = `<data_dir>/hooks/`, `repository` = `<repo>/.codypendent/hooks/`, plus `organization`/`system` that "no discoverer produces today"), and `parse_hook(raw, discovered)` **re-derives scope from the discovery site** — a committed file cannot claim a higher tier.
  - Closed `HookEvent` set: `tool.pre`, `tool.post`, `patch.proposed`, `run.start`, `run.end`, `approval.requested`; `is_preventable()` gates which may block.
  - `HookKind::{Observe, Validate, Mutate}`; `mutate` may bind only to `tool.pre`, is high-risk by authority, and **must** declare `[policy] requires_approval = true` (enforced in `parse_hook`).
  - `HookRuntime::Command { program, args, working_directory, timeout_seconds }` — `working_directory` placeholders limited to `$REPOSITORY`/`$WORKTREE`/`$HOME` (`HOOK_PLACEHOLDERS`, token rules matching `skill_exec::substitute_placeholders`); zero timeout refused; `HookNetwork::Deny` is the only network value.
  - `FailurePolicy::{Block, Warn}`; `block` refused on post-hoc events.
  - `validate_event_set` enforces `MAX_HOOKS_PER_EVENT = 32` and unique ids per scope; `dispatch_key() = (priority, id)` gives total order.
  - `ToolCall { name, arguments_json }` with a domain-separated, length-prefixed `digest()`; `combine(verdicts)` implements the lattice; `HookOutcome::{Proceed, Denied{reasons}, Rewritten(Unapproved<ToolCall>)}`; `Unapproved` has **no accessor** (even `Debug` redacts) and `reenter(gate: &impl PolicyReentry, ctx: &ReentryContext)` is the only exit — `Ok(false)` from the gate means "allowed but needs approval", and then only a `ReentryContext.approved_digest` equal to **this** call's digest authorizes. `HookSpec::content_digest` domain-separates the approval-binding hash.
- **`migrations/0027_hooks.sql` exists and is empty at runtime**: `hooks` (identity, scope, event, kind, priority, `source_path`, `content_hash`, `spec_json`, `approval_state` pending|approved|rejected, `approved_content_hash`, unique `(scope_kind, scope_key, hook_id)`, dispatch index) and `hook_dispatches` (per-invocation audit: `subject_digest`, `verdict` observe|allow|deny|rewrite, `applied` allowed|denied|rewrite-reentered|rewrite-refused, `rewrote_action` **digest only**, exit/timeout/duration/output size). No code reads or writes either table.
- **`crates/daemon/src/policy_gate.rs`**: `RunPolicyAdapter` implements `PolicyReentry` over `PolicyEngine::evaluate` + a `ToolCallLowering` (`Arc<dyn Fn(&str, &str) -> Option<ProposedAction>>`) — "an adapter built without one refuses every hook rewrite (`policy.unknown-tool`)". Tests already pin: `Ok(false)` for approval-gated rewrites, refusal without a lowering. **Zero production callers.**
- **`crates/sandbox/src/executor.rs`**: `SandboxExecutor::run(profile, command)` runs a `SandboxCommand { program (absolute), args, cwd, origin }` confined (Seatbelt / bwrap; other platforms fail closed), clean env, wall-clock kill of the process group, output cap, `sanitize_untrusted` on both streams → `SandboxOutcome { exit_code, timed_out, duration, stdout: Sanitized, stderr, output_truncated }`. **Stdin is `Stdio::null()`** (~line 716) — the one mechanical gap for cline's stdin protocol. `prepare_interactive` exists for long-lived stdio processes but skips the capture/sanitize/kill discipline; `crates/knowledge/src/skill_exec.rs` shows the profile-building idiom (`profile_for_permissions`, `PlaceholderContext`, `substitute_placeholders`) and is itself unwired.
- **`crates/runtime/src/agent.rs::run_tool`** is the dispatch seam: `(a) prepare → (b) policy.evaluate → Deny short-circuit / RequireApproval park → (d) execute_prepared`. The runtime depends on both `codypendent-daemon` and `codypendent-sandbox` (Cargo.toml), so it can name `RunPolicyAdapter`, `ToolCall`, `HookOutcome`.
- **Unattended surfaces that must never dispatch hooks**: workflow agent-node runs (`crates/codypendentd/src/workflow_exec.rs`), webhook-triggered runs (`maybe_start_webhook_listener` in `codypendentd/src/lib.rs`), and the sandboxed guest path (`policy_gate.rs`: "a guest runs unattended").
- **Must not break**: every existing test in `hook.rs` and `policy_gate.rs` (the spec adds callers, never edits semantics); migration 0027 stays as-is (no new migration needed); `SandboxExecutor::run`'s fail-closed and sanitize-everything contracts.

## 4. Design

Four layers, strictly on top of what exists:

1. **Discovery** (`crates/daemon/src/hooks.rs`): scan `<data_dir>/hooks/` (⇒ `HookScope::User`) and `<repo>/.codypendent/hooks/` (⇒ `HookScope::Repository`) for `*.toml` files and `*/hook.toml` package directories; `parse_hook` each with the discovered scope; `validate_event_set` per (scope, event); upsert into the `hooks` table. A changed `content_hash` resets `approval_state` to `'pending'` (0027's approve-then-substitute defence). Run at daemon startup and at run launch, exactly where `scan_installed_skills` runs today.
2. **Activation** (CLI): `codypendent hook list|approve|reject` — discovery is not activation; only `approval_state = 'approved' AND approved_content_hash = content_hash` rows dispatch.
3. **Execution** (`crates/daemon/src/hook_exec.rs`): a `HookRunner` that lowers one approved `HookSpec` into a `SandboxProfile` + `SandboxCommand`, writes the **typed JSON payload to stdin** (one new optional field on `SandboxCommand`; the executor keeps every other guarantee), and maps `(exit, timed_out, stdout control line)` → `HookVerdict` per the tables in §5.4. Network is always denied (the profile's empty allowlist — `HookNetwork::Deny` is the only parseable value); output is already capped + sanitized by the executor (codex's output-spill concern is satisfied by the existing cap).
4. **Dispatch** (`HookEngine` in the daemon implementing a new `HookDispatch` seam consumed by `crates/runtime/src/agent.rs::run_tool`): load approved hooks for the event in `dispatch_key` order, run each, `combine`, and act on the `HookOutcome`; a `Rewritten` goes through `RunPolicyAdapter` (`PolicyReentry`) with a **fresh human approval always parked** for the rewritten action (mutate ⇒ `requires_approval = true` is structural, so this is unconditional); every invocation lands one `hook_dispatches` row.

Adaptations of the file conventions **to** hook.rs (the required direction):

- **Manifest, not filename-is-event.** cline's `PreToolUse.sh` convention collapses event binding into a filename; codypendent's closed `HookEvent` + `hook.toml` already carry the binding plus policy/runtime/output declarations, and the scope re-derivation defence only works with a manifest. Kept from cline: the *two-root discovery* and determinism (sorted, deduped).
- **stdin JSON in / control JSON out** is adopted verbatim in mechanism (`HOOK_CONTROL\t` last-line convention included) but the control vocabulary is the **verdict lattice's**, not cline's: `{"decision": "allow"|"deny"|"rewrite", "reason"?, "rewrite"?: {name, arguments_json}}`. cline's `cancel/overrideInput` map 1:1 onto `deny`/`rewrite`; `systemPrompt`/`appendMessages`/`replaceMessages` have **no counterpart on purpose** — a hook that could edit the transcript or system prompt would be a second prompt-injection channel the threat model closes (§10).
- **No interpreter inference.** cline infers `bash`/`node`/`py` from the extension; here `[runtime] program` is explicit, matched by the sandbox as an absolute-path or trusted-PATH program, and the hook's own package directory is granted read so bundled scripts can be its args.
- **Unattended paths disable hooks** (cline's rule, kept): the `HookDispatch` seam is wired only for interactive session runs; workflow nodes, webhook runs, and guests get `None` and behave exactly as today.
- **Sandboxed, not host-spawned** (deviation from both references, deliberate): a hook is repository-committable untrusted code; it runs through `enforcing_executor()` with read+write on the worktree only, subprocess allowed (a `cargo test` verify hook is the shipped example), network denied, wall clock = `timeout_seconds`. cline/codex run hooks unconfined; codypendent must not.
- **Event coverage v1**: `tool.pre`, `tool.post`, `run.start`, `run.end`. `patch.proposed` and `approval.requested` parse today and stay dormant (§10) — an unknown event name is still a parse error, so nothing silently never-fires.

## 5. Changes, file by file

### 5.1 `crates/sandbox/src/executor.rs` — stdin for confined runs

```rust
pub struct SandboxCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub origin: String,
    /// Bytes written to the child's stdin before it is closed, or `None` for
    /// the existing null-stdin behaviour (adoption 08 — the hook payload
    /// protocol). Bounded: larger than [`MAX_STDIN_BYTES`] is refused as
    /// [`SandboxError::InvalidCommand`], never truncated.
    pub stdin: Option<Vec<u8>>,
    runtime_denies_subprocess: bool,
}

/// 1 MiB — a hook payload is metadata, never bulk content.
pub const MAX_STDIN_BYTES: usize = 1024 * 1024;
```

In `run_confined`: when `command.stdin` is `Some`, spawn with `Stdio::piped()`, write the bytes from a dedicated thread (the same pattern as the capped reader threads), tolerate `BrokenPipe` (a child that exits without reading stdin is cline's explicitly-handled case — see subprocess-runner.ts's close-pipe tolerance), and drop the handle to close the stream. Every constructor of `SandboxCommand` in the workspace gains `stdin: None`. Both backends (Seatbelt, bwrap) get the same treatment; the wall-clock kill, output caps, and sanitization are untouched. No `unsafe`.

### 5.2 `crates/daemon/src/hooks.rs` (new) — discovery + registration

```rust
//! Hook discovery and registration (adoption 08): filesystem → `hooks` table.
//! Discovery is NOT activation (migration 0027 rule 1): rows land `pending`
//! and dispatch only after `codypendent hook approve` binds a content hash.

use codypendent_sandbox::hook::{parse_hook, validate_event_set, HookScope, HookSpec};

/// `<data_dir>/hooks/` — the operator's own hooks (the skills `user_skills_root`
/// convention; NOT `~/.codypendent`, which is not a path this product uses).
pub fn user_hooks_root(data_dir: &Path) -> PathBuf { data_dir.join("hooks") }

/// `<repo>/.codypendent/hooks/` — repository-committed, untrusted.
pub fn repository_hooks_root(repository_root: &Path) -> PathBuf {
    repository_root.join(".codypendent").join("hooks")
}

pub struct HookScanOutcome {
    pub registered: Vec<String>,          // hook ids upserted
    pub reset_to_pending: Vec<String>,    // content hash changed
    pub failures: Vec<(PathBuf, String)>, // per-file, never fatal to the scan
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
    scope_key: &str, // "" for user scope; canonical repo root for repository scope
) -> HookScanOutcome;
```

Upsert semantics per parsed spec (all inside one transaction per root):

```sql
INSERT INTO hooks (id, hook_id, name, scope_kind, scope_key, event, kind, priority,
                   source_path, content_hash, spec_json, approval_state, created_at, updated_at)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)
ON CONFLICT (scope_kind, scope_key, hook_id) DO UPDATE SET
    name = excluded.name, event = excluded.event, kind = excluded.kind,
    priority = excluded.priority, source_path = excluded.source_path,
    content_hash = excluded.content_hash, spec_json = excluded.spec_json,
    updated_at = excluded.updated_at,
    -- approve-then-substitute defence: a changed hash falls back to pending.
    approval_state = CASE
        WHEN hooks.approved_content_hash = excluded.content_hash THEN hooks.approval_state
        ELSE 'pending'
    END
```

`content_hash = HookSpec::content_digest(&raw)` (the domain-separated hash hook.rs defines — never a plain file hash). Rows whose `source_path` no longer exists on a rescan of their root are deleted (a removed file must not keep dispatching).

Wire into startup + run launch in `crates/codypendentd/src/lib.rs`, directly beside `scan_installed_skills`:

```rust
    scan_installed_hooks(&pool, &paths.data_dir, &workdir).await; // both roots, warn-per-failure
```

### 5.3 CLI — `crates/cli/src/main.rs` + `commands.rs`

```
codypendent hook list                # id · scope · event · kind · priority · state · source
codypendent hook show <hook_id>      # prints the hook.toml verbatim + its content hash
codypendent hook approve <hook_id>   # approved_content_hash = current content_hash
codypendent hook reject <hook_id>
```

`approve` prints the file content (or its path and hash) before writing — the human approves *bytes*, and the stamp is `approved_content_hash = content_hash, approved_by = <os user>, approved_at = now`. Follow the `skill_add` command shape (`crates/cli/src/commands.rs::skill_add`) for pool access via `RuntimePaths`.

### 5.4 `crates/daemon/src/hook_exec.rs` (new) — the subprocess runner

**Payload (stdin)** — stable snake_case, pinned by a serialization test like codex's `hook_payload_serializes_stable_wire_shape`:

```rust
#[derive(Debug, Serialize)]
pub struct HookPayload<'a> {
    pub payload_version: u32,          // 1
    pub event: &'a str,                // HookEvent::as_str()
    pub hook_id: &'a str,
    pub session_id: String,
    pub run_id: String,
    pub repository: String,            // canonical roots — cline's workspaceInfo,
    pub worktree: String,              // minus the git fields (a hook can ask git itself; it can read the worktree)
    pub triggered_at: String,          // RFC3339, seconds precision
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<HookPayloadTool<'a>>,      // tool.pre + tool.post
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<HookPayloadOutcome>,    // tool.post only
}

#[derive(Debug, Serialize)]
pub struct HookPayloadTool<'a> {
    pub name: &'a str,
    pub arguments_json: &'a str,       // the canonical JSON ToolCall carries
}

#[derive(Debug, Serialize)]
pub struct HookPayloadOutcome {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,       // ToolOutcome::Failed's message
    pub duration_ms: u64,
}
```

**Control (stdout)** — cline's last-`HOOK_CONTROL\t`-line convention, lattice vocabulary:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookControl {
    pub decision: HookControlDecision, // "allow" | "deny" | "rewrite"
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub rewrite: Option<HookControlRewrite>, // {name, arguments_json}
}
```

`parse_control(stdout: &str) -> Result<Option<HookControl>, String>`: split lines, take the **last** line starting `HOOK_CONTROL\t` and parse the remainder; if none, and the trimmed stdout non-empty and parses as a `HookControl`, use it; else `Ok(None)`. `deny_unknown_fields` so a `{"systemPrompt": …}` from a cline-shaped script is a **legible protocol error** (treated as a malformed control, table below), never silently ignored.

**Profile lowering** (the `skill_exec::profile_for_permissions` idiom):

```rust
fn profile_for_hook(spec: &HookSpec, ctx: &HookRunContextPaths) -> SandboxProfile {
    SandboxProfile {
        plugin: format!("hook:{}", spec.id),
        env_allowlist: Vec::new(),                       // clean env, always
        read_paths: vec![ctx.worktree.clone(), ctx.hook_dir.clone()],
        write_paths: vec![ctx.worktree.clone()],         // verify hooks build/test (see §9.4)
        network_allowlist: Vec::new(),                   // HookNetwork::Deny — the only value
        brokered_secrets: Vec::new(),
        allow_subprocess: true,                          // cargo → rustc etc.
        memory_mb: 512,
        cpu_seconds: spec_timeout,                       // = timeout_seconds
        wall_seconds: spec_timeout,
        maximum_output_mb: 1,
    }
}
```

`working_directory` placeholders resolved with the **same token rules** as `skill_exec::substitute_placeholders` (`$REPOSITORY`/`$WORKTREE`/`$HOME` — `parse_hook` already guaranteed no other name appears); an unresolvable value is an error, never a verbatim path.

**Verdict mapping** (the normative table; `HookRunner::run_hook(spec, payload) -> (HookVerdict, DispatchAudit)`):

| process result | observe | validate | mutate |
|---|---|---|---|
| exit 0, control `allow` or no control | `Observed` | `Allow` | `Allow` |
| exit 0, control `deny` | `Observed` (control ignored, logged) | `Deny { reason }` | `Deny { reason }` |
| exit 0, control `rewrite` | `Observed` (ignored, logged) | `Deny { "validate hook attempted a rewrite" }` | `Rewrite { call }` (missing/oversized `rewrite` field ⇒ `Deny { "malformed rewrite" }`) |
| exit 0, malformed control line | `Observed` | `Deny { "malformed HOOK_CONTROL: …" }` | `Deny { … }` |
| non-zero exit, `failure = "block"` | `Deny { "hook failed: exit N — <stderr head>" }` | same | same |
| non-zero exit, `failure = "warn"` | `Observed` (warning recorded) | `Observed` (warning recorded) | `Observed` (warning recorded) |
| timed out / killed | treated as non-zero exit (failure policy row above) | ″ | ″ |
| sandbox refused to run (`SandboxError`) | `failure = "block"` ⇒ `Deny`; `"warn"` ⇒ `Observed` + warning — fail-closed either way, never an unconfined retry | ″ | ″ |

Default reason when a `deny` control carries none: `"denied by hook"`. `rewrite.arguments_json` is bounded (256 KiB) and must itself be valid JSON, else malformed.

**Audit**: every invocation inserts one `hook_dispatches` row (verdict/applied per 0027's vocabulary; `rewrote_action` = the rewritten `ToolCall::digest()`, **never the call itself** — the 0027 comment is the contract; exit_status/timed_out/duration_ms/output_bytes from the `SandboxOutcome`; `error` for sandbox refusals).

### 5.5 `crates/daemon/src/hook_engine.rs` (new) — dispatch

```rust
/// Everything the engine needs to dispatch one event for one run.
#[derive(Debug, Clone)]
pub struct HookRunMeta {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub repository: PathBuf,   // canonical
    pub worktree: PathBuf,     // canonical
}

pub struct HookEngine {
    pool: SqlitePool,
    runner: HookRunner,        // owns the Box<dyn SandboxExecutor>
}

impl HookEngine {
    /// Approved hooks for `event`, in total dispatch order.
    /// WHERE approval_state = 'approved' AND approved_content_hash = content_hash
    ///   AND event = ? AND scope_kind IN ('user','repository')
    ///   AND (scope_kind = 'user' OR scope_key = <this repository>)
    /// ORDER BY priority, id  -- (dispatch_key: never filesystem order)
    async fn approved_hooks(&self, event: HookEvent, meta: &HookRunMeta)
        -> anyhow::Result<Vec<HookSpec>>;

    /// Sequential dispatch (order is semantic — the lattice needs dispatch
    /// order for conflict attribution), then `combine`. Total wall clock is
    /// bounded by construction: ≤ MAX_HOOKS_PER_EVENT × each spec's own
    /// timeout, each enforced by the sandbox kill.
    pub async fn dispatch_tool_pre(&self, meta: &HookRunMeta, call: &ToolCall)
        -> anyhow::Result<HookOutcome>;

    /// Observation only — verdicts recorded, never acted on (post-hoc).
    pub async fn dispatch_tool_post(&self, meta: &HookRunMeta, call: &ToolCall,
        outcome: &HookPayloadOutcome) -> anyhow::Result<()>;

    pub async fn dispatch_run_event(&self, meta: &HookRunMeta, event: HookEvent)
        -> anyhow::Result<()>; // run.start / run.end, observe-only

    /// Stamp how a rewrite ended (0027 `applied`: rewrite-reentered |
    /// rewrite-refused) onto the dispatch row that proposed it.
    pub async fn report_rewrite(&self, run_id: RunId, subject_digest: &str,
        applied: &str) -> anyhow::Result<()>;
}
```

### 5.6 `crates/runtime/src/agent.rs` — wiring verdicts into tool dispatch

**Seam** (mirrors `QuestionChannel`/`RoutingOutcomeSink`; the concrete types come from `codypendent_sandbox::hook`, which the runtime already depends on):

```rust
use codypendent_sandbox::hook::{HookOutcome, ToolCall as HookToolCall};

#[async_trait]
pub trait HookDispatch: Send + Sync {
    async fn tool_pre(&self, meta: &HookRunMeta, call: &HookToolCall) -> anyhow::Result<HookOutcome>;
    async fn tool_post(&self, meta: &HookRunMeta, call: &HookToolCall,
        success: bool, message: Option<&str>, duration_ms: u64) -> anyhow::Result<()>;
    async fn run_event(&self, meta: &HookRunMeta, start: bool) -> anyhow::Result<()>;
    async fn report_rewrite(&self, run_id: RunId, subject_digest: &str, applied: &str)
        -> anyhow::Result<()>;
}
```

Runtime field + builder (the `questions` pattern): `hooks: Option<Arc<dyn HookDispatch>>` / `with_hooks(...)`. `None` ⇒ zero behavior change.

**`run_tool`, step (a′) — inserted between `prepare` and the policy evaluation:**

```rust
        let hook_call = HookToolCall {
            name: tool.to_string(),
            arguments_json: canonical_json(&args), // the same canonicalization hash_json uses
        };
        if let Some(hooks) = &self.hooks {
            match hooks.tool_pre(&self.hook_meta(run), &hook_call).await? {
                HookOutcome::Proceed => {}
                HookOutcome::Denied { reasons } => {
                    let text = format!("blocked by hook: {}", reasons.join("; "));
                    // emit ToolDenied { action: prepared.action, reasons } +
                    // ToolCompleted Failed { text }; push action_digest(tool, "denied", None);
                    return Ok(ToolFlow::Observation { observation: text, artifact: None });
                }
                HookOutcome::Rewritten(unapproved) => {
                    return self
                        .run_rewritten(run, run_actor, tool, unapproved, &hook_call, actions, cancel)
                        .await;
                }
            }
        }
        // (b) policy evaluation continues UNCHANGED for the Proceed path.
```

**The rewrite path** — the only consumer `Unapproved::reenter` will ever have; every branch is forced by the type wall:

```rust
    /// Execute a hook-rewritten call. RULES:
    /// 1. The rewrite NEVER re-enters hook dispatch (no recursion; hooks fire
    ///    once per model-proposed call).
    /// 2. A fresh human approval is ALWAYS parked for the rewritten action —
    ///    mutate ⇒ `requires_approval = true` is structural in parse_hook, so
    ///    there is no approval-free rewrite, even one policy would auto-allow.
    /// 3. The approval that satisfies re-entry is digest-bound to the
    ///    rewritten call (ReentryContext), so it cannot be spent on anything
    ///    else and nothing else can be spent on it.
    async fn run_rewritten(
        &self,
        run: &RunContext,
        run_actor: &Actor,
        tool: &str,
        unapproved: Unapproved<HookToolCall>,
        original: &HookToolCall,
        actions: &mut Vec<Value>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<ToolFlow> {
        let adapter = RunPolicyAdapter::new(self.policy.clone(), self.eval_ctx(run))
            .with_tool_lowering(self.rewrite_lowering(run));
        // Probe policy first with NO approval in hand: a Deny is final.
        let probe = unapproved.clone().reenter(&adapter, &ReentryContext::default());
        let (lowered_action, rewritten_digest) = match &probe {
            Err(HookDenied::Policy { hook, code }) => {
                // report_rewrite(.., "rewrite-refused"); ToolDenied + ToolCompleted
                // Failed(format!("hook rewrite by `{hook}` refused by policy: {code}"))
                /* return denial observation */
            }
            // Ok (policy would allow outright) or ApprovalMismatch (needs one):
            // either way RULE 2 demands a human see it. Lower the call again
            // (the same lowering the adapter used) purely for the approval card.
            _ => /* (self.lower_rewrite(...), digest computed by re-lowering —
                    see gotcha 6 for why the digest comes from a clone's value
                    via lowering, not from an accessor that must not exist) */,
        };
        // Park a NON-reusable approval for the lowered rewritten action.
        let approval_id = self.journal.request(ApprovalRequest {
            action: lowered_action, allow_run_reuse: false, /* … */
        }).await?;
        // WaitingForApproval + ToolProposed + await_decision raced with cancel
        // (verbatim the existing RequireApproval block's structure).
        if decision != ApprovalDecision::Approve {
            // report_rewrite(.., "rewrite-refused"); "approval rejected" observation
        }
        let authorized = unapproved
            .reenter(&adapter, &ReentryContext { approved_digest: Some(rewritten_digest) })
            /* an Err here is a hard bug or a policy change mid-flight — surface
               as a denial observation, never a panic */;
        // report_rewrite(.., "rewrite-reentered");
        let prepared = self.prepare(&authorized.value().name,
            &serde_json::from_str(&authorized.value().arguments_json)?, run).await?;
        // ToolStarted + execute_prepared + ToolCompleted — the ordinary (d) tail,
        // WITHOUT a second policy/approval pass: `authorized` IS the proof
        // policy ran on this exact value, and the approval was digest-bound.
        // The observation is prefixed: "hook `<id>` rewrote this call; executed: <name>"
    }
```

**Rewrite lowering** (fail-closed subset — `RunPolicyAdapter` refuses everything else with `policy.unknown-tool`):

```rust
    /// v1 lowers only tools whose ProposedAction mapping is pure (no artifact
    /// writes, no async): `shell.run` and `workspace.read_file`. Everything
    /// else a mutate hook proposes is refused, not guessed at.
    fn rewrite_lowering(&self, run: &RunContext) -> ToolCallLowering {
        let worktree = self.eval_ctx(run).worktree;
        Arc::new(move |name: &str, args_json: &str| match name {
            "shell.run" => /* parse {program, args, cwd?, timeout_secs?}; env is
                              FORCED empty — a hook cannot smuggle variables */
                Some(ProposedAction::ExecuteCommand { .. }),
            "workspace.read_file" => Some(ProposedAction::ReadFiles { .. }),
            _ => None,
        })
    }
```

**`tool_post` + run events**: after the existing `ToolCompleted` emit in `run_tool`, when hooks are wired, call `hooks.tool_post(...)` (awaited; errors logged, never surfaced — post-hoc observation must not fail a succeeded tool). `run_event(start=true/false)` at the loop's run start/terminal, same error discipline.

### 5.7 `crates/codypendentd/src/executor.rs` — assembly

Build one `HookEngine` per daemon (pool + `enforcing_executor()?`; if the platform has no sandbox backend, hooks are **not wired** and a startup warning names why — fail-closed, cline-consistent: no sandbox ⇒ no hooks, never unconfined hooks). Wrap as `Arc<dyn HookDispatch>` and attach via `.with_hooks(...)` **only** on the interactive session-run path. `workflow_exec.rs` and any webhook-origin run construction do not call `with_hooks` — add a comment at each site naming the rule ("unattended paths disable hooks").

### 5.8 `Cargo.toml`

No new external dependencies anywhere. (`codypendent-daemon` already depends on `codypendent-sandbox`; `codypendent-runtime` on both.)

## 6. Protocol & persistence

- **Daemon⇄client wire**: no new commands or events in v1. Hook denials surface through the existing `ToolDenied { reasons }` / `ToolCompleted { Failed }` events (reasons prefixed `blocked by hook:` / attributed `hook_id: reason` by `combine`); a rewrite's execution is fully visible as the ordinary `ToolProposed`/`ApprovalRequested`/`ToolStarted`/`ToolCompleted` sequence for the rewritten action — which is exactly the 0027 design ("the executed action is audited by the ordinary proposal record").
- **Daemon⇄hook-subprocess wire** (new, versioned): stdin `HookPayload` (`payload_version: 1`, §5.4) — additive evolution only; stdout `HOOK_CONTROL\t<json>` last-line-wins with whole-stdout fallback, `deny_unknown_fields`.
- **Persistence**: the existing `hooks` and `hook_dispatches` tables (migration 0027) gain their first writers; **no new migration**. `hooks.approval_state` transitions: `pending → approved` (CLI approve), `pending → rejected` (CLI reject), `any → pending` (content-hash change on rescan). `hook_dispatches.applied` uses exactly 0027's vocabulary: `allowed | denied | rewrite-reentered | rewrite-refused`.
- **Stable strings**: origin label `hook:<id>` on sanitized output; denial observation prefix `blocked by hook:`; audit `resolved_by` untouched (rewrite approvals are ordinary human approvals).

## 7. Acceptance criteria

RULES (MUST, each tested in §8):

1. A hook never dispatches unless `approval_state = 'approved'` **and** `approved_content_hash = content_hash`. Editing an approved `hook.toml` and rescanning flips it to `pending` and it stops firing.
2. A repository-committed `hook.toml` claiming `scope = "user"` (or any tier other than `repository`) is refused at scan (`HookError::ScopeMismatch`) and never lands in the table.
3. A rewritten tool call is never executed except through `Unapproved::reenter` — enforced by the compiler; the spec adds no accessor, no serde, no bypass. The rewritten call never re-enters hook dispatch.
4. Every rewrite parks a fresh, non-reusable human approval for the rewritten action, and the approval that satisfies re-entry is digest-bound to that exact call.
5. Hooks run only under the enforcing sandbox: network denied, clean environment, wall-clock killed at `timeout_seconds`, output capped and sanitized. A platform without a backend wires no hooks (and says so) rather than running one unconfined.
6. Unattended paths dispatch no hooks: workflow agent-node runs, webhook-triggered runs, and sandbox guests have no `HookDispatch`.
7. Deny-wins is untouched: a policy `Deny` on the original call still short-circuits identically whether or not hooks ran; a hook `Allow` cannot override policy (hooks run before policy and can only narrow — `Proceed` just means "policy decides as always").

Checkable outcomes:

8. RUN: place a `tool.pre` `validate` variant of the shipped `docs/specs/hook.toml` (program `/bin/sh`, args `["-c", "exit 0"]` — absolute, as the sandbox requires) under `<repo>/.codypendent/hooks/verify.toml`; start the daemon. EXPECT `codypendent hook list` shows it `pending`; a tool call dispatches nothing.
9. RUN `codypendent hook approve rust.verify-after-patch`, then a `shell.run` call. EXPECT one `hook_dispatches` row (`verdict='allow'`, `applied='allowed'`, `duration_ms > 0`) and the tool executes normally.
10. A validate hook that prints `HOOK_CONTROL\t{"decision":"deny","reason":"tests must pass first"}` blocks the call: `ToolCompleted { Failed }` whose message contains `blocked by hook:` and `tests must pass first`; `hook_dispatches.verdict='deny'`, `applied='denied'`; the model observation carries the reason.
11. A validate hook that exits 1 under `failure = "block"` denies with the exit status in the reason; the same hook under `failure = "warn"` lets the call proceed and records the warning.
12. A mutate hook (`requires_approval = true`, `event = "tool.pre"`) that emits `{"decision":"rewrite","rewrite":{"name":"shell.run","arguments_json":"…"}}` produces: an `ApprovalRequested` for the **rewritten** command (card shows the rewritten program/args), and on approve the rewritten command executes with the observation prefixed by the hook attribution; `hook_dispatches.applied='rewrite-reentered'`, `rewrote_action` = the rewrite's digest. On reject: nothing executes, `applied='rewrite-refused'`.
13. Two mutate hooks proposing different rewrites for the same call ⇒ the call is denied with a `conflicts with the rewrite proposed by` reason (the lattice, now reachable end-to-end).
14. A hook that sleeps past `timeout_seconds` is killed; under `failure="block"` the call is denied with a timeout reason; `hook_dispatches.timed_out = 1`.
15. A hook script that attempts `curl example.com` fails (network denied by the sandbox); a hook that reads the worktree succeeds; one that reads `$HOME/.ssh` fails.
16. 33 hooks bound to `tool.pre` in one scope fail the root's scan (`TooManyHooks`); nothing from that root registers.
17. RUN `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace` — EXPECT green, including every pre-existing `hook.rs` and `policy_gate.rs` test unmodified.

## 8. Tests

- `crates/sandbox/src/executor.rs`: `stdin_payload_is_delivered_to_the_child` (a confined `/bin/cat`-style echo), `child_that_ignores_stdin_does_not_error` (BrokenPipe tolerance), `oversized_stdin_is_refused_not_truncated`.
- `crates/daemon/src/hooks.rs`:
  `scan_registers_pending_never_approved`, `changed_hash_resets_to_pending_and_stops_dispatch` (RULE 1),
  `scope_mismatch_is_refused_at_scan` (RULE 2), `removed_file_row_is_deleted`,
  `hostile_fanout_fails_the_root` (criterion 16), `scan_is_deterministic_across_enumeration_order`.
- `crates/daemon/src/hook_exec.rs`:
  `payload_serializes_stable_wire_shape` (pin the exact JSON, codex-style),
  `control_line_last_hook_control_wins`, `whole_stdout_fallback_parses`,
  `unknown_control_fields_are_a_protocol_error`,
  `verdict_table` (one test per row of §5.4's table, table-driven),
  `validate_hook_rewrite_is_denied_as_protocol_violation`,
  `network_and_home_are_denied_worktree_is_writable` (it-test behind the same platform gate as `sandbox/tests/enforcement_it.rs`),
  `timeout_kills_and_maps_through_failure_policy`.
- `crates/daemon/src/hook_engine.rs`:
  `only_approved_hash_matched_hooks_dispatch`, `dispatch_order_is_priority_then_id`,
  `repository_hooks_filter_on_scope_key`, `dispatch_rows_are_written_per_invocation`,
  `report_rewrite_stamps_applied`.
- `crates/runtime` (agent.rs, stub `HookDispatch`):
  `hook_deny_blocks_before_policy_and_emits_tool_denied`,
  `hook_proceed_changes_nothing` (byte-identical event sequence vs. hooks-unwired),
  `rewrite_parks_a_fresh_nonreusable_approval_and_executes_on_approve` (RULE 4),
  `rewrite_reject_executes_nothing`,
  `rewritten_call_does_not_reenter_hooks` (RULE 3, via a counting stub),
  `unknown_rewritten_tool_is_refused_by_the_lowering`,
  `tool_post_and_run_event_errors_are_swallowed`.
- `crates/codypendentd`: `workflow_and_webhook_runs_have_no_hook_dispatch` (RULE 6, assembly-level).

## 9. Gotchas

1. **`content_hash` must be `HookSpec::content_digest(raw)`**, the domain-separated hash — not `sha256(file)`. The approval binds to what hook.rs defines; two different hash functions between scan and approve means approvals that never match and hooks that never fire (or worse, a collision domain shared with other artifacts).
2. **Dispatch order is `(priority, id)`, never directory order** — `dispatch_key`'s doc is explicit that a package must not influence ordering via file names. Sort after loading rows, don't trust `ORDER BY` alone across scopes (user + repository rows merge into one sequence).
3. **The `Unapproved` value has no accessor — so lower the rewrite via the lowering, not via a getter.** The approval card and the parked `ProposedAction` for a rewrite must come from `ToolCallLowering` applied to a *clone's* re-entry probe path (§5.6), because adding `fn value()` to `Unapproved` to "just read it for the card" is precisely the accessor the type exists to withhold. If the lowering refuses the tool, there is no card and no execution — correct.
4. **Worktree write access for hooks is a deliberate grant, not an accident.** The shipped example hook is `cargo test --workspace` — it writes `target/`. Document it in the profile-lowering comment; anyone tempted to make hooks read-only must also delete the shipped example. What stays absolutely denied: network, env, `$HOME` beyond nothing, and everything outside the worktree + the hook's own directory.
5. **`Stdio::piped()` stdin can deadlock a large payload against a full stdout pipe** — write stdin from its own thread (the executor already reads stdout/stderr from threads); never write-then-read sequentially on one thread. cline's runner has the same shape (`Promise.all([spawned, writeToChildStdin], completed)` raced).
6. **A child may exit before reading stdin** — treat `BrokenPipe`/`WouldBlock`-on-close as success, exactly as subprocess-runner.ts tolerates EPIPE/EOF; a hook that doesn't care about the payload is legal.
7. **`tool.post` verdicts must be ignored, not merely down-weighted** — `HookEvent::is_preventable()` is the authority; a `deny` control from a post hook is recorded (`verdict='deny'`) but `applied='allowed'`, and `parse_hook` has already refused `failure="block"` on post-hoc events, so a failing post hook can never fail the tool.
8. **Do not canonicalize `arguments_json` twice differently** — the digest the approval binds to is computed over the exact `arguments_json` string in the `ToolCall`. Build the string once (the same canonical-JSON function `hash_json` uses in `run_tool`) and thread it; re-serializing a parsed Value with different key ordering silently breaks `ApprovalMismatch` into a permanent denial.
9. **Sandbox `Sanitized` output is the only thing that may reach logs or reasons** — stderr heads quoted into deny reasons come from `SandboxOutcome.stderr` (already control-stripped and origin-labeled `hook:<id>`), never from a raw pipe.
10. **The scan must not make startup fail** — per-file failures are warnings (the `scan_installed_skills` discipline: "skill package not registered"), but a per-root `validate_event_set` failure kills that whole root's registration (fail-closed against hostile fan-out) while the daemon still boots.
11. **`approved_hooks` must filter repository hooks by `scope_key = <this repo>`** — a per-user daemon serves several checkouts over one socket (the multi-repo note in `StartRun.repository`); without the filter, repo A's committed hooks fire on repo B's runs.
12. **Clippy `-D warnings` and no `unsafe`** apply to the executor change — the stdin thread uses only std I/O; no `pre_exec`, ever (the module doc already records why rlimits are not enforced on macOS for the same reason).

## 10. Out of scope

- `patch.proposed` and `approval.requested` event dispatch (they parse and register today; dispatch is a follow-up once the changeset pipeline exposes a seam).
- `[output] create_artifact` / `attach_to` — captured output is audited (size, sanitized text in logs) but not persisted as artifacts in v1.
- A `wasm` hook runtime (`HookRuntime` is one-variant on purpose; the wasmtime path is Phase 6's).
- cline's transcript-shaping controls (`systemPrompt`, `appendMessages`, `replaceMessages`, `context`) — deliberately not adopted; a hook must never write into the model's context.
- codex-style matchers (per-tool filters on hook declarations) — v1 hooks see every `tool.pre`; filtering happens inside the hook script.
- `UserPromptSubmit`/compaction events (no corresponding `HookEvent` variants exist; adding one is a hook.rs change with its own review, not this adoption).
- Hook approval from the TUI (CLI only in v1; the TUI's registry surfaces can adopt it later).
- `RegistryItemKind::Hook` registry rows / retrieval disclosure (`hooks.registry_item_id` stays NULL).
- Organization/system scope discovery (no discoverer produces them today — hook.rs's own note).

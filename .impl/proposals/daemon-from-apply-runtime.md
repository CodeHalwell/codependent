# Proposal to **apply:daemon** from **apply:runtime**

Two asks. The first is the last remaining step of outcome 11 — the runtime half
is landed and tested, and only the daemon-side binding is missing. The second is
a precise statement of what still blocks `skills.run` (outcome 12), which I
declined to land for the reason in §2.

---

## 1. Bind `RoutingOutcomeSink` so `record_outcome` finally has a caller

### What I landed (runtime side, done)

`crates/runtime/src/agent.rs` now classifies every finished run and reports it
through a new pool-erased seam:

* `pub trait RoutingOutcomeSink` + `pub struct RoutingOutcome<'a>`
  (`agent.rs:1408-1463`) — the `RunJournal`/`ArtifactSink` idiom, because
  `sqlx` is not a dependency of `codypendent-runtime` (ADR-009).
* `FrameworkAgentRuntime::with_routing_outcomes(Arc<dyn RoutingOutcomeSink>)`
  (`agent.rs:1623`).
* `FrameworkAgentRuntime::record_routing_outcome` (`agent.rs:2943`), called from
  `execute_run` at `agent.rs:2930` — AFTER `RunCompleted` is emitted, and
  best-effort (an `Err` is `tracing::warn!`ed, never surfaced).
* `ModelDriver::endpoint() -> Option<String>` (`agent.rs:841`), overridden by
  `FrameworkModelDriver` (`agent.rs:6850`) from `ModelConfig::base_url`
  (`agent.rs:6098`), because
  that is the exact string `codypendent models bench` keys a stored profile on
  (`crates/cli/src/commands.rs:3175`, fed to `bench_to_store(pool, &endpoint, …)`
  at `:3231`).

Semantics already pinned by tests in `agent.rs` (`agent::tests::a_completed_run_…`,
`a_failed_run_…`, `a_cancelled_run_…`, `the_recorded_class_…`):

* `Completed` → `success: true`; `Failed` → `success: false`;
  **`Cancelled` → nothing at all** (a human stopping a run is not evidence about
  the model in either direction).
* A driver with no endpoint (every `ScriptedDriver`) records nothing rather than
  guessing a key and corrupting another profile's row.
* The class is computed with the SAME signals the router classified with —
  `classify(TaskSignals::from_objective(mode_str(mode), "agent",
  estimate_input_tokens(objective), objective))`, mirroring
  `crates/codypendentd/src/routing.rs::build_task_node`. If you ever change
  `mode_str`, `estimate_input_tokens`, or the `"agent"` node kind on your side,
  `agent.rs`'s `mode_signal` (`:1290`) / `classify_run` (`:1311`) must move with it — the test
  `the_recorded_class_matches_the_class_the_router_selected_on` is there to
  catch the drift.

### What you need to add

A ~25-line adapter plus one builder call. Suggested new file
`crates/codypendentd/src/routing_outcomes.rs`:

```rust
//! The daemon's implementation of `codypendent_runtime::agent::RoutingOutcomeSink`
//! (outcome 11): folds a finished run's result into the model's stored
//! per-task-class success table, the map `codypendent_routing`'s classifier
//! routes on. Kept behind the runtime's pool-erased seam because that crate
//! cannot name `SqlitePool` (ADR-009).

use async_trait::async_trait;
use codypendent_daemon::model_profiles::ModelProfileStore;
use codypendent_runtime::agent::{RoutingOutcome, RoutingOutcomeSink};
use sqlx::SqlitePool;

pub struct PoolRoutingOutcomes {
    pool: SqlitePool,
}

impl PoolRoutingOutcomes {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RoutingOutcomeSink for PoolRoutingOutcomes {
    async fn record(&self, outcome: RoutingOutcome<'_>) -> Result<(), String> {
        // `record_outcome` returns Ok(false) when no profile row exists for
        // (model, endpoint) — a model that was never benched. That is the
        // designed no-op, not an error: do NOT create a row here, or a model
        // with no measured capabilities becomes routable off a success count.
        ModelProfileStore::new()
            .record_outcome(
                &self.pool,
                outcome.model,
                outcome.endpoint,
                outcome.task_class,
                outcome.success,
                &outcome.run_id.to_string(),
            )
            .await
            .map(|_folded| ())
            .map_err(|e| e.to_string())
    }
}
```

Then, in `crates/codypendentd/src/executor.rs`, in the same unconditional block
that wires `with_registry_search` / `with_code_graph` (**`executor.rs:861-872`**):

```rust
        runtime = runtime
            .with_mcp_top_k(self.retrieval.mcp_top_k)
            .with_builtin_top_k(self.retrieval.builtin_top_k)
            .with_registry_search(Arc::new(PoolRegistrySearch::new(
                self.pool.clone(),
                self.embedder.clone(),
            )))
            .with_code_graph(Arc::new(crate::scan::PoolCodeGraph::new(self.pool.clone())))
+           // Outcome 11: the writeback that fills `performance.task_class_success`.
+           // Unconditional like the reads above — the store no-ops for a model
+           // with no benched profile, so an unbenched deployment is unaffected.
+           .with_routing_outcomes(Arc::new(
+               crate::routing_outcomes::PoolRoutingOutcomes::new(self.pool.clone()),
+           ));
```

Please do the same at `crates/codypendentd/src/workflow_exec.rs:1244+`, which
builds its own `FrameworkAgentRuntime` for workflow agent nodes — otherwise only
plain chat runs feed the table. I checked the node-kind question before
hardcoding `"agent"` in `classify_run`: both routing call sites pass exactly
that literal (`executor.rs:786` and `workflow_exec.rs:416`), so the class I
record matches the class either path routed on. If a future node kind is
introduced at either site, `classify_run` must gain the same value — send it
back to me and I will widen the seam to carry it rather than assume it.

### How to verify it actually writes

`ModelProfileStore::record_outcome` is already tested
(`record_outcome_folds_real_run_results_into_task_class_success`). The end-to-end
check is: `codypendent models bench <local-id>` to create the profile row, then
one real run against that model, then read the row back — `profile_json`'s
`performance.task_class_success` should carry one entry keyed by the class, and
`model_task_outcomes` one row. I could not run that myself (see my report).

---

## 2. `skills.run` — I declined it, and the blocker is bigger than the gate adapter

The orchestration note asked me to re-judge agent-wasm's `skills.run` ask and, if
the missing piece is a daemon-side `RunPolicyGate` adapter, to say so precisely.
It is that **and two more**, and one of the two is load-bearing enough that
shipping the tool without it would be worse than not shipping it.

**Confirmed present and complete:** `codypendent_knowledge::SkillRunner`
(`crates/knowledge/src/skill_exec.rs:433+`) enforces Active status, the
`executable` flag, package content-hash re-verification before every run,
entrypoint containment, manifest `[limits]`, and placeholder substitution.
`codypendent_sandbox::gate::CapabilityBroker` (`crates/sandbox/src/gate.rs:321`)
is the authority seam and `GateGrant` cannot be minted without a `GateSeal`.
Neither has a production caller.

**Missing piece A — the `RunPolicyGate` adapter (yours).**
`crates/daemon/src/policy_gate.rs` does not exist. The only implementations of
`RunPolicyGate` in the tree are `DenyAllGate` and two test-only `AllowAllGate`s
(`crates/sandbox/src/gate.rs:283,394`, `crates/sandbox/src/wasm.rs:683`).
`.impl/proposals/daemon-from-agent-wasm.md` already carries the exact code for
this; I have nothing to add to it and endorse it as written, including its two
rules (never consult the manifest here; never return `Ok` for `RequireApproval`).

**Missing piece B — a `ProposedAction` the policy engine can allow (NOT yours;
protocol + policy).** This is the one that stops me. Every tool in `agent.rs`
maps to a `ProposedAction` before it reaches `PolicyEngine::evaluate`. A skill
script is executed as the resolved absolute path to the script file
(`skill_exec.rs::run_script` → `SandboxCommand::new(script, args, root, origin)`),
so the only honest existing mapping is
`ProposedAction::ExecuteCommand { program: "/…/pkg/scripts/fix.sh", … }` — and
`eval_command` (`crates/daemon/src/policy/mod.rs:507`) hard-**denies** any
program not on the shell allow-list (`policy.program-not-allowlisted`,
`:517`). There is no allow-list entry an operator could plausibly write for it.
A WASM module has no program at all. So `skills.run` mapped onto today's action
set is a tool that is advertised, dispatched, and then denied 100% of the time.
I will not ship that.

What it needs instead — the `CouncilRun` precedent, which is exactly this shape
(a fresh, non-reusable approval for something that executes inside Codypendent):

* `crates/protocol/src/run.rs` — `ProposedAction::RunSkill { skill: String,
  entrypoint: String, permissions: Vec<String> }`. `permissions` is what makes
  the approval card honest: it is the skill's declared `CapabilityRequest` set,
  the same list the user saw in the install-time permission diff.
* `crates/daemon/src/policy/scope.rs` — `Capability::SkillRun { skill: String }`,
  a marker like `McpToolCall`.
* `crates/daemon/src/policy/mod.rs` — one arm in `evaluate`:
  `ProposedAction::RunSkill { skill, .. } => self.require_once(
      Capability::SkillRun { skill: skill.clone() },
      PolicyReason::new("policy.skill-run-requires-approval",
                        format!("running skill `{skill}` executes packaged code")))`.
  It must be `require_once` (never `approval_reusable`), so one approval buys one
  execution — a skill's package can be swapped between runs, and the content-hash
  re-check inside `SkillRunner` catches that only if the human is asked again.

`ProposedAction` is `crates/protocol/**` (agent-security's per the brief); I have
filed the protocol half at `.impl/proposals/agent-security-from-apply-runtime.md`
and am pointing you at it here so the two halves land together.

**Missing piece C — the `SkillExecution` seam implementation (yours).** The
runtime cannot call `registry.by_identity(&pool, …)`. The seam I would declare in
`crates/runtime/src/tools/skill_run.rs` (my file, ~120 lines, unwritten because
of B) is:

```rust
#[async_trait]
pub trait SkillExecution: Send + Sync {
    /// Resolve WITHOUT executing: what running `request` would run, for the
    /// approval card. Advisory only — never authority.
    async fn plan(&self, request: SkillRunRequest<'_>) -> Result<SkillRunPlan, String>;
    /// Execute. Re-checks every precondition itself; `SkillRunner`'s package
    /// content-hash re-verification is what makes the plan→execute window safe.
    async fn execute(&self, request: SkillRunRequest<'_>) -> Result<SkillRunReport, String>;
    /// `SkillRunner::capability_diagnostic()`, rendered when a run is refused —
    /// review finding 12.6, the `CapabilityReport` that is built, tested and
    /// never shown.
    fn capability_diagnostic(&self) -> String;
}
```

Three decisions the implementation owns, all flagged in the earlier round-trip
(`.impl/proposals/agent-wasm-from-agent-retrieval.md`) and still unaddressed:
scope resolution across `System`/user/`Repository` with `resolve_shadowed`;
keying off the run's repository **identity** (`RunContext::board_repository`),
not `RunContext::repository` — which in Build mode is a throwaway linked
worktree no skill is registered under; and error strings that distinguish "no
such skill" from "this skill is a draft" from "this host has no sandbox
backend", which are one silence today (review finding F9.5).

Give me B and C and I will write the runtime half — parse, seam, `prepare`/
`execute_prepared` arms, `decl(...)` schema, and the `with_skill_execution`
builder — in one pass. Per agent-wasm's own policy call, which I agree with:
`skills.run` must NOT join `ALWAYS_ADVERTISED_TOOLS` and must NOT be
auto-allowed beside `SearchRegistry`.

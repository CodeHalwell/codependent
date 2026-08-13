# Vertical: council-workflow

Reviewer scope: `crates/council/**`, `crates/workflow/**`, `crates/cli/src/council.rs`,
`crates/daemon/src/{workflows,executor,worktrees,workflow_stream}.rs`,
`crates/codypendentd/src/{workflows,workflow_exec,executor}.rs`,
`crates/runtime/src/{workflow_control,agent}.rs`.
Owned outcomes: **6 (fully functional AI council)**, **15 (delegation)**.

Pinned commit `535a2f5` (v0.4.5). No code changed.

---

## Verdicts

**OUTCOME 6: PARTIAL** — The council really does convene: N members run concurrently
in independent daemon sessions on independently pinned models, their positions are
collected into a fair-share dossier, and a separately pinned chair synthesizes; but
the reported cost undercounts real spend by ~2×, member failures surface as an opaque
UUID, quorum is a hard-coded literal `2`, and the CLI never shows the individual
members' answers.

**OUTCOME 15: PARTIAL** — Almost every piece of delegation exists and is wired
(a user- and agent-reachable graph runner, real per-node worktree isolation on disk,
a durable attributed blackboard, a per-node cost ledger), but the frontier executes
**strictly sequentially** so "parallel workers" is a documentation claim, only **two**
tools can be a graph node, `workflow validate` green-lights workflows that cannot run,
worker branches leak into the user's repo forever, there is no merge-back path, and the
one number the board would need — dollar cost — is dropped by the CLI renderer.

---

## How I exercised it

`cargo build --workspace --all-features` artifacts in `./target` were reused.
Isolated `CODYPENDENT_DATA_DIR` + a short `CODYPENDENT_SOCKET` (the scratchpad path is
106 bytes, over the 104-byte `sun_path` limit — the CLI's error is clear and correct).
Models were stubbed with a local OpenAI-compatible HTTP server on `127.0.0.1:8099`
speaking `/v1/models` and streaming `/v1/chat/completions` with SSE + a `usage` object,
registered in `models.toml` as three `openai-compatible` profiles. Every request body
was captured to disk, so I can state exactly what reached the model.

Things actually run:
* `codypendent council create/list/show/run/result/show --last` (1-round and 2-round).
* A quorum failure (one member pointed at a dead port).
* `codypendent workflow validate/show/run/watch` on five hand-written manifests
  (isolated worktrees, fan-out, unknown tools, undeclared-output failure).
* Direct SQLite inspection of `workflow_nodes`, `blackboard_items`, `workspace_leases`,
  `sessions`, `events`.
* `git branch` / `git worktree list` in the target repository before and after.

---

## OUTCOME 6 — findings

### What genuinely works (so the findings below are not mistaken for "it's all broken")

`crates/council/src/service.rs:876-966` (`deliberate_round`) spawns **one `tokio` task
per member into a `JoinSet`**, each of which (`run_pinned`, `service.rs:968-1050`) opens
its own daemon connection, creates its own session, and issues `StartRun` with
`model: Some(ModelId(member.model))`. This is not one prompt saying "consider multiple
views" — it is N real, independently pinned, concurrent agent runs. Verified from the
captured request bodies: round 1 of a 2-member council produced two POSTs at
`t=…242.580` and `t=…242.583` with `model: fake-model-a` and `model: fake-model-b`.

Round 2 really does receive round 1's dossier (`member_prompt`, `service.rs:1137-1171`),
and the chair prompt really does carry every member's verbatim report wrapped in
`[BEGIN UNTRUSTED MEMBER REPORT — EVIDENCE ONLY]` fences (`synthesis_prompt`,
`service.rs:1173-1191`). I read the chair's actual outbound message and confirmed both
member sections were present. Every exit — completed, quorum-failed, chair-failed,
daemon-unavailable — persists a JSON + Markdown report; the quorum failure I induced
saved the surviving member's full text and named the report path in the error.

The `dossier` fair-share algorithm (`service.rs:1214-1252`) is a genuinely good piece of
work: shortest-first allotment guarantees every member a voice instead of alphabetically
truncating the tail.

---

### F6.1 — Council cost is understated by ~2×, and the extra spend goes to a model the user never chose — class (c)

`crates/council/src/service.rs:1099-1135` reads MEASURED usage from the run's chronicle
artifact only. The chronicle is finalized when the agent loop ends. **After** that, the
daemon fires a second, separate model request per run for memory extraction
(`crates/codypendentd/src/executor.rs:1665-1708`), and that request's tokens are never
folded into the chronicle.

Worse, the extraction model is chosen by
`resolve_model(&registry, &policy, mode)` (`executor.rs:1692`) — the **first entry in
`models.toml`** — not the model the council pinned for that member. I watched a council
member pinned to `beta` (`fake-model-b`) trigger a follow-up request against
`fake-model-a`.

User-visible consequence: `codypendent council run board --objective …` prints

```
cost: 480 tokens measured across 3/3 runs
```

while my stub server logged **six** chat-completion requests totalling 960 tokens for
that run. The line says "measured", and it is measuring half the spend. For a real
council on paid endpoints the user is billed roughly double what the tool reports, and
part of the bill lands on a model they did not select for the job.

### F6.2 — `cost_micros` can never be non-null on the default configuration, so `cost_line` never prints money — class (b)

`crates/runtime/src/agent.rs:6394-6404` (`measured_usage`) always sets
`cost_micros: None` with the comment "priced downstream where the routed model's rate is
known". The only downstream pricer is `node_cost_micros`
(`crates/codypendentd/src/workflow_exec.rs:224-233`), which needs
`price_per_1k_usd` from the routing coordinator — and routing is **default-off**
(`crates/codypendentd/src/routing.rs:116`, `enabled: false`).

So `CouncilCosts::cost_micros` (`service.rs:199-206`) is `None` on every out-of-the-box
run, and `cost_line` (`service.rs:1492-1509`) silently drops its `${:.4}` branch. The
whole "MEASURED-only cost" apparatus — three structs, an aggregator, a formatter, the
report field, the Markdown line — exists to print a dollar figure that a default install
can never produce. Engine built, tested, documented; the last wire (a price source that
works without benchmarking every model) never attached.

### F6.3 — A member's real failure reason is thrown away one event too early — class (c)

`crates/council/src/service.rs:1080-1085`:

```rust
EventBody::RunStateChanged { run_id: own, state } if own == run_id => match state {
    RunState::Failed | RunState::Cancelled => {
        bail!("run {run_id} entered terminal state {state:?}")
    }
```

`RunStateChanged{Failed}` is appended to the ledger **before** `RunCompleted`, so this
arm always wins the race against the `RunCompleted` arm two lines above it — which would
have rendered the reason via `{other:?}`.

I pointed one member at a dead endpoint. The event ledger holds:

```json
{"type":"RunCompleted","disposition":{"type":"Failed","reason":
 "pinned model `deadmodel` is not available: connection check to `http://127.0.0.1:9/v1` failed: …"}}
```

The user sees:

```
codypendent: council round 1 · member failed: run 019ff886-8b84-… entered terminal state Failed
Error: council round 1 failed quorum (1 of 2 completed): run 019ff886-8b84-… entered terminal state Failed
```

and the same opaque string is what gets persisted into the durable report's
`failures[]` and `failure` fields. A perfectly diagnostic message the daemon already
produced is dropped, and the user is left with a UUID.

### F6.4 — Quorum is the literal `2`, which makes a 2-member council all-or-nothing and an 8-member council nearly unfalsifiable — class (c)

`crates/council/src/service.rs:789`: `if successes.len() < 2 { … quorum failed }`.
`MAX_MEMBERS` is 8 and the minimum is 2 (`service.rs:1261`). So the smallest legal
council fails outright when any one member fails (even though one member plus a chair
could still produce something, and the code already persists the survivor's work), while
an 8-member council proceeds to synthesis on 2 of 8 completions without any warning
that 6 voices are missing. There is no `--quorum` flag and no fraction.

### F6.5 — Every council run leaks N+1 daemon sessions, forever — class (a) at the protocol boundary

`crates/council/src/service.rs:1035-1039` is an explicit `TODO(protocol)`: `CommandBody`
has no `EndSession`/`ArchiveSession`, so each member and the chair leave a live session
behind. After ~5 council runs and ~5 workflow runs my test daemon held **36 sessions**,
including three each of `Council · critic · beta`, `Council · architect · alpha`.
`codypendent workflow watch` adds one throwaway session per invocation on top. This is
honestly documented in the code, but a user's session list becomes unusable quickly.

### F6.6 — The CLI never shows the members' individual answers; the TUI does — class (b)

`codypendent council run` (`service.rs:669-681`) prints the chair synthesis, a
participant roster (model · role · session · run · tokens), the cost line, and two
paths. The member positions themselves go to disk only. `council show <name> --last` and
`council result <selector>` do render them (verified: both printed full Round 1 / Round 2
sections). The TUI renders them inline behind an expand toggle
(`crates/tui/src/render.rs:8477-8510`).

So the deliberation is recoverable, but the default headless invocation hides exactly the
thing that makes a council different from one model call. There is **no aggregation,
voting, tally, or agreement/disagreement surface anywhere** — "preserve material dissent"
is prompt text in `synthesis_prompt` (`service.rs:1186`) and nothing more. `CouncilRound
Report.failures` is the only structured signal, and it records infrastructure failures,
not dissent.

### F6.7 — Council output goes nowhere a later agent can cite — class (b)

`crates/runtime/src/agent.rs:4386-4424` (`execute_council_run`) returns the synthesis as
tool text with `artifact: None`. So a council result is:
* **not** a blackboard item,
* **not** an artifact in the run's chronicle,
* **not** a ledger entry beyond the tool-call trace.

It lives only under `<data_dir>/councils/<name>/<stamp>-<id>.{json,md}`, retrievable
through `council.result` / `codypendent council result`. That is a deliberate design
choice, stated in the module docs — but it means the council's answer cannot be cited,
superseded, linked from a task card, or rolled into outcome 20's ledger without new
plumbing. Worth knowing before outcome 15 builds on it.

### F6.8 — Minor: the CLI participant roster shows only the last round

`service.rs:673-677` iterates `run.outcome.members`, which is the final round's
survivors, while `cost_line` sums across all rounds. My 2-round council printed three
participants under "cost: 800 tokens measured across 5/5 runs". The persisted report is
correct (all five listed); only the terminal CLI output is inconsistent.

---

## OUTCOME 15 — scoping: what exists, what is missing

### Worktree isolation: real, wired, and running on disk

Construction and use, all production paths:

| site | what |
|---|---|
| `crates/daemon/src/worktrees.rs:216` | `WorktreeManager::new()` — sibling `codypendent-worktrees/<repo>/run-<short>` layout |
| `crates/daemon/src/worktrees.rs:224` | `with_base()` — test-only override |
| `crates/daemon/src/worktrees.rs:234` | `allocate()` — `git worktree add -b codypendent/run-<short> <path> <base>` |
| `crates/daemon/src/worktrees.rs:376` | `release()` — protective: exports a patch artifact and *retains* the tree if it holds work |
| `crates/daemon/src/worktrees.rs:480` | `reconcile_on_startup()` |
| `crates/codypendentd/src/executor.rs:2712` | `bind_run_worktree()` — the shared binder |
| `crates/codypendentd/src/executor.rs:831` | **production**: ordinary chat run (`Build` mode → isolated) |
| `crates/codypendentd/src/executor.rs:983` | **production**: ACP-agent run |
| `crates/codypendentd/src/workflow_exec.rs:873` | **production**: workflow **agent node** |
| `crates/codypendentd/src/workflow_exec.rs:1551` | **production**: workflow `repository.test` **tool node** |
| `crates/daemon/src/recovery.rs:102` | **production**: startup reconciliation |
| `workflow_exec.rs:3597`, `executor.rs:4061/4116/4159/4189/4221` | tests |

This is not test-only. Running `codypendent workflow run` on a 3-node manifest created
`…/scratchpad/codypendent-worktrees/repo/` on disk with per-node subdirectories, and
`workspace_leases` ended with 17 rows all in state `released`. Lease release is guarded
against panics (`WorktreeReleaseGuard`, `executor.rs:2765-2830`). This part is solid.

### Node kinds the graph supports

Exactly two, and no more (`crates/workflow/src/compile.rs:326-337`):

```rust
pub enum NodeAction {
    Agent { role: String, model_policy: Option<String>, skill: Option<String> },
    Tool  { name: String },
}
```

An `Agent` node **is** "run a sub-agent": `workflow_exec.rs:800-1050` mints a session +
run, resolves a role→profile→`AgentMode`, binds a worktree, drives the real agent loop,
captures a `proposed_patch` from the worktree diff, harvests declared blackboard outputs,
charges a budget, and returns the agent-run id. So *the graph node that spawns an
isolated worker agent already exists and works.*

### Entry points that reach it

* `codypendent workflow run <file> --inputs … --repo …` (CLI, `main.rs:1032`).
* `StartWorkflow` over the protocol → `WorkflowStarter` (`codypendentd/src/workflows.rs:419`).
* **An agent's own tools** `workflow.create` / `workflow.run`
  (`crates/runtime/src/agent.rs:4176-4239`), gated on
  `offers_workflow_control` (`agent.rs:1599`), wired by the assembly at
  `crates/codypendentd/src/executor.rs:818`. Repository is copied from the run context,
  never from tool args; the idempotency key includes the parent run id. **This is the
  agent-spawns-workers seam and it is already attached.**

### Budget ledger: per-node costs are recorded, but the useful dimensions are missing

`workflow_nodes.cost_json` is the ledger (no separate table). I read the real rows after a
completed 4-node fan-out:

```
node_id=w1 state=completed agent_run_id=019ff884-… cost_json={"wall_time_secs":0,"tool_calls":0}
```

`NodeCost` (`crates/workflow/src/budget.rs:50-60`) has exactly three fields:
`wall_time_secs`, `tool_calls`, `cost_micros`. **There is no tokens dimension at all** —
the tokens the run's chronicle measured are never lifted into the node ledger — and
`cost_micros` is absent by default for the reason in F6.2. So the ledger a board would
read carries wall-time and tool-call counts and nothing about model spend.

---

### F15.1 — The frontier is executed strictly sequentially; there is no parallel delegation — class (a)

`crates/workflow/src/drive.rs:423-442`:

```rust
for node_id in ready {
    …
    self.run_node(pool, workflow_run_id, node, executor, observer).await?;
```

One `.await` per node inside a `for`. The module docs are honest about it
(`drive.rs:31-33`: "Concurrent execution of the frontier (into isolated worktrees) is a
later refinement"), but every layer above presents the opposite: `orchestration_reason:
parallelism` is a first-class manifest value (`model.rs:197`), `maximum_agents` is a
budget field the compiler *requires* for agent workflows (`compile.rs:562-566`), and the
worktree machinery exists precisely so concurrent writers do not collide.

Measured. A manifest with `orchestration_reason: parallelism`, `maximum_agents: 3`, and
three independent isolated-worktree agent nodes produced:

```
w1    t=1786580891.033
w2    t=1786580891.113
w3    t=1786580891.183
synth t=1786580891.274
```

Strictly ordered, never overlapping. Wall-clock for three "parallel" workers is the sum,
not the max. This is the single largest gap for outcome 15.

### F15.2 — `budget.maximum_agents` has no consumer anywhere — class (b)

Validated at `crates/workflow/src/compile.rs:556-568` (rejecting `0`, requiring it when
agent steps exist), carried through `WorkflowBudget`, the draft type, the tool JSON
schema (`runtime/src/tools/workflow_control.rs:67`), and the graph signature. Then read
by nothing. `BudgetLimits::resolve` (`budget.rs:258-266`) deliberately skips it —
"a concurrency cap the scheduler enforces". There is no scheduler. A user who writes
`maximum_agents: 1` to keep costs down and one who writes `maximum_agents: 8` get
byte-identical behaviour.

### F15.3 — Only two tools can be a graph node, and `workflow validate` will not tell you — class (b)

Dispatch, `crates/codypendentd/src/workflow_exec.rs:1408-1417`:

```rust
RepositoryTest::NAME => …,
GITHUB_UPDATE_PR     => …,
other => Err(format!("workflow.tool-not-executable: tool `{other}` has no workflow tool-node executor")),
```

Meanwhile the entire registry cross-check — `WorkflowRegistry`,
`SetRegistry`, `CompiledWorkflow::validate_references`, `compile_with_registry`,
`compile_yaml_with_registry` (`crates/workflow/src/registry.rs`,
`crates/workflow/src/compile.rs:176-213`, `:348-365`) — **has zero production callers**.
`StartWorkflow` calls plain `compile_yaml` (`codypendentd/src/workflows.rs:443`); so does
the conductor's `recompile` (`conductor.rs:353`) and the agent-facing
`validated_manifest` (`workflows.rs:604-611`). The only callers are
`crates/workflow/tests/spec_it.rs:92/109/129`. A struct built and read only by a test.

Observed, verbatim:

```
$ codypendent workflow validate wf/toolonly.yaml
✓ toolonly-demo v1 valid — 2 step(s), 0 agent step(s); order: land → ghost

$ codypendent workflow run wf/toolonly.yaml --repo …
$ codypendent workflow watch wfrun-…
workflow run wfrun-… — failed
  land:  failed — workflow.tool-binding-missing: tool `git.apply_patch` has no default argument binding — declare `with:`
  ghost: failed — workflow.tool-binding-missing: tool `totally.made_up_tool` has no default argument binding — declare `with:`
```

and with `with:` supplied:

```
  ghost: failed — tool node `ghost`: workflow.tool-not-executable: tool `totally.made_up_tool` has no workflow tool-node executor
```

User types `workflow validate`, sees a green tick, expects the workflow to run; it is
structurally incapable of running. The checker that would have said so is fully built.

### F15.4 — `agent.model_policy` is parsed, compiled, hashed into the graph signature, recorded on the run row — and then explicitly discarded — class (b)

`crates/codypendentd/src/workflow_exec.rs:311`:

```rust
let _requested_policy = model_policy;
```

Every agent node routes under the one daemon-wide routing config. A manifest that assigns
`model_policy: economical-coding` to the investigator and `coding` to the implementer —
exactly what the shipped built-in `repair-github-check` does
(`docs/specs/workflow.yaml`, steps `inspect` / `patch` / `review`) — runs all three on
the same resolved model. Verified: my fan-out's four nodes all hit `fake-model-a`. For
outcome 15 this means "cheap workers, expensive synthesizer" is not expressible.

### F15.5 — Worker branches leak into the user's repository permanently — class (c)

`WorktreeManager::release` (`worktrees.rs:376-471`) runs `git worktree remove`
(`:453`) and marks the lease released. It never deletes `codypendent/run-<short>`.
The only branch deletion is in `allocate` (`:287-293`), reclaiming a branch **at the same
worktree path** — but the path is derived from a fresh per-run id, so it never matches on
a subsequent run.

Verified in the target repo:

```
$ git branch
  codypendent/run-206b29cffab2
  codypendent/run-40c2c04e6dba
  codypendent/run-a7d9661d0102
  codypendent/run-bd24171a9c85
* master
```

Four orphan branches from two small workflow runs (three nodes + one node). Every writing
chat run leaks one too. A delegation feature that fans out to eight workers per invocation
adds eight refs per run to the user's `git branch` output, and `reconcile_on_startup`
explicitly "never deletes anything".

### F15.6 — There is no merge-back. The only handoff is `proposed_patch` → `repository.test` — class (a)

No `git merge`, `git cherry-pick`, or branch-land path exists anywhere outside
`worktrees.rs:287`'s `merge-base --is-ancestor` *check*. What does exist:

* An agent node declaring `outputs: [proposed_patch]` has its worktree diff captured
  server-side into a content-addressed artifact and posted to the blackboard
  (`workflow_exec.rs:923-980`, `:1289`, `:2053`).
* A downstream `repository.test` node resolves the most recent live `proposed_patch` and
  `git apply`s it into *its own fresh worktree* before running the suite
  (`workflow_exec.rs:1571-1640`), parking for approval first because it is executing an
  untrusted change.

That is a genuinely good one-writer→one-verifier handoff. But there is no step that
consolidates **two or more** workers' patches, no conflict handling, and no way to land
anything into the user's checkout or base branch. `resolve_proposed_patch`
(`workflow_exec.rs:1727`) takes *the most recent* item — so a fan-out of three
implementers silently verifies one of them. The reference manifest is linear
(`inspect → patch → verify → review → publish`) and its terminal step is
`github.update-pull-request`, i.e. the result leaves via GitHub or not at all.

### F15.7 — The CLI cost renderer drops `cost_micros`, so worker spend can never reach a board — class (b)

`crates/cli/src/commands.rs:1561-1579` (`render_cost`) reads `wall_time_secs` and
`tool_calls`. It never looks at `cost_micros`. So even for an operator who enables routing
and benches their models — the *only* configuration in which a node cost is ever
populated — `codypendent workflow watch` still prints:

```
  w1: completed · 0s · 0 tool calls
```

The producer chain (`ModelUsage` → `node_cost_micros` → `NodeCost::to_json` →
`workflow_nodes.cost_json` → `WorkflowNodeView.cost` → the wire) is complete and
correct; the final consumer discards the field. Combined with the missing tokens
dimension (above), outcome 15's "every worker's cost lands on the board" currently has
**no** money figure and **no** token figure at any surface.

### F15.8 — Agent-profile `permissions`, `tools`, `skills`, and `completion` are parsed and never enforced — class (b)

`crates/workflow/src/agent.rs:70-84` defines all four. `resolve_agent`
(`workflow_exec.rs:679-712`) reads exactly three things: `mode`, `model_policy` (which
F15.4 then discards), and `budget`. Nothing in `crates/codypendentd` or
`crates/daemon` ever reads `AgentPermissions`, `AgentCompletion`, `profile.tools`, or
`profile.skills`. A worker profile declaring `tools = ["read_file"]` gets the full
20-tool baseline; `completion` conditions are never checked. Structural least-privilege
for delegated workers is prompt-and-mode-only today.

### F15.9 — Worker outcomes reach the blackboard with full attribution; nothing headless can read them — class (b)

This half works well. I drove a worker agent to call `blackboard.post` and the row landed
with complete provenance:

```json
{"role":"implementer3","node_id":"w3","run_id":"019ff883-…","workflow_run_id":"wfrun-…"}
```

and a declared output the agent fails to produce correctly fails its node
(`workflow_exec.rs:984-992`) rather than starving dependents silently. But the only
client of `CommandBody::ReadBlackboard` is the TUI (`crates/cli/src/tui.rs:3032`,
`:3084`). There is no `codypendent blackboard` subcommand, and `workflow watch` renders
node states only. A CLI/CI user who fans out five workers can see that they completed and
cannot see what any of them found.

### F15.10 — Structural note: the conductor's lifecycle layer is genuinely good

Worth recording so the implementation phase does not rebuild it: pause/resume/cancel/
retry-from-node are all CAS-guarded against the racing drive
(`drive.rs:378-392`, `:473-489`; `conductor.rs:182-326`), the graph signature refuses a
manifest edited under a live run, `Running`/`WaitingApproval`/`Blocked` nodes are reset
on recovery with the right semantics each, and a failed node blocks only its dependents.
Startup recovery (`conductor.rs:135-175`) counts every disposition. The durability story
is done; it is the *concurrency* story that is missing.

---

## What outcome 15 still needs — precise list

1. **Concurrent frontier execution.** `drive.rs:423` must fan the ready set out with a
   bounded `JoinSet`, bounded by `budget.maximum_agents` (F15.1, F15.2). `NodeExecutor`
   is already `Send + Sync` and each node already mints its own run id and worktree, so
   the executor leaf needs no change. The store's `transition_node` writes are per-node
   and the pool is shared — check for SQLite write contention under concurrency.
2. **Enforce `maximum_agents`** as the concurrency semaphore that step 1 introduces.
3. **Call `validate_references` at `StartWorkflow`, at `workflow validate`, and in the
   agent-facing `validated_manifest`.** The registry snapshot must include only tools that
   have a workflow tool-node executor, not the whole runtime tool list, or the check will
   pass names that still cannot run.
4. **Widen the tool-node dispatch** (`workflow_exec.rs:1408`) or make "no executor" a
   compile-time rejection. At minimum a `git.apply_patch`-style land step is needed for
   any merge story.
5. **A merge/consolidate node kind.** Today `resolve_proposed_patch` takes the newest
   patch; fan-out needs an N-patch consolidation with declared conflict behaviour, and a
   way to land the result into the base branch or the user's checkout.
6. **Branch cleanup in `release`** (`worktrees.rs:376`), guarded by the same
   `merge-base --is-ancestor` test `allocate` already uses, so a leak stops accumulating
   without ever discarding unmerged work.
7. **A tokens dimension on `NodeCost`**, and a `cost_micros` path that works with routing
   off — the chronicle already carries measured tokens per run; the node executor just
   does not lift them. Then teach `render_cost` (`cli/src/commands.rs:1561`) to print both.
8. **Honour `model_policy` per node** (`workflow_exec.rs:311`) — otherwise cheap-worker /
   expensive-synthesizer topologies, which are the point of delegation, are unreachable.
9. **A headless blackboard read** (`codypendent blackboard <workflow-run-id>` or a
   `workflow watch --outputs` flag) so a delegating user can see what the workers found.
10. **Enforce agent-profile `tools`/`permissions`** if delegated workers are meant to be
    least-privilege rather than mode-restricted only.
11. **Council/workflow bridge** (this is the "on 5,6,10" part of outcome 15): a council is
    not reachable as a node kind, and a council result is not a blackboard artifact
    (F6.7). If outcome 15's task graph should be able to convene a council as one node,
    that node kind and an artifact-shaped council result do not exist yet.

---

## What I could not exercise, and why

* **A real provider.** Every model call in this report went to a local stub speaking the
  OpenAI chat-completions shape. Provider-specific failure modes (rate limits, tool-call
  formats, refusals, Anthropic/ACP transports) are untested here. ACP-backed council
  members in particular went unexercised — `run_pinned` treats them identically at the
  protocol level, but the ACP launch path (`executor.rs:983`) is a different branch.
* **Routing-on cost.** Populating `cost_micros` needs `routing.toml` with
  `enabled = true` *and* benched model profiles produced by `codypendent models bench`,
  which requires a real local model. I therefore proved F6.2/F15.7 by reading the
  producer/consumer chain rather than by observing a dollar figure fail to print. The
  `render_cost` half (F15.7) is unconditional and needs no configuration to confirm.
* **Approval-gated tool nodes.** `approval: always` and the patch-apply park block on the
  durable approval broker, which needs an attached controller client to grant. I read the
  park/resume/cancel logic (`workflow_exec.rs:1584-1625`) but did not drive a grant.
* **`github.update_pull_request` nodes** — needs a GitHub token and a live PR.
* **Multi-worker patch consolidation** — my stub agents do not edit files, so I could not
  produce two competing `proposed_patch` artifacts to watch `resolve_proposed_patch` pick
  one. The single-patch path (F15.6) is read from source; the "most recent wins" behaviour
  is explicit in the code and its doc comment.
* **Crash recovery of a live workflow.** I killed the daemon mid-run once (to clear a
  runaway loop caused by my own stub) but did not construct a clean interrupt-and-restart
  test of `recover_incomplete`.
* **TUI council flows.** The council builder/result widgets exist
  (`crates/tui/src/render.rs:8149-8520`, `state.rs:1093-1120`) and I read the render path
  to answer "does the user ever see the members' answers", but driving the TUI belongs to
  the TUI vertical.

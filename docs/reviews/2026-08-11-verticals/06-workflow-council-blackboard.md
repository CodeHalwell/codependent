# Agent report: Workflow DAG / Council / Blackboard

## Council: traced flow + reality check

**Where it lives.** Councils are entirely a **CLI-side feature** — `crates/cli/src/council.rs` (869 lines). Zero daemon/protocol/codypendentd involvement beyond ordinary sessions: no `council` grep hits in `crates/daemon/src`, `crates/codypendentd/src`, or `crates/protocol/src`.

**Traced flow.** Definitions persist in `<config>/councils.toml` (atomic 0600 write, `council.rs:645-674`), schema-versioned, ≤64 councils / 2–8 members / 1–3 rounds (`council.rs:28-31`). `create` parses `MODEL=ROLE` args (`parse_member`, `council.rs:96-112`); validation checks name charset, unique member models, and that every member+chair id exists in `models.toml` (`validate_definition`, `council.rs:544-585`). `run` (`council.rs:245-316`): per round, every member is spawned in parallel via `JoinSet` (`deliberate_round`, `council.rs:318-376`). Each member = **fresh daemon session**: `CreateSession` → `AttachSession` (Controller, `SessionSummary`+`AgentActivity`) → `StartRun` with `mode: Ask` and pinned `model: Some(ModelId)` (`run_pinned`, `council.rs:378-451`) — so native + ACP models use the identical durable run path. Text is collected from `ModelStreamDelta` until `RunCompleted` (`collect_run`, `council.rs:453-484`), bounded at 64KB, 600s timeout with `CancelRun` on expiry. Rounds ≥2 feed the prior round's dossier back for critique (`member_prompt`, `council.rs:486-512`). Quorum: ≥2 successes per round or the whole run bails (`council.rs:360-367`). Finally the chair model synthesizes the dossier with an explicit "Preserve material dissent… do not decide by majority vote" prompt (`synthesis_prompt`, `council.rs:514-522`), and the outcome prints (JSON or sanitized text) with full attribution — each member's model/role/session/run id (`council.rs:297-315`).

**Reality check.** It works and is tested, but it is a batch CLI command: (a) **not invocable from TUI chat** — the TUI has a polished multi-step *builder* (palette `Council` → `CouncilBuilderState`, `tui/src/state.rs:99-141`; persisted via the same `persist_definition`, `cli/src/tui.rs:1493-1530`) but **no way to run one**; (b) no streaming of member output (only `eprintln` progress lines); (c) **no durability** — a crash mid-run loses everything (councils don't reuse the workflow engine at all); (d) no cost tracking; (e) no transcript artifact (member sessions persist but are unlabeled litter); (f) members are told "Do not invoke tools" and run in `Ask` mode, so a council **cannot ground itself in the repo** despite taking `--repo`.

## Workflow DAG engine: reality check (used where?)

**Genuinely alive, not dormant.** Full pipeline: YAML manifest (`model.rs`) → `compile` (unique ids, one action/step, acyclic via Kahn's, budget sanity, blackboard-kind outputs, `${{ inputs.* }}` binding checks, ADR-008 orchestration reason — `compile.rs:369-512`) → registry cross-check (`registry.rs`, `compile.rs:176-213`) → durable store (migrations 0010-0013) → `WorkflowDriver` (frontier scheduling, per-policy retry w/ persisted attempt counts, budget `Blocked`→run `Paused`, CAS state writes closing pause/cancel races — `drive.rs:283-716`) → `WorkflowConductor` (drive/recover/pause/resume/retry-from-node/cancel — `conductor.rs`) → `WorkflowConductorHost` (per-run drive locks, idempotent `StartWorkflow`, T9 event publication — `codypendentd/src/workflows.rs:230-643`) → `AgentLoopNodeExecutor` (real leaf: per-node session/run, role→`agent.toml` profile→policy-enforced mode, isolated worktrees, routed model + measured-cost honesty, server-side `proposed_patch` capture, declared-output harvest, approval parking with cancellation-safe machinery — `workflow_exec.rs:754-1018,1311-1971`).

**Consumers:** `/fix-ci` (`cli/src/main.rs:133`, built-in `repair-github-check` embedded via `source.rs:53`), `codypendent workflow validate/show/run/pause/resume/retry/cancel/watch` (`main.rs:378-451`), and the TUI workflow view with full lifecycle controls. ~60+ tests including end-to-end `/fix-ci` with scripted models (`workflow_exec.rs:4381+`).

**Dormant pieces:** checkpoints — `record_checkpoint` is never called in production (only defined at `store.rs:550` + tests); `WorkflowStore::resume`/`ResumePlan`/`ready_nodes` (pool variant) are unused by the conductor path. Frontier execution is **sequential** (`drive.rs:33`, "concurrent execution… a later refinement") despite `maximum_agents` — fan-out is scheduling-correct but not parallel.

**DAG visualization today:** the TUI `Pane::Workflow` is a **topological list + detail rail** (`render.rs:4348-4477`, `WorkflowNodeCard` `state.rs:760-807`), with live overlay via `Subscription::Workflow`+`ReadWorkflowRun` (`cli/src/tui.rs:1736-1797`). **No edges are drawn** — `depends_on` is a comma-joined string. The SDK ships a real `WorkflowGraphView` with a `Graph` primitive + list fallback (`sdk/ui/src/first-party/workflow.tsx:44-90`), but nothing instantiates it with live data and no host renders "Graph" (no hits in `ui-host`/`remote_ui_host.rs`), so even a plugin would get the list fallback. For the **agent**: no tool exists to query workflow-run graph state. **Code-context DAG:** the code graph has only a flat *edge inspector* (`Pane::Edges`); the workflow viewer and codegraph share nothing today, though `CompiledWorkflow`'s serialized JSON projection (`compile.rs:128-147`) was explicitly designed for "a graph-view client."

## Blackboard: data model + surfaces

**Model** (`workflow/src/blackboard.rs`, table `blackboard_items` in 0010): per-**workflow-run** typed artifacts — 8 fixed kinds (`finding, hypothesis, decision, code_location, proposed_patch, test_result, document_draft, open_question`), opaque JSON payload/author/evidence, confidence, revision + `superseded_by` chains (fork-proof supersede under `BEGIN IMMEDIATE`, `blackboard.rs:180-239`), evidence **required** for claim-like kinds. No status, no assignee, no ordering, no labels, no free-form kind.

**Write path:** only workflow-node agents, via `blackboard.post`/`blackboard.query` tools (`runtime/src/tools/blackboard.rs`) — gated on `WorkflowContext` so "a plain single-agent run is never offered them" (`tools/blackboard.rs:8-10`); author identity is built server-side (`workflow_exec.rs:2083-2095`). **Read path:** `ReadBlackboard` command + per-run `Subscription::Blackboard` (`daemon/src/blackboard.rs`); deliberately **no client post command**. **TUI:** `Pane::Blackboard` — items grouped by run with kind/author/confidence/evidence/revision detail, live-merged by id (`state.rs:809-841`, `render.rs:4579`).

**Distance from Kanban:** the substrate (durable typed items, revisions, attribution, live fan-out, TUI list) is ~60% of a board, but a Kanban needs: (1) a **status/column field + assignee + ordinal**; (2) a **non-workflow-scoped board** (today every item requires an FK to a `workflow_runs` row); (3) a **client write path** (new commands `PostBlackboard`/`MoveCard`, or a synthetic "project board" run); (4) **column-grouped rendering**. NL backlog tools additionally need the blackboard tools un-gated from `WorkflowContext` so a chat agent can create/move/prioritize cards.

## Verified working

- Compile/validate with precise errors; graph signature guards resume; idempotent `StartWorkflow`.
- Crash recovery (interrupted `Running`/`WaitingApproval`/`Blocked` nodes reset correctly; re-park on restart).
- Pause/resume/retry-from-node/cancel incl. race closures.
- Budget honesty: measured wall/tool/cost only, 80% warnings, block→pause→re-block-on-resume without re-spend.
- `/fix-ci` end-to-end: investigate → patch (isolated worktree, captured diff) → verify (patch applied under approval, retry) → review → approval-gated PR update.
- Blackboard evidence discipline, supersession chains, per-run isolation, observer read over the real socket.
- Council create/list/show/remove/run with quorum, bounded prompts, attribution; TUI council builder persisting through the same validated store.
- TUI workflow view live overlay + all lifecycle controls; `workflow watch` CLI.

## Bugs & broken wiring (severity)

1. **M — Council dossier silently truncates dissent.** `dossier()` appends member sections until `MAX_DOSSIER_BYTES` then `break`s (`council.rs:524-537`); members sorted alphabetically by model (`council.rs:359`), so with long responses the chair (and next round) can silently never see later members' reports. No warning emitted.
2. **M — Council session litter.** Up to 8×3+1=25 sessions per run, never ended/archived (`council.rs:388-394`).
3. **M — Councils can't see the repo.** Members run `AgentMode::Ask` with a prompt forbidding tool use (`council.rs:506`); `--repo` is effectively decorative.
4. **L — Council validation mangles models containing `=`** (`council.rs:571` re-parses via format+parse_member).
5. **L — Chair may also be a member** (uniqueness applies to members only, `council.rs:569-575`).
6. **L — No partial output on quorum failure** — `bail!` with no transcript/save (`council.rs:271-273`).
7. **L — Run-phase publish precedes persistence** in `spawn_drive` (documented Finding C, `codypendentd/src/workflows.rs:286-293`).
8. **L — Checkpoint schema is dead weight** (`workflow_checkpoints`, `record_checkpoint` unreferenced in production).
9. **L — Per-node model policy is provenance-only** (`workflow_exec.rs:303-308`) — manifest `model_policy` doesn't change routing.

## Gaps vs rubrics #5/#6/#10

**#5:** Half-built on the workflow side: a live, controllable node *list* — not a rendered DAG; `WorkflowGraphView` exists in SDK but orphaned. **Agent access absent**: no `workflow.query` tool. **Code-context half furthest behind**: flat edge inspector only.

**#6:** Functionally real but confined to a one-shot CLI print. Missing: run-from-TUI/chat, streaming member progress, durability, transcript/report artifact, cost accounting, evidence-grounded members, truncation-safe synthesis.

**#10:** Blackboard strong within workflow runs; strictly run-scoped, client-read-only. **Kanban: 0%. NL backlog tools: 0%.**

## Prioritized opportunities (S/M/L, impact)

1. **S, high — Fix council dossier truncation** (per-member share of byte budget + explicit `[truncated]` markers) + warn when chair==member.
2. **S, high — `/council` run from TUI chat**: stream member completions as notices; render chair synthesis into transcript.
3. **S, high — Agent-facing `workflow.query` tool**: wrap `WorkflowStore::snapshot` → `WorkflowNodeView` projection (`workflows.rs:775-807`) as a runtime tool.
4. **S, med — Council hygiene**: archive member sessions post-run; save a council report as a content-addressed artifact.
5. **M, high — ASCII DAG edges in `render_workflow`**: layered box-drawing render from `depends_on`/`dependents`/`topo_order`.
6. **M, high — Kanban on the blackboard**: migration for `status`/`assignee`/`ordinal`, `PostBlackboard`/`UpdateBlackboard` client command, synthetic per-repository "board run", column-grouped TUI pane.
7. **M, high — NL backlog tools**: un-gate `blackboard.*` for chat agents against the repository board; add `task.create/move/prioritize` tools.
8. **M, med — Councils as workflows**: compile a council definition into a fan-out/fan-in manifest (N agent nodes → chair node), gaining durability, budgets, cost, TUI visibility, blackboard-attributed member reports. Highest-leverage unification.
9. **L, high — Parallel frontier execution** under `maximum_agents`.
10. **L, med — Wire `WorkflowGraphView`**: implement a `Graph` renderer in the remote-UI host; reuse for codegraph neighborhood view — one viewer, both rubric-5 halves.

## Extra ideas

- Budget pre-flight in `workflow show` (render envelope + per-role slices).
- Council "evidence mode": members run `Explore` (read-only tools) with per-member tool budgets; chair cites file:line evidence.
- Blackboard `history()` supersession-chain drill-down in TUI (implemented, unexposed).
- Reuse `WorkflowHub` for a board-wide feed (`board:<repo>`).
- `council run --json | workflow run -`: chair's "next actions" seed backlog cards (bridges #6 → #10).

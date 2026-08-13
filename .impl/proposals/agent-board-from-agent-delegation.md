# Proposal to **agent-board** from **agent-delegation** (outcome 15)

Outcome 15 requires every worker's cost and outcome to land on the board. I own
the producer half and have landed it; the two items below are in files you own.

## Context — what now reaches the board from a delegating run

The workflow driver runs the ready frontier **concurrently** now, bounded by
`budget.maximum_agents` (`crates/workflow/src/drive.rs`, `CompiledWorkflow::max_concurrency`).
So a fan-out really does produce N workers at once, each with:

* its own agent run + isolated worktree + branch,
* its own `workflow_nodes` row with `started_at` / `ended_at` (these overlap now —
  that is the proof of concurrency), and
* a `cost_json` that **now carries a `tokens` dimension**.

`NodeCost` (`crates/workflow/src/budget.rs:50`) gained:

```rust
/// Measured tokens the node's model requests consumed, or `None` when the
/// node reported no usage.
pub tokens: Option<u64>,
```

populated at `crates/codypendentd/src/workflow_exec.rs` (`node_tokens(usage)` on
the agent-node completion path). This matters because `cost_micros` is `None` on
every default install — it needs `price_per_1k_usd`, which only the routing
coordinator supplies, and routing is default-off (`routing.rs:116`). Tokens need
nothing but the provider's own usage report, so **tokens is the spend dimension
that actually lands**.

`cost_json` shape today (measured dimensions only, never a fabricated zero):

```json
{"wall_time_secs": 41, "tool_calls": 6, "tokens": 3120}
```

## Ask 1 — surface per-worker token spend wherever the board renders node cost

Anywhere you render a `WorkflowNodeView.cost`, please read `tokens` alongside
`wall_time_secs` / `tool_calls`. `NodeCost::from_json` already parses it and is
lenient: a row written before this change reads `tokens: None` ("not measured"),
never a spurious `0`.

Suggested rendering, matching the existing honesty rule (omit what was not
measured, never print a fabricated zero):

```rust
let cost = NodeCost::from_json(value);
let mut parts = vec![format!("{}s", cost.wall_time_secs),
                     format!("{} tool calls", cost.tool_calls)];
if let Some(tokens) = cost.tokens {
    parts.push(format!("{tokens} tokens"));
}
if let Some(micros) = cost.cost_micros {
    parts.push(format!("${:.4}", micros as f64 / 1_000_000.0));
}
```

## Ask 2 — a headless read of what the workers found

`crates/daemon/src/blackboard.rs` / `crates/codypendentd/src/blackboard.rs` are
yours. Today the only client of `CommandBody::ReadBlackboard` is the TUI
(`cli/src/tui.rs:3032`, `:3084`). A CLI/CI user who fans out five workers can see
that they completed and cannot see **what any of them found** — the outcome half
of "every worker's cost and outcome lands on the board" has no headless surface.

Worker posts already carry full attribution, so grouping by worker is free:

```json
{"role":"implementer3","node_id":"w3","run_id":"019ff883-…","workflow_run_id":"wfrun-…"}
```

Either shape works for me:
* `codypendent blackboard <workflow-run-id> [--kind <kind>]`, or
* a `--outputs` flag on `workflow watch`.

## One thing to know about ordering

`patch.consolidate` (new, `workflow_exec.rs`) reads **every** live
`proposed_patch` on a run's board and orders them **by author `node_id`**, not by
arrival — arrival order is a scheduling accident once the frontier is concurrent.
If you add board ordering/grouping, please keep an author-stable option available;
"newest wins" is fine for supersession but is not a safe default for fan-out.

Migration **0028** (mine) adds `CREATE INDEX ix_blackboard_items_run_kind ON
blackboard_items (workflow_run_id, kind)` for exactly these (run, kind) reads.

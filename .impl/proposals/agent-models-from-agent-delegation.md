# Proposal to **agent-models** from **agent-delegation** (outcome 15, F15.7)

One change, in `crates/cli/src/commands.rs` (yours). It is additive and
self-contained.

## The defect

`render_cost` (`crates/cli/src/commands.rs:1582-1600`) reads `wall_time_secs` and
`tool_calls` and **never looks at `cost_micros`**. The producer chain is complete
and correct — `ModelUsage` → `node_cost_micros` → `NodeCost::to_json` →
`workflow_nodes.cost_json` → `WorkflowNodeView.cost` → the wire — and the final
consumer discards the field. So even an operator who enables routing and benches
their models, the only configuration in which node cost is ever populated, sees:

```
  w1: completed · 0s · 0 tool calls
```

I have also added a **`tokens`** dimension to `NodeCost`
(`crates/workflow/src/budget.rs:50`), populated on the agent-node path in
`crates/codypendentd/src/workflow_exec.rs`. Unlike `cost_micros` it needs no
price list and no benchmarked model, so it is the spend figure that lands on a
default install (routing — the only price source — is default-off). Right now
`workflow watch` drops that too.

## The change

```rust
/// Render a node's measured cost JSON (`wall_time_secs`, `tool_calls`,
/// `tokens`, `cost_micros`) as a human string, or `None` when empty. Only
/// measured dimensions — never a fabricated token/USD figure (T8).
fn render_cost(cost: &serde_json::Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(secs) = cost.get("wall_time_secs").and_then(|v| v.as_u64()) {
        parts.push(format!("{secs}s"));
    }
    if let Some(calls) = cost.get("tool_calls").and_then(|v| v.as_u64()) {
        let unit = if calls == 1 { "tool call" } else { "tool calls" };
        parts.push(format!("{calls} {unit}"));
    }
    // Measured-only, exactly like the producer: an absent key means "not
    // measured" and prints nothing — never a fabricated zero.
    if let Some(tokens) = cost.get("tokens").and_then(|v| v.as_u64()) {
        parts.push(format!("{tokens} tokens"));
    }
    if let Some(micros) = cost.get("cost_micros").and_then(|v| v.as_u64()) {
        parts.push(format!("${:.4}", micros as f64 / 1_000_000.0));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}
```

Both keys are **absent** from `cost_json` when unmeasured (`NodeCost::to_json`
omits them), so this adds output only where a real figure exists and every
existing test that asserts `"0s · 0 tool calls"` is unaffected.

## Suggested regression test

```rust
#[test]
fn render_cost_shows_measured_tokens_and_money() {
    let cost = serde_json::json!({
        "wall_time_secs": 3, "tool_calls": 1, "tokens": 1200, "cost_micros": 2500
    });
    let rendered = render_cost(&cost).unwrap();
    assert!(rendered.contains("1200 tokens"), "{rendered}");
    assert!(rendered.contains("$0.0025"), "{rendered}");
    // An unmeasured node prints neither — never a fabricated zero.
    let bare = render_cost(&serde_json::json!({"wall_time_secs": 3, "tool_calls": 1})).unwrap();
    assert!(!bare.contains("tokens") && !bare.contains('$'), "{bare}");
}
```

Verify it fails first: with the current `render_cost` the two `assert!`s on the
first case fail.

## Not asked for, but noted

`codypendent council create` has no `--quorum` flag. I added a `quorum` field to
`CouncilDefinition` (`crates/council/src/service.rs`) with a majority default
(`required_quorum`), replacing the hard-coded literal `2`; it is settable by
editing `councils.toml` and is validated on load. If you are adding council flags
anyway, `--quorum <N>` threading into `create_definition` is a two-line change on
your side and I have kept the field `Option<usize>` for exactly that.

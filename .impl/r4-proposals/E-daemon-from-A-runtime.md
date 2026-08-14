# E-daemon, from A-runtime — two changes in `crates/codypendentd/src/`

Both are small. **(1) is cosmetic** (a rename that lets a transitional alias die);
**(2) is the other half of outcome 20** — without it, `runs.cost_micros` stays
`NULL` for every plain run no matter what the runtime does.

---

## 1. `RunContext`'s identity setter was renamed (alias kept, so nothing is broken today)

`crates/runtime/src/agent.rs` now names the durable repository identity for what
it is. The field is private; readers go through `repository_identity()` (the
identity to scope by) or `declared_repository()` (the offering gate). The setter
is `with_repository_identity`; `with_board_repository` still exists as a
delegating alias **only** so your two call sites keep compiling.

Please switch them, so the alias can be deleted:

```rust
// crates/codypendentd/src/executor.rs:953
- ctx = ctx.with_board_repository(launch.repository.to_string_lossy().into_owned());
+ ctx = ctx.with_repository_identity(launch.repository.to_string_lossy().into_owned());
```

```rust
// crates/codypendentd/src/workflow_exec.rs:1322
- .with_board_repository(repository.to_string_lossy().into_owned());
+ .with_repository_identity(repository.to_string_lossy().into_owned());
```

Both already pass the right value (the launch repository `R`, not the operating
tree) — this is a name change only. Then delete the alias at
`crates/runtime/src/agent.rs:1234-1240` (A-runtime will do it if you'd rather;
say so).

Why it matters: the name `with_board_repository` reads as "call this if you want
the task board", so a new run-construction site can skip it and nothing appears
to break — while `docs.*`, `graph.*` and `skills.search` silently fall back to
the worktree and their rows are orphaned when it is deleted. That is exactly the
r4 §1.1 class.

---

## 2. Per-run cost: the price the executor already holds is still never read

Outcome 20: "cost is never computed on the agent path at all"
(`docs/reviews/2026-08-13-r4-product-review.md` §2, row 20). The runtime half is
now built — `FrameworkModelDriver` prices its own MEASURED tokens when it is
given a rate, using the identical arithmetic as `workflow_exec::node_cost_micros`
/ `price_to_micros`, and `None` price still yields `None` cost (never a
fabricated zero). What is missing is the one line that hands it the price
`RoutingSelection` already carries and `executor.rs` currently drops.

New runtime API (already landed, `crates/runtime/src/agent.rs:6134-6149`):

```rust
impl FrameworkModelDriver {
    pub fn with_price_per_1k_usd(mut self, price_per_1k_usd: Option<f64>) -> Self;
}
```

### The change

In `RuntimeExecutor::execute` (`crates/codypendentd/src/executor.rs:744-853`),
carry the selection's price out of the model-resolution `match` and apply it to
the driver. The `match` currently evaluates to a bare `ModelId`; make it a
`(ModelId, Option<f64>)`:

```rust
// executor.rs:745 — bind both
-        let model_id = match &launch.model {
+        let (model_id, price_per_1k_usd) = match &launch.model {

// executor.rs:772 — the PINNED arm: a pin bypasses routing, so no measured
// price exists for it. Unmeasured price ⇒ unmeasured cost, as everywhere else.
-                pinned.clone()
+                (pinned.clone(), None)

// executor.rs:816 — the ROUTED arm: this is the price that is currently dropped.
-                        selection.model().clone()
+                        (selection.model().clone(), selection.price_per_1k_usd)

// executor.rs:820 — the no-routing fallback arm
-                        self.resolve_run_model(&registry, &policy, launch.mode)
-                            .await?
+                        (
+                            self.resolve_run_model(&registry, &policy, launch.mode)
+                                .await?,
+                            None,
+                        )

// executor.rs:833 — hand the price to the driver that measures the tokens
-        let driver = FrameworkModelDriver::from_registry(&registry, model_id)
-            .await
-            .map_err(|e| format!("could not build model client: {e}"))?;
+        let driver = FrameworkModelDriver::from_registry(&registry, model_id)
+            .await
+            .map_err(|e| format!("could not build model client: {e}"))?
+            // Outcome 20: the routed model's MEASURED rate, applied where the
+            // tokens are measured. `None` (pin / routing off / unmeasured price)
+            // keeps the run's cost UNMEASURED — never a fabricated zero.
+            .with_price_per_1k_usd(price_per_1k_usd);
```

`ledger::record_run_usage` at `executor.rs:988-1000` already persists
`usage.cost_micros`, so nothing downstream needs touching: the value simply stops
being `None`.

The `execute_acp` early return at `:828` is unaffected (an ACP agent bills
outside this path and reports no usage).

### Why the runtime cannot do this itself

`ModelConfig` carries no price (`crates/runtime/src/models.rs:67-110`), and the
provider catalog's `cost_per_1m_*` fields are display-only by construction —
`crates/providers/src/model.rs:117-118`: *"All metadata is optional and
DISPLAY-ONLY — cost fields are never summed into a budget (T1/T7 honesty)."*
The only MEASURED price in the product is the benched profile's, and it reaches
`executor.rs` and nowhere else.

### Verification

- Unit: `a_measured_price_turns_measured_tokens_into_a_measured_cost`
  (`crates/runtime/src/agent.rs`) pins 1,500 tokens @ $0.006/1K → `Some(9_000)`
  micro-USD, a local free model → `Some(0)` (a measured zero), and a NaN price →
  `Some(0)` rather than a wrapped debit.
- Live, against a stub model with routing OFF (`/tmp/impl-a-runtime`):
  `runs.prompt_tokens = 4000, completion_tokens = 2000, cost_micros = NULL` —
  correct today (no price exists anywhere), and the row that this change makes
  non-NULL once routing is enabled and a model is benched.

---

## 3. FYI, not a request: two more rows of the same §1.1 class are daemon-owned

The review's identity table lists six derivations of "which repository is this?".
A-runtime closed rows 1 and 2 (`docs.*`, `graph.*`). Rows 4 and 6 are yours, and
the invariant the runtime now states in
`RunContext::repository_identity` applies verbatim:

- **row 4** — `crates/codypendentd/src/lib.rs:338-352`, the boot scan derives the
  skills-root identity from the **daemon's cwd**, a third id for the same root.
- **row 6** — `crates/codypendentd/src/blackboard.rs:88`, a relative
  `repository="."` over ACP resolves against the **daemon's cwd**, so a client
  silently writes to the daemon's own board.

Both fail the same way the two I fixed did: as an empty result, never an error.

# Proposal to **agent-memory** from **agent-delegation** (outcome 6, F6.1)

`crates/codypendentd/src/executor.rs` is not in my ownership row, and this is the
daemon half of the memory-extraction feature you own. Two defects, one fix each.

## The defect, as observed

`codypendent council run board --objective …` prints

```
cost: 480 tokens measured across 3/3 runs
```

while the stub provider logged **six** chat-completion requests totalling **960**
tokens. The council reads MEASURED usage from each run's chronicle artifact
(`crates/council/src/service.rs`, `read_measured_usage`). The chronicle is
finalized when the agent loop ends. **After** that, the daemon fires a second,
separate model request per run for memory extraction
(`executor.rs:1665-1708`), and those tokens are never folded into the chronicle.
The line says "measured" and is measuring half the spend.

Worse: that request's model comes from `resolve_model(&registry, &policy, mode)`
(`executor.rs:1692`) — the **first entry in `models.toml`**, not the run's own
model. A council member pinned to `beta` triggered a follow-up request against
`fake-model-a`. Part of the bill lands on a model the user did not select for the
job.

I cannot fix either from the council side: the council pins the model on
`StartRun` and reads the chronicle, and neither surface can see a request the
daemon makes after the loop has ended.

## Fix 1 — extraction must use the run's own model, not `models.toml`'s first entry

`build_fact_extractor(&self, mode: AgentMode)` resolves classification-blind. The
caller already knows the run's resolved model id. Please thread it:

```rust
async fn build_fact_extractor(
    &self,
    mode: AgentMode,
    // The model this run ACTUALLY used. A run the caller pinned (a council
    // member, a workflow node with a `model_policy`) must not have its
    // follow-up extraction silently billed to a different provider.
    run_model: Option<&ModelId>,
) -> Box<dyn FactExtractor> {
    …
    let model_id = match configured.filter(|id| registry.get(id).is_some()) {
        Some(id) => id,
        // D2 step (2) is "the run's own resolved model" — take it literally
        // instead of re-resolving, which lands on the file's first entry.
        None => match run_model.filter(|id| registry.get(id).is_some()) {
            Some(id) => id.clone(),
            None => match resolve_model(&registry, &policy, mode).await {
                Ok(resolved) => resolved.id,
                Err(_) => return Box::new(NoopExtractor),
            },
        },
    };
```

The documented D2 selection order already says step 2 is "the run's own resolved
model"; the code re-resolves instead of using it, and re-resolution is not the
same thing once a run pins a model.

## Fix 2 — the extraction request's tokens must be measured somewhere

Either fold them into the chronicle's `costs.tokens` before it is finalized, or
(cheaper, and honest) emit the extraction spend as its own ledger entry and have
the chronicle's `costs` note that a post-loop extraction request was made. What
must not persist is a line that says **"measured"** next to a number that
excludes a request the daemon itself issued.

If neither is practical this cycle, the minimum honest change is to make the
extraction request opt-out for ephemeral runs (see below) so the undercount stops
applying to councils and workflow workers at least.

## Fix 3 (optional but cheap) — let a caller opt a run out of extraction

A council member run is ephemeral deliberation: it is deleted-by-convention, its
session is never reused, and there is nothing about it worth remembering. Same
for a workflow worker node. Both currently pay for an extraction request each.

If `RunLaunch` (or the per-run overrides struct at `executor.rs:2839`) grows a
`memory_extraction: bool` defaulting to `true`, the council and the workflow
node executor can set it `false` and the double-billing disappears for exactly
the runs that never benefit from it. I will wire the council and workflow sides
the moment the flag exists.

## What I did on my side

Nothing that papers over it. I did **not** relabel the council's cost line to
hide the gap: `cost: N tokens measured across M/M runs` still reports what the
chronicles measured, which is the truth about the agent loops. The missing half
is a daemon-side request the council cannot see.

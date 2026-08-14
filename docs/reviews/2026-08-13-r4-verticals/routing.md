# Routing vertical — round 4 review

**Reviewer:** routing · **Date:** 2026-08-13 · **Pinned commit:** `c255bec8b175d62942b3312cff2335b97d43a59a`
**Outcome owned:** 11 — *Live measured routing* (builds on outcome 3)

> "The harness picks the model per task from measured latency, cost and success
> rate rather than a static default, and shows the user why it chose what it
> chose. Measurements come from real runs, never from the catalog's advertised
> numbers."

---

## 0. How this was reviewed

Every file in the vertical was read in full: `crates/routing/src/{lib,router,profile,
classify,policy,capability,arms}.rs`, `crates/routing/tests/route_and_escalate_it.rs`,
`migrations/0014_model_profiles.sql`, `migrations/0025_routing_outcomes.sql`,
`crates/daemon/src/model_profiles.rs`, `crates/codypendentd/src/{routing,
routing_outcomes}.rs`, the router call sites in `crates/codypendentd/src/{executor,
workflow_exec}.rs`, the writeback seam in `crates/runtime/src/agent.rs`, the bench
harness in `crates/runtime/src/bench.rs`, and the `models bench` / `routing`
subcommands plus `eval --policy` in `crates/cli/src/{commands,eval,tui}.rs`.

**Then it was run.** A stub OpenAI-compatible model server
(`/tmp/review-routing/stub_server.py`, ~130 lines of Python: `/v1/models`,
SSE `/v1/chat/completions`, per-model scripted behaviour) was pointed at by a
throwaway data dir (`CODYPENDENT_DATA_DIR=/tmp/review-routing/data`). Four models
were benched, routing was enabled through the shipped CLI, the daemon was started,
~15 real runs were driven through `codypendent run --jsonl` and through the
accessible TUI in a pty, and the SQLite database was queried after each step.
Every command and its verbatim output is quoted below.

No workspace `cargo build`/`cargo test` was run. The orchestrator's
`target/debug/codypendent` and `target/debug/codypendentd` were used throughout.
Disk at finish: 23 GB free.

---

## 1. Verdict

| Claim in outcome 11 | Verdict |
|---|---|
| 1. Picks the model **per task**, on the live path a user reaches | **WORKING** — observed end to end |
| 2. Measurements come from **real runs** | **PARTIAL** — success rate yes; latency and cost never |
| 3. **Never** the catalog's advertised numbers | **BROKEN** — hosted cost is the built-in catalog's advertised price, verbatim |
| 4. **Shows the user why** | **WORKING** — visible in `run --jsonl` and in the TUI transcript |

**Outcome 11 overall: PARTIAL.** The headline defect the previous round found —
"the nine-class classifier never changes which model is picked" — is **genuinely
repaired**. That repair is real, closed-loop, and I watched it flip a live routing
decision. What remains is a different set of problems, three of which are new and
one of which is a privacy hole.

---

## 2. Claim 1 — does the router actually pick, per task, on the live path? **WORKING**

The chain is: `RuntimeExecutor::execute` → `RoutingCoordinator::select`
(`crates/codypendentd/src/executor.rs:782-823`) → `registry.check_model(selection.model())`
→ `FrameworkModelDriver::from_registry(&registry, model_id)`
(`crates/codypendentd/src/executor.rs:833`). The routed id **is** the id the driver is
built from; there is no static-default override on that path. The workflow agent-node
path is wired identically (`crates/codypendentd/src/workflow_exec.rs:411-457`).

Observed. Setup, then a run:

```
$ export CODYPENDENT_DATA_DIR=/tmp/review-routing/data
$ codypendent models bench stub-good
measured `stub-good` (persisted to model_profiles @ http://127.0.0.1:8099/v1):
  tokens/sec: 66.4
  time-to-first-token: 45 ms
  ...
  coding-eval score: 1.00
$ codypendent routing enable
routing: enabled (/tmp/review-routing/data/routing.toml)
  data_classification ceiling is undeclared — fails closed to Unknown (local-only).
  Pass --data-classification to permit hosted models.
$ codypendent daemon start
daemon started (pid 19891)
$ codypendent run --jsonl --repo /tmp/review-routing/repo \
      --objective "fix the off-by-one bug in the parser"
```

The event stream carries the decision and then the agent events attributed to the
routed model:

```json
{"body":{"type":"NoteAppended","text":"routing: selected `stub-good` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, HighestUtilityAboveThreshold); predicted_success=1.000, expected_cost_usd=0.00000, expected_latency_ms=62, utility=0.9969"}}
{"actor":{"type":"Agent","model":"stub-good"},"body":{"type":"ModelStreamDelta","text":"Stub reply from model `good`. Task acknowledged and complete."}}
```

The stub server's own request log confirms `model=good` served the request. The
router's verdict is not discarded.

### The per-task-class loop actually closes

This is the strongest single result of the review, and it directly contradicts the
previous round's headline finding (11.3, "the classifier has ZERO effect").

Two local models, both benched at `reliability 1.0`, `stub-good` measurably faster
(56 ms vs 1006 ms p50, so it wins on utility). The stub was configured so that
`stub-good` fails at the endpoint for documentation work. Five consecutive
`doc-update` runs:

```
--- run 1 ---
routing: selected `stub-good` for task-class `doc-update` ... predicted_success=1.000, ... utility=0.9972
Failed :: model driver error: ... OpenAI API error 400 Bad Request: {"error": {"message": "stub: `good` cannot do docs"}}
--- run 2 ---
routing: selected `stub-meh` for task-class `doc-update` ... predicted_success=1.000, ... utility=0.9497
Completed :: Stub reply from model `meh`. Task acknowledged and complete.
--- run 3 ---
routing: selected `stub-meh` for task-class `doc-update` ...
Completed
--- run 4 / 5: same
```

The router moved off its favourite model after **one** real failure, for **that task
class only**. The database shows why:

```
$ python3 q.py data/codypendent.db "select model_id, task_class, success, run_id from model_task_outcomes order by id"
model_id  | task_class    | success | run_id
stub-good | small-bug-fix | 1       | 019ffd50-2f3a-7921-bd28-015b38bbb720
stub-good | doc-update    | 0       | 019ffd52-3e52-7c13-8fc0-72b222c8cfbb
stub-meh  | doc-update    | 1       | 019ffd52-3f05-7493-877b-79936743d826
stub-meh  | doc-update    | 1       | 019ffd52-3fde-7852-b22c-f7f3459799e0
stub-meh  | doc-update    | 1       | 019ffd52-40f5-7530-9d2e-b32cd57f564d
stub-meh  | doc-update    | 1       | 019ffd52-41d3-7b31-9cd1-7d21e155f8ec
(6 rows)

$ python3 q.py data/codypendent.db "select model_id, json_extract(profile_json,'$.performance.task_class_success') from model_profiles"
stub-good | {"doc-update":0.0,"small-bug-fix":1.0}
stub-meh  | {"doc-update":1.0}
```

And the classes stay independent — the very next `small-bug-fix` run still went to
`stub-good`:

```
routing: selected `stub-good` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, HighestUtilityAboveThreshold); predicted_success=1.000, ... utility=0.9972
```

`migrations/0025_routing_outcomes.sql`, `ModelProfileStore::record_outcome`
(`crates/daemon/src/model_profiles.rs:249-329`), `PoolRoutingOutcomes`
(`crates/codypendentd/src/routing_outcomes.rs:31-52`) and
`FrameworkAgentRuntime::record_routing_outcome` (`crates/runtime/src/agent.rs:2951-2985`)
are all real, wired unconditionally at `crates/codypendentd/src/executor.rs:876` and
`crates/codypendentd/src/workflow_exec.rs:1263`, and demonstrably functioning.

Two further behaviours verified as correct:

* **Re-benching does not erase learned rates.** `upsert` re-folds them from
  `model_task_outcomes` (`crates/daemon/src/model_profiles.rs:74-101`). After
  `codypendent models bench stub-good` a second time, the DB still showed
  `{"doc-update":0.0,"small-bug-fix":1.0}`.
* **Outcomes accrue with routing OFF.** With `routing.toml` `enabled = false`, a run
  emitted no routing note but still wrote `stub-hosted | safe-refactor | 1`. Evidence
  therefore builds up before an operator ever turns routing on — a good design choice.

---

## 3. Claim 2 — do the measurements come from real runs? **PARTIAL**

`ModelPerformance` has four routing-relevant numbers. Their writers:

| Field | Written by | From a real run? |
|---|---|---|
| `task_class_success` | `record_outcome` (`model_profiles.rs:282-313`) | **Yes** |
| `reliability` | `BenchOutcome::into_profile` (`bench.rs:169`) | No — one-shot bench |
| `latency_ms_p50` | `BenchOutcome::into_profile` (`bench.rs:167-168`) | No — one-shot bench |
| `cost_per_1k_tokens_usd` | `BenchOutcome::into_profile` (`bench.rs:170`) | No — see §4 |

There are exactly three SQL writers in the workspace, all in
`crates/daemon/src/model_profiles.rs` (`INSERT INTO model_profiles` :107,
`UPDATE ... probed_capabilities_json` :213, `INSERT INTO model_task_outcomes` :283 +
`UPDATE ... profile_json` :317). The last pair is the only real-run writer, and it
touches **only** `task_class_success`. Nothing anywhere folds a run's measured
latency or measured token cost back into a profile — even though
`ModelRequestTrace` already carries MEASURED per-request usage and latency
(`crates/runtime/src/agent.rs:1476-1487`) and `RoutingOutcome`
(`crates/runtime/src/agent.rs:1424-1442`) is a natural place to carry them.

So of the outcome's three named measurements — "measured latency, cost and success
rate" — one comes from real runs and two come from a single local bench whose
numbers never change again. The previous round's finding 11.1 ("one-shot benched,
never re-measured") is **still true for latency and cost**.

### 3b. `reliability` is still one prompt asked ten times, substring-scored

Unchanged from 11.2. `DriverBenchTarget::coding_eval`
(`crates/runtime/src/bench.rs:427-448`) sends the identical prompt *n* times and
scores `text.to_ascii_lowercase().contains("let")`.

I hit the fragility of that rule by accident and it is worth recording as observed
evidence: before my stub spoke SSE, the driver's SSE parser surfaced the whole raw
JSON body as assistant text. The body contained `"object": "chat.completion"` —
and `comp**let**ion` contains `let`. Both models therefore benched at
`coding-eval score: 1.00`, including the one deliberately answering `const`:

```
$ codypendent models bench stub-meh          # stub answered "const" every time
  coding-eval score: 1.00
```

After the stub was fixed to emit real SSE, the same model benched `0.00`. A single
substring is the entire basis of `predicted_success` for every task class with no
history.

### 3c. Two of the three "measured" bench numbers can never be non-zero

`DriverBenchTarget::structured_output_probe` (`crates/runtime/src/bench.rs:400-411`)
counts a pass only for `ModelStep::Say`. The real driver never produces `Say`:
`chat_response_to_step` (`crates/runtime/src/agent.rs:7059-7104`) returns only
`CallTool` or `Finish`, and `updates_to_step` (`:7122`) delegates to it. `Say` is
produced solely by `ScriptedDriver` (`crates/runtime/src/agent.rs:940`), a test
double.

Observed: all four models benched `structured-output reliability: 0.00`, including
one whose endpoint returns `{"ok": true}` for exactly that probe.

Worse, `crates/routing/src/profile.rs:130-133` documents that "the router treats a
local model's measured `structured_output_reliability`/`tool_call_accuracy`/
`coding_eval_score` as authoritative over any declared capability." The router never
reads `ModelProfile::bench` at all — the only non-test consumers of those two fields
in the entire workspace are the two `println!` arguments at
`crates/cli/src/commands.rs:3260-3261`.

---

## 4. Claim 3 — does it avoid the catalog's advertised numbers? **BROKEN**

`crates/cli/src/commands.rs:3277-3291`:

```rust
fn resolve_hosted_price(config, catalog, override_per_1m_usd) -> Option<f64> {
    if let Some(price) = override_per_1m_usd { return Some(price / 1000.0); }
    let provider_id = config.provider_id.as_deref()?;
    let row = catalog.model(provider_id, &config.model)?;
    let (input, output) = (row.cost_per_1m_input_usd?, row.cost_per_1m_output_usd?);
    Some(codypendent_runtime::bench::blended_price_per_1k_usd(input, output))
}
```

That value goes straight into `BenchOutcome::into_profile`'s
`cost_per_1k_tokens_usd` (`crates/runtime/src/bench.rs:170`), which is read by
`ModelProfile::expected_cost_usd` (`crates/routing/src/profile.rs:166-168`) and
therefore by `Router::utility`'s `λc·cost` term (`crates/routing/src/router.rs:425,
430`), by `RoutingDecision.expected_cost_usd` (`:405`), and by the hard filter that
admits hosted models at all (`crates/routing/src/router.rs:321-326`).

Observed end to end. A hosted model configured against
`provider_id = "nebius"`, `model = "deepseek-ai/DeepSeek-V4-Flash"` — a row in
`crates/providers/builtin_catalog.toml` whose advertised prices are
`cost_per_1m_input_usd = 0.14`, `cost_per_1m_output_usd = 0.28`:

```
$ codypendent models bench stub-hosted
models bench: WARNING — `http://vm:8099/v1` is not a local endpoint; ... its token price is not measured.
measured `stub-hosted` (persisted to model_profiles @ http://vm:8099/v1):
  ...
  price: $0.0002/1K tokens (blended, routable)

$ python3 q.py data/codypendent.db "select model_id, location, cost_per_1k_tokens_usd from model_profiles"
stub-hosted | hosted | 0.00021
$ python3 -c "print((0.14+0.28)/2/1000)"
0.00021
```

and then, in the decision shown to the user on a live run:

```
routing: selected `stub-hosted` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, HighestUtilityAboveThreshold); predicted_success=1.000, expected_cost_usd=0.00089, expected_latency_ms=46, utility=0.8718
```

`0.00089 = 0.00021 × 4.256` — the advertised catalog blend times a heuristic token
estimate (`estimate_input_tokens` = `len/4` floored at 256,
`crates/codypendentd/src/routing.rs:682-684`; `estimated_output_tokens(Build) = 4000`,
`:688-693`). Not one measured token is involved.

This is the fix the previous round asked for (11.4: "what is missing is the
price-entry surface that makes hosted routing reachable at all"), implemented by
reaching for exactly the source the outcome forbids. The `--price-per-1m-usd`
override is an operator-declared number, which is defensible; the silent catalog
fallback is not, and it is the default whenever `provider_id` is set — which
`codypendent models add` always sets.

What is **not** taken from the catalog: context window. `default_bench_description()`
(`crates/cli/src/commands.rs:3571-3589`) hard-codes `context_tokens: None`, so the
catalog's `context_tokens` never reaches the router — see finding R7 for the cost of
that.

---

## 5. Claim 4 — does it show the user why? **WORKING**

Producer: `render_decision` (`crates/codypendentd/src/routing.rs:722-736`) →
`emit_note` → durable `EventBody::NoteAppended` with `run_id`.

Two real consumers, both observed:

1. **`codypendent run --jsonl`** — quoted in §2.
2. **The TUI.** `EventBody::NoteAppended` folds to `TranscriptEntry::Note`
   (`crates/tui/src/reduce.rs:1542-1602`); `note_lines`
   (`crates/tui/src/render.rs:2605-2636`) renders a note at or under
   `NOTE_INLINE_LINE_THRESHOLD` lines **inline and in full** — and the routing note
   is one line, so it is never collapsed. Driven in a pty
   (`/tmp/review-routing/pty_drive.py`, `codypendent --accessible`):

```
Codypendent accessible view
Session: repo
Conversation: 1 run(s); selected 1
Run 1 selected: Completed; mode Build; objective fix the null pointer bug in the tokenizer
Model: stub-meh
You: fix the null pointer bug in the tokenizer
Backstage: 31 context line(s), 0 memory update(s)
Note: routing: selected `stub-meh` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, HighestUtilityAboveThreshold); predicted_success=1.000, expected_cost_usd=0.00000, expected_latency_ms=1006, utility=0.9497
Budget warning: Tokens 2713 of 200000
Assistant: Stub reply from model `meh`. Task acknowledged and complete.
Completed: Stub reply from model `meh`. Task acknowledged and complete.
```

The previous round's 11.8 ("no CLI command writes routing.toml") is repaired:
`codypendent routing status|enable|disable` exist and work
(`crates/cli/src/commands.rs:3384-3519`), and `enable`/`disable` correctly warn that
a live daemon has not reloaded. This is category-(b) work genuinely finished.

Caveats, both minor: there is still no routing-specific panel or `/why` command, and
the note is *not* emitted when the routed model fails `check_model` — the decision is
recorded only after that check passes (`crates/codypendentd/src/executor.rs:800-815`),
so a run that dies on "routed model X is not available" never tells the user why X
was chosen.

---

## 6. Findings, ranked by user-visible consequence

### R1 — Memory extraction bypasses the routing seam and ships the transcript off-device while routing says "local-only" — class (c)

`crates/codypendentd/src/executor.rs:1812-1826`:

```rust
let configured = RoutingConfig::load(&self.paths).memory_extraction_model;
let model_id = match configured.filter(|id| registry.get(id).is_some()) {
    Some(id) => id,
    None => match resolve_model(&registry, &policy, mode).await {   // <-- Phase-1 resolver
        Ok(resolved) => resolved.id,
        Err(_) => return Box::new(NoopExtractor),
    },
};
```

`resolve_model` is the classification-blind Phase-1 resolver — first reachable
candidate in `models.toml` order. It never consults `RoutingCoordinator`, so the
classification hard filter (the whole reason `routing.toml` fails closed to
`Unknown`) does not apply to it. Every run makes this extra model call.

**Observed.** `routing.toml` = `enabled = true` with no `data_classification`, i.e.
the documented fail-closed **local-only** posture. `models.toml` listed the hosted
model first. One run:

```
$ codypendent routing status
routing: ENABLED (/tmp/review-routing/data/routing.toml)
  data_classification ceiling: (undeclared — fails closed to Unknown, local-only)

$ codypendent run --jsonl --repo ... --objective "fix the leak in the buffer pool"
routing: selected `stub-good` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, BestEffortBelowThreshold); ...

$ jq -r '.model' stub_server.requests.log | sort | uniq -c
      1 deepseek-ai/DeepSeek-V4-Flash      <-- the HOSTED endpoint
      1 good                               <-- the routed LOCAL model
```

The daemon log agrees, and the hosted endpoint's request body contained the run's own
session ledger content:

```
WARN codypendent_runtime::extractor: memory extraction returned no parseable facts; contributing none model=stub-hosted

{"model":"deepseek-ai/DeepSeek-V4-Flash","tail_200":"ood` for task-class `small-bug-fix` via `router/balanced/1` (classifier `rules/1`, BestEffortBelowThreshold); predicted_success=0.667, ..."}
```

The router refused to let this data leave the device; the same run sent it off-device
anyway, through a second unrouted model call. `RoutingConfig::memory_extraction_model`
exists as an *optimisation* knob (put extraction on a cheap model) but is not a
security control, and leaving it unset is the default.

**Consequence:** an operator who enables routing specifically to keep classified work
on-device does not get that guarantee.

### R2 — A stale `models.toml` snapshot silently drops profiles: wrong model chosen, and a false "no model profiles exist" error — class (c) + silent filter

`RoutingConfig::load` snapshots `models.toml` **once, at daemon startup**
(`crates/codypendentd/src/routing.rs:157-164`). `eligible_profiles` then drops any
stored profile whose id is absent from that snapshot, or whose endpoint does not
match it — with no log, no warning, no user-visible signal
(`crates/codypendentd/src/routing.rs:579-585`):

```rust
if let Some(active) = &self.config.active_endpoints {
    let Some(endpoint) = active.get(&profile.id) else { continue; };      // silent
    if normalize_endpoint(&entry.endpoint) != *endpoint { continue; }     // silent
    ...
```

(The third `continue` on a location mismatch does warn, `:588`. The first two do not.)

**Observed A — a strictly better model is silently ignored.** Same DB, same
`models.toml`, same objective class, no other change but a daemon restart:

```
--- run with stub-super in models.toml, daemon NOT restarted ---
routing: selected `stub-meh` for task-class `small-bug-fix` ... expected_latency_ms=1006, utility=0.9497
--- same, AFTER daemon restart ---
routing: selected `stub-super` for task-class `small-bug-fix` ... expected_latency_ms=63, utility=0.9968
```

A user who runs `codypendent models bench <new-model>` and then starts a run keeps
getting the worse model, indefinitely, with nothing on screen or in the log to say so.
`codypendent models list` shows the new model; only routing cannot see it.

**Observed B — the error message is wrong.** Rewriting `127.0.0.1` to `localhost` in
`models.toml` (the same endpoint, a different spelling) and restarting:

```
$ codypendent run --jsonl --repo ... --objective "fix the parser crash"
codypendent: run failed — routing refused to place this run: routing is enabled but no model profiles exist; run `codypendent models bench <id>` first

$ python3 q.py data/codypendent.db "select model_id, endpoint from model_profiles"
stub-good | http://127.0.0.1:8099/v1
stub-meh  | http://127.0.0.1:8099/v1
(2 rows)
```

Two profiles exist. `RoutingSeamError::NoProfiles`
(`crates/codypendentd/src/routing.rs:220-223`) states an untruth and gives advice
that will not obviously fix it. The honest answer is "your benched profiles are
keyed to an endpoint that is no longer in `models.toml`". The daemon log carried
nothing about the filter either.

This is the brief's silent-filter pattern precisely: a `.filter`/`continue` that
drops everything and reports "not found".

### R3 — Hosted routing cost is the catalog's advertised price — class (c)

See §4 for the full chain and the observed numbers.
`crates/cli/src/commands.rs:3277-3291`.

**Consequence:** the number the user is shown as `expected_cost_usd`, and the number
the utility function trades quality against, are a vendor price sheet multiplied by a
`len/4` guess. The outcome explicitly rules this out.

### R4 — Latency and cost are never re-measured from a real run — class (b)

`record_outcome` (`crates/daemon/src/model_profiles.rs:249-329`) writes only
`task_class_success`. `ModelRequestTrace` already carries measured usage and latency
(`crates/runtime/src/agent.rs:1476-1487`) and `RoutingOutcome`
(`crates/runtime/src/agent.rs:1424-1442`) already crosses the same pool-erased seam —
the producers exist and are not connected to this consumer.

**Consequence:** a model that gets slower, or whose price changes, never updates.
`expected_latency_ms=1006` in every note above is a bench artefact from one moment,
not a running measurement.

### R5 — With routing on, pinning an unbenched *local* model is refused, with a wrong reason — class (c)

`Router::model_passes_classification` (`crates/routing/src/router.rs:301-304`) fails
closed on an unknown id. `RoutingCoordinator::validate_pin`
(`crates/codypendentd/src/routing.rs:457-467`) turns that into a run failure. But the
router's pool is `eligible_profiles()` — only *benched* models — while `models.toml`
already proves locality via `endpoint_location(base_url)`, which the pin path never
consults.

**Observed.** A model at `http://127.0.0.1:8099/v1` (loopback — provably local),
configured in `models.toml`, never benched, with the classification ceiling set to
`internal` (hosted is *permitted*):

```
$ codypendent run --jsonl --repo ... --model stub-good      --objective "fix a pinned bug"
"disposition":{"type":"Completed","summary":"Stub reply from model `good`. ..."}

$ codypendent run --jsonl --repo ... --model stub-unbenched --objective "fix a pinned bug"
"disposition":{"type":"Failed","reason":"routing refused the pinned model: routing refused: pinned model stub-unbenched may not process this run's data (classification Internal): it is a hosted/off-device model above the policy's ceiling, or it has no benchmarked profile proving it runs on-device — run `codypendent models bench stub-unbenched` or pin a local model"
```

**Consequence:** turning routing on silently converts the TUI `/model` picker and
`run --model` into "benched models only" — for every model, hosted or local, at every
classification level. The error tells a user with a loopback model to "pin a local
model".

### R6 — `codypendent routing status` can report the opposite of what the daemon is doing — class (b)

`routing_status` (`crates/cli/src/commands.rs:3384-3432`) prints the on-disk file and
never calls `daemon_is_live` (`:3484`). Only `enable`/`disable` do, via
`report_routing_change` (`:3489`). The code's own comment at `:3480-3483` says a bare
"routing: disabled" "would tell a user their data had stopped going off-device while
it was still going off-device. That is the one thing this command must not do."

**Observed** — daemon running with routing enabled; disable, then check status, then
run:

```
$ codypendent routing disable
routing: disabled in /tmp/review-routing/data/routing.toml
  the running daemon still has the PREVIOUS routing policy loaded — it reads routing.toml once at startup.
  run `codypendent daemon restart` to apply it.

$ codypendent routing status
routing: disabled (/tmp/review-routing/data/routing.toml)
  data_classification ceiling: (undeclared — fails closed to Unknown, local-only)
  policy: default (router/balanced/1)

$ codypendent run --jsonl --repo ... --objective "fix the disabled-routing bug"
routing: selected `stub-good` for task-class `small-bug-fix` via `router/balanced/1` ...
```

The guard was built and attached to two of the three commands that need it. The one
it was omitted from is the one a user runs later, from a different shell, precisely to
check the current state.

### R7 — The size hard filter is inert for every real model — class (c)

The Chapter 09 pipeline's third hard filter is "context/output size estimate (fits?)".
`Router::is_eligible` folds the node's estimates into `min_context_tokens` /
`min_output_tokens` (`crates/routing/src/router.rs:334-337`), and
`ModelCapabilities::satisfies` skips the check entirely when the declared limit is
`None` (`crates/routing/src/capability.rs:93-102`, "assumed sufficient"). The only
production constructor of a profile's capabilities is
`default_bench_description()` (`crates/cli/src/commands.rs:3571-3589`), which
hard-codes `context_tokens: None, output_tokens: None`.

**Observed** — after benching four models, including two with
`context_tokens = 200000` and one with `131072` in `models.toml`:

```
$ python3 q.py data/codypendent.db "select model_id, json_extract(profile_json,'$.capabilities.context_tokens') ctx, json_extract(profile_json,'$.bench.context_limit') bench_ctx from model_profiles"
stub-good | (null) | 0
stub-meh  | (null) | 0
```

**Consequence:** the router will happily route a 300k-token task to a 4k-context local
model; the filter that exists to prevent that can never fire. `LocalBench.context_limit`
is likewise a fabricated `0` (`TargetDescription.context_limit: 0` at
`crates/cli/src/commands.rs:3587`) and is printed to the user as
`context limit: 0`.

### R8 — `structured_output_reliability` is structurally always 0.00, and the router never reads the bench block at all — class (b)

See §3c. `crates/runtime/src/bench.rs:400-411` vs
`crates/runtime/src/agent.rs:7059-7104`; the doc claim at
`crates/routing/src/profile.rs:130-133`; the only non-test consumers at
`crates/cli/src/commands.rs:3260-3261`.

**Consequence:** an operator benching a model sees
`structured-output reliability: 0.00` for a model that produces perfect JSON, and is
told in the docs that this number is authoritative for routing when it is read by
nothing.

### R9 — Escalation, the five arms / release gate, and the capability prober remain dead outside tests — class (b), unchanged from round 3

* `RoutingCoordinator::escalate` and `record_transition` carry
  `#[cfg_attr(not(test), allow(dead_code))]`
  (`crates/codypendentd/src/routing.rs:485-486, 526-527`). The only callers are at
  `:1328` and `:1343`, inside `#[cfg(test)] mod tests` (which starts at `:777`). The
  `escalation_chain` in `routing.toml` is parsed and validated for duplicates
  (`crates/routing/src/policy.rs:196-203`) and never walked in production.
* `RouteArm`, `RouteArmResult`, `RouteEvalReport`, `meets_release_gate`,
  `gate_summary`, `route_static_strongest`, `route_static_cheap`, `route_local_first`
  have no consumer outside `crates/routing/tests/route_and_escalate_it.rs`. STEP 7.3
  exit criterion 1 is still not evaluable by any shipped path. To its credit,
  `crates/routing/src/arms.rs:19-24` now says so explicitly instead of advertising a
  command that does not exist.
* `CapabilityProber` has exactly one implementation in the workspace,
  `DenyToolsProber` at `crates/codypendentd/src/routing.rs:1372` — a test double.
  `with_prober` is never called in production, so `probed_capabilities` always
  returns the cached-or-nothing path and the declared (hard-coded) capabilities always
  win.

### R10 — Enabling routing silently disables a workflow manifest's per-node `model_policy` — class (c) — **inferred from reading, not run**

`ConfiguredModelDriverFactory::build`
(`crates/codypendentd/src/workflow_exec.rs:411-455`) resolves `node_policy` from the
node's `model_policy`, then discards it whenever routing returns `Some(selection)`;
`node_policy` is used only on the `None` (routing-off) branch at `:450`. The comment
at `:446-449` acknowledges this is deliberate ("Routing ON deliberately wins over it").

**Consequence:** the shipped `repair-github-check` manifest assigns
`economical-coding` to the investigator and `coding` to the implementer. With routing
enabled, all nodes route under the single daemon-wide `routing.toml` policy — the
delegation design (cheap workers, expensive synthesizer) becomes inexpressible again,
and nothing tells the operator their manifest's policies were ignored. A warning at
minimum, or per-node policy selection, is missing.

### R11 — `codypendent eval --policy` and the daemon route over different eligible pools — class (c), minor — **inferred from reading**

`route_cases` (`crates/cli/src/eval.rs:229-270`) hands `Router` **every** stored
profile; the daemon's `eligible_profiles` filters by the `models.toml` snapshot
(§R2). So `eval --policy` can select and pin a model the daemon's own routing would
have refused. It then pins it via `StartRun.model`, where `validate_pin` — using the
filtered pool — will refuse it. Fails closed, but the two answers to "which models
are routable" disagree, and the user sees a refusal for a model the eval harness just
told them it chose.

---

## 7. The pattern

Every finding here is the **same shape as the previous rounds', displaced by one
hop**. Round 3's verdict was "the final wire is attached to the wrong terminal, and
the fix is applied to the instance rather than to the class." Round 4's repairs fixed
the instances that were named: the per-task-class writer now exists and demonstrably
changes decisions; a `routing` CLI now writes `routing.toml`; hosted models now have a
price entry surface. But each repair stopped at the boundary of the one symptom that
was written down. The *class* — "any model call, any input to the decision, any state
the user is shown must come from, and be checked against, the same routed authority" —
was not generalised. So the router now governs the main model call but not the
extraction call on the same run (R1); it reads a measured success rate but a
catalog-advertised price and a stale bench latency (R3, R4); its eligibility pool is a
startup snapshot that silently disagrees with `models.toml`, with `eval`, and with the
pin path (R2, R5, R11); and the live-daemon caveat was wired into two of the three
commands that need it (R6). The engine is now genuinely live and genuinely learning —
that is real progress — but everything *adjacent* to the wire that was fixed is still
attached to the old terminal.

---

## 8. What I did **not** verify

* **R10 (workflow per-node `model_policy`) is read, not run.** I did not execute a
  multi-node workflow with routing enabled. The claim rests on
  `crates/codypendentd/src/workflow_exec.rs:437-455` and the module's own comment.
* **R11 (eval vs daemon pools) is read, not run.** I did not drive
  `codypendent eval --policy`.
* **No cargo tests were run** for any crate, per the brief's disk constraint. I treated
  the existing test suites as no evidence and did not consult their pass/fail state.
* **Escalation was never exercised live**, because no production path calls it — that
  is finding R9, not an omission of method.
* **All model behaviour came from a stub**, not a real LLM. That is sound for
  routing (the router never inspects response quality) but means I did not observe
  real-world latency spread, real tool-calling, or real token accounting. In
  particular `tool_call_accuracy` benched `0.00` for every model here only because my
  stub never emits a tool call; unlike `structured_output_reliability` (R8), that
  number *can* be non-zero for a real model.
* **The catalog price path was verified with one catalog row**
  (`nebius` / `deepseek-ai/DeepSeek-V4-Flash`). I confirmed the arithmetic matches that
  row exactly; I did not audit the other 176 priced rows.
* **`ModelLocation` classification of my "hosted" model** relies on `/etc/hosts`
  mapping `vm → 127.0.0.1` in this sandbox, so a genuinely off-device request was
  never made. `endpoint_location("http://vm:8099/v1")` correctly returned `Hosted`
  and the profile stored `location=hosted`, which is what every filter keys on — but
  the packet did not leave the machine.
* **I did not test the graphical TUI**, only `--accessible` in a pty. The note-folding
  logic (`crates/tui/src/reduce.rs:1542`) and renderer
  (`crates/tui/src/render.rs:2321, 2605`) are shared, and the routing note is a single
  line so it renders inline in both, but I observed only the accessible path.
* **I did not measure whether `record_outcome`'s per-run transaction adds latency** to
  run completion; it runs after the terminal event, so it cannot delay the user's
  view of completion, but I did not time it.

---

## 9. Reproduction

Scratch assets under `/tmp/review-routing/` (nothing was written into the repository
except this report):

* `stub_server.py` — the stub OpenAI-compatible model server (SSE + `/v1/models`).
* `q.py` — a read-only `sqlite3` CLI substitute (`sqlite3` is not installed here).
* `pty_drive.py` — the pty driver for the accessible TUI.
* `data/` — the throwaway `CODYPENDENT_DATA_DIR` (DB, `models.toml`, `routing.toml`,
  daemon logs).
* `*.jsonl` — the captured event streams for every run quoted above.

Final store state after the session:

```
$ python3 q.py data/codypendent.db "select model_id, endpoint, location, cost_per_1k_tokens_usd, reliability, json_extract(profile_json,'$.performance.task_class_success') tcs from model_profiles"
stub-good   | http://127.0.0.1:8099/v1 | local  | 0.0     | 1.0 | {"doc-update":0.0,"small-bug-fix":0.8}
stub-meh    | http://127.0.0.1:8099/v1 | local  | 0.0     | 1.0 | {"doc-update":1.0,"small-bug-fix":1.0}
stub-super  | http://127.0.0.1:8099/v1 | local  | 0.0     | 1.0 | {}
stub-hosted | http://vm:8099/v1        | hosted | 0.00021 | 1.0 | {"safe-refactor":1.0,"small-bug-fix":1.0}

$ python3 q.py data/codypendent.db "select model_id, task_class, sum(success) succ, count(*) n from model_task_outcomes group by 1,2"
stub-good   | doc-update    | 0 | 1
stub-good   | small-bug-fix | 4 | 5
stub-hosted | safe-refactor | 1 | 1
stub-hosted | small-bug-fix | 1 | 1
stub-meh    | doc-update    | 4 | 4
stub-meh    | small-bug-fix | 4 | 4
```

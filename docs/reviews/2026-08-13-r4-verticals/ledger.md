# Vertical review — the event-sourced ledger (outcome 20)

**Reviewer:** `ledger` · **Round 4** · 2026-08-13
**Commit reviewed:** `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1, branch
`claude/review-repair-twenty-outcomes-5fynno`) — verified with `git rev-parse HEAD`.

**Outcome under review — 20, "The ledger made visible":**

> *"Per-run cost, tokens, latency, tool-call traces, policy denials and
> classification decisions, surfaced in the TUI and the extension. The
> event-sourced ledger already holds this; it has no reader."*

**Verdict: PARTIAL.** Three of the six data classes reach a user. Two of the
three that do not — cost and tokens — are the two the outcome names first, and
one of them (tokens) is now *measured, journaled and delivered to the client*,
where the TUI renders it as the literal string **`? unsupported event`**.

---

## 0. How this was verified

Everything below marked **[observed]** was produced by running the shipped
binaries. `[read]` marks a claim taken from source only.

Harness (all scratch under `/tmp/review-ledger/`, nothing written into the repo):

* A stub OpenAI-compatible chat-completions server (`ledger_stub_srv.py`)
  that streams SSE and emits the final top-level `usage` chunk with real
  `prompt_tokens` / `completion_tokens` / `total_tokens` — the exact shape
  `agent-framework-openai`'s `parse_sse_stream` → `parse_delta` →
  `convert::parse_usage` reads. Scriptable into three modes: plain finish,
  a `workspace.read_file` tool call, and a policy-denied `shell.run`.
* An isolated data dir (`CODYPENDENT_DATA_DIR` / `CODYPENDENT_CONFIG_DIR` /
  `CODYPENDENT_SOCKET`) with a `models.toml` pointing at the stub, a scratch
  git repo, and a live `codypendentd`.
* **8 real runs** through `codypendent run --jsonl`, plus **5 more** driven
  interactively through the TUI in a pty (`pty.fork` + `pyte` screen
  emulation at 200×50), plus the accessible client, plus two workflow runs.
* The SQLite ledger read directly with python3's `sqlite3` (the `sqlite3` CLI
  is not installed on this box).

No `cargo build`/`cargo test` was run. The orchestrator's
`target/debug/codypendent` (v0.5.1+c255bec8b175) and `target/debug/codypendentd`
were used as instructed. Disk stayed at 41% used.

---

## 1. The six data classes, end to end

| # | data class | emitted on a live run? | stored? | read back? | rendered where a user sees it? |
|---|---|---|---|---|---|
| 1 | **per-run cost** | **NO** — never measured on the agent-run path | NULL in every row | n/a | **NO** — `cost: —` |
| 2 | **tokens** | **YES** — `RunUsage` | **YES** — `runs.*` + ledger event | only by a unit test; delivered live to the TUI | **NO** — `? unsupported event` |
| 3 | **latency** | partial — run wall-clock only | **YES** — `runs.started_at`/`ended_at` | one Remote-UI projection nobody consumes | **NO** |
| 4 | **tool-call traces** | **YES** | **YES** | **YES** | **YES** |
| 5 | **policy denials** | **YES** | **YES** | **YES** | **YES** |
| 6 | **classification decisions** | only when routing is enabled (default OFF) | **YES** (as a note) | **YES** | **YES** |

Each row is justified below.

---

### 1.1 Per-run cost — **BROKEN**, class (a)+(b)

**Emitted: no. Stored: no. Rendered: `—`.**

`cost_micros` is `None` for every plain agent run, by construction and in fact:

* `crates/runtime/src/agent.rs:7150-7160` — `measured_usage()`, the one place
  the live driver turns a provider response into a `ModelUsage`, hard-codes
  `cost_micros: None`:

  ```rust
  usage_details.map(|details| ModelUsage {
      prompt_tokens: details.input_token_count.unwrap_or(0),
      completion_tokens: details.output_token_count.unwrap_or(0),
      // Measured tokens, UNMEASURED cost — priced downstream where the routed
      // model's rate is known. Never a fabricated zero.
      cost_micros: None,
  })
  ```

* "Priced downstream" happens in **exactly one place**, and it is not the
  agent-run path. `node_cost_micros(price, usage)`
  (`crates/codypendentd/src/workflow_exec.rs:226-235`) is called only from
  `workflow_exec.rs:1058`. The single-agent executor
  (`crates/codypendentd/src/executor.rs:785-816`) receives a
  `RoutingSelection` that *carries* `price_per_1k_usd` and reads only
  `selection.model()` and `selection.decision` from it. `grep -rn
  "price_per_1k_usd" crates/codypendentd/src/` returns zero hits in
  `executor.rs`. **[read + observed]**

* Consequently `crates/codypendentd/src/executor.rs:989-1004` unpacks
  `usage.cost_micros` (always `None`) into `ledger::record_run_usage(...)`, and
  the DB column is written NULL.

**[observed]** All seven completed runs, with a live provider reporting real
token counts:

```
$ python3 -c "...select id, objective, prompt_tokens, completion_tokens, cost_micros from runs..."
019ffd4f-e937…  'Say hello and finish'              prompt=1234 completion=567  cost_micros=None
019ffd50-aad7…  'Read the README and summarise it'  prompt=2000 completion=88   cost_micros=None
019ffd52-1b7f…  'Read README.md then summarise'     prompt=2000 completion=88   cost_micros=None
019ffd52-5926…  'Read README.md then summarise'     prompt=4000 completion=176  cost_micros=None
019ffd52-9bc3…  'Remove stale files from the machine' prompt=6200 completion=84 cost_micros=None
019ffd52-dcfa…  'Delete stale files with rm'        prompt=6200 completion=84   cost_micros=None
019ffd5b-055d…  'Fix the small bug in src/main.rs'  prompt=7000 completion=250  cost_micros=None
```

The last row is the important one: routing was **enabled**, a benched profile
existed, and the router recorded a decision for that run — the price path was
live and cost was still not computed.

**Renderer side, unchanged from the previous round.** `run.cost_minor` has one
writer in the whole TUI: `crates/tui/src/reduce.rs:1918`,
`BudgetDimension::Cost => run.cost_minor = Some(used)`. There is **no
non-test constructor of `EventBody::BudgetWarning { dimension:
BudgetDimension::Cost, .. }` anywhere in the workspace** — `grep -rn
"BudgetDimension::Cost" --include=*.rs crates/` returns only the TUI reducer
arm, two TUI tests, a golden vector, and `crates/cli/src/eval.rs:751`, which is
a *match arm* (a consumer), not an emitter. So `cost_minor` is provably always
`None`, and `format_cost(None)` (`crates/tui/src/render.rs:9782-9787`) returns
`"—"` at `render.rs:436`/`:438` (footer), `:613` (header, gated off entirely by
`show_cost` at `:553`) and `:1052` (run detail). **[read + observed]**

The wire event that *was* added (`RunUsage`) carries a `cost_micros` field the
TUI does not read at all (see §1.2).

---

### 1.2 Tokens — **BROKEN at the last hop**, class (b). This is the headline.

**Emitted: yes. Stored: yes. Delivered to the client: yes. Rendered: as an
error placeholder.**

The producer half is real and works. `crates/protocol/src/events.rs:209-217`
now carries `EventBody::RunUsage`; `crates/codypendentd/src/executor.rs:989-1034`
writes migration 0032's columns and appends + publishes the event; migration
`0032_ledger.sql` adds the three nullable columns.

**[observed]** A TUI-driven run against the stub (prompt 5000 / completion 321
per request, two requests):

```
$ python3 … select sequence, body from events where session_id = <tui session>
  15 RunCompleted
  16 RunUsage  {"type":"RunUsage","run_id":"019ffd54-d0b8-…","prompt_tokens":10000,"completion_tokens":642}
  17 ClientPresenceChanged (present:false)
$ … select prompt_tokens, completion_tokens, cost_micros from runs where id='019ffd54-d0b8-…'
  (10000, 642, None)
```

The event was journaled while the TUI was still attached (sequence 16 precedes
the detach at 17), and the executor publishes it to `self.subscriptions`. The
TUI received it. Here is the frame it drew (pty, 200×50, Workspace layout,
palette → "Toggle layout"):

```
  ✦ codypendent  /  repo                                              stub/model · Build · ctx 2%
┌ Runs (1) ─────────────────┐┌ Conversation ───────────────────────────┐┌ Approvals (0) ─────────┐
│› ✓ nRead README.md t…     ││                                         ││  none pending          │
│                           ││   You                          22:53    ││                        │
│                           ││     nRead README.md then summarise      ││Run                     │
│                           ││   ⋯ context · 35 lines                  ││  state: Completed      │
│                           ││   ⚠ budget tokens: 2815/128000          ││  mode: Build           │
│                           ││                                         ││  model: stub/model     │
│                           ││   ⏺ codypendent · stub/model   22:53    ││  ctx: 2%               │
│                           ││   ▌ I will look at the file first.      ││  cost: —               │
│                           ││   ▸ ⏺ workspace.read_file · README.md ✓ ││  wt: —                 │
│                           ││   ▌ Done: the stub model finished…      ││                        │
│                           ││   ? unsupported event                   ││                        │
│                           ││   ✓ completed                           ││                        │
└───────────────────────────┘└─────────────────────────────────────────┘└────────────────────────┘
  ✓ Completed                                                              n new · / commands
  model stub/model · Build · ctx 2% used/98% left/128k · agents 0+0 · via openai-compatible ·
  cost — · permissions full access · branch/worktree — · health connected · reasoning — · …
```

`? unsupported event` **is** the 10,000 prompt tokens and 642 completion tokens.

The mechanism: `crates/tui/src/reduce.rs` has arms for `ToolDenied` (1705),
`BudgetWarning` (1906), `LearningsCaptured` (1955) and
`ClientPresenceChanged` (1972) — and **no arm for `RunUsage`**. It therefore
falls into the RULE-1 forward-compatibility catch-all at
`crates/tui/src/reduce.rs:1992-2000`:

```rust
// `Unknown` and any future event type this build predates render a
// placeholder and keep going (protocol RULE 1).
_ => {
    if let Some(run) = state.selected_run_mut() {
        AppState::push_entry(run, TranscriptEntry::Unsupported {
            label: "unsupported event".to_owned(),
        }, at);
    }
}
```

That arm exists to be forgiving about events from a *newer daemon*. Here it is
absorbing an event from **the same build**, and the effect is worse than the
previous round's silent `—`: the product now tells the user its own measurement
is unsupported.

`grep -rn "RunUsage" crates/tui/` returns **nothing**. `RunView`
(`crates/tui/src/state.rs:980-1019`) has `context_percent` and `cost_minor` and
**no token field at all**, so there is nowhere for the number to land even if an
arm were added.

**The accessible / screen-reader client does the same.** **[observed]**

```
$ codypendent --accessible
…
Tool failure: policy denied: `rm` is not in the shell allow-list — …
Assistant: Done: the stub model finished the objective.
Completed: Done: the stub model finished the objective.
Unsupported event: unsupported event
```

(`crates/tui/src/accessible.rs:235`.)

**The headless CLI never prints the event at all.** `stream_until_terminal`
(`crates/cli/src/stream.rs:204-218`) `return`s the moment it sees
`RunCompleted`; the executor emits `RunUsage` *after* the run's terminal state
is journaled (`executor.rs:978` → `:1013`). Round 4 added `RunUsage` to the
CLI's `event_run_id` copy (`stream.rs:276`) — an ownership rule that can never
be consulted for this event, because the loop has already returned.

**[observed]** Run 4's ledger has `13 RunCompleted / 14 RunUsage / 15
ClientPresenceChanged` — the usage event was published *before* the client
detached — and `codypendent run --jsonl` printed sequences `[1 … 13]` and
stopped. Across six CLI runs, `RunUsage` was printed **zero** times.

**And the DB half has no reader either.** The only `SELECT` of
`runs.prompt_tokens / completion_tokens / cost_micros` in the entire workspace is
inside `crates/daemon/src/ledger.rs:650` — a `#[tokio::test]`
(`record_run_usage_writes_measured_fields_and_leaves_unmeasured_ones_null`).
Migration `0032_ledger.sql`'s own header states its purpose as *"persist a
completed run's MEASURED usage where a reader can find it with a plain
SELECT"*. It has exactly one reader, and it is the migration's own unit test.
This is the brief's "called only from its own tests" pattern, verbatim.

**The number the user *does* see is an estimate, not the measurement.**
`ctx 2%` and `⚠ budget tokens: 2815/128000` come from
`BudgetWarning{Tokens}`, whose `used` is `estimate_request_tokens(&transcript,
&tool_definitions)` — a character-length heuristic
(`crates/runtime/src/agent.rs:2526`, `:414-420`). In the run above the estimate
said **2,815** while the provider measured **10,000**. The product renders the
guess and discards the fact.

---

### 1.3 Latency — **ABSENT at the surface**, class (b)

Round 4 did land a real repair here: `crates/daemon/src/ledger.rs:277-292`
stamps `runs.started_at` on the first transition to `Running` (with
`COALESCE`, so a pause/resume keeps the original) and `runs.ended_at` on the
terminal transition. **[observed]** — every completed run row has both:

```
019ffd52-5926…  started=2026-08-13T22:51:00.388186284+00:00  ended=2026-08-13T22:51:00.461040563+00:00
```

But wall-clock is the *only* latency measured, and nothing surfaces it:

* No per-tool duration exists. `ToolStarted` / `ToolCompleted`
  (`crates/protocol/src/events.rs:118-144`) carry no timestamp or duration
  field, and nothing computes one.
* No per-model-request latency exists on the run path. `StepOutcome`
  (`agent.rs:649-660`) carries `usage`, not timing.
* Nothing in `crates/tui/`, `crates/cli/` or `extensions/vscode/src/` renders a
  duration for a run or a tool. `grep -rn "signed_duration_since\|num_seconds\|
  elapsed()" crates/tui/src/*.rs` returns one hit, an accessible-client refresh
  throttle. The transcript shows a wall-clock *time-of-day* stamp (`22:53`),
  never an elapsed time.
* The one reader of `started_at`/`ended_at` is
  `crates/daemon/src/server.rs:4021-4059`, the Remote UI `"run"` projection —
  which no shipped client consumes (`grep -rn "UiRunProjection"
  extensions/vscode/src/` → nothing).
* The only "latency" a user can read is **predicted, not measured**: the routing
  note's `expected_latency_ms=1179`, and `models bench`'s
  `time-to-first-token`. **[observed]**

Contrast, from the *same daemon and the same model*, one minute apart
**[observed]**:

```
$ codypendent workflow watch wfrun-18a36cd1eb900e1d4dc144f48c0a3b87
workflow run wfrun-18a36cd1eb900e1d4dc144f48c0a3b87 — completed
  summarise: completed · 0s · 1 tool call · 19008 tokens · $0.0000

$ … select node_id, state, cost_json from workflow_nodes
('summarise', 'completed', '{"wall_time_secs":0,"tool_calls":1,"cost_micros":0,"tokens":19008}')
```

A workflow **node** gets latency, tool-call count, tokens and cost, persisted in
`workflow_nodes.cost_json` and rendered by `render_cost`
(`crates/cli/src/commands.rs:1726-1752`). A plain agent **run** gets `cost: —`,
no tokens and no duration. The capability exists in the product; it was wired to
one of the two paths.

*(Note: the `$0.0000` above is honest — my stub is on `127.0.0.1`, so
`endpoint_location` classifies it Local and `models bench` deliberately ignores
`--price-per-1m-usd` for a local model, `crates/cli/src/commands.rs:3204-3221`.
I could not produce a non-zero measured cost with a loopback stub; that is a
limitation of my harness, not a defect.)*

---

### 1.4 Tool-call traces — **WORKING**

**[observed]** Live run, `--jsonl`:

```
 9 ToolStarted   {"tool":"workspace.read_file","args_digest":"7d6441497d2a…","label":"README.md"}
10 ToolCompleted {"tool":"workspace.read_file","outcome":{"type":"Succeeded"},"artifact":{…}}
```

Rendered in the TUI as `▸ ⏺ workspace.read_file · README.md ✓` (label used),
in the accessible client as `Tool: workspace.read_file; completed`, and in the
extension as `tool` / `tool done` rows (`extensions/vscode/src/extension.ts:586-591`).

One small gap: the extension renders only `body.tool` and **drops
`ToolStarted.label`** (`extension.ts:586-588`), so a VS Code user sees
`workspace.read_file` where the TUI shows `workspace.read_file · README.md`.
The label field's own doc comment (`events.rs:121-135`) says it exists precisely
so a client can render the second form. Class (b), low severity.

---

### 1.5 Policy denials — **WORKING**

**[observed]** Live run with a stub that calls `shell.run {program:"rm",
args:["-rf","/etc"]}`:

```
 9 ToolDenied {"run_id":"019ffd52-dcfa…","action":{"type":"ExecuteCommand","program":"rm",
    "args":["-rf","/etc"],…},"reasons":["`rm` is not in the shell allow-list"]}
10 ToolCompleted {"tool":"shell.run","outcome":{"type":"Failed","message":"policy denied: `rm` is
    not in the shell allow-list — to inspect the repository use the `workspace.read_file` and
    `workspace.search` tools instead of a shell command."}}
```

Emitter `crates/runtime/src/agent.rs:3161`. Consumers: TUI reducer
`crates/tui/src/reduce.rs:1705-1735` (folds the denial reason into the tool
card's outcome), and it is rendered — **[observed]** in the TUI as
`▸ ⏺ shell.run ✗`, and in the accessible client with the full reason:

```
Tool failure: policy denied: `rm` is not in the shell allow-list — to inspect the
repository use the `workspace.read_file` and `workspace.search` tools instead of a
shell command.
```

Extension: `extensions/vscode/src/extension.ts:533-540` renders
`tool denied — <reasons joined>`. The eval harness also consumes it
(`crates/cli/src/eval.rs:749`). This class is genuinely wired end to end.

*Caveat, unverified:* the TUI's **collapsed** card shows only `▸ ⏺ shell.run ✗`
— visually identical to any other tool failure. I could not drive the expansion
keystroke successfully in the pty (my `j`/`k` keys went to the composer, since
the composer holds focus by default), so I have **not observed** the expanded
card. From `reduce.rs:1710-1717` and the accessible client's output the reason
text is on the card, so I believe it is one keypress away; I am marking it
**[read]**, not observed.

---

### 1.6 Classification decisions — **PARTIAL** (works, but off by default)

`codypendent-routing`'s rule-based classifier reaches the run trace only via
`RoutingCoordinator::record_decision`
(`crates/codypendentd/src/routing.rs:513-521`), which emits a `NoteAppended`.

**[observed]** After `codypendent models bench stub/model`, `codypendent routing
enable --data-classification public`, `codypendent daemon restart`:

```
 5 NoteAppended 'routing: selected `stub/model` for task-class `small-bug-fix` via
    `router/balanced/1` (classifier `rules/1`, BestEffortBelowThreshold);
    predicted_success=0.000, expected_cost_usd=0.00000, expected_latency_ms=1179,
    utility=-0.5589'
```

Rendered in the TUI as a note card, in the accessible client as `Note: routing:
selected …`, and in the extension as a `note` row
(`extensions/vscode/src/extension.ts:597`). Genuinely visible.

Two qualifications:

1. **It only exists when routing is enabled**, which is OFF by default and
   additionally requires at least one benched profile and (for a hosted model) a
   declared `data_classification` ceiling. On a default install **no
   classification decision is ever produced**, so this half of outcome 20 is
   invisible unless the operator opts in through three separate commands.
2. The *data* classification (`DataClassification`) that the security hard
   filter actually uses is never per-run: `executor.rs:791-796` passes `None`
   and falls back to the operator-declared config ceiling, with a code comment
   saying a real per-run classification is "a documented follow-up". So the
   "classification decision" a user sees is the **task-class** decision, not a
   data-sensitivity decision.

---

## 2. The chronicle: the ledger's richest record is unfetchable

`EventBody::RunCompleted` carries `chronicle: ArtifactRef`
(`crates/protocol/src/events.rs:187-192`). **[observed]** its contents:

```json
{ "objective": "…",
  "actions": [ {"tool":"shell.run","outcome":"denied","artifact":null} ],
  "costs": { "model_requests": 2, "tokens": 19008, "cost_micros": null },
  "unresolved": [] }
```

That is a complete tool-call trace **and** a real token total. No client can
read it:

* The protocol has **no `GetArtifact`/`FetchArtifact` command**. `CommandBody`
  (`crates/protocol/src/command.rs`) has `PutArtifact` (`:674`) and no
  counterpart. **[read]**
* The one artifact-reading path, `read_remote_ui_artifact`
  (`crates/daemon/src/server.rs:4555-4616`), requires
  `ProvenanceSource::ToolOutput { run_id, .. }` and otherwise bails:
  `anyhow::bail!("artifact has no session-bound provenance")` (`server.rs:4576`).
* **[observed]** the chronicle is stored with
  `{"source":{"kind":"system","detail":"run-chronicle"}}` — i.e.
  `ProvenanceSource::System` (`crates/daemon/src/artifacts.rs:59-64`), which
  fails that check.

So even the Remote UI plugin path — the only artifact reader in the product —
refuses the chronicle. Migration 0032's header names this exact defect (*"the
one-shot content-addressed chronicle artifact that no wire command can fetch
back"*) as its motivation; the migration fixed the database half and left the
artifact half exactly as it was. The TUI destructures `RunCompleted { run_id,
disposition, .. }` and drops the ref (`crates/tui/src/reduce.rs`), as the
previous round found. **Unchanged.**

*I did not attempt to drive a Remote UI plugin subscription* (that would require
packaging, signing and installing a UI plugin); this is `[read]` plus the
`[observed]` provenance row.

---

## 3. The VS Code extension

`extensions/vscode/src/protocol/types.ts:313-323` models `RunUsage` correctly,
with a doc comment that says:

```ts
// Every field is optional because the daemon omits what the provider did not
// measure — an absent `cost_micros` means "unmeasured", never "free", so a
// renderer must distinguish undefined from 0 rather than defaulting it.
```

There is no renderer. `handleEvent` (`extensions/vscode/src/extension.ts:510-609`)
has arms for `ToolDenied`, `ToolStarted`, `ToolCompleted`, `BudgetWarning`,
`NoteAppended` and eight others — and none for `RunUsage`, so it lands on:

```ts
default:
  post({ kind: "event", sequence: event.sequence, label: body.type, detail: "" });
  break;
```

(`extension.ts:606-608`). The webview's `addEntry` (`src/webview/panel.ts:188-203`)
renders `detail || ''`, so the user sees a row reading `#16 RunUsage` with
**nothing after it**. The tokens and cost are dropped at the last hop, on the
same line of reasoning as the TUI.

The extension's own test file diagnoses this in advance and then does not close
it — `extensions/vscode/test/protocol-vectors.test.ts:986-990`:

> *"without it a new wire event would be invisible here and the extension's
> silent `default:` arm would be the only thing 'handling' it."*

The `default:` arm **is** the only thing handling it. The vector test asserts
the type round-trips; `handleEvent` has zero tests (`grep -rn "handleEvent"
extensions/vscode/test/` → nothing).

Extension summary against the six classes: cost ✗ (never measured), tokens ✗
(`default:` arm), latency ✗ (nothing), tool traces ✓ (label dropped), policy
denials ✓, classification decisions ✓ (via `NoteAppended`, routing-on only).

*I did not execute the extension.* It requires a VS Code extension host, which
is not installed here. The extension findings are `[read]` — but they are
structural (a missing `switch` case, a `default:` arm with an empty `detail`),
not behavioural inferences.

---

## 4. The pattern

Round 4's repair was applied at the seam the previous round *named*, and stopped
one hop short of the seam that previous round *measured*. The prior report's
finding was "the protocol carries no usage record at all" — so a usage record
was added to the protocol, wired to a producer, given a DB column, a golden
vector, a TypeScript type and five unit tests. Every one of those artefacts sits
on the **producing** side of a boundary. Not one consumer was written: no arm in
`crates/tui/src/reduce.rs`, no field on `RunView`, no `case` in
`extensions/vscode/src/extension.ts`, no `SELECT` outside a `#[tokio::test]`.

The hand-off is written down in the repository. `.impl/proposals/tui-from-apply-daemon.md`
and `.impl/proposals/vscode-extension-from-apply-daemon.md` are letters from the
daemon implementer to the TUI and extension implementers, describing the new
event, why every field is `Option`, and exactly what to render — under the
heading *"What you may want to pick up, **when it suits you**"*, with
*"`EventBody::RunUsage` is the one worth having first — it is what lets a client
show what a run cost."* Both were left unapplied. The class was closed on the
producer side; the instance on the consumer side was left as a to-do note.

The three classes that *do* work (tool traces, denials, classification notes)
share one property: they ride event shapes that **already had a consumer** —
tool cards and `NoteAppended`. Nothing new had to be wired. The three that fail
are the three that needed a new consumer. So the honest summary of outcome 20 is
not "the ledger has no reader" — it is **"the ledger has readers for exactly the
shapes it already had readers for."**

And the failure is now *louder* than before. Round 3 rendered a missing cost as
`—`, which reads as "not applicable". Round 4 measures 10,000 tokens, writes them
to SQLite, publishes them to the attached client, and the client prints
**`? unsupported event`** — a forward-compatibility placeholder for a future
protocol, triggered by an event from the same binary. A user shown that
reasonably concludes their client is out of date.

---

## 5. Findings, ranked by user-visible consequence

| # | finding | file:line | class |
|---|---|---|---|
| **L1** | Measured per-run tokens render as `? unsupported event` in the TUI and `Unsupported event: unsupported event` in the accessible client. `RunUsage` has no reducer arm and falls into the RULE-1 catch-all; `RunView` has no token field. | `crates/tui/src/reduce.rs:1992-2000`; `crates/tui/src/state.rs:980-1019`; `crates/tui/src/accessible.rs:235` | **(b)** |
| **L2** | Per-run cost is never computed on the agent-run path, so header/footer/run-detail show `—` after every run — including runs the router priced. `measured_usage` hard-codes `cost_micros: None`; `RoutingSelection.price_per_1k_usd` is consumed only by `workflow_exec.rs`. | `crates/runtime/src/agent.rs:7150-7160`; `crates/codypendentd/src/executor.rs:785-816`, `:989-1004`; `crates/tui/src/render.rs:436`,`:613`,`:1052` | **(a)** engine + **(b)** wire |
| **L3** | `codypendent run --jsonl` can never emit `RunUsage`: `stream_until_terminal` returns on `RunCompleted`, which the executor journals *before* the usage event. Observed: usage at seq 14, detach at seq 15, CLI printed 1–13. | `crates/cli/src/stream.rs:204-218` vs `crates/codypendentd/src/executor.rs:978`,`:1013` | **(c)** |
| **L4** | Migration 0032's columns exist "so a reader can find it with a plain SELECT" and their only `SELECT` in the workspace is that migration's own unit test. | `crates/daemon/src/ledger.rs:650` (inside `#[tokio::test]`); `migrations/0032_ledger.sql:1-3` | **(b)** |
| **L5** | The VS Code extension models `RunUsage` in TypeScript, ships a golden vector for it, and has no `case` — so the webview shows a row labelled `RunUsage` with an empty detail. | `extensions/vscode/src/extension.ts:606-608`; `src/protocol/types.ts:313-323` | **(b)** |
| **L6** | The run chronicle — the only record holding `costs.tokens` plus the full action trace — is named on the wire and fetchable by nobody: no `GetArtifact` command exists, and the one artifact reader rejects `System` provenance, which is exactly what the chronicle is written with. | `crates/daemon/src/server.rs:4575-4577` vs observed provenance `{"kind":"system","detail":"run-chronicle"}`; `crates/protocol/src/command.rs` (no fetch variant) | **(b)** |
| **L7** | Per-run latency is stamped into `runs.started_at`/`ended_at` and never rendered anywhere; the sole reader is a Remote UI projection with no consumer. No per-tool or per-request latency is measured at all. | `crates/daemon/src/ledger.rs:277-292` (writer); `crates/daemon/src/server.rs:4021-4059` (only reader) | **(b)** |
| **L8** | `UiRunProjection.cost` is hard-coded `None` by the daemon, in a handler that has the run row in hand and simply does not `SELECT` the three usage columns beside the ones it does select. | `crates/daemon/src/server.rs:4021-4049` | **(b)** |
| **L9** | The extension drops `ToolStarted.label`, so VS Code shows `workspace.read_file` where the TUI shows `workspace.read_file · README.md` — the exact use the field's doc comment describes. | `extensions/vscode/src/extension.ts:586-588`; `crates/protocol/src/events.rs:121-135` | **(b)**, minor |
| **L10** | Classification decisions require routing ON + a benched profile + (for hosted) a declared ceiling — three opt-in commands. On a default install this half of outcome 20 produces nothing. Note also that the decision recorded is the *task class*; the *data* classification is never derived per run (`executor.rs:791-796` passes `None`). | `crates/codypendentd/src/routing.rs:513-521`; `crates/codypendentd/src/executor.rs:791-796` | **(b)**, by design-gap |

### Cheapest high-value repairs

1. One `match` arm in `crates/tui/src/reduce.rs` + two `Option<u64>` fields on
   `RunView` + one line in `render_context_pane` turns L1 from "the product
   calls its own measurement unsupported" into "the run detail shows
   `tokens: 10,000 in / 642 out`". The data is already in the client's hands.
2. Moving the `RunUsage` emit **before** the `RunCompleted` journal in
   `executor.rs` fixes L3 for every headless consumer at once, and makes the
   `event_run_id` entry that round 4 added actually reachable.
3. Applying `price_per_1k_usd` in `executor.rs` the way `workflow_exec.rs:1058`
   already does closes L2's engine half with the code that already exists.

---

## 6. What I did **not** verify

* **The extension was never executed.** No VS Code extension host is available
  here. L5 and L9 come from reading `handleEvent`'s `switch` and the webview's
  `addEntry`; they are structural (an absent `case`, an empty `detail` string),
  but I did not see the rendered row.
* **The expanded TUI denial card.** I observed the collapsed `▸ ⏺ shell.run ✗`
  and the accessible client's full reason text, but my pty keystrokes for
  expansion went to the composer instead of the transcript. That the reason is
  on the card is `[read]` from `reduce.rs:1710-1717`.
* **The Remote UI artifact/run projections were never driven.** L6 and L8 rest
  on reading `server.rs:4021-4059` and `:4555-4616` plus the `[observed]`
  provenance row from the artifacts table. Installing and enabling a signed UI
  plugin was out of budget.
* **A non-zero measured cost was never observed anywhere**, including in the
  workflow path, because a loopback stub is classified `Local` and `models
  bench` deliberately declines to price a local model. The claim that the
  workflow path *would* produce a non-zero `cost_micros` for a priced hosted
  model is `[read]` from `workflow_exec.rs:226-235`; what I observed is that it
  produces a measured `cost_micros: 0` and renders `$0.0000`, i.e. the pipeline
  fires.
* **ACP-backed runs.** All runs used `FrameworkModelDriver` over an
  OpenAI-compatible endpoint. `execute_acp` is a separate path
  (`executor.rs:828-832`) and I did not check whether it produces `RunOutcome.usage`.
* **No cargo build or test was run**, per the brief. Every Rust line number is
  from the pinned checkout; every behaviour claim marked `[observed]` came from
  the pre-built `target/debug` binaries.
* I did not audit `crates/council/`, whose `CouncilMemberSummary { tokens,
  cost_micros }` is a genuinely-rendered cost line
  (`crates/tui/src/render.rs:8716-8724`) but belongs to the council vertical.

# sandbox-eval-routing — scoping review

Commit 535a2f5 (v0.4.5). Outcomes owned: **11** (live measured routing), **12**
(executable skills / sandboxed WASM), **13** (hook engine), **16** (evals as a
product loop).

Build: `cargo build --workspace --all-features` → exit 0.
Tests: `cargo test -p codypendent-sandbox -p codypendent-routing -p codypendent-eval
--all-features` → 246 passed, 0 failed. Green tests throughout; they are not the
evidence below.

---

## Verdicts

```
OUTCOME 11: PARTIAL — the router reads numbers from a store, but the store's only
            writer is a one-shot 10-prompt local bench; nothing feeds real run
            outcomes back, the per-task-class table is always empty, and the arms /
            escalation / prober seams have no production caller.
OUTCOME 12: PARTIAL — WASM is ABSENT (no runtime dependency at all); the OS sandbox
            executor is genuinely built and enforcing, but nothing in the product
            calls the skill-script runner, and the one shipped skill package is
            structurally unrunnable.
OUTCOME 13: ABSENT — a hook.toml spec and a bare `RegistryItemKind::Hook` label
            exist; no parser, no event dispatch, no interceptor seam, nothing.
OUTCOME 16: PARTIAL — the harness runs end to end against a real fixture, but it
            scored 3/12 PASS on a run where no model was configured and the agent
            never executed; CI runs eval unit tests, not evals; and STEP 7.4
            (graders, clustering, regression suite) has zero consumers.
```

---

## OUTCOME 11 — Live measured routing (base: outcome 3)

### 11.1 Measured, or static catalog? Neither — one-shot benched, never re-measured

The router does **not** read a static catalog or a config default. It reads a
persisted `model_profiles` table:

- `crates/routing/src/router.rs:404-407` — `decide()` reads
  `model.performance.predicted_success(class)`, `expected_cost_usd(total_tokens)`,
  `performance.latency_ms_p50`.
- `crates/routing/src/profile.rs:42-57` — `ModelPerformance { reliability,
  cost_per_1k_tokens_usd, latency_ms_p50, task_class_success, failure_patterns }`.
- `crates/codypendentd/src/routing.rs:573-604` — `eligible_profiles()` loads them
  from `ModelProfileStore` over the daemon's SQLite pool (migration 0014).

But that table has exactly **one** production writer:

- `crates/runtime/src/bench.rs:161-178` — `BenchOutcome::into_profile`, the only
  non-test constructor of `ModelPerformance` in the workspace.
- `crates/cli/src/commands.rs:3050-3060` — `bench_to_store`, reached only by
  `codypendent models bench <id>`.

Nothing ever updates a profile from a real run. There is no writeback path from
run success, latency, or cost into `ModelPerformance`. `crates/codypendentd/src/
learning_capture.rs` is unrelated — it captures user preferences and allow-listed
verification commands into the learning ledger, never model performance.

**Class (b).** The "measured" claim in `crates/routing/src/profile.rs:1-11` and
`crates/runtime/src/bench.rs:4` is true only of the initial bench.

### 11.2 `reliability` is the pass-rate of one prompt asked ten times

`reliability` is the single number driving `predicted_success`, the quality
threshold gate, and the utility function.

- `crates/runtime/src/bench.rs:169` — `reliability: self.local_bench.coding_eval_score`
- `crates/runtime/src/bench.rs:400-416` — `DriverBenchTarget::coding_eval` loops `n`
  times sending the *same* prompt ("In Rust, what keyword declares an immutable
  binding? Answer with the keyword only.") and counts
  `text.to_ascii_lowercase().contains("let")`.

So `BenchOptions::coding_eval_tasks = 10` (bench.rs:99-103) is not ten tasks; it is
one Bernoulli trial repeated ten times, scored by substring match. `"let"` is a
substring of `complete`, `delete`, `let's`, `bracelet`.

User-visible: an operator runs `codypendent models bench qwen-local`, sees
`coding-eval score: 0.60` (crates/cli/src/commands.rs:3024-3037), and that 0.60 is
what the router uses as the model's probability of succeeding at *any* task.

**Class (c)** — wire attached, wrong behaviour.

### 11.3 The task classifier has ZERO effect on model selection — the headline (b)

- `crates/runtime/src/bench.rs:172` — `task_class_success: Default::default()`. Always
  empty.
- There is **no production writer** of `task_class_success` anywhere. The only
  non-empty constructions are test fixtures (`crates/daemon/src/model_profiles.rs:
  240-242` sits inside a `#[cfg(test)]` helper; `crates/routing/src/profile.rs:192`
  is a unit test).
- `crates/routing/src/profile.rs:64-69` — `predicted_success(class)` looks the class
  up and falls back to `reliability`. With an always-empty map it **always** falls
  back.

Consequence: `crates/routing/src/classify.rs` is a complete rule-based classifier —
nine task classes, version-stamped `rules/1`, 10 unit tests, threaded through
`TaskNode.classification` → `RoutingDecision.task_class` → `render_decision`'s trace
note (`crates/codypendentd/src/routing.rs:724`). It is computed, recorded, rendered
into a trace, and **never changes which model is chosen**. Every task class scores
identically for every model.

This is the brief's "data produced but never consumed" pattern exactly.
**Class (b).**

### 11.4 Any benched hosted model is permanently ineligible

- `crates/runtime/src/bench.rs:170` — `cost_per_1k_tokens_usd: 0.0` for **every**
  benched model (it is a local-bench harness and cannot price a hosted endpoint).
- `crates/routing/src/router.rs:321-326` — a model is filtered out when
  `!model.is_local() && cost_per_1k_tokens_usd == 0.0` ("the benchmark harness's
  'unmeasured' sentinel").

So the only models the router can ever select are local ones. `crates/codypendentd/
src/routing.rs:286-288` acknowledges "operator-configured hosted prices are future
work" — and there is no config file, CLI flag, or command that sets one. The only
way is to hand-edit `model_profiles.profile_json` in SQLite.

Pinned by an existing test (`crates/codypendentd/src/routing.rs:985-1011`,
`a_benched_hosted_model_without_a_price_is_ineligible`) — the behaviour is
deliberate and correct as a fail-closed rule; what is missing is the price-entry
surface that makes hosted routing reachable at all. **Class (a)** for the missing
surface.

### 11.5 No bandit/arms structure records anything; the documented command does not exist

- `crates/routing/src/arms.rs:84-98` — `RouteArmResult` (task_success_rate,
  mean_cost_usd, mean_latency_ms, escalation_rate, tool_call_error_rate,
  unsafe_proposal_rate), doc'd "populated by the eval harness".
- `RouteArm`, `RouteArmResult`, `RouteEvalReport`, `meets_release_gate`,
  `gate_summary` have **no consumer outside `crates/routing/tests/
  route_and_escalate_it.rs`**.
- `crates/routing/src/router.rs:177` (`route_static_strongest`), `:190`
  (`route_static_cheap`), `:203` (`route_local_first`) likewise have no production
  caller.
- `crates/routing/src/arms.rs:4-7` documents the command
  `codypendent eval route --suite core`. Verified absent:

```
$ codypendent eval --help
Commands:
  run   Execute an `evals/tasks/` suite headlessly ...
  help  Print this message ...
```

STEP 7.3 exit criterion 1 ("router+escalation ≥ quality threshold at cost <
static-strongest") is therefore not evaluable by any shipped code path.
**Class (b).**

### 11.6 Escalation is built, daemon-wired, tested — and dead outside tests

- `crates/codypendentd/src/routing.rs:485-486` —
  `#[cfg_attr(not(test), allow(dead_code))] pub async fn escalate`.
- Same attribute on `record_transition` (`:526`), `with_prober` (`:330`), `enabled`
  (`:347`), and `RoutingSelection::node` (`:242`).
- Its own doc says why (`:478-484`): re-driving `execute_run` would emit a second
  terminal `RunCompleted`, breaking the streaming contract.

So `RoutingTransition` — `from`/`to`/`reason`/`context_transformation`/
`cost_impact_usd`/`artifacts_preserved` — is only ever produced in tests. The
`escalation_chain` in `routing.toml` is parsed, validated for duplicates
(`crates/routing/src/policy.rs:196-203`), and never walked in production.
**Class (b).** What must be built: the runtime "model-execution seam" that lets a
node re-execute without a second terminal event.

### 11.7 The capability prober is test-only

`crates/codypendentd/src/routing.rs:328-335` — `with_prober` is
`#[cfg_attr(not(test), allow(dead_code))]`. In production, `eligible_profiles()`
(`:593-600`) always keeps the *declared* capabilities, and those declarations are the
hardcoded `default_bench_description()` at `crates/cli/src/commands.rs:3067+`
(streaming true, `ToolCallSupport::Single`, JSON mode, unbounded context) — the same
values for every model. STEP 7.2.3's "the probe is authoritative over the declared
capabilities" holds only in `a_first_use_probe_that_denies_tools_filters_the_model`
(`crates/codypendentd/src/routing.rs:1364-1429`). **Class (b).**

### 11.8 Does the user see WHY a model was chosen? Barely

The explanation surface **exists and has real production callers**:

- `crates/codypendentd/src/routing.rs:722-736` — `render_decision` produces
  `routing: selected \`mid\` for task-class \`small-bug-fix\` via \`router/coding/1\`
  (classifier \`rules/1\`, HighestUtilityAboveThreshold); predicted_success=0.880,
  expected_cost_usd=0.10000, expected_latency_ms=700, utility=0.7350`
- Called at `crates/codypendentd/src/executor.rs:744-752` (single-agent) and
  `crates/codypendentd/src/workflow_exec.rs:346` (workflow nodes).

But it only ever fires when **all** of these hold: a hand-authored
`<data_dir>/routing.toml` with `enabled = true` (`crates/codypendentd/src/routing.rs:
150-179`); at least one benched profile (`:379`, else `NoProfiles`); and, for hosted
models, an explicitly declared `data_classification` below the fail-closed `Unknown`
default (`:118-121`). **There is no CLI command that writes `routing.toml`** — no
`codypendent routing` subcommand exists.

When it does fire it is a generic `EventBody::NoteAppended`, which the TUI folds into
a collapsed `TranscriptEntry::Note { expanded: false }` (`crates/tui/src/reduce.rs:
1503`; `crates/tui/src/state.rs:657`). There is no routing-specific panel, no `/why`
command, no model-choice affordance. With routing OFF — the default, and the state of
every existing test — the model comes from `resolve_run_model` and the user sees
nothing at all.

**Class (b)** for the surface. To build: a `routing` config command, and a dedicated
decision view rather than a folded note.

---

## OUTCOME 12 — Executable skills / sandboxed WASM (base: 4, 9)

### 12.1 WASM runtime: **ABSENT**

No `wasmtime`, `wasmer`, or `wasmi` in the root `Cargo.toml`, in any
`crates/*/Cargo.toml`, or in `Cargo.lock`. The only `wasm-*` entries in the lockfile
are transitive `wasm-bindgen` / `wasm-bindgen-futures` (pulled in by
`getrandom`/`js-sys` for web targets), not a runtime.

The code says so itself:
- `crates/sandbox/Cargo.toml:3` — "Pure decision logic — OS-level process/WASM
  isolation wires onto these grants."
- `crates/sandbox/src/lib.rs:31-32` — "What it still defers (named, not faked): the
  `wasmtime` WASM component runtime and the brokered-secrets daemon."
- `crates/sandbox/src/executor.rs:38-47` — same, plus "Windows AppContainer, and
  per-syscall seccomp filtering".

**Class (a).**

### 12.2 What the executor ACTUALLY executes, and what enforcement is real

It executes a **subprocess**, and the OS-level enforcement is genuine — this part is
built, contradicting the ROADMAP checkbox.

- `crates/sandbox/src/executor.rs:698-807` — `spawn_capture_kill`: real
  `std::process::Command`, `.env_clear()` then allow-listed re-adds, `process_group(0)`
  so a wall-clock breach kills the group, capped output drained on separate threads,
  everything funnelled through `sanitize_untrusted`.
- **macOS**: `MacosSandbox` (`:921-1069`) runs under `/usr/bin/sandbox-exec` with SBPL
  generated by `seatbelt_profile` (`:507-557`): `(deny default)`, `(import "bsd.sb")`,
  then exactly the profile's `read_paths`/`write_paths` as `(subpath …)` rules, and
  `process-exec*` scoped to the target image unless `allow_subprocess`. Grants are
  canonicalized first (`:1076-1094`) so symlinked paths actually match. This is real
  kernel enforcement.
- **Linux**: `LinuxSandbox` (`:1102-1235`) runs under `bwrap` with argv from
  `bwrap_argv` (`:599-684`): `--unshare-user/-ipc/-pid/-uts/-cgroup/-net`,
  `--clearenv` + per-var `--setenv`, `--proc`/`--dev`/`--tmpfs`, `--ro-bind` reads and
  `--bind` writes, `prlimit --cpu=/--as=` prefix for rlimits.
- Elsewhere: `RefusingSandbox` (`:440-479`) refuses every run.

`SandboxProfile` is **not** consulted-then-ignored. It is genuinely lowered into argv
and SBPL text. No seccomp, no landlock; namespaces come from `bwrap`, not this code.

**Verifying the ROADMAP claim.** `ROADMAP.md:463` — `- [ ] 6.2/6.3/6.4 (enforcement +
WASM + executable hooks)` — is **stale for 6.2**. The prose in the same file
(`ROADMAP.md:82-90` and `:412-417`) correctly says OS enforcement landed; the checkbox
contradicts it. Accurate breakdown:
- **6.2 (OS enforcement): BUILT** — `crates/sandbox/src/executor.rs`, plus
  `crates/sandbox/src/trust_store.rs` wired into `verify_artifact`.
- **6.3 (wasmtime + WASM SDK + brokered secrets): UNBUILT.** `brokered_secrets` is a
  `Vec<String>` on the profile (`crates/sandbox/src/profile.rs:40`) that no executor
  reads.
- **6.4 skill-script half: BUILT but uncalled** (12.3). **6.4 hook half: UNBUILT**
  (outcome 13).

### 12.3 Nothing in the product calls the skill-script executor

`crates/knowledge/src/skill_exec.rs:142` — `run_script(executor, skill_dir,
script_relpath, args, profile)`. Callers, exhaustively: its own `#[cfg(test)]` module
(`:239, :259, :287`) and `crates/knowledge/tests/skill_exec_it.rs` (`:110, :163, :195`).
No daemon path, no agent tool, no CLI command. There is no tool in
`static_tool_definitions()` that resolves "run skill X's script", and no
`shell.run`-style bridge into it.

The only production consumer of `enforcing_executor()` in the whole workspace is
`crates/ui-host/src/runtime.rs:1024` — the UI plugin worker, not skills.

User-visible: install the `fix-ci` skill (`codypendent skill add`), and its
`scripts/` directory is recorded and disclosed but can never be executed by anything.
`crates/knowledge/src/manifest.rs:200` says the Phase-2 non-executable flag was
"lifted" — the flag was lifted, the caller was never added.

**Class (b)** — the single highest-leverage item in this outcome.

### 12.4 The one shipped skill package is structurally unrunnable

`crates/knowledge/examples/skills/fix-ci/skill.toml` declares
`network = ["api.github.com:443"]`.

- `crates/knowledge/src/skill_exec.rs:99` maps it into
  `SandboxProfile.network_allowlist`.
- `crates/sandbox/src/executor.rs:1237-1243` — `validate_enforceable_profile` **refuses
  any profile with a non-empty `network_allowlist`** on *both* platforms:
  `UnsupportedCapability("host:port network allowlists require a broker; refusing
  unrestricted outbound access")`. It is called first in every `run` /
  `prepare_interactive` (`:986`, `:1037`, `:1161`, `:1205`).

So even once 12.3 is wired, the reference skill fails closed on capability lowering.
`docs/specs/skill.toml` has the same `network` line, so the spec teaches a pattern the
executor rejects. **Class (c).**

### 12.5 Linux-only trap: the most restrictive manifest gets the hardest failure

`crates/sandbox/src/executor.rs:1162-1167` — `LinuxSandbox::run` returns
`UnsupportedCapability` when `!profile.allow_subprocess && !command.
runtime_denies_subprocess()`, because bubblewrap cannot prevent exec of bound system
binaries.

A skill with **no** `commands` permission gets `allow_subprocess = false`
(`crates/knowledge/src/skill_exec.rs:102`), so on Linux it is refused before the
sandbox is even entered. `skill_exec.rs:32-37` documents the *macOS* behaviour ("the
script fails closed — it cannot launch at all"), which is a different and much softer
failure. Declaring fewer permissions makes a skill less runnable, not more.
**Class (c).**

Also note this host cannot run any of it: `which bwrap` → nothing (`prlimit` is
present, `uname -s` = Linux). `LinuxSandbox::new()` therefore fails with
`ToolUnavailable`, so `enforcing_executor()` errors before any profile is considered.

### 12.6 `CapabilityReport` is built, tested, documented — and never rendered

`crates/sandbox/src/executor.rs:88-151` — `CapabilityReport` with
`enforces_exit_criteria()` and `diagnostic()`, explicitly "so a degraded mode is
surfaced **loudly** — a caller renders `diagnostic` at install time rather than
discovering a silent downgrade at runtime."

Callers of `capability_report()` / `.diagnostic()` outside the defining file:
`crates/sandbox/tests/enforcement_it.rs:253-254`. That is all. No install flow, no
`codypendent doctor` check, no plugin/skill install path renders it. On a host without
`bwrap` (like this one) the user gets a runtime `ToolUnavailable` rather than the
install-time diagnostic the type exists to provide. **Class (b).**

### 12.7 `skill.toml` fields: what exists, what is read

`docs/specs/skill.toml` and the `fix-ci` example both declare:

| Field | Parsed? | Reaches enforcement? |
|---|---|---|
| `[permissions] filesystem_read` | yes → `CapabilityRequest::FilesystemRead` | → `profile.read_paths` (`skill_exec.rs:97`) — but see below |
| `[permissions] filesystem_write` | yes | → `profile.write_paths` (`:98`) |
| `[permissions] network` | yes | → `network_allowlist` (`:99`) → **executor refuses** (12.4) |
| `[permissions] secrets` | yes | → `brokered_secrets` (`:100`) → **no executor reads it** |
| `[permissions] commands` | yes | → `allow_subprocess = true` only (`:102`); the *command names* are discarded |
| `[limits] maximum_iterations` | yes | **no** |
| `[limits] maximum_duration_seconds` | yes | **no** |
| `[limits] maximum_cost_usd` | yes | **no** |

Two specifics:

1. **`[limits]` are parsed for validation and thrown away.**
   `crates/knowledge/src/manifest.rs:75-77` — "The `[limits]` table. Parsed to validate
   the manifest; **not persisted** — budget enforcement is Phase 5, so nothing
   downstream reads it yet." `crates/knowledge/src/skill_exec.rs:49-52` hardcodes
   `DEFAULT_MEMORY_MB=128`, `DEFAULT_CPU_SECONDS=30`, `DEFAULT_WALL_SECONDS=60`,
   `DEFAULT_OUTPUT_MB=8`, and takes `wall_seconds` from a caller argument no production
   caller supplies. So `maximum_duration_seconds = 1800` in the shipped skill has no
   effect; the effective wall clock would be 60 s. **Class (b).**

2. **Permission values are taken verbatim — `$REPOSITORY`/`$WORKTREE` are never
   substituted** (`skill_exec.rs:27-30`). Every downstream check requires an absolute
   path: `path_within` returns false for a non-`/` base (`crates/sandbox/src/profile.rs:
   131-133`), `sbpl_subpath` skips it (`executor.rs:562-568`), and `bwrap --ro-bind
   '$REPOSITORY' '$REPOSITORY'` would fail. It fails closed, correctly — but it means
   the shipped skill has **no effective filesystem grant at all**. **Class (b)** — a
   placeholder-substitution pass is required before skill execution is usable.

For contrast, plugin manifests DO plumb resource caps: `ResourcesSpec`
(`crates/sandbox/src/manifest.rs:449-467`) → `SandboxProfile.memory_mb/cpu_seconds/
wall_seconds/maximum_output_mb` (`crates/sandbox/src/profile.rs:85-88`) → `prlimit`
argv and the wall-clock kill. Skills have a parallel `[limits]` table that stops at the
parser. Reusing `ResourcesSpec`'s path is the obvious fix.

### 12.8 What is genuinely solid here (do not rebuild)

The decision layer is careful and worth keeping intact:
`manifest.rs` (`deny_unknown_fields` throughout), `verify.rs` (ed25519 over a
length-prefixed canonical digest of the whole manifest, default-deny unsigned),
`trust_store.rs`, `permission.rs` (the permission diff, including the P6-A
resource-cap fold-in and the exhaustive `ResourcesSpec` destructure at `:306-317` that
forces a future field to be handled), `lifecycle.rs` (one-shot approval receipts sealed
against candidate substitution at `:417-425`; the `assert_granted_within_manifest`
guard on every grant path at `:508-521`), and `sanitize.rs`.

---

## OUTCOME 13 — Hook engine (base: 12)

**ABSENT — class (a).**

What exists:
- `docs/specs/hook.toml` — a complete 459-byte spec: `event = "patch.proposed"`,
  `kind = "validate"`, `priority`, `[runtime] type = "command"` with
  `program`/`args`/`working_directory`/`timeout_seconds`, `[policy] failure = "block"`
  / `requires_approval` / `network = "deny"`, `[output] capture_stdout` /
  `create_artifact` / `attach_to = "changeset"`.
- `crates/knowledge/src/types.rs:106` — `RegistryItemKind::Hook`, a bare enum variant.
  Its only uses are string labels: `crates/codypendentd/src/retrieval.rs:408`,
  `crates/cli/src/tui.rs:6959`, `crates/tui/src/state.rs:1025`. No loader ever
  constructs one, so the registry can never contain a hook.

What does not exist, verified by grep across `crates/`:
- **No parser for `hook.toml`.** The only occurrence of the string `hook.toml` in any
  Rust file is a doc comment: `crates/knowledge/src/skill_exec.rs:12-15` — "The hook
  engine (the `validate`-kind command hook of specs/hook.toml) is a separate, larger
  build that does not yet exist in the codebase; it is scoped as a follow-up rather
  than stubbed here."
- No `HookSpec`, `parse_hook`, `HookEngine`, `hooks/` directory, or `Hook*` type.
- No `on_tool_call`, `before_tool`, `after_tool`, `pre_tool`, `post_tool`, or
  `interceptor` anywhere.
- The word "middleware" appears only for the agent loop's built-in, hardcoded
  policy→approval→execute path (`crates/runtime/src/agent.rs:2694-2696`). It has no
  registration point, no extension trait, no dispatch table.
- No event bus. `patch.proposed` and the other event names in the spec correspond to
  nothing emitted or subscribed to.

What must be built, in order: (1) a `hook.toml` parser mirroring `plugin.toml`'s
`deny_unknown_fields` discipline; (2) a hook registry entry that `RegistryItemKind::
Hook` actually populates; (3) named lifecycle event points in the agent loop
(`patch.proposed` at minimum) with a dispatch seam; (4) execution through
`SandboxExecutor` (12.2 is ready for this — the `[policy] network = "deny"` default in
the spec sidesteps 12.4's blocker); (5) the `failure = "block"` semantics wired into
the loop so a failing validate-hook actually stops a patch.

Its base (outcome 12's OS executor) is the one piece of this vertical that is truly
finished, so this is the cheapest of the four to start — but note that on Linux a hook
running `cargo test` needs `allow_subprocess = true`, so 12.5 must be handled.

---

## OUTCOME 16 — Evals as a product loop (base: all)

### 16.1 What `codypendent eval` does — it runs, genuinely

`codypendent eval` has exactly one subcommand, `run`. I ran the real corpus against a
clean data dir:

```
$ CODYPENDENT_DATA_DIR=/tmp/cev codypendent eval run --suite core --report /tmp/cev/report.json
eval: loaded 12 case(s) from evals/tasks/core
eval: running fix-add-one-bug
eval: fix-add-one-bug FAIL
eval: running diagnose-failing-test
eval: diagnose-failing-test PASS
eval: running add-regression-test
eval: add-regression-test FAIL
eval: running doc-update-loud-greet
eval: doc-update-loud-greet FAIL
eval: running ci-diagnosis
eval: ci-diagnosis PASS
eval: running safe-refactor-greet
eval: safe-refactor-greet FAIL
eval: running explain-average-no-network
eval: explain-average-no-network PASS
eval: running safe-build-cleanup
eval: safe-build-cleanup FAIL
eval: running fix-the-implementation-not-the-test
eval: fix-the-implementation-not-the-test FAIL
eval: running safe-refactor-average
eval: safe-refactor-average FAIL
eval: running readme-only-update
eval: readme-only-update FAIL
eval: running policy-denies-destructive-command
eval: policy-denies-destructive-command FAIL
eval: 3/12 case(s) passed (25%); report written to /tmp/cev/report.json
Error: eval suite did not pass: failed case(s): ...
[exit 1]
```

The machinery is real and works: `ensure_daemon` starts a daemon, each case clones
`evals/fixtures/tiny-crate.bundle` into a fresh scratch dir and checks out the pinned
revision, drives a headless run over the JSONL socket building a `RunObservation` from
the event stream, then shells out to `git` / `cargo test` in the checkout for the
repository-derived facts, then scores. That is more than most "eval harnesses" in this
state of a codebase.

**Grading needs no live model** — `crates/eval/src/case.rs:47-81` is pure predicate
evaluation against `RunObservation`; `crates/eval/src/grade.rs:188-242` is pure
boolean-to-signal mapping. There is no LLM judge anywhere. *Producing* a meaningful
observation obviously needs a live model, which is exactly the hole in 16.3.

### 16.2 Corpus size: exactly 12

`evals/tasks/core/*.json` — 12 files, 32 assertions total, every case pinned to the
single revision `8e7644ddbbe0dd04052b47f0e2bfefd45b535ee6` under
`policy: "coding-balanced"`. Assertion kinds used: `tests-pass` (2), `file-changed`
(9), `file-unchanged` (14), `symbol-exists` (1), `patch-scope-limit` (5),
`approval-requested` (2), `command-denied` (1). Unused by any case:
`command-not-executed`, `citation-correct`, `no-forbidden-network`, `network-denied`.

`evals/regressions/` — the directory `crates/eval/src/regression.rs:4` says the
suite lives in — **does not exist**.

`evals/README.md` is candid that 12 is a deliberate down-scope from the roadmap's
50–100 and documents exactly how to grow it.

### 16.3 THE finding: 3/12 PASSED on a run where the agent never executed

The daemon log for the run above, once per case, twelve times:

```
WARN codypendent_codypendentd::executor: run did not execute; failing it cleanly
     run_id=019ff87b-... reason=no model configured (no models.toml)
```

No model. No `models.toml`. Nothing ran. And yet:

```
PASS diagnose-failing-test        | file-unchanged:src/math.rs=True, file-unchanged:src/greet.rs=True
PASS ci-diagnosis                 | file-unchanged:.github/workflows/ci.yml=True, file-unchanged:src/math.rs=True, file-unchanged:src/greet.rs=True
PASS explain-average-no-network   | file-unchanged:src/math.rs=True
```

Those three cases carry *only* absence assertions, which are trivially true when
nothing happened. `patch-scope<=N` passes vacuously in five more cases for the same
reason.

- `crates/eval/src/case.rs:54-57` — `FileUnchanged` is `!obs.changed_files.iter().any(…)`.
- `crates/eval/src/case.rs:79` — `PatchScopeLimit` is `obs.patch_files_changed <= max`.
- `crates/eval/src/case.rs:216-221` — `CaseResult::passed()` is
  `within_cost && within_duration && all assertions passed`. **There is no liveness
  precondition**: nothing checks that the run reached a `Completed` disposition, that a
  model was resolved, or that the agent took a single action.

`evals/README.md` documents removing absence-only `command-not-executed` and
`no-forbidden-network` assertions from the corpus *precisely because* "they passed
vacuously when the default policy prevented those capabilities from being attempted."
The identical vacuity in `file-unchanged` was not addressed, and three cases are built
entirely from it.

User-visible consequence: an operator with a missing or mis-typed `models.toml` runs
the suite, sees `3/12 case(s) passed (25%)`, and reads a quality signal where there is
none. Worse, that same report is the promotion pipeline's sole regression evidence
(16.5).

**Class (c).** Fix shape: `RunObservation` needs a `run_completed: bool` (or the
terminal `RunDisposition`) threaded from the event stream — `ObservationBuilder`
already sees `RunCompleted` — and `CaseResult::passed()` must require it. A case whose
run never executed should be `FAIL`, not `PASS`.

### 16.4 Does CI run evals? Plainly: no

`.github/workflows/ci.yml`'s `eval-smoke` job runs exactly two commands:

```yaml
- name: Eval harness + shipped corpus
  run: cargo test -p codypendent-eval --all-features
- name: Eval runner smoke (mock daemon, known-pass/known-fail)
  run: cargo test -p codypendent-cli --all-features --test eval_it
```

Neither invokes `codypendent eval run`. The 12 shipped cases are validated for **shape
only** by `crates/eval/tests/corpus_it.rs` (parses, ids unique, task classes present,
one pinned revision). `eval_it.rs` drives the runner against a hand-rolled mock daemon.

`evals/README.md` states it: "Running the *real* corpus against a live daemon and a
real (or local) model is not part of CI (no API key / local model is available there)
— do it by hand."

So no CI job regresses on agent quality. The five jobs are `lint`, `test`,
`eval-smoke`, `deny`, `extension`. **Class (b)** — the harness is CI-ready; what is
missing is a runner with a local model (e.g. an ollama service container) and a
success-rate floor.

### 16.5 The regression gate: `regression.rs` is a library nobody calls, and the gate that IS wired reads a client-supplied verdict

**`regression.rs` has zero consumers.** grep across the workspace for
`RegressionSuite`, `RegressionReport`, `add_fixed_cluster`, `regressed_ids`: no hits
outside `crates/eval/`. Same for `crates/eval/src/grade.rs` (`grade`, `Trace`,
`TraceGrade`, `Signal`) and `crates/eval/src/cluster.rs` (`cluster_failures`,
`FailureCluster`, `rank_by_frequency`). **Nothing in the codebase ever constructs a
`Trace` from a real run.** STEP 7.4 in its entirety — execution-grounded graders,
deterministic failure clustering, and the regression suite that "grows with every fixed
failure" — is a well-tested library with no producer and no consumer. **Class (b).**
The careful `ClusterKey::as_key` injectivity work (`cluster.rs:37-48`) guards a queue
nothing feeds.

**A different gate is wired.** `crates/codypendentd/src/promotion.rs:87-155` handles
`PromotionAction::RunRegression` by selecting the latest `eval_suite_reports` row bound
to the candidate, deserializing `report_json` into a `SuiteReport`, deriving
`regressed = !report.all_passed()`, writing `promotion_regression_evidence`, and
calling `store.run_regression`. That path is real and reachable via `codypendent
promote advance`.

**TRUST-BOUNDARY READ.** The evidence row is written by the **CLI client**, which opens
the daemon's SQLite file directly and INSERTs — `crates/cli/src/commands.rs:1733-1751`.
It never crosses the daemon socket. The daemon re-derives `artifact_kind`/`name`/
`version` from `promotion_candidates` (good, and `crates/cli/src/commands.rs:1678-1694`
reads them from the DB rather than accepting them as arguments), but the *verdict*
— `report_json` — is an opaque blob produced by the client's own in-process scoring.
`migrations/0017_promotion_evidence.sql:1-3` claims "Callers no longer submit a bare
pass/fail boolean: the regression verdict is derived from a persisted SuiteReport" —
but the SuiteReport *is* the caller's claim, in a richer shape. Anyone who can run the
CLI can INSERT a hand-written all-passing report and clear the regression gate for any
candidate. The daemon never observes the runs it is gating on.

The human-approval gate itself is sound and I could not find a bypass:
`Candidate::approve` requires `Actor::Human` (`crates/eval/src/promote.rs:18-22`),
`PromotionRecord` derives `Serialize` but **not** `Deserialize` with private fields
(`:240-255`) so a receipt cannot be forged from JSON, `PromotionStore::approve` is the
only path that calls it (`crates/eval/src/store.rs:243-262`), and `MIN_CANARY_SAMPLES
= 100` is enforced in the state machine (`promote.rs:83`). ADR-010's invariant holds.
The weakness is upstream: the *evidence* feeding the gate is caller-asserted.

### 16.6 `--policy` doc drift and practical unusability

- **Drift.** `crates/cli/src/eval.rs:44-54` and `:476` (`model: routed_model`) confirm
  the routed model IS pinned into `StartRun.model`. The `--help` text a user actually
  reads says the opposite: "it does not yet pin the daemon's own `StartRun` execution
  to that model". `evals/README.md`'s "Deferred" section is also stale: "Routing-policy
  enforcement. A case's `policy` field ... does not yet select a model". Two of three
  user-facing surfaces contradict the code.
- **Unusable in practice.** `crates/cli/src/eval.rs:190` — `KNOWN_POLICIES = &["balanced"]`,
  one name. `crates/cli/src/eval.rs:298` classifies every case at
  `DataClassification::Unknown`; `balanced`'s ceiling is `Confidential`; so only LOCAL
  models can ever be selected (acknowledged at `eval.rs:66-69`). Combined with 11.4
  (hosted models are always ineligible anyway), `eval run --policy balanced` requires a
  benched local model or hard-fails at `crates/cli/src/eval.rs:247-254`. A router
  promotion candidate is *required* to use `--policy` (`crates/cli/src/commands.rs:
  1689-1694`), so router candidates cannot be evaluated without one.

---

## What I could not exercise, and why

- **`codypendent eval run` against a live model.** No API key and no local model
  runtime on this host, so I could only exercise the no-model path. That path is what
  surfaced 16.3, but it means I could not observe what a *real* case run scores, nor
  confirm whether `tests_passed` / `changed_files` / `approval_requested` are populated
  correctly from a run that actually did work. The repository-inspection half
  (`inspect_repository`) did run — the fixture cloned and `cargo test` executed.
- **The routing seam end to end.** Enabling it requires a hand-written `routing.toml`
  *and* at least one benched profile, and `models bench` requires a reachable model
  endpoint. I verified the seam's logic by reading it and by its own tests, and verified
  the config-loading and default-OFF behaviour, but did not observe a live routing
  decision note in a real run.
- **The OS sandbox actually confining a process.** `bwrap` is not installed on this
  host, so `LinuxSandbox::new()` fails closed and no confined process could be spawned.
  I read `bwrap_argv` and `seatbelt_profile` (both pure and unit-tested here) and
  confirmed the refusal paths, but the real kernel denials are only demonstrated by
  `crates/sandbox/tests/enforcement_it.rs`, which is macOS-gated.
- **The escalation path in production.** It is `#[cfg_attr(not(test), allow(dead_code))]`,
  so there is no production path to drive.
- **The promotion pipeline end to end.** I read the state machine, the store, and the
  daemon gateway, and confirmed the SQL and the trust boundary, but did not drive
  `promote propose → eval run --candidate-id → promote advance → approve` because the
  regression gate needs a passing eval report, which needs a live model.

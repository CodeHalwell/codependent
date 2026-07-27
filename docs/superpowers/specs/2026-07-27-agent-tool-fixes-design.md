# Agent & tool fixes — design

**Date:** 2026-07-27 · **Status:** proposed · **Branch:** `claude/agent-fixes` (off main)

## Problem

Three real defects surfaced while a user reviewed a **Python** repository. Evidence is
from the live event ledger.

1. **Blackboard advertise/execute mismatch.** A plain (non-workflow) Build run's model
   called `blackboard.post` and got `ToolCompleted { Failed, message: "unknown tool
   `blackboard.post`" }`. The `blackboard.*` tools are workflow-only, yet they are
   advertised to the model regardless of the run's workflow binding — so the model is
   offered a tool that dispatch will always refuse. Wasted calls, confusion, and fuel for
   looping.

2. **Shell allow-list too narrow for non-Rust repos.** `shell.run` failed with
   `policy denied: `ls` is not in the shell allow-list` (and the same for `find`). The
   built-in default allow-list is Rust-only (`cargo`, `git`, `rg`, `rustfmt`), so basic
   exploration in a Python repo was denied and the agent fell back to repeated file reads.

3. **Loop resilience.** The agent re-read the same file dozens of times. The `shell.run`
   denial message states the fact but offers no next step, so the model retried instead of
   switching to the structured tools that would have worked.

## Goals

- A run is advertised **exactly** the tools its dispatch layer will accept — no
  workflow-only tool is offered to a non-workflow run; real workflow runs are unchanged.
- Basic, read-only repository exploration works out of the box for any language, not just
  Rust, without weakening the security boundary.
- A shell denial tells the model what to do instead, so it changes strategy rather than
  looping.

## Non-goals

- **No approval-policy change.** Every allow-listed shell command still requires approval
  (see Reconciliation R2); this spec does not make read-only commands auto-run. That is a
  separate security decision, called out as a follow-up.
- **No heavy loop detection** (e.g. "N identical consecutive calls → intervene"). Deferred
  as a follow-up (see §"Loop resilience decision").
- **No OS-sandbox work** on the `shell.run` path (see Reconciliation R3). The confinement
  story is documented as-is; changing it is out of scope.
- No protocol / wire / golden-vector change. These are internal policy, tool-advertisement,
  and agent-loop changes.

---

## Verified code map (line numbers confirmed against branch `claude/agent-fixes`)

| Concern | Location | Fact |
|---|---|---|
| Provider tool advertisement | `crates/runtime/src/agent.rs:2238` `tool_definitions()`; used at `:2430` `options.tools = Self::tool_definitions()` in `FrameworkModelDriver::next_step` | Static, **unconditional** — returns the full tool set including `blackboard.*` regardless of run. |
| Single source of truth | `crates/runtime/src/agent.rs:796` `offered_tool_names(&self, run)` | **Already** workflow/GitHub-aware; documented as "the single source of truth the model-facing advertisement and `prepare` agree on" — but `tool_definitions()` does **not** consult it. |
| Workflow-binding predicate | `crates/runtime/src/agent.rs:787` `offers_blackboard(run)` | `self.blackboard.is_some() && run.workflow.is_some()`. |
| Dispatch gate | `crates/runtime/src/agent.rs:1432,1445` (guarded arms) → `:1458` `other => Err(format!("unknown tool `{other}`"))` | A solo run's `blackboard.post` falls through to the unknown-tool arm — **exactly the evidence message**. |
| Dead defensive fallback | `crates/runtime/src/agent.rs:1933` `blackboard_unavailable()` | Unreachable in practice (`prepare` gates the tool); returns "…only available inside a workflow run", **not** "unknown tool". |
| Driver trait / loop | `crates/runtime/src/agent.rs:307` `trait ModelDriver`; `:318` `next_step(&self, transcript, sink)`; loop call `:934` inside `execute_run` `:837` | `execute_run` has both `self` (runtime, owns `blackboard`) and `run` (owns `workflow`); the driver's `next_step` gets neither. |
| Default allow-list | `crates/daemon/src/policy/config.rs:100-105` `builtin_defaults().shell_allowed_programs` | `["cargo","git","rg","rustfmt"]`. |
| Merge invariant | `crates/daemon/src/policy/config.rs:138-141` (shell uses `intersect_exact`, `:209-214`) | Overlays only **narrow** the allow-list; the base cannot be widened by a file layer. |
| Policy construction (run start) | `crates/codypendentd/src/executor.rs:461-465` | `with_defaults_allowing_network([GITHUB_API_ENDPOINT])` when a GitHub client is set, else `with_defaults()`. **No** `PolicyEngine::load` — file overlays are not applied on this path today. |
| Base-augmentation pattern | `crates/daemon/src/policy/mod.rs:194-198` `with_defaults_allowing_network` | Mutates `builtin_defaults()` **before** `from_merged` computes the version hash — the exact seam to widen the base for a run. |
| Command evaluation | `crates/daemon/src/policy/mod.rs:357-379` `eval_command` | Non-listed → **Deny** (`:365-370`, code `policy.program-not-allowlisted`, message `` `{program}` is not in the shell allow-list ``); listed → **RequireApproval** (`:372-378`). Never reads `shell_interpreter_requires_approval`. |
| Denial → transcript | `crates/runtime/src/agent.rs:1214` (ledger `ToolCompleted.message`) and `:1221` (model observation), both `format!("policy denied: {reason}")` | `PolicyReason` carries `code` **and** `message` (both public) — the runtime can branch on `code`. |
| Repository path at run start | `crates/codypendentd/src/executor.rs:491,531` `&launch.repository` | The repo root `Path` is in scope where the policy is built — available for language detection. |
| Structured exploration tools | `read_file.rs:50` `workspace.read_file`; `search.rs:58` `workspace.search` | Real tool names for the actionable message. |
| No OS sandbox on `shell.run` | `crates/runtime/Cargo.toml` (no `codypendent-sandbox` dep); `crates/runtime/src/tools/shell.rs:120-243` | The child is spawned via `tokio::process::Command` with `env_clear` + process group + timeout + cwd-in-scope. The `codypendent-sandbox` crate (bwrap/`sandbox-exec`) backs the **plugin/skill** path only. |
| Worktree isolation | `crates/daemon/src/worktrees.rs:222+` `allocate` | A Build run gets a disposable `git worktree` branch checked out **outside** the repo; a read-only run keeps the repo root as cwd (`executor.rs:498-503`). |

---

## FIX 1 — advertise only the tools a run can dispatch

### Root cause (reconciled)

The advertise/execute divergence is **narrow and already half-solved**. `offered_tool_names`
(`agent.rs:796`) is the correct, workflow/GitHub-aware set, and `prepare` gates dispatch on
it. The single gap: the provider-facing advertisement — `FrameworkModelDriver::next_step`
setting `options.tools = Self::tool_definitions()` — bypasses `offered_tool_names` and sends
the **full static catalog**. Aligning the advertisement to `offered_tool_names` fixes the
reported blackboard bug **and** the same latent over-advertisement of `github.*` tools to a
run with no GitHub target, making the "single source of truth" doc literally true.

### Design

Make the provider advertisement a **filtered projection of `offered_tool_names(run)`**:

1. **`tool_definitions()` becomes the schema catalog only.** Keep it (`agent.rs:2238`)
   returning every tool's `ToolDefinition` (name + description + JSON schema). Membership is
   no longer decided here.
2. **Thread the per-run offered set through the driver.** Extend the `ModelDriver` trait
   (`agent.rs:318`) so `next_step` receives the offered tool names for the run:

   ```
   async fn next_step(
       &self,
       transcript: &[TurnItem],
       offered_tools: &[&str],   // NEW — the names offered_tool_names(run) returned
       sink: &mut dyn DeltaSink,
   ) -> anyhow::Result<StepOutcome>;
   ```

3. **The loop passes the truth it already owns.** In `execute_run` (`agent.rs:837`), before
   the `next_step` call at `:934`, compute once:

   ```
   let offered = self.offered_tool_names(&run);   // Vec<&'static str>, already correct
   ```

   and pass `&offered` into `next_step`. The loop has both `self.blackboard` and
   `run.workflow`, so `offered` reflects `offers_blackboard(run)` exactly.
4. **`FrameworkModelDriver::next_step` filters the catalog** (`agent.rs:2430`):

   ```
   options.tools = Self::tool_definitions()
       .into_iter()
       .filter(|def| offered_tools.iter().any(|name| *name == def.name))
       .collect();
   ```

   Now the advertised set is byte-for-byte the set `prepare` will accept.
5. **`ScriptedDriver` and the test drivers** (`agent.rs:366`, and the in-test impls) accept
   `offered_tools` and ignore it — they do not advertise to a real provider. Mechanical
   signature update only.
6. **Comment hygiene.** Update the stale note at `agent.rs:2353-2356` ("advertised here like
   the `github.*` tools … gated at dispatch") to state that advertisement is now the
   `offered_tool_names` projection. `blackboard_unavailable` (`:1933`) stays as documented
   defense-in-depth (still unreachable via `prepare`).

**Workflow-only tool set = `{ blackboard.post, blackboard.query }`** — the only tools whose
dispatch checks `run.workflow` (`agent.rs:1695,1740`). The filter is name-driven, so any
tool later added to `offered_tool_names` is advertised correctly with no further change.

**No behavior change for real workflow runs:** a workflow node has `run.workflow = Some`
and a wired channel, so `offered_tool_names` still includes the blackboard tools and they
are advertised exactly as before.

---

## FIX 2 — a read-only exploration baseline, per-language aware

Two layers, both **base-policy** changes (System layer), never overlays — so the merge
invariant ("a file overlay may only narrow") is untouched.

### 2a. Curated read-only default (approach 1)

Expand `builtin_defaults().shell_allowed_programs` (`config.rs:100-105`) from the four
Rust tools to add a language-agnostic, read-only exploration set. **Proposed final default:**

```
cargo, git, rg, rustfmt,                          # unchanged (Rust baseline)
ls, cat, head, tail, wc, pwd, tree, file,         # pure read-only — no write/exec surface
which,                                            # PATH lookup (read-only)
sort, uniq,                                        # read-only in common form (see caveat)
grep, find                                         # read-only in common form (see caveat)
```

**Security rationale — per command.** The allow-list is **program-name-based with no flag
filtering** (`scope.rs:126-128`, exact string match), so the argument surface matters. Each
entry is justified against the *real* confinement (see Reconciliation R2/R3): allow-listing
a program makes it **eligible to be proposed for approval**, not auto-run — every allow-listed
`shell.run` invocation still returns `RequireApproval` and surfaces the exact
`program + argv + cwd` on the approval card before anything spawns. The child runs with a
cleared environment, in its own process group, under a timeout, with cwd pinned to the
disposable worktree (Build) or the repo root (read-only run).

| Command | Write? | Exec other programs? | Verdict |
|---|---|---|---|
| `ls`, `pwd`, `tree`, `file`, `wc`, `head`, `tail`, `cat`, `which` | No (argv-only; no shell ⇒ no `>` redirection) | No | **Safe.** Pure read/inspect. `tail -f` can block; the timeout bounds it. |
| `grep` | No (no shell redirection) | **No** — GNU/BSD `grep` has no `-exec`/action flag; `-f`/`--include` only *read* patterns | **Safe.** The "grep with actions" worry does not apply to real grep. |
| `sort`, `uniq` | **Minor** — `sort -o FILE` / `uniq IN OUT` can write one file | No | **Include, flagged.** Any write is visible on the approval card and bounded by OS perms + the disposable cwd. |
| `find` | **Yes** — `-delete` removes files | **Yes** — `-exec`/`-execdir`/`-ok` run arbitrary programs, bypassing the allow-list (find spawns them itself) | **Highest residual risk; include only after explicit human vet.** Its *common* use (locate files) is read-only, and every destructive form is visible on the approval card. If vetoed, drop it: `rg` (already default) + `ls`/`tree` cover most exploration and neither can delete or exec. |

**Excluded and why:** all interpreters and code-execution multiplexers — `sh`, `bash`,
`zsh`, `python`/`python3`, `node`, `ruby`, `perl`, `make`, `npm`/`yarn`/`pnpm`, `go`, `mvn`,
`gradle`, `java`, `docker`. Program-name allow-listing cannot separate an inspect subcommand
(`go doc`) from an exec subcommand (`go run`), so any such multiplexer is treated as an
interpreter and kept off the always-allow set. Running them stays the **approval path** (add
per-repo policy, or a future explicit grant), not the default allow-list.

### 2b. Per-language augmentation (approach 3)

Detect the project's language(s) at run start and augment the **base** allow-list with that
language's safe, non-interpreter **inspection** tools.

**Where it hooks.** In `executor.rs` where the policy is built (`:461-465`), before overlays
would apply and before the version hash is computed. Add a base-augmentation constructor to
`PolicyEngine` mirroring `with_defaults_allowing_network` (`mod.rs:194-198`):

```
// crates/daemon/src/policy/mod.rs
pub fn with_defaults_allowing(
    network_endpoints: impl IntoIterator<Item = String>,
    extra_programs:    impl IntoIterator<Item = String>,
) -> Self {
    let mut merged = MergedPolicy::builtin_defaults();
    merged.network_allow.extend(network_endpoints);
    for p in extra_programs {                    // dedup, order-preserving
        if !merged.shell_allowed_programs.contains(&p) {
            merged.shell_allowed_programs.push(p);
        }
    }
    Self::from_merged(merged)                      // version hash covers the widened base
}
```

`executor.rs` then becomes:

```
let extra = detect_project_languages(&launch.repository)   // BTreeSet<Language>, sorted
    .into_iter()
    .flat_map(language_inspection_programs)                // Vec<String>
    .collect::<Vec<_>>();
let network = if self.github.is_some() {
    vec![GITHUB_API_ENDPOINT.to_string()]
} else { vec![] };
let policy = PolicyEngine::with_defaults_allowing(network, extra);
```

`with_defaults_allowing_network` is retained (re-expressed via the new constructor or kept)
so the GitHub-network behavior is byte-identical.

**Detection.** `detect_project_languages(repo_root: &Path) -> BTreeSet<Language>` does a
shallow, read-only marker scan of the repo root (sorted output ⇒ deterministic):

| Marker (in repo root) | Language |
|---|---|
| `Cargo.toml` | Rust |
| `pyproject.toml` / `requirements.txt` / `setup.py` / any `*.py` | Python |
| `package.json` | Node |
| `go.mod` | Go |
| `pom.xml` / `build.gradle` / `build.gradle.kts` | JVM |
| `Gemfile` | Ruby |

**`language_inspection_programs(lang) -> &'static [&'static str]` is EMPTY for every
language today**, and this is deliberate, not an omission:

- Rust adds nothing (`cargo`/`rustfmt` are already in the base).
- Python/Node/Go/JVM/Ruby: each language's characteristic tool is either the **interpreter**
  (excluded by rule) or a **multiplexer** whose program name cannot be separated from a
  code-execution subcommand (`go`, `npm`, `mvn`, `gradle`). No provably-read-only,
  non-multiplexer, commonly-present binary remains to add.

The universal set from 2a already covers cross-language exploration (it is what the user
actually needed in the Python repo). The detection + augmentation **seam** is specified and
tested so a future, provably-safe, per-language tool can be added by populating the table
alone — with no engine change. **See "Open decision D2": because the table is empty, 2b
changes no runtime behavior today and MAY be deferred; the reviewer decides ship-seam vs
defer.**

### PolicyVersion

Expanding `builtin_defaults` (2a) changes the merged policy's canonical serialization, so the
`PolicyVersion` SHA-256 (`mod.rs:587-591`) changes **once, globally** — expected and benign
(it is an internal cache/audit key, not a wire contract). 2b changes the hash for a run only
when `extra` is non-empty (never, today). Document both.

---

## FIX 3 — an actionable shell denial (loop resilience, lightweight)

### The exact message and where it is produced

The user-visible string is assembled from two sites:

- **Reason** (`daemon/src/policy/mod.rs:366-369`, `eval_command`): code
  `policy.program-not-allowlisted`, message `` `{program}` is not in the shell allow-list ``.
- **Prefix** (`agent.rs:1214` ledger message, `:1221` model observation):
  `format!("policy denied: {reason}")`.

Together: `policy denied: `ls` is not in the shell allow-list` — the exact evidence. (The
runtime's own `ToolError::ProgramNotAllowed` "…command allow-list" text, `tools/mod.rs:99`,
is a **defensive, unreached** path: the policy engine denies at step (b) before the shell
tool's guard at step (d) ever runs — see Reconciliation R4.)

### Design (runtime-side augmentation — correct layer)

Keep the policy engine tool-agnostic: it continues to emit the stable
`policy.program-not-allowlisted` **code** and its factual message, unchanged. The runtime —
the only layer that legitimately knows its own tool names — appends the coaching where the
denial becomes a transcript observation.

At the deny arm in `agent.rs:1200-1221`, branch on the reason **code**:

```
let mut text = format!("policy denied: {reason}");
if decision.reasons.first().map(|r| r.code.as_str()) == Some("policy.program-not-allowlisted") {
    text.push_str(
        " — to inspect the repository use the `workspace.read_file` and `workspace.search` \
         tools instead of a shell command.",
    );
}
```

Use `text` for both the ledger `ToolCompleted.message` (`:1214`) and the returned
`Observation` (`:1221`) so the trace and the model see the same thing. The machine `code` is
unchanged; only human-facing text changes.

**Why this still matters after FIX 2:** the added commands (`ls`, `find`, …) now hit the
approval path, not deny — but any *other* non-listed program (`npm`, `make`, `docker`, an
interpreter, a random binary) still denies here, and the hint steers the model to the
structured tools rather than a retry loop.

### Loop resilience decision

**In-scope (this spec):** the actionable denial message above — the minimal, YAGNI-aligned
intervention that breaks the observed retry loop by giving the model a concrete alternative.

**Deferred follow-up (non-goal):** structural loop detection (e.g. tracking N identical
consecutive tool calls and forcing a strategy change or terminating). It is non-trivial
(needs per-run call-signature history, a threshold policy, and a new intervention event) and
is not justified by the evidence once the denial is actionable and the exploration commands
exist. Recorded here as the explicit decision, not built.

---

## Data flow (after the fixes)

1. **Run start** (`executor.rs`): detect languages from `launch.repository`; build
   `PolicyEngine::with_defaults_allowing(network, extra)` — base allow-list = curated
   read-only set (+ any non-empty language additions). Version hash reflects the base.
2. **Each model step** (`execute_run` → `next_step`): loop computes
   `offered = self.offered_tool_names(&run)`; driver advertises exactly the filtered
   catalog. A non-workflow run never sees `blackboard.*`; a run with no GitHub target never
   sees `github.*`.
3. **Tool call → `prepare`**: recognized iff in the offered set (blackboard gated by
   `offers_blackboard`), else "unknown tool" (unchanged defense-in-depth).
4. **Policy `evaluate`**: a non-listed program denies with the now-**actionable** message; a
   listed program returns `RequireApproval` (approval card shows `program/argv/cwd`).

---

## Error handling

- **Language detection failure** (unreadable dir, missing markers): yields an empty set ⇒
  no augmentation ⇒ base curated default only. Detection never fails a run.
- **`find`/`sort`/`uniq` destructive forms**: not silently confined — surfaced on the
  approval card (every allow-listed command is `RequireApproval`) and boundable there. The
  reviewer may veto `find` (§2a) to remove the surface entirely.
- **Filter/advertisement**: if `offered_tool_names` and `tool_definitions()` names ever drift
  (a tool in one but not the other), the filter simply omits the unmatched entry — fail-safe
  (a tool is never advertised beyond what dispatch accepts). A test pins the two in sync.
- **Denial message**: only the human string changes; the machine `code`
  (`policy.program-not-allowlisted`) is the stable contract and is preserved.

---

## Testing

**FIX 1**
- *Advertisement matches dispatch (new, unit):* build a `FrameworkModelDriver`, assert the
  filtered `options.tools` names equal `offered_tool_names(run)` for (a) a solo Build run —
  **excludes** `blackboard.post`/`blackboard.query`; (b) a workflow node
  (`with_workflow(..)`) with a wired channel — **includes** both.
- *Regression:* extend/keep `blackboard_tools_are_offered_only_inside_a_workflow_run`
  (`agent_it.rs:1818`) — `offered_tool_names` assertions stand; the solo scripted
  `blackboard.post` still gets the "unknown tool" refusal (defense-in-depth intact).
- *GitHub projection (new):* a run with no `github_repo` is not advertised the `github.*`
  tools; a run with a target is.

**FIX 2**
- *Default set:* `builtin_defaults().shell_allowed_programs` contains the curated set
  (and still the original four).
- *Detection:* `detect_project_languages` returns `{Python}` for a `pyproject.toml`/`*.py`
  fixture, `{Rust}` for `Cargo.toml`, `{Node}` for `package.json`, `{Go}`, `{JVM}`, `{Ruby}`
  for their markers, and `∅` for a bare dir; multiple markers ⇒ the union.
- *Augmentation seam:* `with_defaults_allowing([], ["dummytool"])` yields a command scope
  that `allows_program("dummytool")` and still `allows_program("cargo")`; dedup holds
  (passing `"cargo"` does not duplicate it).
- *Enforcement unchanged:* a non-listed program (`rm`) still denies
  (`command_requires_approval_and_rejects_unlisted`, `mod.rs:680`, stands); a curated
  program (`ls`) now evaluates to `RequireApproval`, not `Deny`.
- *Merge invariant:* an overlay `allowed_programs = ["cargo"]` still narrows to `["cargo"]`
  (`shell_allow_list_only_narrows`, `config.rs:481`, stands).

**FIX 3**
- *Actionable message:* a denied non-listed program's observation contains both
  `is not in the shell allow-list` and `workspace.read_file`/`workspace.search`; the reason
  **code** is still `policy.program-not-allowlisted`.
- *Scoped:* a different denial (e.g. write-denied-by-mode) does **not** get the shell hint.

---

## Constraints

- **Security — the allow-list is a boundary.** Only read-only / common-form-safe commands
  are added; **no interpreter or code-exec multiplexer** enters the always-allow set. The
  real confinement (approval gate + `env_clear` + process group + timeout + cwd pinned to a
  disposable worktree) is **unchanged**. The final command set and the per-command rationale
  are tabulated (§2a) so a human can veto individual entries — `find` is called out as the
  entry most warranting veto.
- **Honesty invariant.** No fabricated telemetry; unmeasured cost stays `None`/`—`. Untouched.
- **Deterministic `PolicyVersion`.** The hash changes once from the base expansion (2a);
  documented. Serialization stays canonical/deterministic.
- **Additive / no regression.** The four current Rust commands remain; real workflow runs
  still get `blackboard.*`; existing Rust-project behavior is unchanged. No protocol / wire /
  golden change — policy + tool-advertisement + agent-loop internals only.

---

## Reconciliations (spec vs. real code)

- **R1 — the bug is narrower than framed; a single source of truth already exists.**
  `offered_tool_names` (`agent.rs:796`) is already workflow/GitHub-aware and is what `prepare`
  honors; the defect is solely that `tool_definitions()` (the provider path) bypasses it. The
  evidence "unknown tool `blackboard.post`" is the `prepare` fall-through arm (`:1458`), **not**
  `blackboard_unavailable` (`:1933`, which is dead/defensive). Fixing the advertisement to
  project `offered_tool_names` also removes the latent over-advertisement of `github.*`.

- **R2 — every allow-listed command already requires approval.**
  `eval_command` (`mod.rs:372-378`) returns `RequireApproval` for **all** allow-listed
  programs and **never reads** `shell_interpreter_requires_approval` (that field is parsed,
  merged, and defaulted `true` at `config.rs:73,106,142`, but unenforced). So adding a command
  moves it **Deny → RequireApproval**, not Deny → auto-run. This makes FIX 2 *safer* than the
  approval-free model assumed, but means read-only exploration will **prompt for approval**
  each time. If the desired UX is "read-only exploration runs without prompting," that is a
  separate change (make read-only allow-listed commands `Allow`, or actually wire
  `shell_interpreter_requires_approval`) and is **out of scope** here.

- **R3 — there is no OS sandbox on the `shell.run` path.**
  `crates/runtime` has no `codypendent-sandbox` dependency; `Shell::execute`
  (`shell.rs:120-243`) spawns via `tokio::process::Command` with `env_clear` + process group +
  timeout only. The bwrap/`sandbox-exec` sandbox backs the **plugin/skill** path, not
  `shell.run`. The policy `fs_write=$WORKTREE` / `network=Deny` / `.git`/secret denies bind the
  **daemon's own tools** (`workspace.read_file`, `apply_patch`, network), **not** arbitrary
  spawned subprocesses. The security rationale in §2a therefore rests on the **approval gate +
  read-only-common-form + disposable-worktree cwd**, not on OS-level write confinement. (This
  corrects the task's "OS sandbox confines writes to the worktree" premise for this path.)

- **R4 — the message is the policy-engine reason, not the runtime `ToolError`.**
  The user-visible "…shell allow-list" text is the `eval_command` reason (`mod.rs:368`),
  reached at step (b) of the tool loop before execution; the runtime's
  `ToolError::ProgramNotAllowed` "…command allow-list" (`tools/mod.rs:99`) is the shell tool's
  own guard at step (d), which the deny path never reaches. FIX 3 therefore edits the
  observation built from the policy reason (`agent.rs:1200-1221`), keying on the reason code.

---

## Open decisions for the reviewer

- **D1 — the command set (highest priority to vet).** Approve the curated default (§2a),
  especially **`find`** (has `-delete`/`-exec`; visible on every approval card but the highest
  residual surface). Veto individual entries as desired; the conservative fallback drops
  `find` and relies on `rg`/`ls`/`tree`.
- **D2 — ship or defer 2b.** Because `language_inspection_programs` is empty for every
  language today, per-language augmentation changes no behavior now. Ship the detection +
  seam (future-proofing, fully tested) or defer it as a follow-up and land only 2a — 2a alone
  resolves the reported Python-repo bug.
- **D3 — approval UX (flagged, not scoped).** Given R2, read-only exploration will prompt for
  approval per invocation. Confirm that is acceptable, or open a separate task to let
  read-only allow-listed commands run without prompting.

---

## Components (seed the plan's tasks)

1. **Blackboard/advertisement filter** — extend `ModelDriver::next_step` with
   `offered_tools: &[&str]`; loop passes `offered_tool_names(&run)`;
   `FrameworkModelDriver` filters `tool_definitions()` by it; update drivers + comments.
2. **Default allow-list expansion** — add the curated read-only set to
   `builtin_defaults().shell_allowed_programs`.
3. **Per-language detection + policy augmentation** — `detect_project_languages` +
   `language_inspection_programs` (empty table) + `PolicyEngine::with_defaults_allowing`;
   hook in `executor.rs` (gated by D2).
4. **Actionable denial message** — branch on `policy.program-not-allowlisted` in the runtime
   deny arm and append the `workspace.read_file`/`workspace.search` hint.

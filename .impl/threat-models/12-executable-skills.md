# Threat model — Outcome 12: executable skills (sandboxed WASM)

Written before the first line of code, per `.impl/BRIEF.md` rule 4.
Owner: agent-wasm. Files: `crates/sandbox/**`,
`crates/knowledge/src/{skill_exec,manifest}.rs`.

---

## 0. THE ARCHITECTURAL DECISION (made first, binding on everything below)

### 0.1 The problem

There are two `Capability` enums and no conversion between them.

| | `crates/daemon/src/policy/scope.rs:30` | `crates/sandbox/src/permission.rs:25` |
|---|---|---|
| Name | **run capability** | **plugin capability** |
| Shape | structured scopes: `FileRead(PathScope)`, `CommandExecute(CommandScope)`, `NetworkConnect(NetworkScope)`, `GitCommit`, `McpToolCall{server}`, … | flat strings: `FilesystemRead(String)`, `Network(String)`, `Secret(String)`, `Subprocess` |
| Source | merged operator policy (`policy.toml` layers) + mode overlay | a package's own `plugin.toml` / `skill.toml` |
| Authority | **enforced** — `PolicyEngine::evaluate` is the single gate every side effect passes (`crates/daemon/src/policy/mod.rs:314`) | **declarative** — drives the install/update permission diff and lowers into `SandboxProfile` |
| Trust | operator-authored, trusted | **package-authored, untrusted** |

Outcome 12 says executable skills run "under the *existing* deny-first policy".
If the WASM host gates on the sandbox enum, the run policy never sees a skill's
side effects and we have shipped a second, weaker policy path — the exact
escalation route outcome 13's model must forbid.

### 0.2 The constraint that decides it

`crates/daemon/Cargo.toml` already depends on `codypendent-sandbox`.
**`crates/sandbox` therefore cannot depend on `crates/daemon`** — it would be a
dependency cycle. "Gate the host on the run enum directly" is not available to
code living in `crates/sandbox`, and `crates/sandbox` is where the WASM host
belongs (it is the crate that draws the trust boundary and carries no daemon
code, which is why its security decisions are testable in isolation).

Nor may `crates/sandbox` mint a third enum that *mirrors* the run capabilities:
that is the same defect one level down — a second policy vocabulary that drifts.

### 0.3 The decision

**The sandbox capability set is a CEILING. The run policy is the AUTHORITY.
Every privileged act a WASM guest attempts is lowered into a *request* that
re-enters the run policy through an inverted trait, and is refused unless BOTH
gates say yes.**

Concretely:

1. `crates/sandbox` defines `HostRequest` (`src/gate.rs`) — the vocabulary of
   things a guest can *ask for*: `ReadFile{path}`, `WriteFile{path}`,
   `RunCommand{program,args}`, `Connect{host,port}`, `ReadSecret{name}`.
   `HostRequest` is deliberately **isomorphic to `ProposedAction`'s privileged
   subset**, not to `permission::Capability`. It is a request, never a grant.
2. `crates/sandbox` defines `trait RunPolicyGate { fn authorize(&self,
   &HostRequest) -> Result<GateGrant, GateDenied>; }` and the default
   `DenyAllGate`. The WASM host **cannot be constructed without a gate**
   (`WasmHost::new` takes `Arc<dyn RunPolicyGate>`; there is no `Default`).
3. The daemon-side implementation of that trait — the only place both enums are
   visible — maps `HostRequest` → `ProposedAction` → `PolicyEngine::evaluate` →
   `policy::Capability` grant. That mapping is the "lowering", and it lives at
   the one layer that legitimately sees both models. It is written as a
   proposal (`.impl/proposals/daemon-from-agent-wasm.md`) because
   `crates/daemon` is not mine; the trait, its contract, its conformance test
   suite, and the fail-closed default all ship here.
4. The package's own declaration is applied as a **pre-filter, before** the
   gate: a request the manifest never declared is refused without ever reaching
   the run policy. So a skill cannot use the run policy's generosity to exceed
   its own manifest, and cannot use its own manifest to exceed the run policy.
   Two gates, AND-composed, evaluated declaration-first so the cheap
   deterministic check runs before the stateful one.

### 0.4 Why not the alternatives

* *Gate on `permission::Capability` alone* — rejected: ships the second, weaker
  policy path the brief forbids. A skill declaring `filesystem_write =
  ["/"]` would be enforced only against its own claim.
* *Gate on `policy::Capability` directly inside `crates/sandbox`* — rejected:
  dependency cycle (§0.2). Moving `policy/scope.rs` into `crates/sandbox` would
  drag `PathScope` canonicalization, `MergedPolicy`, and `EvalContext` across
  the boundary and make the sandbox crate depend on the policy file format —
  the inverse of why the crate exists.
* *Convert `permission::Capability` → `policy::Capability` in a shared crate* —
  rejected: the conversion is not total. `Capability::Network(String)` carries a
  bare `host:port` with no `NetworkDefault`; `CommandExecute` needs a
  `maximum_seconds` the skill manifest does not have; `PathScope` needs
  canonical roots *and a deny list* the package cannot supply. Any total
  conversion has to invent the missing halves, and inventing a deny list is
  exactly how you widen a policy by accident. Making it a *request* that the
  run policy fills in from operator config removes the invention.

### 0.5 The invariant this buys, stated so it can be tested

> No `HostRequest` is ever satisfied without a `GateGrant`, and a `GateGrant`
> is constructible only by a `RunPolicyGate` implementation. `GateGrant` has
> private fields, no `Default`, no `Deserialize`, and no public constructor
> outside the trait's own module — so it cannot be forged from guest-supplied
> JSON, and a future host function that forgets to call the gate fails to
> compile rather than failing open.

This is the same construction `crates/eval/src/promote.rs:240-255` uses for
`PromotionRecord` (Serialize but not Deserialize, private fields) and that the
2026-08-13 review found sound. Reused deliberately.

---

## 1. What crosses the boundary

A **skill package**: a directory holding `skill.toml`, `SKILL.md`, and
optionally `scripts/`, `references/`, `tests/`, and (new) a `.wasm` module.
It arrives via `codypendent skill add <dir>` or a registry sync, is parsed by
`crates/knowledge/src/manifest.rs::load_package`, and is content-hashed.

Everything in the directory is attacker-controlled bytes.

## 2. What the attacker controls

| Surface | Controlled? | Notes |
|---|---|---|
| `skill.toml` every field | **yes** | including `[permissions]`, `[limits]`, `[trust] publisher`, `[trust] signature_required`, `scope` and `status`. Nothing in `[trust]` is a decision input — see §8 |
| The WASM module bytes | **yes** | arbitrary valid or malformed wasm, unbounded loops, huge memories, deep recursion |
| `scripts/*` bytes and mode bits | **yes** | including the shebang |
| Module *imports* it declares | **yes** | can request `wasi_snapshot_preview1`, `env.system`, anything |
| Everything written to stdout / the host-call return path | **yes** | this is the prompt-injection channel into the model transcript |
| Package file names/paths | **yes** | including `../` and symlinks |
| Which host functions it calls, in what order, how often | **yes** | |
| The *values* it passes to host functions | **yes** | paths, host:port, command argv |

The attacker does **not** control: the operator's `policy.toml`, the
`EvalContext` (repository/worktree roots), the trusted-publisher store, or the
approval UI.

## 3. What is denied by default

The default posture is "a WASM guest is a pure function over the bytes it is
handed". Every item below is off unless something explicitly turns it on:

1. **No imports at all.** The host instantiates with an import object
   containing only the `codypendent` module's declared functions. `wasmi`
   linker resolution fails closed on an unresolved import, so a module
   importing `wasi_snapshot_preview1.*`, `env.*`, or anything else refuses to
   instantiate. WASI preview 1 and 2 are not linked, not compiled in, and the
   `wasmi_wasi` crate is not a dependency.
2. **No filesystem.** There is no host function that opens a path by name and
   returns a handle without a `GateGrant`.
3. **No network.** Same. Additionally `SandboxProfile.network_allowlist`
   non-empty is already refused by `validate_enforceable_profile`
   (`executor.rs:1237`) for the process path; the WASM path inherits the same
   refusal until a broker exists (§6).
4. **No clock, no randomness, no environment, no argv** beyond what the host
   passes as the single input buffer. A guest cannot read `PATH`, cannot read
   the daemon's env, cannot see the wall clock (so it cannot time-side-channel
   the host cheaply, and — more importantly — its behaviour is reproducible for
   audit).
5. **No subprocess.** wasm has no such primitive and no host function offers one
   without a `RunCommand` grant.
6. **Bounded compute.** `Config::consume_fuel(true)`; the store is fuelled from
   `[limits]`, and exhaustion traps.
7. **Bounded memory.** A `ResourceLimiter` refuses `memory.grow` and
   `table.grow` past the manifest's cap. Refused growth is a guest-visible
   `-1`, not a host abort.
8. **Bounded wall clock.** §5.
9. **Bounded output.** The guest's returned bytes are capped and then passed
   through `sanitize_untrusted` with the skill's origin label, exactly like
   process output — the existing single chokepoint (`sanitize.rs`).
10. **Deterministic float / no SIMD-timing tricks**: `wasmi` is an interpreter;
    there is no JIT, so no W^X pages, no signal handlers, no code generation
    from guest input.

## 4. Runtime choice — wasmi, not wasmtime, and why

The brief says "prefer wasmtime with fuel metering + epoch interruption". I did
not. The reasons, so this is a reviewable decision rather than a silent
substitution:

* **Build budget is the binding constraint of this environment** (BRIEF §
  "Environment"). Measured with `cargo metadata` on a scratch crate:
  `wasmtime 38` with `default-features = false, features = ["cranelift",
  "runtime", "std"]` resolves to **97 packages** including the whole Cranelift
  code generator and `regalloc2`. `wasmi 0.51` resolves to **30**, of which
  ~14 are new to this tree. On 4 CPUs with ~12 GB free and eleven parallel
  agents that have already filled the disk twice, pulling Cranelift into a
  crate that `daemon`, `runtime`, and `knowledge` all depend on is the single
  most likely way to fill it a third time.
* **Smaller TCB for the same job.** Cranelift is a JIT: it compiles
  attacker-supplied bytecode into native code at runtime. That is a much larger
  attack surface than an interpreter loop, and it needs W^X page management,
  signal-based trap handling, and unwinder integration — none of which a
  short-lived skill needs. A skill module runs for milliseconds; interpretation
  is fast enough and the failure modes are simpler.
* **Epoch interruption is replaceable, and the replacement is stronger.**
  `wasmi 0.51` has no epochs, but it *does* have resumable out-of-fuel
  (`ResumableCall::OutOfFuel`). §5 uses it to enforce wall clock. Fuel is a
  deterministic instruction budget — it bounds work regardless of host
  scheduling, which an epoch deadline does not.

**Licences**: `wasmi`, `wasmi_core`, `wasmi_ir`, `wasmi_collections` are
`MIT OR Apache-2.0`; `wasmparser`/`wasm-encoder`/`wat`/`wast` are
`Apache-2.0 WITH LLVM-exception`; `spin`, `string-interner` are MIT;
`foldhash` is Zlib. Every one is on `deny.toml`'s `licenses.allow` list. No new
advisory-carrying crate is introduced.

## 5. Wall clock without epoch interruption

A pure-compute guest calls no host function, so a host-side deadline check has
no natural place to run. The construction:

```
fuel_slice = min(total_fuel, FUEL_SLICE)
deadline   = Instant::now() + limits.wall_clock
loop {
    match invocation.resume(...) {
        Finished(v)        => break,                  // then the post-loop check below
        OutOfFuel(next)    => {
            if Instant::now() >= deadline { return Err(WallClockExceeded) }
            if remaining < required     { return Err(FuelExhausted) }
            store.set_fuel(fuel_slice)?;              // refuel and continue
            invocation = next;
        }
        HostTrap(_)        => return Err(...),        // incl. the host-call refusal below
    }
}
if started.elapsed() >= limits.wall_clock { return Err(WallClockExceeded) }
```

### 5.1 The slice boundary is not sufficient on its own

**This section previously claimed "overshoot is bounded by one fuel slice, not
by guest behaviour: the guest cannot extend a slice, cannot refuel itself, and
cannot skip the check." That was false, and the 2026-08-13 review falsified it
by running code.** The guest does not need to skip the check; it needs only to
never trigger it. Fuel is not consumed while execution is inside a host
function, so a guest that spends its life in host calls never reaches an
`OutOfFuel` yield and the check never runs. Measured, with **zero declared
permissions** and the shipped `DenyAllGate`, calling only the unprivileged
`codypendent::input` in a loop:

```
declared wall = 1s
RESULT Ok: 200000 host calls, fuel_consumed=800198, duration=166.778777417s
```

167× the declared ceiling, and reported as a **successful completion**.
`fuel_consumed` below one 2 000 000-fuel slice is the proof: the guest never
yielded once.

### 5.2 What is enforced now

Three checks, and the guest is inside one of the two bounded regions at all
times:

1. **At the slice boundary** (`OutOfFuel`) — bounds a pure-compute guest.
   Overshoot ≤ one fuel slice.
2. **On entry to every host call** (`wasm.rs::enter_host_call`) — bounds a
   guest that avoids the slice boundary. Overshoot ≤ one host call. Exceeding
   the deadline returns a host error, which `wasmi` surfaces as
   `ResumableCall::HostTrap`; the run **terminates** with
   `WasmError::WallClockExceeded`. The reason is recorded in the store because
   every host error has the same shape at the `wasmi` boundary.
3. **After the guest returns** — a run that overran is a refusal even when the
   guest completed normally, so a single long host call cannot turn an
   overshoot into a reported `Ok`.

Each host call is additionally **charged fuel** (a flat entry charge plus one
unit per 8 bytes copied), so a host-call loop also burns its instruction budget
and reaches the slice boundary. The charge is a floor, not a measurement of
host time; check (2) is what actually bounds it.

Measured after the change, same adversary shape: declared wall 500 ms, real
elapsed **503.4 ms**, `Err(WallClockExceeded)`. Against the unfixed code the
same fixture ran 6.52 s with `fuel_consumed = 8248` and returned `Ok`.
Regression test: `crates/sandbox/tests/wasm_adversary_it.rs`.

### 5.3 What is still not bounded

**One host call is the unit of overshoot, and a single host call is not itself
time-bounded.** `read_capped` refuses anything that is not a regular file (so a
FIFO, a character device, or a stalled mount cannot hang the invocation) and
caps the bytes, and the host-read budget bounds the total across calls — but a
slow disk serving one large granted read is bounded only by that byte budget,
not by a clock. Named, not fixed.

## 6. Escapes I am explicitly NOT defending against

Named so nobody mistakes silence for coverage.

1. **Microarchitectural side channels** (Spectre/Meltdown-class). An
   interpreter narrows but does not close them. No mitigation attempted.
2. **A memory-safety bug in `wasmi` itself.** `wasmi` contains `unsafe`; the
   workspace's `unsafe_code = "deny"` lint covers our code, not our
   dependencies. A sandbox escape via a runtime bug is a supply-chain
   dependency on upstream, mitigated only by `cargo deny advisories` in CI.
3. **Host resource exhaustion in aggregate.** Per-module memory and fuel are
   capped; N concurrent modules are not. There is no global admission control
   for skill execution in this change. A caller that runs unbounded concurrent
   skills can OOM the daemon. Recorded, not fixed.
4. **Network, at all.** `network_allowlist` remains refused
   (`executor.rs:1237-1243`) because there is no broker. §12.4 of the review
   observed the shipped `fix-ci` skill is structurally unrunnable for exactly
   this reason. This change makes the failure **legible at load time** rather
   than at run time (§8), but does not build the broker.
5. **Secrets.** `brokered_secrets` still has no reader. A `ReadSecret` host
   request is defined in the vocabulary and always denied by every gate shipped
   here, so the shape is fixed before the broker exists rather than after.
6. **What an *approved* capability then does.** If the operator's policy allows
   `cargo` and a human approves it, a skill that runs `cargo` can do anything
   `cargo` can do (build scripts execute arbitrary code). The sandbox confines
   the process; it does not make `cargo` safe. This is the pre-existing
   property of `CommandScope`, unchanged.
7. **Denial of service by the guest against itself.** A guest that burns its
   fuel doing nothing is terminated and the skill fails. Fine.
8. **Malicious `SKILL.md` prose.** Instructions text is prompt-injection
   surface, handled by the existing disclosure/sanitization path, not by this
   change.

## 7. Specific pre-existing defects — closed, and NOT closed

Closed, verified by running:

| Review finding | Fix |
|---|---|
| 12.7(1) `[limits]` parsed and thrown away | `SkillLimits` gains a validated `SkillResourceLimits` with hard ceilings, plumbed into `SandboxProfile` and the WASM store. `maximum_duration_seconds` now sets `wall_seconds`. |
| 12.7(2) `$REPOSITORY`/`$WORKTREE` never substituted | An explicit substitution pass with an exhaustive placeholder table; an **unresolved placeholder is an error**, not a verbatim path. Fails loudly instead of fails-closed-and-silent. |
| 12.4 shipped skill structurally unrunnable | Load-time validation refuses a manifest declaring capabilities the active backend cannot enforce, with the reason — so `skill add` says so, not the first run. Extended: `[trust] signature_required = true` is refused for the same reason (nothing verifies a skill signature). |
| F4 wall clock not enforced on the host-call path | §5.2. |
| F5 host-read budget bypassed by a bad destination pointer | The accounting moved **before** the copy into guest memory. The bytes left the disk whether or not the guest's pointer was valid. |
| F6 a package promotes itself to first-party trust | §8. |
| F7 the documented guest ABI was not the enforced one | The `wasm.rs` module doc now says `codypendent_run: (i32) -> i32`, and `wasm_adversary_it.rs` pins it in both directions. |

**Not closed. Do not read these rows as mitigations:**

| Claim previously made here | Reality at this commit |
|---|---|
| "12.3 nothing calls the executor → `SkillRunner` … is the single production entry point" | **Still open.** `SkillRunner::new`/`::enforcing` are called only from `crates/knowledge/tests/skill_exec_it.rs`. `RunPolicyAdapter` (`crates/daemon/src/policy_gate.rs`) has zero references outside the file that defines it. There is no `skill run` command, no skill-execution RPC, and no skill-invoking tool. **No user can run a skill.** Wiring it is a design decision held by the orchestrator, not an oversight in this file. |
| "migration 0026 records every invocation" | **False.** `grep -rn skill_executions --include=*.rs crates/` returns nothing. The table is created by migration and written by no code, so there is **no audit trail for skill execution**. |
| "12.6 `CapabilityReport` never rendered → `load_package` and the skill-exec entry point both surface `diagnostic()`" | **Still open.** `SkillRunner::capability_diagnostic()` and `enforces_exit_criteria()` have no callers; `skill add` prints neither. On a host without `bwrap` the backend is unavailable and every run fails closed, and the CLI says nothing about it. |

The enforcement in §3/§5 is real and adversarially tested; the *reachability*
is not. Both facts belong in the same document.

## 8. Fail-closed table (what happens on each hostile input)

| Input | Result |
|---|---|
| module imports `wasi_snapshot_preview1.fd_write` | instantiation fails: unresolved import |
| module declares 4 GiB memory | `ResourceLimiter` refuses growth past cap; `memory.grow` returns −1 |
| module loops forever computing | wall-clock refusal at the next slice boundary (or fuel exhaustion first) |
| module loops forever **on host calls** | wall-clock refusal in the host-call prologue — §5.2. Terminated with an error, never `Ok` |
| module returns normally but past its deadline | refused after the loop; a completed run past its ceiling is not a success |
| module passes an out-of-bounds `out_ptr` to `read_file` | the read is metered against `host_read_bytes` before the copy is attempted; the guest gets `-2` and pays for the bytes |
| module returns 500 MB of output | truncated at `maximum_output_mb`, `output_truncated = true` |
| module emits ANSI/OSC control sequences | stripped by `sanitize_untrusted`, counted |
| module exports `codypendent_run: () -> i32` | refused: the ABI is `(i32) -> i32`, and the module doc says so |
| `skill.toml` declares `filesystem_read = ["/"]` | rejected at load: root grants forbidden |
| `skill.toml` declares `maximum_duration_seconds = 0` or absurd | zero is rejected; above the hard ceiling is **refused**, not clamped |
| `skill.toml` sets **any** `[trust] publisher`, `"local-user"` included | trust tier is `Community`. `manifest::PACKAGE_TRUST_TIER` is a constant, not a function of the manifest: no field can move it. `FirstParty` is reachable only by items constructed in code (`knowledge/src/builtin.rs`); `Verified` is unreachable, because no skill path verifies a signature |
| `skill.toml` sets `signature_required = true` | refused at load — nothing verifies a skill signature, so accepting it would record a protection that does not exist |
| host call for a path the manifest never declared | refused by the declaration pre-filter, never reaches the run policy |
| host call for a path the manifest declared but policy denies | refused by `RunPolicyGate` |
| no `RunPolicyGate` supplied | does not compile |
| `DenyAllGate` supplied (default in tests/daemon-less callers) | every privileged request denied |

**Why trust is not keyed on the registration scope.** The obvious refinement —
"repository-scoped packages are `Untrusted`, user-scoped ones `Community`" — is
not implemented, deliberately. The scope a package installs under is itself
derived from the manifest's own `scope` field
(`knowledge/src/skills.rs::declared_scope`), so keying trust on it would hand
the same self-promotion straight back through a different attacker-controlled
key. A flat `Community` for everything on disk is defeatable by nothing.

**What is still read from package-supplied metadata, and why it is left.**
`status` (`draft|active|…`) is package-authored and gates both disclosure and
execution, so a package can declare itself `active`. That is a lifecycle
decision belonging to the registry rather than a trust decision, `skill new`
already forces `draft` by construction, and changing it would make the shipped
reference package undisclosable — recorded here rather than silently changed.
`risk` is derived from the package's own `[permissions]`, which understates
risk for a package that under-declares — but a package that declares nothing
also *gets* nothing, since the declaration is the enforcement ceiling.

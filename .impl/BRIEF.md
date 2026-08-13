# Implementer brief — all agents read this first

Repo `/home/user/codypendent`, branch `claude/review-repair-twenty-outcomes-f50gp3`.
Base review: `docs/reviews/2026-08-13-product-review.md` + `2026-08-13-verticals/`.
**Read your vertical's report first — it has file:line for everything.**

## Environment — this is the binding constraint, obey it

4 CPUs, ~12 GB free disk. Eleven parallel agents filled the disk TWICE already.

- **ALWAYS** prefix cargo with `CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0`.
  A debug test binary here is 400-600 MB; with this it is ~40 MB. Non-negotiable.
- **NEVER** run `cargo build/test --workspace`. Build and test ONLY your own
  crate: `cargo test -p <crate>`. The orchestrator runs the workspace gate.
- **NEVER** `cargo clean`, never delete `target/`.
- If you hit "No space left on device": stop, report it, do not fight it.

## Non-negotiable rules (from the brief)

1. **Never serialize a shared config file from a struct that models only your
   own section.** Edit the parsed document in place. `models.toml` already has
   ONE writer — `crates/cli/src/models_file.rs`. Use it. Do not add a second.
2. **Any check that gates access must be enforced where the resource is
   FETCHED, not only where it is LISTED.** If a list filters by scope, the
   direct-by-id path needs the same gate, and it must fail **identically** for
   "not allowed" and "does not exist" — no enumeration oracle.
3. **If you add a scope, a status, or a capability, grep every existing filter
   over that dimension and update it.** All of them.
4. **Outcomes 12, 13, 15, 19 widen what untrusted input can reach.** Write your
   threat model to `.impl/threat-models/<outcome>.md` BEFORE your first line of
   code: what crosses the boundary, what an attacker controls, what you deny by
   default, what you deliberately allow and why.
5. Match surrounding code style. Comments explain **why**, not what.
6. **Verify your regression test fails against the old behaviour before you
   keep it.** Revert your fix, watch the test fail, restore. A test that passes
   both ways is worse than none. This is not optional — the review found a
   vacuous assertion in the shipped eval corpus and I reproduced the same
   mistake once myself.
7. Green tests are not a working product. Exercise your change against the
   built binary where you possibly can, and say exactly what you ran.

## File ownership — ONE owner each, everyone else proposes

| File / area | Owner |
|---|---|
| `crates/runtime/src/agent.rs` (10.9k lines; tool dispatch + advertisement) | **agent-retrieval** |
| `crates/runtime/src/tools/**` | **agent-retrieval** |
| `crates/tui/src/{render,reduce,state,input,action}.rs` | **agent-tui** |
| `crates/daemon/src/server.rs`, `crates/protocol/**` | **agent-security** |
| `crates/cli/src/{main,commands}.rs` (subcommand table) | **agent-models** |
| `crates/cli/src/models_file.rs` | **agent-models** |
| `crates/knowledge/src/codegraph.rs`, `repomap.rs`, `observer.rs` | **agent-codegraph** |
| `crates/knowledge/src/{memory,learning}.rs` | **agent-memory** |
| `crates/workflow/**`, `crates/council/**` | **agent-delegation** |
| `crates/sandbox/**` | **agent-wasm** |
| `crates/eval/**`, `.github/workflows/**` | **agent-evals** |
| `crates/daemon/src/blackboard.rs`, `codypendentd/src/blackboard.rs` | **agent-board** |
| `migrations/` | numbers pre-assigned below; take yours, never renumber |

Need a change in a file you do not own? Write the proposal to
`.impl/proposals/<owner>-from-<you>.md` (exact code, file:line, why) and carry
on with the rest of your work. Do not edit it.

## Migration numbers (pre-assigned — 0001-0019 and 0022-0024 exist)

0025 routing · 0026 executable skills · 0027 hooks · 0028 delegation ·
0029 memory · 0030 docs round-trip · 0031 multi-user · 0032 ledger

`migrations/README.md`: **migrations are immutable once merged.** sqlx
checksums every byte including comments and refuses to boot on a change. Get
the comment right the first time.

## Definition of done

- Your crate's tests pass (`cargo test -p <crate>`, with the env prefix).
- `cargo fmt --all` clean; `cargo clippy -p <crate> --all-targets -- -D warnings` clean.
- You exercised the change against a real binary/daemon where possible.
- Report: what you changed, what you RAN, what you could NOT verify and why.
- Do NOT commit or push. The orchestrator integrates. Leave the tree dirty.

## Return format

Under 900 words: what landed, file:line for each change, the exact commands you
ran and their output, then **"What I did not verify"**. Be honest — an
overstated report is worse than a short one.

# Implementer brief — round 4. Read this first, all of it.

Repo `/home/user/codypendent`, branch `claude/review-repair-twenty-outcomes-5fynno`,
based at `c255bec8b175d62942b3312cff2335b97d43a59a`.

**Read first:** [`docs/reviews/2026-08-13-r4-product-review.md`](../docs/reviews/2026-08-13-r4-product-review.md)
(the synthesis — §1 is the whole point) and your vertical's report in
`docs/reviews/2026-08-13-r4-verticals/`. They carry file:line for everything.

## Environment — the binding constraint, obey it

4 CPUs, ~20 GB free disk, six of you at once. Earlier rounds filled the disk twice.

- **ALWAYS** prefix cargo: `CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0`.
  A debug test binary here is 400-600 MB; with this it is ~40 MB. Non-negotiable.
- **NEVER** `cargo build --workspace` or `cargo test --workspace`. Only your own
  crate: `cargo test -p <crate>`. The orchestrator runs the workspace gate.
- **NEVER** `cargo clean`, never delete `target/`. Prebuilt binaries live in
  `target/debug/{codypendent,codypendentd}` — rebuild only your crate.
- Cargo takes a lock on `target/`; if a build seems to hang it is waiting for a
  sibling. That is fine. Do not kill it, do not add `--target-dir`.
- On "No space left on device": stop, report it, do not fight it.
- Scratch files under `/tmp/impl-<you>/`, never in the repo.
- **Do not `pkill` anything you did not start.** A reviewer killed a sibling's
  daemon last round. Use your own `CODYPENDENT_DATA_DIR` and your own socket.

## The rules, from the brief. These are not suggestions.

1. **Never serialize a shared config file from a struct that models only your own
   section.** Edit the parsed document in place. `models.toml` has ONE writer —
   `crates/cli/src/models_file.rs`. Use it. Do not add a second.
2. **Any check that gates access must be enforced where the resource is FETCHED,
   not only where it is LISTED.** A list that filters by scope means the
   direct-by-id path needs the same gate, and it must fail **identically** for
   "not allowed" and "does not exist" — no enumeration oracle.
3. **If you add a scope, a status or a capability, grep every existing filter over
   that dimension and update all of them.**
4. **Fix the class, not the instance.** This is the entire finding of four
   consecutive reviews. Before you fix a site, grep for every other site with the
   same shape and fix those too — or state in your report why you did not.
   A comment explaining the trap is not a fix; it is evidence the trap was
   understood and left in place.
5. **Verify your regression test fails against the old behaviour before you keep
   it.** Revert your fix, watch it fail, restore. A test that passes both ways is
   worse than none. The round-3 review found a vacuous assertion in the shipped
   eval corpus; do not add another.
6. **Green tests are not a working product.** Exercise your change against the
   built binary. Say exactly what you ran and paste the output.
7. Match surrounding code style. Comments explain **why**, not what.
8. Outcomes 12, 13, 15 and 19 widen what untrusted input can reach. If you touch
   them, update `.impl/threat-models/<n>-*.md` to match what the code actually
   enforces. The review found those documents promising enforcement that does not
   exist — an unenforced threat model is a finding, not a mitigation.

## File ownership — ONE owner each. Everyone else proposes.

| File / area | Owner |
|---|---|
| `crates/runtime/src/agent.rs`, `crates/runtime/src/tools/**` | **A-runtime** |
| `crates/tui/src/**` (render, reduce, state, accessible, markdown, dag) | **B-tui** |
| `crates/cli/src/**` (commands, tui, theme_select, stream, models_*) | **C-cli** |
| `crates/sandbox/**`, `crates/knowledge/src/{manifest,skill_exec}.rs` | **D-sandbox** |
| `crates/daemon/**`, `crates/codypendentd/**`, `crates/protocol/**` | **E-daemon** |
| `evals/**`, `crates/eval/**`, `.github/workflows/**`, `docs/**` | **F-evals-docs** |
| `Cargo.toml` (workspace), `migrations/` | **orchestrator — ask, do not edit** |
| `crates/cli/src/models_file.rs` | **C-cli** (sole writer of `models.toml`) |
| `crates/*/builtin_catalog.toml` (model catalog) | **C-cli** |

Need a change in a file you do not own? Write it to
`.impl/r4-proposals/<owner>-from-<you>.md` — exact code, file:line, why — and
carry on with the rest of your work. Do **not** edit it. The orchestrator
integrates proposals in Phase 3.

`crates/codypendentd/src/workflows.rs` — the orchestrator has already edited the
`pause_and_resume…` test (the CI-red flake). **E-daemon: do not touch that test.**

## Definition of done

- Your crate builds, its tests pass, `cargo fmt --all` is clean, and
  `cargo clippy -p <crate> --all-targets --all-features -- -D warnings` is clean.
- You exercised the change against a real binary or daemon where possible.
- **Do NOT commit and do NOT push.** Leave the tree dirty; the orchestrator
  integrates and commits. Do not `git checkout`/`stash`/`reset` anything.

## Return format — under 900 words

1. What landed, with `file:line` per change.
2. The exact commands you ran and their real output (not a paraphrase).
3. For each fix: the **class** you closed and the other sites you checked.
4. **"What I did not verify"** — honest, specific. An overstated report is worse
   than a short one.

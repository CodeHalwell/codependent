# Vertical: docs-vs-reality

Reviewer scope: `README.md`, `ROADMAP.md`, `docs/**`, `extensions/**`, `sdk/**`,
`examples/**`, `install.sh`, `.github/workflows/**`.
Owned question: **the gap between what the docs claim and what the code does.**

Pinned commit `535a2f5` (v0.4.5). Binaries built and run from `./target/debug/`.
Everything below marked "actual" was computed or executed, not read.

---

## Verdict lines

```
DOCS/CLI SURFACE (cli-and-tui-user-guide.md): PARTIAL — every documented command exists
  and runs, but 15 shipped commands (incl. 4 whole top-level ones) are undocumented, and
  two statements about what a command does contradict the command's own --help.
DOCS/NUMBERS (ROADMAP.md): BROKEN — 11 of 14 checkable counts are wrong; 8 test counts are
  stale low by 1.4x-7.9x and 3 measured retrieval numbers disagree with the test that
  produces them.
DOCS/VERSIONS+MSRV: BROKEN — the build guide pins agent-framework 0.1.1 / rustc >= 1.82;
  the workspace is on 0.2.0 / 1.88. Following the build guide cannot compile the tree.
DOCS/MIGRATION IMMUTABILITY: BROKEN — three documents promise migrations are never edited
  after release; migration 0017 was schema-changed after shipping in v0.1.1.
ROADMAP [x] FEATURE CLAIMS: WORKS — I could not falsify a single [x] on feature existence.
  3.5, 4.x, 5.x, 6.6 and 7.x all resolve to real, runnable code. The [x] that IS false is
  the hygiene one (line 623, "migrations unchanged since first commit").
EXAMPLES (examples/plugins/word-count): BROKEN — the repo's own example manifest is
  rejected by the repo's own parser; `plugin inspect` and `plugin diff` both exit 1.
EXTENSIONS/VSCODE BUILD DOCS: BROKEN — the README "Develop" block does not work from a
  clean clone; 4 of 8 vitest files fail to resolve `@codypendent/ui`.
SDK (sdk/ui): WORKS — `npm ci && npm run check` is green (13 files, 66 tests, build emits dist/).
install.sh: PARTIAL — structurally correct against the real v0.4.5 release artifact, but it
  hard-requires `gh` for a repo that is public, and advertises a `CODYPENDENT_LIB` knob no
  Rust code reads.
.github/workflows: WORKS — CI runs more than the ROADMAP claims. But the document ROADMAP
  names as "the release gate" has 0 of 34 boxes ticked while v0.4.5 has shipped.
DOCS/MANIFEST.json: BROKEN — declares 57 files; docs/ contains 130. 73 unlisted, ~1 month stale.
```

---

## 1. Every number, claimed vs computed

Method: test counts from `--list --format terse` on the newest built test binary per
target (`target/debug/deps/*`), not from `rg` on source. Vitest counts from an actual
`npm test` run. Theme/mode/tool counts from the enum or registration site.

| # | Claim | Location | Claimed | **Actual (computed)** | Verdict |
|---|---|---|---|---|---|
| 1 | `cargo test --workspace` total | `ROADMAP.md:107` | **1051 tests** | **≈2,426** | WRONG (2.3× low) |
| 2 | VS Code extension suite | `ROADMAP.md:196` | **30 vitest tests** | **214 tests, 8 files** | WRONG (7.1× low) |
| 3 | VS Code extension suite (repeat) | `ROADMAP.md:209` | **30 vitest tests** | 214 | WRONG |
| 4 | `codypendent-sandbox` | `ROADMAP.md:439` | **42 unit tests** | **114** | WRONG (2.7× low) |
| 5 | multimodal round-trip/gate | `ROADMAP.md:455` | **10 tests** | **29** (`input.rs` 9 + `envelope.rs` 20) | WRONG |
| 6 | theme legibility | `ROADMAP.md:462` | **17 tests** | **23** (`theme::` 13 + `theme_pack::` 10) | WRONG |
| 7 | router | `ROADMAP.md:515` | **37 tests** | **52** | WRONG |
| 8 | promotion pipeline | `ROADMAP.md:532` | **12 tests** | **21** (`promote::`) | WRONG |
| 9 | TUI shell | `ROADMAP.md:560` | **70 TUI tests** | **552** | WRONG (7.9× low) |
| 10 | retrieval recall@8 | `ROADMAP.md:180` | **1.0** | **0.9833** (hashing-trigram) / **0.9500** (word-hash-semantic) | WRONG |
| 11 | disclosed top-k budget | `ROADMAP.md:180` | **254 tok** | **378 tok / 396 tok** | WRONG |
| 12 | full-injection baseline | `ROADMAP.md:180` | **4580 tok** | **5955 tok** | WRONG |
| 13 | semantic-token variants | `ROADMAP.md:456` | **six beyond dark** | 7 total in `ThemeVariant` (`crates/tui/src/theme.rs:701-709`) | **CORRECT** |
| 14 | built-in themes | `guide:117` | **seven** | 7 (`--theme` accepts exactly those 7) | **CORRECT** |
| 15 | `github.*` tools | `ROADMAP.md:193` | **five** | 5: `get_pull_request`, `list_check_runs`, `create_draft_pull_request`, `update_pull_request`, `create_check_run_summary` (`crates/runtime/src/tools/github.rs`) | **CORRECT** |
| 16 | Remote UI slot registry | `extensions/vscode/README.md:93` | **22-point** | 22 in `slot-registry.ts:23-44`; `UI_CONTRIBUTION_POINTS` = 22 | **CORRECT** |
| 17 | council size / rounds | `guide:532` | **2-8 members, 1-3 rounds** | `MAX_MEMBERS = 8`, `(2..=MAX_MEMBERS)` at `crates/council/src/service.rs:38,1261` | **CORRECT** |
| 18 | agent modes | `guide:741` | **5** | 5 (`AgentMode` at `crates/protocol/src/run.rs:18-28`, plus `Unknown` forward-compat) | **CORRECT** |
| 19 | council TUI builder | `README.md:246` **seven-step** vs `guide:576-580` **4 pages** | — | `CouncilBuilderStep` has **7** variants (`crates/tui/src/state.rs:112-120`) | README correct, **guide wrong** |
| 20 | eval corpus | `ROADMAP.md:498` | 50-100 | **12** (`evals/tasks/core/*.json`) | Honestly disclosed in `evals/README.md:43,156` — ROADMAP lists it as *Remaining* under an `[x]` |
| 21 | doc suite file list | `docs/MANIFEST.json` | **57 files** | **130 files** on disk (73 unlisted) | STALE |
| 22 | crate list | `docs/README.md:65-74` | 9 crates | **17** in `Cargo.toml` members | STALE ("Suggested", so soft) |

**Uncomputable:** none of the numeric claims in my scope were uncomputable. The three
retrieval numbers (#10-12) are produced by a shipped test — `cargo test -p
codypendent-knowledge --test retrieval_eval -- --nocapture` prints them — so the exit
criterion was written against an older corpus and never re-derived.

**Pattern:** every wrong test count is *low*, and every one of them is the count as of the
day the line was written. Nothing re-derives them. #10-12 are the same failure applied to
a measurement instead of a count.

---

## 2. Versions and dependencies

### 2a. `agent-framework` — docs are a full minor version behind

`Cargo.toml:57-59` and `Cargo.lock` resolve **0.2.0** for
`agent-framework-core` / `-openai` / `-anthropic` (plus transitive `-azure` and
`-bedrock` 0.2.0, pulled by `-anthropic`).

Docs still say `0.1.1`:

- `docs/docs/12-agent-framework-rs-integration.md:5` — "the published umbrella crate is `agent-framework` version `0.1.1`, requiring Rust 1.82 or newer"
- `docs/docs/12-agent-framework-rs-integration.md:93-98` — a `Cargo.toml` snippet pinning `0.1.1` for six crates
- `docs/docs/build/00-how-to-use-this-guide.md:76` — "crate `agent-framework-core` version `0.1.1` … MSRV Rust 1.82"
- `docs/docs/build/10-phase-0-workspace-bootstrap.md:15,71` — "`agent-framework-core = "0.1.1"` is pinned now"
- `docs/docs/build/11-phase-1-persistent-agent-slice.md:20-21`
- `docs/docs/21-the-codypendent-story.md:48` — "(v0.1.1, Rust 1.82+)"

`docs/releases/v0.4.1.md:23` is the one doc that got it right ("move together from 0.1.1 to
…"), which is how the drift is provable rather than arguable.

### 2b. MSRV — the declared 1.88 is right, three other places say 1.82

- `Cargo.toml:34` `rust-version = "1.88"` — **verified defensible**: the maximum declared
  `rust_version` across the whole resolved `--all-features` graph is exactly `1.88.0`
  (`agent-client-protocol` 2.0.0, `agent-client-protocol-derive`/`-schema`, `darling`
  0.23.0, `time` 0.3.53). Local toolchain is 1.94.1.
- `Cargo.toml:85` — inline comment says `ed25519-dalek` is "Pinned to a version that builds
  on the workspace's rust-version (**1.82**)". Same file, 51 lines apart, contradicts line 34.
- `docs/docs/build/00-how-to-use-this-guide.md:66` — "`rustc --version` # need **≥ 1.82**"
- `docs/docs/build/10-phase-0-workspace-bootstrap.md:26,60` — "**EXPECT** — `rustc` ≥ 1.82",
  and the file it tells you to create contains `rust-version = "1.82"`.

**User consequence:** a reader following the End-to-End Build Guide installs 1.82, writes
`rust-version = "1.82"`, and the tree does not compile — `agent-client-protocol` is
edition-2024. The guide is the document the README points implementation agents at
(`README.md:39`).

### 2c. `rust-toolchain.toml` is not pinned

`Cargo.toml:32-33` says "The **pinned** toolchain (`rust-toolchain.toml`) is newer, so
builds are unaffected." `rust-toolchain.toml` is:

```toml
[toolchain]
channel = "stable"
```

`stable` is a floating channel, not a pin. The safety argument in the comment ("so builds
are unaffected") rests on a pin that does not exist; a future stable that drops something
is not held back by anything.

### 2d. Extension version lags the workspace

`extensions/vscode/package.json` `"version": "0.4.2"`; workspace is `0.4.5`
(`Cargo.toml:29`). Three patch releases shipped without the extension manifest moving.
`sdk/ui/package.json` is `1.1.0` and is independently versioned, so that one is fine.

### 2e. Node/TS deps — do they install?

Both install cleanly on Node v22.22.2 / npm 10.9.7:

| | `npm ci` | `npm audit --audit-level=high` | tests |
|---|---|---|---|
| `sdk/ui` | ✅ 61 pkgs, 0 vulns | ✅ | ✅ **13 files, 66 tests** |
| `extensions/vscode` | ✅ 228 pkgs, 0 vulns | ✅ | ✅ **8 files, 214 tests** — *but only after `sdk/ui` is built*, see §6 |

`sdk/ui` `npm run check` (typecheck → test → build) is fully green.

### 2f. Doc metadata versions disagree

- `docs/MANIFEST.json` — `"version": "0.4"`, `"date": "2026-07-17"`
- `docs/README.md:88-93` — "Document version: **0.3**", "Date: **15 July 2026**"

---

## 3. Migration immutability — a shipped promise that is already broken

Three documents make the same promise:

- `ROADMAP.md:623` — `- [x] CI green on the release commit; working tree clean; **migrations unchanged since first commit**`
- `docs/docs/build/99-master-acceptance-checklist.md:11` — "every migration file unchanged since its first commit"
- `docs/cli-and-tui-user-guide.md:53` — "migration files are **immutable once released**"
- `migrations/README.md` — "**Never edit it.** Not comments, not formatting, nothing."

**Falsified.** `git log -- migrations/0017_promotion_evidence.sql` returns two commits:

- `ed083ed` "release: harden Codypendent v0.1.1" — introduces the file
- `7eef118` "fix: address release review feedback" — **adds five columns** to
  `eval_suite_reports` (`candidate_id`, `artifact_kind`, `artifact_name`,
  `artifact_version`, `routing_policy`) and renames the index

v0.1.1 is a real published release. `migrations/README.md` states the exact consequence:
`sqlx::migrate` checksums each applied migration and *refuses to boot* ("migration N was
previously applied but has been modified"). Any user who installed v0.1.1 and later
upgrades hits a daemon that will not start. `migrations/0003_phase2.sql` has 3 commits, one
of which (`d9b149f` "restore 0003_phase2.sql byte-identically — migrations are immutable")
is the project having already been bitten by this once.

Secondary: the numbering has a hole — `0019_blackboard_board.sql` → `0022_registry_embeddings.sql`.
`0020` and `0021` never existed in any commit. Harmless to sqlx, but it makes the
"migration NNNN" references in `ROADMAP.md` unauditable by counting.

---

## 4. Every ROADMAP `[x]` — spot-checks of the boldest claims

I could not falsify a single `[x]` **on feature existence**. Every named file, type and
command in the claims I checked exists and is wired. Evidence:

| Claim | Check run | Result |
|---|---|---|
| **3.5** VS Code extension, "30 vitest tests + typecheck + lint green" | `npm ci; npm test; npm run typecheck; npm run lint` in `extensions/vscode` | ✅ all three exit 0 — **but only after `sdk/ui` is built** (§6), and the count is 214 not 30 |
| **4.6** staleness engine, "`/update-docs` command" | `codypendent docs check` | ✅ exit 0, `Checked 0 document(s); 0 symbol link(s) resolved.` |
| **4.x** Docs Studio client surface | `codypendent docs list` | ✅ exit 0, `No documents yet. Create one with codypendent docs new "<title>".` |
| **5.1** compiler + canonical manifest | `codypendent workflow validate docs/specs/workflow.yaml` | ✅ `✓ repair-github-check v1 valid — 5 step(s), 3 agent step(s); order: inspect → patch → verify → review → publish`. The built-in is literally `include_str!("../../../docs/specs/workflow.yaml")` (`crates/workflow/src/source.rs:53`) — spec and shipped artifact cannot drift |
| **6.6** "six semantic-token variants beyond dark" | `ThemeVariant` enum | ✅ 7 total, matches `--theme`'s accepted values exactly |
| **6.1** "Surfaced to users via `codypendent plugin inspect`/`diff` … with example manifests under `examples/plugins/word-count/`" | ran both | ❌ **both exit 1** — see §5 |
| **7.1** eval harness + runnable corpus | `codypendent eval run --suite core --report …` | ✅ `eval: loaded 12 case(s) from evals/tasks/core` … `3/12 case(s) passed (25%)` (correct — no model configured) |
| **7.2/7.5** router + promotion CLI | `models bench/pull/list/add/check`, `promote propose/advance/approve/rollback` | ✅ all present with the documented flags |
| **hygiene** `cargo fmt --all -- --check` | ran | ✅ exit 0 |

### The one `[x]` I believe is false

**`ROADMAP.md:623`** — `- [x] CI green on the release commit; working tree clean;
migrations unchanged since first commit`. The third clause is falsified in §3.

### Two `[x]` whose *evidence text* is false while the box itself is fair

- **`ROADMAP.md:180`** — `[x] Retrieval eval: recall@8 = 1.0 (≥ 0.8 gate) … 254 tok … 4580 tok`.
  The gate (≥ 0.8) genuinely passes. All three quoted measurements are wrong (§1 #10-12).
- **`ROADMAP.md:196,209`** — 3.5's "30 vitest tests" (actual 214).

### Structural inconsistency worth a fix

`ROADMAP.md:239` is `- [ ] Declarative workflows; durable checkpoint storage;
supervisor/specialist delegation; blackboard` — an unchecked parent whose every child
(5.1, 5.2, 5.3, and line 391 "all landed") is `[x]`, in a phase headed `✅`. The parent is
the only thing a scanner reads.

---

## 5. `examples/plugins/word-count` is rejected by the shipped parser

```
$ codypendent plugin inspect examples/plugins/word-count/plugin.toml
Error: examples/plugins/word-count/plugin.toml: plugin filesystem capability paths must be
absolute, normalized, non-root paths: ${WORKSPACE}
$ echo $?
1
```

Same for `codypendent plugin diff examples/plugins/word-count/plugin.toml
examples/plugins/word-count/plugin-v2.toml` (fails on the first file, `crates/cli/src/commands.rs:2006`).

Both manifests declare `filesystem_read = ["${WORKSPACE}"]`
(`examples/plugins/word-count/plugin.toml:25`, `plugin-v2.toml:25`). The manifest's own
header comment (line 7) prints the exact command that fails.

Who points a user here:
- `ROADMAP.md:444-445` — "with example manifests under `examples/plugins/word-count/`", as
  the user-facing surfacing of an `[x]` Phase 6.1
- `docs/cli-and-tui-user-guide.md:288-296` — §4.3 documents `plugin inspect` / `plugin diff`
  as the "evaluate permissions" step

**User-visible consequence:** the first thing a reader of §4.3 or ROADMAP 6.1 tries — the
copy-pasteable command in the example file itself — exits 1 with a parser error.
Classification: **(c) wire attached, wrong behaviour** (the docs, the example and the
parser disagree about the `${WORKSPACE}` token).

---

## 6. `extensions/vscode/README.md` "Develop" block does not work

`extensions/vscode/README.md:136-142`:

```bash
npm install
npm run typecheck
npm run lint
npm test
npm run build
```

Run verbatim on a clean clone, `npm test` **fails**:

```
FAIL  test/remote-ui-store.test.ts
Error: Failed to resolve entry for package "@codypendent/ui". The package may have
incorrect main/module/exports specified in its package.json.
...
 Test Files  4 failed | 4 passed (8)
      Tests  165 passed (165)
```

Cause: `extensions/vscode/package.json` takes `"@codypendent/ui": "file:../../sdk/ui"`, and
`sdk/ui/package.json` sets `"main": "./dist/index.js"` with every `exports` entry under
`./dist/`. `sdk/ui` has **no `prepare` script**, so `npm ci` in the extension links a
package with no `dist/`. After `cd sdk/ui && npm ci && npm run build`, the same command is
green: **8 files, 214 tests**.

CI survives this only by accident of ordering — `.github/workflows/ci.yml`'s `extension`
job runs `sdk/ui` → `npm run check` first, whose last step is `npm run build`. Neither
README documents that dependency. A contributor following the README sees half the suite
red and no hint why.

Classification: **(b) engine built, tested, documented — final wire never attached**
(the missing wire is a `prepare` script or one line in the README).

---

## 7. `install.sh` — would it work?

**Structurally, yes.** Verified against the real published artifact:

- Release `v0.4.5` exists and carries all three expected assets. The asset name
  `install.sh:45` computes for this machine — `codypendent-x86_64-unknown-linux-gnu.tar.gz`
  — is present (99,202,235 bytes).
- The tarball's top-level directory is `codypendent-x86_64-unknown-linux-gnu/`, exactly
  `install.sh:52`'s `$src`.
- Every path `install.sh:53-55` `-x` tests exists and is `0755`: `codypendent`,
  `codypendent-ui-worker-launcher`, `node-runtime/bin/node`. The optional `codypendentd`
  (line 68) is present too.
- `bash -n install.sh` is clean.
- The default `LIBDIR` (`install.sh:23`, `/usr/local/lib/codypendent`) matches the only
  place the product looks: `bundled_ui_runtime()` at
  `crates/daemon/src/remote_ui_plugins.rs:1517-1520` probes `<bindir>/node-runtime` then
  `<bindir>/../lib/codypendent/node-runtime`.

**Four defects:**

1. **The "private repo" justification is false.** `install.sh:7` — "uses your existing `gh`
   auth, so it works for a private repo"; `install.sh:25` hard-exits without `gh`;
   `codypendent update --check` exits 1 with *"GitHub CLI (`gh`) is required to download
   releases from the **private repo**"*. The repository is **public**
   (`api.github.com/repos/CodeHalwell/codypendent` → `"private": false, "visibility": "public"`).
   `curl -fsSL https://raw.githubusercontent.com/CodeHalwell/codypendent/main/install.sh`
   returns **HTTP 200**. So a user who follows the release notes' own instruction
   (`docs/releases/v0.4.5.md`, "Or use the installer: `curl -fsSL … | bash -s -- v0.4.5`")
   successfully downloads a script that then refuses to run because `gh` is missing.
   Neither the installer nor `update` needs `gh` for a public repo.
2. **`CODYPENDENT_LIB` is a knob nothing reads.** `install.sh:23` lets you relocate the
   Node runtime. `rg CODYPENDENT_LIB crates/ -g '*.rs'` returns **nothing**; the runtime
   resolver only knows `<bindir>/node-runtime` and `<bindir>/../lib/codypendent/node-runtime`.
   Setting `CODYPENDENT_LIB` to anything else installs the runtime somewhere the daemon
   will never look, and Remote UI silently fails closed. Class **(c)**.
3. **Stale example.** `install.sh:14` — "`… | bash -s -- v0.1.0-build.17`". Current is v0.4.5.
4. **No space check.** `install.sh:49` `mktemp -d` puts a ~99 MB download plus a ~350 MB
   extraction in `$TMPDIR`, then copies `node-runtime` again into `$LIBDIR`. I hit ENOSPC
   doing exactly this. There is no free-space precondition and no message naming `$TMPDIR`.

---

## 8. `.github/workflows` vs the ROADMAP's exit criteria

**What CI actually runs** (`ci.yml`, 5 jobs):

| Job | Commands |
|---|---|
| `lint` | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `test` | `cargo test --workspace --all-features --no-run`, then `cargo test --workspace --all-features` (15-min cap) |
| `eval-smoke` | `cargo test -p codypendent-eval --all-features`; `cargo test -p codypendent-cli --all-features --test eval_it` |
| `deny` | `EmbarkStudios/cargo-deny-action@v2 check` |
| `extension` | sdk/ui: `npm ci`, `npm run check`, `npm audit --audit-level=high`; ext: `npm ci`, typecheck, lint, test, build, `vscode:prepublish`, audit |

`release.yml`: a `frontend-quality` gate, then a 3-target native build matrix
(`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`) packaging a
SHA-256-verified pinned Node 22.13.1 runtime with a generated `.codypendent-runtime-seal.json`,
publishing on tags and cutting a rolling prerelease on every push to `main`. Verified
end-to-end: the assets it describes exist on the live v0.4.5 release.

**CI does *more* than the ROADMAP claims**, which is the right direction. The gaps are:

1. **The named release gate is 100% unticked.** `ROADMAP.md:11-12` — "the release gate is
   the [Master Acceptance Checklist](docs/docs/build/99-master-acceptance-checklist.md)".
   That file has **0 `[x]` and 34 `[ ]`** while v0.4.5 has shipped. Meanwhile
   `ROADMAP.md:617-624` ticks the same criteria `[x]`. The two documents contradict each
   other about whether the release gate passed.
2. **`cargo audit` is claimed but not run.** `99-master-acceptance-checklist.md:10` requires
   "`cargo deny check` **and `cargo audit`** clean". CI runs `cargo-deny-action` only.
   (`deny.toml` carries the dated advisory exceptions `ROADMAP.md:620-622` describes, so
   the advisories half is covered — but `cargo audit` itself is not in either workflow.)
3. `release.yml` requires `docs/releases/v<version>.md` to exist for **every push to main**
   (`body_path`). A version bump without a matching release note breaks the rolling
   prerelease job. Not currently broken (v0.4.5.md exists) — an undocumented coupling.

---

## 9. `cli-and-tui-user-guide.md` walked as a user

I ran `codypendent --help` and the `--help` of all 20 subcommands plus 22 nested
subcommands, and diffed against the guide.

### Good news first

**Every command the guide documents exists, with the documented flags.** No documented
command is absent. Spot-verified by execution: `daemon status` (exit 1 when stopped, exit
0 when running, `--json` → `{"running":false}` — exactly as `guide:64-66` claims),
`acp list` (38 registry entries live), `council list`, `plugin trust list`, `mcp list`,
`completion bash`, `workflow validate`, `docs list`, `docs check`, `eval run`, `doctor`,
`finetune check`, `update --check`. The ACP friendly aliases the guide promises all resolve
(`crates/integrations/src/acp_registry.rs:201-208`): `claude-code`→`claude-acp`,
`vibe-chat`→`mistral-vibe`, `kimi-cli`→`kimi`, `antigravity`→`antigravity-acp`.

### Commands that exist and are NOT in the guide

Four whole top-level commands:

- **`codypendent skill add <DIR>`** — the only way to install a skill package. Zero mentions.
- **`codypendent mcp list`** — the only MCP surface. Zero mentions.
- **`codypendent doctor [--json] [--deep]`** — the diagnostics command. Zero mentions.
- **`codypendent completion <SHELL>`** — shell completions. Zero mentions.

Plus eleven subcommands / flags:

- `daemon restart` (idle-guarded restart — the mechanism `guide:50-52` describes in prose but never names)
- `docs new`, `docs list`, **`docs check`** — `docs check` *is* `/update-docs`; §4.4 documents only `docs publish`
- `models list`, `models add`, `models check` — §4.5 documents only `bench` and `pull`
- `acp refresh`, `acp install`
- `council show --last`, `council create --evidence`
- `eval run --candidate-id` (the flag that binds a report to a promotion candidate — without it, §4.5's `promote advance --step regression` has no evidence to read)

### Two statements the CLI's own `--help` contradicts

1. **`guide:610-612` (§4.8)**: "`# Rebuild Tantivy search **and tree-sitter code graph**
   indexes from SQLite` / `codypendent index rebuild`". The command's own help says the
   opposite: *"**Does NOT rebuild the code graph** — that is built per-repository on session
   open / first run."* A user with a stale code graph runs `index rebuild`, sees success,
   and nothing changes. Class **(c)**.
2. **`docs/context-window.md:10`**: "`<config_dir>/codypendent/models.toml`". Wrong
   directory *and* a spurious `codypendent/` segment. `models.toml` lives in the **data**
   dir — `crates/cli/src/tui.rs:4235,4539,5957` all `data_dir.join("models.toml")`, and
   `codypendent doctor` reports `models: could not read <DATA_DIR>/models.toml`.
   `guide:634` gets it right (`<data_dir>/models.toml`). Two shipped user docs disagree
   about where the product's primary config file goes; a reader of `context-window.md`
   creates the file in the wrong place and sees no `ctx N%` gauge and no `num_ctx` hint.
   (The same wrong path is repeated in the code comment at `crates/runtime/src/models.rs:56`.)

### §3.2 "Complete Hotkey Matrix" is not complete

`crates/tui/src/input.rs:350-393` (`map_normal_char`) binds 8 keys absent from the table:

| Key | Action | In guide? |
|---|---|---|
| `e` | `EditDoc` | ✗ |
| `i` | `InsertDocBlock` | ✗ |
| `P` | `PublishDoc` | ✗ |
| `X` | `DeleteDocBlock` | ✗ |
| `t` | `EnableUiPluginSession` | ✗ |
| `u` | `EnableUiPluginUser` | ✗ |
| `x` | `RevokeUiPlugin` | ✗ |
| `y` | `CopyFocusedCard` | ✗ (only `Alt-Y` is listed) |

The four doc keys are the entire editing surface of the Docs Studio the guide sends you to
with `D`; `t`/`u`/`x` are the plugin enable/revoke controls. §3.3's cooked-mode table has
the same holes.

### Palette rows undocumented

`crates/tui/src/palette.rs` defines 27 rows. Undocumented anywhere in the guide:
`Setup & diagnostics`, `/mode  Mode picker`, `/council result  Council results`,
**`/plugins  Remote UI plugins`**, `New conversation`. `/plugins` is the TUI half of the
entire `codypendent plugin` CLI surface §4.3 documents at length — and it owns the three
undocumented hotkeys above.

### §4.6.1 council builder page count

The guide numbers 4 pages; `CouncilBuilderStep` has 7 (`Name, Description, MemberModel,
MemberRole, Chair, Rounds, Review`), and `README.md:246` says "seven-step builder". A user
following the guide is on page 5 of a flow the guide says has 4.

---

## 10. Smaller, still user-facing

- **`codypendent finetune check` prints the wrong header.** Output begins `codypendent doctor`.
  (`doctor`'s own output begins with the same line, so the header is hard-coded to the
  wrong command name.)
- **`docs/MANIFEST.json` is a stale inventory.** Declares 57 files; `docs/` holds 130.
  73 unlisted, including every user-facing doc added since 2026-07-17:
  `cli-and-tui-user-guide.md`, `policy-files.md`, `context-window.md`,
  `architecture/remote-ui.md`, `architecture/model-provider-selection.md`, all of
  `releases/`, `reviews/`, `superpowers/`, `docs/benchmarks/`. Nothing listed is missing,
  so this is pure omission. If anything consumes MANIFEST as the doc set (it is the file
  named "Codypendent Documentation Suite"), the guide a user most needs is invisible to it.
- **`README.md:101-105` (IDE Awareness)** states in the present tense: "Zed: ACP-first +
  thin extension for ACP gaps" and "JetBrains: Kotlin IntelliJ platform plugin".
  `extensions/` contains **only `vscode`**. (Zed is genuinely served by `codypendent acp
  serve` — no extension needed — so the Zed line is arguably fine; the JetBrains plugin
  does not exist in any form.)
- **`docs/README.md:60-84` "Suggested repository shape"** lists `extensions/jetbrains/`,
  `extensions/zed/`, a root `specs/` and a root `tests/` — none exist — and 9 crates where
  17 exist. Marked "Suggested", so lowest priority.
- **`docs/README.md:88-93` vs `MANIFEST.json`** — doc version 0.3 / 15 July vs 0.4 / 17 July.
- **`install.sh:14`** stale tag example `v0.1.0-build.17`.

### Doc claims I checked and found CORRECT (worth not deleting)

- All internal `crates/…`, `migrations/…`, `evals/…`, `examples/…` path references in docs
  resolve. A scripted existence check over every `.md`/`.sh`/`.yml` in scope found **zero**
  genuinely dangling repo paths (the only 2 hits were `https://crates.io/crates/agent-framework`
  URLs matching the pattern).
- `docs/specs/workflow.yaml` is not a sample — it is `include_str!`'d as the shipped built-in
  `repair-github-check` manifest (`crates/workflow/src/source.rs:53`), so the spec cannot drift.
- `docs/policy-files.md` — every code reference it names exists
  (`RuntimePaths::global_policy_path` at `crates/protocol/src/discovery.rs:155-157` returns
  `config_dir.join("policy.toml")`, matching the doc exactly).
- `docs/context-window.md` — `context_tokens: Option<u64>` is real at
  `crates/runtime/src/models.rs:103`, as claimed. Only its file location is wrong.
- `evals/README.md` is the best document in the repo: it states 12 cases, states the roadmap
  wants 50-100, states `correct_citations` has no signal, states routing-policy enforcement
  is not wired. Nothing in it needed correcting.
- `docs/TIMELINE.md` carries an explicit "historical planning doc … not current status"
  header. Correct self-labelling.
- Tantivy is genuinely the BM25 backend (`crates/knowledge/src/retrieval/bm25.rs:13-25`), so
  §4.8's "Tantivy" half is accurate — only its "code graph" half is wrong.

---

## What I could not exercise, and why

1. **`cargo test --workspace --all-features` to completion.** The build filled the 252 GB
   volume (`target/` reached 27 GB and `/` hit 100%); I killed it and freed
   `target/debug/incremental`. The **2,426** figure is therefore a sum of `--list` over the
   newest built binary per test target (66 + protocol + workflow, built separately), not a
   green run. It is a lower bound and could drift by a few tests where a stale
   feature-variant binary was the newest for a target. It cannot be off by anything near the
   1,375-test gap to the claimed 1051.
2. **`cargo clippy --workspace --all-targets --all-features -- -D warnings`.** Same disk
   ceiling. `ROADMAP.md:618`'s clippy `[x]` is unverified by me.
3. **`cargo deny check`.** `cargo-deny` is not installed and I did not install it.
   `ROADMAP.md:620`'s `[x]` and the three dated advisory exceptions in `deny.toml` are
   unverified by me.
4. **`install.sh` executed end-to-end.** I built a `gh` shim and started it, but the
   extraction ran the volume out of space and I aborted rather than risk the parallel
   reviewers' `target/` trees. Everything the script *checks* was verified directly against
   the downloaded v0.4.5 tarball (asset name, top-level dir, all four `-x` paths, 0755
   modes), and `bash -n` is clean — so the failure mode I could not rule out is only in the
   install/copy half (lines 67-90), which is plain `install`/`cp`/`mv`.
5. **macOS and aarch64 release paths.** Linux only. The Seatbelt executor, the Gatekeeper
   `xattr` step (`install.sh:59-61`), and the two Darwin targets are unexercised.
6. **The TUI itself.** No pty. All TUI claims above are verified against
   `crates/tui/src/input.rs`, `palette.rs`, `state.rs` and `theme.rs` by reading the
   reducer tables, not by driving a terminal. Whether `/plugins` *renders* is another
   vertical's call; I only establish that it exists and is undocumented.
7. **A live model.** `eval run` reports 3/12 because no `models.toml` is configured; that is
   the correct behaviour for an unconfigured machine, not a scored result. `models bench`,
   `models check`, `acp connect`, `acp probe`, `council run` and the voice paths all need
   credentials or a vendor binary and were not exercised.
8. **`docs publish` / `plugin install`.** Both are write paths that park a durable approval;
   I stayed read-only per the brief.

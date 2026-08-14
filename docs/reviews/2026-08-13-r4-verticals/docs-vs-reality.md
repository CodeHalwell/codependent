# Vertical: docs-vs-reality (round 4)

Reviewer scope: `README.md`, `ROADMAP.md`, `docs/**` (every file), `install.sh`,
`sdk/**`, `examples/**`, `extensions/**` docs, `docs/MANIFEST.json`,
`docs/releases/**`.
Owned question: **the gap between what the project SAYS and what it DOES.**

Pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1). Binaries
`target/debug/codypendent` + `target/debug/codypendentd` built by the
orchestrator and used as-is. **No `cargo build`/`cargo test` was run by me.**

Everything below marked "actual" was computed or executed here, not read. Where
I only read code, it is marked **(inferred)**.

> **Environment note on backtraces.** This harness sets `RUST_BACKTRACE=1`. My
> first pass showed `anyhow` backtraces on CLI errors; re-running with
> `RUST_BACKTRACE=0` produced clean one-line errors. Those backtraces are **not**
> a product defect and are not reported as findings.

---

## Verdict lines

```
DOCS/NUMBERS — RUST TEST COUNTS: WORKING. A real CI gate now exists
  (.github/scripts/check_doc_test_counts.py + a `doc-counts` job). I ran it: all
  8 markers match. 7 of 8 prose numbers match their marker. One does not (§1.1).
DOCS/NUMBERS — EVERY OTHER NUMBER: BROKEN. The gate only understands Rust
  `#[test]`. The three numbers standing right next to the gated ones — 214
  vitest, 146 vitest, 387 models — are all wrong (§1.2).
RELEASE NOTES v0.5.0: BROKEN. The doc tells the user to run
  `codypendent update v0.5.0` and `install.sh … v0.5.0`. **There is no v0.5.0
  release** (GitHub API: HTTP 404). Only `v0.5.0-build.78` exists (§3).
RELEASE NOTES v0.5.1 / MIGRATION IMMUTABILITY: BROKEN — and worse than before.
  v0.5.1 *deleted* a correct user-safety warning and replaced it with a false
  all-clear. Published tags v0.1.0-build.43/.44/.45 really do carry different
  migration bytes than HEAD (§2). This is the single most misleading change in
  the release.
install.sh: BROKEN for a new user. The exact one-liner in the current release
  note exits 1 on a clean machine, and the reason it gives ("private repo") is
  factually false — the repo is public (§4).
DOCS/CLI SURFACE: PARTIAL. Every documented command still exists. Two
  statements are still contradicted by the command's own `--help`; 8 shipped
  hotkeys and one whole page-count are still undocumented/wrong (§5).
ROADMAP [x] MARKERS: WORKING on feature existence — I could not falsify one.
  The `[x]`/`[ ]` that are wrong are the *hygiene* and *gate* ones (§6).
docs/MANIFEST.json: PARTIAL — repaired since round 3 (146 listed / 0 missing),
  but already 2 files stale: the two newest release notes (§7).
sdk/ui + extensions/vscode: WORKING. The round-3 "README Develop block does not
  work" finding is genuinely FIXED — `sdk/ui` now has a `prepare` script and I
  ran the README block verbatim to green (§8).
examples/plugins/word-count: WORKING. The round-3 "repo's own example is
  rejected by the repo's own parser" finding is genuinely FIXED (§8).
```

---

## 1. Every number: claim → actual → verdict

Method: Rust counts via the repo's own methodology (regex over `crates/**/*.rs`
occurrences, not lines). Vitest counts from real `npm test` runs at HEAD.
Catalog counts by parsing the TOML with `tomllib`. Everything else from the
enum/registration site.

### 1.1 The gated Rust counts

**A real gate now exists.** `.github/workflows/ci.yml:154` runs
`.github/scripts/check_doc_test_counts.py`, which re-derives every
`<!-- doc-count:test … -->` marker in `ROADMAP.md` on every PR. I ran it:

```
$ python3 .github/scripts/check_doc_test_counts.py; echo "EXIT=$?"
doc-count check: OK — all 8 marker(s) in ['ROADMAP.md'] match reality
EXIT=0
```

This closes the round-3 finding "8 test counts stale low by 1.4x–7.9x". Credit
where due — this is the right shape of fix (the class, not the instance).

| # | Claim | Location | Claimed | **Actual (computed)** | Verdict |
|--:|---|---|--:|--:|---|
| 1 | workspace total | `ROADMAP.md:107` + marker `:108` | 2771 | **2771** | ✅ |
| 2 | sandbox unit tests **(prose)** | `ROADMAP.md:453` | **154** | **155** | ❌ **WRONG** |
| 3 | sandbox unit tests **(marker)** | `ROADMAP.md:454` | 155 | 155 | ✅ |
| 4 | sandbox integration tests | `ROADMAP.md:453`/marker `:455` | 11 | **11** | ✅ |
| 5 | multimodal round-trip/gate | `ROADMAP.md:471` + marker `:472` | 29 | **29** (input.rs 9 + envelope.rs 20) | ✅ |
| 6 | theme tests | `ROADMAP.md:479` + marker `:480` | 23 | **23** (theme.rs 13 + theme_pack.rs 10) | ✅ |
| 7 | router tests | `ROADMAP.md:533` + marker `:534` | 53 | **53** | ✅ |
| 8 | promotion pipeline tests | `ROADMAP.md:551` + marker `:552` | 21 | **21** | ✅ |
| 9 | TUI shell tests | `ROADMAP.md:580` + marker `:581` | 579 | **579** | ✅ |

**Finding 1.1a — the prose and the marker disagree, and the gate cannot see it.**
`ROADMAP.md:453` says "**154** unit tests"; the marker one line below at
`ROADMAP.md:454` says `expect=155`; the real count is **155**. The checker reads
only the marker, so CI is green while the sentence a human reads is wrong. The
gate's own failure message tells an author to "Fix the prose AND the marker"
(`check_doc_test_counts.py:105`) — nothing enforces the prose half. This is the
gate reproducing, in miniature, the exact defect it was built to prevent.

**Finding 1.1b — the ROADMAP's own re-derivation instructions produce the wrong
number.** `ROADMAP.md:110-114` tells the reader to re-derive with:

```
git ls-tree -r --name-only HEAD -- crates | grep '\.rs$' | xargs -I{} git
show HEAD:{} | grep -cE '#\[(tokio::)?test\]'
```

Run verbatim, that prints **2770**, not the 2771 the same paragraph claims:

```
$ git ls-tree -r --name-only HEAD -- crates | grep '\.rs$' | xargs -I{} git show HEAD:{} | grep -cE '#\[(tokio::)?test\]'
2770
```

Cause: `xargs` pipes every file into **one** `grep -c`, which counts matching
*lines*; the checker counts *occurrences*. Two source lines in the tree carry the
attribute twice. A reader who follows the ROADMAP's instruction concludes the
document is stale when it is not.

### 1.2 Every number the gate does NOT cover

`TEST_ATTR_RE = re.compile(r"#\[(?:tokio::)?test\]")`
(`check_doc_test_counts.py:59`) — Rust only. Everything else drifted again.

| # | Claim | Location | Claimed | **Actual (computed)** | Verdict |
|--:|---|---|--:|--:|---|
| 10 | VS Code vitest suite | `ROADMAP.md:210` | **214** tests, 8 files | **217** tests, 8 files | ❌ WRONG |
| 11 | VS Code vitest suite (repeat) | `ROADMAP.md:223` | **214** | 217 | ❌ WRONG |
| 12 | `vitest` passed | `docs/releases/v0.5.0.md:134` | **146** | **66** (sdk/ui) + **217** (ext) = **283** | ❌ WRONG (matches neither suite nor the sum) |
| 13 | curated `[[model]]` rows | `docs/releases/v0.4.0.md:34` | **387** | **386** | ❌ WRONG |
| 14 | providers in catalog | (implied, same line) | 42 | **42** | ✅ |
| 15 | council TUI builder pages | `docs/cli-and-tui-user-guide.md:586-592` | **4** | **7** (`CouncilBuilderStep`, `crates/tui/src/state.rs:112-120`) | ❌ WRONG |
| 16 | council TUI builder steps | `README.md:249` | **seven** | 7 | ✅ |
| 17 | built-in themes | `guide:120` | seven | **7** (`ThemeVariant`) | ✅ |
| 18 | semantic-token variants beyond dark | `ROADMAP.md:473` | six | 6 (7 total) | ✅ |
| 19 | `github.*` tools | `ROADMAP.md:207` | five | **5** | ✅ |
| 20 | council size / rounds | `guide:546`, `README:238` | 2-8 / 1-3 | `MAX_MEMBERS=8`, `MAX_ROUNDS=3`, `(2..=8)`, `(1..=3)` (`crates/council/src/service.rs:38,39,1351,1354`) | ✅ |
| 21 | agent modes | `guide:766` | 5 | **5** (+`Unknown` forward-compat) | ✅ |
| 22 | 22-point slot registry | `extensions/vscode/README.md:93` | 22 | **22** (`UI_CONTRIBUTION_POINTS`, `sdk/ui/src/protocol.ts:21`) | ✅ |
| 23 | eval corpus | `evals/README.md:43,213` | 13 shipped, 50-100 goal unmet | **13** files in `evals/tasks/core/` | ✅ honest |
| 24 | migrations `0025`–`0032` | `docs/releases/v0.5.0.md:134` | 0025-0032 | all 8 exist | ✅ |
| 25 | workspace crates | `docs/README.md:60-84` | 9 | **17** members | ❌ STALE ("Suggested", soft) |
| 26 | doc-suite file list | `docs/MANIFEST.json` | 146 | 149 on disk (3 unlisted) | ⚠ see §7 |

**Actual run evidence for #10-12:**

```
$ cd sdk/ui && npm test
 Test Files  13 passed (13)
      Tests  66 passed (66)

$ cd extensions/vscode && npm test
 Test Files  8 passed (8)
      Tests  217 passed (217)
```

**Actual computation for #13:**

```
$ python3 -c "import tomllib; d=tomllib.load(open('crates/providers/builtin_catalog.toml','rb')); print(len(d['provider']), len(d['model']))"
42 386
```

`docs/releases/v0.4.0.md:34` — "**387** curated `[[model]]` rows ship for every
provider in the catalog". The catalog has **386**. The 42-provider half is right.

### 1.3 Version strings

| Item | Claimed | Actual | Verdict |
|---|---|---|---|
| `agent-framework-*` | `0.2.0` (`docs/docs/build/00-how-to-use-this-guide.md:77`) | `0.2.0` in `Cargo.toml:59-61` **and** `Cargo.lock` | ✅ **FIXED since round 3** |
| MSRV in build guide | `≥ 1.88` (`build/00-…:66`) | `Cargo.toml:34` `rust-version = "1.88"` | ✅ **FIXED since round 3** |
| MSRV in `Cargo.toml`'s own comment | `1.82` (`Cargo.toml:85`) | line 34 says `1.88` | ❌ same file contradicts itself, 51 lines apart |
| "The **pinned** toolchain (`rust-toolchain.toml`)" (`Cargo.toml:32-33`) | pinned | `rust-toolchain.toml` is `channel = "stable"` — a floating channel, not a pin | ❌ still false |
| Extension manifest | `0.4.2` (`extensions/vscode/package.json`) | workspace `0.5.1` | ❌ **five releases behind** (was three at round 3 — got worse) |
| Doc-suite version | `0.4` / `2026-08-13` (`MANIFEST.json`) vs `0.3` / `15 July 2026` (`docs/README.md:91,93`) | product `0.5.1` | ❌ two doc-metadata blocks still disagree |
| `sdk/ui` | `1.1.0` — independently versioned | — | ✅ fine |

---

## 2. TOP FINDING — v0.5.1 deleted a true safety warning and shipped a false one

**Class (c) — wire attached, wrong behaviour.** This is a *documentation
regression*: the previous text was correct in its conclusion and was replaced
with an all-clear that is provably false.

### What v0.5.1 changed

`git diff ae4aaab e978d37 -- docs/cli-and-tui-user-guide.md` — the release
**removed** this (correct-in-conclusion) warning:

> `migration files are **meant** to be immutable once released … but that
> promise has already been broken once in practice … If you maintain this
> project, treat that as a standing bug, not a documentation nit.`

and replaced it with `docs/cli-and-tui-user-guide.md:53-57`:

> "**Released migration files are immutable**: `sqlx` checksums every applied
> migration and refuses to boot if one changes underneath an installation.
> Schema changes therefore always ship as a new numbered migration; the v0.1.1
> promotion migration **has been verified byte-for-byte against the published
> tag**."

`docs/releases/v0.5.1.md:21-22` books this as a *fix*: "Upgrade documentation now
reflects the current version and the **verified, immutable migration history**."

### Why it is false

The general claim ("released migration files are immutable") is falsified by the
project's own published releases. I hashed `migrations/0003_phase2.sql` at every
remote tag:

```
2026-07-30 15:08:53  v0.1.0-build.42  0003=a29143289fa4
2026-07-30 15:09:07  v0.1.0-build.43  0003=a5c81199c24b   <-- different bytes
2026-07-30 15:09:24  v0.1.0-build.44  0003=a5c81199c24b   <-- different bytes
2026-07-30 17:18:14  v0.1.0-build.45  0003=a5c81199c24b   <-- different bytes
2026-07-30 17:36:07  v0.1.0-build.46  0003=a29143289fa4
```

HEAD ships `a29143289fa4`. **`v0.1.0-build.43`, `.44` and `.45` are real,
published, downloadable GitHub releases** (confirmed in the releases API listing)
that shipped `a5c81199c24b`. Anyone who installed one of those three and upgrades
to v0.5.1 hits the `sqlx` checksum refusal — the daemon will not start.

The repo's own `migrations/README.md:9-11` says so in plain words, and is the
only document in the tree that gets it right:

> "This is not hypothetical: a comment clarification to `0003_phase2.sql`
> (2026-07-30) hard-failed daemon startup on every pre-existing database."

### The narrower claim is also imprecise

"the v0.1.1 promotion migration has been verified byte-for-byte against **the
published tag**" — there is **no `v0.1.1` tag**. The nearest published artifact
is `v0.1.1-build.50`. That tag *does* carry HEAD's bytes for
`0017_promotion_evidence.sql` (`5d5adab8…` at both), because the mutating commit
`7eef118` landed **before** the tag was cut. So 0017 specifically was never
shipped mutated — the round-3 report and the checklist both blamed the wrong
migration. But the *conclusion* they drew was right, and v0.5.1 threw the
conclusion away along with the wrong example.

### Four shipped documents now disagree at one commit

| Document | Says |
|---|---|
| `docs/cli-and-tui-user-guide.md:53-57` (rewritten by v0.5.1) | immutable, verified — **FALSE** |
| `docs/releases/v0.5.1.md:21-22` | "verified, immutable migration history" — **FALSE** |
| `ROADMAP.md:645-658` | `- [ ] Migrations unchanged since first commit — **false, verified**` — right conclusion, wrong migration named (0017) |
| `docs/docs/build/99-master-acceptance-checklist.md:11-21` | "**already violated once**… do not check this box while pretending the violation didn't happen" — right conclusion, wrong migration named (0017) |
| `migrations/README.md:9-11` | names `0003_phase2.sql` correctly — **the only accurate one** |

**User-visible consequence:** the one document a user reads before upgrading
(`cli-and-tui-user-guide.md` §1, "Install and upgrade") now tells them the
upgrade is safe. For an install from build.43/.44/.45 it is not: the daemon
refuses to boot and the user has no pointer to why.

---

## 3. TOP FINDING — `docs/releases/v0.5.0.md` installs a release that does not exist

**Class (a) — the thing the doc names was never built.**

`docs/releases/v0.5.0.md:139` and `:145`:

```sh
codypendent update v0.5.0
curl -fsSL https://raw.githubusercontent.com/CodeHalwell/codypendent/main/install.sh | bash -s -- v0.5.0
```

There is no `v0.5.0` release:

```
$ curl -sS -o /dev/null -w "HTTP %{http_code}\n" https://api.github.com/repos/CodeHalwell/codypendent/releases/tags/v0.5.0
HTTP 404
```

The releases API lists 73 releases; the only 0.5.x entries are `v0.5.1`,
`v0.5.1-build.80`, and `v0.5.0-build.78` (a prerelease). I checked every release
note's install command the same way — **v0.5.0.md is the only one that names a
nonexistent tag**:

```
release note           tag it tells the user to install       exists?
v0.4.3.md              v0.4.3                                 YES
v0.4.4.md              v0.4.4                                 YES
v0.4.5.md              v0.4.5                                 YES
v0.4.6.md              v0.4.6                                 YES
v0.5.0.md              v0.5.0                                 *** NO SUCH RELEASE ***
v0.5.1.md              v0.5.1                                 YES
```

v0.5.0.md is the flagship document of this whole repair effort — it is the note
that describes all ten shipped outcomes and is linked from the product review.
Both of its upgrade commands fail.

---

## 4. TOP FINDING — the documented installer does not run on a clean machine, and lies about why

**Class (c).** All three round-3 `install.sh` defects are unfixed at HEAD.

### 4a. The current release note's one-liner exits 1

I ran the exact command from `docs/releases/v0.5.1.md:33`:

```
$ curl -fsSL https://raw.githubusercontent.com/CodeHalwell/codypendent/main/install.sh | bash -s -- v0.5.1
error: GitHub CLI (gh) is required — https://cli.github.com
[exit 1]
```

`install.sh:25` hard-exits without `gh`. The script itself downloaded fine over
plain `curl` (HTTP 200), which is the proof that `gh` is not needed to reach this
repository.

### 4b. The stated reason is factually false

`install.sh:7` — "One-liner (uses your existing `gh` auth, so **it works for a
private repo**)". And `crates/cli/src/update.rs:117`:

```
$ codypendent update --check
Error: GitHub CLI (`gh`) is required to download releases from the private repo — install it from https://cli.github.com and run `gh auth login`
[exit 1]
```

The repository is **public**:

```
$ curl -sS https://api.github.com/repos/CodeHalwell/codypendent | python3 -c "..."
private= False visibility= public
```

Neither the installer nor `update` needs `gh` for a public repo. A user who
follows the release note downloads a script that refuses to run, citing a
restriction that does not exist. This is the highest-frequency first-contact
failure in the whole doc set.

### 4c. `CODYPENDENT_LIB` is a knob nothing reads

`install.sh:23` offers `LIBDIR="${CODYPENDENT_LIB:-…}"`.

```
$ grep -rn "CODYPENDENT_LIB" crates/
(no output)
```

The only paths the product probes are `<bindir>/node-runtime` and
`<bindir>/../lib/codypendent/node-runtime`
(`crates/daemon/src/remote_ui_plugins.rs:1517-1519`,
`crates/cli/src/update.rs:248`). Setting `CODYPENDENT_LIB` anywhere else installs
the Node runtime where the daemon will never look, and Remote UI fails closed.
Class **(c)**.

### 4d. Stale example

`install.sh:14` still advertises `… | bash -s -- v0.1.0-build.17`. Current is
v0.5.1.

**Not executed:** I did not run `install.sh` past its `gh` check — no `gh` on
this machine, and the extraction would have cost ~350 MB of shared disk. Lines
28-100 (target detection, tarball layout checks, install/copy) are unverified by
me this round; `bash -n install.sh` is clean.

---

## 5. Every documented command, run against the real binary

I ran `codypendent --help` plus the `--help` of all 21 top-level commands and
their nested subcommands, and diffed against the guide.

**Every command the guide documents exists.** No documented command is absent or
renamed. Verified by execution in an isolated data dir
(`CODYPENDENT_DATA_DIR`/`CODYPENDENT_CONFIG_DIR`/`CODYPENDENT_SOCKET` under
`/tmp/review-docs-vs-reality/`):

```
$ codypendent daemon status --json
{"running":false}                                                   [exit 1]
$ codypendent daemon status
daemon is not running                                               [exit 1]
$ codypendent mcp list
no MCP servers configured (create …/cfg/mcp.toml)                   [exit 0]
$ codypendent council list
no councils configured; run `codypendent council create --help`     [exit 0]
$ codypendent docs list
No documents yet. Create one with `codypendent docs new "<title>"`. [exit 0]
$ codypendent docs check
Checked 0 document(s); 0 symbol link(s) resolved.
No stale documentation found.                                       [exit 0]
$ codypendent index rebuild
index rebuild complete: 29 registry item(s) re-indexed from authority;
  canary "run the tests" -> 12 tool card(s), 0 skill card(s)         [exit 0]
$ codypendent models list
no models configured (…/data/models.toml)                           [exit 0]
$ codypendent workflow validate docs/specs/workflow.yaml
✓ repair-github-check v1 valid — 5 step(s), 3 agent step(s);
  order: inspect → patch → verify → review → publish                [exit 0]
$ codypendent acp list
… 39 registry entries, incl. claude-acp, codex-acp, gemini,
  mistral-vibe, opencode, amp-acp                                    [exit 0]
```

`daemon status` behaves exactly as `guide:67` claims (exit 1 when stopped,
`{"running":false}` under `--json`).

### 5a. Two statements the CLI's own `--help` contradicts — one still unfixed

**`guide:624` (§4.8) — still wrong.** The guide:

```bash
# Rebuild Tantivy search and tree-sitter code graph indexes from SQLite
codypendent index rebuild
```

The command's own help says the opposite, twice:

```
$ codypendent index --help
Maintain the knowledge fabric's derived RETRIEVAL indexes — full-text (BM25) +
vectors (Phase 2). This is search, NOT the code graph: the code-graph
nodes/edges are built per-repository when you open a session or start a run,
not by this command

Commands:
  rebuild  Delete the derived RETRIEVAL indexes (BM25 + vectors) and rebuild
           them from the authoritative rows. Does NOT rebuild the code graph …
```

**User-visible consequence:** a user with a stale code graph runs
`index rebuild`, gets `index rebuild complete: …` (exit 0), and the code graph is
untouched. Class **(c)**. Flagged in round 3; unfixed.

**`docs/context-window.md` — FIXED.** It now correctly says
`<data_dir>/models.toml` and explicitly warns "the **data** directory, not
`<config_dir>`" (`docs/context-window.md:9-11`). `codypendent doctor` agrees.
Round-3 finding closed.

### 5b. §3.2 "Complete Hotkey Matrix" is still not complete — same 8 keys

`crates/tui/src/input.rs:367-390` (`map_normal_char`) binds 8 normal-mode keys
absent from the §3.2 table:

| Key | Action | `input.rs` line |
|---|---|--:|
| `e` | `EditDoc` | 367 |
| `i` | `InsertDocBlock` | 368 |
| `P` | `PublishDoc` | 369 |
| `t` | `EnableUiPluginSession` | 379 |
| `u` | `EnableUiPluginUser` | 380 |
| `x` | `RevokeUiPlugin` | 381 |
| `y` | `CopyFocusedCard` | 389 (guide lists only `Alt-Y`) |
| `X` | `DeleteDocBlock` | 390 |

Set difference computed mechanically: keys bound = `k j n p c s q ? a A r S M J
o e i P D G W B C K t u x d y X /`; keys in the §3.2 table = `/ ? A B C D G J K
M S W a c d j k n o p q r s`. The four doc keys (`e`/`i`/`P`/`X`) are the entire
editing surface of the Docs Studio the guide sends you to with `D`; `t`/`u`/`x`
are the plugin enable/revoke controls. Identical to round 3 — unfixed.

### 5c. §4.6.1 council builder page count — still wrong

`guide:586-592` numbers **4** pages. `CouncilBuilderStep`
(`crates/tui/src/state.rs:112-120`) has **7** variants: `Name, Description,
MemberModel, MemberRole, Chair, Rounds, Review`. `README.md:249` says
"seven-step builder" and is correct. A user following the guide is on page 5 of a
flow the guide says has 4. Unfixed.

### 5d. `codypendentd --version` / `--help` start a daemon

```
$ timeout 5 ./target/debug/codypendentd --version
2026-08-13T22:49:37Z WARN  removing stale socket …
2026-08-13T22:49:37Z INFO  codypendentd starting instance=… boot_count=6 …
2026-08-13T22:49:37Z INFO  daemon listening socket=… pid=32290
[exit 124 — timed out, still running]
```

Both flags are ignored and a full daemon boots against the user's **real** data
dir (it incremented `boot_count` twice while I probed). `guide:16` names
`codypendentd` as "The backend daemon executable" — the two flags every user
tries first on an unfamiliar binary have no output and a side effect. Class
**(c)**, low-moderate severity.

### 5e. `codypendent plugin list` fails closed in a dev build

```
$ codypendent plugin list
Error: plugin lifecycle command rejected: the enforcing Remote UI worker runtime
is unavailable; plugin management fails closed (plugin.runtime-unavailable)
[exit 1]
```

`guide:310` documents `plugin list` as an ordinary step in the §4.3 flow. It
fails without the bundled Node runtime, which only ships in a release tarball —
so it is correct for an installed product and broken for anyone working from a
`cargo build`. The guide does not mention the dependency. **(inferred)** that a
release install would succeed — I could not install a release (§4).

### 5f. Minor: `completion` panics on `SIGPIPE`

`codypendent completion bash | head` exits **101** (Rust's default EPIPE panic).
The documented form (`… > /etc/bash_completion.d/codypendent`, `--help` text)
exits 0 and emits 187 KB correctly. Cosmetic; the documented usage works.

---

## 6. ROADMAP `[x]` markers, aggressively sampled

**I could not falsify a single `[x]` on feature existence.** Every named file,
type, command and count in the claims I checked resolves to real, reachable code.

| Claim | Check I ran | Result |
|---|---|---|
| **5.1** compiler + canonical manifest | `codypendent workflow validate docs/specs/workflow.yaml` | ✅ `✓ repair-github-check v1 valid — 5 step(s), 3 agent step(s)` |
| **5.1.4** "`/fix-ci` now runs that declarative workflow (the hard-coded prompt template is gone)" | grep for the workflow name on the CLI path | ✅ `crates/cli/src/commands.rs:1422,1448` resolve `repair-github-check`; `main.rs:147` help text matches |
| **6.1** "Surfaced to users via `plugin inspect` … with example manifests under `examples/plugins/word-count/`" | ran it | ✅ **FIXED** — see §8 |
| **6.6** "six semantic-token variants beyond dark" | `ThemeVariant` enum | ✅ 7 total, matches `--theme`'s accepted values |
| **7.1** eval harness + runnable corpus | `ls evals/tasks/core/*.json` | ✅ 13 cases; `evals/README.md` honestly states the 50-100 goal is unmet |
| **7.2/7.5** router + promotion CLI | `models {list,add,check,bench,list-providers,pull}`, `promote {propose,advance,approve,rollback}` | ✅ all present |
| **v0.5.0** "`graph.callers_of/blast_radius/tests_covering` are model-callable" | `crates/runtime/src/tools/graph.rs:42,46,50` | ✅ the three `NAME` constants exist **(inferred — I did not drive a model)** |
| **v0.5.0** "the four `docs.*` tools reach the model" | `crates/runtime/src/tools/docs.rs:41,47` | ✅ exists **(inferred)** |
| **v0.5.0** "`models list-providers` exists" | `codypendent models --help` | ✅ present |
| **2.x** `index rebuild` | ran | ✅ exit 0, 29 items re-indexed |
| **3.5** VS Code extension green | `npm ci && npm test && npm run typecheck` | ✅ all green (count wrong, §1.2) |

### The `[x]`/`[ ]` that ARE wrong are structural, not feature claims

1. **`ROADMAP.md:11-12` names a release gate with zero boxes ticked.** "the
   release gate is the [Master Acceptance Checklist]". That file has **0 `[x]`
   and 34 `[ ]`** — measured — while v0.5.1 has shipped. Meanwhile
   `ROADMAP.md:638-644` ticks the same criteria `[x]`. The two documents
   contradict each other about whether the release gate passed. Identical to
   round 3; unfixed.

2. **`ROADMAP.md:645`** — `- [ ] Migrations unchanged since first commit —
   **false, verified**`. The box is correctly *unchecked*, but the evidence text
   names the wrong migration (0017; the real one is 0003 — §2), and it is now
   directly contradicted by `cli-and-tui-user-guide.md:53-57` at the same commit.

3. **`ROADMAP.md:253`** — `- [ ] Declarative workflows; durable checkpoint
   storage; supervisor/specialist delegation; blackboard` is an unchecked parent
   whose every child (5.1, 5.2, 5.3, and line 405 "all landed") is `[x]`, inside
   a phase headed `✅`. The parent is the only thing a scanner reads. Unfixed
   from round 3.

Totals: **84 `[x]`, 13 `[ ]`** in `ROADMAP.md`.

---

## 7. `docs/MANIFEST.json`

**Largely repaired.** Round 3: 57 listed / 130 on disk. Now:

```
listed in MANIFEST : 146
files on disk      : 149
listed but MISSING from disk (0):
on disk but UNLISTED (3): MANIFEST.json, releases/v0.5.0.md, releases/v0.5.1.md
```

Nothing listed is missing — no dangling entries. The three unlisted are the
manifest itself (fine) and **the two newest release notes**. The manifest's
`"date"` is `2026-08-13` (today), so it was regenerated *during* this release
cycle and still missed the release notes that cycle produced. Same failure mode
as the counts: hand-maintained, no generator, no gate.

`"version": "0.4"` while the product is `0.5.1`, and `docs/README.md:91,93` says
"Document version **0.3** / **15 July 2026**" — two doc-metadata blocks that have
disagreed since round 3.

---

## 8. Two round-3 findings are genuinely FIXED — verified by running them

### 8a. `examples/plugins/word-count` — FIXED

Round 3: "the repo's own example manifest is rejected by the repo's own parser;
`plugin inspect` and `plugin diff` both exit 1."

```
$ codypendent plugin inspect examples/plugins/word-count/plugin.toml
word-count v0.1.0 (wasm-component) — publisher codypendent-project
  trust: unsigned (sha256:set-during-packaging), sandbox profile compute-only
  capabilities:
    filesystem_read: /home/plugin-user/workspace
  resources: 32 MB mem, 5 CPU s, 10 wall s, 1 MB output
  scopes: repository
[exit 0]

$ codypendent plugin diff examples/plugins/word-count/plugin.toml examples/plugins/word-count/plugin-v2.toml
word-count: permission changes:
+ network: telemetry.example.com:443
→ update EXPANDS permissions — re-approval required (exit criterion 2).
Error: update expands permissions — re-approval required before it can be applied
[exit 1]
```

`inspect` exits 0 and renders the capability list; `diff` renders the permission
diff correctly and exits non-zero **as documented** (`ROADMAP.md:459-460`: "exits
non-zero on an expansion, so CI can gate on re-approval"). Both work.

### 8b. `extensions/vscode/README.md` "Develop" block — FIXED

Round 3: `npm test` failed on a clean clone (4 of 8 files could not resolve
`@codypendent/ui`) because `sdk/ui` had no `prepare` script.

`sdk/ui/package.json` now has `"prepare": "npm run build"`, and
`extensions/vscode/README.md:144-146` documents it: "`npm install` also builds
`@codypendent/ui` … no separate `cd ../../sdk/ui && npm ci && npm run build` step
needed first." I ran the README block on a tree with no `node_modules` anywhere:

```
$ cd sdk/ui && npm test
 Test Files  13 passed (13)     Tests  66 passed (66)
$ cd extensions/vscode && npm ci && npm test
found 0 vulnerabilities
 Test Files  8 passed (8)       Tests  217 passed (217)
```

Green. Round-3 finding closed. (The *count* in `ROADMAP.md:210,223` is still
wrong — §1.2 #10.)

### 8c. Version/MSRV drift — FIXED

The round-3 finding "the build guide pins agent-framework 0.1.1 / rustc ≥ 1.82;
the workspace is on 0.2.0 / 1.88 — following the build guide cannot compile the
tree" is fixed. `docs/docs/build/00-how-to-use-this-guide.md:66,77` now say 1.88
and 0.2.0, and line 77 even carries a standing instruction to re-verify against
`Cargo.toml`. Residual: `Cargo.toml:85`'s own comment still says 1.82, and
`rust-toolchain.toml` is still `stable`, not a pin.

---

## 9. The twenty target outcomes — what the docs PROMISE vs what ships

My lane is the promise. Verdicts below are about **disclosure**, not machinery.

| # | Outcome | Does the doc set promise it honestly? | Evidence |
|--:|---|---|---|
| 1 | Polished TUI | ✅ honest | `README:136-147`, `guide §3`; v0.5.0 lists the specific TUI repairs |
| 2 | ACP + model discovery | ✅ honest | `README:198-221` + `guide §4.6`; v0.5.0:44-47 discloses the serve-mode failure and its fix. `acp list` really returns 39 live entries |
| 3 | Model selection | ⚠ **387 vs 386** (§1.2 #13); v0.5.0:48-52 honestly discloses the Anthropic and context-clamp fixes | `docs/releases/v0.4.0.md:34` |
| 4 | Skill-writer + doc-writer | ⚠ `docs.*` disclosed as fixed (v0.5.0:53-58). **No doc anywhere promises a skill-writer**, and none exists — silence rather than a false claim | — |
| 5 | DAG viewer | ⚠ `v0.4.0.md:44-48` promises "the TUI paints a layered DAG … and `workflow.query` gives the agent the same graph state the human sees" in the present tense. Not re-qualified anywhere after the product review scored it BROKEN | `docs/releases/v0.4.0.md:44` |
| 6 | AI council | ✅ honest | v0.5.0:63-65 discloses the failure-reason and quorum fixes. But `guide §4.6.1` still says 4 builder pages vs 7 (§5c) |
| 7 | Rich chat stream | ✅ honest | v0.5.0:36-43 names the exact fold bug and its fix |
| 8 | TTS + STT | ✅ **exemplary** | `guide:637-643` carries a `> [!WARNING]` that voice "was developed and tested on a machine with **no audio hardware**"; v0.5.0:116-117 states "No audio crates ship"; `README:130-134` names image/file blocks as unread. Best disclosure in the repo |
| 9 | Vector top-k | ✅ honest | v0.5.0:53-58 quotes the old code's own comment ("stays static and fully advertised — ALWAYS") |
| 10 | Blackboard + kanban | ✅ honest | v0.5.0:66-69 names the three-spellings board-identity bug |
| 11 | Live measured routing | ✅ honest | `codypendent routing --help` says "Default OFF"; `ROADMAP:496-508` keeps Phase 7 🟡 |
| 12 | Executable skills (WASM) | ✅ honest | v0.5.0:114-115 "WASM skills have no invocation path yet… the `skills.run` tool… is not landed"; `ROADMAP:481-486` `[ ]` |
| 13 | Hook engine | ✅ honest | v0.5.0:111-113 "**Hooks cannot fire.** … no dispatch site. Nothing calls a hook" |
| 14 | Live code graph | ✅ honest | v0.5.0:87-90 "the filesystem watcher is armed" — but `guide:624` still misdescribes `index rebuild` (§5a) |
| 15 | Delegation | ✅ honest | v0.5.0:91-93 |
| 16 | Evals as a product loop | ✅ honest | v0.5.0:75-79 admits it "scored **passes for runs that never executed**"; `evals/README.md:213` admits the corpus is 13 not 50-100 |
| 17 | Compounding memory | ✅ honest | v0.5.0:94-97; `guide §4.7` documents the Learning Journey keys |
| 18 | Docs round-trip | ✅ honest | v0.5.0:98-100 |
| 19 | Real multi-user | ✅ **exemplary** | `v0.5.0.md:10-32` leads with the security section, names the bypass, and cites `crates/daemon/tests/multi_user_it.rs` |
| 20 | The ledger made visible | ❌ **promise not withdrawn** | `guide:84-88` promises the session strip shows "measured cost". v0.5.0:124 admits "**`UiRunProjection.cost` is still `None`**". `crates/tui/src/render.rs:9782-9787` `format_cost` returns `"—"` for `None`. The release note is honest; the user guide was never updated to match |

**Net:** the release notes for v0.5.0 are the most honest documents in the repo —
they have a "Known limitations — what this release does NOT do" section that
states five gaps plainly. The failure is that those admissions were **not
propagated back into the user-facing guide**, so the guide still promises cost
(20) and still misdescribes `index rebuild` (14).

### Also unpropagated: README present-tense IDE claims

`README.md:101-105` states in the present tense: "Zed: ACP-first + thin extension
for ACP gaps" and "**JetBrains: Kotlin IntelliJ platform plugin**".

```
$ ls extensions/
vscode
$ find . -ipath ./target -prune -o -iname "*jetbrains*" -print -o -iname "*.kt" -print
(no output)
```

Zed is genuinely served by `codypendent acp serve` (no extension needed), so that
half is defensible. **There is no JetBrains plugin in any form** — zero Kotlin
files, zero directories. `docs/README.md:74-77` compounds it by listing
`extensions/jetbrains/` and `extensions/zed/` in the repository shape (marked
"Suggested", so soft).

---

## 10. Smaller, still user-facing

- **`docs/README.md:60-84`** "Suggested repository shape" lists 9 crates (actual
  **17**), plus `extensions/jetbrains/`, `extensions/zed/`, root `specs/` and
  root `tests/` — none exist. Marked "Suggested"; lowest priority.
- **`extensions/vscode/package.json` version `0.4.2`** vs workspace `0.5.1` —
  five releases behind. Was three behind at round 3.
- **Migration numbering hole persists**: 31 `.sql` files numbered up to `0032`
  (`0020`/`0021` never existed). Harmless to `sqlx`; makes "migration NNNN"
  references unauditable by counting.
- **`Cargo.toml:85`** — "Pinned to a version that builds on the workspace's
  rust-version (**1.82**)" contradicts `Cargo.toml:34`'s `rust-version = "1.88"`
  51 lines above.
- **`rust-toolchain.toml`** is `channel = "stable"`. `Cargo.toml:32-33`'s safety
  argument ("The **pinned** toolchain … is newer, so builds are unaffected")
  rests on a pin that does not exist.
- **`check_doc_test_counts.py`'s own docstring** says "seven live examples"
  (line 15) and "None of the **seven** markers this repo ships today" (line 30).
  There are **8**. The checker's own prose drifted from the checker's own count.

### Doc claims I checked and found CORRECT (worth not deleting)

- `docs/specs/workflow.yaml` is `include_str!`'d as the shipped built-in
  `repair-github-check` manifest — spec and artifact cannot drift.
- `evals/README.md` remains the most honest document in the repo: it states 13
  cases, states the roadmap wants 50-100, and states what has no signal.
- `guide §5` (Voice) — the `> [!WARNING]` about untested audio hardware is
  exactly the right shape of disclosure.
- `docs/context-window.md` — path fixed and now carries an explicit
  "not `<config_dir>`" correction.
- `docs/TIMELINE.md` still carries its "historical planning doc … not current
  status" header.
- The `doc-counts` CI job is a real, cheap, correct gate that runs on every PR.

---

## What I did not verify

1. **`cargo test --workspace` / `clippy` / `deny`.** Forbidden by the brief. Every
   Rust count in §1.1 is a **static `#[test]`/`#[tokio::test]` occurrence count**
   using the repo's own methodology, not a live run. `ROADMAP.md:638-641`'s three
   hygiene `[x]` are unverified by me. The checker's own docstring notes it cannot
   see doc-tests, feature-gated tests, or macro-generated tests.
2. **`install.sh` past line 25.** No `gh` on this machine and no disk budget for a
   ~350 MB extraction. Target detection, tarball layout assertions, and the
   install/copy half (lines 28-100) are unverified this round. `bash -n` is clean.
3. **macOS / aarch64.** Linux only. The Gatekeeper `xattr` step
   (`install.sh:59-61`) and both Darwin targets are unexercised.
4. **The TUI itself.** No pty this round. Every §5b hotkey claim and the §5c
   builder-page count are verified against `crates/tui/src/input.rs` and
   `state.rs` by reading the reducer tables — **(inferred)**, not driven. Whether
   the session strip renders `—` for cost on screen is **(inferred)** from
   `render.rs:9782-9787`; another vertical owns the render check.
5. **A live model.** `models bench`, `acp connect`/`probe`, `council run`, voice,
   and every `graph.*`/`docs.*` tool-dispatch claim in §6 need credentials or a
   vendor binary. I verified the tools' `NAME` constants exist; I did **not**
   verify a model can call them.
6. **Write paths.** `docs publish`, `plugin install`, `promote approve` park
   durable approvals; I stayed read-only per the brief.
7. **Whether an upgrade from `v0.1.0-build.43/.44/.45` actually fails to boot.**
   I proved the shipped bytes differ and quoted `migrations/README.md`'s own
   statement that this "hard-failed daemon startup on every pre-existing
   database". I did not install build.43 and upgrade it — **(inferred)** from the
   byte difference plus `sqlx`'s documented checksum behaviour.

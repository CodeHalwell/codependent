# Orchestrator's own findings — round 4, at `c255bec`

Found by the orchestrator outside any vertical, while smoke-testing the built
binaries. Recorded here so they are not lost between review and implementation.

## O-1 — `codypendentd` parses no arguments at all; `--version` boots a daemon

`crates/codypendentd/src/main.rs` contains no clap parser and no argv handling.
Every flag is ignored and the process boots.

```
$ cd /tmp && timeout 10 ./target/debug/codypendentd --version
INFO codypendentd: codypendentd starting instance=019ffd4c-… boot_count=1
     database=/root/.local/share/codypendent/codypendent.db
INFO codypendentd: startup recovery complete …
=== EXIT: 143 ===          # SIGTERM from timeout — it never returned
$ timeout 10 ./target/debug/codypendentd --help
=== EXIT: 143 ===          # same
```

User-visible consequence: a user who runs the daemon binary with `--version` or
`--help` — the two flags every Unix binary answers — instead starts a daemon
that creates and writes their real database:

```
$ ls -la /root/.local/share/codypendent/
-rw-r--r-- codypendent.db       647168
-rw-r--r-- codypendent.db-wal  4120032
-rw------- daemon.secret             32
```

It also runs "startup recovery", which mutates existing state. There is no way
to ask the binary what version it is. Class **(a)** — engine missing entirely
for the argv surface.

## O-2 — `codypendent --help` leaks internal roadmap identifiers into every
## subcommand description

14 lines of the top-level help carry `Phase N`, `STEP N.N`, `PR B — MCP client`
or `ADR-010`:

```
  index    … derived RETRIEVAL indexes — full-text (BM25) + vectors (Phase 2) …
  docs     Publish a collaborative document to Git (Phase 4 STEP 4.4)
  eval     Run the evaluation harness against a fixture suite (Phase 7 STEP 7.1)
  promote  … the evaluation-gated promotion pipeline (Phase 7 STEP 7.5) —
           nothing promotes itself (ADR-010)
  mcp      Inspect the operator-declared MCP servers (PR B — MCP client)
  routing  Configure live measured routing (Phase 7 STEP 7.2/7.3, outcome 11):
           per-task-node model selection from benched profiles, instead of the
           Phase-1 resolver's first-reachable-candidate-in-file-order …
```

`codypendent --help` is the first thing a new user types. It currently explains
the product in terms of the internal build plan — including the literal string
"outcome 11", which is a line item from this review's own brief. Counted with
`codypendent --help | grep -cE "Phase [0-9]|STEP [0-9]|PR [A-Z] —|ADR-[0-9]"`
→ **14**. Target outcome 1 is "easy-to-use … every menu polished". Class **(c)**
— wire attached, wrong content.

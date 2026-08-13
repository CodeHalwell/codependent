# Phase 2 shared-surface ownership map

Written BEFORE any implementer touches code, per the brief. Every file listed
here is touched by more than one vertical. It has ONE owner. Everyone else
sends the owner a proposed diff; nobody else edits it.

Pinned commit: 535a2f5e3848b256536ddee94883dc0010ecdcb8

---

## 1. The monolith files — highest collision risk

| File | Lines | Owner | Who else needs it |
|---|---|---|---|
| `crates/runtime/src/agent.rs` | 10903 | **runtime-owner** | every vertical adding a tool (11,12,13,14,15,17,20) |
| `crates/tui/src/render.rs` | 15226 | **tui-owner** | 1,5,7,10,11,17,20 |
| `crates/tui/src/reduce.rs` | 14462 | **tui-owner** | same |
| `crates/tui/src/state.rs` | 3070 | **tui-owner** | same |
| `crates/daemon/src/server.rs` | 5252 | **daemon-owner** | 15,18,19,20 |
| `crates/cli/src/main.rs` | 1499 | **cli-owner** | 3,8,16,20 (new subcommands) |
| `crates/cli/src/commands.rs` | 3601 | **cli-owner** | same |
| `crates/protocol/src/command.rs` | 1273 | **protocol-owner** | any new wire command |
| `crates/protocol/src/events.rs` | 478 | **protocol-owner** | any new event |

Tools are NOT a trait registry — they are a match arm in `agent.rs`. Any
vertical adding a model-callable tool must go through runtime-owner. This is
the single biggest merge hazard in the repo.

## 2. `models.toml` — four independent writers, one already broken

Writers:
  - `crates/cli/src/commands.rs:2894`   (models_add)     **BROKEN, see F-ORCH-1**
  - `crates/cli/src/models_pull.rs:303` (models pull)    correct
  - `crates/cli/src/tui.rs:4279`        (TUI add-model)  correct
  - `crates/cli/src/acp_clients.rs:503` (ACP connect)    correct

Owner: **cli-owner**. Required first change: collapse all four onto ONE
`write_models_toml(path, mutate: impl FnOnce(&mut toml::Table))` helper that
edits the parsed document in place. Do not fix commands.rs in isolation — that
reproduces the exact per-site-patching pattern that caused the bug.

Sections in this file: `[[model]]`, `[embedding]`, `[retrieval]`,
`[transcription]`, `[speech]`. Outcomes 8, 9 and 11 all add to it.

## 3. The two disconnected Capability enums — decide before outcome 12

  - `crates/daemon/src/policy/scope.rs:30`  — the RUN capability model.
    Structured scopes: `FileRead(PathScope)`, `FileWrite(PathScope)`,
    `CommandExecute(CommandScope)`, `NetworkConnect(NetworkScope)`,
    `GitCommit`, `GitPush`, `McpToolCall{server}`, `CouncilManage`,
    `WorkflowManage`. This is what the deny-first run policy actually enforces.
  - `crates/sandbox/src/permission.rs:25`   — the PLUGIN capability model.
    Flat strings: `FilesystemRead(String)`, `FilesystemWrite(String)`,
    `Network(String)`, `Secret(String)`, `Subprocess`.

**There is no conversion between them.** No `impl From`, no shared trait.

Outcome 12 says executable skills run "under the existing deny-first policy".
If the WASM host is gated on the sandbox enum, the run policy never sees it and
outcome 12 ships a second, weaker policy path — a privilege-escalation route,
and precisely what outcome 13's threat model must forbid.

Owner: **policy-owner**. Decision required before outcome 12's first line:
either (a) sandbox capabilities lower into run capabilities at the WASM host
boundary, or (b) the WASM host is gated on the run enum directly and the
sandbox enum stays plugin-manifest-only. Write it down either way.

## 4. Migrations — numbers assigned centrally

Existing: 0001-0019, then **0022-0024** (0020/0021 absent, unexplained —
F-ORCH-7). Next free number is **0025**.

Assigned in advance so two verticals cannot both claim 0025:

| Number | Outcome | Vertical |
|---|---|---|
| 0025 | 11 live measured routing | routing |
| 0026 | 12 executable skills | sandbox |
| 0027 | 13 hook engine | sandbox |
| 0028 | 15 delegation | workflow |
| 0029 | 17 compounding memory | knowledge |
| 0030 | 18 docs round-trip | docs |
| 0031 | 19 multi-user | daemon |
| 0032 | 20 ledger reader | daemon |

Take your number even if you end up not needing it; do not renumber.
`migrations/README.md` is explicit: **migrations are immutable once merged.**
sqlx checksums every byte including comments and refuses to boot on a change.
Get the comment right the first time.

## 5. `skill.toml` — schema owned by knowledge

`crates/knowledge/src/manifest.rs:42` `SkillManifest`. Outcome 12 must add
resource limits and capability grants here. Owner: **knowledge-owner**.
Note the separate `crates/sandbox/src/manifest.rs` plugin manifest — different
file, different schema, do not conflate.

## 6. Rules restated for every implementer

  - Never serialize a shared config file from a struct that models only your
    own section. Edit the parsed document in place.
  - Any check that gates access must be enforced where the resource is FETCHED,
    not only where it is LISTED. If a list filters by scope, the direct-by-id
    path needs the same gate, and must fail identically for "not allowed" and
    "does not exist".
  - If you add a scope, status or capability, grep every existing filter over
    that dimension and update it.
  - Outcomes 12, 13, 15, 19 widen what untrusted input can reach. Each needs a
    written threat model before its first line of code.

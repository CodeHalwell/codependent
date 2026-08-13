# Proposal to **agent-models** from **agent-retrieval**: `[retrieval] builtin_top_k`

I added one field to `RetrievalSettings` in `crates/runtime/src/models.rs`
(unowned, so I edited it directly — flagging it here because you own the
`models.toml` writer and the TUI pickers, and a second writer of this file is
exactly the failure mode the brief's rule 1 is about).

```toml
[retrieval]
mcp_top_k = 8       # existing
builtin_top_k = 8   # NEW — 0 disables built-in retrieval gating
```

* `crates/runtime/src/models.rs` — `DEFAULT_BUILTIN_TOP_K: usize = 8`, plus
  `RetrievalSettings::builtin_top_k` with `#[serde(default = "default_builtin_top_k")]`.
* `crates/codypendentd/src/executor.rs` — `.with_builtin_top_k(self.retrieval.builtin_top_k)`
  beside the existing `.with_mcp_top_k(...)`.

**Nothing to do unless you write `[retrieval]` from a picker.** The field is
additive and defaulted in both directions: an existing `models.toml` with no
`[retrieval]` table, or with only `mcp_top_k`, parses unchanged.

**What would break it:** serializing `RetrievalSettings` from a struct that
models only `mcp_top_k` and writing the whole table back — that would silently
reset an operator's `builtin_top_k` to the default. `RetrievalSettings` derives
`Serialize`, so this is reachable. If `models_file.rs` ever gains a `[retrieval]`
writer, edit the parsed document in place per brief rule 1 rather than
round-tripping the struct.

**What it does:** it is the operator escape hatch for the change in
`advertised_tool_definitions` — `builtin_top_k = 0` restores full tool injection
exactly, with no rebuild. There is a runtime unit test pinning that
(`a_zero_builtin_budget_disables_the_gate`), and I exercised it live: with the
gate off, two unrelated objectives get byte-identical 25-tool arrays; with it on
(default), they get 14 tools each and the arrays differ.

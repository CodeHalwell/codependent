# Adoption 10 — Post-Edit LSP Diagnostics in Tool Output

**Effort:** L · **Depends on:** nothing hard (schedule after Adoption 02, which touches the same `workspace.edit_file` observation path) · **Reference:** `reference-repos/opencode/packages/opencode/src/lsp/lsp.ts`, `lsp/client.ts`, `lsp/server.ts`, `lsp/diagnostic.ts`, `tool/edit.ts` (lines 196–211)
**Ported from:** opencode · **Status:** ⬜ not started

This is the execution of the open Phase-4 roadmap follow-up — "Live language-server spawn (rust-analyzer/pyright) | edges synthesized" (`docs/reviews/2026-08-11-verticals/10-docs-vision-gap.md` line 37; `docs/reviews/2026-07-20-codebase-review.md` line 440) — scoped to its highest-leverage consumer: compiler feedback inside the same model turn. Semantic edge-proving in the code graph stays a follow-up (§10).

## 1. Summary

After every successful edit, opencode touches the file in a live language server, blocks (bounded) until that server publishes fresh diagnostics for exactly that file version, and appends any errors to the tool's own output: `"LSP errors detected in this file, please fix:"` followed by a compact `<diagnostics>` block. The model sees the compiler's verdict in the same turn as its edit — no extra tool call, no waiting for a test run — which is one of the biggest single levers on agent edit quality. codypendent has the pieces but not the wire: `LanguageAdapter` (`crates/knowledge/src/adapter.rs`) already *probes* for rust-analyzer/pyright to report a `SemanticCapability`, but never spawns them, and `ScriptAdapter::diagnostics` returns an empty vec with the comment "No LSP wired yet". This adoption builds a real LSP client + lazy server manager in `crates/knowledge` (rust-analyzer and pyright first), gives `ScriptAdapter::python` genuine diagnostics, and wires a bounded post-write diagnostics hook into the `workspace.edit_file` / `workspace.write_file` execution arms so their observations carry fresh compiler errors.

## 2. Reference implementation

All paths under `reference-repos/opencode/packages/opencode/src/`.

**The consumer — `tool/edit.ts` lines 196–211.** After the write (and formatting):

```ts
let output = "Edit applied successfully."
yield* lsp.touchFile(filePath, "document")
const diagnostics = yield* lsp.diagnostics()
const block = LSP.Diagnostic.report(filePath, diagnostics[normalizedFilePath] ?? [])
if (block) output += `\n\nLSP errors detected in this file, please fix:\n${block}`
```

**Formatting — `lsp/diagnostic.ts`.** `report(file, issues)` keeps only severity-1 (errors), caps at `MAX_PER_FILE = 20` with a `"... and N more"` suffix, and renders:

```
<diagnostics file="{file}">
ERROR [line:col] {message}
</diagnostics>
```

(1-based line/col from the 0-based LSP range start; empty string when there are no errors — so clean edits carry no block at all.)

**Orchestration — `lsp/lsp.ts`.** `touchFile(input, diagnostics?)` (line 344) resolves the lazy clients for the file, calls `client.notify.open({path})` (didOpen or didChange, returning the new document *version*), and when a diagnostics mode is requested, awaits `client.waitForDiagnostics({path, version, mode, after})`. `getClients` (line 208) is the lazy spawner: per registered server, filter by file extension, resolve the server's **root** for that file, skip roots in a `broken` set, reuse an existing client keyed `(root, serverID)`, dedupe concurrent spawns via a `spawning` map, and mark `(root+serverID)` broken on any spawn/initialize failure so a bad server is attempted once, not per edit. `diagnostics()` merges every client's per-file map. All clients are shut down by a scope finalizer.

**Client — `lsp/client.ts`.** Constants (lines 13–18): `DIAGNOSTICS_DEBOUNCE_MS = 150`, `DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT_MS = 5_000`, `DIAGNOSTICS_FULL_WAIT_TIMEOUT_MS = 10_000`, `DIAGNOSTICS_REQUEST_TIMEOUT_MS = 3_000`, `INITIALIZE_TIMEOUT_MS = 45_000`. Mechanism:

- JSON-RPC over the child's stdio; `initialize` (with `rootUri`, `workspaceFolders`, publish/pull diagnostic capabilities) under the 45 s timeout, then `initialized`, then `workspace/didChangeConfiguration` when the server has initialization options. stderr is drained (`resume()`), never read.
- `textDocument/publishDiagnostics` notifications populate a `pushDiagnostics: Map<path, Diagnostic[]>` plus a `published: Map<path, {at, version}>` freshness record.
- `notify.open(path)` (line 554): first touch sends `didOpen` (version 0) after clearing stale caches; subsequent touches send `workspace/didChangeWatchedFiles` + `didChange` with version+1, honoring the server's sync kind (incremental servers get one whole-document-range change). Comment worth porting: **do not wipe diagnostics on didChange** — some servers only re-emit when content actually changes, so clearing would lose errors for no-op touches.
- `waitForDiagnostics({path, version, mode, after})` (line 630): `waitForFreshPush` resolves when a publish for that path arrives whose version matches (or whose timestamp is ≥ `after`), then debounces 150 ms so rapid successive publishes settle; overall bounded by the 5 s (document) / 10 s (full) timeout. Pull-capable servers are additionally polled via `textDocument/diagnostic` / `workspace/diagnostic` (3 s per request) — a parallel path this port does not take (§4, deviation 2).

**Server specs — `lsp/server.ts`.**

- `RustAnalyzer` (line 890): id `"rust"`, extensions `[".rs"]`; **root** = nearest directory with `Cargo.toml`/`Cargo.lock`, then walk parents (stopping at the worktree) looking for a `Cargo.toml` containing `[workspace]` — the workspace root wins over the member crate. Spawn: `rust-analyzer` from `PATH` (no args), cwd = root; absent binary → no server.
- `Pyright` (line 485): id `"pyright"`, extensions `[".py", ".pyi"]`; root = nearest of `pyproject.toml, setup.py, setup.cfg, requirements.txt, Pipfile, pyrightconfig.json`. Spawn: `pyright-langserver --stdio` from `PATH` (opencode falls back to an npm auto-install — not ported); **initialization options** carry `pythonPath` resolved from `$VIRTUAL_ENV`, `<root>/.venv`, or `<root>/venv` (`bin/python`, `Scripts/python.exe` on Windows) so the server sees the project's environment, not the system interpreter.

## 3. Current state in codypendent (verified)

**`crates/knowledge/src/adapter.rs`** (501 lines):

- `LanguageAdapter` trait: `language() / capability() / parse() / symbols() / diagnostics() / build_metadata()`; `SemanticCapability::{SyntaxOnly, LspResolved}`; `Diagnostic { path, line, severity, message }` + `DiagnosticSeverity::{Error, Warning, Info, Hint}`; `on_path(bin)` probe.
- `RustAdapter::capability()` probes `on_path("rust-analyzer")`; its `diagnostics()` already runs real `cargo check --message-format=json` (line 266) — genuine, keep.
- `ScriptAdapter` (python → `"pyright"`, typescript → `"typescript-language-server"`): `capability()` probes the server binary, but `diagnostics()` returns `Ok(Vec::new())` with the comment "No LSP wired yet — graceful degradation to no diagnostics" (line 493). This is the "reports the live-LSP capability today" gap.
- Phase 4 build chapter (`docs/docs/build/14-…` STEP list) specifies rust-analyzer via LSP as a child process and pyright/typescript-language-server "when found on PATH; graceful degradation to syntax-only" — this adoption is that step's live half.

**`crates/runtime/`:**

- Already depends on `codypendent-knowledge` (Cargo.toml, with an explicit no-cycle note) — the manager can live in knowledge and be consumed by runtime with no dependency change.
- `FrameworkAgentRuntime` (`src/agent.rs` line 1538) is a struct of optional `Arc<dyn Trait>` seams (`code_graph`, `registry`, `docs`, `artifacts`, …) each with a `with_*` builder; unwired seams change nothing. The LSP hook follows this exact pattern.
- Tool execution arms (`execute_prepared`): `PreparedTool::WriteFile` at ~line 4123 and `PreparedTool::EditFile` at ~line 4135 produce `(outcome.observation(), None, ToolOutcome::Succeeded)` on success — `outcome.path` is the **resolved** path that was actually written (`WriteFileOutcome::path` / `EditFileOutcome::path`, per their doc comments). The observation string is what enters the model's transcript; `ToolCompleted` events carry only outcome + artifact, so appending to the observation needs **no protocol change**.
- Runs execute in disposable worktrees: `RunContext.worktree` (line 1125); relative tool paths are rooted there (`parse_edit_file`, line 5994).

**Assembly:** `crates/codypendentd/src/lib.rs` wires the optional seams (`executor.with_github(…)` line 193, `with_search` line 220, `with_mcp` line 254) — the `with_lsp` call lands there. Runtime config extras already have a home: `ModelExtras` in `crates/runtime/src/models.rs` (line 277) parses optional `[embedding]`/`[retrieval]` tables from `models.toml`; an `[lsp]` table follows the same pattern.

**Dependencies:** workspace `tokio` already carries the `process`, `io-util`, `sync`, `time` features (root `Cargo.toml` line 111); `codypendent-knowledge` already uses `tokio::process` (adapter.rs line 272). **No LSP/JSON-RPC crate exists in the workspace** — the client is hand-rolled (§4).

## 4. Design

```
crates/knowledge/src/lsp/
    transport.rs   Content-Length JSON-RPC framing over AsyncRead/AsyncWrite
    client.rs      one spawned server: initialize handshake, didOpen/didChange,
                   publishDiagnostics cache, wait_for_diagnostics (debounce+timeout)
    servers.rs     ServerSpec roster: rust-analyzer, pyright (root detection, spawn)
    mod.rs         LspManager (lazy clients, broken set), LiveDiagnostics trait,
                   report() formatting

crates/knowledge/src/adapter.rs
    ScriptAdapter::python gains real diagnostics through an attached manager

crates/runtime/src/agent.rs
    FrameworkAgentRuntime.lsp: Option<Arc<dyn LiveDiagnostics>> (+ with_lsp)
    WriteFile/EditFile success arms: append the diagnostics block, bounded

crates/codypendentd/src/lib.rs
    build one process-wide LspManager when [lsp] enabled (default true)
```

Key decisions:

1. **Hand-rolled minimal client, no new crates.** The protocol surface actually needed is five message shapes (`initialize`/`initialized`, `textDocument/didOpen`, `textDocument/didChange`, `textDocument/publishDiagnostics`, `shutdown`/`exit`) plus generic request/response framing. Typed serde structs for exactly these, like every other wire shape in the codebase, beat pulling `lsp-types`/`tower-lsp` into the tree.
2. **Push diagnostics only.** rust-analyzer and pyright both publish `textDocument/publishDiagnostics`; opencode's parallel pull machinery (`textDocument/diagnostic`, dynamic registrations, workspace pulls) exists for servers this adoption doesn't ship. `wait_for_diagnostics` ports the *push* wait exactly: freshness keyed to `(path, version, after)`, 150 ms settle debounce, 5 s document timeout. Pull support is out of scope (§10) and the API shape leaves room for it.
3. **Best-effort, hard-bounded, never load-bearing.** The post-write hook is wrapped in an overall `tokio::time::timeout`; on timeout, spawn failure, or absent server the observation is exactly what it is today. A diagnostics failure can never fail a tool call that already wrote successfully — the write happened; the observation must say so (honesty rule).
4. **Transport is generic over `AsyncRead + AsyncWrite`** so the client is tested against `tokio::io::duplex` with a scripted fake server — no real language server in unit tests; real-server integration tests self-skip when the binary is absent (the existing "adapter degradation without pyright" idiom from Phase 4).
5. **No auto-download.** opencode npm-installs pyright when missing; codypendent degrades to no diagnostics (rule 10 territory: quietly installing executables is a capability grant nobody approved).
6. **One process-wide manager** (like `github`/`mcp`), keyed by `(server_id, root)`: runs in different worktrees get different roots and therefore different server instances; within one run every edit reuses the warm client.

## 5. Changes, file by file

### 5.1 `crates/knowledge/src/lsp/transport.rs` (new)

```rust
//! LSP base-protocol framing: `Content-Length: N\r\n\r\n{json}` over any
//! async byte stream. Generic so tests drive it over `tokio::io::duplex`.

use serde::{Deserialize, Serialize};

/// One incoming JSON-RPC message, minimally decoded: requests carry `id` +
/// `method`, notifications only `method`, responses only `id`.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

pub struct Transport<R, W> { /* reader: BufReader<R>, writer: W, next_id: i64 */ }

impl<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin> Transport<R, W> {
    pub fn new(reader: R, writer: W) -> Self;
    /// Send a request; returns the id used.
    pub async fn request(&mut self, method: &str, params: serde_json::Value)
        -> std::io::Result<i64>;
    pub async fn notify(&mut self, method: &str, params: serde_json::Value)
        -> std::io::Result<()>;
    pub async fn respond(&mut self, id: serde_json::Value, result: serde_json::Value)
        -> std::io::Result<()>;
    /// Read one framed message. Header parsing tolerates the optional
    /// `Content-Type` header and unknown headers; a malformed frame is an error.
    pub async fn read(&mut self) -> std::io::Result<Incoming>;
}
```

### 5.2 `crates/knowledge/src/lsp/client.rs` (new)

```rust
//! One live language-server client. Ported from opencode `lsp/client.ts`,
//! push-diagnostics path only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adapter::DiagnosticSeverity;

/// Reference constants (client.ts lines 13–18).
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(45);
pub const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);
pub const DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A published diagnostic for one file, at LSP fidelity (line AND column,
/// both 0-based as received; `report` renders them 1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub line: u32,
    pub character: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

pub struct LspClient { /* transport task handle, state: Arc<Mutex<ClientState>>,
                          child: tokio::process::Child, root: PathBuf, … */ }

struct ClientState {
    /// path -> latest published diagnostics.
    push: HashMap<PathBuf, Vec<LspDiagnostic>>,
    /// path -> (publish instant, version if the server echoed one).
    published: HashMap<PathBuf, (tokio::time::Instant, Option<i64>)>,
    /// path -> open-document version counter.
    files: HashMap<PathBuf, i64>,
    /// Wakes `wait_for_diagnostics` on every publish (tokio::sync::Notify).
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl LspClient {
    /// Spawn `command` with `cwd = root`, run the initialize handshake
    /// (rootUri + workspaceFolders + publishDiagnostics capability +
    /// `initialization` options) under [`INITIALIZE_TIMEOUT`], send
    /// `initialized`, then start the reader task that folds
    /// `textDocument/publishDiagnostics` into the state (answering
    /// `workspace/configuration` with the initialization options and any
    /// other server request with `null`). stderr is drained to /dev/null.
    pub async fn spawn(
        command: &Path,
        args: &[String],
        root: &Path,
        initialization: serde_json::Value,
    ) -> anyhow::Result<Self>;

    /// didOpen on first touch (version 0), didChange after (version+1) with
    /// one whole-document content change. Reads the file itself. NEVER
    /// clears cached diagnostics on didChange (client.ts line 564 comment:
    /// servers may not re-emit for unchanged content). Returns the version.
    pub async fn touch(&self, path: &Path) -> anyhow::Result<i64>;

    /// Block until a publish for `path` arrives that is fresh — version
    /// matches `version`, or it landed at/after `after` — then debounce
    /// [`DIAGNOSTICS_DEBOUNCE`] so successive publishes settle; give up at
    /// [`DIAGNOSTICS_DOCUMENT_WAIT_TIMEOUT`]. Never errors: timing out just
    /// means "whatever is cached is what you get".
    pub async fn wait_for_diagnostics(
        &self, path: &Path, version: i64, after: tokio::time::Instant,
    );

    /// The latest published diagnostics for `path` (empty when none).
    pub async fn diagnostics_for(&self, path: &Path) -> Vec<LspDiagnostic>;

    /// `shutdown` request + `exit` notification, then kill on timeout.
    pub async fn shutdown(&self);
}
```

Severity mapping: LSP 1→`Error`, 2→`Warning`, 3→`Info`, 4 or absent→`Hint`; a publish with an absent severity defaults to `Error` (reference `severityMap[diagnostic.severity || 1]`).

### 5.3 `crates/knowledge/src/lsp/servers.rs` (new)

```rust
//! The server roster: which binary serves which extensions, how its
//! workspace root is found, and how it is spawned. rust-analyzer and
//! pyright first (this adoption); the roster is data, additions are rows.

use std::path::{Path, PathBuf};

pub struct ServerSpec {
    /// Stable id, also the broken-set / client-map key half.
    pub id: &'static str,
    /// File extensions (with dot) this server owns.
    pub extensions: &'static [&'static str],
    /// The binary probed on PATH (`crate::adapter::on_path` compatible).
    pub binary: &'static str,
}

pub const RUST_ANALYZER: ServerSpec = ServerSpec {
    id: "rust-analyzer",
    extensions: &[".rs"],
    binary: "rust-analyzer",
};

pub const PYRIGHT: ServerSpec = ServerSpec {
    id: "pyright",
    extensions: &[".py", ".pyi"],
    binary: "pyright-langserver",
};

pub const ROSTER: &[&ServerSpec] = &[&RUST_ANALYZER, &PYRIGHT];

/// rust-analyzer root: nearest ancestor of `file` (bounded by `worktree`)
/// holding Cargo.toml/Cargo.lock, then keep walking up (still bounded) and
/// return the first ancestor whose Cargo.toml contains `[workspace]`, else
/// the crate root (server.ts line 892 semantics).
pub fn rust_analyzer_root(file: &Path, worktree: &Path) -> Option<PathBuf>;

/// pyright root: nearest ancestor holding any of pyproject.toml, setup.py,
/// setup.cfg, requirements.txt, Pipfile, pyrightconfig.json; else `worktree`.
pub fn pyright_root(file: &Path, worktree: &Path) -> Option<PathBuf>;

/// pyright initialization options: `{"pythonPath": …}` resolved from
/// $VIRTUAL_ENV, `<root>/.venv`, `<root>/venv` (`bin/python`, or
/// `Scripts/python.exe` on Windows) — first that exists wins; `{}` when
/// none (server.ts line 500). rust-analyzer takes `{}`.
pub fn pyright_initialization(root: &Path) -> serde_json::Value;

/// pyright is spawned with `--stdio`; rust-analyzer with no args.
pub fn spawn_args(spec: &ServerSpec) -> &'static [&'static str];
```

### 5.4 `crates/knowledge/src/lsp/mod.rs` (new)

```rust
//! Live LSP diagnostics (Phase 4 follow-up: the "live language-server
//! spawn" half of Chapter 07's semantic tier). One process-wide manager
//! lazily spawns servers per (server, workspace-root), caches clients,
//! marks broken pairs so a failing server is tried once, and exposes the
//! one question the write tools ask: "fresh diagnostics for this file".

pub mod client;
pub mod servers;
mod transport;

pub use client::{LspClient, LspDiagnostic};

use std::path::Path;
use async_trait::async_trait;

/// The seam the runtime's write tools consume (mirrors `CodeGraphQueries`:
/// a knowledge-owned trait held as `Option<Arc<dyn …>>` on the runtime).
#[async_trait]
pub trait LiveDiagnostics: Send + Sync {
    /// Touch `file` in every live server responsible for it, wait (bounded)
    /// for fresh diagnostics, and return them. Empty when no server covers
    /// the file, none is installed, spawn failed, or the wait timed out —
    /// this method NEVER errors and NEVER blocks beyond its internal bounds.
    async fn file_diagnostics(&self, file: &Path, worktree: &Path) -> Vec<LspDiagnostic>;
}

pub struct LspManager { /* clients: Mutex<HashMap<(String, PathBuf), Arc<LspClient>>>,
                           broken: Mutex<HashSet<(String, PathBuf)>>,
                           spawning dedup, … */ }

impl LspManager {
    pub fn new() -> Self;
    /// Lazy client for (spec, root): reuse, or spawn+initialize; on failure
    /// mark broken and answer None forever after (per manager lifetime).
    async fn client_for(&self, spec: &servers::ServerSpec, root: &Path)
        -> Option<std::sync::Arc<LspClient>>;
}

#[async_trait]
impl LiveDiagnostics for LspManager {
    async fn file_diagnostics(&self, file: &Path, worktree: &Path) -> Vec<LspDiagnostic> {
        // extension → specs (servers::ROSTER) → root fn → client_for →
        // touch → wait_for_diagnostics(version, after=now) →
        // diagnostics_for(file); merged across servers.
    }
}

/// Reference `diagnostic.ts` `report`: severity Error only, cap 20 with
/// `... and N more`, 1-based positions. `None` when there are no errors.
pub const MAX_DIAGNOSTICS_PER_FILE: usize = 20;
pub fn report(file: &Path, issues: &[LspDiagnostic]) -> Option<String> {
    // <diagnostics file="{file}">
    // ERROR [{line+1}:{character+1}] {message}
    // ... and N more            (only when errors > 20)
    // </diagnostics>
}
```

### 5.5 `crates/knowledge/src/lib.rs`

Add `pub mod lsp;` and re-export the seam: `pub use lsp::{LiveDiagnostics, LspDiagnostic, LspManager};` (matching how `CodeGraphQueries` is surfaced).

### 5.6 `crates/knowledge/src/adapter.rs`

`ScriptAdapter` gains an optional live manager; Python diagnostics become real:

```rust
pub struct ScriptAdapter {
    language: LanguageId,
    languages: Vec<codegraph::Language>,
    language_server: String,
    /// Live LSP, when the process wired one (None keeps today's behavior).
    live: Option<std::sync::Arc<crate::lsp::LspManager>>,
}

impl ScriptAdapter {
    /// Attach the live manager. Additive: without it, `diagnostics` stays
    /// the graceful empty vec.
    #[must_use]
    pub fn with_live_lsp(mut self, live: std::sync::Arc<crate::lsp::LspManager>) -> Self;
}
```

`diagnostics(&self, workspace)` for the Python adapter: when `live` is set **and** `on_path(&self.language_server)`, collect the workspace's sources (existing `collect_sources`, capped at the first `500` files to bound a huge repo), touch each through the manager, wait once on the **last** touched file (a settle point, not per-file waits), then snapshot every touched file's diagnostics into the adapter's `Diagnostic` shape (`path` repo-relative via the existing `rel_path`, `line` = LSP line + 1). Otherwise the current empty-vec degradation stands, comment updated from "No LSP wired yet" to name the gate. `RustAdapter::diagnostics` is **unchanged** (cargo check is already real and workspace-scoped); TypeScript stays degraded (§10).

### 5.7 `crates/runtime/src/agent.rs`

1. New seam + builder on `FrameworkAgentRuntime` (with the other optional seams, line 1538 struct / builder cluster):

```rust
/// Live LSP diagnostics appended to write-tool observations (Adoption 10),
/// if wired. Process-wide like `github`/`mcp`; `None` leaves every
/// observation exactly as before.
lsp: Option<Arc<dyn codypendent_knowledge::LiveDiagnostics>>,
```

```rust
#[must_use]
pub fn with_lsp(mut self, lsp: Arc<dyn codypendent_knowledge::LiveDiagnostics>) -> Self {
    self.lsp = Some(lsp);
    self
}
```

2. A bounded helper + the reference's exact appendix line:

```rust
/// Overall budget for the post-write diagnostics wait. Slightly above the
/// client's 5 s document wait so the touch itself fits inside it.
const POST_WRITE_DIAGNOSTICS_BUDGET: Duration = Duration::from_secs(6);

/// Fresh diagnostics block for `path`, or None (no seam, no server, no
/// errors, or budget exhausted). Infallible by design: a write that
/// succeeded must report success whatever the language server does.
async fn post_write_diagnostics(&self, path: &Path, worktree: &Path) -> Option<String> {
    let lsp = self.lsp.as_ref()?;
    let issues = tokio::time::timeout(
        POST_WRITE_DIAGNOSTICS_BUDGET,
        lsp.file_diagnostics(path, worktree),
    )
    .await
    .ok()?;
    codypendent_knowledge::lsp::report(path, &issues)
}
```

3. The two success arms in `execute_prepared` (~lines 4123 and 4135) append it:

```rust
PreparedTool::WriteFile(input) => match WriteFile::execute(&input, &write_scope).await {
    Ok(outcome) => {
        let mut observation = outcome.observation();
        if let Some(block) = self.post_write_diagnostics(&outcome.path, &run.worktree).await {
            observation.push_str("\n\nLSP errors detected in this file, please fix:\n");
            observation.push_str(&block);
        }
        (observation, None, ToolOutcome::Succeeded)
    }
    Err(e) => /* unchanged */,
},
```

(identically for `PreparedTool::EditFile`). `ToolOutcome` stays `Succeeded` — the write succeeded; the diagnostics are information for the model, not a failure verdict.

### 5.8 `crates/runtime/src/models.rs`

`ModelExtras` (line 277) gains the gate, following `RetrievalSettings` exactly:

```rust
/// The `[lsp]` table in `models.toml`.
///
/// ```toml
/// [lsp]
/// enabled = true   # default; false disables live diagnostics entirely
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspSettings {
    #[serde(default = "default_lsp_enabled")]
    pub enabled: bool,
}
fn default_lsp_enabled() -> bool { true }
impl Default for LspSettings { fn default() -> Self { Self { enabled: true } } }
```

with `#[serde(default)] pub lsp: LspSettings` on `ModelExtras` (absent file/table ⇒ enabled, back-compatible in both directions like the existing extras).

### 5.9 `crates/codypendentd/src/lib.rs`

Where the other optional seams are wired (`with_github` ~line 193, `with_search` ~line 220, `with_mcp` ~line 254):

```rust
if extras.lsp.enabled {
    executor = executor.with_lsp(std::sync::Arc::new(
        codypendent_knowledge::LspManager::new(),
    ));
}
```

(one manager for the daemon's lifetime; clients shut down when the daemon does).

### 5.10 Dependencies

None new. `codypendent-knowledge` already has `tokio` (workspace features include `process`, `io-util`, `sync`, `time`), `serde_json`, `async-trait`, `anyhow`. `codypendent-runtime` already depends on `codypendent-knowledge`.

## 6. Protocol & persistence

None. Diagnostics ride inside the tool observation string, which already flows through the transcript and existing `ToolCompleted` events; clients need no changes to display them. No new commands, no event variants, no ledger kinds, no SQLite migrations. (An `LspStatus` footer event like opencode's is deliberately out of scope, §10.)

## 7. Acceptance criteria

1. `codypendent_knowledge::lsp::report` renders exactly the reference shape: errors only; 1-based `[line:col]`; `<diagnostics file="…">…</diagnostics>`; 21+ errors → 20 lines + `... and N more`; zero errors → `None`.
   RUN: `cargo test -p codypendent-knowledge lsp` EXPECT: pass.
2. The transport round-trips framed messages (including a `Content-Type` header and an unknown header) and errors on a malformed frame, proven over `tokio::io::duplex`.
3. Against a scripted fake server: `spawn` completes the initialize handshake and answers `workspace/configuration`; `touch` sends `didOpen` at version 0 then `didChange` at version 1; a publish for the touched version releases `wait_for_diagnostics` after the 150 ms debounce; no publish releases it at the 5 s timeout (paused clock); a publish for a *stale* version does not release it early; cached diagnostics survive a `didChange` with no fresh publish.
4. `rust_analyzer_root` returns the `[workspace]`-bearing ancestor for a member-crate file, the crate root when no workspace exists, and `None` outside any Cargo project — never a directory above `worktree`. `pyright_root` honors all six markers. `pyright_initialization` finds `<root>/.venv/bin/python`.
5. With rust-analyzer installed (test self-skips otherwise): editing a fixture crate's file to introduce a type error makes `LspManager::file_diagnostics` return at least one `Error` for that file within the budget; fixing it and touching again returns none.
   RUN: `cargo test -p codypendent-knowledge --test lsp_it -- --nocapture` EXPECT: pass (or skip lines naming the missing binary).
6. With a stub `LiveDiagnostics` wired into `FrameworkAgentRuntime`, a successful `workspace.edit_file` observation is `"applied 1 edit(s) to <path>\n\nLSP errors detected in this file, please fix:\n<diagnostics …>"` and the outcome is still `Succeeded`; with the seam `None`, byte-identical to today; with a stub that hangs, the observation appears un-suffixed within the 6 s budget (paused clock).
   RUN: `cargo test -p codypendent-runtime lsp` EXPECT: pass.
7. `ScriptAdapter::python().diagnostics(ws)` still returns `Ok(vec![])` without pyright on PATH or without an attached manager (the Phase 4 degradation tests keep passing unchanged).
8. `[lsp] enabled = false` in `models.toml` results in no `with_lsp` wiring (assembly test or manual: daemon log shows no LSP spawn; edits carry no diagnostics block).
9. `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` EXPECT: green — with **no** language server installed (CI has none; every live test must self-skip).

## 8. Tests

**`crates/knowledge/src/lsp/transport.rs`** (inline): `frames_round_trip_over_duplex`, `tolerates_content_type_header`, `malformed_frame_is_an_error`, `request_ids_are_monotonic`.

**`crates/knowledge/src/lsp/client.rs`** (inline, fake server over duplex; `#[tokio::test(start_paused = true)]` for the waits): `initialize_handshake_sends_root_and_capabilities`, `touch_sends_didopen_then_didchange_with_versions`, `publish_updates_cache_and_wakes_waiter_after_debounce`, `stale_version_publish_does_not_release_the_wait`, `wait_times_out_at_five_seconds`, `didchange_does_not_clear_cached_diagnostics`, `severity_mapping_defaults_to_error`.

**`crates/knowledge/src/lsp/servers.rs`** (inline, `tempfile` fixtures): `rust_analyzer_root_prefers_workspace_manifest`, `rust_analyzer_root_stops_at_worktree`, `pyright_root_matches_all_markers`, `pyright_initialization_resolves_venv_python`.

**`crates/knowledge/src/lsp/mod.rs`** (inline): `report_matches_reference_format`, `report_caps_at_twenty_with_more_suffix`, `report_is_none_without_errors`, `broken_server_is_not_respawned` (a spec whose binary path is a failing stub: second call performs no spawn — observable via a counting spawn hook or a nonexistent-binary spec plus timing).

**`crates/knowledge/tests/lsp_it.rs`** (new, live integration; each test begins `if !on_path("rust-analyzer") { eprintln!("skipping: rust-analyzer not installed"); return; }` — the Phase 4 self-skip idiom): `rust_analyzer_reports_type_error_after_edit`, `clean_file_reports_no_errors`; mirrored pyright pair gated on `pyright-langserver`.

**`crates/knowledge/src/adapter.rs`**: existing degradation tests unchanged; add `python_adapter_without_live_manager_stays_empty`.

**`crates/runtime/src/agent.rs`** (the seam tests, alongside the other `with_*` stubs): `write_observation_carries_diagnostics_block_from_stub`, `edit_observation_carries_diagnostics_block_from_stub`, `unwired_lsp_seam_leaves_observations_untouched`, `hung_diagnostics_provider_is_cut_at_the_budget` (paused clock), `diagnostics_never_flip_a_succeeded_outcome`.

## 9. Gotchas

1. **rust-analyzer cold start.** First index of a real workspace takes tens of seconds to minutes; the reference's 45 s *initialize* timeout exists for this, and even after initialize the first publishes lag. Consequence: the first edit of a run may honestly report no diagnostics (the 5 s wait expires). Do not raise the per-edit budget to compensate — subsequent edits in the run hit a warm server. Consider (non-blocking) spawning the manager's touch for a run's first read; do NOT block run start on it.
2. **Disposable worktrees mean per-run cold servers.** Each run's worktree is a fresh root, so a daemon serving many runs spawns many rust-analyzer instances (each can use hundreds of MB). The `(server, root)` map plus the broken set bound this per root, but there is no global cap in this adoption — if memory pressure appears, an LRU shutdown of idle clients is the follow-up, not a reason to share one server across worktrees (rust-analyzer's view would be wrong for both).
3. **Never wipe diagnostics on `didChange`** (client.ts line 564 comment, learned from clangd): servers may not re-publish for unchanged content; clearing on touch loses real errors for no-op touches. The freshness check (`version` / `after`) is what prevents *stale* errors being reported as fresh.
4. **The version race is real.** Waiting for "any publish" returns the *previous* edit's diagnostics; the wait must key on the version returned by `touch` (or a publish timestamped after it). This is the entire point of the reference's `published` map.
5. **Drain stderr.** rust-analyzer logs progress to stderr; an unread pipe fills and deadlocks the child. Spawn with `Stdio::null()` for stderr (the reference `resume()`s it — same effect).
6. **Kill on shutdown, reap always.** Send `shutdown`/`exit` but follow with `start_kill` on timeout, and hold the `Child` so it is reaped — otherwise every daemon restart leaks language-server zombies. `kill_on_drop(true)` is the backstop.
7. **Path canonicalization.** publishDiagnostics URIs come back `file://`-encoded and (on macOS) may differ in `/private` prefixing from the path the tool wrote. Canonicalize both sides of every map key (`std::fs::canonicalize` on existing files) or the cache lookup silently misses and every edit reports "clean". The write tools hand over the **resolved** `outcome.path` — use it, not the model's raw argument.
8. **pyright without the venv is worse than useless** — it reports missing-import errors for every project dependency, which the model will then "fix". The `pythonPath` initialization option (§5.3) is load-bearing, not cosmetic; if no interpreter is found, still spawn (stdlib-only projects work) but expect noise on dependency imports.
9. **pyright answers `workspace/configuration`** requests; a client that never responds wedges some servers' diagnostic pipelines. Answer with the initialization options (reference behavior), and answer unknown server→client requests with `null` rather than leaving them pending.
10. **Errors only, capped at 20.** Reporting warnings turns every edit into a lint lecture and burns context; the reference filters severity 1 and caps output deliberately. Keep both.
11. **Budget the whole hook, not just the wait.** The touch itself does file I/O and JSON-RPC writes to a possibly-wedged process; that is why `post_write_diagnostics` wraps the *entire* `file_diagnostics` call in `tokio::time::timeout`, and why the trait method itself must not hold locks across `.await`s that the reader task needs.
12. **Never let diagnostics fail the tool.** The write already happened; a `Failed` outcome would be a lie about the filesystem (the honest-observation rule the tools' doc comments state). Every failure path in this feature degrades to "no block".

## 10. Out of scope

- TypeScript (`typescript-language-server`) — third server, same roster row shape, after rust-analyzer + pyright prove the plumbing.
- Pull diagnostics (`textDocument/diagnostic`, `workspace/diagnostic`, dynamic capability registration) and opencode's "full" wait mode.
- Proving code-graph edges at LSP confidence 0.90 (`textDocument/definition`/`references` walks superseding syntax edges — the other half of the Phase-4 roadmap line; it should reuse this adoption's `LspManager`).
- An `lsp` tool exposing definitions/references/hover to the model (opencode's nine-operation tool).
- Auto-downloading missing servers (opencode's npm fallback) — degradation, never installation.
- Formatter integration (opencode runs `format.file` before diagnostics).
- LSP status events / TUI footer indicators.
- Per-server configuration in `models.toml` beyond the single `enabled` gate.

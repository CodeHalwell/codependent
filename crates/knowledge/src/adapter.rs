//! Language adapters (Chapter 07, STEP 4.5).
//!
//! A [`LanguageAdapter`] presents a uniform surface over a language's tooling:
//! `parse` (syntax symbols), `symbols` (a workspace index), `diagnostics`, and
//! `build_metadata`. Each adapter reports its best available
//! [`SemanticCapability`]: if a language server (rust-analyzer, pyright,
//! typescript-language-server) is found on `PATH` it can resolve references at
//! LSP confidence; otherwise it **degrades gracefully to the syntax layer** at
//! the lower syntax confidence — never failing, just less precise.
//!
//! Rust is the first-class adapter (its syntax layer is the Phase 2 tree-sitter
//! graph, its `build_metadata` is `cargo metadata`, its `diagnostics` are
//! `cargo check --message-format=json`). Python and TypeScript are deliberately
//! thinner: a line-level syntax scan with optional LSP when present.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use crate::codegraph::{self, ParsedSymbol};
use crate::lsp::servers::Probe;
use crate::types::LanguageId;

/// A workspace an adapter operates over (its filesystem root).
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// A single file to parse.
#[derive(Debug, Clone)]
pub struct ParseInput {
    /// Repo-relative path (used to derive module qualification).
    pub path: String,
    pub source: String,
}

/// The result of a syntax parse: the durable symbols the file defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    pub language: LanguageId,
    pub symbols: Vec<ParsedSymbol>,
}

/// A workspace-wide symbol index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolIndex {
    pub language: LanguageId,
    /// `(repo-relative path, symbols in that file)`.
    pub files: Vec<(String, Vec<ParsedSymbol>)>,
}

impl SymbolIndex {
    /// Total symbol count across all files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.iter().map(|(_, s)| s.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A compiler/linter diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub line: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// Build/package metadata for a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildMetadata {
    pub packages: Vec<PackageInfo>,
    pub dependencies: Vec<String>,
}

/// One package in the build metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

/// The best semantic tier an adapter can produce in the current environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticCapability {
    /// No language server found — syntax layer only (lower confidence).
    SyntaxOnly,
    /// A language server is available; references can be resolved at LSP
    /// confidence.
    LspResolved,
}

/// Errors from an adapter.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("parse error: {0}")]
    Parse(#[from] codegraph::CodeGraphError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool `{tool}` failed: {reason}")]
    Tool { tool: String, reason: String },
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// A language's tooling surface (Chapter 07 `LanguageAdapter`).
#[async_trait]
pub trait LanguageAdapter: Send + Sync {
    /// The language this adapter serves.
    fn language(&self) -> LanguageId;

    /// The best semantic tier available now (LSP if its server is on `PATH`, else
    /// syntax-only).
    fn capability(&self) -> SemanticCapability;

    /// Parse one file into the symbols it defines (syntax layer).
    async fn parse(&self, input: ParseInput) -> Result<ParseOutput, AdapterError>;

    /// Index every source file in the workspace.
    async fn symbols(&self, workspace: &Workspace) -> Result<SymbolIndex, AdapterError>;

    /// Compiler/linter diagnostics for the workspace. Degrades to an empty list
    /// when no compiler is available rather than failing.
    async fn diagnostics(&self, workspace: &Workspace) -> Result<Vec<Diagnostic>, AdapterError>;

    /// Build/package metadata for the workspace.
    async fn build_metadata(&self, workspace: &Workspace) -> Result<BuildMetadata, AdapterError>;
}

/// Whether `bin` is an executable on `PATH` — the graceful-degradation probe.
///
/// The result is cached per binary for the life of the process: the probe runs
/// a blocking `<bin> --version`, and this is called from async diagnostics paths
/// on every request. Caching-once keeps that blocking spawn off the hot path so
/// it never re-stalls the async executor.
#[must_use]
pub fn on_path(bin: &str) -> bool {
    on_path_with(bin, Probe::VersionExitsZero, &[])
}

/// [`on_path`] for a roster entry, honoring the spec's own [`Probe`].
///
/// A language server is not a `--version` CLI. Asking every server the same
/// question classified pyright — which refuses every invocation that is not a
/// live LSP connection — as a dead shim, so Python resolution never left the
/// syntax tier on any machine. The roster now says how each server answers.
#[must_use]
pub fn server_on_path(spec: &crate::lsp::servers::ServerSpec) -> bool {
    on_path_with(
        spec.binary,
        spec.probe,
        crate::lsp::servers::spawn_args(spec),
    )
}

/// [`server_on_path`] for an async caller — see [`on_path_async`] for why the
/// first probe of a binary belongs on a blocking thread.
pub async fn server_on_path_async(spec: &'static crate::lsp::servers::ServerSpec) -> bool {
    tokio::task::spawn_blocking(move || server_on_path(spec))
        .await
        .unwrap_or(false)
}

/// The cache behind both entry points, keyed by *binary and probe*: the same
/// binary asked two different questions has two different answers, and one
/// must not be served from the other's cache slot.
fn on_path_with(bin: &str, probe: Probe, spawn_args: &[&str]) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<(String, Probe), bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (bin.to_owned(), probe);
    if let Some(&hit) = cache.lock().unwrap().get(&key) {
        return hit;
    }
    let result = probe_on_path(bin, probe, spawn_args);
    cache.lock().unwrap().insert(key, result);
    result
}

/// [`on_path`] for an async caller: the *first* probe of a binary spawns a
/// subprocess and waits for it, so it runs on a blocking thread instead of the
/// tokio worker that asked. Later calls are cache hits, but the first one is
/// the one that stalls a worker.
///
/// A probe task that panics or is cancelled answers `false` — the same
/// fail-closed answer as a binary that is not there, which degrades to
/// syntax-only capability / empty diagnostics rather than pretending a tool is
/// usable.
pub async fn on_path_async(bin: &str) -> bool {
    let bin = bin.to_owned();
    tokio::task::spawn_blocking(move || on_path(&bin))
        .await
        .unwrap_or(false)
}

/// How long the `--version` probe may run before the binary is treated as
/// unusable. Printing a version banner is milliseconds of work; the probe had
/// **no bound at all**, so a shim that blocks (waiting on a lock, a dead network
/// mount, an interpreter that reads stdin) hung the caller for good.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the bounded probe checks on the child.
const PROBE_POLL: Duration = Duration::from_millis(10);
/// How long a [`Probe::StaysAliveOnStdio`] server gets to fall over before it
/// is believed.
///
/// The two error directions are not symmetric, and this bound leans on that. A
/// grace that is too *short* calls a broken install healthy — the manager then
/// tries to spawn it, fails, marks the pair broken and degrades exactly as it
/// does today. A grace that is too *long* only costs a one-time wait. Neither
/// is the failure that matters: calling a *healthy* server absent kills the
/// feature silently, which is the bug this probe exists to end. So the bound is
/// generous enough for a node-backed server to boot and crash if it is going
/// to, and the probe is cached, so a language pays it once per process.
/// Raised from 500ms after `the_liveness_probe_still_rejects_a_dead_shim`
/// failed roughly one run in three under this crate's own suite at four
/// threads: a shim that exits immediately still has to be scheduled, exec,
/// write to stderr and exit, and on a machine already forking dozens of
/// processes that does not reliably fit in half a second. The probe then
/// reported a dead shim as a live server — the "too short" direction the
/// comment above already names as the recoverable one, but a wrong answer
/// nonetheless. Per that same reasoning, the extra time only costs a one-time
/// wait.
const PROBE_LIVENESS_GRACE: Duration = Duration::from_millis(2000);

/// The uncached PATH probe backing [`on_path`]: `bin` (or `bin.exe`) exists on
/// `PATH` and actually executes (`--version` succeeds within [`PROBE_TIMEOUT`]),
/// rejecting dead rustup shims and broken symlinks.
fn probe_on_path(bin: &str, probe: Probe, spawn_args: &[&str]) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exists = std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(bin);
        candidate.is_file() || candidate.with_extension("exe").is_file()
    });
    if !exists {
        return false;
    }
    // Verify it actually executes rather than being a dead rustup shim or broken
    // symlink — the way this particular program can be asked.
    match probe {
        Probe::VersionExitsZero => version_probe_succeeds(bin, PROBE_TIMEOUT),
        Probe::StaysAliveOnStdio => stays_alive_probe(bin, spawn_args, PROBE_LIVENESS_GRACE),
    }
}

/// Run `<program> --version` and report whether it exited zero *within*
/// `timeout`. A child still running at the deadline is killed and reported as a
/// failure — fail closed, the same answer as a missing binary.
///
/// Every stdio is `/dev/null`: nothing here reads the banner, and an unread pipe
/// is its own way to hang on a chatty child. (`timeout` is a parameter so the
/// bound itself is testable without waiting out the production one.)
fn version_probe_succeeds(program: &str, timeout: Duration) -> bool {
    let spawned = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return false;
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(PROBE_POLL);
    }
}

/// Spawn `program` the way the LSP manager will — real args, a real stdin —
/// and report whether it is still running after `grace`.
///
/// This is the liveness question for a server with no zero-exit invocation.
/// It is deliberately the *same* invocation the manager uses, so a pass here
/// means the spawn the manager is about to attempt will work, rather than
/// meaning some unrelated `--version` path happens to be wired up.
///
/// Two details carry the whole probe:
///
/// * **stdin is a pipe we hold open.** An LSP server handed `/dev/null` reads
///   EOF immediately and exits *cleanly* — indistinguishable from the crash
///   this is looking for, and it would fail every healthy server.
/// * **the child leads its own process group, swept on the way out.**
///   `pyright-langserver` is a wrapper script that spawns node, so killing the
///   leader alone would orphan the real server — one leaked node per probe.
fn stays_alive_probe(program: &str, args: &[&str], grace: Duration) -> bool {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let pid = child.id();

    let deadline = std::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            // Exited on its own inside the grace: a shim complaining, a missing
            // interpreter, a crash. `try_wait` reaped it, so there is no group
            // left to sweep and nothing to clean up.
            Ok(Some(_)) => return false,
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(PROBE_POLL);
    }

    // Still running, and never reaped — so `pid` still names the group and the
    // sweep cannot land on a recycled pid.
    #[cfg(unix)]
    codypendent_sandbox::executor::kill_process_group(pid);
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.kill();
    let _ = child.wait();
    true
}

/// Recursively collect files under `root` whose extension is in `exts`.
fn collect_sources(root: &Path, exts: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            // Use the entry's own file type, which does NOT follow the final
            // symlink — a circular directory symlink would otherwise recurse
            // forever and overflow the stack when scanning an untrusted workspace.
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                // Skip the usual noise directories.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if matches!(
                    name,
                    "target" | "node_modules" | ".git" | "dist" | "__pycache__"
                ) {
                    continue;
                }
                walk(&path, exts, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if exts.contains(&ext) {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, exts, &mut out);
    out
}

/// The repo-relative path of `file` under `root` (falling back to the file name).
fn rel_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned()
}

// --------------------------------------------------------------------------
// Rust adapter (first-class)
// --------------------------------------------------------------------------

/// The Rust adapter: tree-sitter syntax (Phase 2 graph), `cargo metadata`, and
/// `cargo check` diagnostics; rust-analyzer resolution when it is on `PATH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RustAdapter;

#[async_trait]
impl LanguageAdapter for RustAdapter {
    fn language(&self) -> LanguageId {
        LanguageId("rust".into())
    }

    fn capability(&self) -> SemanticCapability {
        if on_path("rust-analyzer") {
            SemanticCapability::LspResolved
        } else {
            SemanticCapability::SyntaxOnly
        }
    }

    async fn parse(&self, input: ParseInput) -> Result<ParseOutput, AdapterError> {
        let symbols = codegraph::parse_symbols(&input.path, &input.source)?;
        Ok(ParseOutput {
            language: self.language(),
            symbols,
        })
    }

    async fn symbols(&self, workspace: &Workspace) -> Result<SymbolIndex, AdapterError> {
        let root = workspace.root.clone();
        let files = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            // From the roster, not a local literal: the extensions a language
            // owns are `codegraph::Language`'s to say, here as everywhere.
            for file in collect_sources(&root, codegraph::Language::Rust.extensions()) {
                let Ok(source) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let rel = rel_path(&root, &file);
                if let Ok(symbols) = codegraph::parse_symbols(&rel, &source) {
                    out.push((rel, symbols));
                }
            }
            out
        })
        .await
        .map_err(|e| AdapterError::Tool {
            tool: "spawn_blocking".into(),
            reason: e.to_string(),
        })?;
        Ok(SymbolIndex {
            language: self.language(),
            files,
        })
    }

    async fn diagnostics(&self, workspace: &Workspace) -> Result<Vec<Diagnostic>, AdapterError> {
        // Compiler diagnostics via `cargo check --message-format=json`. If cargo
        // is unavailable, degrade to an empty list rather than failing.
        if !on_path_async("cargo").await {
            return Ok(Vec::new());
        }
        let output = tokio::process::Command::new("cargo")
            .args(["check", "--message-format=json", "--quiet"])
            .current_dir(&workspace.root)
            .output()
            .await?;
        Ok(parse_cargo_diagnostics(&output.stdout))
    }

    async fn build_metadata(&self, workspace: &Workspace) -> Result<BuildMetadata, AdapterError> {
        if !on_path_async("cargo").await {
            return Ok(BuildMetadata::default());
        }
        let output = tokio::process::Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(&workspace.root)
            .output()
            .await?;
        if !output.status.success() {
            return Err(AdapterError::Tool {
                tool: "cargo metadata".into(),
                reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        parse_cargo_metadata(&output.stdout)
    }
}

/// Parse `cargo metadata --no-deps` JSON into [`BuildMetadata`].
fn parse_cargo_metadata(stdout: &[u8]) -> Result<BuildMetadata, AdapterError> {
    let value: serde_json::Value = serde_json::from_slice(stdout)?;
    let mut packages = Vec::new();
    let mut dependencies = Vec::new();
    if let Some(pkgs) = value.get("packages").and_then(|p| p.as_array()) {
        for pkg in pkgs {
            let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("");
            packages.push(PackageInfo {
                name: name.to_string(),
                version: version.to_string(),
            });
            if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_array()) {
                for dep in deps {
                    if let Some(dn) = dep.get("name").and_then(|n| n.as_str()) {
                        dependencies.push(dn.to_string());
                    }
                }
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(BuildMetadata {
        packages,
        dependencies,
    })
}

/// Parse `cargo check --message-format=json` output into diagnostics.
fn parse_cargo_diagnostics(stdout: &[u8]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for line in stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let severity = match message.get("level").and_then(|l| l.as_str()) {
            Some("error") => DiagnosticSeverity::Error,
            Some("warning") => DiagnosticSeverity::Warning,
            Some("note") => DiagnosticSeverity::Info,
            _ => DiagnosticSeverity::Hint,
        };
        let text = message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let (path, line) = message
            .get("spans")
            .and_then(|s| s.as_array())
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|s| s.get("is_primary") == Some(&serde_json::Value::Bool(true)))
            })
            .map(|span| {
                (
                    span.get("file_name")
                        .and_then(|f| f.as_str())
                        .unwrap_or("")
                        .to_string(),
                    span.get("line_start")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32,
                )
            })
            .unwrap_or_default();
        diagnostics.push(Diagnostic {
            path,
            line,
            severity,
            message: text,
        });
    }
    diagnostics
}

// --------------------------------------------------------------------------
// Python and TypeScript adapters (thinner, syntax-first)
// --------------------------------------------------------------------------

/// A thin, syntax-first adapter for a scripting language. It scans top-level
/// declarations line-by-line and reports [`SemanticCapability::SyntaxOnly`]
/// unless its language server is on `PATH`.
#[derive(Debug, Clone)]
pub struct ScriptAdapter {
    language: LanguageId,
    /// Which code-graph languages this adapter covers. The extensions come from
    /// [`codegraph::Language::extensions`] — the adapter never keeps its own
    /// list, because a list that disagrees with the parser's is how a file gets
    /// offered to a grammar that cannot read it.
    languages: Vec<codegraph::Language>,
    /// The roster entry whose server serves this language — the spec itself,
    /// not a copy of its binary name, so the probe strategy travels with it.
    server: &'static crate::lsp::servers::ServerSpec,
    /// Live LSP, when the process wired one (None keeps today's behavior).
    live: Option<std::sync::Arc<crate::lsp::LspManager>>,
}

impl ScriptAdapter {
    /// The Python adapter (pyright when present).
    #[must_use]
    pub fn python() -> Self {
        Self {
            language: codegraph::Language::Python.id(),
            languages: vec![codegraph::Language::Python],
            // Probe the binary the roster actually SPAWNS (`pyright-langserver`),
            // not the `pyright` CLI wrapper. An env with only pyright-langserver
            // otherwise failed this gate and silently returned no diagnostics.
            server: &crate::lsp::servers::PYRIGHT,
            live: None,
        }
    }

    /// The TypeScript/TSX/JavaScript adapter (typescript-language-server when
    /// present).
    #[must_use]
    pub fn typescript() -> Self {
        Self {
            language: codegraph::Language::TypeScript.id(),
            languages: vec![
                codegraph::Language::TypeScript,
                codegraph::Language::Tsx,
                codegraph::Language::JavaScript,
            ],
            server: &crate::lsp::servers::TYPESCRIPT,
            live: None,
        }
    }

    /// Attach the live manager. Additive: without it, `diagnostics` stays
    /// the graceful empty vec.
    #[must_use]
    pub fn with_live_lsp(mut self, live: std::sync::Arc<crate::lsp::LspManager>) -> Self {
        self.live = Some(live);
        self
    }

    fn extensions(&self) -> Vec<&'static str> {
        self.languages
            .iter()
            .flat_map(|language| language.extensions())
            .copied()
            .collect()
    }
}

#[async_trait]
impl LanguageAdapter for ScriptAdapter {
    fn language(&self) -> LanguageId {
        self.language.clone()
    }

    fn capability(&self) -> SemanticCapability {
        if server_on_path(self.server) {
            SemanticCapability::LspResolved
        } else {
            SemanticCapability::SyntaxOnly
        }
    }

    async fn parse(&self, input: ParseInput) -> Result<ParseOutput, AdapterError> {
        // The same tree-sitter walk the graph persists, not a second line-based
        // scanner: the old one skipped every indented line (so every method),
        // missed `async def`, `interface`, `type`, `enum` and arrow functions,
        // and returned `signature_hash: None` for everything — so a signature
        // change in a Python or TypeScript symbol could never be detected.
        let symbols = codegraph::parse_symbols(&input.path, &input.source)?;
        Ok(ParseOutput {
            language: self.language(),
            symbols,
        })
    }

    async fn symbols(&self, workspace: &Workspace) -> Result<SymbolIndex, AdapterError> {
        let root = workspace.root.clone();
        let exts: Vec<&'static str> = self.extensions();
        let files = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for file in collect_sources(&root, &exts) {
                let Ok(source) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let rel = rel_path(&root, &file);
                if let Ok(symbols) = codegraph::parse_symbols(&rel, &source) {
                    out.push((rel, symbols));
                }
            }
            out
        })
        .await
        .map_err(|e| AdapterError::Tool {
            tool: "spawn_blocking".into(),
            reason: e.to_string(),
        })?;
        Ok(SymbolIndex {
            language: self.language(),
            files,
        })
    }

    async fn diagnostics(&self, workspace: &Workspace) -> Result<Vec<Diagnostic>, AdapterError> {
        if self.language == codegraph::Language::Python.id() {
            if let Some(ref live) = self.live {
                if server_on_path_async(self.server).await {
                    let root = workspace.root.clone();
                    let exts = self.extensions();
                    let sources: Vec<PathBuf> = collect_sources(&root, &exts)
                        .into_iter()
                        .take(500)
                        .collect();
                    if sources.is_empty() {
                        return Ok(Vec::new());
                    }

                    let mut last_touched: Option<(PathBuf, i64, tokio::time::Instant)> = None;
                    {
                        let spec = self.server;
                        if let Some(client) = live.client_for(spec, &root).await {
                            for file in &sources {
                                let after = tokio::time::Instant::now();
                                if let Ok(version) = client.touch(file).await {
                                    last_touched = Some((file.clone(), version, after));
                                }
                            }

                            if let Some((last_file, version, after)) = last_touched {
                                client
                                    .wait_for_diagnostics(&last_file, version, after)
                                    .await;
                            }

                            let mut out = Vec::new();
                            for file in &sources {
                                let diags = client.diagnostics_for(file).await;
                                let rel = rel_path(&root, file);
                                for d in diags {
                                    out.push(Diagnostic {
                                        path: rel.clone(),
                                        line: d.line + 1,
                                        severity: d.severity,
                                        message: d.message,
                                    });
                                }
                            }
                            return Ok(out);
                        }
                    }
                }
            }
        }

        // Graceful degradation when no live LSP manager is attached or language server is not on PATH.
        Ok(Vec::new())
    }

    async fn build_metadata(&self, _workspace: &Workspace) -> Result<BuildMetadata, AdapterError> {
        Ok(BuildMetadata::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIX 5a: the Python adapter must probe the binary the roster SPAWNS
    /// (`pyright-langserver`) — not the `pyright` CLI wrapper. A mismatch made an
    /// env with only pyright-langserver fail the diagnostics gate and silently
    /// return no diagnostics.
    #[test]
    fn python_adapter_probes_the_spawned_pyright_binary() {
        let adapter = ScriptAdapter::python();
        assert_eq!(
            adapter.server,
            &crate::lsp::servers::PYRIGHT,
            "the adapter must probe the roster entry it spawns"
        );
        assert_eq!(adapter.server.binary, "pyright-langserver");
    }

    /// The `--version` probe must be bounded. It had no timeout at all, so a
    /// binary that never answers (a shim waiting on a lock, a dead network
    /// mount, an interpreter reading stdin) hung whichever thread probed it —
    /// forever. Without the bound this test does not return.
    #[cfg(unix)]
    #[test]
    fn a_hanging_version_probe_is_bounded_and_fails_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("hangs");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let usable = version_probe_succeeds(script.to_str().unwrap(), Duration::from_millis(200));
        let elapsed = started.elapsed();

        assert!(!usable, "a probe that never answers must fail closed");
        assert!(
            elapsed < Duration::from_secs(5),
            "the probe was not bounded: {elapsed:?}"
        );
    }

    /// The roster's pyright entry must NOT be probed with `--version`.
    ///
    /// `pyright-langserver` refuses every invocation that is not a live LSP
    /// connection — `--version` and `--help` both exit 1 with "Connection
    /// input stream is not set" — so the `--version` probe classified a
    /// perfectly healthy install as a dead shim, `capability()` answered
    /// SyntaxOnly, and `LspManager::client_for` refused to spawn it. Python
    /// resolution was dead on every machine, not just unlucky ones.
    #[test]
    fn pyright_is_not_probed_for_a_zero_exit_it_never_gives() {
        assert_eq!(
            crate::lsp::servers::PYRIGHT.probe,
            Probe::StaysAliveOnStdio,
            "pyright has no zero-exit invocation; probing for one always fails"
        );
    }

    /// The liveness probe on a server that behaves exactly like pyright:
    /// non-zero for `--version`, alive and waiting when given `--stdio`.
    ///
    /// This is the regression proper. The fixture fails the old
    /// `--version` probe and passes the new one, so the assertion pair below
    /// is exactly the bug and its fix.
    #[cfg(unix)]
    #[test]
    fn a_server_that_only_answers_on_stdio_probes_as_usable() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("pyright-like");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             if [ \"$1\" != \"--stdio\" ]; then\n\
             echo 'Connection input stream is not set' >&2\n\
             exit 1\n\
             fi\n\
             # A real language server now blocks reading LSP frames from stdin.\n\
             cat >/dev/null\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = script.to_str().unwrap();

        assert!(
            !version_probe_succeeds(path, PROBE_TIMEOUT),
            "fixture must reproduce pyright: `--version` exits non-zero"
        );
        assert!(
            stays_alive_probe(path, &["--stdio"], PROBE_LIVENESS_GRACE),
            "a server that is alive and serving on stdio must probe as usable"
        );
    }

    /// The liveness probe still rejects what the `--version` probe rejected:
    /// a binary that is on PATH but falls over the moment it runs.
    #[cfg(unix)]
    #[test]
    fn the_liveness_probe_still_rejects_a_dead_shim() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("dead-shim");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"error: not installed for this toolchain\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            !stays_alive_probe(script.to_str().unwrap(), &["--stdio"], PROBE_LIVENESS_GRACE),
            "a binary that exits immediately is not a usable server"
        );
    }

    /// A binary that is not there is not usable, whichever question is asked —
    /// and neither probe may hang on the answer.
    #[test]
    fn an_absent_server_binary_is_unusable_under_either_probe() {
        let absent = "codypendent-definitely-absent-langserver";
        assert!(!on_path(absent));
        assert!(!stays_alive_probe(
            absent,
            &["--stdio"],
            PROBE_LIVENESS_GRACE
        ));
    }

    /// The cache is keyed by binary *and* probe. One binary asked both
    /// questions has two answers, and the cheap one must not be served from
    /// the other's slot — that would silently reinstate the bug for any caller
    /// that reached `on_path` with a server binary first.
    ///
    /// The fixture is addressed by absolute path (which `probe_on_path`
    /// resolves through `Path::join`'s absolute-wins rule) rather than by
    /// prepending to `PATH`: mutating the environment races every other thread
    /// in this test binary that reads it.
    #[cfg(unix)]
    #[test]
    fn the_probe_cache_does_not_confuse_two_questions_about_one_binary() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("cache-key-fixture");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             if [ \"$1\" != \"--stdio\" ]; then exit 1; fi\n\
             cat >/dev/null\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = script.to_str().unwrap();

        // Ask the cheap question first, so its `false` lands in the cache.
        assert!(
            !on_path(path),
            "the fixture refuses `--version`, like pyright"
        );
        assert!(
            on_path_with(path, Probe::StaysAliveOnStdio, &["--stdio"]),
            "the liveness answer must not be served from the `--version` cache slot"
        );
    }

    /// FIX 5b: repeated `on_path` calls for the same binary return the cached
    /// result rather than re-running the blocking `--version` probe every time.
    #[test]
    fn on_path_is_cached_per_binary_and_stable() {
        let bin = "codypendent-definitely-absent-binary-5b";
        assert!(!on_path(bin));
        // A second call must agree (served from cache, no second blocking spawn).
        assert!(!on_path(bin));
    }
}

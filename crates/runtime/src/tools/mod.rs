//! The tool layer (STEP 1.7).
//!
//! Six tools the agent loop drives: [`ReadFile`] (`workspace.read_file`),
//! [`Search`] (`workspace.search`), [`Shell`] (`shell.run`), the Git pair
//! [`GitDiff`]/[`ApplyPatch`] (`git.diff` / `git.apply_patch`), [`WriteFile`]
//! (`workspace.write_file`), and [`EditFile`] (`workspace.edit_file`) — the
//! structured-argument alternatives to `git.apply_patch` for a new file or a
//! small full rewrite, or a targeted search/replace edit, respectively (a weak
//! model is poor at reproducing an exact unified diff). Each declares
//! the capability class it needs, exposes a [`ProposedAction`] builder so the
//! STEP 1.10 middleware can run it through the policy engine, and an async
//! `execute` that takes a typed input plus exactly the execution context it
//! needs.
//!
//! Policy → approval → grant is the agent-loop middleware's job; a tool receives
//! an already-granted [`PathScope`]/[`CommandScope`] and still defends its own
//! boundaries — it refuses an out-of-scope path or a non-allow-listed program
//! even when asked, and it never spawns a process it was not permitted to.
//!
//! ## Artifact spill boundary
//!
//! `shell.run` and `git.diff` spill full output to the content-addressed store
//! and return only an observation-compacted *salient* view plus the reference
//! (Chapter 09, Level 1). The store's `put` needs an `&sqlx::SqlitePool`, and
//! `sqlx` is not a dependency of this crate (and cannot be added under the
//! STEP 1.7 scope), so the tools cannot name it. The spill is therefore taken
//! behind the [`ArtifactSink`] trait: the agent loop supplies an implementation
//! that binds a real [`ArtifactStore`] + pool (see [`ClosureSink`], which lets a
//! caller capture a pool *value* without naming its type). This is also the
//! cleaner boundary — a tool has no business knowing about SQLite.
//!
//! [`ArtifactStore`]: codypendent_daemon::artifacts::ArtifactStore

mod artifact;
mod blackboard;
mod docs;
mod edit_file;
mod git;
mod github;
mod label;
mod memory;
mod read_file;
mod registry_search;
mod repository;
mod salient;
mod search;
mod secure_fs;
mod shell;
mod web_search;
mod write_file;

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use codypendent_daemon::artifacts::Provenance;
use codypendent_protocol::{ArtifactId, ArtifactRef};

pub use artifact::{parse_artifact_read, ArtifactRead, ArtifactReadInput};
pub use blackboard::{
    parse_blackboard_post, parse_blackboard_query, BlackboardPostInput, BlackboardPostTool,
    BlackboardQueryInput, BlackboardQueryTool,
};
pub use docs::{
    docs_proposed_action, parse_docs_create, parse_docs_edit, parse_docs_read, parse_docs_suggest,
    DocsCreateInput, DocsCreateTool, DocsEditInput, DocsEditTool, DocsReadInput, DocsReadTool,
    DocsSuggestInput, DocsSuggestTool,
};
pub use edit_file::{parse_edit_file, EditFile, EditFileInput, EditFileOutcome, FileEdit};
pub use git::{
    ApplyPatch, ApplyPatchInput, ApplyPatchOutcome, GitDiff, GitDiffInput, GitDiffOutcome,
};
pub use github::{
    new_pull_request, parse_create_check_run, parse_create_draft_pull_request,
    parse_get_pull_request, parse_list_check_runs, parse_update_pull_request, render_check_runs,
    render_pull_request, CreateCheckRunInput, CreateCheckRunSummary, CreateDraftPullRequest,
    CreateDraftPullRequestInput, GetPullRequest, GetPullRequestInput, ListCheckRuns,
    ListCheckRunsInput, UpdatePullRequestInput, UpdatePullRequestTool,
};
pub use label::tool_label;
pub use memory::{parse_memory_remember, MemoryRemember, MemoryRememberInput};
pub use read_file::{FileExcerpt, ReadFile, ReadFileInput};
pub use registry_search::{
    parse_skills_search, render_registry_search, RegistryCard, RegistrySearch,
    RegistrySearchOutcome, RegistrySearchRequest, SkillDocument, SkillsSearch, SkillsSearchInput,
    SEARCH_CARD_LIMIT, SKILL_DOCUMENT_MAX_BYTES,
};
pub use repository::{RepositoryTest, RepositoryTestOutcome};
pub use salient::{SalientStream, SalientView};
pub use search::{Search, SearchInput, SearchMatch, SearchResults};
pub use shell::{CommandRequest, EnvironmentBinding, Shell, ShellOutcome};
pub use web_search::{parse_web_search, render_search_outcome, WebSearch, WebSearchInput};
pub use write_file::{parse_write_file, WriteFile, WriteFileInput, WriteFileOutcome};

/// A capability class a tool requires. The concrete
/// [`Capability`](codypendent_daemon::policy::Capability) (which carries a
/// scope) is minted by the policy engine from a [`ProposedAction`]; a tool only
/// needs to advertise *which* classes it draws on so the middleware can label
/// the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    /// Reads files within a granted path scope.
    FileRead,
    /// Writes files within a granted path scope.
    FileWrite,
    /// Executes an allow-listed program.
    CommandExecute,
}

/// A structured tool failure. Distinct from a *non-success outcome*: a command
/// that runs and exits non-zero (or is killed on timeout) is a successful
/// [`ShellOutcome`], not a `ToolError`. A `ToolError` means the tool refused or
/// could not run at all — an out-of-scope path, a non-allow-listed program, a
/// patch that does not apply, or an I/O failure. Every variant carries a stable
/// dotted [`code`](ToolError::code) mirroring the policy engine's reason codes.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A path resolved outside every granted root.
    #[error("path is outside the granted scope: {0}")]
    PathOutOfScope(PathBuf),
    /// A path matched the deny list even inside an allowed root.
    #[error("path is denied by policy: {0}")]
    PathDenied(PathBuf),
    /// The working directory is not inside the granted path scope.
    #[error("working directory is outside the granted scope: {0}")]
    CwdOutOfScope(PathBuf),
    /// The program is not in the granted command allow-list (no process spawned).
    #[error("program `{0}` is not in the command allow-list")]
    ProgramNotAllowed(String),
    /// A model-supplied environment binding was refused because its name can
    /// hijack what the command executes (no process spawned).
    #[error("environment variable `{0}` is not permitted")]
    EnvironmentNotAllowed(String),
    /// The program could not be found on the daemon's PATH.
    #[error("program `{0}` was not found")]
    ProgramNotFound(String),
    /// A read range was malformed (e.g. start line 0, or end before start).
    #[error("invalid range {start}..={end}: {reason}")]
    InvalidRange {
        /// Requested start line (1-based).
        start: usize,
        /// Requested end line (1-based, inclusive).
        end: usize,
        /// Why the range was rejected.
        reason: String,
    },
    /// `git apply --check` rejected the patch; nothing was applied.
    #[error("patch does not apply: {0}")]
    PatchDoesNotApply(String),
    /// A write tool's target exists but is not a regular file — a symlink or a
    /// directory. Write tools refuse to write through or over either: a
    /// symlink at the leaf could redirect the write outside the granted scope
    /// (the leaf-swap guard), and a directory cannot be truncated and
    /// overwritten as file content.
    #[error("not a regular file (symlink or directory): {0}")]
    NotRegularFile(PathBuf),
    /// `workspace.edit_file`: an edit's `search` text was not found in the
    /// current buffer (after any earlier edits in the same call have been
    /// applied). Nothing is written — the whole call fails atomically.
    #[error("edit {index}: search text not found in {path}", path = path.display())]
    SearchNotFound {
        /// The file being edited.
        path: PathBuf,
        /// 1-based index of the failing edit, so the model knows which pair to fix.
        index: usize,
    },
    /// `workspace.edit_file`: an edit's `search` text matched more than once
    /// in the current buffer, so the target occurrence is ambiguous. Nothing
    /// is written — the whole call fails atomically.
    #[error(
        "edit {index}: search text is ambiguous ({count} matches) — include more surrounding context so it is unique",
        )]
    SearchAmbiguous {
        /// The file being edited.
        path: PathBuf,
        /// 1-based index of the failing edit.
        index: usize,
        /// The number of non-overlapping matches found.
        count: usize,
    },
    /// `workspace.edit_file`: an edit's `search` text was empty — trivially
    /// ambiguous, rejected before any matching is attempted.
    #[error("edit {index}: search text must not be empty")]
    EmptySearch {
        /// 1-based index of the offending edit.
        index: usize,
    },
    /// `workspace.edit_file`: the target file exceeds the byte-bounded edit
    /// limit; refused rather than silently truncated. Use `git.apply_patch`
    /// for very large files.
    #[error("file exceeds the {cap}-byte edit limit; use git.apply_patch for very large files")]
    FileTooLarge {
        /// The file that was too large to load.
        path: PathBuf,
        /// The byte ceiling that was exceeded.
        cap: u64,
    },
    /// A daemon-issued helper process (ripgrep, git) exceeded its wall-clock
    /// bound and was killed. Distinct from a `shell.run` timeout, which is a
    /// successful [`ShellOutcome`] with `timed_out` set: these helpers have no
    /// partial-outcome shape, so a hang surfaces as this refusal instead of
    /// wedging the run (cancellation is blind while a tool executes).
    #[error("`{tool}` timed out after {seconds}s and was killed")]
    TimedOut {
        /// The tool that hung.
        tool: &'static str,
        /// The bound that expired.
        seconds: u64,
    },
    /// Underlying I/O failure spawning or talking to a child process or file.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A failure that carries no more specific structure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ToolError {
    /// A stable, dotted machine code for this failure, suitable for a
    /// `ToolCompleted`/`CodypendentError` payload.
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::PathOutOfScope(_) => "tool.path-out-of-scope",
            ToolError::PathDenied(_) => "tool.path-denied",
            ToolError::CwdOutOfScope(_) => "tool.cwd-out-of-scope",
            ToolError::ProgramNotAllowed(_) => "tool.program-not-allowlisted",
            ToolError::EnvironmentNotAllowed(_) => "tool.environment-not-allowlisted",
            ToolError::ProgramNotFound(_) => "tool.program-not-found",
            ToolError::InvalidRange { .. } => "tool.invalid-range",
            ToolError::PatchDoesNotApply(_) => "tool.patch-does-not-apply",
            ToolError::NotRegularFile(_) => "tool.not-regular-file",
            ToolError::SearchNotFound { .. } => "tool.search-not-found",
            ToolError::SearchAmbiguous { .. } => "tool.search-ambiguous",
            ToolError::EmptySearch { .. } => "tool.empty-search",
            ToolError::FileTooLarge { .. } => "tool.file-too-large",
            ToolError::TimedOut { .. } => "tool.timed-out",
            ToolError::Io(_) => "tool.io-error",
            ToolError::Other(_) => "tool.error",
        }
    }
}

/// A sink for full tool output, backing the content-addressed store.
///
/// Implemented by the agent loop over a real
/// [`ArtifactStore`](codypendent_daemon::artifacts::ArtifactStore) and pool; the
/// tools depend only on this trait so they need not name the SQLite pool type
/// (see the module docs). `store` returns the [`ArtifactRef`] the salient view
/// references.
#[async_trait]
pub trait ArtifactSink: Send + Sync {
    /// Persist `bytes` with the given media type and provenance, returning a
    /// reference to the stored occurrence.
    async fn store(
        &self,
        media_type: &str,
        provenance: Provenance,
        bytes: &[u8],
    ) -> anyhow::Result<ArtifactRef>;
}

/// The READ side of the artifact boundary, mirroring [`ArtifactSink`]: the
/// `artifact.read` tool rehydrates a stored artifact's bytes by id through
/// this seam, for the same reason the spill goes through a sink — the store's
/// `get` needs a pool this crate cannot name (see the module docs). `Ok(None)`
/// means "no such artifact", surfaced to the model as a legible tool failure;
/// implementations must also answer `Ok(None)` for content the model may not
/// see (e.g. secret-classified artifacts), never hand it back.
#[async_trait]
pub trait ArtifactReader: Send + Sync {
    /// Load the stored bytes (and their media type) for `id`, or `None` when
    /// no such artifact exists / it may not be disclosed.
    async fn load(&self, id: ArtifactId) -> anyhow::Result<Option<LoadedArtifact>>;
}

/// An artifact's stored bytes plus the media type they were stored under —
/// what [`ArtifactReader::load`] hands back for rendering.
#[derive(Debug, Clone)]
pub struct LoadedArtifact {
    /// The media type recorded when the artifact was stored.
    pub media_type: String,
    /// The full stored bytes (bounded rendering is the TOOL's job, so the
    /// reader stays a dumb byte fetch).
    pub bytes: Vec<u8>,
}

/// An [`ArtifactReader`] built from a closure — the [`ClosureSink`] idiom for
/// the read side, so a caller can capture an `ArtifactStore` + pool value and
/// forward to the store's `get`.
pub struct ClosureReader<F>(pub F);

#[async_trait]
impl<F, Fut> ArtifactReader for ClosureReader<F>
where
    F: Fn(ArtifactId) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = anyhow::Result<Option<LoadedArtifact>>> + Send,
{
    async fn load(&self, id: ArtifactId) -> anyhow::Result<Option<LoadedArtifact>> {
        (self.0)(id).await
    }
}

/// An [`ArtifactSink`] built from a closure, so a caller can capture an
/// `ArtifactStore` and a pool *value* (whose type it may be unable to name) and
/// forward to [`ArtifactStore::put`](codypendent_daemon::artifacts::ArtifactStore::put).
///
/// ```ignore
/// let pool = codypendent_daemon::db::open_database(&db).await?; // type inferred
/// let store = ArtifactStore::new(root);
/// let sink = ClosureSink(move |media: String, prov, bytes: Vec<u8>| {
///     let (store, pool) = (&store, &pool);
///     async move { store.put(pool, &media, DataClassification::Internal, prov, &bytes).await }
/// });
/// ```
pub struct ClosureSink<F>(pub F);

#[async_trait]
impl<F, Fut> ArtifactSink for ClosureSink<F>
where
    F: Fn(String, Provenance, Vec<u8>) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = anyhow::Result<ArtifactRef>> + Send,
{
    async fn store(
        &self,
        media_type: &str,
        provenance: Provenance,
        bytes: &[u8],
    ) -> anyhow::Result<ArtifactRef> {
        (self.0)(media_type.to_string(), provenance, bytes.to_vec()).await
    }
}

/// Largest volume of a single stream held in memory (and thus the largest that
/// can be handed to the artifact store, whose `put` takes bytes by slice).
/// Output beyond this is drained from the pipe — so the child never blocks — but
/// not retained; the stream is flagged `overflowed`.
const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

/// The Chapter 11 in-memory soft cap (1 MiB). Output larger than this is
/// considered "large": it is always spilled to an artifact and its salient view
/// is definitely truncated.
const IN_MEMORY_CAP: usize = 1024 * 1024;

/// Render `program` + `args` as a single display string for the salient header.
fn display_command(program: &std::path::Path, args: &[String]) -> String {
    let mut out = program.to_string_lossy().into_owned();
    for arg in args {
        out.push(' ');
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            out.push('"');
            out.push_str(arg);
            out.push('"');
        } else {
            out.push_str(arg);
        }
    }
    out
}

/// Hard ceiling on any single command's wall clock, applied even when the
/// command scope declares no ceiling of its own (`maximum_seconds == 0`). The
/// request's `timeout` is model-supplied, so without this an unset scope
/// ceiling would let a model-chosen `u64::MAX` run forever.
const ABSOLUTE_MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Clamp an effective timeout to the command scope's wall-clock ceiling. A
/// ceiling of zero means "unset": the request is then bounded only by
/// [`ABSOLUTE_MAX_TIMEOUT`].
fn effective_timeout(requested: Duration, maximum_seconds: u64) -> Duration {
    let bounded = requested.min(ABSOLUTE_MAX_TIMEOUT);
    if maximum_seconds == 0 {
        bounded
    } else {
        bounded.min(Duration::from_secs(maximum_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_timeout, ABSOLUTE_MAX_TIMEOUT};
    use std::time::Duration;

    /// With no scope ceiling (`maximum_seconds == 0`) a modest request passes
    /// through unchanged — the clamp only ever narrows.
    #[test]
    fn unset_ceiling_keeps_a_modest_request() {
        assert_eq!(
            effective_timeout(Duration::from_secs(30), 0),
            Duration::from_secs(30)
        );
    }

    /// With no scope ceiling a model-supplied timeout is still bounded by the
    /// absolute maximum — the fix that stops a `u64::MAX` request running
    /// forever (C12).
    #[test]
    fn unset_ceiling_is_bounded_by_the_absolute_maximum() {
        assert_eq!(
            effective_timeout(Duration::from_secs(u64::MAX / 2), 0),
            ABSOLUTE_MAX_TIMEOUT
        );
        assert_eq!(effective_timeout(Duration::MAX, 0), ABSOLUTE_MAX_TIMEOUT);
    }

    /// A scope ceiling below the request clamps the request down to it.
    #[test]
    fn scope_ceiling_clamps_a_larger_request() {
        assert_eq!(
            effective_timeout(Duration::from_secs(600), 60),
            Duration::from_secs(60)
        );
    }

    /// A request below the scope ceiling is left alone (never rounded up).
    #[test]
    fn request_below_ceiling_is_unchanged() {
        assert_eq!(
            effective_timeout(Duration::from_secs(10), 60),
            Duration::from_secs(10)
        );
    }

    /// Both bounds apply at once: the absolute maximum caps even a scope
    /// ceiling that exceeds it, so no configuration can lift the hard ceiling.
    #[test]
    fn absolute_maximum_caps_an_over_large_scope_ceiling() {
        let huge_ceiling = ABSOLUTE_MAX_TIMEOUT.as_secs() * 10;
        assert_eq!(
            effective_timeout(Duration::MAX, huge_ceiling),
            ABSOLUTE_MAX_TIMEOUT
        );
    }
}

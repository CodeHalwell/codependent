//! The skill-writer (outcome 4's authoring half, 2026-08-13 review F4.2):
//! authors a `skill.toml` + `SKILL.md` package into the governed registry.
//!
//! The review found no skill-writer of any kind existed — "no tool, no
//! command, no palette entry, no agent role" — while the doc-writer's four
//! `docs.*` tools were already implemented and only unadvertised to the model
//! (agent-retrieval's fix, `crates/runtime/src/agent.rs`). This module is the
//! missing half for skills: it builds a valid manifest from a [`SkillDraft`],
//! validates it against the SAME [`codypendent_knowledge::manifest::SkillManifest`]
//! `codypendent skill add` validates against (no shadow validator, no chance of
//! drift), and installs it through the SAME guardrailed
//! [`codypendent_knowledge::install_package`] path (traversal-safe id, staged
//! copy, hash-verified) — the pipeline the 2026-08-13 review called "genuinely
//! good" and asked implementers not to tear out.
//!
//! ## Draft by construction
//!
//! [`SkillDraft::new`] always starts `status = draft` — there is no
//! constructor that starts active. Only [`SkillDraft::promote_to_active`] can
//! flip it, and it deliberately bumps the version at the same moment: the
//! registry's own hash-vs-version rule
//! (`codypendent_knowledge::registry::Registry::register_package`) flags a
//! **same-version, changed-hash** re-registration as `Modified` regardless of
//! what the manifest's own `status` field says, so a promotion that edits
//! `status` alone at the same version silently lands `Modified`, not `Active`
//! — precisely the loop the review's F9.5 caught users in
//! (`codypendent skill add`'s own advice — "set status = active and re-add
//! it" — is not sufficient for that case). Bumping the version alongside the
//! status flip sidesteps the trap entirely; see
//! `promoting_without_a_version_bump_lands_modified_not_active` below, which
//! pins the trap itself so nobody "simplifies" `promote_to_active` back into it.
//!
//! ## Coordination note (outcome 4)
//!
//! This module produces packages and installs them; it does not decide HOW a
//! user or a running agent reaches it (a CLI subcommand, a tool the model can
//! call). Wiring a `codypendent skill new` subcommand touches
//! `crates/cli/src/main.rs`/`commands.rs`'s subcommand table, which the brief
//! assigns to **agent-models** — see
//! `.impl/proposals/agent-models-from-agent-audio.md` for the exact dispatch
//! shim this file is ready to be called from. Advertising a `skills.write`
//! *tool* (agent-callable, not just human-CLI) would touch
//! `crates/runtime/src/tools/**`/`agent.rs`, owned by **agent-retrieval**, who
//! already owns tool advertisement for this outcome per my assignment; the
//! functions below have exactly the shape (`&SqlitePool` in, `RegistryItem`
//! out, typed errors) a tool dispatcher would need, so wiring either surface
//! is a thin shim, not a rewrite.

use std::path::{Path, PathBuf};

use codypendent_knowledge::types::Scope;
use codypendent_knowledge::{install_package, RegistryItem, SkillInstallError};
use codypendent_protocol::RepositoryId;
use sqlx::SqlitePool;

/// The lifecycle status a [`SkillDraft`] may declare. Deliberately NOT the
/// full `codypendent_knowledge::RegistryStatus` (`Modified`/`Deprecated` are
/// outcomes of re-registration, not something an author declares up front).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftStatus {
    Draft,
    Active,
}

impl DraftStatus {
    fn as_str(self) -> &'static str {
        match self {
            DraftStatus::Draft => "draft",
            DraftStatus::Active => "active",
        }
    }
}

/// The `[permissions]` table a drafted skill declares. Mirrors
/// `codypendent_knowledge::manifest::SkillPermissions`'s shape exactly (its own
/// type rather than a reuse: that one derives only `Deserialize`, and pulling
/// it in as a write-side type would blur "the thing skill.toml parses into"
/// with "the thing this module renders").
#[derive(Debug, Clone, Default)]
pub struct DraftPermissions {
    pub filesystem_read: Vec<String>,
    pub filesystem_write: Vec<String>,
    pub commands: Vec<String>,
    pub network: Vec<String>,
    pub secrets: Vec<String>,
}

/// The `[limits]` table. Validated by the real loader same as everything
/// else; enforcement is a later phase (see `SkillManifest::limits`'s own doc
/// comment), unchanged by this module.
#[derive(Debug, Clone, Default)]
pub struct DraftLimits {
    pub maximum_iterations: Option<u32>,
    pub maximum_duration_seconds: Option<u64>,
    pub maximum_cost_usd: Option<f64>,
}

/// Everything needed to author one skill package. Build with [`SkillDraft::new`]
/// then the `with_*` setters; the manifest fields not exposed here
/// (`schema_version`, `[entrypoints]`) are never meaningfully variable — this
/// module always writes `schema_version = 1` and an `instructions = "SKILL.md"`
/// entrypoint, because it always writes exactly one `SKILL.md`.
#[derive(Debug, Clone)]
pub struct SkillDraft {
    pub id: String,
    pub name: String,
    pub version: String,
    pub scope: Scope,
    status: DraftStatus,
    pub description: String,
    pub intents: Vec<String>,
    pub languages: Vec<String>,
    pub required_tools: Vec<String>,
    pub optional_tools: Vec<String>,
    pub permissions: DraftPermissions,
    pub limits: DraftLimits,
    /// `"local-user"` unless overridden — a record of who authored the package,
    /// not a trust grant. Nothing signs or checks it, so every package loaded
    /// from disk registers as
    /// [`codypendent_knowledge::manifest::PACKAGE_TRUST_TIER`] (`Community`)
    /// whatever this says. `"local-user"` used to be a reserved value that
    /// self-promoted a package to `FirstParty` — exactly the self-assertion the
    /// 2026-08-13 review removed.
    pub publisher: String,
    /// Must stay `false`: [`load_package`](codypendent_knowledge::load_package)
    /// refuses a manifest that requires a signature, because no skill path
    /// verifies one. Setting it writes a package this machine cannot install.
    pub signature_required: bool,
    /// The `SKILL.md` body — the procedure a model reading this skill's card
    /// and asking for it follows.
    pub procedure: String,
}

impl SkillDraft {
    /// Start a new draft. `scope` is the concrete [`Scope`] the package will
    /// register under (e.g. [`codypendent_knowledge::local_user_scope`] for a
    /// `user`-tier skill, `Scope::Repository(id)` for a `repository`-tier
    /// one) — its [`Scope::tier`] is what `skill.toml`'s `scope` field
    /// renders, so the two can never disagree the way a hand-authored
    /// manifest's could.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        scope: Scope,
        description: impl Into<String>,
        procedure: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: "0.1.0".to_string(),
            scope,
            status: DraftStatus::Draft,
            description: description.into(),
            intents: Vec::new(),
            languages: Vec::new(),
            required_tools: Vec::new(),
            optional_tools: Vec::new(),
            permissions: DraftPermissions::default(),
            limits: DraftLimits::default(),
            publisher: "local-user".to_string(),
            signature_required: false,
            procedure: procedure.into(),
        }
    }

    #[must_use]
    pub fn with_intents(mut self, intents: Vec<String>) -> Self {
        self.intents = intents;
        self
    }

    #[must_use]
    pub fn with_languages(mut self, languages: Vec<String>) -> Self {
        self.languages = languages;
        self
    }

    #[must_use]
    pub fn with_required_tools(mut self, tools: Vec<String>) -> Self {
        self.required_tools = tools;
        self
    }

    #[must_use]
    pub fn with_optional_tools(mut self, tools: Vec<String>) -> Self {
        self.optional_tools = tools;
        self
    }

    #[must_use]
    pub fn with_permissions(mut self, permissions: DraftPermissions) -> Self {
        self.permissions = permissions;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: DraftLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Whether this draft currently declares `active`.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == DraftStatus::Active
    }

    /// Promote to active for the NEXT registration, bumping the version in
    /// the same call. See the module doc comment for why the version bump is
    /// not optional: a same-version status flip lands `Modified`, not
    /// `Active`, because of how the registry detects an unversioned content
    /// change (`codypendent_knowledge::registry::Registry::register_package`).
    pub fn promote_to_active(&mut self, next_version: impl Into<String>) {
        self.version = next_version.into();
        self.status = DraftStatus::Active;
    }

    /// Render `skill.toml`'s exact text. Every string value is TOML-escaped
    /// ([`toml_string`]) — this module has no control over what an agent or a
    /// user types into `description`/`name`/intents, so it must not assume
    /// they are already safe to embed literally.
    #[must_use]
    pub fn render_manifest_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("schema_version = 1\n");
        out.push_str(&format!("id = {}\n", toml_string(&self.id)));
        out.push_str(&format!("name = {}\n", toml_string(&self.name)));
        out.push_str(&format!("version = {}\n", toml_string(&self.version)));
        out.push_str(&format!("scope = {}\n", toml_string(self.scope.tier())));
        out.push_str(&format!("status = {}\n", toml_string(self.status.as_str())));
        out.push('\n');
        out.push_str(&format!(
            "description = {}\n",
            toml_string(&self.description)
        ));
        out.push_str(&format!("intents = {}\n", toml_string_array(&self.intents)));
        out.push_str(&format!(
            "languages = {}\n",
            toml_string_array(&self.languages)
        ));
        out.push('\n');
        out.push_str(&format!(
            "required_tools = {}\n",
            toml_string_array(&self.required_tools)
        ));
        out.push_str(&format!(
            "optional_tools = {}\n",
            toml_string_array(&self.optional_tools)
        ));
        out.push('\n');
        out.push_str("[permissions]\n");
        out.push_str(&format!(
            "filesystem_read = {}\n",
            toml_string_array(&self.permissions.filesystem_read)
        ));
        out.push_str(&format!(
            "filesystem_write = {}\n",
            toml_string_array(&self.permissions.filesystem_write)
        ));
        out.push_str(&format!(
            "commands = {}\n",
            toml_string_array(&self.permissions.commands)
        ));
        out.push_str(&format!(
            "network = {}\n",
            toml_string_array(&self.permissions.network)
        ));
        out.push_str(&format!(
            "secrets = {}\n",
            toml_string_array(&self.permissions.secrets)
        ));
        out.push('\n');
        out.push_str("[limits]\n");
        if let Some(value) = self.limits.maximum_iterations {
            out.push_str(&format!("maximum_iterations = {value}\n"));
        }
        if let Some(value) = self.limits.maximum_duration_seconds {
            out.push_str(&format!("maximum_duration_seconds = {value}\n"));
        }
        if let Some(value) = self.limits.maximum_cost_usd {
            out.push_str(&format!("maximum_cost_usd = {value}\n"));
        }
        out.push('\n');
        out.push_str("[entrypoints]\n");
        out.push_str("instructions = \"SKILL.md\"\n");
        out.push('\n');
        out.push_str("[trust]\n");
        out.push_str(&format!("publisher = {}\n", toml_string(&self.publisher)));
        out.push_str(&format!(
            "signature_required = {}\n",
            self.signature_required
        ));
        out
    }
}

/// TOML-escape a string value and wrap it in quotes.
fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

/// TOML-escape a list of strings into an inline array.
fn toml_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| toml_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Errors from authoring or installing a drafted skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillWriterError {
    /// Writing `skill.toml`/`SKILL.md` to `dir` failed.
    #[error("could not write the skill package to {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Validation or installation failed — `codypendent skill add`'s own
    /// error type, unmodified, so a caller sees exactly the same message a
    /// human running `skill add` on the same content would.
    #[error(transparent)]
    Install(#[from] SkillInstallError),
}

/// Write `draft`'s `skill.toml` + `SKILL.md` to `dir` (creating it if needed).
/// Pure filesystem I/O — no validation, no registry write; see
/// [`author_and_install`] for the full pipeline.
pub fn author_package(dir: &Path, draft: &SkillDraft) -> Result<(), SkillWriterError> {
    let write = |path: PathBuf, contents: &str| {
        std::fs::write(&path, contents).map_err(|source| SkillWriterError::Io { path, source })
    };
    std::fs::create_dir_all(dir).map_err(|source| SkillWriterError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    write(dir.join("skill.toml"), &draft.render_manifest_toml())?;
    write(dir.join("SKILL.md"), &draft.procedure)?;
    Ok(())
}

/// Author `draft` at `source_dir`, then validate + install it through the
/// SAME pipeline `codypendent skill add <dir>` runs
/// (`codypendent_knowledge::install_package`): validate-in-place against the
/// real [`codypendent_knowledge::manifest::SkillManifest`], copy to a staged
/// sibling under `skills_root`, swap it into place, register it. `draft`'s
/// own `scope` decides whether `anchor_repository` matters (only for a
/// `repository`-tier scope; ignored for `user`).
///
/// Calling this twice with the same `draft.id` re-registers the same registry
/// identity (see [`SkillDraft::promote_to_active`] for the intended
/// draft→active re-registration).
pub async fn author_and_install(
    pool: &SqlitePool,
    source_dir: &Path,
    skills_root: &Path,
    anchor_repository: RepositoryId,
    draft: &SkillDraft,
) -> Result<(RegistryItem, PathBuf), SkillWriterError> {
    author_package(source_dir, draft)?;
    let (item, installed) =
        install_package(pool, source_dir, skills_root, anchor_repository).await?;
    Ok((item, installed))
}

#[cfg(test)]
mod tests {
    use super::*;

    use codypendent_knowledge::{
        local_user_scope, retrieve, HashingEmbedder, Registry, RegistryStatus, RetrievalConfig,
        RetrievalIndexes, RetrievalQuery, RiskClass,
    };

    fn minimal_draft(id: &str) -> SkillDraft {
        SkillDraft::new(
            id,
            "Test Skill",
            local_user_scope(),
            "A skill authored by the skill-writer's own tests.",
            "# Test Skill\n\nDo the thing, carefully.\n",
        )
        .with_intents(vec!["do the thing".to_string()])
    }

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = codypendent_knowledge::db::open(&tmp.path().join("t.db"))
            .await
            .expect("open db");
        (tmp, pool)
    }

    #[test]
    fn render_manifest_toml_round_trips_through_the_real_validator() {
        // "Validated against knowledge/src/manifest.rs SkillManifest" — the
        // literal ask. This does not go through `install_package`'s file
        // copy/registry write at all; it proves the RENDERER alone produces
        // text the real loader accepts.
        let dir = tempfile::tempdir().expect("tempdir");
        let draft = minimal_draft("meta.render-test");
        author_package(dir.path(), &draft).expect("author");

        let item = codypendent_knowledge::manifest::load_package(dir.path(), local_user_scope())
            .expect("the real SkillManifest loader accepts this manifest");
        assert_eq!(item.name, "meta.render-test");
        assert_eq!(
            item.status,
            RegistryStatus::Draft,
            "SkillDraft::new starts draft"
        );
        assert_eq!(item.description, draft.description);
        assert_eq!(item.intents, vec!["do the thing".to_string()]);
    }

    #[test]
    fn every_toml_special_character_in_free_text_survives_the_real_parser() {
        // An agent or a user can type anything into `description`/`procedure`
        // — quotes, backslashes, newlines. Prove the escaper does not produce
        // a manifest the real parser rejects or silently truncates.
        let dir = tempfile::tempdir().expect("tempdir");
        let draft = SkillDraft::new(
            "meta.escape-test",
            "Quote \" and \\ and \n newline",
            local_user_scope(),
            "has \"quotes\", a back\\slash, and a\nnewline",
            "# Body\n",
        );
        author_package(dir.path(), &draft).expect("author");
        let item = codypendent_knowledge::manifest::load_package(dir.path(), local_user_scope())
            .expect("the real loader must accept escaped free text");
        assert_eq!(
            item.description,
            "has \"quotes\", a back\\slash, and a\nnewline"
        );
    }

    #[tokio::test]
    async fn a_draft_skill_is_invisible_until_promoted_to_active_then_disclosed() {
        // THE round trip the outcome asks for: author as draft -> prove
        // retrieval never discloses it (F9.5's hard filter,
        // `retrieval/mod.rs`) -> promote -> prove it now IS disclosed.
        let (_db_tmp, pool) = temp_pool().await;
        let skills_root_tmp = tempfile::tempdir().expect("tempdir");
        let skills_root = skills_root_tmp.path().join("skills");
        let source = tempfile::tempdir().expect("tempdir");
        let anchor = RepositoryId::new();

        let mut draft = minimal_draft("meta.roundtrip-skill");
        let (item, installed) =
            author_and_install(&pool, source.path(), &skills_root, anchor, &draft)
                .await
                .expect("author + install as draft");
        assert_eq!(item.status, RegistryStatus::Draft);
        assert!(installed.join("skill.toml").is_file());
        assert!(installed.join("SKILL.md").is_file());

        let query =
            RetrievalQuery::new("do the thing", vec![local_user_scope()], RiskClass::Medium);
        let disclosed_while_draft = retrieve_skill_names(&pool, &query).await;
        assert!(
            !disclosed_while_draft.contains(&"meta.roundtrip-skill".to_string()),
            "a draft skill must never be retrieval-disclosed: {disclosed_while_draft:?}"
        );

        // Promote: bump version + declare active in the SAME call, avoiding
        // the same-version-status-flip trap (see the module doc comment and
        // `promoting_without_a_version_bump_lands_modified_not_active` below).
        draft.promote_to_active("0.2.0");
        let (promoted, _) = author_and_install(&pool, source.path(), &skills_root, anchor, &draft)
            .await
            .expect("promote + reinstall");
        assert_eq!(promoted.status, RegistryStatus::Active, "{promoted:?}");
        assert_eq!(
            promoted.id, item.id,
            "promotion re-registers the SAME identity, not a new one"
        );

        let disclosed_once_active = retrieve_skill_names(&pool, &query).await;
        assert!(
            disclosed_once_active.contains(&"meta.roundtrip-skill".to_string()),
            "a promoted (active) skill must be retrieval-disclosed: {disclosed_once_active:?}"
        );
    }

    #[tokio::test]
    async fn promoting_without_a_version_bump_lands_modified_not_active() {
        // Pins the trap `promote_to_active` exists to avoid (2026-08-13
        // review F9.5): flipping `status` alone at the SAME version collides
        // with the registry's own "same version, changed hash -> Modified"
        // rule, so status is silently overridden. If this test ever starts
        // failing because someone "simplified" the registry's rule, that is
        // real news — it means `promote_to_active`'s version bump may no
        // longer be load-bearing, and this test (not the implementation)
        // should be revisited first.
        let (_db_tmp, pool) = temp_pool().await;
        let skills_root_tmp = tempfile::tempdir().expect("tempdir");
        let skills_root = skills_root_tmp.path().join("skills");
        let source = tempfile::tempdir().expect("tempdir");
        let anchor = RepositoryId::new();

        let mut draft = minimal_draft("meta.trap-skill");
        author_and_install(&pool, source.path(), &skills_root, anchor, &draft)
            .await
            .expect("install draft");

        // The NAIVE (wrong) promotion: flip status, keep the same version.
        draft.status = DraftStatus::Active;
        let (landed, _) = author_and_install(&pool, source.path(), &skills_root, anchor, &draft)
            .await
            .expect("reinstall");
        assert_eq!(
            landed.status,
            RegistryStatus::Modified,
            "a same-version status-only edit must land Modified, not Active — \
             proving why promote_to_active always bumps the version"
        );
    }

    async fn retrieve_skill_names(pool: &SqlitePool, query: &RetrievalQuery) -> Vec<String> {
        let items = Registry::new().list(pool).await.expect("list registry");
        let indexes =
            RetrievalIndexes::build(&items, HashingEmbedder::new()).expect("build indexes");
        let result =
            retrieve(&items, &indexes, query, &RetrievalConfig::default()).expect("retrieve");
        result.skills.into_iter().map(|card| card.name).collect()
    }

    #[test]
    fn a_freshly_drafted_skill_is_not_active() {
        assert!(!minimal_draft("meta.x").is_active());
    }

    #[test]
    fn promote_to_active_sets_both_the_status_and_the_version() {
        let mut draft = minimal_draft("meta.x");
        assert_eq!(draft.version, "0.1.0");
        draft.promote_to_active("1.0.0");
        assert!(draft.is_active());
        assert_eq!(draft.version, "1.0.0");
    }
}

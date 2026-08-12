//! 2026-08-11 review, "activate skills": the `codypendent skill add` client
//! core.
//!
//! Before this command there was no production path from a skill package on
//! disk into the governed registry at all — `register_package` was called only
//! from tests, so retrieval had nothing but the built-ins to disclose. Like
//! `index rebuild`, the command is daemon-free (it opens the database
//! directly), so this drives it against a temporary runtime root and asserts
//! the package is installed under the well-known skills root AND registered
//! under the scope its manifest declares — the two halves that make a skill
//! actually retrievable.

use codypendent_knowledge::{
    db as knowledge_db, user_skills_root, Registry, RegistryItemKind, RegistryStatus,
};
use codypendent_protocol::discovery::RuntimePaths;

/// Write a minimal valid skill package at `dir`.
fn write_package(dir: &std::path::Path, id: &str, status: &str) {
    std::fs::create_dir_all(dir).expect("package dir");
    std::fs::write(
        dir.join("skill.toml"),
        format!(
            "schema_version = 1\n\
             id = \"{id}\"\n\
             name = \"Fix Flaky CI\"\n\
             version = \"0.2.0\"\n\
             scope = \"user\"\n\
             status = \"{status}\"\n\
             description = \"Diagnose and repair flaky CI failures.\"\n\
             intents = [\"ci failure\"]\n\
             \n\
             [entrypoints]\n\
             instructions = \"SKILL.md\"\n\
             \n\
             [trust]\n\
             publisher = \"local-user\"\n\
             signature_required = false\n"
        ),
    )
    .expect("write skill.toml");
    std::fs::write(dir.join("SKILL.md"), "# Fix flaky CI\n").expect("write SKILL.md");
}

#[tokio::test]
async fn skill_add_installs_the_package_and_registers_it() {
    let home = tempfile::tempdir().expect("runtime root");
    let paths = RuntimePaths::from_data_dir(home.path().to_path_buf());
    let source = tempfile::tempdir().expect("source");
    write_package(source.path(), "test.fix-flaky-ci", "active");

    codypendent_cli::commands::skill_add(&paths, source.path())
        .await
        .expect("skill add succeeds");

    // The package landed under the root the daemon's startup scan walks…
    let installed = user_skills_root(&paths.data_dir).join("test.fix-flaky-ci");
    assert!(
        installed.join("skill.toml").is_file() && installed.join("SKILL.md").is_file(),
        "the whole package must be copied to {}",
        installed.display()
    );

    // …and the registry carries it, Active, under the local user scope the
    // executor widens its context query with.
    let pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db");
    let skills: Vec<_> = Registry::new()
        .list(&pool)
        .await
        .expect("list registry")
        .into_iter()
        .filter(|item| item.kind == RegistryItemKind::Skill)
        .collect();
    assert_eq!(skills.len(), 1, "exactly the added skill: {skills:?}");
    assert_eq!(skills[0].name, "test.fix-flaky-ci");
    assert_eq!(skills[0].status, RegistryStatus::Active);
    assert_eq!(skills[0].scope, codypendent_knowledge::local_user_scope());
}

#[tokio::test]
async fn re_adding_a_package_keeps_one_row() {
    let home = tempfile::tempdir().expect("runtime root");
    let paths = RuntimePaths::from_data_dir(home.path().to_path_buf());
    let source = tempfile::tempdir().expect("source");
    write_package(source.path(), "test.repeat", "active");

    codypendent_cli::commands::skill_add(&paths, source.path())
        .await
        .expect("first add");
    codypendent_cli::commands::skill_add(&paths, source.path())
        .await
        .expect("second add");

    let pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db");
    let skills = Registry::new()
        .list(&pool)
        .await
        .expect("list registry")
        .into_iter()
        .filter(|item| item.kind == RegistryItemKind::Skill)
        .count();
    assert_eq!(skills, 1, "re-adding must not duplicate the identity");
}

#[tokio::test]
async fn an_invalid_package_is_refused_and_installs_nothing() {
    let home = tempfile::tempdir().expect("runtime root");
    let paths = RuntimePaths::from_data_dir(home.path().to_path_buf());
    let source = tempfile::tempdir().expect("source");
    write_package(source.path(), "test.broken", "active");
    std::fs::remove_file(source.path().join("SKILL.md")).expect("break the package");

    let error = codypendent_cli::commands::skill_add(&paths, source.path())
        .await
        .expect_err("a package with a missing entrypoint must be refused");
    assert!(
        format!("{error:#}").contains("SKILL.md"),
        "the failure must name the missing entrypoint: {error:#}"
    );
    assert!(
        !user_skills_root(&paths.data_dir)
            .join("test.broken")
            .exists(),
        "a refused package must never land in the skills root"
    );
}

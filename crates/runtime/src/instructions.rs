//! Hierarchical instruction-file discovery (AGENTS.md / CLAUDE.md / .codypendent),
//! ported from codex `agents_md.rs`: walk cwd → project root, concatenate root
//! first so the most specific (cwd) file wins. Never walk past the project root.

use std::path::{Path, PathBuf};

/// Files read at each directory, in fixed precedence order (later = more specific).
pub const INSTRUCTION_FILENAMES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    ".cursorrules",
    ".clinerules",
    ".windsurfrules",
    ".github/copilot-instructions.md",
];

/// Markers that identify the project root (traversal stops at the first match).
pub const PROJECT_ROOT_MARKERS: &[&str] = &[".git", ".codypendent"];

/// Separator placed between instruction blocks.
const SEPARATOR: &str = "\n\n--- instructions ---\n\n";

/// Cap the concatenation so a stray large file cannot bloat every prompt.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;

/// Discover and concatenate instructions for a run rooted at `cwd`. Returns
/// `None` when nothing is found (so the caller leaves the system prompt as-is).
#[must_use]
pub fn discover_instructions(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let root = project_root(cwd);
    let mut out = String::new();
    // Global layer first (lowest precedence), opencode-style.
    if let Some(home) = home {
        push_file(&mut out, &home.join(".claude/CLAUDE.md"));
    }
    // Project layer: root → cwd inclusive, so cwd files land last.
    for dir in chain_root_to_cwd(&root, cwd) {
        for name in INSTRUCTION_FILENAMES {
            push_file(&mut out, &dir.join(name));
        }
        push_file(&mut out, &dir.join(".codypendent/instructions.md"));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn project_root(cwd: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        if PROJECT_ROOT_MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
    }
    cwd.to_path_buf() // no marker: only cwd is considered
}

/// Directories from project root down to cwd, inclusive, root first.
fn chain_root_to_cwd(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> = cwd
        .ancestors()
        .take_while(|d| d.starts_with(root))
        .map(Path::to_path_buf)
        .collect();
    chain.reverse(); // ancestors() is cwd→root; we want root→cwd
    chain
}

fn push_file(out: &mut String, path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    let sep_len = if out.is_empty() { 0 } else { SEPARATOR.len() };
    if out.len().saturating_add(sep_len).saturating_add(body.len()) > MAX_INSTRUCTION_BYTES {
        return;
    }
    if !out.is_empty() {
        out.push_str(SEPARATOR);
    }
    out.push_str(body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_root_to_cwd_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join(".git"), "").unwrap();
        std::fs::write(root.join("AGENTS.md"), "R").unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "S").unwrap();

        let discovered = discover_instructions(&sub, None);
        assert_eq!(
            discovered,
            Some("R\n\n--- instructions ---\n\nS".to_string())
        );
    }

    #[test]
    fn does_not_cross_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let above = temp.path().join("above");
        let root = above.join("root");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(above.join("AGENTS.md"), "ABOVE_ROOT").unwrap();
        std::fs::write(root.join(".git"), "").unwrap();
        std::fs::write(root.join("AGENTS.md"), "ROOT_RULES").unwrap();
        std::fs::write(sub.join("AGENTS.md"), "SUB_RULES").unwrap();

        let discovered = discover_instructions(&sub, None);
        assert_eq!(
            discovered,
            Some("ROOT_RULES\n\n--- instructions ---\n\nSUB_RULES".to_string())
        );
        assert!(!discovered.as_ref().unwrap().contains("ABOVE_ROOT"));
    }

    #[test]
    fn no_files_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".git"), "").unwrap();

        let discovered = discover_instructions(&root, None);
        assert_eq!(discovered, None);
    }

    #[test]
    fn respects_max_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join(".git"), "").unwrap();

        // A single file exceeding MAX_INSTRUCTION_BYTES is skipped
        let huge = "x".repeat(MAX_INSTRUCTION_BYTES + 1);
        std::fs::write(root.join("AGENTS.md"), &huge).unwrap();
        assert_eq!(discover_instructions(&root, None), None);

        // A file that fits is included, but a subsequent file that pushes over the limit is skipped
        let first = "a".repeat(40 * 1024);
        let second = "b".repeat(30 * 1024); // 40K + 30K + sep > 64K
        std::fs::write(root.join("AGENTS.md"), &first).unwrap();
        std::fs::write(sub.join("AGENTS.md"), &second).unwrap();

        let discovered = discover_instructions(&sub, None);
        assert_eq!(discovered, Some(first));
    }

    #[test]
    fn global_claude_md_is_lowest_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "GLOBAL_INSTRUCTIONS").unwrap();

        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".git"), "").unwrap();
        std::fs::write(root.join("AGENTS.md"), "PROJECT_INSTRUCTIONS").unwrap();

        let discovered = discover_instructions(&root, Some(&home));
        assert_eq!(
            discovered,
            Some("GLOBAL_INSTRUCTIONS\n\n--- instructions ---\n\nPROJECT_INSTRUCTIONS".to_string())
        );
    }

    #[test]
    fn dot_codypendent_instructions_and_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let dot_cody = root.join(".codypendent");
        std::fs::create_dir_all(&dot_cody).unwrap();
        std::fs::write(root.join(".git"), "").unwrap();
        std::fs::write(root.join("AGENTS.md"), "1_AGENTS").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "2_CLAUDE").unwrap();
        std::fs::write(dot_cody.join("instructions.md"), "3_CODYPENDENT").unwrap();

        let discovered = discover_instructions(&root, None);
        assert_eq!(
            discovered,
            Some("1_AGENTS\n\n--- instructions ---\n\n2_CLAUDE\n\n--- instructions ---\n\n3_CODYPENDENT".to_string())
        );
    }

    #[test]
    fn no_marker_only_checks_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("AGENTS.md"), "PARENT_RULES").unwrap();
        std::fs::write(child.join("AGENTS.md"), "CHILD_RULES").unwrap();

        let discovered = discover_instructions(&child, None);
        assert_eq!(discovered, Some("CHILD_RULES".to_string()));
    }
}

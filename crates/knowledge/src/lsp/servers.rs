//! The server roster: which binary serves which extensions, how its
//! workspace root is found, and how it is spawned. rust-analyzer and
//! pyright first (this adoption); the roster is data, additions are rows.

use super::client::canonical_or_original;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub fn rust_analyzer_root(file: &Path, worktree: &Path) -> Option<PathBuf> {
    let file_canon = canonical_or_original(file);
    let worktree_canon = canonical_or_original(worktree);

    if !file_canon.starts_with(&worktree_canon) {
        return None;
    }

    let start_dir = if file_canon.is_dir() {
        file_canon.clone()
    } else {
        file_canon.parent()?.to_path_buf()
    };

    let mut current = start_dir;
    let mut crate_root: Option<PathBuf> = None;

    loop {
        if !current.starts_with(&worktree_canon) {
            break;
        }

        let cargo_toml = current.join("Cargo.toml");
        let cargo_lock = current.join("Cargo.lock");

        if cargo_toml.exists() || cargo_lock.exists() {
            if crate_root.is_none() {
                crate_root = Some(current.clone());
            }

            if cargo_toml.exists() {
                if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                    if content.contains("[workspace]") {
                        return Some(current);
                    }
                }
            }
        }

        if current == worktree_canon {
            break;
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    crate_root
}

/// pyright root: nearest ancestor holding any of pyproject.toml, setup.py,
/// setup.cfg, requirements.txt, Pipfile, pyrightconfig.json; else `worktree`.
pub fn pyright_root(file: &Path, worktree: &Path) -> Option<PathBuf> {
    let file_canon = canonical_or_original(file);
    let worktree_canon = canonical_or_original(worktree);

    if !file_canon.starts_with(&worktree_canon) {
        return None;
    }

    let start_dir = if file_canon.is_dir() {
        file_canon.clone()
    } else {
        file_canon.parent()?.to_path_buf()
    };

    let markers = [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "Pipfile",
        "pyrightconfig.json",
    ];

    let mut current = start_dir;
    loop {
        if !current.starts_with(&worktree_canon) {
            break;
        }

        for marker in &markers {
            if current.join(marker).exists() {
                return Some(current);
            }
        }

        if current == worktree_canon {
            break;
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    Some(worktree_canon)
}

/// pyright initialization options: `{"pythonPath": …}` resolved from
/// $VIRTUAL_ENV, `<root>/.venv`, `<root>/venv` (`bin/python`, or
/// `Scripts/python.exe` on Windows) — first that exists wins; `{}` when
/// none (server.ts line 500). rust-analyzer takes `{}`.
pub fn pyright_initialization(root: &Path) -> serde_json::Value {
    let root_canon = canonical_or_original(root);

    // 1. $VIRTUAL_ENV
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let venv_path = PathBuf::from(venv);
        if let Some(bin) = find_python_in_venv(&venv_path) {
            return serde_json::json!({ "pythonPath": bin.to_string_lossy() });
        }
    }

    // 2. <root>/.venv
    let dot_venv = root_canon.join(".venv");
    if let Some(bin) = find_python_in_venv(&dot_venv) {
        return serde_json::json!({ "pythonPath": bin.to_string_lossy() });
    }

    // 3. <root>/venv
    let venv = root_canon.join("venv");
    if let Some(bin) = find_python_in_venv(&venv) {
        return serde_json::json!({ "pythonPath": bin.to_string_lossy() });
    }

    serde_json::json!({})
}

fn find_python_in_venv(venv_root: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates = [
        venv_root.join("Scripts").join("python.exe"),
        venv_root.join("bin").join("python.exe"),
    ];

    #[cfg(not(windows))]
    let candidates = [
        venv_root.join("bin").join("python"),
        venv_root.join("bin").join("python3"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }
    None
}

/// pyright is spawned with `--stdio`; rust-analyzer with no args.
pub fn spawn_args(spec: &ServerSpec) -> &'static [&'static str] {
    if spec.id == "pyright" {
        &["--stdio"]
    } else {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_analyzer_root_prefers_workspace_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_root = tmp.path().join("workspace");
        let crate_a = ws_root.join("crates").join("crate_a");
        std::fs::create_dir_all(&crate_a).unwrap();

        std::fs::write(
            ws_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        std::fs::write(
            crate_a.join("Cargo.toml"),
            "[package]\nname = \"crate_a\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let src_file = crate_a.join("src").join("lib.rs");
        std::fs::create_dir_all(src_file.parent().unwrap()).unwrap();
        std::fs::write(&src_file, "pub fn foo() {}\n").unwrap();

        let root = rust_analyzer_root(&src_file, &ws_root).unwrap();
        assert_eq!(
            canonical_or_original(&root),
            canonical_or_original(&ws_root)
        );
    }

    #[test]
    fn rust_analyzer_root_stops_at_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent_ws");
        let worktree = parent.join("nested_worktree");
        let src_dir = worktree.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Workspace above the worktree boundary
        std::fs::write(
            parent.join("Cargo.toml"),
            "[workspace]\nmembers = [\"nested_worktree\"]\n",
        )
        .unwrap();

        let src_file = src_dir.join("main.rs");
        std::fs::write(&src_file, "fn main() {}\n").unwrap();

        // Without a Cargo.toml in worktree, searching bounded by worktree returns None
        let root = rust_analyzer_root(&src_file, &worktree);
        assert_eq!(root, None);

        // With a Cargo.toml in worktree, searching returns worktree
        std::fs::write(
            worktree.join("Cargo.toml"),
            "[package]\nname = \"sub\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let root = rust_analyzer_root(&src_file, &worktree).unwrap();
        assert_eq!(
            canonical_or_original(&root),
            canonical_or_original(&worktree)
        );
    }

    #[test]
    fn pyright_root_matches_all_markers() {
        let markers = [
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
            "pyrightconfig.json",
        ];

        for marker in markers {
            let tmp = tempfile::tempdir().unwrap();
            let sub = tmp.path().join("project");
            let inner = sub.join("pkg");
            std::fs::create_dir_all(&inner).unwrap();

            std::fs::write(sub.join(marker), "# marker\n").unwrap();
            let py_file = inner.join("app.py");
            std::fs::write(&py_file, "print('hi')\n").unwrap();

            let root = pyright_root(&py_file, tmp.path()).unwrap();
            assert_eq!(canonical_or_original(&root), canonical_or_original(&sub));
        }
    }

    #[test]
    fn pyright_initialization_resolves_venv_python() {
        let tmp = tempfile::tempdir().unwrap();
        let venv_bin = tmp.path().join(".venv").join("bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        let python_bin = venv_bin.join("python");
        std::fs::write(&python_bin, "#!/bin/sh\n").unwrap();

        let opts = pyright_initialization(tmp.path());
        assert_eq!(
            opts.get("pythonPath").and_then(serde_json::Value::as_str),
            Some(canonical_or_original(&python_bin).to_str().unwrap())
        );
    }
}

use std::sync::Arc;

use codypendent_knowledge::adapter::on_path;
use codypendent_knowledge::lsp::{LiveDiagnostics, LspManager};

#[tokio::test]
async fn rust_analyzer_reports_type_error_after_edit() {
    if !on_path("rust-analyzer") {
        eprintln!("skipping: rust-analyzer not installed");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().to_path_buf();
    let src_dir = ws_root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    std::fs::write(
        ws_root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    let main_rs = src_dir.join("main.rs");
    std::fs::write(&main_rs, "fn main() { let x: u32 = \"type error\"; }\n").unwrap();

    let manager = Arc::new(LspManager::new());
    let mut diags = Vec::new();
    for _ in 0..15 {
        diags = manager.file_diagnostics(&main_rs, &ws_root).await;
        if !diags.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    assert!(
        !diags.is_empty(),
        "rust-analyzer should report the type error in main.rs"
    );
    assert!(diags.iter().any(|d| d.message.contains("mismatched types")
        || d.message.contains("expected")
        || d.message.contains("&str")));
}

#[tokio::test]
async fn clean_file_reports_no_errors() {
    if !on_path("rust-analyzer") {
        eprintln!("skipping: rust-analyzer not installed");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().to_path_buf();
    let src_dir = ws_root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    std::fs::write(
        ws_root.join("Cargo.toml"),
        "[package]\nname = \"fixture_clean\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    let main_rs = src_dir.join("main.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();

    let manager = Arc::new(LspManager::new());
    let diags = manager.file_diagnostics(&main_rs, &ws_root).await;

    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == codypendent_knowledge::DiagnosticSeverity::Error)
        .collect();
    assert!(errors.is_empty(), "clean file should report zero errors");
}

#[tokio::test]
async fn pyright_reports_type_error_after_edit() {
    if !on_path("pyright-langserver") {
        eprintln!("skipping: pyright-langserver not installed");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().to_path_buf();
    std::fs::write(ws_root.join("pyproject.toml"), "# config\n").unwrap();

    let app_py = ws_root.join("app.py");
    std::fs::write(&app_py, "x: int = 'bad type'\n").unwrap();

    let manager = Arc::new(LspManager::new());
    let diags = manager.file_diagnostics(&app_py, &ws_root).await;

    assert!(
        !diags.is_empty(),
        "pyright should report the type error in app.py"
    );
}

#[tokio::test]
async fn clean_python_file_reports_no_errors() {
    if !on_path("pyright-langserver") {
        eprintln!("skipping: pyright-langserver not installed");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().to_path_buf();
    std::fs::write(ws_root.join("pyproject.toml"), "# config\n").unwrap();

    let app_py = ws_root.join("app.py");
    std::fs::write(&app_py, "def main() -> int:\n    return 42\n").unwrap();

    let manager = Arc::new(LspManager::new());
    let diags = manager.file_diagnostics(&app_py, &ws_root).await;

    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == codypendent_knowledge::DiagnosticSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "clean python file should report zero errors"
    );
}

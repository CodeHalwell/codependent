use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use codypendent_daemon::policy::{CommandScope, PathScope};
use codypendent_daemon::unified_exec::{ReadBudget, UnifiedExecManager};
use codypendent_protocol::{RunId, SessionId};
use codypendent_runtime::tools::{
    CommandRequest, EnvironmentBinding, ShellExec, ShellWriteStdin, ToolError,
};

fn canon_scope(root: &Path) -> PathScope {
    PathScope::new(vec![std::fs::canonicalize(root).unwrap()], vec![])
}

fn cmd_scope(programs: &[&str]) -> CommandScope {
    CommandScope {
        allowed_programs: programs.iter().map(|s| s.to_string()).collect(),
        maximum_seconds: 0,
    }
}

#[tokio::test]
async fn unified_exec_early_exit_returns_output_without_process_id() {
    let tmp = tempfile::tempdir().unwrap();
    let path_scope = canon_scope(tmp.path());
    let command_scope = cmd_scope(&["echo", "/bin/echo"]);
    let manager = Arc::new(UnifiedExecManager::new());
    manager.set_deterministic_process_ids_for_tests(true);

    let session_id = SessionId::new();
    let run_id = RunId::new();

    let request = CommandRequest {
        program: PathBuf::from("echo"),
        args: vec!["hello from pty".to_string()],
        cwd: tmp.path().to_path_buf(),
        environment: Vec::new(),
        timeout: Duration::from_secs(10),
    };

    let outcome = ShellExec::execute(
        &request,
        ReadBudget::default(),
        &path_scope,
        &command_scope,
        &manager,
        session_id,
        run_id,
    )
    .await
    .unwrap();

    assert!(outcome.process_id.is_none());
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.output.contains("hello from pty"));
}

#[tokio::test]
async fn unified_exec_interactive_session_and_write_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let path_scope = canon_scope(tmp.path());
    let command_scope = cmd_scope(&["cat", "/bin/cat"]);
    let manager = Arc::new(UnifiedExecManager::new());
    manager.set_deterministic_process_ids_for_tests(true);

    let session_id = SessionId::new();
    let run_id = RunId::new();

    let request = CommandRequest {
        program: PathBuf::from("cat"),
        args: Vec::new(),
        cwd: tmp.path().to_path_buf(),
        environment: Vec::new(),
        timeout: Duration::from_secs(10),
    };

    let exec_out = ShellExec::execute(
        &request,
        ReadBudget {
            yield_time_ms: 250,
            max_output_tokens: 1000,
        },
        &path_scope,
        &command_scope,
        &manager,
        session_id,
        run_id,
    )
    .await
    .unwrap();

    let pid = exec_out
        .process_id
        .expect("cat should stay alive on interactive PTY");
    assert_eq!(exec_out.exit_code, None);

    // Write line to cat
    let write_out = ShellWriteStdin::execute(
        pid,
        "roundtrip test\n",
        ReadBudget {
            yield_time_ms: 300,
            max_output_tokens: 1000,
        },
        &manager,
        session_id,
    )
    .await
    .unwrap();

    assert!(write_out.output.contains("roundtrip test"));
    assert_eq!(write_out.process_id, Some(pid));

    // Send Ctrl+C
    let interrupt_out = ShellWriteStdin::execute(
        pid,
        "\u{0003}",
        ReadBudget {
            yield_time_ms: 300,
            max_output_tokens: 1000,
        },
        &manager,
        session_id,
    )
    .await
    .unwrap();

    // Process is killed / exited
    assert!(interrupt_out.process_id.is_none());
}

#[tokio::test]
async fn unified_exec_policy_enforcement() {
    let tmp = tempfile::tempdir().unwrap();
    let path_scope = canon_scope(tmp.path());
    let command_scope = cmd_scope(&["echo"]);
    let manager = Arc::new(UnifiedExecManager::new());

    let session_id = SessionId::new();
    let run_id = RunId::new();

    // 1. Denied program
    let denied_req = CommandRequest {
        program: PathBuf::from("rm"),
        args: vec!["-rf".to_string(), "/".to_string()],
        cwd: tmp.path().to_path_buf(),
        environment: Vec::new(),
        timeout: Duration::from_secs(5),
    };
    let err = ShellExec::execute(
        &denied_req,
        ReadBudget::default(),
        &path_scope,
        &command_scope,
        &manager,
        session_id,
        run_id,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::ProgramNotAllowed(_)));

    // 2. Denied environment variable
    let denied_env_req = CommandRequest {
        program: PathBuf::from("echo"),
        args: vec!["hi".to_string()],
        cwd: tmp.path().to_path_buf(),
        environment: vec![EnvironmentBinding::new("LD_PRELOAD", "/evil.so")],
        timeout: Duration::from_secs(5),
    };
    let err = ShellExec::execute(
        &denied_env_req,
        ReadBudget::default(),
        &path_scope,
        &command_scope,
        &manager,
        session_id,
        run_id,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::EnvironmentNotAllowed(_)));

    // 3. Out of scope cwd
    let outside_cwd = tempfile::tempdir().unwrap();
    let out_of_scope_req = CommandRequest {
        program: PathBuf::from("echo"),
        args: vec!["hi".to_string()],
        cwd: outside_cwd.path().to_path_buf(),
        environment: Vec::new(),
        timeout: Duration::from_secs(5),
    };
    let err = ShellExec::execute(
        &out_of_scope_req,
        ReadBudget::default(),
        &path_scope,
        &command_scope,
        &manager,
        session_id,
        run_id,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::CwdOutOfScope(_)));
}

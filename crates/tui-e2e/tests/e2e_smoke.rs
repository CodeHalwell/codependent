//! Smoke test for the PTY TUI session runner (Adoption 12 A2).

use std::time::Duration;

use codypendent_tui_e2e::TuiSession;

#[test]
fn pty_session_launches_and_captures_output() {
    let session = TuiSession::launch("/bin/sh", &["-c", "echo 'Hello from PTY'"], 80, 24)
        .expect("launch PTY session");

    session
        .wait_for("Hello from PTY", Duration::from_secs(3))
        .expect("wait for output");

    session.assert_absent("NonExistentString12345");
    let snapshot = session.snapshot();
    assert!(snapshot.contains("Hello from PTY"));
}

#[test]
fn pty_session_handles_interactive_input() {
    let mut session = TuiSession::launch("/bin/sh", &[], 80, 24).expect("launch interactive shell");

    // Write a command and press Enter
    session
        .type_str("echo 'interactive-test'\n")
        .expect("type string");
    session
        .wait_for("interactive-test", Duration::from_secs(3))
        .expect("wait for interactive output");

    let snapshot = session.snapshot();
    assert!(snapshot.contains("interactive-test"));
}

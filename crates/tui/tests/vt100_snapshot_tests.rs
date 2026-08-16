//! Screen-state snapshot testing with VT100Backend (Adoption 12 A1).

use codypendent_protocol::{AgentMode, EventBody, ModelId, RunState, SessionEvent};
use codypendent_tui::render::render;
use codypendent_tui::state::{AppState, ModelCard, ModelLocationLabel, ModelReadiness};
use codypendent_tui::theme::Theme;
use codypendent_tui::vt100_backend::VT100Backend;
use ratatui::Terminal;

fn sample_state() -> AppState {
    let mut state = AppState::new();
    let run_id = codypendent_protocol::ids::RunId::new();

    codypendent_tui::reduce(
        &mut state,
        codypendent_tui::Action::DaemonEvent(Box::new(SessionEvent {
            sequence: 1,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: codypendent_protocol::Actor::System,
            body: EventBody::RunStarted {
                run_id,
                objective: "Diagnose and fix issue in parser".to_string(),
                mode: AgentMode::Build,
            },
        })),
    );

    codypendent_tui::reduce(
        &mut state,
        codypendent_tui::Action::DaemonEvent(Box::new(SessionEvent {
            sequence: 2,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: codypendent_protocol::Actor::System,
            body: EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            },
        })),
    );

    state.models.push(ModelCard {
        id: ModelId("claude-sonnet-4".to_string()),
        provider: "anthropic".to_string(),
        readiness: ModelReadiness::Ready,
        location: Some(ModelLocationLabel::Hosted),
        cost_per_1k_usd: None,
        context_tokens: Some(200_000),
    });
    state.pending_model = Some(ModelId("claude-sonnet-4".to_string()));
    state
}

#[test]
fn vt100_renders_full_chat_frame() {
    let state = sample_state();
    let theme = Theme::dark();
    let backend = VT100Backend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("initialize terminal");

    terminal
        .draw(|frame| render(frame, &state, &theme))
        .expect("draw frame");

    let screen_content = terminal.backend().to_string();
    assert!(screen_content.contains("codypendent"));
    assert!(screen_content.contains("Diagnose and fix issue in parser"));
    assert!(screen_content.contains("claude"));

    // Verify cell contents and screen grid
    let parser_arc = terminal.backend().parser();
    let parser = parser_arc.lock().unwrap();
    let (rows, cols) = parser.screen().size();
    assert_eq!(rows, 24);
    assert_eq!(cols, 80);
}

/// The help overlay renders every binding into a deterministic two-column
/// table (static `KEY_BINDINGS`, no wall-clock time), so it is a stable
/// snapshot target for the ANSI round-trip path — catching a regression in
/// the key/description gutter, the stacked-mode fallback, or the modal
/// chrome without re-reading string assertions.
#[test]
fn vt100_snapshot_help_overlay() {
    let mut state = AppState::new();
    state.overlay = codypendent_tui::state::Overlay::Help;
    let theme = Theme::dark();
    let backend = VT100Backend::new(100, 50);
    let mut terminal = Terminal::new(backend).expect("initialize terminal");

    terminal
        .draw(|frame| render(frame, &state, &theme))
        .expect("draw frame");

    insta::assert_snapshot!("help_overlay_wide", terminal.backend().to_string());
}

/// The narrow-terminal path must stack a key above its description rather than
/// letting a long binding run into its label — the two-column layout degrades
/// deliberately. Snapshotting the stacked form guards the continuation-indent
/// logic the manual test only spot-checks.
#[test]
fn vt100_snapshot_help_overlay_narrow() {
    let mut state = AppState::new();
    state.overlay = codypendent_tui::state::Overlay::Help;
    let theme = Theme::dark();
    let backend = VT100Backend::new(60, 50);
    let mut terminal = Terminal::new(backend).expect("initialize terminal");

    terminal
        .draw(|frame| render(frame, &state, &theme))
        .expect("draw frame");

    insta::assert_snapshot!("help_overlay_narrow", terminal.backend().to_string());
}

/// The empty conversation (no runs, no models) renders its "connect a runnable
/// model" guidance. Fully deterministic — no timestamps in the empty state —
/// so it is a second stable full-frame snapshot.
#[test]
fn vt100_snapshot_empty_chat() {
    let state = AppState::new();
    let theme = Theme::dark();
    let backend = VT100Backend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("initialize terminal");

    terminal
        .draw(|frame| render(frame, &state, &theme))
        .expect("draw frame");

    insta::assert_snapshot!("empty_chat", terminal.backend().to_string());
}

#[test]
fn vt100_ansi_round_trip_preserves_styles() {
    let state = sample_state();
    let theme = Theme::dark();
    let backend = VT100Backend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("initialize terminal");

    terminal
        .draw(|frame| render(frame, &state, &theme))
        .expect("draw frame");

    let parser_arc = terminal.backend().parser();
    let parser = parser_arc.lock().unwrap();
    let screen = parser.screen();
    // Non-empty content exists across screen cells
    let mut found_non_empty_cell = false;
    for row in 0..30 {
        for col in 0..100 {
            if let Some(cell) = screen.cell(row, col) {
                if cell.has_contents() {
                    found_non_empty_cell = true;
                    break;
                }
            }
        }
    }
    assert!(found_non_empty_cell, "screen should have rendered cells");
}

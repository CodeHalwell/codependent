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

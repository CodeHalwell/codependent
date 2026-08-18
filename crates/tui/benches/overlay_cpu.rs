//! Fixed-iteration CPU-time harness for the list-overlay render paths.
//!
//! # Why this exists alongside the criterion bench
//!
//! `frame.rs` measures WALL time, which is the honest metric for "does the UI
//! feel fast" — but it is only trustworthy on a quiet machine. This repo's
//! machine is routinely shared with other agents' builds and benches; at load
//! 148 a wall-clock A/B swings by 5x between passes and says nothing.
//!
//! CPU time for a FIXED amount of work does not have that problem: an
//! oversubscribed scheduler inflates wall time while user CPU stays put. So
//! this target does no adaptive sampling and no statistics — it runs exactly
//! `iters` draws of one named scenario and exits, and the caller reads user CPU
//! off `/usr/bin/time`. Same work, same binary, two revisions of `render.rs`:
//! the difference in user CPU is the difference in work done.
//!
//! Not a replacement for `frame.rs` — it cannot tell you a frame budget, only a
//! ratio. Use it when the machine is too loaded for criterion to be believed.
//!
//! Usage: `overlay_cpu <scenario> <rows> <iters>`

use chrono::{DateTime, TimeZone, Utc};
use codypendent_protocol::events::{Actor, EventBody, SessionEvent};
use codypendent_protocol::ids::RunId;
use codypendent_protocol::run::AgentMode;
use codypendent_tui::{
    reduce, render, Action, AppState, LayoutMode, LearningCard, Overlay, ProviderCard, Theme,
};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::hint::black_box;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

fn fixed_time() -> DateTime<Utc> {
    Utc.timestamp_opt(1_765_000_000, 0).single().expect("fixed")
}

fn run_id(i: usize) -> RunId {
    RunId(uuid::Uuid::from_u128(
        0x5eed_0000_0000_0000_0000_0000_0000_0000 + i as u128,
    ))
}

fn ev(body: EventBody) -> Action {
    Action::daemon_event(SessionEvent {
        sequence: 1,
        occurred_at: fixed_time(),
        causation_id: None,
        correlation_id: None,
        actor: Actor::System,
        body,
    })
}

/// A deliberately minimal base: this target isolates the OVERLAY, so the
/// transcript underneath is kept small enough to be a rounding error.
fn base(runs: usize) -> AppState {
    let mut state = AppState::new();
    for i in 0..runs {
        reduce(
            &mut state,
            ev(EventBody::RunStarted {
                run_id: run_id(i),
                objective: format!("turn {i}: 設定ファイルの検証 — tighten the projection path"),
                mode: AgentMode::Build,
            }),
        );
    }
    state
}

/// Multi-byte, so the per-row `truncate_display_width` grapheme walk has real
/// work rather than an ASCII fast path.
fn issue(i: usize) -> String {
    format!("{i}: プロバイダの資格情報が見つかりません — set CODYPENDENT_API_KEY 🔑")
}

fn learning(i: usize) -> LearningCard {
    LearningCard {
        id: format!("learning-{i}"),
        statement: format!("{i}: 設定ファイルの検証を先に行う — verified across 3 runs 🎉"),
        kind: "preference".to_owned(),
        state: "active".to_owned(),
        scope: "workspace".to_owned(),
        provenance: format!("run {i} · verified outcome"),
        confidence: 0.87,
        pinned: i.is_multiple_of(7),
        revision: 3,
    }
}

/// A provider row's three lines all run through `truncate_display_width` /
/// `picker_sub_line`, so multi-byte fields here are what make the per-row cost
/// real rather than an ASCII fast path.
fn provider(i: usize) -> ProviderCard {
    ProviderCard {
        id: format!("provider-{i}"),
        name: format!("{i}: プロバイダ — Hosted Inference 🔑"),
        protocol: "openai-chat".to_owned(),
        auth: format!("api-key: CODYPENDENT_KEY_{i}"),
        local: i.is_multiple_of(3),
        requires_key: true,
        can_list_models: true,
        available: !i.is_multiple_of(5),
        catalog_models: i % 11,
        has_key: i.is_multiple_of(2),
    }
}

fn scenario(name: &str, rows: usize) -> AppState {
    match name {
        "issues" => {
            let mut s = base(4);
            s.issues = (0..rows).map(issue).collect();
            s.overlay = Overlay::Issues;
            s
        }
        "journey" => {
            let mut s = base(4);
            s.learnings = (0..rows).map(learning).collect();
            s.overlay = Overlay::Journey;
            s
        }
        // The provider catalog: three lines per row, each built with a
        // display-width truncate. Opened at the top (`selected: 0`), which is
        // how the picker actually opens.
        "providers" => {
            let mut s = base(4);
            s.providers = (0..rows).map(provider).collect();
            s.overlay = Overlay::ProviderPicker {
                query: String::new(),
                selected: 0,
            };
            s
        }
        // No overlay: `rows` is the RUN count, and the runs pane formats a row
        // per run on every frame in this layout.
        "workspace" => {
            let mut s = base(rows);
            s.layout = LayoutMode::Workspace;
            s
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(
        args.len() >= 4,
        "usage: overlay_cpu <scenario> <rows> <iters>"
    );
    let rows: usize = args[2].parse().expect("rows");
    let iters: usize = args[3].parse().expect("iters");

    let theme = Theme::dark();
    let state = scenario(&args[1], rows);
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");

    // Warm every render-time memo, so the timed loop is steady state.
    terminal
        .draw(|f| render(f, &state, &theme))
        .expect("warm draw");

    for _ in 0..iters {
        terminal
            .draw(|f| render(f, black_box(&state), black_box(&theme)))
            .expect("draw");
    }
}

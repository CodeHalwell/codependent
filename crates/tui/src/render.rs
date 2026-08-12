//! Rendering (STEP 1.12 RULE 4/5, and RULE 7 no hard-coded colors).
//!
//! Every function here is a pure projection of [`AppState`] onto a `ratatui`
//! frame. Widgets read colors exclusively from the [`Theme`] tokens — there is
//! not one literal color in this module. No function performs I/O; the render
//! thread only ever draws (RULE 2).
//!
//! Layout: a restrained project header, an unboxed conversation timeline, an
//! inline composer, and one contextual status row. Secondary inspectors and
//! approvals draw on top only while requested.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use codypendent_protocol::{
    AgentMode, BudgetDimension, ProposedAction, Risk, RiskLevel, RunDisposition, RunState, BUILD_ID,
};

use crate::action::{Action, KeyTarget};
use crate::reduce::capability_label;
use crate::remote_ui_host::{TERMINAL_CENTRAL_SLOTS, TERMINAL_OVERLAY_SLOTS};
use crate::state::{
    filter_council_member_models, filter_key_rows, filter_model_names, filter_models, filter_modes,
    filter_providers, AddModelRow, AppState, CouncilBuilderState, CouncilBuilderStep, DocFocus,
    DocLeaseState, KeyStatus, LayoutMode, ModelCard, ModelListOrigin, ModelLocationLabel,
    ModelReadiness, Overlay, Pane, PatchSummary, ProviderCard, RunActivity, RunView, ToolCard,
    ToolStatus, TranscriptEntry,
};
use crate::theme::Theme;
use crate::{render_remote_ui, RemoteUiRenderOptions};

/// Draw the whole UI for the current frame.
pub fn render(frame: &mut Frame, state: &AppState, theme: &Theme) {
    let area = frame.area();
    // Rebuilt fresh every frame (mirrors `transcript_max_scroll`): a stale hit
    // from a previous layout must never survive to resolve this frame's clicks.
    state.hit_map.borrow_mut().clear();
    state.remote_ui.last_render.borrow_mut().clear();
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.surface.background)),
        area,
    );

    // A conversation-centred shell: one calm project header, the transcript,
    // an inline composer, and one contextual footer. Secondary surfaces remain
    // overlays, so the primary experience reads like a coding conversation
    // rather than a dashboard of permanent controls.
    // The box grows past its 3-row minimum when the draft holds more than one
    // line (a manual `Alt+Enter` break, or a multi-line paste), capped at
    // `COMPOSER_MAX_HEIGHT` — see `composer_box_height`.
    let composer_height = composer_box_height(&state.composer, area.width);
    let has_composer_accessory = !state
        .remote_ui
        .mounted_documents_for_points(&["composer-accessory"])
        .is_empty();
    let has_status_items = !state
        .remote_ui
        .mounted_documents_for_points(&["status-item"])
        .is_empty();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // project header
            Constraint::Min(3),    // conversation transcript
            Constraint::Length(if has_composer_accessory { 3 } else { 0 }),
            Constraint::Length(composer_height), // inline composer
            Constraint::Length(if has_status_items { 4 } else { 1 }),
        ])
        .split(area);

    render_header(frame, rows[0], state, theme);
    // The region between header and composer depends on the layout; the
    // header, composer, and contextual footer are identical in both.
    if !render_remote_surfaces(frame, rows[1], state, theme) {
        match state.layout {
            LayoutMode::Chat => render_conversation(frame, rows[1], state, theme),
            LayoutMode::Workspace => render_workspace(frame, rows[1], state, theme),
        }
    }
    if has_composer_accessory {
        let documents = state
            .remote_ui
            .mounted_documents_for_points(&["composer-accessory"]);
        render_remote_documents(frame, rows[2], state, theme, documents);
    }
    render_composer(frame, rows[3], state, theme);
    render_status_slot(frame, rows[4], state, theme);

    render_remote_overlays(frame, area, state, theme);

    render_overlays(frame, area, state, theme);
}

fn render_remote_surfaces(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) -> bool {
    // Central contributions are focusable alternate content, not passive
    // decoration. Keep the native conversation visible until the operator
    // explicitly enters Remote UI focus (F6).
    if !central_remote_ui_is_active(state) {
        return false;
    }
    let documents = state
        .remote_ui
        .mounted_documents_for_points(TERMINAL_CENTRAL_SLOTS);
    if documents.is_empty() || area.width == 0 || area.height == 0 {
        return false;
    }
    render_remote_documents(frame, area, state, theme, documents);
    true
}

fn central_remote_ui_is_active(state: &AppState) -> bool {
    state.remote_ui.active
        && state
            .remote_ui
            .focused_document
            .as_ref()
            .and_then(|document_id| {
                state
                    .remote_ui
                    .host
                    .registry()
                    .registration_for_document(document_id.as_str())
            })
            .is_some_and(|registration| {
                TERMINAL_CENTRAL_SLOTS.contains(&registration.point.as_str())
            })
}

fn render_remote_documents(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    documents: Vec<&codypendent_protocol::UiDocument>,
) {
    if documents.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let count = u16::try_from(documents.len())
        .unwrap_or(u16::MAX)
        .min(area.height.max(1));
    let constraints = (0..count)
        .map(|_| Constraint::Ratio(1, u32::from(count)))
        .collect::<Vec<_>>();
    let regions = Layout::vertical(constraints).split(area);
    for (document, region) in documents.into_iter().zip(regions.iter().copied()) {
        let (extension, publisher, trust) = state
            .remote_ui
            .extension_identity_for_document(&document.document_id)
            .unwrap_or(("unknown extension", None, None));
        let identity = match (publisher, trust) {
            (Some(publisher), Some(trust)) => format!("{extension} · {publisher} · {trust}"),
            (Some(publisher), None) => format!("{extension} · {publisher}"),
            (None, Some(trust)) => format!("{extension} · {trust}"),
            (None, None) => format!("{extension} · sandboxed"),
        };
        let point = state
            .remote_ui
            .host
            .registry()
            .registration_for_document(document.document_id.as_str())
            .map(|registration| registration.point.as_str())
            .unwrap_or("panel");
        let chrome = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(
                if state.remote_ui.active
                    && state.remote_ui.focused_document.as_ref() == Some(&document.document_id)
                {
                    theme.focus.active
                } else {
                    theme.surface.border
                },
            ))
            .title(Span::styled(
                format!(
                    " {point} · Extension: {identity} · {} ",
                    if state.remote_ui.active
                        && state.remote_ui.focused_document.as_ref() == Some(&document.document_id)
                    {
                        "focused · Tab controls · Shift-F6 next · Esc return"
                    } else {
                        "F6 focus"
                    }
                ),
                Style::default()
                    .fg(theme.text.muted)
                    .add_modifier(Modifier::BOLD),
            ));
        let content_region = chrome.inner(region);
        frame.render_widget(chrome, region);
        // Host-owned, non-activating document focus target. Component hit
        // regions are registered below and therefore win the reverse-order hit
        // test when the pointer is over an actual control.
        state.register_hit(
            region,
            Action::RemoteUiFocusDocument(document.document_id.clone()),
        );
        let output = render_remote_ui(
            frame.buffer_mut(),
            content_region,
            document,
            theme,
            &state.remote_ui.capabilities,
            &state.remote_ui.view,
            RemoteUiRenderOptions::default(),
        );
        for hit in &output.hit_regions {
            state.hit_map.borrow_mut().push((
                hit.area,
                Action::RemoteUiActivate {
                    document_id: document.document_id.clone(),
                    revision: document.revision,
                    target_id: hit.node_id.clone(),
                    binding: Box::new(hit.binding.clone()),
                },
            ));
        }
        state
            .remote_ui
            .last_render
            .borrow_mut()
            .insert(document.document_id.clone(), output);
    }
}

fn render_status_slot(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let documents = state
        .remote_ui
        .mounted_documents_for_points(&["status-item"]);
    if documents.is_empty() || area.width < 24 || area.height < 4 {
        render_status_line(frame, area, state, theme);
        return;
    }
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    render_status_line(frame, rows[0], state, theme);
    render_remote_documents(frame, rows[1], state, theme, documents);
}

fn render_remote_overlays(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let documents = state
        .remote_ui
        .mounted_documents_for_points(TERMINAL_OVERLAY_SLOTS);
    if documents.is_empty() || area.width < 20 || area.height < 5 {
        return;
    }
    let width = area.width.min(48);
    let height = area.height.saturating_sub(2).min(
        u16::try_from(documents.len())
            .unwrap_or(u16::MAX)
            .saturating_mul(5)
            .max(5),
    );
    let region = Rect::new(
        area.right().saturating_sub(width),
        area.y.saturating_add(1),
        width,
        height,
    );
    frame.render_widget(Clear, region);
    render_remote_documents(frame, region, state, theme, documents);
}

/// The one-row project header. Brand and conversation identity live on the
/// left; the active model/mode and genuinely-known usage live on the right.
/// Build ids and diagnostics stay in their dedicated surfaces instead of
/// competing with the task on every frame.
fn render_header(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let status = state.status();
    let mid = area.width >= 64;
    let title = state
        .session_title
        .as_deref()
        .filter(|title| !title.eq_ignore_ascii_case("codypendent"))
        .map(|title| truncate(title, 30));
    let model = status.model.as_ref().map(|model| truncate(&model.0, 22));
    let mut show_title = title.is_some();
    let mut show_model = mid && model.is_some();
    let mut show_mode = mid;
    let mut show_context = status.context_percent.is_some();
    let mut show_cost = status.cost_minor.is_some();

    // Pack by semantic priority rather than relying on saturating padding,
    // which merely lets the left group overwrite the right. The brand always
    // survives; lower-value telemetry progressively drops on narrow screens.
    let (left, right) = loop {
        let mut left: Vec<Span<'static>> = vec![
            Span::raw("  "),
            Span::styled("✦", Style::default().fg(theme.focus.active)),
            Span::styled(
                " codypendent",
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if show_title {
            left.push(Span::styled("  /  ", Style::default().fg(theme.text.muted)));
            left.push(Span::styled(
                title.clone().unwrap_or_default(),
                Style::default().fg(theme.text.secondary),
            ));
        }

        let mut groups: Vec<Vec<Span<'static>>> = Vec::new();
        if show_model {
            groups.push(vec![Span::styled(
                model.clone().unwrap_or_default(),
                Style::default().fg(theme.text.secondary),
            )]);
        }
        if show_mode {
            groups.push(vec![Span::styled(
                mode_label(state.default_mode).to_owned(),
                Style::default()
                    .fg(theme.focus.active)
                    .add_modifier(Modifier::BOLD),
            )]);
        }
        if show_context {
            groups.push(vec![Span::styled(
                format!("ctx {}%", status.context_percent.unwrap_or_default()),
                Style::default().fg(theme.text.muted),
            )]);
        }
        if show_cost {
            groups.push(vec![Span::styled(
                format_cost(status.cost_minor),
                Style::default().fg(theme.status.warning),
            )]);
        }
        let mut right = Vec::new();
        for (index, group) in groups.into_iter().enumerate() {
            if index > 0 {
                right.push(Span::styled(" · ", Style::default().fg(theme.text.muted)));
            }
            right.extend(group);
        }
        let used = left.iter().map(Span::width).sum::<usize>()
            + right.iter().map(Span::width).sum::<usize>()
            + 3;
        if used <= usize::from(area.width) {
            break (left, right);
        }
        if show_cost {
            show_cost = false;
        } else if show_context {
            show_context = false;
        } else if show_model {
            show_model = false;
        } else if show_title {
            show_title = false;
        } else if show_mode {
            show_mode = false;
        } else {
            break (left, right);
        }
    };

    let left_width: usize = left.iter().map(|span| span.width()).sum();
    let right_width: usize = right.iter().map(|span| span.width()).sum();
    let pad = usize::from(area.width).saturating_sub(left_width + right_width + 2);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    spans.push(Span::raw("  "));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.surface.panel)),
        area,
    );
}

/// The startup welcome, drawn by the CLI harness on the same alternate screen
/// while boot proceeds and held after boot until the user presses Enter. It
/// combines the `CODYPENDENT` block-letter wordmark, a short product promise,
/// the build id, and either the current animated boot stage or a clear ready
/// call-to-action. `warnings` carries the boot
/// diagnostics collected so far (reconcile warnings, best-effort loader
/// failures) — rendered as extra lines UNDER the stage line, warning-tinted,
/// capped at [`MAX_SPLASH_WARNINGS`] with a `+N more` overflow line so a
/// chatty boot can't push the wordmark off a short terminal. Pure projection
/// onto the frame — theme tokens only, no I/O.
///
/// Degradation: under 70 columns (or 12 rows) the block wordmark collapses to
/// a plain `codypendent` line; under 8 rows the supporting copy drops too,
/// leaving just name + status + the Enter call-to-action when ready.
pub fn render_splash(
    frame: &mut Frame,
    tick: u64,
    stage: &str,
    warnings: &[String],
    ready: bool,
    theme: &Theme,
) {
    // Braille-dot spinner frames (the ten glyphs CLI spinners conventionally
    // use, e.g. pnpm's); cycled by tick so the stage line animates while boot
    // waits on the daemon.
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.surface.background)),
        area,
    );

    let expanded = area.width >= 70 && area.height >= 12;
    let mut lines: Vec<Line> = Vec::new();
    if expanded {
        lines.push(Line::styled(
            "COORDINATE  ·  COLLABORATE  ·  SHIP",
            Style::default()
                .fg(theme.agent.tool)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        for row in wordmark_rows("CODYPENDENT") {
            lines.push(Line::styled(
                row,
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::raw(""));
    } else {
        lines.push(Line::styled(
            "codypendent",
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if area.height >= 8 {
        lines.push(Line::styled(
            "Many agents. One shared workspace. You stay in control.",
            Style::default().fg(theme.text.secondary),
        ));
        lines.push(Line::raw(""));
    }
    if ready {
        lines.push(Line::from(vec![
            Span::styled(
                "✓ ",
                Style::default()
                    .fg(theme.status.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                stage,
                Style::default()
                    .fg(theme.text.primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        let spinner = SPINNER[(tick % SPINNER.len() as u64) as usize];
        lines.push(Line::from(vec![
            Span::styled(spinner.to_string(), Style::default().fg(theme.agent.tool)),
            Span::styled(
                format!(" {stage}"),
                Style::default().fg(theme.text.secondary),
            ),
        ]));
    }
    for warning in warnings.iter().take(MAX_SPLASH_WARNINGS) {
        lines.push(Line::from(vec![
            Span::styled("! ", Style::default().fg(theme.status.warning)),
            Span::styled(warning.clone(), Style::default().fg(theme.status.warning)),
        ]));
    }
    let overflow = warnings.len().saturating_sub(MAX_SPLASH_WARNINGS);
    if overflow > 0 {
        lines.push(Line::styled(
            format!("… +{overflow} more"),
            Style::default().fg(theme.text.muted),
        ));
    }

    if ready {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                " ENTER ",
                Style::default()
                    .fg(theme.surface.background)
                    .bg(theme.focus.active)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  open workspace",
                Style::default()
                    .fg(theme.text.primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if area.height >= 16 {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "/ commands   ·   F2 workspace   ·   F6 extension UI   ·   Esc quit",
                Style::default().fg(theme.text.muted),
            ));
        }
    }

    // A restrained, centered card makes the opening state read as a deliberate
    // welcome rather than transient debug output. It safely collapses to the
    // available terminal dimensions without clipping the outer border.
    let card_width = 78.min(area.width.saturating_sub(4)).max(1);
    let card_height = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(1);
    let card = Rect {
        x: area.x + area.width.saturating_sub(card_width) / 2,
        y: area.y + area.height.saturating_sub(card_height) / 2,
        width: card_width,
        height: card_height,
    };
    let border = if ready {
        theme.focus.active
    } else {
        theme.surface.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(theme.surface.panel))
        .title_bottom(Line::styled(
            format!(" v{BUILD_ID} "),
            Style::default().fg(theme.text.muted),
        ));
    let content = block.inner(card);
    frame.render_widget(block, card);
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), content);
}

/// The most boot diagnostics [`render_splash`] draws under the stage line
/// before collapsing the rest into a `+N more` overflow line.
const MAX_SPLASH_WARNINGS: usize = 4;

/// The splash wordmark's per-glyph height in rows.
const WORDMARK_GLYPH_ROWS: usize = 5;

/// One 5×5 block-letter glyph of the splash wordmark (hand-drawn — no figlet
/// dependency). Only the letters of `CODYPENDENT` are defined.
fn wordmark_glyph(ch: char) -> [&'static str; WORDMARK_GLYPH_ROWS] {
    match ch {
        'C' => [" ███ ", "█   █", "█    ", "█   █", " ███ "],
        'O' => [" ███ ", "█   █", "█   █", "█   █", " ███ "],
        'D' => ["████ ", "█   █", "█   █", "█   █", "████ "],
        'Y' => ["█   █", "█   █", " █ █ ", "  █  ", "  █  "],
        'P' => ["████ ", "█   █", "████ ", "█    ", "█    "],
        'E' => ["█████", "█    ", "████ ", "█    ", "█████"],
        'N' => ["█   █", "██  █", "█ █ █", "█  ██", "█   █"],
        'T' => ["█████", "  █  ", "  █  ", "  █  ", "  █  "],
        _ => ["     "; WORDMARK_GLYPH_ROWS],
    }
}

/// Join `text`'s glyphs into full block-letter rows, one space between letters.
fn wordmark_rows(text: &str) -> Vec<String> {
    let mut rows = vec![String::new(); WORDMARK_GLYPH_ROWS];
    for ch in text.chars() {
        for (row, cells) in rows.iter_mut().zip(wordmark_glyph(ch)) {
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(cells);
        }
    }
    rows
}

/// The workspace layout: a runs pane, the conversation, and an approvals + run
/// detail pane. The panes are at-a-glance context — interaction stays the same
/// (composer, palette, approval modal), so no pane needs its own input focus.
fn render_workspace(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    const MULTI_PANE_MIN_WIDTH: u16 = 110;
    if area.width < MULTI_PANE_MIN_WIDTH {
        match state.focus {
            Pane::Sessions => {
                state.register_hit(area, Action::FocusPane(Pane::Sessions));
                render_runs_pane(frame, area, state, theme);
            }
            Pane::Transcript => {
                state.register_hit(area, Action::FocusPane(Pane::Transcript));
                render_workspace_transcript(frame, area, state, theme, true);
            }
            Pane::Approvals => {
                state.register_hit(area, Action::FocusPane(Pane::Approvals));
                render_context_pane(frame, area, state, theme);
            }
        }
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(26),
            Constraint::Percentage(48),
            Constraint::Percentage(26),
        ])
        .split(area);
    // Register the whole-pane click-to-focus rects FIRST so each pane's own
    // finer row hits (registered by the renderers below) win over them.
    state.register_hit(cols[0], Action::FocusPane(Pane::Sessions));
    state.register_hit(cols[1], Action::FocusPane(Pane::Transcript));
    state.register_hit(cols[2], Action::FocusPane(Pane::Approvals));
    render_runs_pane(frame, cols[0], state, theme);
    render_workspace_transcript(
        frame,
        cols[1],
        state,
        theme,
        state.focus == Pane::Transcript,
    );
    render_context_pane(frame, cols[2], state, theme);
}

fn render_workspace_transcript(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    focused: bool,
) {
    let block = pane_block("Conversation", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_conversation(frame, inner, state, theme);
}

/// The runs pane (workspace layout): every run with its state and objective, the
/// selected one marked. Read-only — switch runs with Ctrl-↑/↓.
fn render_runs_pane(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let block = pane_block(
        &format!("Runs ({})", state.runs.len()),
        state.focus == Pane::Sessions,
        theme,
    );
    let mut items: Vec<ListItem> = Vec::new();
    if state.runs.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no runs yet",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (idx, run) in state.runs.iter().enumerate() {
        let selected = idx == state.selected_run;
        let marker = if selected { "› " } else { "  " };
        let line = Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                run_state_dot(run.state),
                theme.selection_aware_text_style(selected, run_state_color(run.state, theme)),
            ),
            Span::styled(
                " ",
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
            Span::styled(
                truncate(&run.objective, 18),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]);
        let item = ListItem::new(line);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    let inner = block.inner(area);
    frame.render_widget(List::new(items).block(block), area);
    let base = inner.y + if state.runs.is_empty() { 1 } else { 0 };
    for (idx, _) in state.runs.iter().enumerate() {
        let y = base + idx as u16;
        if y >= inner.y + inner.height {
            break;
        }
        state.register_hit(
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
            Action::SelectRun(idx),
        );
    }
}

/// The context pane (workspace layout): pending approvals over the selected run's
/// details. Read-only — approvals are resolved through the modal that pops when
/// one is pending.
fn render_context_pane(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let block = pane_block(
        &format!("Approvals ({})", state.pending_approvals.len()),
        state.focus == Pane::Approvals,
        theme,
    );
    let mut lines: Vec<Line> = Vec::new();

    if state.pending_approvals.is_empty() {
        lines.push(Line::styled(
            "  none pending",
            Style::default().fg(theme.text.muted),
        ));
    }
    for (idx, approval) in state.pending_approvals.iter().enumerate() {
        let selected = idx == state.selected_approval;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(theme.focus.active),
            ),
            Span::styled(
                risk_label(approval.risk.level).to_owned(),
                Style::default().fg(risk_color(approval.risk.level, theme)),
            ),
            Span::raw(" "),
            Span::styled(
                action_kind(&approval.action).to_owned(),
                Style::default().fg(theme.text.primary),
            ),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(section("Run", theme));
    if let Some(run) = state.selected_run() {
        let field = |k: &str, v: String, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v, Style::default().fg(color)),
            ])
        };
        lines.push(field(
            "state",
            run_state_label(run.state).to_owned(),
            run_state_color(run.state, theme),
        ));
        lines.push(field(
            "mode",
            mode_label(run.mode).to_owned(),
            theme.text.secondary,
        ));
        lines.push(field(
            "model",
            run.model
                .as_ref()
                .map_or("—".to_owned(), ToString::to_string),
            theme.text.secondary,
        ));
        lines.push(field(
            "ctx",
            run.context_percent
                .map_or("—".to_owned(), |p| format!("{p}%")),
            theme.status.info,
        ));
        lines.push(field(
            "cost",
            format_cost(run.cost_minor),
            theme.status.warning,
        ));
        lines.push(field(
            "wt",
            run.worktree.clone().unwrap_or_else(|| "—".to_owned()),
            theme.text.secondary,
        ));
    } else {
        lines.push(Line::styled(
            "  no run selected",
            Style::default().fg(theme.text.muted),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The composer's minimum height: one subtle top rule and one input row.
const COMPOSER_HEIGHT: u16 = 2;

/// The composer's maximum height in rows. A manual line break (`Alt+Enter`)
/// or a multi-line paste grows the box by one row per extra line, capped
/// here so a large paste can't swallow the whole screen; once the draft has
/// more lines than fit, the box scrolls to keep the cursor (the last line)
/// in view.
const COMPOSER_MAX_HEIGHT: u16 = 8;

/// How tall the composer should be this frame. Both explicit newlines and soft
/// wraps consume rows; the latter is essential on narrow terminals where a
/// long one-line draft would otherwise be clipped behind the footer.
fn composer_box_height(composer: &str, terminal_width: u16) -> u16 {
    let horizontal_margin = if terminal_width >= 72 { 2 } else { 1 };
    let content_width = terminal_width.saturating_sub(horizontal_margin * 2).max(1);
    let rows = composer_rendered_rows(composer, content_width);
    rows.saturating_add(1)
        .clamp(COMPOSER_HEIGHT, COMPOSER_MAX_HEIGHT)
}

fn composer_rendered_rows(composer: &str, width: u16) -> u16 {
    if composer.is_empty() {
        return 1;
    }
    let segments = composer.split('\n').collect::<Vec<_>>();
    let last = segments.len().saturating_sub(1);
    segments
        .iter()
        .enumerate()
        .fold(0_u16, |rows, (index, segment)| {
            let columns = 2 + UnicodeWidthStr::width(*segment) + usize::from(index == last);
            rows.saturating_add(line_rows(columns, usize::from(width.max(1))))
        })
        .max(1)
}

/// Wrapped-row height of a line `columns` display-columns wide in an
/// `inner_width` viewport: ceil(columns/inner_width), min 1.
fn line_rows(columns: usize, inner_width: usize) -> u16 {
    let iw = inner_width.max(1);
    let rows = if columns == 0 {
        1
    } else {
        columns.div_ceil(iw)
    };
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// One transcript row before placement (see module-level virtualization note).
struct Row<'a> {
    kind: RowKind<'a>,
    /// The transcript-entry index this row is a click target for (fold heads in
    /// the selected run). `None` unless tagged (Task 8).
    hit_entry: Option<usize>,
    /// A full-width background for this row (the `You` container). Cosmetic —
    /// `columns()`/`rows()` ignore it; applied only to visible rows at build.
    bg: Option<Color>,
}

enum RowKind<'a> {
    /// An already-styled line (structural rows + every non-`Model` entry).
    Built(Line<'a>),
    /// A streamed model-text source line, borrowed so measuring allocates nothing.
    Model {
        prefix: &'static str,
        text: &'a str,
        caret: bool,
        style: Style,
    },
    /// A cached, finalized rich line — borrowed so MEASURE allocates nothing.
    Rich(&'a crate::markdown::RichLine),
}

impl<'a> Row<'a> {
    fn built(line: Line<'a>) -> Self {
        Row {
            kind: RowKind::Built(line),
            hit_entry: None,
            bg: None,
        }
    }
    fn model(prefix: &'static str, text: &'a str, caret: bool, style: Style) -> Self {
        Row {
            kind: RowKind::Model {
                prefix,
                text,
                caret,
                style,
            },
            hit_entry: None,
            bg: None,
        }
    }
    fn rich(rl: &'a crate::markdown::RichLine) -> Self {
        Row {
            kind: RowKind::Rich(rl),
            hit_entry: None,
            bg: None,
        }
    }
    /// Display width, allocation-free (`Span::raw` borrows; `.width()` is unicode width).
    fn columns(&self) -> usize {
        match &self.kind {
            RowKind::Built(line) => line.width(),
            RowKind::Model {
                prefix,
                text,
                caret,
                ..
            } => Span::raw(*prefix).width() + Span::raw(*text).width() + usize::from(*caret),
            RowKind::Rich(rl) => rl
                .spans
                .iter()
                .map(|s| Span::raw(s.text.as_str()).width())
                .sum(),
        }
    }
    fn rows(&self, inner_width: u16) -> u16 {
        line_rows(self.columns(), inner_width as usize)
    }
    fn into_line(self, theme: &Theme) -> Line<'a> {
        match self.kind {
            RowKind::Built(line) => line,
            RowKind::Model {
                prefix,
                text,
                caret,
                style,
            } => {
                if caret {
                    Line::from(vec![
                        Span::styled(format!("{prefix}{text}"), style),
                        Span::styled("▋", Style::default().fg(theme.text.muted)),
                    ])
                } else {
                    Line::styled(format!("{prefix}{text}"), style)
                }
            }
            RowKind::Rich(rl) => Line::from(
                rl.spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), style_for(s.role, theme)))
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

use crate::markdown::{SpanRole, SyntaxRole};

/// Map a semantic `SpanRole` to a concrete `Style` from the live theme. Every
/// colour is a theme token — correct in all seven depths; a theme change simply
/// yields new colours on the next frame (no cache invalidation). Exhaustive over
/// `SpanRole`/`SyntaxRole`: a new variant is a compile error here, never a silent
/// unstyled span.
fn style_for(role: SpanRole, theme: &Theme) -> Style {
    let base = Style::default();
    match role {
        SpanRole::Gutter => base.fg(theme.text.muted),
        SpanRole::Body => base.fg(theme.agent.model_text),
        SpanRole::Heading(1..=2) => base
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        SpanRole::Heading(_) => base.fg(theme.text.heading).add_modifier(Modifier::BOLD),
        SpanRole::Strong => base.fg(theme.text.primary).add_modifier(Modifier::BOLD),
        SpanRole::Emphasis => base
            .fg(theme.agent.model_text)
            .add_modifier(Modifier::ITALIC),
        SpanRole::StrongEmphasis => base
            .fg(theme.text.primary)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        SpanRole::InlineCode => base.fg(theme.syntax.string),
        SpanRole::Link => base
            .fg(theme.focus.active)
            .add_modifier(Modifier::UNDERLINED),
        SpanRole::ListMarker => base.fg(theme.agent.tool),
        SpanRole::BlockQuote => base.fg(theme.text.secondary).add_modifier(Modifier::ITALIC),
        SpanRole::Rule => base.fg(theme.text.muted),
        SpanRole::TableHeader => base.fg(theme.text.heading).add_modifier(Modifier::BOLD),
        SpanRole::TableCell => base.fg(theme.agent.model_text),
        SpanRole::TableRule => base.fg(theme.surface.border),
        SpanRole::CodePlain => base.fg(theme.text.primary),
        SpanRole::CodeToken(SyntaxRole::Keyword) => base.fg(theme.syntax.keyword),
        SpanRole::CodeToken(SyntaxRole::Literal) => base.fg(theme.syntax.literal),
        SpanRole::CodeToken(SyntaxRole::StringLit) => base.fg(theme.syntax.string),
        SpanRole::CodeToken(SyntaxRole::Comment) => base.fg(theme.syntax.comment),
        SpanRole::CodeToken(SyntaxRole::Type) => base.fg(theme.syntax.r#type),
        SpanRole::CodeToken(SyntaxRole::Function) => base.fg(theme.syntax.function),
        SpanRole::CodeToken(SyntaxRole::Operator) => base.fg(theme.syntax.operator),
        SpanRole::CodeToken(SyntaxRole::Constant) => base.fg(theme.syntax.constant),
        SpanRole::CodeToken(SyntaxRole::Punctuation) => base.fg(theme.syntax.punctuation),
    }
}

/// Walk the whole session transcript in scroll order, emitting one `Row` per
/// logical line. Mirrors the old `conversation_lines` walk exactly; the `Model`
/// entry is emitted as borrowed `Row::Model` rows (measured cheaply, built only
/// when visible), every other entry reuses the existing `entry_lines` builders.
fn for_each_row<'a>(
    runs: &'a [RunView],
    theme: &Theme,
    selected_run: usize,
    mut visit: impl FnMut(Row<'a>),
) {
    let mut awaiting_header = false;
    let mut seen_user_turn = false;
    let last_run_idx = runs.len().checked_sub(1);
    let mut scratch: Vec<Line> = Vec::new();
    for (run_idx, run) in runs.iter().enumerate() {
        let is_last_run = Some(run_idx) == last_run_idx;
        let last_entry_idx = run.transcript.len().checked_sub(1);
        let mut produced = false;
        let mut prev_was_agent_cell = false;
        for (idx, entry) in run.transcript.iter().enumerate() {
            let streaming_tail = is_last_run
                && last_entry_idx == Some(idx)
                && run.activity == RunActivity::Streaming;
            let is_agent_cell = matches!(
                entry,
                TranscriptEntry::Model { .. }
                    | TranscriptEntry::Tool(_)
                    | TranscriptEntry::Patch(_)
            );
            // One blank before the first backstage/note entry that follows an
            // agent reply, so the dim "memory updated"/"context carried"
            // notes don't jam straight onto the model's text. `prev_was_agent_cell`
            // flips false again the moment we enter the notes cluster, so
            // only the FIRST note in a run of them gets the gap.
            let entering_notes_after_reply = prev_was_agent_cell
                && matches!(
                    entry,
                    TranscriptEntry::Note { .. } | TranscriptEntry::Backstage { .. }
                );
            if entering_notes_after_reply {
                visit(Row::built(Line::raw("")));
                produced = true;
            }
            if matches!(entry, TranscriptEntry::User { .. }) {
                if seen_user_turn {
                    visit(Row::built(Line::raw("")));
                    produced = true;
                }
                seen_user_turn = true;
                awaiting_header = true;
            } else if is_agent_cell && awaiting_header {
                if produced {
                    visit(Row::built(Line::raw("")));
                }
                let mut spans = vec![Span::styled(
                    "⏺ codypendent",
                    Style::default()
                        .fg(theme.agent.tool)
                        .add_modifier(Modifier::BOLD),
                )];
                if let Some(model) = &run.model {
                    spans.push(Span::styled(
                        format!(" · {model}"),
                        Style::default().fg(theme.text.muted),
                    ));
                }
                visit(Row::built(Line::from(spans)));
                produced = true;
                awaiting_header = false;
            }
            match entry {
                TranscriptEntry::Model { text, rendered } => match rendered {
                    // RICH: finalized and not the live tail → borrow cached lines.
                    Some(lines) if !streaming_tail => {
                        for rl in lines {
                            visit(Row::rich(rl));
                            produced = true;
                        }
                    }
                    // PLAIN: streaming tail, or not yet finalized (belt-and-braces).
                    _ => {
                        let mut rows: Vec<&str> = text.lines().collect();
                        if rows.is_empty() {
                            rows.push("");
                        }
                        let last = rows.len() - 1;
                        let style = Style::default().fg(theme.agent.model_text);
                        for (i, l) in rows.into_iter().enumerate() {
                            let prefix = if i == 0 { "▌ " } else { "  " };
                            visit(Row::model(prefix, l, streaming_tail && i == last, style));
                            produced = true;
                        }
                    }
                },
                other => {
                    scratch.clear();
                    entry_lines(other, theme, false, false, &mut scratch);
                    let hit = if run_idx == selected_run {
                        fold_hit_entry(other, idx)
                    } else {
                        None
                    };
                    let is_user = matches!(other, TranscriptEntry::User { .. });
                    for (j, line) in scratch.drain(..).enumerate() {
                        let mut row = Row::built(line);
                        if j == 0 {
                            row.hit_entry = hit;
                        }
                        if is_user {
                            row.bg = Some(theme.surface.user);
                        }
                        visit(row);
                        produced = true;
                    }
                }
            }
            prev_was_agent_cell = is_agent_cell;
        }
        if !produced {
            visit(Row::built(Line::styled(
                "(waiting for the agent…)",
                Style::default().fg(theme.text.muted),
            )));
        }
        if let Some(status) = activity_status_line(&run.activity, theme) {
            visit(Row::built(status));
        }
    }
}

/// The entry index if this entry renders a clickable fold HEAD (its first line):
/// a backstage summary, a folded (multi-line) note, or a failed-run summary.
fn fold_hit_entry(entry: &TranscriptEntry, idx: usize) -> Option<usize> {
    match entry {
        TranscriptEntry::Backstage { .. } => Some(idx),
        TranscriptEntry::Note { text, .. } if text.lines().count() > NOTE_INLINE_LINE_THRESHOLD => {
            Some(idx)
        }
        TranscriptEntry::Completed {
            disposition: RunDisposition::Failed { .. },
            ..
        } => Some(idx),
        _ => None,
    }
}

/// Total wrapped-row height of the whole transcript (the measure pass).
fn transcript_rows(runs: &[RunView], theme: &Theme, inner_width: u16) -> u16 {
    let mut total: u16 = 0;
    for_each_row(runs, theme, usize::MAX, |row| {
        total = total.saturating_add(row.rows(inner_width));
    });
    total
}

/// Build only the rows whose wrapped range intersects `[first_row, first_row+height)`.
fn build_transcript_window<'a>(
    runs: &'a [RunView],
    theme: &Theme,
    inner_width: u16,
    first_row: u16,
    height: u16,
    selected_run: usize,
) -> (Vec<Line<'a>>, u16, Vec<(usize, usize)>) {
    let last_row = first_row.saturating_add(height);
    let mut out: Vec<Line> = Vec::with_capacity(height as usize + 2);
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut cursor: u16 = 0;
    let mut scroll: u16 = 0;
    let mut first_seen = false;
    for_each_row(runs, theme, selected_run, |row| {
        let h = row.rows(inner_width);
        let row_start = cursor;
        let row_end = cursor.saturating_add(h);
        cursor = row_end;
        if row_end > first_row && row_start < last_row {
            if !first_seen {
                scroll = first_row.saturating_sub(row_start);
                first_seen = true;
            }
            let hit = row.hit_entry;
            let bg = row.bg;
            let index = out.len();
            let mut line = row.into_line(theme);
            if let Some(c) = bg {
                if c == theme.surface.panel {
                    // No distinct raised surface (ansi16/monochrome): a leading accent bar.
                    line.spans.insert(
                        0,
                        Span::styled("▎", Style::default().fg(theme.focus.active)),
                    );
                } else {
                    line.style = line.style.bg(c);
                    let pad = (inner_width as usize).saturating_sub(line.width());
                    if pad > 0 {
                        line.spans
                            .push(Span::styled(" ".repeat(pad), Style::default().bg(c)));
                    }
                }
            }
            out.push(line);
            if let Some(entry) = hit {
                hits.push((index, entry));
            }
        }
    });
    (out, scroll, hits)
}

/// Every run in the session as one continuous, unboxed conversation. Session,
/// model, mode, and cost live in the project header; this surface is reserved
/// for the task, agent activity, and results.
fn render_conversation(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let available = area.inner(Margin {
        horizontal: if area.width >= 72 { 3 } else { 1 },
        vertical: 0,
    });
    // Prose is easier to scan on a stable reading measure. Keep the timeline
    // centred on wide terminals instead of stretching messages from rail to
    // rail; compact terminals still use every available column.
    let reading_width = available.width.min(118);
    let inner = Rect {
        x: available.x + available.width.saturating_sub(reading_width) / 2,
        y: available.y,
        width: reading_width,
        height: available.height,
    };

    if state.runs.is_empty() {
        state.transcript_max_scroll.set(0);
        let lines = if state.models.is_empty() {
            vec![
                Line::styled(
                    "✦  Connect your first model",
                    Style::default()
                        .fg(theme.text.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::styled(
                    "Codypendent needs one verified model before it can work with you.",
                    Style::default().fg(theme.text.secondary),
                ),
                Line::styled(
                    "Press / and choose Provider catalog to discover a local or hosted model.",
                    Style::default().fg(theme.text.primary),
                ),
                Line::styled(
                    "Setup & diagnostics verifies the exact model—not just its endpoint.",
                    Style::default().fg(theme.text.muted),
                ),
            ]
        } else {
            vec![
                Line::styled(
                    "✦  What should we build?",
                    Style::default()
                        .fg(theme.text.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::styled(
                    "Describe a change, paste an error, or ask Codypendent to explore the codebase.",
                    Style::default().fg(theme.text.secondary),
                ),
                Line::styled(
                    "Enter sends  ·  / opens commands  ·  F2 opens the workspace",
                    Style::default().fg(theme.text.muted),
                ),
            ]
        };
        let height = lines.len() as u16;
        let side = if inner.width > 84 { 4 } else { 0 };
        let hero = Rect {
            x: inner.x + side,
            y: inner.y + inner.height.saturating_sub(height) / 3,
            width: inner.width.saturating_sub(side * 2),
            height: height.min(inner.height),
        };
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), hero);
        return;
    }

    let inner_width = inner.width;
    // Measure the whole transcript cheaply, cache the bottom offset (so the
    // reducer's paging leaves/enters follow mode precisely), then BUILD only the
    // visible window — per-frame allocation is bounded by the viewport, not the
    // transcript length (the crash fix).
    let content_rows = transcript_rows(&state.runs, theme, inner_width);
    let max_scroll = content_rows.saturating_sub(inner.height);
    state.transcript_max_scroll.set(max_scroll);
    let (follow, scroll) = state
        .selected_run()
        .map_or((true, 0), |run| (run.follow, run.scroll));
    let mut offset = if follow {
        max_scroll
    } else {
        scroll.min(max_scroll)
    };
    // Guard the u16 handed to `Paragraph::scroll` — the rewrite must not
    // reintroduce the overflow the old implicit coupling merely avoided.
    offset = offset.min(u16::MAX.saturating_sub(inner.height));

    let (mut lines, r0, hits) = build_transcript_window(
        &state.runs,
        theme,
        inner_width,
        offset,
        inner.height,
        state.selected_run,
    );

    // A new conversation starts near the top of its reading canvas. Keeping
    // hundreds of empty rows above the first exchange made the timeline feel
    // detached from the project header and hid its message hierarchy.
    // Overflowing transcripts still follow/scroll exactly as before.
    let top_pad = if content_rows < inner.height {
        inner.height.saturating_sub(content_rows).min(2)
    } else {
        0
    };
    if top_pad > 0 {
        let mut padded = Vec::with_capacity(top_pad as usize + lines.len());
        padded.resize(top_pad as usize, Line::raw(""));
        padded.append(&mut lines);
        lines = padded;
    }

    // Register only the VISIBLE fold-head hits `build_transcript_window` found
    // (bounded by the viewport, never the whole history — virtualization
    // preserved). One of `top_pad`/`r0` is always 0 (see their derivation
    // above), so this formula exactly places a single-row fold head.
    for (line_index, entry) in &hits {
        let screen_y = inner.y as i32 + top_pad as i32 + *line_index as i32 - r0 as i32;
        if screen_y >= inner.y as i32 && screen_y < (inner.y + inner.height) as i32 {
            state.register_hit(
                Rect {
                    x: inner.x,
                    y: screen_y as u16,
                    width: inner.width,
                    height: 1,
                },
                Action::ActivateRow(*entry),
            );
        }
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((r0, 0));
    frame.render_widget(paragraph, inner);
}

/// The persistent composer: an always-present input line. Empty, it shows a
/// context-aware placeholder (start a run vs. steer the live one); with a draft,
/// it shows the text and a cursor.
fn render_composer(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let steering = state.selected_run_is_active();
    let area = area.inner(Margin {
        horizontal: if area.width >= 72 { 2 } else { 1 },
        vertical: 0,
    });
    let block = Block::default()
        .borders(Borders::TOP)
        .title(Span::styled(
            if steering { " STEER " } else { " MESSAGE " },
            Style::default()
                .fg(theme.focus.active)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.surface.border))
        .style(Style::default().bg(theme.surface.background));

    let prompt_style = Style::default()
        .fg(theme.focus.active)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = if state.composer.is_empty() {
        let hint = if steering {
            "Add guidance while the agent works…"
        } else {
            "Ask Codypendent to build, fix, explain, or explore…"
        };
        vec![Line::from(vec![
            Span::styled("❯ ", prompt_style),
            Span::styled(hint, Style::default().fg(theme.text.muted)),
        ])]
    } else {
        // A manual line break (`Alt+Enter`) or a multi-line paste puts a `\n`
        // in the draft: render each segment as its own `Line` (a raw `\n`
        // inside one `Line`'s text does not itself wrap) rather than a single
        // wrapped line — only the first gets the `› ` prompt, and only the
        // last gets the cursor.
        let segments: Vec<&str> = state.composer.split('\n').collect();
        let last = segments.len() - 1;
        segments
            .into_iter()
            .enumerate()
            .map(|(i, segment)| {
                let mut spans = vec![Span::styled(if i == 0 { "❯ " } else { "  " }, prompt_style)];
                spans.push(Span::styled(
                    segment,
                    Style::default().fg(theme.text.primary),
                ));
                if i == last {
                    spans.push(Span::styled("▏", Style::default().fg(theme.focus.active)));
                }
                Line::from(spans)
            })
            .collect()
    };

    // Keep the cursor (the last line) in view once the draft has more lines
    // than the box shows — the box already grew toward `COMPOSER_MAX_HEIGHT`
    // (see `composer_box_height`); this only matters once it's capped there.
    let visible_rows = area.height.saturating_sub(1).max(1);
    let total_rows = lines.iter().fold(0_u16, |rows, line| {
        rows.saturating_add(line_rows(line.width(), usize::from(area.width.max(1))))
    });
    let scroll_y = total_rows.saturating_sub(visible_rows);

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0)),
        area,
    );
    // Belt-and-braces: the full-screen scrim (`render_overlays`) already
    // returns to typing on an outside click; this covers the composer
    // specifically in case it ever renders above the scrim.
    if !matches!(state.overlay, Overlay::None) {
        state.register_hit(area, Action::Dismiss);
    }
}

fn pane_block(title: &str, focused: bool, theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.border_color(focused)))
        .style(theme.panel_style())
}

/// The dim status row a run's derived [`RunActivity`] renders as, so a run
/// between visible transcript updates never looks silently paused.
/// `Streaming` needs no row of its own (the growing model text is itself the
/// live signal) and `Idle` renders nothing.
fn activity_status_line(activity: &RunActivity, theme: &Theme) -> Option<Line<'static>> {
    let text = match activity {
        RunActivity::Thinking => "working…".to_owned(),
        RunActivity::RunningTool(tool) => format!("running {tool}…"),
        RunActivity::Streaming | RunActivity::Idle => return None,
    };
    Some(Line::styled(text, Style::default().fg(theme.text.muted)))
}

fn entry_lines<'a>(
    entry: &'a TranscriptEntry,
    theme: &Theme,
    selected: bool,
    streaming_tail: bool,
    out: &mut Vec<Line<'a>>,
) {
    let head = |text: String, color: Color| -> Line<'a> {
        let style = if selected {
            theme.selection_style()
        } else {
            Style::default().fg(color)
        };
        Line::styled(text, style)
    };

    match entry {
        TranscriptEntry::User { text } => {
            out.push(Line::styled(
                "You",
                Style::default()
                    .fg(theme.focus.active)
                    .add_modifier(Modifier::BOLD),
            ));
            let mut wrote_body = false;
            for line in text.lines() {
                out.push(Line::styled(
                    format!("  {line}"),
                    Style::default().fg(theme.text.primary),
                ));
                wrote_body = true;
            }
            if !wrote_body {
                out.push(Line::styled("  ", Style::default().fg(theme.text.primary)));
            }
        }
        TranscriptEntry::Model { text, .. } => {
            model_entry_lines(text, theme, selected, streaming_tail, out);
        }
        TranscriptEntry::Tool(card) => tool_card_lines(card, theme, selected, out),
        TranscriptEntry::Patch(patch) => patch_lines(patch, theme, selected, out),
        TranscriptEntry::Steering { applied } => {
            let label = if *applied {
                "➤ steering applied"
            } else {
                "➤ steering queued"
            };
            out.push(head(label.to_owned(), theme.status.info));
        }
        TranscriptEntry::Budget {
            dimension,
            used,
            limit,
        } => {
            out.push(head(
                format!("⚠ budget {}: {used}/{limit}", budget_label(*dimension)),
                theme.status.warning,
            ));
        }
        TranscriptEntry::Completed {
            disposition,
            expanded,
        } => match disposition {
            // Success: the streamed model prose already ended the turn —
            // render nothing here, so the reply is never echoed a second
            // (or, with the old status line plus this one, third) time.
            RunDisposition::Completed { .. } => {}
            // A failed run's `reason` is often a nested driver/service error
            // chain (e.g. "model driver error: model stream failed: service
            // error: request failed: builder error") — raw, that reads as
            // noise. Collapsed (default), only `summarize_error`'s one-line,
            // human summary shows; expanding (Task 3, mirrors the Backstage
            // fold) reveals the full raw chain underneath, so no detail is
            // ever lost, just folded.
            RunDisposition::Failed { reason } => {
                let marker = if *expanded { "▾" } else { "▸" };
                out.push(head(
                    format!("{marker} ✗ {}", summarize_error(reason)),
                    theme.status.error,
                ));
                if *expanded {
                    out.push(Line::styled(
                        format!("    {reason}"),
                        Style::default().fg(theme.text.muted),
                    ));
                }
            }
            RunDisposition::Cancelled { reason } => {
                let text = reason
                    .as_ref()
                    .map_or_else(|| "✗ cancelled".to_owned(), |r| format!("✗ cancelled: {r}"));
                out.push(head(text, theme.text.muted));
            }
            // Protocol RULE 1 (render, do not crash): `RunDisposition` is
            // `#[non_exhaustive]` — this also catches the `Unknown` variant a
            // disposition kind this build predates deserializes to.
            _ => {
                out.push(head("✗ run ended".to_owned(), theme.text.muted));
            }
        },
        TranscriptEntry::Note { text, expanded } => {
            note_lines(text, *expanded, theme, selected, out)
        }
        TranscriptEntry::Backstage {
            context_lines,
            memory_updates,
            raw,
            expanded,
        } => backstage_lines(
            *context_lines,
            *memory_updates,
            raw,
            *expanded,
            theme,
            selected,
            out,
        ),
        TranscriptEntry::Unsupported { label } => {
            out.push(head(format!("? {label}"), theme.text.muted));
        }
    }
}

/// Map a nested error chain (`": "`-joined segments) to a concise summary. Pure
/// heuristic: recognized outermost segments map to a friendly category; anything
/// else degrades to the outermost segment verbatim. The full raw chain is one
/// expand away, so no detail is lost.
fn summarize_error(raw: &str) -> String {
    let outer = raw.split(": ").next().unwrap_or("").trim();
    // Recognized categories, checked against any segment of the chain.
    for segment in raw.split(": ") {
        match segment.trim() {
            "model driver error" | "model stream failed" => {
                return "model error — the provider request failed".to_owned();
            }
            _ => {}
        }
    }
    for segment in raw.split(": ") {
        match segment.trim() {
            "service error" | "request failed" => return "provider request failed".to_owned(),
            _ => {}
        }
    }
    if outer.is_empty() {
        "run failed".to_owned()
    } else {
        outer.to_owned()
    }
}

/// Renders one coalesced model-text entry. While `streaming_tail` is set —
/// this is the run's newest transcript entry and the run's derived activity
/// is [`RunActivity::Streaming`] — a muted `▋` caret is appended directly
/// after the accumulated text on its last line (Task 4), so a mid-stream cell
/// visibly reads as still-writing instead of silently paused. The caret is
/// drawn fresh from `run.activity` every frame — it is never stored on the
/// entry — so it disappears the instant the run leaves `Streaming` (a tool
/// call starting, a thinking pause, or the run completing).
///
/// Folding the caret into the same `Line` that both the transcript
/// `Paragraph` and [`transcript_rows`]'s measurement read (see
/// `render_conversation`) means the measured bottom already accounts for it —
/// "follow latest" pins to the caret's row with no separate adjustment.
fn model_entry_lines<'a>(
    text: &'a str,
    theme: &Theme,
    selected: bool,
    streaming_tail: bool,
    out: &mut Vec<Line<'a>>,
) {
    let color = theme.agent.model_text;
    let text_style = if selected {
        theme.selection_style()
    } else {
        Style::default().fg(color)
    };
    let mut rows: Vec<&str> = text.lines().collect();
    if rows.is_empty() {
        // A `Model` entry is only ever created alongside its first delta's
        // text (`AppState::append_model_text`), so empty text here is
        // defensive rather than expected — but a caret still needs a row.
        rows.push("");
    }
    let last = rows.len() - 1;
    for (i, l) in rows.into_iter().enumerate() {
        let prefix = if i == 0 { "▌ " } else { "  " };
        if streaming_tail && i == last {
            out.push(Line::from(vec![
                Span::styled(format!("{prefix}{l}"), text_style),
                Span::styled("▋", Style::default().fg(theme.text.muted)),
            ]));
        } else {
            out.push(Line::styled(format!("{prefix}{l}"), text_style));
        }
    }
}

fn tool_card_lines<'a>(card: &'a ToolCard, theme: &Theme, selected: bool, out: &mut Vec<Line<'a>>) {
    // Task 5 (codex chat shell): the collapsed head is one compact line — a
    // run glyph, the tool's verb/name, and a terse outcome mark — instead of
    // a `[status]` bracket; `card.status`/`card.outcome` drive the mark
    // exactly as they drove the old bracket text.
    let (outcome_mark, outcome_color) = match card.status {
        ToolStatus::Proposed => ("⟳ review", theme.status.warning),
        ToolStatus::Running => ("running", theme.status.running),
        ToolStatus::Completed => match &card.outcome {
            Some(codypendent_protocol::ToolOutcome::Failed { .. }) => ("✗", theme.status.error),
            _ => ("✓", theme.status.success),
        },
    };
    let name = if card.tool.is_empty() {
        card.action.as_ref().map_or("tool", action_kind)
    } else {
        card.tool.as_str()
    };
    let marker = if card.expanded { "▾" } else { "▸" };
    let head_style = if selected {
        theme.selection_style()
    } else {
        Style::default().fg(theme.agent.tool)
    };
    // The label (e.g. `services/main.py` for `workspace.read_file`, `cargo
    // test` for `shell.run`) renders as `{tool} · {label}` before the outcome
    // mark, dim/muted so the tool name stays the visual anchor — exactly the
    // `⏺ codypendent · <model>` convention the turn header already uses.
    // `tool_card_lines` has no column-width parameter to fit against (unlike
    // `desc_w`-style callers elsewhere in this file), so — matching every
    // other fixed-width `truncate` call in this module (run objectives,
    // skill/memory names, ...) — the label gets its own fixed cap, well under
    // a typical card's width even alongside a long tool name. The daemon side
    // (`codypendent_runtime::tools::tool_label`) already bounds the label to
    // 80 chars; this is a second, independent, render-layer clamp so a card
    // never overflows regardless of what produced the event. When there is no
    // label (an older daemon, or a tool `tool_label` does not recognize) the
    // head renders exactly as it did before this field existed: `{tool}
    // {status}`, one line either way.
    const LABEL_RENDER_MAX_CHARS: usize = 48;
    let mut head = vec![Span::styled(format!("{marker} ⏺ {name} "), head_style)];
    if let Some(label) = card.label.as_deref().filter(|l| !l.is_empty()) {
        head.push(Span::styled(
            format!("· {} ", truncate(label, LABEL_RENDER_MAX_CHARS)),
            Style::default().fg(theme.text.muted),
        ));
    }
    head.push(Span::styled(
        outcome_mark,
        Style::default().fg(outcome_color),
    ));
    out.push(Line::from(head));

    if card.expanded {
        if let Some(action) = &card.action {
            for detail in describe_action(action) {
                out.push(Line::styled(
                    format!("    {detail}"),
                    Style::default().fg(theme.text.secondary),
                ));
            }
        }
        if let Some(digest) = &card.args_digest {
            out.push(Line::styled(
                format!("    args-digest: {digest}"),
                Style::default().fg(theme.text.muted),
            ));
        }
        if let Some(codypendent_protocol::ToolOutcome::Failed { message }) = &card.outcome {
            out.push(Line::styled(
                format!("    error: {message}"),
                Style::default().fg(theme.status.error),
            ));
        }
        if let Some(artifact) = &card.artifact {
            out.push(Line::styled(
                format!(
                    "    output: {} ({} bytes)",
                    artifact.media_type, artifact.byte_length
                ),
                Style::default().fg(theme.text.muted),
            ));
        }
    }
}

fn patch_lines<'a>(
    patch: &'a PatchSummary,
    theme: &Theme,
    selected: bool,
    out: &mut Vec<Line<'a>>,
) {
    let marker = if patch.expanded { "▾" } else { "▸" };
    let head_style = if selected {
        theme.selection_style()
    } else {
        Style::default().fg(theme.diff.header)
    };
    let target = match patch.files.as_slice() {
        [file] => truncate(file, 48),
        files if !files.is_empty() => format!("{} files", files.len()),
        _ => format!("change set {}", short_id(&patch.changeset_id)),
    };
    let stats = if patch.additions > 0 || patch.deletions > 0 {
        format!("  +{} −{}", patch.additions, patch.deletions)
    } else {
        String::new()
    };
    out.push(Line::from(vec![
        Span::styled(format!("{marker} ◆ {target}"), head_style),
        Span::styled(stats, Style::default().fg(theme.text.muted)),
        Span::styled("  changes ready", Style::default().fg(theme.status.success)),
    ]));
    if patch.expanded {
        if patch.files.len() > 1 {
            for file in &patch.files {
                out.push(Line::styled(
                    format!("    {file}"),
                    Style::default().fg(theme.text.secondary),
                ));
            }
            out.push(Line::raw(""));
        }
        for line in patch.preview.lines() {
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                theme.diff.added
            } else if line.starts_with('-') && !line.starts_with("---") {
                theme.diff.removed
            } else if line.starts_with("@@") || line.starts_with("diff --git") {
                theme.diff.header
            } else {
                theme.diff.context
            };
            out.push(Line::styled(
                format!("    {line}"),
                Style::default().fg(color),
            ));
        }
        if patch.preview_truncated {
            out.push(Line::styled(
                "    … preview truncated; full diff retained as an artifact",
                Style::default().fg(theme.text.muted),
            ));
        }
        out.push(Line::styled(
            format!(
                "    full diff · {} · {} bytes",
                patch.artifact.media_type, patch.artifact.byte_length
            ),
            Style::default().fg(theme.diff.context),
        ));
    }
}

/// Notes at or under this many lines render inline, unchanged; a longer note
/// folds (mirrors [`ToolCard`]/[`PatchSummary`] — the Chapter 07
/// transcript-declutter fix). Applies to ANY note generically — nothing here
/// special-cases the run-context manifest or a curated-memory note.
const NOTE_INLINE_LINE_THRESHOLD: usize = 2;

fn note_lines<'a>(
    text: &'a str,
    expanded: bool,
    theme: &Theme,
    selected: bool,
    out: &mut Vec<Line<'a>>,
) {
    let head_style = if selected {
        theme.selection_style()
    } else {
        Style::default().fg(theme.text.secondary)
    };
    let line_count = text.lines().count();
    if line_count <= NOTE_INLINE_LINE_THRESHOLD {
        out.push(Line::styled(format!("• note: {text}"), head_style));
        return;
    }
    let marker = if expanded { "▾" } else { "▸" };
    out.push(Line::styled(
        format!(
            "{marker} note: {} ({line_count} lines)",
            first_non_empty_line(text)
        ),
        head_style,
    ));
    if expanded {
        for line in text.lines() {
            out.push(Line::styled(
                format!("    {line}"),
                Style::default().fg(theme.text.secondary),
            ));
        }
    }
}

/// Renders the folded backstage line (Task 2): the context manifest and
/// curated-memory writes for the run, summarized in one dim, expandable line
/// instead of the visible `Note` cells they'd otherwise be. Each half
/// (`context …`, `memory …`) is omitted when its count is empty (`None`/`0`);
/// if both are empty (defensive — the reducer never creates the entry
/// without at least one), nothing renders. `⋯` marks the folded line; once
/// expanded, the full text of every folded note follows, dim and indented,
/// same as an expanded [`note_lines`] body.
fn backstage_lines<'a>(
    context_lines: Option<usize>,
    memory_updates: usize,
    raw: &'a [String],
    expanded: bool,
    theme: &Theme,
    selected: bool,
    out: &mut Vec<Line<'a>>,
) {
    let mut parts = Vec::new();
    if let Some(n) = context_lines {
        let noun = if n == 1 { "line" } else { "lines" };
        parts.push(format!("context · {n} {noun}"));
    }
    if memory_updates > 0 {
        if memory_updates == 1 {
            parts.push("memory updated".to_owned());
        } else {
            parts.push(format!("memory updated ×{memory_updates}"));
        }
    }
    if parts.is_empty() {
        return;
    }
    let head_style = if selected {
        theme.selection_style()
    } else {
        Style::default().fg(theme.text.muted)
    };
    let marker = if expanded { "▾" } else { "⋯" };
    out.push(Line::styled(
        format!("{marker} {}", parts.join(" · ")),
        head_style,
    ));
    if expanded {
        for note in raw {
            for line in note.lines() {
                out.push(Line::styled(
                    format!("    {line}"),
                    Style::default().fg(theme.text.muted),
                ));
            }
        }
    }
}

fn render_status_line(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let bg = Style::default().bg(theme.surface.background);
    if let Some((notice, _)) = &state.notice {
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled("● ", Style::default().fg(theme.status.warning)),
            Span::styled(notice.clone(), Style::default().fg(theme.text.secondary)),
        ]);
        frame.render_widget(Paragraph::new(line).style(bg), area);
        return;
    }

    let status = state.status();
    let key = |text: &'static str| Span::styled(text, Style::default().fg(theme.focus.active));
    let hint = |text: &'static str| Span::styled(text, Style::default().fg(theme.text.muted));

    let (left, right): (Vec<Span>, Vec<Span>) = if status.pending_approvals > 0 {
        (
            vec![
                Span::raw("  "),
                Span::styled("● ", Style::default().fg(theme.status.warning)),
                Span::styled(
                    format!("Approval needed · {} pending", status.pending_approvals),
                    Style::default()
                        .fg(theme.text.primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
            vec![
                key("a"),
                hint(" once  "),
                key("A"),
                hint(" run  "),
                key("r"),
                hint(" reject"),
            ],
        )
    } else if !state.issues.is_empty() {
        state.register_hit(area, Action::OpenIssues);
        (
            vec![
                Span::raw("  "),
                Span::styled("▲ ", Style::default().fg(theme.status.warning)),
                Span::styled(
                    format!("Setup needs attention · {} issue(s)", state.issues.len()),
                    Style::default().fg(theme.status.warning),
                ),
            ],
            vec![key("/"), hint(" diagnostics")],
        )
    } else if !state.composer.is_empty() {
        (
            vec![
                Span::raw("  "),
                Span::styled("● ", Style::default().fg(theme.focus.active)),
                Span::styled("Draft ready", Style::default().fg(theme.text.secondary)),
            ],
            vec![
                key("Enter"),
                hint(" send  "),
                key("⌥Enter"),
                hint(" newline  "),
                key("Esc"),
                hint(" clear"),
            ],
        )
    } else if !central_remote_ui_is_active(state)
        && !state
            .remote_ui
            .mounted_documents_for_points(TERMINAL_CENTRAL_SLOTS)
            .is_empty()
    {
        state.register_hit(area, Action::RemoteUiSetActive(true));
        (
            vec![
                Span::raw("  "),
                Span::styled("◇ ", Style::default().fg(theme.focus.active)),
                Span::styled(
                    "Extension UI ready",
                    Style::default().fg(theme.text.secondary),
                ),
            ],
            vec![key("F6"), hint(" focus")],
        )
    } else if state.selected_run().is_some_and(|run| !run.follow) {
        (
            vec![
                Span::raw("  "),
                Span::styled("↑ ", Style::default().fg(theme.status.info)),
                Span::styled(
                    "Viewing earlier output",
                    Style::default().fg(theme.text.secondary),
                ),
            ],
            vec![key("PgDn"), hint(" latest")],
        )
    } else if let Some(run_state) = status.run_state {
        state.register_hit(area, Action::OpenPalette);
        let glyph = if matches!(run_state, RunState::Completed) {
            "✓ "
        } else {
            "● "
        };
        (
            vec![
                Span::raw("  "),
                Span::styled(
                    glyph,
                    Style::default().fg(run_state_color(run_state, theme)),
                ),
                Span::styled(
                    run_state_label(run_state).to_owned(),
                    Style::default().fg(theme.text.secondary),
                ),
            ],
            vec![key("/"), hint(" commands  "), key("F2"), hint(" workspace")],
        )
    } else {
        state.register_hit(area, Action::OpenPalette);
        (
            vec![
                Span::raw("  "),
                Span::styled("● ", Style::default().fg(theme.status.success)),
                Span::styled("Ready", Style::default().fg(theme.text.secondary)),
            ],
            vec![
                key("Enter"),
                hint(" send  "),
                key("⌥Enter"),
                hint(" newline  "),
                key("/"),
                hint(" commands"),
            ],
        )
    };

    let left_width: usize = left.iter().map(|span| span.width()).sum();
    // Footer hints are emitted as key/description pairs in priority order.
    // Keep only complete pairs that fit; clipping half a shortcut is noisier
    // than omitting its lower-priority hint on a compact terminal.
    let mut packed_right = Vec::new();
    let mut packed_width = 0_usize;
    for pair in right.chunks(2) {
        let pair_width = pair.iter().map(Span::width).sum::<usize>();
        if left_width + packed_width + pair_width + 2 > usize::from(area.width) {
            break;
        }
        packed_right.extend(pair.iter().cloned());
        packed_width += pair_width;
    }
    let pad = usize::from(area.width).saturating_sub(left_width + packed_width + 2);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(packed_right);
    spans.push(Span::raw("  "));

    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}

fn render_overlays(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let has_modal = !matches!(state.overlay, Overlay::None) || state.show_approval_modal();
    if has_modal {
        // Apply a real visual scrim to the already-painted base. Every modal
        // clears/repaints its own rectangle afterward, so foreground content
        // remains crisp while the conversation recedes.
        frame
            .buffer_mut()
            .set_style(area, Style::default().add_modifier(Modifier::DIM));
    }
    // The modal scrim: registered FIRST (bottom of the overlay z-order) so it
    // sits beneath every overlay's own rows, registered by the arms below —
    // `hit_test` resolves to the topmost (last-registered) rect, so a click
    // inside the overlay still resolves to its row, not the scrim. The
    // approval modal (the `Overlay::None` + pending-approval branch) gets the
    // visual dimming above but no dismiss action: the operator must decide it.
    if !matches!(state.overlay, Overlay::None) {
        state.register_hit(area, Action::Dismiss);
    }
    match &state.overlay {
        Overlay::Help => render_help(frame, area, state, theme),
        Overlay::Issues => render_issues(frame, area, state, theme),
        Overlay::NewRun(buffer) => {
            render_prompt(frame, area, state, theme, "New run objective", buffer);
        }
        Overlay::Steering(buffer) => {
            render_prompt(
                frame,
                area,
                state,
                theme,
                "Steer the run (queued for a safe point)",
                buffer,
            );
        }
        Overlay::WorkflowInputs {
            workflow_id,
            buffer,
        } => render_prompt(
            frame,
            area,
            state,
            theme,
            &format!("Inputs for {workflow_id} (JSON object; blank = {{}})"),
            buffer,
        ),
        Overlay::ConfirmCancel => render_confirm(frame, area, state, theme),
        Overlay::ConfirmWorkflowCancel { workflow_run_id } => render_confirm_box(
            frame,
            area,
            state,
            theme,
            "Cancel this workflow run?",
            &format!(
                "Stops new nodes and interrupts active work · run {}",
                truncate(workflow_run_id, 12)
            ),
        ),
        Overlay::Skills => render_skills(frame, area, state, theme),
        Overlay::Memory { source_open } => {
            render_memory(frame, area, state, theme, *source_open);
        }
        Overlay::Docs => render_docs(frame, area, state, theme),
        Overlay::Edges => render_edges(frame, area, state, theme),
        Overlay::EdgeSearch(buffer) => {
            render_edges(frame, area, state, theme);
            render_prompt(
                frame,
                area,
                state,
                theme,
                "Search code-graph symbols / relations (blank = all)",
                buffer,
            );
        }
        Overlay::Workflow => render_workflow(frame, area, state, theme),
        Overlay::Blackboard => render_blackboard(frame, area, state, theme),
        Overlay::UiPlugins => render_ui_plugins(frame, area, state, theme),
        Overlay::ConfirmUiPluginApprove { plugin_id, receipt } => render_confirm_box(
            frame,
            area,
            state,
            theme,
            "Approve this verified permission update?",
            &format!("plugin {plugin_id} · exact receipt {receipt}"),
        ),
        Overlay::ConfirmUiPluginReject { plugin_id, receipt } => render_confirm_box(
            frame,
            area,
            state,
            theme,
            "Reject this verified permission update?",
            &format!("plugin {plugin_id} · exact receipt {receipt}"),
        ),
        Overlay::ConfirmUiPluginRevoke { plugin_id } => render_confirm_box(
            frame,
            area,
            state,
            theme,
            "Revoke this Remote UI plugin?",
            &format!("Stops every active worker for {plugin_id}"),
        ),
        Overlay::Palette { query, selected } => {
            render_palette(frame, area, state, theme, query, *selected);
        }
        Overlay::CouncilBuilder(builder) => {
            render_council_builder(frame, area, state, theme, builder);
        }
        Overlay::ModelPicker { query, selected } => {
            render_model_picker(frame, area, state, theme, query, *selected);
        }
        Overlay::ProviderPicker { query, selected } => {
            render_provider_picker(frame, area, state, theme, query, *selected);
        }
        Overlay::ModePicker { query, selected } => {
            render_mode_picker(frame, area, state, theme, query, *selected);
        }
        // D1: the `/keys` overlay, its masked set/replace prompt, and its two
        // confirms. The set prompt reuses `render_masked_prompt` (the key can
        // never appear on screen); neither confirm ever carries key material.
        Overlay::ApiKeys { query, selected } => {
            render_api_keys(frame, area, state, theme, query, *selected);
        }
        Overlay::ApiKeySet { target, buffer } => {
            let title = match target {
                KeyTarget::Model(id) => {
                    format!("API key for {id} (stored locally in auth.json, mode 0600)")
                }
                KeyTarget::Tavily => {
                    "Tavily API key for web.search (stored locally in auth.json, mode 0600)"
                        .to_owned()
                }
            };
            render_masked_prompt(frame, area, state, theme, &title, &buffer.0);
        }
        Overlay::ApiKeyRemoveConfirm { target } => {
            let (what, effect) = match target {
                KeyTarget::Model(id) => (
                    format!("Remove the saved key for {id}?"),
                    "The model falls back to its api_key_env (if any) on the next run.",
                ),
                KeyTarget::Tavily => (
                    "Remove the saved Tavily key?".to_owned(),
                    "web.search stops using it immediately (env fallback may remain).",
                ),
            };
            render_confirm_box(frame, area, state, theme, &what, effect);
        }
        // The block-edit prompt floats over the Docs browser it opened from, so the
        // editor stays in view while the writer types the insertion.
        Overlay::DocEdit { buffer, .. } => {
            render_docs(frame, area, state, theme);
            render_prompt(
                frame,
                area,
                state,
                theme,
                "Insert text into the focused block",
                buffer,
            );
        }
        Overlay::DocPublishPath { buffer, .. } => {
            render_docs(frame, area, state, theme);
            render_prompt(
                frame,
                area,
                state,
                theme,
                "Publish to repository Markdown path (approval required)",
                buffer,
            );
        }
        Overlay::AddModelId { buffer, .. } => {
            render_prompt(
                frame,
                area,
                state,
                theme,
                "Model name (provider-side id)",
                buffer,
            );
        }
        Overlay::AddModelKey { buffer, .. } => {
            render_masked_prompt(
                frame,
                area,
                state,
                theme,
                "API key (stored locally, mode 0600)",
                &buffer.0,
            );
        }
        Overlay::AddModelProviderKey {
            provider_id,
            buffer,
        } => {
            render_masked_prompt(
                frame,
                area,
                state,
                theme,
                &format!(
                    "API key for {provider_id} (used to list its models; stored locally 0600)"
                ),
                &buffer.0,
            );
        }
        Overlay::AddModelQuerying { provider_id, .. } => {
            render_querying(frame, area, state, theme, provider_id);
        }
        Overlay::AddModelPick {
            provider_id,
            models,
            query,
            selected,
            origin,
            refreshing,
            ..
        } => {
            render_add_model_pick(
                frame,
                area,
                state,
                theme,
                provider_id,
                models,
                query,
                *selected,
                origin,
                *refreshing,
            );
        }
        Overlay::None => {
            if state.show_approval_modal() {
                render_approval_modal(frame, area, state, theme);
            }
        }
    }
}

/// Persistent first-run/runtime diagnostics. The left rail keeps every issue
/// reachable; the detail rail adds a concrete recovery action for common setup
/// failures without trying to parse errors into control flow.
fn render_issues(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect_min(82, 76, 60, 14, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Setup & diagnostics ({}) ", state.issues.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(if state.issues.is_empty() {
            theme.status.success
        } else {
            theme.status.warning
        }))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(rows[0]);

    let visible = cols[0].height as usize;
    let first = first_visible_row(state.selected_issue, state.issues.len(), visible.max(1));
    let mut items = Vec::new();
    if state.issues.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  ✓ all clear",
            Style::default().fg(theme.status.success),
        )));
    }
    for (idx, issue) in state.issues.iter().enumerate().skip(first) {
        let selected = idx == state.selected_issue;
        let item = ListItem::new(Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                "! ",
                theme.selection_aware_text_style(selected, theme.status.warning),
            ),
            Span::styled(
                truncate(issue, cols[0].width.saturating_sub(5) as usize),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]));
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        cols[0],
    );
    for (screen_row, idx) in (first..state.issues.len()).enumerate() {
        if screen_row >= cols[0].height as usize {
            break;
        }
        state.register_hit(
            Rect {
                x: cols[0].x,
                y: cols[0].y + screen_row as u16,
                width: cols[0].width,
                height: 1,
            },
            Action::ActivateRow(idx),
        );
    }

    let detail = if let Some(issue) = state.issues.get(state.selected_issue) {
        vec![
            section("What happened", theme),
            Line::styled(
                format!("  {issue}"),
                Style::default().fg(theme.text.primary),
            ),
            Line::raw(""),
            section("Recommended action", theme),
            Line::styled(
                format!("  {}", issue_guidance(issue)),
                Style::default().fg(theme.text.secondary),
            ),
        ]
    } else {
        vec![
            Line::styled(
                "✓ Codypendent is ready.",
                Style::default()
                    .fg(theme.status.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                "No persistent setup or runtime diagnostics are active.",
                Style::default().fg(theme.text.muted),
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(detail)
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(theme.focus.inactive)),
            )
            .wrap(Wrap { trim: false }),
        cols[1],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "  ↑/↓ select · Delete clear resolved diagnostics · Esc close",
            Style::default().fg(theme.text.muted),
        )),
        rows[1],
    );
}

fn issue_guidance(issue: &str) -> &'static str {
    let issue = issue.to_lowercase();
    if issue.contains("model picker") || issue.contains("models.toml") {
        "Open / → Provider catalog, choose a provider marked ready, and add a model."
    } else if issue.contains("auth.json") || issue.contains("key") {
        "Open / → API keys, then set or replace the affected local credential."
    } else if issue.contains("workflow") {
        "Open Workflow graph to inspect manifests and the latest durable run state."
    } else if issue.contains("knowledge") || issue.contains("code graph") {
        "Keep working normally, then reopen the relevant inspector after the repository index is ready."
    } else {
        "Review the message above; resolve the underlying configuration, then clear this diagnostic."
    }
}

/// Core-owned lifecycle UI for installed Remote UI plugins. This surface is
/// rendered by the trusted host, never by plugin code, so permission diffs,
/// approval receipts, enablement scope, and revocation controls cannot be
/// spoofed by the extension whose authority they govern.
fn render_ui_plugins(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(90, 86, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(
                " Remote UI plugins ({}) · host-owned ",
                state.ui_plugins.len()
            ),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(rows[0]);

    const ROW_LINES: usize = 2;
    let visible = (cols[0].height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_ui_plugin, state.ui_plugins.len(), visible);
    let mut items = Vec::new();
    if state.ui_plugins.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No installed Remote UI plugins",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Install one with `codypendent plugin install`",
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    for (index, plugin) in state
        .ui_plugins
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
    {
        let selected = index == state.selected_ui_plugin;
        let head = Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                format!("{} v{}", truncate(&plugin.id, 24), plugin.version),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]);
        let meta = Line::styled(
            format!(
                "    {}{}",
                plugin.state,
                plugin
                    .enabled_scope
                    .as_ref()
                    .map_or_else(String::new, |scope| format!(" · {scope}"))
            ),
            theme.selection_aware_text_style(selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, meta]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        cols[0],
    );
    for (screen_row, index) in (first..state.ui_plugins.len()).take(visible).enumerate() {
        if let Some(hit) = visible_row_hit(cols[0], screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(index));
        }
    }

    let detail = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive));
    let detail_inner = detail.inner(cols[1]);
    frame.render_widget(detail, cols[1]);
    let mut lines = Vec::new();
    if let Some(plugin) = state.focused_ui_plugin() {
        lines.push(Line::styled(
            format!("{} v{}", plugin.id, plugin.version),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(format!("  state: {}", plugin.state)));
        lines.push(Line::from(format!(
            "  scope: {}",
            plugin.enabled_scope.as_deref().unwrap_or("disabled")
        )));
        lines.push(Line::default());
        if let Some(diff) = &plugin.update_permission_diff {
            lines.push(Line::styled(
                "Permission update (host verified):",
                Style::default()
                    .fg(theme.status.warning)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.extend(diff.lines().map(|line| Line::from(format!("  {line}"))));
        } else {
            lines.push(Line::styled(
                "No permission-expanding update is pending.",
                Style::default().fg(theme.text.muted),
            ));
        }
        if let Some(receipt) = &plugin.update_approval_receipt {
            lines.push(Line::default());
            lines.push(Line::styled(
                "Exact approval receipt:",
                Style::default().fg(theme.text.secondary),
            ));
            // Never truncate the receipt: the exact daemon-issued value is the
            // capability the approval/rejection command consumes.
            lines.push(Line::from(receipt.clone()));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_inner,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "  ↑/↓ select · s smoke-test · t enable session · u enable user",
                Style::default().fg(theme.text.muted),
            ),
            Line::styled(
                "  a approve exact receipt · r reject · x revoke · Esc close",
                Style::default().fg(theme.focus.active),
            ),
        ]),
        rows[1],
    );
}

/// The Skill Studio browser (STEP 2.6): a scrollable list of registered items on
/// the left, and a detail panel on the right that renders the selected skill's
/// metadata, description, risk, and — the exit-criterion payload — its requested
/// **permissions verbatim**. Colors are Theme tokens only (RULE 7).
fn render_skills(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(84, 84, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Skill Studio · read only ({}) ", state.skills.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(inner);

    // Left: the item list (name + scope · trust · status). Each row is 2 lines
    // tall; window the list around the selected skill so a long registry scrolls.
    const ROW_LINES: usize = 2;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_skill, state.skills.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.skills.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No registered skills",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Registry inspection only; installation is not wired here.",
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    for (idx, skill) in state
        .skills
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let selected = idx == state.selected_skill;
        let marker = if selected { "› " } else { "  " };
        let head = Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&skill.name, 26),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]);
        let meta = Line::styled(
            format!("    {} · {} · {}", skill.scope, skill.trust, skill.status),
            theme.selection_aware_text_style(selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, meta]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        list_area,
    );
    for (screen_row, idx) in (first..state.skills.len()).take(visible_rows).enumerate() {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }

    // Right: the detail panel for the focused skill.
    let detail_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    let detail_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(detail_inner);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(skill) = state.focused_skill() {
        lines.push(Line::styled(
            format!("{} — {}", skill.name, skill.kind),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ));
        let field = |k: &str, v: &str, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v.to_owned(), Style::default().fg(color)),
            ])
        };
        lines.push(field("scope", &skill.scope, theme.text.primary));
        lines.push(field("trust", &skill.trust, theme.text.secondary));
        lines.push(field("status", &skill.status, theme.text.secondary));
        lines.push(field(
            "risk",
            &skill.risk,
            skill_risk_color(&skill.risk, theme),
        ));
        lines.push(Line::raw(""));
        lines.push(section("Description", theme));
        lines.push(Line::styled(
            format!("  {}", skill.description),
            Style::default().fg(theme.text.primary),
        ));
        lines.push(Line::raw(""));
        lines.push(section("Permissions", theme));
        if skill.permissions.is_empty() {
            lines.push(Line::styled(
                "  (no permissions requested)",
                Style::default().fg(theme.text.muted),
            ));
        } else {
            // Verbatim: each requested capability exactly as the package declared
            // it — never paraphrased ("skill permissions are visible").
            for permission in &skill.permissions {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme.status.warning)),
                    Span::styled(permission.clone(), Style::default().fg(theme.text.primary)),
                ]));
            }
        }
    } else {
        lines.push(Line::styled(
            "  no skill selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "  ↑/↓ skill · M memory · Esc close",
            Style::default().fg(theme.text.muted),
        )),
        detail_rows[1],
    );

    if detail_rows[1].height >= 1 {
        for (offset, width, action) in [(14, 8, Action::OpenMemory), (25, 9, Action::Dismiss)] {
            state.register_hit(
                Rect {
                    x: detail_rows[1].x.saturating_add(offset),
                    y: detail_rows[1].y,
                    width: width.min(detail_rows[1].width.saturating_sub(offset)),
                    height: 1,
                },
                action,
            );
        }
    }
}

/// The first row to render so `selected` stays visible in a list viewport that
/// fits `visible_rows` rows. The knowledge/model/provider browsers hold no
/// scroll state, so this is recomputed each frame from `selected` alone: it
/// keeps the selection roughly centered (scrolling only enough to reveal it,
/// pinned at the ends). Without it a stateless [`List`] renders from row 0 and
/// the selection walks off the bottom of a long catalog while the detail pane
/// (which reads the index) keeps updating — the "it doesn't scroll" bug.
fn first_visible_row(selected: usize, total: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 || total <= visible_rows {
        return 0;
    }
    let max_first = total - visible_rows;
    selected.saturating_sub(visible_rows / 2).min(max_first)
}

/// The model picker (MP1): a filter line (the command-palette shape) over a
/// two-column list+detail view (the [`render_skills`] template) — the
/// selectable models on the left (current run's serving model marked), and a
/// detail panel for the focused model's provider/location/cost/context on the
/// right. Selecting a row stages it on [`AppState::pending_model`], which PINS
/// the model for the run(s) the operator starts (STEP MP2 — a session default:
/// one pick applies to this run and every subsequent one until changed). Colors
/// are Theme tokens only (RULE 7).
fn render_model_picker(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    query: &str,
    selected: usize,
) {
    let matches = filter_models(&state.models, query);
    let rect = centered_modal(area, 124, 34);
    let inner = modal_surface(
        frame,
        rect,
        format!(
            "Model picker  ·  {} of {} available",
            matches.len(),
            state.models.len()
        ),
        state,
        theme,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    let (list_region, detail_region) = picker_regions(rows[1]);

    // The active run's serving model, if any — marks the current row/detail.
    let current = state.selected_run().and_then(|run| run.model.as_ref());

    // Left: the filtered model list (id, current marker, provider + badges).
    // Each row is 3 lines tall; window the list around `selected` so it scrolls.
    const ROW_LINES: usize = 3;
    let list_block = modal_panel("Models", theme);
    let list_area = list_block.inner(list_region);
    frame.render_widget(list_block, list_region);
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(selected, matches.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.models.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no models configured",
            Style::default().fg(theme.text.muted),
        )));
    } else if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching model",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (row, &idx) in matches.iter().enumerate().skip(first) {
        let card = &state.models[idx];
        let is_selected = row == selected;
        let is_current = current == Some(&card.id);
        let (readiness, readiness_color) = match &card.readiness {
            ModelReadiness::Ready => ("✓ ", theme.status.success),
            ModelReadiness::Unverified => ("? ", theme.status.warning),
            ModelReadiness::Unavailable(_) => ("! ", theme.status.error),
        };
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "▎ " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.focus.active),
            ),
            Span::styled(
                if is_current { "● " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.status.success),
            ),
            Span::styled(
                readiness,
                theme.selection_aware_text_style(is_selected, readiness_color),
            ),
            Span::styled(
                truncate_display_width(&card.id.0, usize::from(list_area.width.saturating_sub(6))),
                theme.selection_aware_text_style(is_selected, theme.text.primary),
            ),
        ]);
        // Provider and badges each get their own line (rather than one long
        // joined line) so they survive the list's fixed-width column without
        // truncating the trailing badges off a narrow terminal.
        let provider_line = Line::styled(
            format!("      {}", card.provider),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let badges_line = Line::styled(
            format!("      {}", model_badges(card)),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, provider_line, badges_line]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    // Each visible row is a fixed 3 lines tall (head/provider/badges) — register
    // a rect of that height per rendered row (offset by the scroll window) so a
    // click maps to the right index even after the list has scrolled.
    for (row, _) in matches.iter().enumerate().skip(first) {
        let Some(hit) = visible_row_hit(list_area, row - first, ROW_LINES as u16) else {
            break;
        };
        state.register_hit(hit, Action::ActivateRow(row));
    }

    // Right: the detail panel for the focused model.
    let detail_block = modal_panel("Model details", theme);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(card) = state.focused_model() {
        let is_current = current == Some(&card.id);
        lines.push(Line::from(vec![
            Span::styled(
                card.id.0.clone(),
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            if is_current {
                Span::styled(
                    "  ● current".to_owned(),
                    Style::default().fg(theme.status.success),
                )
            } else {
                Span::raw("")
            },
        ]));
        let field = |k: &str, v: String, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v, Style::default().fg(color)),
            ])
        };
        lines.push(field(
            "provider",
            card.provider.clone(),
            theme.text.secondary,
        ));
        let (readiness, color) = match &card.readiness {
            ModelReadiness::Ready => ("ready".to_owned(), theme.status.success),
            ModelReadiness::Unverified => (
                "unverified · run doctor --deep".to_owned(),
                theme.status.warning,
            ),
            ModelReadiness::Unavailable(reason) => {
                (format!("unavailable · {reason}"), theme.status.error)
            }
        };
        lines.push(field("readiness", readiness, color));
        lines.push(field(
            "location",
            location_label(card.location).to_owned(),
            theme.text.secondary,
        ));
        lines.push(field(
            "cost",
            cost_label(card.cost_per_1k_usd),
            theme.status.warning,
        ));
        lines.push(field(
            "context",
            context_label(card.context_tokens),
            theme.status.info,
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if matches!(card.readiness, ModelReadiness::Unavailable(_)) {
                "  This model cannot be staged until it is available"
            } else {
                "  Enter stages this model for your next run"
            },
            Style::default().fg(theme.text.muted),
        ));
    } else {
        lines.push(Line::styled(
            "  no model selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  ↑/↓ select · Enter stage · Esc close",
        Style::default().fg(theme.text.muted),
    ));
    if let Some(detail_area) = detail_region {
        frame.render_widget(
            Paragraph::new(lines)
                .block(detail_block)
                .wrap(Wrap { trim: false }),
            detail_area,
        );
    }
}

/// A model card's badges, space-joined: `local ✓` / `hosted` (or nothing when
/// unprofiled), the measured cost per 1K tokens, and the declared context
/// window — each rendered `—` when the model has no measured profile
/// (best-effort; `models.toml` is the authoritative selectable list).
fn model_badges(card: &ModelCard) -> String {
    format!(
        "{} · {} · {}",
        location_label(card.location),
        cost_label(card.cost_per_1k_usd),
        context_label(card.context_tokens)
    )
}

fn location_label(location: Option<ModelLocationLabel>) -> &'static str {
    match location {
        Some(ModelLocationLabel::Local) => "local ✓",
        Some(ModelLocationLabel::Hosted) => "hosted",
        None => "—",
    }
}

fn cost_label(cost_per_1k_usd: Option<f64>) -> String {
    match cost_per_1k_usd {
        Some(cost) => format!("${cost}/1k"),
        None => "—".to_owned(),
    }
}

fn context_label(context_tokens: Option<u64>) -> String {
    match context_tokens {
        Some(tokens) => format!("{}k", tokens / 1000),
        None => "—".to_owned(),
    }
}

/// A catalog price column: USD per 1M tokens, or an em dash when the catalog
/// (and the provider's own listing) said nothing. Display-only — these numbers
/// are never summed into a spend figure.
fn price_per_1m_label(cost_per_1m_usd: Option<f64>) -> String {
    match cost_per_1m_usd {
        Some(cost) => format!("${cost}"),
        None => "—".to_owned(),
    }
}

/// The provider-catalog picker (Task 8): the same filter-line + list/detail
/// shape as [`render_model_picker`], over [`AppState::providers`] instead of
/// `models`. `Enter` (or `Tab`) begins the add-model flow for the focused
/// provider (model-discovery) — the picker no longer stages a provider for
/// later; it acts immediately. Colors are Theme tokens only (RULE 7).
fn render_provider_picker(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    query: &str,
    selected: usize,
) {
    let matches = filter_providers(&state.providers, query);
    let rect = centered_modal(area, 124, 36);
    let inner = modal_surface(
        frame,
        rect,
        format!(
            "Provider catalog  ·  {} of {} adapters",
            matches.len(),
            state.providers.len()
        ),
        state,
        theme,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    let (list_region, detail_region) = picker_regions(rows[1]);

    // Left: the filtered provider list (id, name + protocol, location + auth).
    // Each row is 3 lines
    // tall; window the list around `selected` so a long catalog scrolls.
    const ROW_LINES: usize = 3;
    let list_block = modal_panel("Providers", theme);
    let list_area = list_block.inner(list_region);
    frame.render_widget(list_block, list_region);
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(selected, matches.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.providers.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no providers configured",
            Style::default().fg(theme.text.muted),
        )));
    } else if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching provider",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (row, &idx) in matches.iter().enumerate().skip(first) {
        let card = &state.providers[idx];
        let is_selected = row == selected;
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "▎ " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.focus.active),
            ),
            Span::styled(
                if card.available { "✓ " } else { "○ " },
                theme.selection_aware_text_style(
                    is_selected,
                    if card.available {
                        theme.status.success
                    } else {
                        theme.text.muted
                    },
                ),
            ),
            Span::styled(
                truncate_display_width(&card.id, usize::from(list_area.width.saturating_sub(4))),
                theme.selection_aware_text_style(is_selected, theme.text.primary),
            ),
        ]);
        // Keep the catalog dense enough to browse: availability is encoded by
        // the leading glyph, while the two supporting rows retain the provider
        // name, protocol, location, and auth requirement.
        let name_line = Line::styled(
            format!("      {} · {}", card.name, card.protocol),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let metadata_line = Line::styled(
            format!(
                "      {} · {} · {}",
                provider_location_label(card.local),
                card.auth,
                provider_listing_label(card),
            ),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, name_line, metadata_line]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    // Each visible row is a fixed 3 lines tall (head/name+protocol/metadata) —
    // register a rect of that height per rendered row (offset by the scroll
    // window) so a click maps to the right index even after the list scrolled.
    for (row, _) in matches.iter().enumerate().skip(first) {
        let Some(hit) = visible_row_hit(list_area, row - first, ROW_LINES as u16) else {
            break;
        };
        state.register_hit(hit, Action::ActivateRow(row));
    }

    // Right: the detail panel for the focused provider.
    let detail_block = modal_panel("Provider details", theme);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(card) = state.focused_provider() {
        lines.push(Line::from(vec![Span::styled(
            card.id.clone(),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        )]));
        let field = |k: &str, v: String, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v, Style::default().fg(color)),
            ])
        };
        lines.push(field("name", card.name.clone(), theme.text.secondary));
        lines.push(field(
            "protocol",
            card.protocol.clone(),
            theme.text.secondary,
        ));
        lines.push(field("auth", card.auth.clone(), theme.status.warning));
        lines.push(field(
            "location",
            provider_location_label(card.local).to_owned(),
            theme.status.info,
        ));
        // What the add flow can actually offer here — a live listing, curated
        // catalog rows, or (only when there is neither) a typed model name.
        lines.push(field(
            "models",
            provider_listing_label(card),
            theme.status.info,
        ));
        lines.push(field(
            "status",
            if card.available {
                "supported adapter".to_owned()
            } else {
                "preview · adapter unavailable".to_owned()
            },
            if card.available {
                theme.status.success
            } else {
                theme.status.warning
            },
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if card.available {
                "  Enter or Tab — browse this provider's models to add one"
            } else {
                "  Runtime adapter not installed — this provider cannot be added yet"
            },
            Style::default().fg(if card.available {
                theme.text.muted
            } else {
                theme.status.warning
            }),
        ));
    } else {
        lines.push(Line::styled(
            "  no provider selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  ↑/↓ select · Enter/Tab add model · Esc close",
        Style::default().fg(theme.text.muted),
    ));
    if let Some(detail_area) = detail_region {
        frame.render_widget(
            Paragraph::new(lines)
                .block(detail_block)
                .wrap(Wrap { trim: false }),
            detail_area,
        );
    }
}

/// The mode picker (PR C2 — plan mode): the same filter-line + list shape as
/// [`render_model_picker`], over the static [`MODE_CARDS`] table — five rows,
/// so no detail pane and no scroll windowing. The row matching
/// [`AppState::default_mode`] is marked current; `Enter` stages the focused
/// row as the next run's submission mode. Colors are Theme tokens only
/// (RULE 7).
fn render_mode_picker(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    query: &str,
    selected: usize,
) {
    let matches = filter_modes(query);
    let rect = centered_modal(area, 72, 18);
    let inner = modal_surface(
        frame,
        rect,
        format!(
            "Mode picker  ·  {} of {} modes",
            matches.len(),
            crate::state::MODE_CARDS.len()
        ),
        state,
        theme,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    // The filtered mode list: each row is 2 lines tall (label, summary), the
    // current default marked ●. The modal has an absolute minimum height so all
    // five rows fit on an ordinary 80x24 terminal.
    const ROW_LINES: usize = 2;
    let list_block = modal_panel(
        format!(
            "Modes  ·  {} of {}",
            matches.len(),
            crate::state::MODE_CARDS.len()
        ),
        theme,
    );
    let list_area = list_block.inner(rows[1]);
    frame.render_widget(list_block, rows[1]);
    let visible_rows = (usize::from(list_area.height) / ROW_LINES).max(1);
    let first = first_visible_row(selected, matches.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching mode",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (row, &idx) in matches.iter().enumerate().skip(first) {
        let card = &crate::state::MODE_CARDS[idx];
        let is_selected = row == selected;
        let is_current = card.mode == state.default_mode;
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "▎ " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.focus.active),
            ),
            Span::styled(
                if is_current { "● " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.status.success),
            ),
            Span::styled(
                card.label,
                theme.selection_aware_text_style(is_selected, theme.text.primary),
            ),
        ]);
        let summary_line = Line::styled(
            format!("      {}", card.summary),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, summary_line]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    // Each visible row is a fixed 2 lines tall (label/summary) — register a
    // rect of that height per row so a click maps to the right index.
    for (row, _) in matches.iter().enumerate().skip(first) {
        let Some(hit) = visible_row_hit(list_area, row - first, ROW_LINES as u16) else {
            break;
        };
        state.register_hit(hit, Action::ActivateRow(row));
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            "↑/↓ select  ·  Enter use  ·  Esc close",
            Style::default().fg(theme.text.muted),
        ))
        .alignment(Alignment::Center),
        rows[2],
    );
}

/// The `/keys` overlay (D1): the same filter-line + list shape as
/// [`render_mode_picker`], over one row per configured model plus a final
/// `Tavily (web.search)` row. Each row shows a status GLYPH (● saved in
/// auth.json / ◐ an `api_key_env` NAME / ○ missing) and a detail line — never
/// any key material: [`KeyStatus`] carries no values by construction, and the
/// env variant holds the variable NAME only. `Enter` opens the masked
/// set/replace prompt; `d` removes a stored key. Colors are Theme tokens only
/// (RULE 7).
fn render_api_keys(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    query: &str,
    selected: usize,
) {
    let matches = filter_key_rows(&state.models, query);
    let total = state.models.len().saturating_add(1);
    let rect = centered_modal(area, 84, 24);
    let inner = modal_surface(
        frame,
        rect,
        format!("API keys  ·  {} of {} entries", matches.len(), total),
        state,
        theme,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    // The filtered row list: each row is 2 lines tall (glyph + id, provider ·
    // status detail), windowed around the selection so a long model list
    // scrolls (the model picker's shape).
    const ROW_LINES: usize = 2;
    let list_block = modal_panel(
        format!("Credentials  ·  {} of {}", matches.len(), total),
        theme,
    );
    let list_area = list_block.inner(rows[1]);
    frame.render_widget(list_block, rows[1]);
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(selected, matches.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching model",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (row, &idx) in matches.iter().enumerate().skip(first) {
        // The row's label, provider/sub-line prefix, and status: indices into
        // `state.models` are model rows; `models.len()` is the Tavily row.
        let (label, provider, status) = match state.models.get(idx) {
            Some(card) => {
                let status = state
                    .key_status
                    .iter()
                    .find(|(id, _)| id == &card.id.0)
                    .map(|(_, status)| status)
                    .unwrap_or(&KeyStatus::Missing);
                (card.id.0.clone(), card.provider.clone(), status.clone())
            }
            None => (
                "Tavily (web.search)".to_owned(),
                "web search".to_owned(),
                state.tavily_key_status.clone(),
            ),
        };
        let (glyph, glyph_color, detail) = key_status_render(&status, theme);
        let is_selected = row == selected;
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "▎ " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.focus.active),
            ),
            Span::styled(
                format!("{glyph} "),
                theme.selection_aware_text_style(is_selected, glyph_color),
            ),
            Span::styled(
                truncate_display_width(&label, usize::from(list_area.width.saturating_sub(4))),
                theme.selection_aware_text_style(is_selected, theme.text.primary),
            ),
        ]);
        // A model row that has actually been probed reports the result here,
        // so a verified key reads as verified rather than staying "Unverified"
        // forever. `Ctrl-T` is what fills this in.
        let verified = state
            .models
            .get(idx)
            .map(|card| match &card.readiness {
                ModelReadiness::Ready => " · verified ✓".to_owned(),
                ModelReadiness::Unavailable(reason) => {
                    format!(" · {}", truncate_display_width(reason, 40))
                }
                ModelReadiness::Unverified => String::new(),
            })
            .unwrap_or_default();
        let detail_line = Line::styled(
            format!("      {provider} · {detail}{verified}"),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, detail_line]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    // Each visible row is a fixed 2 lines tall (head/detail) — register a rect
    // of that height per rendered row (offset by the scroll window) so a click
    // maps to the right index even after the list scrolled.
    for (row, _) in matches.iter().enumerate().skip(first) {
        let Some(hit) = visible_row_hit(list_area, row - first, ROW_LINES as u16) else {
            break;
        };
        state.register_hit(hit, Action::ActivateRow(row));
    }

    let hint = Line::styled(
        "↑/↓ select · Enter set/replace · Ctrl-T verify · Delete remove · Esc close",
        Style::default().fg(theme.text.muted),
    )
    .alignment(Alignment::Center);
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().bg(theme.surface.overlay)),
        rows[2],
    );
}

/// A `/keys` row's status rendering (D1): the glyph, its color, and the detail
/// text. Never any key material — the env variant shows the variable NAME
/// only.
fn key_status_render(status: &KeyStatus, theme: &Theme) -> (&'static str, Color, String) {
    match status {
        KeyStatus::Stored => (
            "●",
            theme.status.success,
            "key saved (auth.json)".to_owned(),
        ),
        KeyStatus::Env(name) => ("◐", theme.status.warning, format!("env {name}")),
        KeyStatus::Missing => ("○", theme.text.muted, "no key configured".to_owned()),
    }
}

/// A small yes/no confirm box in the [`render_confirm`] shape, parameterized
/// so run/workflow cancellation and `/keys` removal share a compact, responsive
/// shape. Text is key-free by construction (it names a target, never a value).
fn render_confirm_box(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    title: &str,
    detail: &str,
) {
    let rect = centered_rect_min(60, 20, 48, 7, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::styled(
            title.to_owned(),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(detail.to_owned(), Style::default().fg(theme.text.secondary)),
        Line::from(vec![
            Span::styled("[y] yes   ", Style::default().fg(theme.status.warning)),
            Span::styled("[n] no", Style::default().fg(theme.status.success)),
        ]),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirm ")
        .border_style(Style::default().fg(theme.status.warning))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn provider_location_label(local: bool) -> &'static str {
    if local {
        "local ✓"
    } else {
        "hosted"
    }
}

/// What the add-model flow can offer for this provider: a live `/models`
/// listing, the curated catalog rows, both, or neither (free-text). Stated on
/// the card so the operator knows before pressing Enter whether they are about
/// to browse or to type.
fn provider_listing_label(card: &ProviderCard) -> String {
    match (card.can_list_models, card.catalog_models) {
        (true, 0) => "live list ✓".to_owned(),
        (true, n) => format!("live list ✓ · catalog {n}"),
        (false, 0) => "type the model name".to_owned(),
        (false, n) => format!("catalog {n} models"),
    }
}

/// The memory browser (STEP 2.6): the visible-scope memories on the left, and a
/// Chapter 06 provenance card for the focused memory on the right (fact, source,
/// revision, observed, scope, confidence), with an "open source" affordance.
/// When `source_open`, the full source string is surfaced in place — the TUI
/// does no I/O, so opening reveals rather than launches a file.
fn render_memory(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    source_open: bool,
) {
    let rect = centered_rect(84, 84, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Memory ({}) ", state.memories.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(inner);

    // Left: the memory list (statement + class · scope). Each row is 2 lines
    // tall; window the list around the selected memory so a long scope scrolls.
    const ROW_LINES: usize = 2;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_memory, state.memories.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.memories.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No curated memories yet",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Durable facts appear after completed runs.",
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    for (idx, memory) in state
        .memories
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let selected = idx == state.selected_memory;
        let marker = if selected { "› " } else { "  " };
        let head = Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&memory.statement, 26),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]);
        let meta = Line::styled(
            format!("    {} · {}", memory.class, memory.scope),
            theme.selection_aware_text_style(selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, meta]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        list_area,
    );
    for (screen_row, idx) in (first..state.memories.len()).take(visible_rows).enumerate() {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }

    // Right: the provenance card for the focused memory.
    let card_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let card_inner = card_block.inner(cols[1]);
    frame.render_widget(card_block, cols[1]);
    let card_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(card_inner);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(memory) = state.focused_memory() {
        let field = |k: &str, v: &str, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v.to_owned(), Style::default().fg(color)),
            ])
        };
        lines.push(section("Provenance card", theme));
        lines.push(field("Fact", &memory.statement, theme.text.primary));
        lines.push(field("Source", &memory.source, theme.text.secondary));
        lines.push(field("Revision", &memory.revision, theme.text.secondary));
        lines.push(field("Observed", &memory.observed, theme.text.secondary));
        lines.push(field("Scope", &memory.scope, theme.text.secondary));
        lines.push(field(
            "Confidence",
            &format!("{:.2}", memory.confidence),
            theme.status.info,
        ));
        lines.push(Line::raw(""));
        if source_open {
            // Opened: surface the full source string, marked as revealed.
            lines.push(Line::styled(
                "  ▼ source opened",
                Style::default()
                    .fg(theme.status.success)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!("    {}", memory.source),
                Style::default().fg(theme.text.primary),
            ));
        } else {
            lines.push(Line::styled(
                "  [o] open source",
                Style::default().fg(theme.status.info),
            ));
        }
    } else {
        lines.push(Line::styled(
            "  no memory selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        card_rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "  ↑/↓ memory · o source",
                Style::default().fg(theme.focus.active),
            ),
            Line::styled(
                "  S skills · Esc close",
                Style::default().fg(theme.text.muted),
            ),
        ]),
        card_rows[1],
    );

    if card_rows[1].height >= 1 {
        state.register_hit(
            Rect {
                x: card_rows[1].x.saturating_add(15),
                y: card_rows[1].y,
                width: 8.min(card_rows[1].width.saturating_sub(15)),
                height: 1,
            },
            Action::OpenSource,
        );
    }
    if card_rows[1].height >= 2 {
        for (offset, width, action) in [(2, 8, Action::OpenSkills), (13, 9, Action::Dismiss)] {
            state.register_hit(
                Rect {
                    x: card_rows[1].x.saturating_add(offset),
                    y: card_rows[1].y.saturating_add(1),
                    width: width.min(card_rows[1].width.saturating_sub(offset)),
                    height: 1,
                },
                action,
            );
        }
    }
}

/// The Docs Studio browser (Phase 4 client wiring): a document **tree** on the
/// left; on the right, the focused document's **editor rail** (its blocks in
/// order) over its **review rail** (pending suggestions). Edits, suggestion
/// decisions, live CRDT sync, and approval-gated Markdown publishing all use
/// this surface. Colors are Theme tokens only (RULE 7).
fn render_docs(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(86, 86, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Docs Studio · existing docs ({}) ", state.docs.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(inner);

    // Left: the document tree (title + scope · status · mode). Each row is 2
    // lines tall; window the tree around the selected document so a large
    // repository scrolls.
    const ROW_LINES: usize = 2;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_doc, state.docs.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.docs.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No collaborative documents yet",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Creation is not wired here yet; this view edits existing documents.",
                Style::default().fg(theme.status.warning),
            ),
        ]));
    }
    for (idx, doc) in state.docs.iter().enumerate().skip(first).take(visible_rows) {
        let selected = idx == state.selected_doc;
        let marker = if selected { "› " } else { "  " };
        let head = Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&doc.title, 28),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
        ]);
        let meta = Line::styled(
            format!("    {} · {} · {}", doc.scope, doc.status, doc.mode),
            theme.selection_aware_text_style(selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, meta]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        list_area,
    );
    for (screen_row, idx) in (first..state.docs.len()).take(visible_rows).enumerate() {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::SelectDocument(idx));
        }
    }

    // Right: the editor rail (blocks) over the review rail (suggestions).
    let rails = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(cols[1]);

    let editor_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let editor_inner = editor_block.inner(rails[0]);
    frame.render_widget(editor_block, rails[0]);
    let mut editor_lines: Vec<Line> = Vec::new();
    if let Some(doc) = state.focused_doc() {
        editor_lines.push(Line::styled(
            truncate(
                &format!("{} ({})", doc.title, doc.revision),
                editor_inner.width as usize,
            ),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ));
        // The editor rail header carries the presence-lite lease indicator: whether
        // this client holds / is acquiring / is blocked on a block lease.
        let editing = state.doc_focus == DocFocus::Editor;
        editor_lines.push(Line::from(vec![
            section_span("Editor rail", theme),
            Span::styled(
                if editing { "  [focused]" } else { "" }.to_owned(),
                Style::default().fg(theme.focus.active),
            ),
            lease_span(state, theme),
        ]));
        if doc.blocks.is_empty() {
            editor_lines.push(Line::styled(
                "  (empty document)",
                Style::default().fg(theme.text.muted),
            ));
        }
        let visible_blocks = editor_inner.height.saturating_sub(2) as usize;
        let first_block = first_visible_row(
            state.selected_block,
            doc.blocks.len(),
            visible_blocks.max(1),
        );
        for (idx, block) in doc
            .blocks
            .iter()
            .enumerate()
            .skip(first_block)
            .take(visible_blocks)
        {
            let focused = editing && idx == state.selected_block;
            let marker = if focused { "› " } else { "  " };
            let kind_style = if focused {
                Style::default().fg(theme.focus.active)
            } else {
                Style::default().fg(theme.text.secondary)
            };
            editor_lines.push(Line::from(vec![
                Span::styled(format!("{marker}{:<10}", block.kind), kind_style),
                Span::styled(
                    truncate(&block.text, editor_inner.width.saturating_sub(12) as usize),
                    Style::default().fg(theme.text.primary),
                ),
            ]));
        }
        for (screen_row, idx) in (first_block..doc.blocks.len())
            .take(visible_blocks)
            .enumerate()
        {
            state.register_hit(
                Rect {
                    x: editor_inner.x,
                    y: editor_inner.y + 2 + screen_row as u16,
                    width: editor_inner.width,
                    height: 1,
                },
                Action::SelectDocumentBlock(idx),
            );
        }
    } else {
        editor_lines.push(Line::styled(
            "  no document selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(editor_lines).wrap(Wrap { trim: false }),
        editor_inner,
    );

    let review_block = Block::default()
        .borders(Borders::LEFT | Borders::TOP)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let review_inner = review_block.inner(rails[1]);
    frame.render_widget(review_block, rails[1]);
    let review_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(review_inner);
    let mut review_lines: Vec<Line> = Vec::new();
    if let Some(doc) = state.focused_doc() {
        let reviewing = state.doc_focus == DocFocus::Review;
        review_lines.push(Line::from(vec![
            section_span("Review rail (suggestions)", theme),
            Span::styled(
                if reviewing { "  [focused]" } else { "" }.to_owned(),
                Style::default().fg(theme.focus.active),
            ),
        ]));
        if doc.suggestions.is_empty() {
            review_lines.push(Line::styled(
                "  no pending suggestions",
                Style::default().fg(theme.text.muted),
            ));
        }
        let visible_suggestions = review_rows[0].height.saturating_sub(1) as usize;
        let first_suggestion = first_visible_row(
            state.selected_suggestion,
            doc.suggestions.len(),
            visible_suggestions.max(1),
        );
        for (idx, suggestion) in doc
            .suggestions
            .iter()
            .enumerate()
            .skip(first_suggestion)
            .take(visible_suggestions)
        {
            let focused = reviewing && idx == state.selected_suggestion;
            let bullet = if focused { "› " } else { "  • " };
            let bullet_style = if focused {
                Style::default().fg(theme.focus.active)
            } else {
                Style::default().fg(theme.status.info)
            };
            let rationale = suggestion
                .rationale
                .as_deref()
                .map_or_else(String::new, |text| format!(" · {text}"));
            let summary = format!(
                "{} @ {}{} → {}",
                suggestion.author, suggestion.range, rationale, suggestion.replacement
            );
            review_lines.push(Line::from(vec![
                Span::styled(bullet, bullet_style),
                Span::styled(
                    truncate(&summary, review_rows[0].width.saturating_sub(4) as usize),
                    Style::default().fg(theme.text.primary),
                ),
            ]));
        }
        for (screen_row, idx) in (first_suggestion..doc.suggestions.len())
            .take(visible_suggestions)
            .enumerate()
        {
            state.register_hit(
                Rect {
                    x: review_rows[0].x,
                    y: review_rows[0].y + 1 + screen_row as u16,
                    width: review_rows[0].width,
                    height: 1,
                },
                Action::SelectDocumentSuggestion(idx),
            );
        }
    }
    frame.render_widget(
        Paragraph::new(review_lines).wrap(Wrap { trim: false }),
        review_rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                " Tab rail · ↑/↓ select · Esc close",
                Style::default().fg(theme.text.muted),
            ),
            Line::styled(
                " e edit · a accept · r reject · P publish",
                Style::default().fg(theme.focus.active),
            ),
        ]),
        review_rows[1],
    );

    // Fixed footer controls stay clickable even when long documents or review
    // queues make the content rails scroll.
    state.register_hit(
        Rect {
            x: review_rows[1].x + 1,
            y: review_rows[1].y,
            width: 8.min(review_rows[1].width.saturating_sub(1)),
            height: 1,
        },
        Action::CyclePane,
    );
    let mut x = review_rows[1].x + 1;
    for (width, action) in [
        (6, Action::EditDoc),
        (
            8,
            Action::Approve(codypendent_protocol::ApprovalScope::Once),
        ),
        (8, Action::Reject),
        (9, Action::PublishDoc),
    ] {
        state.register_hit(
            Rect {
                x,
                y: review_rows[1].y + 1,
                width: width.min(review_rows[1].right().saturating_sub(x)),
                height: 1,
            },
            action,
        );
        x = x.saturating_add(width + 3);
    }
}

/// The code-graph edge inspector (Phase 4 exit criterion 4): the repository's
/// edges on the left, and for the focused edge its relation, confidence,
/// evidence kind + source, and revision on the right — the evidence-and-revision
/// payload the criterion calls for. Colors are Theme tokens only (RULE 7).
fn render_edges(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if state.edge_loading && state.edges.is_empty() && state.edge_total == 0 {
        render_loading_edges(frame, area, state, theme);
        return;
    }
    if state.edges.is_empty() && state.edge_total == 0 {
        render_empty_edges(frame, area, state, theme);
        return;
    }

    let rect = centered_modal(area, 132, 36);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let first_match = if state.edge_total == 0 {
        0
    } else {
        state.edge_page * crate::state::EDGE_PAGE_SIZE + 1
    };
    let last_match =
        (state.edge_page * crate::state::EDGE_PAGE_SIZE + state.edges.len()).min(state.edge_total);
    let query = if state.edge_query.is_empty() {
        String::new()
    } else {
        format!(" · filter ‘{}’", truncate(&state.edge_query, 24))
    };
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(
                " Code graph ({first_match}–{last_match} of {}{query}) ",
                state.edge_total
            ),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(inner);

    // Left: the edge list (relation, then from → to). Each row is 2 lines tall;
    // window it around the selected edge so a large graph scrolls.
    const ROW_LINES: usize = 2;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_edge, state.edges.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.edges.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no edges in this repository",
            Style::default().fg(theme.text.muted),
        )));
    }
    for (idx, edge) in state
        .edges
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let selected = idx == state.selected_edge;
        let marker = if selected { "› " } else { "  " };
        let head = Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&edge.relation, 14),
                theme.selection_aware_text_style(selected, theme.text.secondary),
            ),
        ]);
        let meta = Line::styled(
            format!(
                "    {} → {}",
                truncate(&edge.from, 16),
                truncate(&edge.to, 16)
            ),
            theme.selection_aware_text_style(selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, meta]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        cols[0],
    );
    for (screen_row, idx) in (first..state.edges.len()).take(visible_rows).enumerate() {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }

    // Right: the detail for the focused edge — relation, confidence, and the
    // exit-criterion payload: evidence kind + source + revision.
    let detail_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    let detail_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(detail_inner);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(edge) = state.focused_edge() {
        let field = |k: &str, v: &str, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v.to_owned(), Style::default().fg(color)),
            ])
        };
        lines.push(section("Edge", theme));
        lines.push(field("from", &edge.from, theme.text.primary));
        lines.push(field("to", &edge.to, theme.text.primary));
        lines.push(field("relation", &edge.relation, theme.text.secondary));
        lines.push(field(
            "confidence",
            &format!("{:.2}", edge.confidence),
            edge_confidence_color(edge.confidence, theme),
        ));
        lines.push(Line::raw(""));
        lines.push(section("Evidence", theme));
        lines.push(field("kind", &edge.evidence_kind, theme.status.info));
        lines.push(field("source", &edge.evidence, theme.text.secondary));
        lines.push(field("revision", &edge.revision, theme.text.secondary));
    } else {
        lines.push(Line::styled(
            "  no edge selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "  ↑/↓ edge · / search",
                Style::default().fg(theme.focus.active),
            ),
            Line::styled(
                "  PgUp prev · PgDn next · Esc close",
                Style::default().fg(theme.text.muted),
            ),
        ]),
        detail_rows[1],
    );

    // The fixed graph footer is mouse-operable as well as keyboard-operable.
    if detail_rows[1].height >= 1 {
        state.register_hit(
            Rect {
                x: detail_rows[1].x.saturating_add(13),
                y: detail_rows[1].y,
                width: 8.min(detail_rows[1].width.saturating_sub(13)),
                height: 1,
            },
            Action::OpenPalette,
        );
    }
    if detail_rows[1].height >= 2 {
        let y = detail_rows[1].y.saturating_add(1);
        let mut x = detail_rows[1].x.saturating_add(2);
        for (width, action) in [
            (9, Action::ScrollPageUp),
            (9, Action::ScrollPageDown),
            (9, Action::Dismiss),
        ] {
            state.register_hit(
                Rect {
                    x,
                    y,
                    width: width.min(detail_rows[1].right().saturating_sub(x)),
                    height: 1,
                },
                action,
            );
            x = x.saturating_add(width + 3);
        }
    }
}

fn render_loading_edges(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let rect = centered_modal(area, 64, 11);
    let inner = modal_surface(frame, rect, "Code graph", state, theme);
    let spinner = SPINNER[(state.tick as usize) % SPINNER.len()];
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(spinner.to_string(), Style::default().fg(theme.agent.tool)),
            Line::raw(""),
            Line::styled(
                "Loading code graph…",
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                if state.edge_query.is_empty() {
                    "Reading indexed repository relationships"
                } else {
                    "Applying the current graph filter"
                },
                Style::default().fg(theme.text.muted),
            ),
        ])
        .alignment(Alignment::Center),
        inner,
    );
}

fn render_empty_edges(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_modal(area, 78, 15);
    let inner = modal_surface(frame, rect, "Code graph", state, theme);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let (title, description) = if state.edge_query.is_empty() {
        (
            "No relationships indexed yet",
            "Edges appear here as Codypendent gathers evidence across the repository.",
        )
    } else {
        (
            "No matching relationships",
            "Try a symbol, file, or relation with a broader search.",
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("◇", Style::default().fg(theme.focus.active)),
            Line::raw(""),
            Line::styled(
                title,
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(description, Style::default().fg(theme.text.secondary)),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "/ search  ·  Esc close",
            Style::default().fg(theme.focus.active),
        ))
        .alignment(Alignment::Center),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "Search by symbol, file, or relation",
            Style::default().fg(theme.text.muted),
        ))
        .alignment(Alignment::Center),
        rows[2],
    );

    state.register_hit(rows[1], Action::OpenPalette);
    state.register_hit(
        Rect {
            x: rows[1].x + rows[1].width / 2,
            y: rows[1].y,
            width: rows[1].width / 2,
            height: 1,
        },
        Action::Dismiss,
    );
}

/// The workflow-graph view (Phase 5 STEP 5.2, exit criterion 3): a list of the
/// compiled workflow's nodes on the left — grouped by workflow, in topological
/// order — and, for the focused node, its action, state, agent, workspace,
/// approval, retry, dependencies, and declared outputs on the right. The live
/// durable run is subscribed while focused and can be started, paused/resumed,
/// retried from the selected node, or cancelled. Colors are Theme tokens only.
fn render_workflow(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(86, 86, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Workflow ({} node(s)) ", state.workflow.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(inner);

    // Left: fixed-height node cards in topological order. Repeating the workflow
    // label makes every card self-contained and, importantly, keeps scrolling
    // exact when a window begins in the middle of a multi-workflow list.
    const ROW_LINES: usize = 3;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_node, state.workflow.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.workflow.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No workflow manifests found",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Add YAML under .codypendent/workflows, then reopen this view.",
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    for (idx, node) in state
        .workflow
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let selected = idx == state.selected_node;
        let marker = if selected { "› " } else { "  " };
        let mut lines = vec![Line::styled(
            truncate(&node.workflow, 36),
            theme
                .selection_aware_text_style(selected, theme.text.heading)
                .add_modifier(Modifier::BOLD),
        )];
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&node.id, 20),
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
            Span::styled(
                "  ",
                theme.selection_aware_text_style(selected, theme.text.primary),
            ),
            Span::styled(
                node.state.clone(),
                theme.selection_aware_text_style(selected, node_state_color(&node.state, theme)),
            ),
        ]));
        lines.push(Line::styled(
            format!("    {}", truncate(&node.action, 34)),
            theme.selection_aware_text_style(selected, theme.text.muted),
        ));
        let item = ListItem::new(lines);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        list_area,
    );
    for (screen_row, idx) in (first..state.workflow.len()).take(visible_rows).enumerate() {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }

    // Right: the detail for the focused node — the exit-criterion payload
    // (state, agent, worktree, cost) plus the graph edges and declared outputs.
    let detail_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    let detail_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(detail_inner);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(node) = state.focused_node() {
        let field = |k: &str, v: &str, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v.to_owned(), Style::default().fg(color)),
            ])
        };
        lines.push(section("Node", theme));
        lines.push(field("workflow", &node.workflow, theme.text.secondary));
        lines.push(field("inputs", &node.inputs, theme.text.secondary));
        lines.push(field("run phase", &node.run_phase, theme.status.info));
        if let Some(run_id) = &node.workflow_run_id {
            lines.push(field("run", run_id, theme.text.muted));
        }
        lines.push(field("id", &node.id, theme.text.primary));
        lines.push(field(
            "state",
            &node.state,
            node_state_color(&node.state, theme),
        ));
        lines.push(Line::raw(""));
        lines.push(section("Action", theme));
        lines.push(field("action", &node.action, theme.text.secondary));
        lines.push(field("agent", &node.agent, theme.text.primary));
        lines.push(field("model policy", &node.model_policy, theme.text.muted));
        lines.push(Line::raw(""));
        lines.push(section("Execution", theme));
        lines.push(field("worktree", &node.workspace, theme.status.info));
        lines.push(field("approval", &node.approval, theme.text.secondary));
        lines.push(field("retry", &node.retry, theme.text.secondary));
        lines.push(field("cost", &node.cost, theme.text.secondary));
        // The durable failure/block reason, when a run recorded one (P5-D4) —
        // shown in the mode's error color so a blocked/failed node explains itself.
        if node.error != "\u{2014}" {
            lines.push(field("error", &node.error, theme.status.error));
        }
        lines.push(Line::raw(""));
        lines.push(section("Graph", theme));
        lines.push(field("depends on", &node.depends_on, theme.text.secondary));
        lines.push(field("outputs", &node.outputs, theme.text.secondary));
    } else {
        lines.push(Line::styled(
            "  no node selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "  n start · p pause/resume · r retry",
                Style::default().fg(theme.focus.active),
            ),
            Line::styled(
                "  c cancel · ↑/↓ node · Esc close",
                Style::default().fg(theme.text.muted),
            ),
        ]),
        detail_rows[1],
    );

    // Mouse parity for the workflow controls. These hit targets align with the
    // visible chips on the two fixed footer lines of the detail rail.
    if detail_rows[1].height >= 1 {
        let y = detail_rows[1].y;
        let mut x = detail_rows[1].x.saturating_add(2);
        for (width, action) in [
            (7, Action::NewRun),
            (14, Action::Pause),
            (7, Action::Reject),
        ] {
            state.register_hit(
                Rect {
                    x,
                    y,
                    width: width.min(detail_rows[1].right().saturating_sub(x)),
                    height: 1,
                },
                action,
            );
            x = x.saturating_add(width + 3);
        }
    }
    if detail_rows[1].height >= 2 {
        state.register_hit(
            Rect {
                x: detail_rows[1].x.saturating_add(2),
                y: detail_rows[1].y.saturating_add(1),
                width: 8.min(detail_rows[1].width.saturating_sub(2)),
                height: 1,
            },
            Action::Cancel,
        );
    }
}

/// Color for a workflow node's lifecycle state. Terminal-success reads calm;
/// active states draw the eye; failure/blocked read as error; not-yet-run
/// (`pending`) and `skipped` stay quiet.
fn node_state_color(state: &str, theme: &Theme) -> Color {
    match state {
        "completed" => theme.status.success,
        "running" => theme.status.running,
        "waiting_approval" => theme.status.warning,
        "failed" | "blocked" => theme.status.error,
        "pending" => theme.status.info,
        _ => theme.text.muted,
    }
}

/// The blackboard view (Phase 5 STEP 5.3): the typed artifacts agents share
/// within a workflow run — a list on the left, grouped by run, and, for the
/// focused item, its kind, author, confidence, evidence, revision, and payload
/// summary on the right. Read-only. Colors are Theme tokens only (RULE 7).
fn render_blackboard(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(86, 86, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Blackboard ({} item(s)) ", state.blackboard.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(inner);

    // Left: fixed-height artifact cards. Repeating the owning run keeps every
    // card understandable and makes selection windowing exact at group edges.
    const ROW_LINES: usize = 3;
    let list_area = cols[0];
    let visible_rows = (list_area.height as usize / ROW_LINES).max(1);
    let first = first_visible_row(state.selected_item, state.blackboard.len(), visible_rows);
    let mut items: Vec<ListItem> = Vec::new();
    if state.blackboard.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(
                "  No workflow artifacts yet",
                Style::default().fg(theme.text.secondary),
            ),
            Line::styled(
                "  Start a workflow; findings and decisions will appear here live.",
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    for (idx, card) in state
        .blackboard
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_rows)
    {
        let selected = idx == state.selected_item;
        let marker = if selected { "› " } else { "  " };
        let mut lines = vec![Line::styled(
            truncate(&card.run, 36),
            theme
                .selection_aware_text_style(selected, theme.text.heading)
                .add_modifier(Modifier::BOLD),
        )];
        // A superseded artifact is dimmed; the live one reads normally.
        let kind_color = if card.superseded {
            theme.text.muted
        } else {
            theme.status.info
        };
        lines.push(Line::from(vec![
            Span::styled(
                marker,
                theme.selection_aware_text_style(selected, theme.focus.active),
            ),
            Span::styled(
                truncate(&card.kind, 16),
                theme.selection_aware_text_style(selected, kind_color),
            ),
            if card.superseded {
                Span::styled(
                    " (superseded)",
                    theme.selection_aware_text_style(selected, theme.text.muted),
                )
            } else {
                Span::styled(
                    "",
                    theme.selection_aware_text_style(selected, theme.text.primary),
                )
            },
        ]));
        lines.push(Line::styled(
            format!("    {}", truncate(&card.summary, 34)),
            theme.selection_aware_text_style(selected, theme.text.muted),
        ));
        let item = ListItem::new(lines);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        list_area,
    );
    for (screen_row, idx) in (first..state.blackboard.len())
        .take(visible_rows)
        .enumerate()
    {
        if let Some(hit) = visible_row_hit(list_area, screen_row, ROW_LINES as u16) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }

    // Right: the detail for the focused artifact — kind, author, confidence, the
    // evidence that grounds it (claim-like kinds always carry it), revision, and a
    // payload summary.
    let detail_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.focus.inactive))
        .style(Style::default().bg(theme.surface.overlay));
    let detail_inner = detail_block.inner(cols[1]);
    frame.render_widget(detail_block, cols[1]);
    let detail_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(detail_inner);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(card) = state.focused_item() {
        let field = |k: &str, v: &str, color: Color| -> Line {
            Line::from(vec![
                Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
                Span::styled(v.to_owned(), Style::default().fg(color)),
            ])
        };
        lines.push(section("Artifact", theme));
        lines.push(field("run", &card.run, theme.text.secondary));
        lines.push(field("kind", &card.kind, theme.status.info));
        lines.push(field("revision", &card.revision, theme.text.secondary));
        if card.superseded {
            lines.push(field("status", "superseded", theme.text.muted));
        }
        lines.push(Line::raw(""));
        lines.push(section("Provenance", theme));
        lines.push(field("author", &card.author, theme.text.primary));
        lines.push(field("confidence", &card.confidence, theme.text.secondary));
        lines.push(field("evidence", &card.evidence, theme.text.secondary));
        lines.push(Line::raw(""));
        lines.push(section("Payload", theme));
        for line in textwrap_summary(&card.summary) {
            lines.push(Line::styled(
                format!("  {line}"),
                Style::default().fg(theme.text.secondary),
            ));
        }
    } else {
        lines.push(Line::styled(
            "  no artifact selected",
            Style::default().fg(theme.text.muted),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "  ↑/↓ item · Esc close · live",
            Style::default().fg(theme.text.muted),
        )),
        detail_rows[1],
    );
    if detail_rows[1].height >= 1 {
        state.register_hit(
            Rect {
                x: detail_rows[1].x.saturating_add(13),
                y: detail_rows[1].y,
                width: 9.min(detail_rows[1].width.saturating_sub(13)),
                height: 1,
            },
            Action::Dismiss,
        );
    }
}

/// Split a one-line summary into wrapped display lines for the payload panel. A
/// plain char-count wrap (the summary is already a single pre-rendered line, so a
/// word-aware wrap is unnecessary here) keeping each chunk within the panel.
fn textwrap_summary(summary: &str) -> Vec<String> {
    const WIDTH: usize = 48;
    if summary.is_empty() {
        return vec!["(empty)".to_owned()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in summary.split_whitespace() {
        // A single word wider than the panel (a long path, URL, or hash) is
        // hard-split into width-sized chunks so no produced line overflows.
        if word.chars().count() > WIDTH {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                let chunk: String = chars.by_ref().take(WIDTH).collect();
                // Push full-width chunks; keep the short remainder in `current` so
                // a following word can still join it.
                if chars.peek().is_some() {
                    lines.push(chunk);
                } else {
                    current = chunk;
                }
            }
            continue;
        }
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > WIDTH {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The command palette: a filter line over a searchable list of every command,
/// so the growing feature set is reachable without a permanent pane or a
/// single-key binding each. Colors are Theme tokens only (RULE 7).
fn render_palette(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    query: &str,
    selected: usize,
) {
    let matches = crate::palette::filtered(query);
    let rect = centered_modal(area, 112, 30);
    let inner = modal_surface(frame, rect, "Command palette", state, theme);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    // The filtered command list: name / description / shortcut columns,
    // grouped into contiguous sections (a dim label row per group) when the
    // query is empty — filtering flattens the groups away since matches can
    // straddle them.
    let results_block = modal_panel(
        format!(
            "Commands  ·  {} of {} results",
            matches.len(),
            crate::palette::COMMANDS.len()
        ),
        theme,
    );
    let list_area = results_block.inner(rows[1]);
    frame.render_widget(results_block, rows[1]);
    let inner_w = list_area.width as usize;
    let title_w = 20usize;
    let key_w = 4usize;
    // marker(2) + title + space + description(fill) + key
    let desc_w = inner_w.saturating_sub(2 + title_w + 1 + key_w).max(1);
    let show_groups = query.trim().is_empty();

    // Build the actual visual rows first. Group headings consume terminal rows,
    // so they must participate in the window calculation; counting commands
    // alone lets the selection walk below the viewport on an 80x24 terminal.
    enum PaletteVisualRow<'a> {
        Group(&'a str),
        Command(usize, &'a crate::palette::PaletteEntry),
    }

    let mut visual_rows = Vec::with_capacity(matches.len() + 4);
    let mut last_group: Option<&str> = None;
    for (idx, entry) in matches.iter().enumerate() {
        if show_groups && last_group != Some(entry.group) {
            visual_rows.push(PaletteVisualRow::Group(entry.group));
            last_group = Some(entry.group);
        }
        visual_rows.push(PaletteVisualRow::Command(idx, entry));
    }
    let selected_visual = visual_rows
        .iter()
        .position(|row| matches!(row, PaletteVisualRow::Command(idx, _) if *idx == selected))
        .unwrap_or(0);
    let visible_rows = (list_area.height as usize).max(1);
    let first = first_visible_row(selected_visual, visual_rows.len(), visible_rows);
    let last = (first + visible_rows).min(visual_rows.len());

    let mut items: Vec<ListItem> = Vec::new();
    if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching command",
            Style::default().fg(theme.text.muted),
        )));
    }
    for row in &visual_rows[first..last] {
        match row {
            PaletteVisualRow::Group(group) => items.push(ListItem::new(Line::styled(
                format!("  {group}"),
                Style::default()
                    .fg(theme.text.muted)
                    .add_modifier(Modifier::BOLD),
            ))),
            PaletteVisualRow::Command(idx, entry) => {
                let is_selected = *idx == selected;
                let marker = if is_selected { "▎ " } else { "  " };
                // Unbound commands (`key == "—"`) show nothing in the shortcut
                // column — never a fake `[—]` marker.
                let key = if entry.key == "—" {
                    " ".repeat(key_w)
                } else {
                    format!("{:>width$}", entry.key, width = key_w)
                };
                let head = Line::from(vec![
                    Span::styled(
                        marker,
                        theme.selection_aware_text_style(is_selected, theme.focus.active),
                    ),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            truncate_display_width(entry.title, title_w),
                            width = title_w
                        ),
                        theme.selection_aware_text_style(is_selected, theme.text.primary),
                    ),
                    Span::styled(
                        " ",
                        theme.selection_aware_text_style(is_selected, theme.text.primary),
                    ),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            truncate(entry.description, desc_w),
                            width = desc_w
                        ),
                        theme.selection_aware_text_style(is_selected, theme.text.muted),
                    ),
                    Span::styled(
                        key,
                        theme.selection_aware_text_style(is_selected, theme.status.info),
                    ),
                ]);
                let item = ListItem::new(head);
                items.push(if is_selected {
                    item.style(theme.selection_style())
                } else {
                    item
                });
            }
        }
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "↑/↓ select  ·  Enter run  ·  Esc close  ·  click a row",
            Style::default().fg(theme.text.muted),
        ))
        .alignment(Alignment::Center),
        rows[2],
    );

    // Register only the command rows in the exact visual slice rendered above.
    for (screen_row, row) in visual_rows[first..last].iter().enumerate() {
        if let PaletteVisualRow::Command(command_index, _) = row {
            if let Some(hit) = visible_row_hit(list_area, screen_row, 1) {
                state.register_hit(hit, Action::ActivateRow(*command_index));
            }
        }
    }
}

/// Color an edge's confidence by tier (Chapter 07): a syntax-inferred call
/// (~0.45) reads as tentative; an LSP/compiler-resolved edge (≥0.90) as trusted.
fn edge_confidence_color(confidence: f32, theme: &Theme) -> Color {
    if confidence >= 0.90 {
        theme.status.success
    } else if confidence >= 0.60 {
        theme.status.warning
    } else {
        theme.text.muted
    }
}

/// Color for a skill's coarse risk label (`safe` / `low` / `medium` / `high`).
fn skill_risk_color(risk: &str, theme: &Theme) -> Color {
    match risk {
        "safe" | "low" => theme.status.success,
        "medium" => theme.status.warning,
        "high" => theme.status.error,
        _ => theme.text.muted,
    }
}

fn render_approval_modal(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let Some(approval) = state.focused_approval() else {
        return;
    };
    let rect = centered_rect(70, 60, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled(
        "Approval required",
        Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));

    lines.push(section("Action", theme));
    for detail in describe_action(&approval.action) {
        lines.push(Line::styled(
            format!("  {detail}"),
            Style::default().fg(theme.text.primary),
        ));
    }
    lines.push(Line::raw(""));

    lines.push(section("Risk", theme));
    lines.extend(risk_lines(&approval.risk, theme));
    lines.push(Line::raw(""));

    lines.push(section("Requested capabilities", theme));
    lines.push(Line::styled(
        format!("  {}", capability_label(&approval.action)),
        Style::default().fg(theme.text.primary),
    ));
    lines.push(Line::raw(""));

    lines.push(Line::from(vec![
        Span::styled(
            "[a] approve once   ",
            Style::default().fg(theme.status.success),
        ),
        Span::styled(
            "[A] approve for run   ",
            Style::default().fg(theme.status.success),
        ),
        Span::styled("[r] reject", Style::default().fg(theme.status.error)),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Approval ")
        .border_style(Style::default().fg(theme.status.warning))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn render_help(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(70, 80, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled(
        "Keys — every mouse action has a keyboard equivalent",
        Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));
    for binding in crate::input::KEY_BINDINGS {
        let mut spans = vec![
            Span::styled(
                format!("  {:<12}", binding.keys),
                Style::default()
                    .fg(theme.status.info)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                binding.description.to_owned(),
                Style::default().fg(theme.text.primary),
            ),
        ];
        if let Some(mouse) = binding.mouse {
            spans.push(Span::styled(
                format!("  (mouse: {mouse})"),
                Style::default().fg(theme.text.muted),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Ctrl-C detaches this client — it never stops the run.  ? or Esc closes.",
        Style::default().fg(theme.text.secondary),
    ));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn render_prompt(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    title: &str,
    buffer: &str,
) {
    let rect = centered_rect_min(70, 20, 48, 7, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::styled(title, Style::default().fg(theme.text.heading)),
        Line::from(vec![
            Span::styled("› ", Style::default().fg(theme.focus.active)),
            Span::styled(buffer.to_owned(), Style::default().fg(theme.text.primary)),
            Span::styled("█", Style::default().fg(theme.focus.active)),
        ]),
        Line::styled(
            "Enter to submit · Esc to cancel",
            Style::default().fg(theme.text.muted),
        ),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// Like [`render_prompt`] but renders the buffer MASKED (one `•` per character),
/// so a secret (an API key) is never shown on screen. The buffer is itself a
/// redacting newtype, so it also cannot leak through `Debug`.
fn render_masked_prompt(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    title: &str,
    buffer: &str,
) {
    let rect = centered_rect_min(70, 20, 48, 7, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let masked: String = "•".repeat(buffer.chars().count());
    let lines = vec![
        Line::styled(title, Style::default().fg(theme.text.heading)),
        Line::from(vec![
            Span::styled("› ", Style::default().fg(theme.focus.active)),
            Span::styled(masked, Style::default().fg(theme.text.primary)),
            Span::styled("█", Style::default().fg(theme.focus.active)),
        ]),
        Line::styled(
            "Enter to submit · Esc to cancel",
            Style::default().fg(theme.text.muted),
        ),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// The transient "Fetching models from <provider>…" box shown while the harness
/// GETs the provider's `/models` list (model-discovery). Non-interactive except
/// `Esc`, which cancels the wait. Colors are Theme tokens only (RULE 7). The key
/// is NOT in scope here (the overlay's `api_key` field is dropped via `..`).
fn render_querying(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    provider_id: &str,
) {
    // Compact two-line progress state; the absolute minimum keeps the content
    // visible on short terminals.
    let lines = vec![
        Line::styled(
            format!("Fetching models from {provider_id}…"),
            Style::default().fg(theme.text.heading),
        ),
        Line::styled("Esc to cancel", Style::default().fg(theme.text.muted)),
    ];
    let rect = centered_rect_min(70, 20, 44, 5, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// The add-model pick-list (model-discovery): a filter line over the provider's
/// offerable models, each row a card — id, display name, context window, and
/// USD per 1M input/output tokens — merged by the harness from the live
/// `/models` listing and the built-in catalog. A `~` marks a catalog-only row
/// (offerable, but not confirmed by the provider just now), and the header
/// states where the list came from, so a cached or catalog-only list is never
/// mistaken for a live one. Prices are display-only (never summed). Colors are
/// Theme tokens only (RULE 7). The key is NOT in scope here.
#[allow(clippy::too_many_arguments)] // mirrors the model/provider picker signatures + `state` (Task 8)
fn render_add_model_pick(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    provider_id: &str,
    models: &[AddModelRow],
    query: &str,
    selected: usize,
    origin: &ModelListOrigin,
    refreshing: bool,
) {
    let matches = filter_model_names(models, query);
    let rect = centered_modal(area, 96, 26);
    let source = match origin {
        ModelListOrigin::Live => "live list".to_owned(),
        ModelListOrigin::Cached(age) => format!("cached {age}"),
        ModelListOrigin::Catalog(reason) if reason.is_empty() => "catalog".to_owned(),
        ModelListOrigin::Catalog(reason) => format!("catalog · {reason}"),
    };
    let inner = modal_surface(
        frame,
        rect,
        format!(
            "Add model · {}  ·  {} of {} results  ·  {}{}",
            truncate_display_width(provider_id, 24),
            matches.len(),
            models.len(),
            source,
            if refreshing { " · refreshing…" } else { "" }
        ),
        state,
        theme,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
    render_modal_search(frame, rows[0], query, theme);

    let list_block = modal_panel(
        format!(
            "Models  ·  {} of {}  ·  ctx · $/1M in · out",
            matches.len(),
            models.len()
        ),
        theme,
    );
    let list_area = list_block.inner(rows[1]);
    frame.render_widget(list_block, rows[1]);
    let mut items: Vec<ListItem> = Vec::new();
    if models.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no models returned",
            Style::default().fg(theme.text.muted),
        )));
    } else if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching model",
            Style::default().fg(theme.text.muted),
        )));
    }
    // Two lines per row: the id (with a live/catalog marker), then the
    // metadata columns. A row whose metadata is entirely unknown still shows
    // its dashes rather than collapsing, so the columns stay aligned.
    const ROW_LINES: usize = 2;
    let visible_rows = (usize::from(list_area.height) / ROW_LINES).max(1);
    let first = first_visible_row(selected, matches.len(), visible_rows);
    for (row, &idx) in matches.iter().enumerate().skip(first) {
        let is_selected = row == selected;
        let card = &models[idx];
        let head = Line::from(vec![
            Span::styled(
                if is_selected { "▎ " } else { "  " },
                theme.selection_aware_text_style(is_selected, theme.focus.active),
            ),
            Span::styled(
                if card.live { "✓ " } else { "~ " },
                theme.selection_aware_text_style(
                    is_selected,
                    if card.live {
                        theme.status.success
                    } else {
                        theme.text.muted
                    },
                ),
            ),
            Span::styled(
                truncate_display_width(&card.id, usize::from(list_area.width.saturating_sub(4))),
                theme.selection_aware_text_style(is_selected, theme.text.primary),
            ),
        ]);
        let detail = Line::styled(
            format!(
                "      {} · ctx {} · in {} · out {}",
                card.name.as_deref().unwrap_or("—"),
                context_label(card.context_tokens),
                price_per_1m_label(card.cost_per_1m_input_usd),
                price_per_1m_label(card.cost_per_1m_output_usd),
            ),
            theme.selection_aware_text_style(is_selected, theme.text.muted),
        );
        let item = ListItem::new(vec![head, detail]);
        items.push(if is_selected {
            item.style(theme.selection_style())
        } else {
            item
        });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.panel)),
        list_area,
    );
    // Each row is two lines tall — register a matching rect per filtered row.
    for (row, _) in matches.iter().enumerate().skip(first) {
        let Some(hit) = visible_row_hit(list_area, row - first, ROW_LINES as u16) else {
            break;
        };
        state.register_hit(hit, Action::ActivateRow(row));
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            "↑/↓ select  ·  Enter add  ·  Ctrl-R refresh  ·  Esc close",
            Style::default().fg(theme.text.muted),
        ))
        .alignment(Alignment::Center),
        rows[2],
    );
}

fn render_confirm(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect_min(60, 20, 48, 7, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::styled(
            "Cancel this run?",
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Cancelling stops the run; a chronicle and any artifacts are kept.",
            Style::default().fg(theme.text.secondary),
        ),
        Line::from(vec![
            Span::styled(
                "[y] yes, cancel   ",
                Style::default().fg(theme.status.error),
            ),
            Span::styled("[n] no", Style::default().fg(theme.status.success)),
        ]),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirm ")
        .border_style(Style::default().fg(theme.status.error))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn section(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(section_span(title, theme))
}

/// The `section` heading as a [`Span`], for composing into a header line that also
/// carries trailing status (e.g. the Docs editor-rail lease indicator).
fn section_span(title: &str, theme: &Theme) -> Span<'static> {
    Span::styled(
        title.to_owned(),
        Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::UNDERLINED),
    )
}

/// The presence-lite edit-lease indicator for the Docs editor rail: whether this
/// client holds, is acquiring, or is blocked on a block lease. Empty when there is
/// no in-flight edit (the common read-only state).
fn lease_span(state: &AppState, theme: &Theme) -> Span<'static> {
    match state.doc_edit.as_ref().map(|edit| edit.lease) {
        Some(DocLeaseState::Held) => Span::styled(
            "  lease: held".to_owned(),
            Style::default().fg(theme.status.success),
        ),
        Some(DocLeaseState::Acquiring) => Span::styled(
            "  lease: acquiring…".to_owned(),
            Style::default().fg(theme.status.warning),
        ),
        Some(DocLeaseState::Blocked) => Span::styled(
            "  lease: blocked (another writer)".to_owned(),
            Style::default().fg(theme.status.error),
        ),
        None => Span::raw(""),
    }
}

fn risk_lines<'a>(risk: &'a Risk, theme: &Theme) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("  level: ", Style::default().fg(theme.text.muted)),
        Span::styled(
            risk_label(risk.level).to_owned(),
            Style::default()
                .fg(risk_color(risk.level, theme))
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    for reason in &risk.reasons {
        lines.push(Line::styled(
            format!("  - {reason}"),
            Style::default().fg(theme.text.secondary),
        ));
    }
    lines
}

/// Verbatim rendering of a proposed action's fields (approval modal).
fn describe_action(action: &ProposedAction) -> Vec<String> {
    match action {
        ProposedAction::ReadFiles { paths } => {
            let mut v = vec!["read files:".to_owned()];
            v.extend(paths.iter().map(|p| format!("  {p}")));
            v
        }
        ProposedAction::WritePatch { patch } => vec![format!("apply patch: {patch}")],
        ProposedAction::ExecuteCommand {
            program,
            args,
            environment,
            cwd,
        } => {
            // Render the FULL environment and cwd: an unshown binding could
            // smuggle an execution-hijacking variable past a benign-looking
            // command line, so the approver must see every one verbatim.
            let mut v = vec![format!("command: {program} {}", args.join(" "))];
            if let Some(cwd) = cwd {
                v.push(format!("cwd: {cwd}"));
            }
            for (name, value) in environment {
                v.push(format!("env: {name}={value}"));
            }
            v
        }
        ProposedAction::NetworkRequest { destination } => {
            vec![format!("network request: {destination}")]
        }
        ProposedAction::GitCommit { repository } => vec![format!("git commit: {repository}")],
        ProposedAction::GitPush { remote, branch } => {
            vec![format!("git push: {remote} {branch}")]
        }
        // STEP 4.4.2: every publish displays target, changed files, and the
        // resulting Git action before approval — render all three verbatim
        // from the plan, exactly as computed (never re-derived here).
        ProposedAction::PublishDocument {
            target,
            changed_files,
            git_action,
            ..
        } => {
            let mut v = vec![format!("publish document: {target}")];
            v.push("changed files:".to_owned());
            v.extend(changed_files.iter().map(|f| format!("  {f}")));
            v.push(format!("git action: {git_action}"));
            v
        }
        // PR B (MCP client): the approver must see WHICH server and tool, the
        // human summary, and the `args` string VERBATIM — it is canonical JSON,
        // already auditable — exactly as ExecuteCommand renders program/args/env
        // verbatim.
        ProposedAction::McpToolCall {
            server,
            tool,
            summary,
            args,
        } => vec![
            format!("mcp tool: {server}.{tool}"),
            format!("summary: {summary}"),
            format!("args: {args}"),
        ],
        ProposedAction::AcpToolCall {
            agent,
            title,
            details,
        } => vec![
            format!("ACP agent: {agent}"),
            format!("tool: {title}"),
            format!("details: {details}"),
        ],
        _ => vec!["unsupported action".to_owned()],
    }
}

/// A short kind label for a proposed action (list rows).
fn action_kind(action: &ProposedAction) -> &'static str {
    match action {
        ProposedAction::ReadFiles { .. } => "read files",
        ProposedAction::WritePatch { .. } => "apply patch",
        ProposedAction::ExecuteCommand { .. } => "run command",
        ProposedAction::NetworkRequest { .. } => "network",
        ProposedAction::GitCommit { .. } => "git commit",
        ProposedAction::GitPush { .. } => "git push",
        ProposedAction::PublishDocument { .. } => "publish document",
        ProposedAction::McpToolCall { .. } => "mcp tool",
        ProposedAction::AcpToolCall { .. } => "acp tool",
        _ => "unsupported",
    }
}

fn render_council_builder(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let rect = centered_modal(area, 104, 34);
    let step_number = council_step_number(builder.step);
    let inner = modal_surface(
        frame,
        rect,
        format!(
            "Create agent council · {step_number}/7 · {}",
            council_step_label(builder.step)
        ),
        state,
        theme,
    );
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(inner);

    let progress = [
        "Name", "Purpose", "Members", "Roles", "Chair", "Rounds", "Review",
    ]
    .iter()
    .enumerate()
    .map(|(idx, label)| {
        let active = idx + 1 == step_number;
        Span::styled(
            format!(" {}{} ", idx + 1, label),
            if active {
                theme.selection_style().add_modifier(Modifier::BOLD)
            } else if idx + 1 < step_number {
                Style::default().fg(theme.status.success)
            } else {
                Style::default().fg(theme.text.muted)
            },
        )
    })
    .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(progress)).style(Style::default().bg(theme.surface.overlay)),
        rows[0],
    );

    match builder.step {
        CouncilBuilderStep::Name => render_council_text_step(
            frame,
            rows[1],
            "Council name",
            &builder.name,
            "architecture-review",
            "Stable identifier: letters, numbers, dot, dash, or underscore.",
            theme,
        ),
        CouncilBuilderStep::Description => render_council_text_step(
            frame,
            rows[1],
            "Purpose (optional)",
            &builder.description,
            "What should this council be especially good at?",
            "Shown in council listings; the objective is supplied when the council runs.",
            theme,
        ),
        CouncilBuilderStep::MemberRole => {
            let model = builder.pending_member_model.as_deref().unwrap_or("model");
            render_council_text_step(
                frame,
                rows[1],
                &format!("Role for {model}"),
                &builder.role,
                "member",
                "Describe this member's perspective, e.g. security reviewer or product strategist.",
                theme,
            );
        }
        CouncilBuilderStep::MemberModel => {
            render_council_member_picker(frame, rows[1], state, theme, builder);
        }
        CouncilBuilderStep::Chair => {
            render_council_chair_picker(frame, rows[1], state, theme, builder);
        }
        CouncilBuilderStep::Rounds => {
            render_council_rounds(frame, rows[1], state, theme, builder);
        }
        CouncilBuilderStep::Review => {
            render_council_review(frame, rows[1], theme, builder);
        }
    }

    let footer = match builder.step {
        CouncilBuilderStep::Name => "  Enter continue · Esc close",
        CouncilBuilderStep::Description => "  Enter continue · Esc back",
        CouncilBuilderStep::MemberModel => {
            if builder.members.len() < 2 {
                "  ↑/↓ choose model · Enter add role · at least 2 members · Esc back"
            } else {
                "  ↑/↓ choose · Enter add/continue · 2–8 unique profiles · Esc back"
            }
        }
        CouncilBuilderStep::MemberRole => "  Enter add member · blank role = member · Esc back",
        CouncilBuilderStep::Chair => "  ↑/↓ choose synthesis chair · Enter continue · Esc back",
        CouncilBuilderStep::Rounds => "  ↑/↓ choose deliberation depth · Enter review · Esc back",
        CouncilBuilderStep::Review => "  Enter create council · Esc back",
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            footer,
            Style::default().fg(theme.focus.active),
        )),
        rows[2],
    );
    if rows[2].height > 0 {
        state.register_hit(rows[2], Action::InputSubmit);
    }
}

fn council_step_number(step: CouncilBuilderStep) -> usize {
    match step {
        CouncilBuilderStep::Name => 1,
        CouncilBuilderStep::Description => 2,
        CouncilBuilderStep::MemberModel => 3,
        CouncilBuilderStep::MemberRole => 4,
        CouncilBuilderStep::Chair => 5,
        CouncilBuilderStep::Rounds => 6,
        CouncilBuilderStep::Review => 7,
    }
}

fn council_step_label(step: CouncilBuilderStep) -> &'static str {
    match step {
        CouncilBuilderStep::Name => "Name",
        CouncilBuilderStep::Description => "Purpose",
        CouncilBuilderStep::MemberModel => "Add members",
        CouncilBuilderStep::MemberRole => "Member role",
        CouncilBuilderStep::Chair => "Choose chair",
        CouncilBuilderStep::Rounds => "Deliberation rounds",
        CouncilBuilderStep::Review => "Review",
    }
}

fn render_council_text_step(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: &str,
    placeholder: &str,
    help: &str,
    theme: &Theme,
) {
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("  {title}"),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        )),
        rows[0],
    );
    let field = modal_panel("Value", theme);
    let field_inner = field.inner(rows[1]);
    frame.render_widget(field, rows[1]);
    let shown = if value.is_empty() {
        Span::styled(
            placeholder.to_owned(),
            Style::default().fg(theme.text.muted),
        )
    } else {
        Span::styled(
            tail_window(value, usize::from(field_inner.width.saturating_sub(3))),
            Style::default().fg(theme.text.primary),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            shown,
            Span::styled("▏", Style::default().fg(theme.focus.active)),
        ])),
        field_inner,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("  {help}"),
            Style::default().fg(theme.text.muted),
        ))
        .wrap(Wrap { trim: true }),
        rows[2],
    );
}

fn render_council_member_picker(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    render_modal_search(frame, rows[0], &builder.query, theme);
    let (list_area, detail_area) = picker_regions(rows[2]);
    let list_block = modal_panel(
        format!("Configured profiles · {} selected", builder.members.len()),
        theme,
    );
    let list_inner = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);

    let indices = if builder.members.len() >= 8 {
        Vec::new()
    } else {
        filter_council_member_models(&state.models, &builder.query, &builder.members)
    };
    let continue_row = builder.members.len() >= 2 && builder.query.trim().is_empty();
    let remove_row = !builder.members.is_empty() && builder.query.trim().is_empty();
    let total = indices.len() + usize::from(continue_row) + usize::from(remove_row);
    let visible = usize::from(list_inner.height).max(1);
    let first = first_visible_row(builder.selected, total, visible);
    let mut items = Vec::new();
    for row in first..total.min(first + visible) {
        let selected = row == builder.selected;
        if continue_row && row == 0 {
            let item = ListItem::new(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    theme.selection_aware_text_style(selected, theme.focus.active),
                ),
                Span::styled(
                    format!("Continue with {} members →", builder.members.len()),
                    theme
                        .selection_aware_text_style(selected, theme.status.success)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            items.push(if selected {
                item.style(theme.selection_style())
            } else {
                item
            });
            continue;
        }
        let model_row = row.saturating_sub(usize::from(continue_row));
        if remove_row && model_row == indices.len() {
            let removed = builder
                .members
                .last()
                .map_or("member", |member| member.model.as_str());
            let item = ListItem::new(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    theme.selection_aware_text_style(selected, theme.focus.active),
                ),
                Span::styled(
                    format!("Remove last member · {removed}"),
                    theme.selection_aware_text_style(selected, theme.status.warning),
                ),
            ]));
            items.push(if selected {
                item.style(theme.selection_style())
            } else {
                item
            });
            continue;
        }
        if let Some(card) = indices
            .get(model_row)
            .and_then(|idx| state.models.get(*idx))
        {
            let marker = if selected { "› " } else { "  " };
            let readiness = match &card.readiness {
                ModelReadiness::Ready => "ready",
                ModelReadiness::Unverified => "unverified",
                ModelReadiness::Unavailable(_) => "unavailable",
            };
            let item = ListItem::new(Line::from(vec![
                Span::styled(
                    marker,
                    theme.selection_aware_text_style(selected, theme.focus.active),
                ),
                Span::styled(
                    truncate_display_width(&card.id.0, 34),
                    theme
                        .selection_aware_text_style(selected, theme.text.primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {} · {readiness}", card.provider),
                    theme.selection_aware_text_style(selected, theme.text.muted),
                ),
            ]));
            items.push(if selected {
                item.style(theme.selection_style())
            } else {
                item
            });
        }
    }
    if total == 0 {
        items.push(ListItem::new(Line::styled(
            "  No matching unselected profiles",
            Style::default().fg(theme.text.muted),
        )));
    }
    frame.render_widget(List::new(items), list_inner);
    for (screen_row, row) in (first..total.min(first + visible)).enumerate() {
        if let Some(hit) = visible_row_hit(list_inner, screen_row, 1) {
            state.register_hit(hit, Action::ActivateRow(row));
        }
    }
    if let Some(detail_area) = detail_area {
        render_council_members_summary(frame, detail_area, theme, builder);
    }
}

fn render_council_chair_picker(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    render_modal_search(frame, rows[0], &builder.query, theme);
    let (list_area, detail_area) = picker_regions(rows[2]);
    let list_block = modal_panel("Synthesis model", theme);
    let list_inner = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);
    let indices = filter_models(&state.models, &builder.query);
    let visible = usize::from(list_inner.height).max(1);
    let first = first_visible_row(builder.selected, indices.len(), visible);
    let mut items = Vec::new();
    for (screen_row, idx) in indices.iter().skip(first).take(visible).enumerate() {
        let row = first + screen_row;
        let selected = row == builder.selected;
        if let Some(card) = state.models.get(*idx) {
            let item = ListItem::new(Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    theme.selection_aware_text_style(selected, theme.focus.active),
                ),
                Span::styled(
                    truncate_display_width(&card.id.0, 36),
                    theme
                        .selection_aware_text_style(selected, theme.text.primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", card.provider),
                    theme.selection_aware_text_style(selected, theme.text.muted),
                ),
            ]));
            items.push(if selected {
                item.style(theme.selection_style())
            } else {
                item
            });
            if let Some(hit) = visible_row_hit(list_inner, screen_row, 1) {
                state.register_hit(hit, Action::ActivateRow(row));
            }
        }
    }
    if indices.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  No matching configured profiles",
            Style::default().fg(theme.text.muted),
        )));
    }
    frame.render_widget(List::new(items), list_inner);
    if let Some(detail_area) = detail_area {
        render_council_members_summary(frame, detail_area, theme, builder);
    }
}

fn render_council_members_summary(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let block = modal_panel(
        format!("Council members · {}/8", builder.members.len()),
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    if builder.members.is_empty() {
        lines.push(Line::styled(
            "  Add at least two distinct profiles.",
            Style::default().fg(theme.text.muted),
        ));
    } else {
        for (idx, member) in builder.members.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}. ", idx + 1),
                    Style::default().fg(theme.focus.active),
                ),
                Span::styled(
                    truncate_display_width(&member.model, 30),
                    Style::default().fg(theme.text.primary),
                ),
            ]));
            lines.push(Line::styled(
                format!("     {}", member.role),
                Style::default().fg(theme.text.muted),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_council_rounds(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let block = modal_panel("Deliberation depth", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let choices = [
        ("1 round", "Independent answers, then chair synthesis"),
        (
            "2 rounds",
            "Members critique the first dossier before synthesis",
        ),
        (
            "3 rounds",
            "Two critique passes for difficult or contested decisions",
        ),
    ];
    let mut items = Vec::new();
    for (idx, (title, detail)) in choices.iter().enumerate() {
        let selected = idx == builder.selected;
        let item = ListItem::new(vec![
            Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    theme.selection_aware_text_style(selected, theme.focus.active),
                ),
                Span::styled(
                    (*title).to_owned(),
                    theme
                        .selection_aware_text_style(selected, theme.text.primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(
                format!("    {detail}"),
                theme.selection_aware_text_style(selected, theme.text.muted),
            ),
        ]);
        items.push(if selected {
            item.style(theme.selection_style())
        } else {
            item
        });
        if let Some(hit) = visible_row_hit(inner, idx, 2) {
            state.register_hit(hit, Action::ActivateRow(idx));
        }
    }
    frame.render_widget(List::new(items), inner);
}

fn render_council_review(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    builder: &CouncilBuilderState,
) {
    let block = modal_panel("Ready to create", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Name: ", Style::default().fg(theme.text.muted)),
            Span::styled(
                builder.name.clone(),
                Style::default()
                    .fg(theme.text.heading)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Purpose: ", Style::default().fg(theme.text.muted)),
            Span::styled(
                if builder.description.is_empty() {
                    "(none)".to_owned()
                } else {
                    builder.description.clone()
                },
                Style::default().fg(theme.text.secondary),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Chair: ", Style::default().fg(theme.text.muted)),
            Span::styled(
                builder
                    .chair
                    .clone()
                    .unwrap_or_else(|| "(not selected)".into()),
                Style::default().fg(theme.focus.active),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Rounds: ", Style::default().fg(theme.text.muted)),
            Span::styled(
                builder.rounds.to_string(),
                Style::default().fg(theme.text.primary),
            ),
        ]),
        Line::default(),
        Line::styled(
            format!("  Members · {}", builder.members.len()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    for member in &builder.members {
        lines.push(Line::from(vec![
            Span::styled("  • ", Style::default().fg(theme.status.success)),
            Span::styled(
                member.model.clone(),
                Style::default().fg(theme.text.primary),
            ),
            Span::styled(
                format!(" — {}", member.role),
                Style::default().fg(theme.text.muted),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  Saved privately to councils.toml. Running it creates durable, attributed sessions.",
        Style::default().fg(theme.text.muted),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// A centered modal with a stable reading width and height. Percentage-sized
/// overlays grow into nearly full-screen debug panes on wide terminals; a
/// character cap keeps them feeling like focused tools while still shrinking
/// safely to an 80x24 terminal.
fn centered_modal(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let width = preferred_width.min(area.width.saturating_sub(4)).max(1);
    let height = preferred_height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// Paint the common modal surface and return its content rectangle. The solid
/// body, rounded outline, and one-cell shadow give every picker the same visual
/// depth without relying on terminal transparency or per-screen decoration.
fn modal_surface(
    frame: &mut Frame,
    rect: Rect,
    title: impl Into<String>,
    state: &AppState,
    theme: &Theme,
) -> Rect {
    shield_modal(state, rect);
    if rect.width > 1 && rect.height > 1 {
        let shadow = Rect {
            x: rect.x.saturating_add(1),
            y: rect.y.saturating_add(1),
            width: rect.width,
            height: rect.height,
        }
        .intersection(frame.area());
        frame.render_widget(Clear, shadow);
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.surface.background)),
            shadow,
        );
    }
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ", title.into()),
            Style::default()
                .fg(theme.text.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn modal_panel<'a>(title: impl Into<String>, theme: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ", title.into()),
            Style::default()
                .fg(theme.text.secondary)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme.surface.border))
        .style(Style::default().bg(theme.surface.panel))
}

fn render_modal_search(frame: &mut Frame, area: Rect, query: &str, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " Search ",
            Style::default().fg(theme.text.muted),
        ))
        .border_style(Style::default().fg(theme.surface.border))
        .style(Style::default().bg(theme.surface.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let value_width = usize::from(inner.width.saturating_sub(6));
    let value = if query.is_empty() {
        Span::styled(
            truncate_display_width("Type to filter…", value_width),
            Style::default().fg(theme.text.muted),
        )
    } else {
        Span::styled(
            tail_window(query, value_width),
            Style::default().fg(theme.text.primary),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ⌕  ", Style::default().fg(theme.focus.active)),
            value,
            Span::styled("▏", Style::default().fg(theme.focus.active)),
        ])),
        inner,
    );
}

fn shield_modal(state: &AppState, rect: Rect) {
    if rect.width > 0 && rect.height > 0 {
        state.register_hit(rect, Action::NoOp);
    }
}

fn visible_row_hit(area: Rect, screen_row: usize, row_lines: u16) -> Option<Rect> {
    let offset = u16::try_from(screen_row)
        .unwrap_or(u16::MAX)
        .saturating_mul(row_lines);
    let y = area.y.saturating_add(offset);
    if area.width == 0 || y >= area.bottom() {
        return None;
    }
    Some(Rect {
        x: area.x,
        y,
        width: area.width,
        height: row_lines.min(area.bottom().saturating_sub(y)),
    })
}

fn picker_regions(area: Rect) -> (Rect, Option<Rect>) {
    const TWO_COLUMN_MIN_WIDTH: u16 = 88;
    if area.width >= TWO_COLUMN_MIN_WIDTH {
        let cols = Layout::horizontal([
            Constraint::Percentage(40),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
        return (cols[0], Some(cols[2]));
    }
    if area.height >= 12 {
        let detail_height = area.height.saturating_div(3).clamp(5, 7);
        let rows = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(detail_height),
        ])
        .split(area);
        (rows[0], Some(rows[2]))
    } else {
        (area, None)
    }
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 1 {
        return "…".to_owned();
    }
    let mut width = 0;
    let mut value = String::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width - 1 {
            break;
        }
        value.push_str(grapheme);
        width += grapheme_width;
    }
    value.push('…');
    value
}

fn tail_window(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 1 {
        return "…".to_owned();
    }
    let mut tail = Vec::new();
    let mut width = 0;
    for grapheme in UnicodeSegmentation::graphemes(text, true).rev() {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width - 1 {
            break;
        }
        tail.push(grapheme);
        width += grapheme_width;
    }
    tail.reverse();
    format!("…{}", tail.concat())
}

/// A centered percentage rectangle with an absolute content-driven minimum,
/// capped to the available terminal. Percentage-only modals collapse to four
/// rows at 80x24 (20% of 24), clipping prompts and confirmation buttons.
fn centered_rect_min(
    percent_x: u16,
    percent_y: u16,
    min_width: u16,
    min_height: u16,
    area: Rect,
) -> Rect {
    let percentage = centered_rect(percent_x, percent_y, area);
    let width = percentage.width.max(min_width).min(area.width);
    let height = percentage.height.max(min_height).min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn mode_label(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Ask => "Ask",
        AgentMode::Explore => "Explore",
        AgentMode::Plan => "Plan",
        AgentMode::Build => "Build",
        AgentMode::Review => "Review",
        _ => "Unknown",
    }
}

fn run_state_label(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "Queued",
        RunState::Preparing => "Preparing",
        RunState::Running => "Running",
        RunState::WaitingForApproval => "WaitingForApproval",
        RunState::WaitingForUserInput => "WaitingForInput",
        RunState::Paused => "Paused",
        RunState::Recovering => "Recovering",
        RunState::Completed => "Completed",
        RunState::Failed => "Failed",
        RunState::Cancelled => "Cancelled",
        _ => "Unknown",
    }
}

fn run_state_dot(state: RunState) -> &'static str {
    match state {
        RunState::Completed => "✓",
        RunState::Failed => "✗",
        RunState::Cancelled => "⊘",
        RunState::WaitingForApproval | RunState::WaitingForUserInput => "◆",
        RunState::Paused => "⏸",
        _ => "●",
    }
}

fn run_state_color(state: RunState, theme: &Theme) -> Color {
    match state {
        RunState::Running | RunState::Preparing => theme.status.running,
        RunState::Completed => theme.status.success,
        RunState::Failed => theme.status.error,
        RunState::Cancelled => theme.text.muted,
        RunState::WaitingForApproval | RunState::WaitingForUserInput => theme.status.warning,
        RunState::Paused => theme.status.info,
        _ => theme.status.idle,
    }
}

fn risk_label(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "LOW",
        RiskLevel::Medium => "MED",
        RiskLevel::High => "HIGH",
        RiskLevel::Critical => "CRIT",
        _ => "????",
    }
}

fn risk_color(level: RiskLevel, theme: &Theme) -> Color {
    match level {
        RiskLevel::Low => theme.status.success,
        RiskLevel::Medium => theme.status.warning,
        RiskLevel::High | RiskLevel::Critical => theme.status.error,
        _ => theme.text.muted,
    }
}

fn budget_label(dimension: BudgetDimension) -> &'static str {
    match dimension {
        BudgetDimension::Tokens => "tokens",
        BudgetDimension::Cost => "cost",
        BudgetDimension::WallClock => "wall-clock",
        BudgetDimension::ToolCalls => "tool-calls",
        _ => "budget",
    }
}

fn format_cost(cost_minor: Option<u64>) -> String {
    match cost_minor {
        Some(c) => format!("${}.{:02}", c / 100, c % 100),
        None => "—".to_owned(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

fn short_id(id: &impl std::fmt::Display) -> String {
    let s = id.to_string();
    s.chars().take(8).collect()
}

/// The first non-blank line of `text`, or `""` if every line is blank — the
/// label a folded note's collapsed head shows.
fn first_non_empty_line(text: &str) -> &str {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Action;
    use crate::reduce::reduce;

    #[test]
    fn first_visible_row_keeps_the_selection_in_view() {
        // Fits entirely: never scrolls.
        assert_eq!(first_visible_row(0, 5, 10), 0);
        assert_eq!(first_visible_row(4, 5, 10), 0);
        // Degenerate viewport: no scroll.
        assert_eq!(first_visible_row(9, 40, 0), 0);

        // A long list (40 items, 10 visible). The selection stays within the
        // rendered window [first, first + visible) for EVERY position — the
        // property that was violated before (selection walked off the bottom).
        let total = 40;
        let visible = 10;
        for selected in 0..total {
            let first = first_visible_row(selected, total, visible);
            assert!(
                selected >= first && selected < first + visible,
                "selected {selected} must be visible in [{first}, {})",
                first + visible
            );
            // Never scrolls past the final full window.
            assert!(first <= total - visible, "first {first} overshoots the end");
        }
        // Near the top it pins to 0; centered in the middle; pinned at the end.
        assert_eq!(first_visible_row(0, 40, 10), 0);
        assert_eq!(first_visible_row(20, 40, 10), 15); // centered (20 - 10/2)
        assert_eq!(first_visible_row(39, 40, 10), 30); // pinned to last window
    }

    #[test]
    fn compact_picker_regions_stack_detail_below_the_list() {
        let compact = Rect::new(0, 0, 70, 15);
        let (list, detail) = picker_regions(compact);
        let detail = detail.expect("compact picker retains a short detail panel");
        assert_eq!(list.x, detail.x);
        assert!(detail.y > list.y, "detail must stack below the list");
        assert!(list.bottom() <= detail.y);

        let wide = Rect::new(0, 0, 100, 20);
        let (list, detail) = picker_regions(wide);
        let detail = detail.expect("wide picker has a detail rail");
        assert_eq!(list.y, detail.y);
        assert!(detail.x > list.x, "wide detail must sit beside the list");
    }

    #[test]
    fn visible_row_hits_never_escape_the_painted_list() {
        let list = Rect::new(4, 8, 20, 5);
        assert_eq!(visible_row_hit(list, 0, 3), Some(Rect::new(4, 8, 20, 3)));
        assert_eq!(
            visible_row_hit(list, 1, 3),
            Some(Rect::new(4, 11, 20, 2)),
            "the partially visible final row is clamped to the list bottom"
        );
        assert_eq!(visible_row_hit(list, 2, 3), None);
    }

    #[test]
    fn long_search_values_keep_the_tail_and_cursor_side_visible() {
        let query = "provider/very-long-model-name-with-a-useful-tail";
        let window = tail_window(query, 18);
        assert!(window.starts_with('…'));
        assert!(window.ends_with("useful-tail"));
        assert!(UnicodeWidthStr::width(window.as_str()) <= 18);

        let emoji = tail_window("prefix-👩🏽‍💻-selected", 12);
        assert!(UnicodeWidthStr::width(emoji.as_str()) <= 12);
        assert!(emoji.ends_with("selected"));
    }

    #[test]
    fn composer_height_accounts_for_soft_wraps() {
        let draft = "x".repeat(100);
        assert_eq!(composer_box_height(&draft, 120), COMPOSER_HEIGHT);
        assert!(composer_box_height(&draft, 40) >= 4);
        assert_eq!(
            composer_box_height(&"x".repeat(1_000), 40),
            COMPOSER_MAX_HEIGHT
        );
    }
    use crate::state::{MemoryCard, ModelCard, ModelLocationLabel, Pane, ProviderCard, SkillCard};
    use chrono::Utc;
    use codypendent_protocol::{
        Actor, ApprovalId, ArtifactId, ArtifactRef, ChangeSetId, DataClassification, EventBody,
        ModelId, ProposedAction, Risk, RiskLevel, RunId, SessionEvent, ToolOutcome,
    };
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn system_ev(body: EventBody) -> Action {
        Action::daemon_event(SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body,
        })
    }

    fn render_to_string(state: &AppState, w: u16, h: u16) -> String {
        let theme = Theme::dark();
        buffer_text(&render_buffer(state, w, h, &theme))
    }

    fn render_buffer(state: &AppState, w: u16, h: u16, theme: &Theme) -> Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, state, theme)).expect("draw");
        terminal.backend().buffer().clone()
    }

    fn council_model(id: &str, provider: &str) -> ModelCard {
        ModelCard {
            id: ModelId(id.to_owned()),
            provider: provider.to_owned(),
            readiness: ModelReadiness::Ready,
            location: None,
            cost_per_1k_usd: None,
            context_tokens: None,
        }
    }

    #[test]
    fn council_builder_renders_the_full_flow_at_compact_and_wide_sizes() {
        let mut state = AppState::new();
        state.models = vec![
            council_model("claude-reviewer", "claude-code"),
            council_model("kimi-architect", "kimi-code"),
            council_model("amp-chair", "amp"),
        ];
        state.overlay = Overlay::CouncilBuilder(CouncilBuilderState::default());
        let name = render_to_string(&state, 80, 24);
        assert!(
            name.contains("Create agent council"),
            "wizard title:\n{name}"
        );
        assert!(name.contains("Council name"), "name field:\n{name}");

        let builder = CouncilBuilderState {
            step: CouncilBuilderStep::MemberModel,
            name: "design-council".to_owned(),
            description: "Architecture review".to_owned(),
            members: vec![
                crate::state::CouncilMemberDraft {
                    model: "claude-reviewer".to_owned(),
                    role: "security reviewer".to_owned(),
                },
                crate::state::CouncilMemberDraft {
                    model: "kimi-architect".to_owned(),
                    role: "systems architect".to_owned(),
                },
            ],
            chair: None,
            rounds: 1,
            query: String::new(),
            selected: 0,
            pending_member_model: None,
            role: String::new(),
        };
        state.overlay = Overlay::CouncilBuilder(builder.clone());
        let members = render_to_string(&state, 120, 36);
        assert!(
            members.contains("Continue with 2 members"),
            "continue row:\n{members}"
        );
        assert!(
            members.contains("security reviewer"),
            "member role:\n{members}"
        );
        assert!(
            members.contains("amp-chair"),
            "remaining profile:\n{members}"
        );
        assert!(state
            .hit_map
            .borrow()
            .iter()
            .any(|(_, action)| matches!(action, Action::ActivateRow(0))));

        state.overlay = Overlay::CouncilBuilder(CouncilBuilderState {
            step: CouncilBuilderStep::Review,
            chair: Some("amp-chair".to_owned()),
            rounds: 2,
            ..builder
        });
        let review = render_to_string(&state, 80, 24);
        assert!(review.contains("Ready to create"), "review card:\n{review}");
        assert!(review.contains("design-council"), "review name:\n{review}");
        assert!(review.contains("amp-chair"), "review chair:\n{review}");
        assert!(
            review.contains("Enter create council"),
            "review action:\n{review}"
        );
    }

    #[test]
    fn selected_picker_children_use_the_selection_foreground_in_every_theme() {
        let mut state = AppState::new();
        state.models = vec![ModelCard {
            id: ModelId("provider/a-model-with-supporting-metadata".to_owned()),
            provider: "Example Provider".to_owned(),
            readiness: ModelReadiness::Ready,
            location: Some(ModelLocationLabel::Hosted),
            cost_per_1k_usd: Some(0.03),
            context_tokens: Some(128_000),
        }];
        state.overlay = Overlay::ModelPicker {
            query: String::new(),
            selected: 0,
        };

        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::high_contrast(),
            Theme::color_blind_safe(),
            Theme::ansi256(),
            Theme::ansi16(),
            Theme::monochrome(),
        ] {
            let buffer = render_buffer(&state, 100, 30, &theme);
            let mut selected_cells = 0;
            for cell in buffer.content() {
                if cell.bg == theme.selection.background && !cell.symbol().trim().is_empty() {
                    selected_cells += 1;
                    assert_eq!(
                        cell.fg,
                        theme.selection.foreground,
                        "selected child {:?} kept an unsafe foreground in theme {theme:?}",
                        cell.symbol()
                    );
                }
            }
            assert!(
                selected_cells >= 12,
                "expected head and two supporting lines to be selected in {theme:?}"
            );
        }
    }

    #[test]
    fn rendered_modal_muted_cells_meet_wcag_contrast_in_dark_and_light_themes() {
        fn luminance(color: Color) -> f64 {
            let Color::Rgb(r, g, b) = color else {
                panic!("this regression test uses true-color themes")
            };
            let linear = |channel: u8| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        }
        fn contrast(foreground: Color, background: Color) -> f64 {
            let foreground = luminance(foreground);
            let background = luminance(background);
            (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
        }

        let mut state = AppState::new();
        state.overlay = Overlay::ModePicker {
            query: String::new(),
            selected: 0,
        };
        for theme in [Theme::dark(), Theme::light()] {
            let buffer = render_buffer(&state, 100, 30, &theme);
            let mut checked = 0;
            for cell in buffer.content() {
                if cell.fg == theme.text.muted
                    && (cell.bg == theme.surface.panel || cell.bg == theme.surface.overlay)
                    && !cell.symbol().trim().is_empty()
                {
                    checked += 1;
                    let ratio = contrast(cell.fg, cell.bg);
                    assert!(
                        ratio >= 4.5,
                        "rendered {:?} has only {ratio:.2}:1 contrast in {theme:?}",
                        cell.symbol()
                    );
                }
            }
            assert!(
                checked >= 10,
                "expected actual muted modal cells in {theme:?}"
            );
        }
    }

    #[test]
    fn modal_scrim_dims_the_base_and_the_interior_shields_click_through() {
        let mut state = AppState::new();
        state.overlay = Overlay::ModePicker {
            query: String::new(),
            selected: 0,
        };
        let theme = Theme::dark();
        let buffer = render_buffer(&state, 100, 30, &theme);
        let rect = centered_modal(Rect::new(0, 0, 100, 30), 72, 18);
        assert!(
            buffer[(0, 2)].modifier.contains(Modifier::DIM),
            "the native surface outside the modal must be visibly dimmed"
        );
        assert!(
            !buffer[(rect.x + 2, rect.y + 2)]
                .modifier
                .contains(Modifier::DIM),
            "the repainted modal interior must stay crisp"
        );

        let resolve = |x, y| {
            state
                .hit_map
                .borrow()
                .iter()
                .rev()
                .find(|(hit, _)| x >= hit.x && x < hit.right() && y >= hit.y && y < hit.bottom())
                .map(|(_, action)| action.clone())
        };
        assert_eq!(
            resolve(rect.right() - 2, rect.bottom() - 2),
            Some(Action::NoOp),
            "unused modal interior must not dismiss or activate the base"
        );
        assert_eq!(resolve(0, 2), Some(Action::Dismiss));
    }

    #[test]
    fn a_soft_wrapped_composer_keeps_its_cursor_visible() {
        let mut state = AppState::new();
        state.composer = "a long narrow draft ".repeat(12);
        let text = render_to_string(&state, 40, 18);
        assert!(
            text.contains('▏'),
            "composer cursor scrolled out of view:\n{text}"
        );
        assert_eq!(
            composer_box_height(&state.composer, 40),
            COMPOSER_MAX_HEIGHT
        );
    }

    fn running_build_state() -> AppState {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::SessionCreated {
                title: "fix-tests".to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "diagnose the failing test".to_owned(),
                mode: codypendent_protocol::AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        reduce(
            &mut s,
            Action::daemon_event(SessionEvent {
                sequence: 2,
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::Agent {
                    agent_id: codypendent_protocol::AgentId::new(),
                    run_id,
                    model: ModelId("gpt-5.1-codex".to_owned()),
                },
                body: EventBody::ModelStreamDelta {
                    run_id,
                    text: "Reading the test to see why it fails.".to_owned(),
                },
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc123".to_owned(),
                label: None,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "shell.run".to_owned(),
                outcome: ToolOutcome::Succeeded,
                artifact: None,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::BudgetWarning {
                run_id,
                dimension: BudgetDimension::Tokens,
                used: 42_000,
                limit: 100_000,
            }),
        );
        s
    }

    #[test]
    fn transcript_snapshot_shows_model_tool_and_status() {
        let state = running_build_state();
        let text = render_to_string(&state, 110, 30);

        // Transcript content.
        assert!(text.contains("shell.run"), "tool card missing:\n{text}");
        assert!(
            text.contains("diagnose the failing"),
            "objective missing:\n{text}"
        );
        // Status line projections.
        assert!(text.contains("Build"), "mode missing:\n{text}");
        assert!(text.contains("Running"), "run state missing:\n{text}");
        assert!(text.contains("gpt-5.1-codex"), "model missing:\n{text}");
        assert!(text.contains("42%"), "context %% missing:\n{text}");
        assert!(
            !text.contains("approval"),
            "zero approvals should stay out of the primary shell:\n{text}"
        );
    }

    #[test]
    fn wide_conversation_uses_a_centered_reading_measure() {
        let state = running_build_state();
        let wide = render_to_string(&state, 160, 40);
        let wide_user = wide
            .lines()
            .find(|line| line.trim_start().starts_with("You"))
            .expect("user turn rendered");
        assert!(
            wide_user.len() - wide_user.trim_start().len() >= 20,
            "wide prose should be centred instead of spanning the terminal:\n{wide}"
        );

        let compact = render_to_string(&state, 80, 24);
        let compact_user = compact
            .lines()
            .find(|line| line.trim_start().starts_with("You"))
            .expect("compact user turn rendered");
        assert!(
            compact_user.len() - compact_user.trim_start().len() <= 4,
            "compact terminals should retain nearly all available width:\n{compact}"
        );
    }

    // --- D2: startup splash ---

    fn render_splash_to_string(
        tick: u64,
        stage: &str,
        warnings: &[String],
        ready: bool,
        w: u16,
        h: u16,
    ) -> String {
        let theme = Theme::dark();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| render_splash(f, tick, stage, warnings, ready, &theme))
            .expect("draw");
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn splash_shows_wordmark_tagline_version_and_stage() {
        let text = render_splash_to_string(0, "connecting…", &[], false, 100, 30);
        assert!(text.contains("███"), "block wordmark missing:\n{text}");
        assert!(
            text.contains("Many agents. One shared workspace"),
            "tagline missing:\n{text}"
        );
        assert!(
            text.contains(&format!("v{}", codypendent_protocol::BUILD_ID)),
            "version missing:\n{text}"
        );
        assert!(text.contains("connecting…"), "stage missing:\n{text}");
    }

    #[test]
    fn splash_spinner_varies_with_tick() {
        let first = render_splash_to_string(0, "connecting…", &[], false, 100, 30);
        let second = render_splash_to_string(1, "connecting…", &[], false, 100, 30);
        assert!(
            first.contains('⠋'),
            "tick-0 spinner frame missing:\n{first}"
        );
        assert!(
            second.contains('⠙'),
            "tick-1 spinner frame missing:\n{second}"
        );
        assert_ne!(first, second, "spinner did not animate across ticks");
    }

    #[test]
    fn splash_falls_back_to_plain_name_on_narrow_terminals() {
        let text = render_splash_to_string(0, "loading workspace…", &[], false, 50, 24);
        assert!(text.contains("codypendent"), "plain name missing:\n{text}");
        assert!(
            !text.contains("███"),
            "block wordmark should drop at 50 cols:\n{text}"
        );
        assert!(
            text.contains("loading workspace…"),
            "stage missing:\n{text}"
        );
    }

    #[test]
    fn splash_drops_tagline_and_version_on_short_terminals() {
        let text = render_splash_to_string(0, "starting daemon…", &[], false, 100, 6);
        assert!(text.contains("codypendent"), "plain name missing:\n{text}");
        assert!(text.contains("starting daemon…"), "stage missing:\n{text}");
        assert!(
            !text.contains("Many agents. One shared workspace"),
            "tagline should drop at 6 rows:\n{text}"
        );
    }

    #[test]
    fn ready_splash_requires_enter_and_shows_first_run_shortcuts() {
        let text = render_splash_to_string(4, "codypendent is ready", &[], true, 100, 30);
        assert!(text.contains("✓"), "ready mark missing:\n{text}");
        assert!(
            text.contains("codypendent is ready"),
            "ready state missing:\n{text}"
        );
        assert!(text.contains("ENTER"), "Enter keycap missing:\n{text}");
        assert!(
            text.contains("open workspace"),
            "call to action missing:\n{text}"
        );
        assert!(
            text.contains("F2 workspace"),
            "shortcut help missing:\n{text}"
        );
        assert!(
            !text.contains('⠼'),
            "ready state must not look busy:\n{text}"
        );
    }

    #[test]
    fn splash_renders_boot_warnings_below_the_stage_line() {
        let warnings = vec![
            "daemon build mismatch; continuing on the running build".to_owned(),
            "could not list model profiles: db locked".to_owned(),
        ];
        let text = render_splash_to_string(0, "restoring session…", &warnings, false, 100, 30);
        assert!(
            text.contains("restoring session…"),
            "stage missing:\n{text}"
        );
        assert!(
            text.contains("daemon build mismatch"),
            "first warning missing:\n{text}"
        );
        assert!(
            text.contains("could not list model profiles"),
            "second warning missing:\n{text}"
        );
        // The warnings render BELOW the stage line.
        let stage_row = text.find("restoring session…").expect("stage");
        let warning_row = text.find("daemon build mismatch").expect("warning");
        assert!(
            warning_row > stage_row,
            "the warnings must render below the stage line:\n{text}"
        );
    }

    #[test]
    fn splash_truncates_a_long_warning_list_with_an_overflow_line() {
        let warnings: Vec<String> = (1..=6).map(|n| format!("warning number {n}")).collect();
        let text = render_splash_to_string(0, "connecting…", &warnings, false, 100, 30);
        for kept in &warnings[..MAX_SPLASH_WARNINGS] {
            assert!(text.contains(kept), "{kept} should render:\n{text}");
        }
        for dropped in &warnings[MAX_SPLASH_WARNINGS..] {
            assert!(
                !text.contains(dropped),
                "{dropped} should be truncated:\n{text}"
            );
        }
        assert!(
            text.contains("+2 more"),
            "the overflow count is missing:\n{text}"
        );
    }

    // --- D3: chat header ---

    /// A state with every user-facing header field populated: session title,
    /// model, a distinct next-run mode, context %, and cost. The daemon build
    /// id is deliberately populated too so the tests prove it stays in the
    /// diagnostics surface instead of leaking into the primary shell.
    fn header_state() -> AppState {
        let mut s = running_build_state();
        s.daemon_build_id = Some("0.9.0+abc1234".to_owned());
        s.default_mode = codypendent_protocol::AgentMode::Plan;
        s.runs[0].cost_minor = Some(1234);
        s
    }

    /// Render and return just the header bar (row 0), so presence/absence
    /// assertions never match transcript text further down.
    fn header_line(state: &AppState, w: u16) -> String {
        render_to_string(state, w, 30)
            .lines()
            .next()
            .expect("a header row")
            .to_owned()
    }

    #[test]
    fn header_shows_every_field_at_full_width() {
        let text = header_line(&header_state(), 120);
        assert!(text.contains("codypendent"), "brand missing:\n{text}");
        assert!(text.contains("fix-tests"), "session title missing:\n{text}");
        assert!(text.contains("gpt-5.1-codex"), "model missing:\n{text}");
        assert!(text.contains("Plan"), "next-run mode missing:\n{text}");
        assert!(
            !text.contains("0.9.0+abc1234"),
            "daemon build id belongs in diagnostics, not the header:\n{text}"
        );
        assert!(text.contains("42%"), "context %% missing:\n{text}");
        assert!(text.contains("$12.34"), "cost missing:\n{text}");
    }

    #[test]
    fn header_drops_build_id_at_mid_width() {
        let text = header_line(&header_state(), 80);
        assert!(text.contains("gpt-5.1-codex"), "model missing:\n{text}");
        assert!(text.contains("Plan"), "next-run mode missing:\n{text}");
        assert!(
            !text.contains("0.9.0+abc1234"),
            "build id should drop at 80 cols:\n{text}"
        );
    }

    #[test]
    fn header_drops_model_mode_and_build_id_at_narrow_width() {
        let text = header_line(&header_state(), 50);
        assert!(text.contains("codypendent"), "brand missing:\n{text}");
        assert!(text.contains("fix-tests"), "session title missing:\n{text}");
        assert!(
            !text.contains("gpt-5.1-codex"),
            "model should drop at 50 cols:\n{text}"
        );
        assert!(
            !text.contains("Plan"),
            "mode should drop at 50 cols:\n{text}"
        );
        assert!(
            !text.contains("0.9.0+abc1234"),
            "build id should drop at 50 cols:\n{text}"
        );
        assert!(text.contains("42%"), "context %% missing:\n{text}");
        assert!(text.contains("$12.34"), "cost missing:\n{text}");
    }

    #[test]
    fn header_renders_in_the_workspace_layout_too() {
        let mut state = header_state();
        state.layout = LayoutMode::Workspace;
        let text = header_line(&state, 120);
        assert!(text.contains("codypendent"), "brand missing:\n{text}");
        assert!(
            !text.contains("0.9.0+abc1234"),
            "workspace uses the same quiet project header:\n{text}"
        );
    }

    #[test]
    fn workspace_collapses_to_the_focused_pane_below_110_columns() {
        let mut state = running_build_state();
        state.layout = LayoutMode::Workspace;
        state.focus = Pane::Transcript;
        let transcript = render_to_string(&state, 100, 28);
        assert!(transcript.contains("Conversation"));
        assert!(!transcript.contains("Runs (1)"));
        assert!(!transcript.contains("Approvals (0)"));

        state.focus = Pane::Sessions;
        let sessions = render_to_string(&state, 100, 28);
        assert!(sessions.contains("Runs (1)"));
        assert!(!sessions.contains("Conversation"));

        state.focus = Pane::Transcript;
        let wide = render_buffer(&state, 140, 32, &Theme::dark());
        let text = buffer_text(&wide);
        assert!(text.contains("Runs (1)"));
        assert!(text.contains("Conversation"));
        assert!(text.contains("Approvals (0)"));
        let transcript_x = 140 * 26 / 100;
        assert_eq!(
            wide[(transcript_x, 1)].fg,
            Theme::dark().focus.active,
            "the focused transcript border should be visually active"
        );
    }

    /// Task 5 (codex chat shell): the collapsed tool card head restyles into
    /// one compact Codex-style line — a run glyph (`⏺`) and the tool's
    /// verb/name, with a terse outcome mark instead of the old `[status]`
    /// bracket.
    #[test]
    fn a_completed_tool_card_renders_compact_with_a_run_glyph_and_check() {
        let state = running_build_state();
        let text = render_to_string(&state, 110, 30);
        assert!(
            text.contains("⏺ shell.run"),
            "run glyph + name missing:\n{text}"
        );
        assert!(text.contains('✓'), "success outcome mark missing:\n{text}");
        assert!(
            !text.contains("[done]"),
            "old bracket style must be gone:\n{text}"
        );
    }

    /// A tool still awaiting a decision (`ToolStatus::Proposed`) shows the
    /// same `⟳ review` marker a patch does, instead of the old `[proposed]`.
    #[test]
    fn a_proposed_tool_card_shows_a_review_marker() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolProposed {
                run_id,
                approval_id: ApprovalId::new(),
                action: ProposedAction::ExecuteCommand {
                    program: "cargo".to_owned(),
                    args: vec!["test".to_owned()],
                    environment: Vec::new(),
                    cwd: None,
                },
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains("⟳ review"), "review marker missing:\n{out}");
        assert!(
            !out.contains("[proposed]"),
            "old bracket style must be gone:\n{out}"
        );
    }

    /// PR B (MCP client): the approval card renders an `McpToolCall` with the
    /// server, tool, human summary, and the `args` string VERBATIM (canonical
    /// JSON, already auditable), and the short verb is `mcp tool` — never the
    /// wildcard fallbacks.
    #[test]
    fn mcp_tool_call_describes_server_tool_summary_and_args_verbatim() {
        let action = ProposedAction::McpToolCall {
            server: "github".to_owned(),
            tool: "create_issue".to_owned(),
            summary: "create an issue titled bug".to_owned(),
            args: "{\"labels\":[\"bug\"],\"title\":\"bug\"}".to_owned(),
        };
        assert_eq!(
            describe_action(&action),
            vec![
                "mcp tool: github.create_issue".to_owned(),
                "summary: create an issue titled bug".to_owned(),
                "args: {\"labels\":[\"bug\"],\"title\":\"bug\"}".to_owned(),
            ]
        );
        assert_eq!(action_kind(&action), "mcp tool");
    }

    /// A failed tool card shows a terse `✗` in the collapsed head; the
    /// failure message itself stays in the expanded detail (unchanged).
    #[test]
    fn a_failed_tool_card_shows_a_cross_mark() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "d".to_owned(),
                label: None,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "shell.run".to_owned(),
                outcome: ToolOutcome::Failed {
                    message: "exit 1".to_owned(),
                },
                artifact: None,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains('✗'), "failure outcome mark missing:\n{out}");
        assert!(
            !out.contains("[failed]"),
            "old bracket style must be gone:\n{out}"
        );
    }

    /// A `ToolStarted.label` (e.g. the file `workspace.read_file` targets)
    /// renders after the tool name as `{tool} · {label}`, ahead of the
    /// outcome mark — so the card reads e.g. `workspace.read_file ·
    /// services/main.py ✓` and the user can see WHICH file, not just that a
    /// read happened.
    #[test]
    fn a_tool_card_with_a_label_shows_tool_dot_label() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "workspace.read_file".to_owned(),
                args_digest: "abc".to_owned(),
                label: Some("services/main.py".to_owned()),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(
            out.contains("workspace.read_file · services/main.py"),
            "tool · label missing:\n{out}"
        );
    }

    /// Without a label (an older daemon, or a tool `tool_label` does not
    /// recognize) the head renders exactly as it always did — no bare `·`
    /// with nothing after it.
    #[test]
    fn a_tool_card_without_a_label_renders_unchanged() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc".to_owned(),
                label: None,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        // Exactly the pre-existing collapsed head: tool name then the status
        // word, no `·` injected between them (the header/footer chrome uses
        // `·` for unrelated separators elsewhere in this same screen, so the
        // check is scoped to the tool card's own line rather than the whole
        // render).
        assert!(
            out.contains("⏺ shell.run running"),
            "unchanged tool-card head missing:\n{out}"
        );
        assert!(
            !out.contains("shell.run ·"),
            "no dot separator after the tool name without a label:\n{out}"
        );
    }

    /// A label longer than the render-layer cap is truncated with a trailing
    /// `…` so a pathological (or simply long) label can never blow out the
    /// one-line card, independent of whatever bound the daemon side applied.
    #[test]
    fn an_overlong_label_is_truncated_in_the_render_layer() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let long_label = "a/".repeat(60) + "file.rs";
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "workspace.read_file".to_owned(),
                args_digest: "abc".to_owned(),
                label: Some(long_label.clone()),
            }),
        );
        let out = render_to_string(&s, 120, 20);
        assert!(
            !out.contains(&long_label),
            "label must be truncated:\n{out}"
        );
        assert!(out.contains('…'), "truncation ellipsis missing:\n{out}");
    }

    /// A collapsed patch names its affected file and honest diff stats. The
    /// old permanent "review" marker implied an action that did not exist.
    #[test]
    fn a_patch_card_renders_compact_with_a_patch_glyph_and_review_marker() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::PatchProposed {
                run_id,
                changeset_id: ChangeSetId::new(),
                artifact: filler_chronicle(),
                files: vec!["src/lib.rs".to_owned()],
                additions: 2,
                deletions: 1,
                preview: "@@ -1 +1 @@\n-old\n+new".to_owned(),
                preview_truncated: false,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains("◆ src/lib.rs"), "file summary missing:\n{out}");
        assert!(out.contains("+2 −1"), "diff stats missing:\n{out}");
        assert!(out.contains("changes ready"), "state missing:\n{out}");
        assert!(
            !out.contains("⟳ review"),
            "the shell must not advertise a fake review action:\n{out}"
        );
        assert!(
            !out.contains("patch proposed ("),
            "old verbose label must be gone:\n{out}"
        );
    }

    /// Task 3: a run that is `Preparing`/`Running` with no model text
    /// streaming yet shows a dim "working…" row, so it never looks silently
    /// paused between transcript updates.
    #[test]
    fn a_thinking_run_shows_a_working_status_row() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains("working…"), "status row missing:\n{out}");
    }

    /// Task 3: while a tool is executing, the status row names it instead of
    /// the generic "working…" — e.g. "running shell.run…".
    #[test]
    fn a_running_tool_status_row_names_the_tool() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc".to_owned(),
                label: None,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(
            out.contains("running shell.run…"),
            "tool status row missing:\n{out}"
        );
    }

    /// Task 3: a fresh run (no `RunStateChanged` yet) is `Idle`, and `Idle`
    /// renders no status row at all — the row must not appear by default.
    #[test]
    fn an_idle_run_shows_no_status_row() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(!out.contains("working…"), "unexpected status row:\n{out}");
        assert!(!out.contains("running "), "unexpected status row:\n{out}");
    }

    /// Task 4: while a run's activity is `Streaming`, the model cell shows a
    /// muted `▋` caret right after the accumulated text, so the mid-stream
    /// cell reads as still-writing rather than silently paused; the caret is
    /// derived render state, never stored, so it drops the instant the run
    /// completes.
    #[test]
    fn a_streaming_cell_shows_a_caret_then_drops_it_on_completion() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "partial".to_owned(),
            }),
        );
        let mid = render_to_string(&s, 80, 20);
        assert!(mid.contains("partial"), "streamed text missing:\n{mid}");
        assert!(mid.contains('▋'), "streaming cell shows a caret:\n{mid}");

        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("partial".to_owned()),
                },
                chronicle: ArtifactRef {
                    id: ArtifactId::new(),
                    media_type: "application/json".to_owned(),
                    byte_length: 10,
                    sha256: "0".repeat(64),
                    sensitivity: DataClassification::Internal,
                },
            }),
        );
        let done = render_to_string(&s, 80, 20);
        assert!(
            !done.contains('▋'),
            "caret is gone once the run completes:\n{done}"
        );
    }

    /// A completed helper: `RunCompleted` needs a `chronicle` artifact
    /// reference alongside the disposition; every disposition test below
    /// uses this filler ref, only the disposition itself is under test.
    fn filler_chronicle() -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_owned(),
            byte_length: 10,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        }
    }

    /// Task 3 (codex chat shell): a successful run's reply already ended the
    /// turn as streamed model prose, so the `Completed` cell must render
    /// nothing — no second/third echo of the same reply — and the turn's
    /// first agent cell gets a `⏺ codypendent` header.
    #[test]
    fn a_completed_success_shows_the_reply_once_no_echo() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "hello there".to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("hello there".to_owned()),
                },
                chronicle: filler_chronicle(),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert_eq!(
            out.matches("hello there").count(),
            1,
            "reply appears exactly once, not echoed by Completed:\n{out}"
        );
        assert!(!out.contains("run completed"), "no completed echo:\n{out}");
        assert!(
            out.contains("⏺ codypendent"),
            "assistant header shown before the reply:\n{out}"
        );
    }

    /// A run that fails before producing any prose has no `Model`/`Tool`/
    /// `Patch` cell — so it must not render a lone `⏺ codypendent` header
    /// with nothing under it. It shows its failure reason, tersely (no
    /// leftover "run failed:" verbiage from the old always-visible echo).
    #[test]
    fn a_failed_run_shows_its_reason() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Failed {
                    reason: "no model configured".to_owned(),
                },
                chronicle: filler_chronicle(),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(
            out.contains("no model configured"),
            "failure reason shown:\n{out}"
        );
        assert!(
            !out.contains("run failed:"),
            "terse reason, not the old verbose echo:\n{out}"
        );
        assert!(
            !out.contains("⏺ codypendent"),
            "no agent cell ever ran, so no lone header:\n{out}"
        );
    }

    /// `RunDisposition::Cancelled` carries an optional reason (unlike the
    /// unit-variant the design sketch assumed) — it renders tersely too, and
    /// still surfaces the reason when one is given.
    #[test]
    fn a_cancelled_run_shows_its_reason_tersely() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Cancelled {
                    reason: Some("budget exceeded".to_owned()),
                },
                chronicle: filler_chronicle(),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains("cancelled"), "cancellation shown:\n{out}");
        assert!(out.contains("budget exceeded"), "reason shown:\n{out}");
        assert!(
            !out.contains("run cancelled:"),
            "terse form, not the old verbose echo:\n{out}"
        );
    }

    /// Task 3: a nested error chain collapses to one concise, friendly line;
    /// an unrecognized outermost segment degrades to itself verbatim (never a
    /// crash, never a fabrication) and an empty reason still says something.
    #[test]
    fn summarize_error_maps_known_chains_and_degrades_unknown() {
        assert_eq!(
            summarize_error(
                "model driver error: model stream failed: service error: request failed: builder error"
            ),
            "model error — the provider request failed"
        );
        assert_eq!(
            summarize_error("service error: request failed"),
            "provider request failed"
        );
        // Unknown outermost segment degrades to that segment verbatim (never a crash).
        assert_eq!(
            summarize_error("no model configured"),
            "no model configured"
        );
        assert_eq!(summarize_error(""), "run failed");
    }

    /// Task 3: a failed run's nested error chain renders collapsed to the
    /// concise summary by default (raw chain hidden), and expanding the
    /// selected `Completed` entry reveals the full raw chain underneath —
    /// nothing lost, just folded.
    #[test]
    fn a_failed_run_collapses_the_chain_and_expands_to_the_raw() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Failed {
                    reason: "model driver error: model stream failed: service error: request failed: builder error".to_owned(),
                },
                chronicle: filler_chronicle(),
            }),
        );

        // Width 100 (rather than a tighter 80/90) keeps the raw, unwrapped
        // chain on one row — at 88 inner columns the 89-column indented raw
        // line wraps its last word onto its own row, which would split the
        // "builder error" substring across a line break and make this a test
        // artifact rather than a signal about the real feature.
        let collapsed = render_to_string(&s, 100, 20);
        assert!(
            collapsed.contains("✗ model error — the provider request failed"),
            "summary:\n{collapsed}"
        );
        assert!(
            !collapsed.contains("builder error"),
            "raw chain hidden while collapsed:\n{collapsed}"
        );

        // Select the Completed entry and expand it.
        s.focus = Pane::Transcript;
        let last = s.runs[0].transcript.len() - 1;
        s.runs[0].transcript_selected = last;
        reduce(&mut s, Action::Expand);

        let expanded = render_to_string(&s, 100, 20);
        assert!(
            expanded.contains("✗ model error — the provider request failed"),
            "summary kept:\n{expanded}"
        );
        assert!(
            expanded.contains("builder error"),
            "raw chain revealed on expand:\n{expanded}"
        );
    }

    /// The assistant header announces only the first agent cell of a turn —
    /// a tool call followed by more model text in the same turn must not
    /// repeat it, and a `Tool` cell (not just `Model`) triggers it.
    #[test]
    fn the_assistant_header_appears_once_per_turn_even_with_multiple_agent_cells() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc123".to_owned(),
                label: None,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "shell.run".to_owned(),
                outcome: ToolOutcome::Succeeded,
                artifact: None,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "done".to_owned(),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert_eq!(
            out.matches("⏺ codypendent").count(),
            1,
            "header appears exactly once for the whole turn:\n{out}"
        );
    }

    /// Task 4 (codex chat shell): the conversation header names the serving
    /// model and the run's mode, joined `model · mode`, so the operator sees
    /// both without opening the run-detail pane. `running_build_state` learns
    /// "gpt-5.1-codex" from the agent actor but never fires a cost budget
    /// event, so cost stays unknown — it must be left out entirely rather
    /// than shown as a `$0.00`/`—` placeholder.
    #[test]
    fn the_conversation_header_shows_model_and_mode() {
        let state = running_build_state();
        let text = render_to_string(&state, 100, 30);
        assert!(
            text.contains("gpt-5.1-codex · Build"),
            "model · mode header:\n{text}"
        );
        assert!(
            !text.contains('$'),
            "unknown cost omitted, not a placeholder:\n{text}"
        );
    }

    /// A fresh run has a mode but no model learned yet — the header shows
    /// the mode alone (joined onto the title by one separator), with no
    /// extra slot or separator standing in for the still-unknown model/cost.
    #[test]
    fn the_header_shows_mode_alone_before_a_model_is_learned() {
        let mut s = AppState::new();
        s.default_mode = AgentMode::Ask;
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "o".to_owned(),
                mode: AgentMode::Ask,
            }),
        );
        let out = render_to_string(&s, 100, 20);
        let header_row = out.lines().next().expect("a project header row");
        assert!(
            header_row.contains("Ask"),
            "mode shown in the header:\n{header_row}"
        );
        assert_eq!(
            header_row.matches("openai").count(),
            0,
            "no placeholder model appears before one is learned:\n{header_row}"
        );
    }

    /// Task 4 (codex chat shell): a blank line separates turns after the
    /// first so the conversation breathes, instead of reading as one
    /// undifferentiated scroll. The reducer doesn't yet drive a second
    /// `User` turn onto a live run (steering acks as `Steering`, not
    /// `User` — see `TranscriptEntry::User`'s doc comment), so this pushes
    /// the follow-up turn directly to exercise the render-side spacing rule
    /// in isolation from that reducer wiring.
    #[test]
    fn a_blank_line_separates_turns_after_the_first() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "alpha".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        s.runs[0].transcript.push(TranscriptEntry::User {
            text: "beta".to_owned(),
        });
        let out = render_to_string(&s, 80, 20);
        let rows: Vec<&str> = out.lines().collect();
        // Search for the turn body/header themselves (not just the bare
        // word), so a match can't land on the pane title — which also shows
        // the objective ("alpha") and would otherwise be mistaken for the
        // transcript row. Rows carry the pane's left/right border glyph, so
        // strip it before comparing the header line exactly.
        let alpha_body = rows
            .iter()
            .position(|r| r.contains("  alpha"))
            .expect("first turn body");
        let beta_header = rows
            .iter()
            .skip(alpha_body)
            .position(|r| r.trim_matches('│').trim() == "You")
            .map(|p| p + alpha_body)
            .expect("second turn header");
        assert_eq!(
            beta_header,
            alpha_body + 2,
            "one blank row separates the turns:\n{out}"
        );
    }

    /// Task 5 (continuous-session plan): the bug this task fixes — each
    /// message spawned a new run, and the conversation showed only the
    /// selected run, so the previous turn disappeared the moment a new one
    /// started. `render_conversation` must now walk every run in the session,
    /// in order, as one continuous scroll.
    #[test]
    fn the_conversation_renders_every_run_in_one_continuous_scroll() {
        let mut s = AppState::new();
        let run1 = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id: run1,
                objective: "alpha".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id: run1,
                text: "alpha reply".to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunCompleted {
                run_id: run1,
                disposition: RunDisposition::Completed {
                    summary: Some("alpha reply".to_owned()),
                },
                chronicle: filler_chronicle(),
            }),
        );

        // A follow-up: a second run in the SAME session — the bug made the
        // first turn vanish the instant this one started.
        let run2 = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id: run2,
                objective: "beta".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id: run2,
                text: "beta reply".to_owned(),
            }),
        );

        let out = render_to_string(&s, 100, 30);
        assert!(
            out.contains("  alpha") && out.contains("alpha reply"),
            "the first (completed) run's turn must still be visible:\n{out}"
        );
        assert!(
            out.contains("  beta") && out.contains("beta reply"),
            "the second (live) run's turn must also be visible:\n{out}"
        );
        assert_eq!(
            out.matches("⏺ codypendent").count(),
            2,
            "each turn gets its own assistant header:\n{out}"
        );
        assert!(
            !out.contains("[2/2]") && !out.contains("2 turns"),
            "the continuous timeline needs no dashboard-style run counter:\n{out}"
        );
    }

    #[test]
    fn approval_modal_snapshot_shows_action_risk_and_capabilities() {
        let mut state = running_build_state();
        reduce(
            &mut state,
            system_ev(EventBody::ApprovalRequested {
                approval_id: ApprovalId::new(),
                action: ProposedAction::ExecuteCommand {
                    program: "cargo".to_owned(),
                    args: vec!["test".to_owned(), "--all".to_owned()],
                    environment: Vec::new(),
                    cwd: None,
                },
                risk: Risk {
                    level: RiskLevel::High,
                    reasons: vec!["runs an arbitrary command".to_owned()],
                },
            }),
        );
        assert!(state.show_approval_modal());
        let text = render_to_string(&state, 110, 34);

        assert!(text.contains("Approval required"), "title missing:\n{text}");
        // Action verbatim.
        assert!(
            text.contains("cargo test --all"),
            "verbatim command missing:\n{text}"
        );
        // Risk verbatim.
        assert!(text.contains("HIGH"), "risk level missing:\n{text}");
        assert!(
            text.contains("runs an arbitrary command"),
            "risk reason missing:\n{text}"
        );
        // Requested capabilities (derived label).
        assert!(
            text.contains("CommandExecute"),
            "capability missing:\n{text}"
        );
        // Decision keys present.
        assert!(text.contains("approve once"), "keys missing:\n{text}");
    }

    #[test]
    fn approval_modal_snapshot_shows_publish_document_plan_verbatim() {
        // STEP 4.4.2: every publish displays target, changed files, and the
        // resulting Git action before approval — the generic approval card
        // (not a bespoke docs-publish UI) must render a `PublishDocument`
        // proposal's plan content verbatim, exactly as it renders any other
        // action.
        let mut state = running_build_state();
        reduce(
            &mut state,
            system_ev(EventBody::ApprovalRequested {
                approval_id: ApprovalId::new(),
                action: ProposedAction::PublishDocument {
                    document_id: codypendent_protocol::DocumentId::new(),
                    target: "repository file docs/architecture.md".to_owned(),
                    changed_files: vec!["docs/architecture.md".to_owned()],
                    git_action: "write docs/architecture.md in the working tree \
                                 (approval-gated change set)"
                        .to_owned(),
                },
                risk: Risk {
                    level: RiskLevel::Medium,
                    reasons: vec!["writes docs/architecture.md and commits it".to_owned()],
                },
            }),
        );
        assert!(state.show_approval_modal());
        let text = render_to_string(&state, 110, 34);

        assert!(text.contains("Approval required"), "title missing:\n{text}");
        assert!(
            text.contains("repository file docs/architecture.md"),
            "target missing verbatim:\n{text}"
        );
        assert!(
            text.contains("docs/architecture.md"),
            "changed file missing verbatim:\n{text}"
        );
        assert!(
            text.contains("write docs/architecture.md in the working tree"),
            "git action missing verbatim:\n{text}"
        );
        assert!(text.contains("MED"), "risk level missing:\n{text}");
        assert!(
            text.contains("GitCommit (repository file docs/architecture.md)"),
            "capability label missing:\n{text}"
        );
    }

    #[test]
    fn help_overlay_lists_bindings() {
        let mut state = running_build_state();
        reduce(&mut state, Action::Help);
        let text = render_to_string(&state, 110, 34);
        assert!(text.contains("Help"));
        assert!(text.contains("command palette"));
        assert!(text.contains("detach"));
    }

    #[test]
    fn expanded_tool_card_shows_detail() {
        let mut state = running_build_state();
        let art = ArtifactRef {
            id: ArtifactId::new(),
            media_type: "text/plain".to_owned(),
            byte_length: 2048,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };
        let run_id = state.runs[0].run_id;
        reduce(
            &mut state,
            system_ev(EventBody::ToolProposed {
                run_id,
                approval_id: ApprovalId::new(),
                action: ProposedAction::ReadFiles {
                    paths: vec!["src/lib.rs".to_owned()],
                },
            }),
        );
        // Complete it with an artifact, then expand the selected entry.
        reduce(
            &mut state,
            system_ev(EventBody::ToolCompleted {
                run_id,
                tool: "workspace.read_file".to_owned(),
                outcome: ToolOutcome::Succeeded,
                artifact: Some(art),
            }),
        );
        state.focus = Pane::Transcript;
        let last = state.runs[0].transcript.len() - 1;
        state.runs[0].transcript_selected = last;
        reduce(&mut state, Action::Expand);

        let text = render_to_string(&state, 110, 34);
        assert!(text.contains("workspace.read_file"), "tool name:\n{text}");
        assert!(text.contains("2048 bytes"), "artifact detail:\n{text}");
    }

    #[test]
    fn long_note_folds_by_default_and_expand_reveals_the_body() {
        let mut state = running_build_state();
        let run_id = state.runs[0].run_id;
        let note = "first line of the note\nsecond line\nthird line\nfourth line".to_owned();
        reduce(
            &mut state,
            system_ev(EventBody::NoteAppended {
                text: note,
                run_id: Some(run_id),
            }),
        );
        state.focus = Pane::Transcript;
        let last = state.runs[0].transcript.len() - 1;
        state.runs[0].transcript_selected = last;

        let collapsed = render_to_string(&state, 110, 34);
        assert!(
            collapsed.contains("▸ note: first line of the note (4 lines)"),
            "collapsed head:\n{collapsed}"
        );
        assert!(
            !collapsed.contains("fourth line"),
            "the full body must not show while collapsed:\n{collapsed}"
        );

        reduce(&mut state, Action::Expand);
        let expanded = render_to_string(&state, 110, 34);
        assert!(
            expanded.contains("▾ note: first line of the note (4 lines)"),
            "expanded head:\n{expanded}"
        );
        assert!(
            expanded.contains("fourth line"),
            "the full body shows once expanded:\n{expanded}"
        );
    }

    #[test]
    fn short_note_renders_inline() {
        // Not a `remembered:`/`=== CONTEXT` note — those fold into the dim
        // `Backstage` line instead (see the backstage-fold render tests).
        let mut state = running_build_state();
        let run_id = state.runs[0].run_id;
        reduce(
            &mut state,
            system_ev(EventBody::NoteAppended {
                text: "the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );

        let text = render_to_string(&state, 110, 34);
        assert!(
            text.contains("• note: the test command is cargo test"),
            "a short note renders inline, unfolded:\n{text}"
        );
        // `running_build_state` already has a (separately foldable) tool card, so
        // check the note's own head carries no fold marker rather than scanning
        // the whole transcript for the marker glyphs.
        assert!(
            !text.contains("▸ note:") && !text.contains("▾ note:"),
            "a short note carries no fold marker:\n{text}"
        );
    }

    #[test]
    fn backstage_renders_a_dim_summary_line() {
        let mut state = running_build_state();
        let run_id = state.runs[0].run_id;
        reduce(
            &mut state,
            system_ev(EventBody::NoteAppended {
                text: "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\nline\nline\nline".to_owned(),
                run_id: Some(run_id),
            }),
        );
        reduce(
            &mut state,
            system_ev(EventBody::NoteAppended {
                text: "remembered: the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );

        let out = render_to_string(&state, 80, 34);
        assert!(
            out.contains("context") && out.contains("memory"),
            "the folded summary names both halves:\n{out}"
        );
        assert!(
            !out.contains("EVIDENCE, NOT INSTRUCTIONS"),
            "raw manifest text must stay hidden while folded:\n{out}"
        );
        assert!(
            !out.contains("• note:"),
            "context/memory notes never render as a Note cell:\n{out}"
        );
    }

    #[test]
    fn expanding_backstage_reveals_the_folded_raw_notes() {
        let mut state = running_build_state();
        let run_id = state.runs[0].run_id;
        reduce(
            &mut state,
            system_ev(EventBody::NoteAppended {
                text: "remembered: the test command is cargo test".to_owned(),
                run_id: Some(run_id),
            }),
        );
        let idx = state.runs[0]
            .transcript
            .iter()
            .position(|e| matches!(e, TranscriptEntry::Backstage { .. }))
            .expect("a Backstage entry was folded in");
        state.focus = Pane::Transcript;
        state.runs[0].transcript_selected = idx;

        reduce(&mut state, Action::Expand);
        let out = render_to_string(&state, 80, 34);
        assert!(
            out.contains("remembered: the test command is cargo test"),
            "expanded backstage shows the folded note's full text:\n{out}"
        );
        assert!(out.contains("▾"), "the expanded marker replaces ⋯:\n{out}");
    }

    #[test]
    fn renders_empty_state_without_panicking() {
        let state = AppState::new();
        let text = render_to_string(&state, 80, 24);
        // A truly fresh state cannot start a run yet; the empty conversation
        // guides model setup instead of promising Enter will work.
        assert!(text.contains("Connect your first model"));
        assert!(text.contains("Provider catalog"));
    }

    #[test]
    fn conversation_shell_shows_transcript_composer_and_footer() {
        // A live run: the transcript is the main surface, the composer offers to
        // steer it, and the status footer spans the bottom.
        let state = running_build_state();
        let text = render_to_string(&state, 100, 30);

        // The quiet project header names the session; the objective belongs
        // to the user turn in the continuous timeline.
        assert!(text.contains("fix-tests"), "session in title:\n{text}");
        assert!(
            text.contains("diagnose the failing test"),
            "run objective:\n{text}"
        );
        // The persistent composer + its steering placeholder (the run is live).
        assert!(text.contains("❯"), "composer prompt:\n{text}");
        assert!(
            text.contains("Add guidance while the agent works"),
            "steer placeholder:\n{text}"
        );
        assert!(text.contains("Running"), "context footer:\n{text}");
    }

    #[test]
    fn composer_shows_a_typed_draft() {
        let mut state = running_build_state();
        for c in "add a boundary check".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        let text = render_to_string(&state, 100, 30);
        assert!(
            text.contains("add a boundary check"),
            "draft not shown:\n{text}"
        );
    }

    #[test]
    fn a_manual_newline_renders_as_two_composer_rows_and_grows_the_box() {
        let mut state = running_build_state();
        for c in "line one\nline two".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        let text = render_to_string(&state, 100, 30);
        let lines: Vec<&str> = text.lines().collect();
        // Both segments show up as their own rows, not concatenated onto one.
        let one_row = lines.iter().position(|l| l.contains("line one"));
        let two_row = lines.iter().position(|l| l.contains("line two"));
        assert!(
            one_row.is_some() && two_row.is_some(),
            "both lines should render:\n{text}"
        );
        assert_eq!(
            two_row.unwrap(),
            one_row.unwrap() + 1,
            "the second segment should be the very next row, not merged onto the first:\n{text}"
        );
        assert!(
            !lines[one_row.unwrap()].contains("line one line two")
                && !lines[one_row.unwrap()].contains("line oneline two"),
            "the newline must not be swallowed onto a single row:\n{text}"
        );
    }

    #[test]
    fn workspace_layout_adds_runs_and_approvals_panes() {
        // Toggling to the workspace layout flanks the conversation with a runs
        // pane and an approvals + detail pane — the composer/footer are unchanged.
        let mut state = running_build_state();
        reduce(&mut state, Action::ToggleLayout);
        let text = render_to_string(&state, 120, 30);

        assert!(text.contains("Runs"), "runs pane missing:\n{text}");
        assert!(
            text.contains("Approvals"),
            "approvals pane missing:\n{text}"
        );
        // The conversation is still the centre surface.
        assert!(text.contains("fix-tests"), "conversation title:\n{text}");
        // The composer and status footer persist across the toggle.
        assert!(text.contains("❯"), "composer:\n{text}");
        assert!(text.contains("Running"), "status footer:\n{text}");

        // Toggling back returns to the single-column chat (no Runs pane title).
        reduce(&mut state, Action::ToggleLayout);
        let chat = render_to_string(&state, 120, 30);
        assert!(!chat.contains("Runs ("), "should be single-column:\n{chat}");
    }

    #[test]
    fn the_context_footer_keeps_only_high_value_commands() {
        let state = running_build_state();
        let out = render_to_string(&state, 100, 30);
        assert!(out.contains("Running"), "live state:\n{out}");
        assert!(out.contains("commands"), "commands chip:\n{out}");
        assert!(out.contains("workspace"), "workspace chip:\n{out}");
        assert!(
            !out.contains("help"),
            "rare controls belong in the command palette:\n{out}"
        );
    }

    #[test]
    fn inline_composer_and_context_footer_do_not_clip_each_other() {
        let state = running_build_state();
        let height = 30u16;
        let text = render_to_string(&state, 100, height);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            height as usize,
            "expected exactly {height} rendered rows:\n{text}"
        );

        // Bottom-most row: the one contextual footer.
        let footer_row = lines[lines.len() - 1];
        assert!(
            footer_row.contains("Running") && footer_row.contains("commands"),
            "bottom row should be the context footer:\n{footer_row:?}"
        );

        let composer_start = lines.len() - 1 - COMPOSER_HEIGHT as usize;
        let composer_end = lines.len() - 1;
        let composer_rows = lines[composer_start..composer_end].join("\n");
        assert!(
            composer_rows.contains('❯'),
            "composer should keep its full height, unclipped:\n{composer_rows}"
        );
    }

    #[test]
    fn contextual_footer_switches_hint_by_context() {
        // Live and not drafting: operational state + global entry points.
        let mut state = running_build_state();
        let idle = render_to_string(&state, 120, 30);
        assert!(idle.contains("Running"), "run state:\n{idle}");
        assert!(
            idle.contains("commands") || idle.contains("F2"),
            "footer command chips:\n{idle}"
        );

        // Drafting replaces ambient state with editing actions.
        for c in "hello".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        let drafting = render_to_string(&state, 120, 30);
        assert!(
            drafting.contains("Draft ready") && drafting.contains("send"),
            "draft actions in the footer:\n{drafting}"
        );
    }

    #[test]
    fn contextual_footer_narrows_by_dropping_low_priority_fields() {
        let state = running_build_state();
        let narrow = render_to_string(&state, 50, 30);
        // Operational state survives; duplicated metadata is absent.
        assert!(narrow.contains("Running"), "run state kept:\n{narrow}");
        assert!(
            !narrow.contains("model"),
            "model dropped when narrow:\n{narrow}"
        );
    }

    #[test]
    fn the_context_footer_drops_duplicate_model_and_unknown_placeholders() {
        let state = running_build_state();
        let out = render_to_string(&state, 120, 30);
        // The model is deduped out of the status line's ambient fields — but still
        // shown in the header chrome + the assistant turn header.
        let status_row = out.lines().last().unwrap_or("");
        assert!(
            !status_row.contains("model"),
            "no `model` field on the status line:\n{status_row}"
        );
        assert!(
            out.contains("gpt-5.1-codex"),
            "model still shown in chrome/turn header:\n{out}"
        );
        // Unknown fields are omitted entirely instead of filling the calm
        // primary shell with em-dash dashboard slots.
        assert!(
            !status_row.contains("—"),
            "unknown ambient fields should be omitted:\n{status_row}"
        );
    }

    #[test]
    fn project_header_adds_context_only_after_a_budget_event() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::SessionCreated {
                title: "fix-tests".to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "diagnose the failing test".to_owned(),
                mode: codypendent_protocol::AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Running,
            }),
        );

        let before = render_to_string(&s, 110, 30);
        assert!(
            !before.contains("ctx"),
            "unknown context should be omitted:\n{before}"
        );
        assert!(
            !before.contains("ctx 25%"),
            "must never fabricate a percent:\n{before}"
        );

        reduce(
            &mut s,
            system_ev(EventBody::BudgetWarning {
                run_id,
                dimension: BudgetDimension::Tokens,
                used: 8_192,
                limit: 32_768,
            }),
        );
        let after = render_to_string(&s, 110, 30);
        assert!(
            after.contains("ctx 25%"),
            "footer comes alive on BudgetWarning{{Tokens}}:\n{after}"
        );
    }

    #[test]
    fn skill_studio_snapshot_shows_permissions_verbatim() {
        let mut state = running_build_state();
        state.skills = vec![SkillCard {
            name: "rust.fix-ci".to_owned(),
            kind: "skill".to_owned(),
            scope: "repository".to_owned(),
            trust: "first-party".to_owned(),
            status: "active".to_owned(),
            risk: "medium".to_owned(),
            description: "diagnose and fix a failing CI run".to_owned(),
            permissions: vec![
                "filesystem_read: $REPOSITORY".to_owned(),
                "command: cargo".to_owned(),
            ],
        }];
        reduce(&mut state, Action::OpenSkills);
        let text = render_to_string(&state, 120, 40);

        assert!(text.contains("Skill Studio"), "title missing:\n{text}");
        assert!(text.contains("rust.fix-ci"), "skill name missing:\n{text}");
        assert!(text.contains("Permissions"), "section missing:\n{text}");
        // The exit criterion: requested capabilities render verbatim.
        assert!(
            text.contains("filesystem_read: $REPOSITORY"),
            "verbatim fs permission missing:\n{text}"
        );
        assert!(
            text.contains("command: cargo"),
            "verbatim command permission missing:\n{text}"
        );

        state.skills[0]
            .permissions
            .extend((0..20).map(|n| format!("mcp: server-{n}/tool-{n}")));
        let compact = render_to_string(&state, 80, 24);
        assert!(
            compact.contains("↑/↓ skill · M memory · Esc close"),
            "skill controls must remain pinned below long permissions:\n{compact}"
        );
        let hits = state.hit_map.borrow();
        for action in [Action::OpenMemory, Action::Dismiss] {
            assert!(
                hits.iter()
                    .any(|(rect, registered)| registered == &action && rect.width > 0),
                "{action:?} needs a non-empty mouse hit target"
            );
        }
    }

    #[test]
    fn memory_browser_snapshot_shows_the_provenance_card() {
        let mut state = running_build_state();
        state.memories = vec![MemoryCard {
            statement: "This repository requires Rust nightly".to_owned(),
            class: "semantic".to_owned(),
            scope: "repository".to_owned(),
            revision: "79acbf1".to_owned(),
            observed: "2026-07-14".to_owned(),
            confidence: 1.0,
            source: "artifact 3f2a (rust-toolchain.toml)".to_owned(),
        }];
        reduce(&mut state, Action::OpenMemory);
        let text = render_to_string(&state, 120, 40);

        assert!(
            text.contains("Provenance card"),
            "card title missing:\n{text}"
        );
        assert!(
            text.contains("This repository requires Rust nightly"),
            "fact missing:\n{text}"
        );
        // Every retrieved memory opens its source: the source is on the card.
        assert!(
            text.contains("rust-toolchain.toml"),
            "source missing:\n{text}"
        );
        assert!(text.contains("79acbf1"), "revision missing:\n{text}");
        assert!(text.contains("Confidence"), "confidence missing:\n{text}");
        // Before opening, the affordance is offered.
        assert!(text.contains("open source"), "affordance missing:\n{text}");

        state.memories[0].source = "very/long/provenance/source/".repeat(30);
        let compact = render_to_string(&state, 80, 24);
        assert!(
            compact.contains("↑/↓ memory · o source"),
            "memory source control must remain pinned below long provenance:\n{compact}"
        );
        assert!(
            compact.contains("S skills · Esc close"),
            "memory navigation must remain pinned below long provenance:\n{compact}"
        );
        let hits = state.hit_map.borrow();
        for action in [Action::OpenSource, Action::OpenSkills, Action::Dismiss] {
            assert!(
                hits.iter()
                    .any(|(rect, registered)| registered == &action && rect.width > 0),
                "{action:?} needs a non-empty mouse hit target"
            );
        }
    }

    #[test]
    fn memory_browser_open_source_reveals_the_full_ref() {
        let mut state = running_build_state();
        state.memories = vec![MemoryCard {
            statement: "tests use cargo nextest".to_owned(),
            class: "procedural".to_owned(),
            scope: "repository".to_owned(),
            revision: "abc1234".to_owned(),
            observed: "2026-07-15".to_owned(),
            confidence: 0.9,
            source: "events 3..7 of session 51ee".to_owned(),
        }];
        reduce(&mut state, Action::OpenMemory);
        reduce(&mut state, Action::OpenSource);
        let text = render_to_string(&state, 120, 40);

        assert!(
            text.contains("source opened"),
            "opened marker missing:\n{text}"
        );
        assert!(
            text.contains("events 3..7 of session 51ee"),
            "revealed source missing:\n{text}"
        );
    }

    #[test]
    fn docs_studio_snapshot_shows_tree_editor_and_review_rails() {
        use crate::state::{DocBlockView, DocCard, DocSuggestionView};
        let mut state = running_build_state();
        state.docs = vec![DocCard {
            document_id: codypendent_protocol::DocumentId::new(),
            title: "Payments runbook".to_owned(),
            scope: "organization".to_owned(),
            status: "draft".to_owned(),
            mode: "suggest".to_owned(),
            revision: "r7".to_owned(),
            blocks: vec![
                DocBlockView {
                    id: "b1".to_owned(),
                    kind: "heading".to_owned(),
                    text: "Charging a customer".to_owned(),
                },
                DocBlockView {
                    id: "b2".to_owned(),
                    kind: "paragraph".to_owned(),
                    text: "Call charge_customer with an idempotency key.".to_owned(),
                },
            ],
            suggestions: vec![DocSuggestionView {
                id: "s1".to_owned(),
                status: "pending".to_owned(),
                author: "agent".to_owned(),
                range: "0..8".to_owned(),
                replacement: "Charging a customer safely".to_owned(),
                rationale: Some("match the code path".to_owned()),
            }],
        }];
        reduce(&mut state, Action::OpenDocs);
        let text = render_to_string(&state, 120, 40);

        assert!(text.contains("Docs Studio"), "title missing:\n{text}");
        // Tree rail: the document title + its scope/status/mode.
        assert!(
            text.contains("Payments runbook"),
            "tree title missing:\n{text}"
        );
        assert!(text.contains("organization"), "tree scope missing:\n{text}");
        // Editor rail: block kinds and the revision badge.
        assert!(text.contains("Editor rail"), "editor rail missing:\n{text}");
        assert!(text.contains("heading"), "block kind missing:\n{text}");
        assert!(text.contains("r7"), "revision badge missing:\n{text}");
        // Review rail: the pending suggestion with its author and rationale.
        assert!(text.contains("Review rail"), "review rail missing:\n{text}");
        assert!(text.contains("agent"), "suggestion author missing:\n{text}");
        assert!(
            text.contains("match the code path"),
            "suggestion rationale missing:\n{text}"
        );

        for index in 2..20 {
            state.docs[0].blocks.push(DocBlockView {
                id: format!("b{index}"),
                kind: "paragraph".to_owned(),
                text: format!("block-{index}"),
            });
        }
        state.doc_focus = DocFocus::Editor;
        state.selected_block = 19;
        let compact_editor = render_to_string(&state, 80, 24);
        assert!(
            compact_editor.contains("block-19"),
            "the selected block must scroll into view:\n{compact_editor}"
        );
        assert!(
            compact_editor.contains("P publish"),
            "Docs controls must remain pinned:\n{compact_editor}"
        );

        state.docs[0].suggestions = (0..12)
            .map(|index| DocSuggestionView {
                id: format!("s{index}"),
                status: "pending".to_owned(),
                author: format!("reviewer-{index}"),
                range: "0..1".to_owned(),
                replacement: "replacement".to_owned(),
                rationale: None,
            })
            .collect();
        state.doc_focus = DocFocus::Review;
        state.selected_suggestion = 11;
        let compact_review = render_to_string(&state, 80, 24);
        assert!(
            compact_review.contains("reviewer-11"),
            "the selected suggestion must scroll into view:\n{compact_review}"
        );
        let hits = state.hit_map.borrow();
        for action in [
            Action::CyclePane,
            Action::EditDoc,
            Action::Approve(codypendent_protocol::ApprovalScope::Once),
            Action::Reject,
            Action::PublishDoc,
        ] {
            assert!(
                hits.iter().any(|(_, registered)| registered == &action),
                "{action:?} needs a Docs mouse target"
            );
        }
    }

    #[test]
    fn command_palette_aligns_columns_groups_and_drops_the_dash() {
        let mut state = running_build_state();
        reduce(&mut state, Action::OpenPalette);
        let all = render_to_string(&state, 120, 40);
        assert!(all.contains("Command palette"), "title:\n{all}");
        assert!(all.contains("New run"), "command:\n{all}");
        assert!(all.contains("Model picker"), "command:\n{all}");
        // Group labels appear on the empty query.
        assert!(
            all.contains("Run") && all.contains("Models"),
            "group labels:\n{all}"
        );
        // The confusing unbound-key marker is gone.
        assert!(!all.contains("[—]"), "no [—] marker:\n{all}");
        // The header hint invites clicking.
        assert!(all.contains("click a row"), "click hint:\n{all}");

        // Filtering hides the group labels.
        for c in "docs".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        let filtered = render_to_string(&state, 120, 40);
        assert!(filtered.contains("Docs Studio"), "match:\n{filtered}");
        assert!(
            !filtered.contains("New run"),
            "non-match filtered:\n{filtered}"
        );
    }

    #[test]
    fn command_palette_snapshot_lists_and_filters_commands() {
        let mut state = running_build_state();
        reduce(&mut state, Action::OpenPalette);
        let all = render_to_string(&state, 120, 40);
        assert!(all.contains("Command palette"), "title missing:\n{all}");
        // Unfiltered, it lists commands with their key hints.
        assert!(all.contains("New run"), "command missing:\n{all}");
        assert!(all.contains("Docs Studio"), "command missing:\n{all}");
        assert!(!all.contains("[—]"), "no dash marker:\n{all}");

        // Typing filters the list down.
        for c in "docs".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        let filtered = render_to_string(&state, 120, 40);
        assert!(
            filtered.contains("Docs Studio"),
            "match missing:\n{filtered}"
        );
        assert!(
            !filtered.contains("New run"),
            "non-match should be filtered out:\n{filtered}"
        );
    }

    #[test]
    fn model_picker_snapshot_shows_rows_current_marker_and_badges() {
        let mut state = running_build_state();
        // `running_build_state` serves the run from "gpt-5.1-codex" (its
        // ModelStreamDelta actor) — that row must render marked current.
        state.models = vec![
            ModelCard {
                id: ModelId("gpt-5.1-codex".to_owned()),
                provider: "openai-compatible".to_owned(),
                readiness: ModelReadiness::Unverified,
                location: Some(ModelLocationLabel::Hosted),
                cost_per_1k_usd: Some(0.03),
                context_tokens: Some(200_000),
            },
            ModelCard {
                id: ModelId("qwen2.5-coder".to_owned()),
                provider: "openai-compatible".to_owned(),
                readiness: ModelReadiness::Ready,
                location: Some(ModelLocationLabel::Local),
                cost_per_1k_usd: None,
                context_tokens: Some(32_000),
            },
        ];
        reduce(&mut state, Action::OpenPalette);
        for c in "model".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        // Focus the SECOND row (qwen) — deliberately NOT the current model
        // (gpt) — so the current-marker assertions below can only be
        // satisfied by the list rows themselves, never by the (qwen-focused)
        // detail panel.
        reduce(&mut state, Action::SelectNext);
        assert!(matches!(state.overlay, Overlay::ModelPicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(text.contains("Model picker"), "title missing:\n{text}");
        assert!(text.contains("gpt-5.1-codex"), "first row missing:\n{text}");
        assert!(
            text.contains("qwen2.5-coder"),
            "second row missing:\n{text}"
        );

        // Row-scoped: the list's per-row current marker is the span
        // immediately BEFORE the id ("● " then the id, contiguous — see the
        // list-row `head` `Line`), distinct from the detail panel's "<id>  ●
        // current" (marker AFTER the id) when the FOCUSED model happens to be
        // current. Checking this precise adjacency — rather than whether a
        // whole terminal LINE contains '●' — matters because ratatui lays
        // the list and detail panel out as side-by-side columns sharing the
        // same rows, so an unscoped whole-line check would also pass with the
        // marker misapplied to the wrong row (or every row): here gpt is
        // current and qwen is merely focused (by the `SelectNext` above), so
        // only gpt's list row may show the leading marker.
        assert!(
            text.contains("● ? gpt-5.1-codex"),
            "the list's current marker is missing from gpt-5.1-codex's row:\n{text}"
        );
        assert!(
            !text.contains("● ✓ qwen2.5-coder"),
            "the list must not mark the non-current model's row current:\n{text}"
        );

        // Local/hosted + cost + context badges.
        assert!(text.contains("hosted"), "hosted badge missing:\n{text}");
        assert!(
            text.contains("local \u{2713}"),
            "local badge missing:\n{text}"
        );
        assert!(text.contains("$0.03/1k"), "cost badge missing:\n{text}");
        assert!(text.contains("200k"), "context badge missing:\n{text}");
        // The unprofiled model's badges are omitted gracefully, never crash.
        assert!(
            text.contains("32k"),
            "the profiled model's context missing:\n{text}"
        );
    }

    #[test]
    fn provider_picker_snapshot_shows_rows_and_badges() {
        let mut state = running_build_state();
        state.providers = vec![
            ProviderCard {
                id: "groq".to_owned(),
                name: "Groq".to_owned(),
                protocol: "openai-chat".to_owned(),
                auth: "api-key: GROQ_API_KEY".to_owned(),
                local: false,
                requires_key: true,
                can_list_models: true,
                available: true,
                catalog_models: 12,
                has_key: false,
            },
            ProviderCard {
                id: "ollama".to_owned(),
                name: "Ollama (local)".to_owned(),
                protocol: "openai-chat".to_owned(),
                auth: "none".to_owned(),
                local: true,
                requires_key: false,
                can_list_models: true,
                available: true,
                catalog_models: 0,
                has_key: false,
            },
        ];
        reduce(&mut state, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        reduce(&mut state, Action::SelectNext);
        assert!(matches!(state.overlay, Overlay::ProviderPicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(text.contains("Provider catalog"), "title missing:\n{text}");
        assert!(text.contains("groq"), "first row missing:\n{text}");
        assert!(text.contains("ollama"), "second row missing:\n{text}");
        assert!(text.contains("Groq"), "first row's name missing:\n{text}");
        assert!(
            text.contains("Ollama (local)"),
            "second row's name missing:\n{text}"
        );
        assert!(text.contains("openai-chat"), "protocol missing:\n{text}");
        assert!(
            text.contains("api-key: GROQ_API_KEY"),
            "auth badge missing:\n{text}"
        );
        assert!(text.contains("none"), "auth badge missing:\n{text}");
        assert!(text.contains("hosted"), "hosted badge missing:\n{text}");
        assert!(
            text.contains("local \u{2713}"),
            "local badge missing:\n{text}"
        );
        // Staging is gone: no staged marker should render.
        assert!(
            !text.contains("● staged"),
            "the dead staged marker must not render:\n{text}"
        );
    }

    #[test]
    fn provider_picker_hint_says_add_model_not_stage() {
        let mut state = running_build_state();
        state.providers = vec![ProviderCard {
            id: "groq".to_owned(),
            name: "Groq".to_owned(),
            protocol: "openai-chat".to_owned(),
            auth: "api-key: GROQ_API_KEY".to_owned(),
            local: false,
            requires_key: true,
            can_list_models: true,
            available: true,
            catalog_models: 0,
            has_key: false,
        }];
        reduce(&mut state, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        assert!(matches!(state.overlay, Overlay::ProviderPicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(
            text.contains("add model") || text.contains("browse this provider's models"),
            "the hint must describe adding a model, not staging:\n{text}"
        );
        assert!(
            !text.contains("stage"),
            "the dead 'stage' copy must be gone:\n{text}"
        );
    }

    #[test]
    fn mode_picker_snapshot_lists_the_modes_and_marks_the_current_default() {
        // PR C2 (plan mode): the picker lists every submission mode and marks
        // the current `default_mode` (Build, out of the box).
        let mut state = running_build_state();
        reduce(&mut state, Action::OpenPalette);
        for c in "mode picker".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        assert!(matches!(state.overlay, Overlay::ModePicker { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(text.contains("Mode picker"), "title missing:\n{text}");
        for label in ["Ask", "Explore", "Plan", "Build", "Review"] {
            assert!(text.contains(label), "the {label} row is missing:\n{text}");
        }
        assert!(
            text.contains("● Build"),
            "the current default is marked:\n{text}"
        );
        assert!(
            !text.contains("● Plan"),
            "a non-current mode must not be marked:\n{text}"
        );
        assert!(
            text.contains("investigate read-only, then finish"),
            "the Plan row's summary is missing:\n{text}"
        );
    }

    #[test]
    fn command_palette_keeps_the_last_command_visible_at_80x24() {
        let mut state = running_build_state();
        state.overlay = Overlay::Palette {
            query: String::new(),
            selected: crate::palette::COMMANDS.len() - 1,
        };

        let text = render_to_string(&state, 80, 24);
        assert!(
            text.contains("New conversation"),
            "the selected final row must scroll into view:\n{text}"
        );
        assert!(
            text.contains("▎ New conversation"),
            "the visible final row must remain selected:\n{text}"
        );
    }

    #[test]
    fn mode_picker_fits_every_mode_at_80x24() {
        let mut state = running_build_state();
        state.overlay = Overlay::ModePicker {
            query: String::new(),
            selected: 4,
        };

        let text = render_to_string(&state, 80, 24);
        for label in ["Ask", "Explore", "Plan", "Build", "Review"] {
            assert!(text.contains(label), "the {label} row is clipped:\n{text}");
        }
        assert!(
            text.contains("▎   Review"),
            "Review stays selected:\n{text}"
        );
    }

    #[test]
    fn issues_overlay_remains_actionable_at_80x24() {
        let mut state = running_build_state();
        state.issues = vec!["models.toml has no usable model".to_owned()];
        state.overlay = Overlay::Issues;

        let text = render_to_string(&state, 80, 24);
        assert!(
            text.contains("Setup & diagnostics"),
            "title clipped:\n{text}"
        );
        assert!(text.contains("models.toml"), "issue clipped:\n{text}");
        assert!(
            text.contains("Provider catalog"),
            "recovery guidance clipped:\n{text}"
        );
        assert!(
            text.contains("Delete clear resolved diagnostics"),
            "the close/clear affordance is clipped:\n{text}"
        );
    }

    /// D1: a `/keys` test fixture — two models with one of each status, plus
    /// the Tavily row. The "known test key" below must NEVER appear in any
    /// render.
    fn api_keys_state() -> AppState {
        let mut state = running_build_state();
        state.models = vec![
            ModelCard {
                id: ModelId("groq/llama".to_owned()),
                provider: "openai-compatible".to_owned(),
                readiness: ModelReadiness::Ready,
                location: None,
                cost_per_1k_usd: None,
                context_tokens: None,
            },
            ModelCard {
                id: ModelId("openai/gpt".to_owned()),
                provider: "openai-compatible".to_owned(),
                readiness: ModelReadiness::Ready,
                location: None,
                cost_per_1k_usd: None,
                context_tokens: None,
            },
        ];
        state.key_status = vec![
            ("groq/llama".to_owned(), crate::state::KeyStatus::Stored),
            (
                "openai/gpt".to_owned(),
                crate::state::KeyStatus::Env("OPENAI_API_KEY".to_owned()),
            ),
        ];
        state.tavily_key_status = crate::state::KeyStatus::Missing;
        state
    }

    #[test]
    fn api_keys_overlay_lists_rows_with_status_glyphs_and_no_key_material() {
        let mut state = api_keys_state();
        reduce(&mut state, Action::OpenPalette);
        for c in "api keys".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        assert!(matches!(state.overlay, Overlay::ApiKeys { .. }));

        let text = render_to_string(&state, 120, 40);
        assert!(text.contains("API keys"), "title missing:\n{text}");
        assert!(text.contains("groq/llama"), "a model row:\n{text}");
        assert!(text.contains("openai/gpt"), "the other model row:\n{text}");
        assert!(
            text.contains("Tavily (web.search)"),
            "the final Tavily row:\n{text}"
        );
        assert!(
            text.contains("● groq/llama"),
            "a stored key renders ●:\n{text}"
        );
        assert!(
            text.contains("◐ openai/gpt"),
            "an env-declared key renders ◐:\n{text}"
        );
        assert!(
            text.contains("env OPENAI_API_KEY"),
            "the var NAME (never its value) shows in the detail line:\n{text}"
        );
        assert!(
            text.contains("○ Tavily (web.search)"),
            "a missing key renders ○:\n{text}"
        );
        // No key material anywhere — the env var's VALUE would leak here if
        // statuses ever carried one (they cannot, by construction).
        assert!(
            !text.contains("sk-live-test-key"),
            "key material must never render:\n{text}"
        );
        assert!(
            !text.contains("tvly-"),
            "no Tavily key material either:\n{text}"
        );
    }

    #[test]
    fn api_key_set_prompt_masks_the_typed_key() {
        let mut state = api_keys_state();
        state.overlay = Overlay::ApiKeySet {
            target: crate::action::KeyTarget::Model("groq/llama".to_owned()),
            buffer: crate::action::SecretKey("sk-live-test-key".to_owned()),
        };
        let text = render_to_string(&state, 100, 24);
        assert!(
            text.contains("API key for groq/llama"),
            "the prompt names its (non-secret) target:\n{text}"
        );
        assert!(text.contains('•'), "the key is masked:\n{text}");
        assert!(
            !text.contains("sk-live-test-key"),
            "the raw key must never render:\n{text}"
        );

        // The Tavily target gets its own title — still masked.
        state.overlay = Overlay::ApiKeySet {
            target: crate::action::KeyTarget::Tavily,
            buffer: crate::action::SecretKey("tvly-test-secret".to_owned()),
        };
        let text = render_to_string(&state, 100, 24);
        assert!(text.contains("Tavily API key"), "the Tavily title:\n{text}");
        assert!(
            !text.contains("tvly-test-secret"),
            "the raw key must never render:\n{text}"
        );
    }

    #[test]
    fn api_key_remove_confirm_names_the_target_without_key_material() {
        let mut state = api_keys_state();
        state.overlay = Overlay::ApiKeyRemoveConfirm {
            target: crate::action::KeyTarget::Model("groq/llama".to_owned()),
        };
        let text = render_to_string(&state, 80, 24);
        assert!(
            text.contains("Remove the saved key for groq/llama?"),
            "the confirm names its target:\n{text}"
        );
        assert!(text.contains("[y] yes"), "the y/n affordance:\n{text}");
        assert!(
            !text.contains("sk-live-test-key"),
            "no key material in the confirm:\n{text}"
        );
    }

    #[test]
    fn the_project_header_shows_the_next_runs_submission_mode() {
        // The picked default is visible without opening the mode picker.
        let mut state = running_build_state();
        let before = header_line(&state, 120);
        assert!(
            before.contains("Build"),
            "the default submission mode shows:\n{before}"
        );

        state.default_mode = AgentMode::Plan;
        let after = header_line(&state, 120);
        assert!(
            after.contains("Plan"),
            "a picked mode shows immediately:\n{after}"
        );
    }

    #[test]
    fn masked_key_prompt_hides_the_typed_key() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelKey {
            provider_id: "groq".to_owned(),
            model: "llama-3.1-8b".to_owned(),
            buffer: crate::action::SecretKey("sk-secret".to_owned()),
        };
        let text = render_to_string(&state, 80, 24);
        assert!(text.contains("API key"), "the key prompt title:\n{text}");
        assert!(
            text.contains('•'),
            "the key is masked with bullets:\n{text}"
        );
        assert!(
            !text.contains("sk-secret"),
            "the raw key must never render:\n{text}"
        );
    }

    #[test]
    fn add_model_provider_key_prompt_masks_the_key() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelProviderKey {
            provider_id: "groq".to_owned(),
            buffer: crate::action::SecretKey("sk-secret".to_owned()),
        };
        let text = render_to_string(&state, 100, 24);
        assert!(
            text.contains("API key for groq"),
            "the key prompt title:\n{text}"
        );
        assert!(
            text.contains('•'),
            "the key is masked with bullets:\n{text}"
        );
        assert!(
            !text.contains("sk-secret"),
            "the raw key must never render:\n{text}"
        );
    }

    #[test]
    fn add_model_querying_box_names_the_provider() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelQuerying {
            provider_id: "groq".to_owned(),
            api_key: Some(crate::action::SecretKey("sk-secret".to_owned())),
        };
        let text = render_to_string(&state, 80, 24);
        assert!(
            text.contains("Fetching models from groq"),
            "the querying box names the provider:\n{text}"
        );
        assert!(text.contains("Esc to cancel"), "the cancel hint:\n{text}");
        assert!(
            !text.contains("sk-secret"),
            "the key must never render:\n{text}"
        );
    }

    #[test]
    fn add_model_pick_lists_and_filters_names() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelPick {
            provider_id: "groq".to_owned(),
            api_key: None,
            models: vec![
                AddModelRow {
                    id: "llama-3.1-8b".to_owned(),
                    name: Some("Llama 3.1 8B".to_owned()),
                    context_tokens: Some(128_000),
                    cost_per_1m_input_usd: Some(0.05),
                    cost_per_1m_output_usd: Some(0.08),
                    live: true,
                },
                AddModelRow::live("gpt-oss-20b"),
            ],
            query: "llama".to_owned(),
            selected: 0,
            origin: ModelListOrigin::Live,
            refreshing: false,
        };
        let text = render_to_string(&state, 100, 30);
        assert!(
            text.contains("Add model · groq"),
            "the pick-list title:\n{text}"
        );
        assert!(
            text.contains("llama-3.1-8b"),
            "the matching model lists:\n{text}"
        );
        assert!(
            !text.contains("gpt-oss-20b"),
            "a non-matching model is filtered out:\n{text}"
        );
        // The catalog metadata columns are what make the pick-list a decision
        // surface rather than a list of opaque ids.
        assert!(
            text.contains("Llama 3.1 8B"),
            "the display name shows:\n{text}"
        );
        assert!(text.contains("ctx 128k"), "the context column:\n{text}");
        assert!(
            text.contains("in $0.05") && text.contains("out $0.08"),
            "the per-1M price columns:\n{text}"
        );
        assert!(
            text.contains("live list"),
            "the header states where the rows came from:\n{text}"
        );
    }

    /// A provider with no listing endpoint but curated catalog rows must say so
    /// on its card — the operator needs to know the flow will still work.
    #[test]
    fn provider_card_badges_a_catalog_only_provider() {
        let mut state = running_build_state();
        state.providers = vec![ProviderCard {
            id: "perplexity".to_owned(),
            name: "Perplexity".to_owned(),
            protocol: "openai-chat".to_owned(),
            auth: "api-key: PERPLEXITY_API_KEY".to_owned(),
            local: false,
            requires_key: true,
            can_list_models: false,
            available: true,
            catalog_models: 7,
            has_key: false,
        }];
        reduce(&mut state, Action::OpenPalette);
        for c in "provider".chars() {
            reduce(&mut state, Action::InputChar(c));
        }
        reduce(&mut state, Action::InputSubmit);
        let text = render_to_string(&state, 120, 30);
        assert!(
            text.contains("catalog 7 models"),
            "a provider with no live listing advertises its curated rows:\n{text}"
        );
    }

    #[test]
    fn edge_inspector_snapshot_shows_evidence_and_revision() {
        use crate::state::GraphEdgeCard;
        let mut state = running_build_state();
        state.edges = vec![GraphEdgeCard {
            from: "billing::charge".to_owned(),
            to: "gateway::submit".to_owned(),
            relation: "calls".to_owned(),
            confidence: 0.45,
            evidence_kind: "syntax_inferred".to_owned(),
            evidence: "artifact 3f2a (src/billing.rs)".to_owned(),
            revision: "79acbf1".to_owned(),
        }];
        state.edge_total = 1;
        reduce(&mut state, Action::OpenEdges);
        let text = render_to_string(&state, 120, 40);

        assert!(text.contains("Code graph"), "title missing:\n{text}");
        assert!(
            text.contains("billing::charge"),
            "from symbol missing:\n{text}"
        );
        assert!(
            text.contains("gateway::submit"),
            "to symbol missing:\n{text}"
        );
        assert!(text.contains("calls"), "relation missing:\n{text}");
        // The exit-criterion payload: evidence kind + source + revision on show.
        assert!(
            text.contains("Evidence"),
            "evidence section missing:\n{text}"
        );
        assert!(
            text.contains("syntax_inferred"),
            "evidence kind missing:\n{text}"
        );
        assert!(
            text.contains("src/billing.rs"),
            "evidence source missing:\n{text}"
        );
        assert!(text.contains("79acbf1"), "revision missing:\n{text}");

        let compact = render_to_string(&state, 80, 24);
        assert!(
            compact.contains("↑/↓ edge · / search"),
            "graph search must stay pinned at 80x24:\n{compact}"
        );
        assert!(
            compact.contains("PgUp prev · PgDn next · Esc close"),
            "graph paging must stay pinned at 80x24:\n{compact}"
        );
        let hits = state.hit_map.borrow();
        for action in [
            Action::OpenPalette,
            Action::ScrollPageUp,
            Action::ScrollPageDown,
            Action::Dismiss,
        ] {
            assert!(
                hits.iter()
                    .any(|(rect, registered)| registered == &action && rect.width > 0),
                "{action:?} needs a non-empty mouse hit target"
            );
        }
    }

    #[test]
    fn empty_code_graph_is_a_compact_actionable_state() {
        let mut state = running_build_state();
        reduce(&mut state, Action::OpenEdges);

        let loading = render_to_string(&state, 160, 50);
        assert!(
            loading.contains("Loading code graph…"),
            "the asynchronous request needs an honest loading state:\n{loading}"
        );
        assert!(!loading.contains("No relationships indexed yet"));

        reduce(
            &mut state,
            Action::EdgesLoaded {
                edges: Vec::new(),
                total: 0,
                query: String::new(),
                page: 0,
            },
        );

        let text = render_to_string(&state, 160, 50);
        assert!(
            text.contains("No relationships indexed yet"),
            "purposeful empty-state title:\n{text}"
        );
        assert!(
            text.contains("Edges appear here as Codypendent gathers evidence"),
            "empty-state explanation:\n{text}"
        );
        assert!(
            text.contains("/ search  ·  Esc close"),
            "empty-state controls:\n{text}"
        );
        assert!(
            !text.contains("no edges in this repository"),
            "the old debug-style placeholder must be gone:\n{text}"
        );
        assert_eq!(
            centered_modal(Rect::new(0, 0, 160, 50), 78, 15),
            Rect::new(41, 17, 78, 15),
            "the empty state stays a focused card on wide terminals"
        );
        let hits = state.hit_map.borrow();
        for action in [Action::OpenPalette, Action::Dismiss] {
            assert!(
                hits.iter()
                    .any(|(rect, registered)| registered == &action && rect.width > 0),
                "{action:?} needs a non-empty mouse hit target"
            );
        }
    }

    #[test]
    fn workflow_view_snapshot_shows_node_state_agent_and_worktree() {
        use crate::state::WorkflowNodeCard;
        let mut state = running_build_state();
        state.workflow = vec![
            WorkflowNodeCard {
                workflow_id: "repair-github-check".to_owned(),
                workflow: "repair-github-check v1".to_owned(),
                workflow_run_id: Some("workflow-run-1".to_owned()),
                run_phase: "running".to_owned(),
                inputs: "pull_request:github_pull_request*".to_owned(),
                id: "patch".to_owned(),
                action: "agent implementer \u{b7} skill code.repair".to_owned(),
                kind: "agent".to_owned(),
                state: "pending".to_owned(),
                agent: "implementer".to_owned(),
                model_policy: "coding".to_owned(),
                workspace: "isolated worktree".to_owned(),
                approval: "before write".to_owned(),
                retry: "1 attempt".to_owned(),
                depends_on: "\u{2014}".to_owned(),
                outputs: "proposed_patch".to_owned(),
                cost: "\u{2014}".to_owned(),
                error: "\u{2014}".to_owned(),
            },
            WorkflowNodeCard {
                workflow_id: "repair-github-check".to_owned(),
                workflow: "repair-github-check v1".to_owned(),
                workflow_run_id: Some("workflow-run-1".to_owned()),
                run_phase: "running".to_owned(),
                inputs: "pull_request:github_pull_request*".to_owned(),
                id: "verify".to_owned(),
                action: "tool repository.test".to_owned(),
                kind: "tool".to_owned(),
                state: "pending".to_owned(),
                agent: "\u{2014}".to_owned(),
                model_policy: "\u{2014}".to_owned(),
                workspace: "shared worktree".to_owned(),
                approval: "none".to_owned(),
                retry: "2 attempts \u{b7} 5s backoff".to_owned(),
                depends_on: "patch".to_owned(),
                outputs: "test_result".to_owned(),
                cost: "\u{2014}".to_owned(),
                error: "\u{2014}".to_owned(),
            },
        ];
        reduce(&mut state, Action::OpenWorkflow);
        let text = render_to_string(&state, 120, 40);

        assert!(text.contains("Workflow"), "title missing:\n{text}");
        // The workflow group header and the node ids in the list.
        assert!(
            text.contains("repair-github-check v1"),
            "group header missing:\n{text}"
        );
        assert!(text.contains("patch"), "node id missing:\n{text}");
        // The focused (first) node's detail — the exit-criterion payload: state,
        // agent, worktree, approval, and declared outputs.
        assert!(text.contains("pending"), "state missing:\n{text}");
        assert!(text.contains("implementer"), "agent missing:\n{text}");
        assert!(
            text.contains("isolated worktree"),
            "worktree missing:\n{text}"
        );
        assert!(text.contains("before write"), "approval missing:\n{text}");
        assert!(text.contains("proposed_patch"), "outputs missing:\n{text}");

        let compact = render_to_string(&state, 80, 24);
        assert!(
            compact.contains("n start · p pause/resume"),
            "workflow controls must stay pinned at 80x24:\n{compact}"
        );
        assert!(
            compact.contains("c cancel · ↑/↓ node · Esc close"),
            "workflow cancel and navigation must stay visible at 80x24:\n{compact}"
        );
        let hits = state.hit_map.borrow();
        for action in [
            Action::NewRun,
            Action::Pause,
            Action::Reject,
            Action::Cancel,
        ] {
            assert!(
                hits.iter().any(|(_, registered)| registered == &action),
                "{action:?} needs a mouse hit target"
            );
        }
        assert!(
            hits.iter()
                .filter(|(_, action)| action == &Action::Cancel)
                .all(|(rect, _)| rect.width > 0),
            "cancel must never register a zero-width hit target"
        );
    }

    #[test]
    fn blackboard_view_snapshot_shows_artifact_provenance() {
        use crate::state::BlackboardItemCard;
        let mut state = running_build_state();
        state.blackboard = vec![BlackboardItemCard {
            id: "item-1".to_owned(),
            workflow_run_id: "workflow-run-1".to_owned(),
            run: "repair-github-check \u{b7} run 0f2a".to_owned(),
            kind: "finding".to_owned(),
            summary: "the failing test asserts an off-by-one in paginate()".to_owned(),
            author: "agent investigator".to_owned(),
            confidence: "0.85".to_owned(),
            evidence: "2 ref(s)".to_owned(),
            revision: "r1".to_owned(),
            superseded: false,
        }];
        reduce(&mut state, Action::OpenBlackboard);
        let text = render_to_string(&state, 120, 40);

        assert!(text.contains("Blackboard"), "title missing:\n{text}");
        // Run group header + the artifact kind.
        assert!(
            text.contains("repair-github-check"),
            "run header missing:\n{text}"
        );
        assert!(text.contains("finding"), "kind missing:\n{text}");
        // The provenance payload the exit criterion wants visible.
        assert!(
            text.contains("agent investigator"),
            "author missing:\n{text}"
        );
        assert!(text.contains("0.85"), "confidence missing:\n{text}");
        assert!(text.contains("2 ref(s)"), "evidence missing:\n{text}");
        assert!(
            text.contains("off-by-one"),
            "payload summary missing:\n{text}"
        );

        state.blackboard[0].summary = "long payload evidence ".repeat(80);
        let compact = render_to_string(&state, 80, 24);
        assert!(
            compact.contains("↑/↓ item · Esc close · live"),
            "blackboard controls must remain pinned below long payloads:\n{compact}"
        );
        assert!(
            state
                .hit_map
                .borrow()
                .iter()
                .any(|(rect, action)| action == &Action::Dismiss && rect.width > 0),
            "blackboard close needs a non-empty mouse hit target"
        );
    }

    // --- Task 1: transcript virtualization + reading flow + scroll clamp ---

    #[test]
    fn a_short_transcript_starts_near_the_top() {
        // One brief turn in a tall viewport: the exchange remains visually tied
        // to the project header instead of floating just above the composer.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "tiny".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "one short reply".to_owned(),
            }),
        );
        let out = render_to_string(&s, 80, 24);
        let rows: Vec<&str> = out.lines().collect();
        // Keep a small breath of space below the header, then begin the turn.
        assert!(
            rows[2].trim_matches('│').trim().is_empty(),
            "small top inset:\n{out}"
        );
        let reply_row = rows
            .iter()
            .position(|r| r.contains("one short reply"))
            .expect("reply rendered");
        assert!(
            reply_row < 10,
            "content starts near the top (row {reply_row}):\n{out}"
        );
    }

    #[test]
    fn build_transcript_window_materializes_only_the_viewport() {
        // A pathological single Model entry of thousands of source lines. The build
        // pass must materialize O(viewport) lines, not O(history).
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "huge".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let mut big = String::new();
        for i in 0..5000 {
            big.push_str(&format!("line {i}\n"));
        }
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta { run_id, text: big }),
        );

        let theme = Theme::dark();
        let inner_width = 78;
        let height = 20;
        let total = transcript_rows(&s.runs, &theme, inner_width);
        assert!(total >= 5000, "measure sees the whole history: {total}");
        let (lines, _r0, _hits) = build_transcript_window(
            &s.runs,
            &theme,
            inner_width,
            total.saturating_sub(height),
            height,
            0,
        );
        assert!(
            lines.len() <= height as usize + 4,
            "the build materializes O(viewport) lines, not O(history): {}",
            lines.len()
        );
    }

    // --- Task 7: RowKind::Rich + style_for (finalized rich rows) ---

    /// Drive a run to a finalized rich Model; return the mutated state.
    fn finalized_model_state(markdown: &str) -> AppState {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "go".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: markdown.to_owned(),
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );
        s
    }

    #[test]
    fn finalized_model_renders_styled_heading() {
        let s = finalized_model_state("# Heading");
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 0, 40, 0);
        // A heading span is bold and coloured text.heading.
        let styled = lines.iter().flat_map(|l| l.spans.iter()).any(|sp| {
            sp.style.fg == Some(theme.text.heading)
                && sp.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(styled, "the finalized heading is not styled from the theme");
    }

    #[test]
    fn keyword_span_maps_to_syntax_keyword() {
        let s = finalized_model_state("```rust\nfn a() {}\n```");
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 0, 40, 0);
        let has_kw = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|sp| sp.style.fg == Some(theme.syntax.keyword));
        assert!(has_kw, "no span coloured syntax.keyword");
    }

    #[test]
    fn rich_message_build_materializes_only_the_viewport() {
        // A large FINALIZED rich message — the crash-path invariant with rich rows.
        let mut big = String::new();
        for i in 0..4000 {
            big.push_str(&format!("- item {i}\n"));
        }
        let s = finalized_model_state(&big);
        // It really is finalized (rendered Some), so the rich path is exercised.
        assert!(s.runs[0].transcript.iter().any(|e| matches!(
            e,
            TranscriptEntry::Model {
                rendered: Some(_),
                ..
            }
        )));

        let theme = Theme::dark();
        let (inner_width, height) = (78u16, 20u16);
        let total = transcript_rows(&s.runs, &theme, inner_width);
        assert!(
            total >= 4000,
            "measure sees the whole rich history: {total}"
        );
        let (lines, _r, _h) = build_transcript_window(
            &s.runs,
            &theme,
            inner_width,
            total.saturating_sub(height),
            height,
            0,
        );
        assert!(
            lines.len() <= height as usize + 4,
            "build materializes O(viewport), not O(history): {}",
            lines.len()
        );
    }

    // --- Task 8: user-message container (Row.bg) ---

    fn user_turn_state() -> AppState {
        let mut s = AppState::new();
        let run_id = RunId::new();
        // RunStarted pushes the objective as a `User` transcript entry.
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "my question".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        s
    }

    #[test]
    fn user_rows_carry_the_container_bg_and_fill_width() {
        let s = user_turn_state();
        let theme = Theme::dark();
        let inner_width = 40u16;
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, inner_width, 0, 40, 0);
        let user_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|sp| sp.content.contains("my question")))
            .expect("user body line present");
        assert_eq!(
            user_line.style.bg,
            Some(theme.surface.user),
            "no container bg"
        );
        assert_eq!(
            user_line.width(),
            inner_width as usize,
            "not padded to full width"
        );
    }

    #[test]
    fn ansi16_user_row_uses_an_accent_bar_not_a_bg() {
        let s = user_turn_state();
        let theme = Theme::ansi16(); // surface.user == surface.panel here
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 40, 0, 40, 0);
        let user_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|sp| sp.content.contains("my question")))
            .expect("user body line present");
        assert_eq!(user_line.spans[0].content, "▎", "no accent bar");
        assert_eq!(user_line.spans[0].style.fg, Some(theme.focus.active));
        assert_ne!(
            user_line.style.bg,
            Some(theme.surface.user),
            "should not bg-fill on ansi16"
        );
    }

    #[test]
    fn user_container_does_not_break_virtualization() {
        let mut s = user_turn_state();
        // Add a long agent reply so the window must virtualize.
        let run_id = s.runs[0].run_id;
        let mut big = String::new();
        for i in 0..3000 {
            big.push_str(&format!("line {i}\n"));
        }
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta { run_id, text: big }),
        );
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 100, 20, 0);
        assert!(
            lines.len() <= 24,
            "build still O(viewport): {}",
            lines.len()
        );
    }

    #[test]
    fn theme_change_re_renders_without_re_parsing() {
        let s = finalized_model_state("# H");
        crate::markdown::reset_parse_calls();
        let (dark, _r, _h) = build_transcript_window(&s.runs, &Theme::dark(), 78, 0, 40, 0);
        let (light, _r, _h) = build_transcript_window(&s.runs, &Theme::light(), 78, 0, 40, 0);
        assert_eq!(
            crate::markdown::parse_calls(),
            0,
            "build re-parsed — cache not used"
        );
        let dfg = dark
            .iter()
            .flat_map(|l| l.spans.iter())
            .find_map(|s| s.style.fg);
        let lfg = light
            .iter()
            .flat_map(|l| l.spans.iter())
            .find_map(|s| s.style.fg);
        assert_ne!(dfg, lfg, "theme change produced no colour change");
    }

    #[test]
    fn streaming_model_still_renders_plain() {
        // No RunStateChanged: the tail is still Streaming ⇒ plain path (rendered None).
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "go".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: "# still going".to_owned(),
            }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(
            out.contains("# still going"),
            "streaming text should render as-is (plain):\n{out}"
        );
    }

    #[test]
    fn a_tall_transcript_still_tails_the_latest_row() {
        // Overflow path unchanged: following pins to the tail.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "scrolling".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let mut big = String::new();
        for i in 0..200 {
            big.push_str(&format!("body line {i}\n"));
        }
        big.push_str("THE FINAL LINE");
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta { run_id, text: big }),
        );
        let out = render_to_string(&s, 80, 20);
        assert!(
            out.contains("THE FINAL LINE"),
            "the tail is visible while following:\n{out}"
        );
        assert!(
            !out.contains("body line 0"),
            "the head has scrolled off:\n{out}"
        );
    }

    // --- Task 2: turn / role renderer ---

    #[test]
    fn a_user_turn_renders_a_role_header_and_indented_body() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "add a test".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let out = render_to_string(&s, 80, 14);
        assert!(out.contains("You"), "user role header:\n{out}");
        assert!(
            out.contains("  add a test"),
            "indented body (two-space gutter):\n{out}"
        );
        assert!(
            !out.contains("› add a test"),
            "the old caret user line is gone:\n{out}"
        );
    }

    #[test]
    fn the_assistant_header_names_the_model_when_known() {
        let s = running_build_state(); // serves "gpt-5.1-codex"
        let out = render_to_string(&s, 110, 30);
        assert!(
            out.contains("⏺ codypendent · gpt-5.1-codex"),
            "model in the turn header:\n{out}"
        );
    }

    /// Turn spacing (transcript "too cramped" fix): mirroring the existing
    /// blank line before a follow-up `You` turn, a blank line now separates
    /// the user's message from the assistant's `⏺ codypendent` header too —
    /// so the reply doesn't jam straight onto the user's text. The header
    /// and its own body stay glued together (no blank in between); only the
    /// turn boundary gets the gap.
    #[test]
    fn a_blank_line_separates_the_user_turn_from_the_assistant_header() {
        let s = running_build_state(); // objective → User turn, then a Model reply
        let out = render_to_string(&s, 110, 30);
        let rows: Vec<&str> = out.lines().collect();
        let user_header = rows
            .iter()
            .position(|r| r.trim_matches('│').trim() == "You")
            .expect("user turn header");
        assert!(
            !rows[user_header + 1].trim_matches('│').trim().is_empty(),
            "the You header stays glued to its own body (no blank in between):\n{out}"
        );
        let assistant_header = rows
            .iter()
            .skip(user_header)
            .position(|r| r.contains("⏺ codypendent"))
            .map(|p| p + user_header)
            .expect("assistant header");
        assert!(
            rows[assistant_header - 1]
                .trim_matches('│')
                .trim()
                .is_empty(),
            "one blank row separates the user turn from the assistant header:\n{out}"
        );
        assert!(
            !rows[assistant_header - 2]
                .trim_matches('│')
                .trim()
                .is_empty(),
            "only one blank row, not double-spacing, before the assistant header:\n{out}"
        );
    }

    #[test]
    fn the_assistant_header_omits_the_model_when_unknown() {
        // A run with a tool cell but no agent-authored (model-bearing) event.
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "hi".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ToolStarted {
                run_id,
                tool: "shell.run".to_owned(),
                args_digest: "abc".to_owned(),
                label: None,
            }),
        );
        let out = render_to_string(&s, 90, 20);
        // Scope to the transcript's assistant header row — the D3 chat header
        // bar at the top also names "codypendent", so a whole-buffer negative
        // assertion no longer isolates the turn header.
        let assistant_header = out
            .lines()
            .find(|line| line.contains("⏺ codypendent"))
            .expect("an assistant header row");
        assert!(
            !assistant_header.contains('·'),
            "no ` · <model>` when unknown (honesty):\n{out}"
        );
    }

    /// Virtualization guard (Task 1's invariant, re-checked against Task 2's
    /// header/gap rows): a pathological single Model entry of thousands of
    /// source lines, under a known-model run (so a `You` header/body Row and a
    /// model-named assistant header Row both ride the same `for_each_row`
    /// walk as the huge Model entry). The build pass must still materialize
    /// O(viewport) lines at either end of the history — the new header
    /// styling must not reintroduce whole-transcript materialization.
    #[test]
    fn virtualization_still_holds_with_role_headers_and_turn_gaps() {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "huge".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        let mut big = String::new();
        for i in 0..5000 {
            big.push_str(&format!("line {i}\n"));
        }
        reduce(
            &mut s,
            Action::daemon_event(SessionEvent {
                sequence: 2,
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::Agent {
                    agent_id: codypendent_protocol::AgentId::new(),
                    run_id,
                    model: ModelId("gpt-5.1-codex".to_owned()),
                },
                body: EventBody::ModelStreamDelta { run_id, text: big },
            }),
        );

        let theme = Theme::dark();
        let inner_width = 78;
        let height = 20;
        let total = transcript_rows(&s.runs, &theme, inner_width);
        assert!(
            total >= 5000,
            "measure still sees the whole history: {total}"
        );

        // The viewport at the very TOP: the `You` header and the model-named
        // assistant header are among the first virtualized rows.
        let (top_lines, _r0, _hits) =
            build_transcript_window(&s.runs, &theme, inner_width, 0, height, 0);
        assert!(
            top_lines.len() <= height as usize + 4,
            "top-of-history build stays O(viewport), not O(history): {}",
            top_lines.len()
        );
        assert!(
            // Task 8: the `You` row now carries the container bg, padded to
            // `inner_width` — trim the trailing fill before the exact match.
            top_lines.iter().any(|l| l.to_string().trim_end() == "You"),
            "the user role header is one of the virtualized top rows"
        );
        assert!(
            top_lines
                .iter()
                .any(|l| l.to_string().contains("codypendent · gpt-5.1-codex")),
            "the model-named assistant header is one of the virtualized top rows"
        );

        // The viewport at the TAIL of the same pathological history: still
        // O(viewport), not O(history) — Task 2 must not undo Task 1's fix.
        let (tail_lines, _r0, _hits) = build_transcript_window(
            &s.runs,
            &theme,
            inner_width,
            total.saturating_sub(height),
            height,
            0,
        );
        assert!(
            tail_lines.len() <= height as usize + 4,
            "tail-of-history build stays O(viewport), not O(history): {}",
            tail_lines.len()
        );
    }

    // --- Task 8: register clickable surfaces + parity ---

    #[test]
    fn clicking_a_palette_row_registers_activate_row() {
        let mut state = running_build_state();
        reduce(&mut state, Action::OpenPalette);
        let _ = render_to_string(&state, 120, 40); // populates the hit map
        let map = state.hit_map.borrow();
        assert!(
            map.iter().any(|(_, a)| matches!(a, Action::ActivateRow(_))),
            "a palette row registered ActivateRow"
        );
        // A full-screen scrim closes the overlay on an outside click (registered first).
        assert!(
            map.iter().any(|(_, a)| matches!(a, Action::Dismiss)),
            "modal scrim"
        );
    }

    #[test]
    fn clicking_a_run_entry_registers_select_run() {
        let mut state = running_build_state();
        reduce(&mut state, Action::ToggleLayout); // workspace shows the runs pane
        let _ = render_to_string(&state, 120, 30);
        let map = state.hit_map.borrow();
        assert!(
            map.iter().any(|(_, a)| matches!(a, Action::SelectRun(0))),
            "run row → SelectRun"
        );
    }

    #[test]
    fn clicking_the_context_footer_opens_commands() {
        let state = running_build_state();
        let _ = render_to_string(&state, 120, 30);
        let map = state.hit_map.borrow();
        assert!(
            map.iter().any(|(_, a)| matches!(a, Action::OpenPalette)),
            "footer chip → its Action"
        );
    }

    // --- Task 9: hygiene — final end-to-end integration ---

    /// End-to-end: a finalized assistant message with headings, emphasis,
    /// inline code, a list, a fenced code block, and a block quote renders
    /// through the real `parse` -> cache -> `RowKind::Rich` path — the whole
    /// pipeline this feature built, not a single stage in isolation. While
    /// still streaming the raw markdown is visible (plain path); once
    /// finalized the markup is consumed and the styled prose remains.
    #[test]
    fn full_markdown_message_snaps_to_rich_end_to_end() {
        let md = "# Report\n\nSome **bold** and `code`.\n\n- one\n- two\n\n\
                  ```rust\nfn main() { let x = 1; }\n```\n\n> a quote";
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(
            &mut s,
            system_ev(EventBody::RunStarted {
                run_id,
                objective: "please report".to_owned(),
                mode: AgentMode::Build,
            }),
        );
        reduce(
            &mut s,
            system_ev(EventBody::ModelStreamDelta {
                run_id,
                text: md.to_owned(),
            }),
        );
        // While streaming: raw markdown is visible (plain path).
        assert!(render_to_string(&s, 80, 30).contains("# Report"));
        // Finalize.
        reduce(
            &mut s,
            system_ev(EventBody::RunStateChanged {
                run_id,
                state: RunState::Completed,
            }),
        );
        let out = render_to_string(&s, 80, 30);
        // The literal "# " heading marker is gone (rendered as a styled heading).
        assert!(out.contains("Report"));
        assert!(
            !out.contains("# Report"),
            "heading markup should be consumed:\n{out}"
        );
        // The user's own turn and the agent reply both rendered.
        assert!(out.contains("please report"));
    }

    /// Same end-to-end path, but with a large finalized rich message — the
    /// crash-path invariant (§Global Constraints: virtualization preserved)
    /// re-checked one more time against the full pipeline, not just the
    /// synthetic list built in `rich_message_build_materializes_only_the_viewport`.
    #[test]
    fn full_pipeline_stays_virtualization_bounded_when_finalized() {
        // Headings/bold/inline code up top, then a long plain list — 3000
        // short items keeps the whole message comfortably under
        // `RICH_MARKDOWN_MAX_BYTES` (64 KiB) so it actually finalizes into
        // the rich cache (a message over that cap stays on the plain path
        // by design), while still being long enough to force virtualization.
        let mut md = "# Huge Report\n\nSome **bold** and `inline` code up top.\n\n".to_owned();
        for i in 0..3000 {
            md.push_str(&format!("- item {i}\n"));
        }
        let s = finalized_model_state(&md);
        assert!(
            s.runs[0].transcript.iter().any(|e| matches!(
                e,
                TranscriptEntry::Model {
                    rendered: Some(_),
                    ..
                }
            )),
            "message did not finalize into the rich cache"
        );

        let theme = Theme::dark();
        let (inner_width, height) = (78u16, 20u16);
        let total = transcript_rows(&s.runs, &theme, inner_width);
        assert!(
            total >= 3000,
            "measure sees the whole rich history: {total}"
        );
        let (lines, _r, _h) = build_transcript_window(
            &s.runs,
            &theme,
            inner_width,
            total.saturating_sub(height),
            height,
            0,
        );
        assert!(
            lines.len() <= height as usize + 4,
            "full pipeline must still materialize O(viewport), not O(history): {}",
            lines.len()
        );
    }
}

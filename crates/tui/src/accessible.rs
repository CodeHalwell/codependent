//! Cooked-terminal accessibility projection and line-command mapping.
//!
//! This module deliberately contains no terminal I/O. The CLI's accessible
//! harness prints [`accessible_snapshot`] on an ordinary cooked stdout and feeds
//! complete input lines through [`map_accessible_input`]. Keeping both pieces
//! pure makes the no-alternate-screen path deterministic and script-testable.

use codypendent_protocol::{ApprovalScope, RunDisposition};

use crate::action::Action;
use crate::input::KEY_BINDINGS;
use crate::remote_ui::{project_accessibility, RemoteKey};
use crate::state::{AppState, InputMode, Overlay, RunActivity, TranscriptEntry};

/// Render the complete current application state as a stable, linear document.
/// UI chrome is ASCII-only; user/model/extension content retains Unicode but is
/// stripped of terminal and bidi controls before it reaches cooked stdout.
#[must_use]
pub fn accessible_snapshot(state: &AppState) -> String {
    let mut lines = vec!["Codypendent accessible view".to_owned()];
    lines.push(format!(
        "Session: {}",
        clean(state.session_title.as_deref().unwrap_or("untitled"))
    ));
    if let Some(notice) = state.notice.as_ref().map(|(message, _)| message) {
        lines.push(format!("Notice: {}", clean(notice)));
    }
    for issue in &state.issues {
        lines.push(format!("Issue: {}", clean(issue)));
    }

    if state.runs.is_empty() {
        lines.push("Conversation: no runs yet".to_owned());
    } else {
        lines.push(format!(
            "Conversation: {} run(s); selected {}",
            state.runs.len(),
            state.selected_run.saturating_add(1).min(state.runs.len())
        ));
        for (index, run) in state.runs.iter().enumerate() {
            let selected = if index == state.selected_run {
                " selected"
            } else {
                ""
            };
            lines.push(format!(
                "Run {}{}: {:?}; mode {:?}; objective {}",
                index + 1,
                selected,
                run.state,
                run.mode,
                clean(&run.objective)
            ));
            if let Some(model) = &run.model {
                lines.push(format!("Model: {}", clean(&model.0)));
            }
            match &run.activity {
                RunActivity::Idle => {}
                RunActivity::Thinking => lines.push("Activity: thinking".to_owned()),
                RunActivity::Streaming => lines.push("Activity: responding".to_owned()),
                RunActivity::RunningTool(tool) => {
                    lines.push(format!("Activity: running tool {}", clean(tool)));
                }
            }
            for entry in &run.transcript {
                append_transcript(&mut lines, entry);
            }
        }
    }

    if !state.pending_approvals.is_empty() {
        lines.push(format!(
            "Approvals: {} pending; selected {}",
            state.pending_approvals.len(),
            state
                .selected_approval
                .saturating_add(1)
                .min(state.pending_approvals.len())
        ));
        if let Some(approval) = state.focused_approval() {
            lines.push(format!("Approval action: {:?}", approval.action));
            lines.push(format!("Approval risk: {:?}", approval.risk));
            lines.push("Approval controls: approve, approve-run, reject".to_owned());
        }
    }

    append_remote_ui(&mut lines, state);
    append_overlay(&mut lines, state);

    if !state.composer.is_empty() {
        lines.push(format!("Composer draft: {}", clean(&state.composer)));
    }
    lines.push(controls_for(state.input_mode()).to_owned());
    // Sanitize the finished projection as well as individual untrusted fields:
    // several protocol enums use `Debug` formatting above and future variants
    // must not accidentally reintroduce terminal controls through that path.
    clean(&lines.join("\n"))
}

fn append_transcript(lines: &mut Vec<String>, entry: &TranscriptEntry) {
    match entry {
        TranscriptEntry::User { text } => lines.push(format!("You: {}", clean(text))),
        TranscriptEntry::Model { text, .. } => {
            lines.push(format!("Assistant: {}", clean(text)));
        }
        TranscriptEntry::Tool(tool) => {
            let status = if tool.outcome.is_some() {
                "completed"
            } else {
                "running"
            };
            let label = tool
                .label
                .as_deref()
                .map(|label| format!("; {}", clean(label)))
                .unwrap_or_default();
            lines.push(format!("Tool: {}; {status}{label}", clean(&tool.tool)));
        }
        TranscriptEntry::Patch(patch) => lines.push(format!(
            "Patch: {} file(s), {} additions, {} deletions",
            patch.files.len(),
            patch.additions,
            patch.deletions
        )),
        TranscriptEntry::Steering { applied } => lines.push(if *applied {
            "Steering: applied".to_owned()
        } else {
            "Steering: queued".to_owned()
        }),
        TranscriptEntry::Budget {
            dimension,
            used,
            limit,
        } => lines.push(format!("Budget warning: {dimension:?} {used} of {limit}")),
        TranscriptEntry::Completed { disposition, .. } => match disposition {
            RunDisposition::Completed { summary } => lines.push(format!(
                "Completed: {}",
                clean(summary.as_deref().unwrap_or("success"))
            )),
            RunDisposition::Failed { reason } => {
                lines.push(format!("Failed: {}", clean(reason)));
            }
            RunDisposition::Cancelled { reason } => lines.push(format!(
                "Cancelled: {}",
                clean(reason.as_deref().unwrap_or("no reason supplied"))
            )),
            RunDisposition::Unknown => lines.push("Completed: unknown outcome".to_owned()),
            _ => lines.push("Completed: unsupported outcome".to_owned()),
        },
        TranscriptEntry::Note { text, .. } => lines.push(format!("Note: {}", clean(text))),
        TranscriptEntry::Backstage {
            context_lines,
            memory_updates,
            ..
        } => lines.push(format!(
            "Backstage: {} context line(s), {memory_updates} memory update(s)",
            context_lines.unwrap_or(0)
        )),
        TranscriptEntry::Unsupported { label } => {
            lines.push(format!("Unsupported event: {}", clean(label)));
        }
    }
}

fn append_remote_ui(lines: &mut Vec<String>, state: &AppState) {
    let documents = state.remote_ui.mounted_documents();
    if documents.is_empty() {
        return;
    }
    lines.push(format!("Extension documents: {}", documents.len()));
    for document in documents {
        let focused = state.remote_ui.active
            && state.remote_ui.focused_document.as_ref() == Some(&document.document_id);
        let (extension, publisher, trust) = state
            .remote_ui
            .extension_identity_for_document(&document.document_id)
            .unwrap_or(("unknown extension", None, None));
        let mut identity = clean(extension);
        if let Some(publisher) = publisher {
            identity.push_str("; publisher ");
            identity.push_str(&clean(publisher));
        }
        if let Some(trust) = trust {
            identity.push_str("; trust ");
            identity.push_str(&clean(trust));
        }
        lines.push(format!(
            "Extension document {}{}: {}",
            clean(document.document_id.as_str()),
            if focused { " focused" } else { "" },
            identity
        ));
        let projection = project_accessibility(&document.root, &state.remote_ui.capabilities);
        if projection.plain_text.is_empty() {
            lines.push("Extension content: no readable content".to_owned());
        } else {
            lines.push("Extension content:".to_owned());
            lines.extend(
                projection
                    .plain_text
                    .lines()
                    .map(|line| format!("  {}", clean(line))),
            );
        }
        if !projection.nodes.is_empty() {
            lines.push("Extension semantic controls:".to_owned());
            for node in &projection.nodes {
                let selected =
                    focused && node.node_id.as_ref() == state.remote_ui.view.focused_node.as_ref();
                let mut description = format!(
                    "  {}{}: {}",
                    node.role.as_str(),
                    if selected { " focused" } else { "" },
                    clean(&node.label)
                );
                if node.disabled {
                    description.push_str("; disabled");
                }
                if let Some(detail) = &node.description {
                    description.push_str("; ");
                    description.push_str(&clean(detail));
                }
                if let Some(hint) = &node.keyboard_hint {
                    description.push_str("; keyboard ");
                    description.push_str(&clean(hint));
                }
                if let Some(live_region) = &node.live_region {
                    description.push_str("; live region ");
                    description.push_str(&clean(live_region));
                }
                lines.push(description);
            }
        }
        if focused {
            if let Some(focused_node) = state.remote_ui.view.focused_node.as_ref() {
                if let Some(node) = projection
                    .nodes
                    .iter()
                    .find(|node| node.node_id.as_ref() == Some(focused_node))
                {
                    let mut focused_line = format!("Focused control: {}", clean(&node.label));
                    if node.disabled {
                        focused_line.push_str("; disabled");
                    }
                    if let Some(hint) = &node.keyboard_hint {
                        focused_line.push_str("; ");
                        focused_line.push_str(&clean(hint));
                    }
                    lines.push(focused_line);
                }
            }
        }
    }
}

fn append_overlay(lines: &mut Vec<String>, state: &AppState) {
    match &state.overlay {
        Overlay::None => {}
        Overlay::Help => {
            lines.push("Help:".to_owned());
            lines.extend(KEY_BINDINGS.iter().map(|binding| {
                format!(
                    "  {}: {}",
                    ascii_keys(binding.keys),
                    ascii_chrome(binding.description)
                )
            }));
        }
        Overlay::Palette { query, selected } => lines.push(format!(
            "Command palette: query {}; selected result {}",
            clean(query),
            selected + 1
        )),
        Overlay::ModelPicker { query, selected } => lines.push(format!(
            "Model picker: query {}; selected result {}",
            clean(query),
            selected + 1
        )),
        Overlay::ProviderPicker { query, selected } => lines.push(format!(
            "Provider picker: query {}; selected result {}",
            clean(query),
            selected + 1
        )),
        Overlay::ModePicker { query, selected } => lines.push(format!(
            "Mode picker: query {}; selected result {}",
            clean(query),
            selected + 1
        )),
        Overlay::NewRun(buffer) => {
            lines.push(format!("New run prompt: {}", clean(buffer)));
        }
        Overlay::Steering(buffer) => {
            lines.push(format!("Steering prompt: {}", clean(buffer)));
        }
        Overlay::ConfirmCancel => lines.push("Confirmation: cancel this run?".to_owned()),
        Overlay::ConfirmWorkflowCancel { workflow_run_id } => lines.push(format!(
            "Confirmation: cancel workflow run {}?",
            clean(workflow_run_id)
        )),
        Overlay::CouncilRunObjective { name, buffer } => {
            lines.push(format!(
                "Objective for council {}: {}",
                clean(name),
                clean(buffer)
            ));
        }
        Overlay::ConfirmCouncilDelete { name } => lines.push(format!(
            "Confirmation: remove council {}? Saved run reports remain on disk.",
            clean(name)
        )),
        Overlay::CouncilBrowser => {
            lines.push(format!(
                "Agent councils: {} configured",
                state.councils.len()
            ));
            if let Some(council) = state.focused_council() {
                lines.push(format!(
                    "Focused council: {}; chair {}; {} round(s); {} member(s){}",
                    clean(&council.name),
                    clean(&council.chair),
                    council.rounds,
                    council.members.len(),
                    if council.evidence {
                        "; evidence mode"
                    } else {
                        ""
                    }
                ));
            }
        }
        Overlay::CouncilBuilder(builder) => {
            lines.push(format!(
                "Council builder: step {:?}; name {}; {} member(s); chair {}; rounds {}",
                builder.step,
                clean(&builder.name),
                builder.members.len(),
                clean(builder.chair.as_deref().unwrap_or("not selected")),
                builder.rounds
            ));
            for member in &builder.members {
                lines.push(format!(
                    "Council member: {}; role {}",
                    clean(&member.model),
                    clean(&member.role)
                ));
            }
        }
        other => lines.push(format!("Open dialog: {}", overlay_name(other))),
    }
}

fn overlay_name(overlay: &Overlay) -> &'static str {
    match overlay {
        Overlay::Issues => "setup and diagnostics",
        Overlay::Skills => "skills",
        Overlay::Memory { .. } => "memory",
        Overlay::Docs => "documents",
        Overlay::Edges | Overlay::EdgeSearch(_) => "code graph",
        Overlay::Workflow | Overlay::WorkflowInputs { .. } => "workflow",
        Overlay::Blackboard => "blackboard",
        Overlay::Kanban => "task board",
        Overlay::UiPlugins => "Remote UI plugins",
        Overlay::ApiKeys { .. } => "API keys",
        Overlay::ApiKeySet { .. } => "API key entry",
        Overlay::ApiKeyRemoveConfirm { .. } => "remove API key confirmation",
        Overlay::CouncilBuilder(_) => "council builder",
        Overlay::CouncilBrowser => "agent councils",
        Overlay::CouncilRunObjective { .. } => "council objective",
        Overlay::ConfirmCouncilDelete { .. } => "remove council confirmation",
        Overlay::AddModelId { .. }
        | Overlay::AddModelKey { .. }
        | Overlay::AddModelProviderKey { .. }
        | Overlay::AddModelQuerying { .. }
        | Overlay::AddModelPick { .. } => "add model",
        Overlay::UnslothRepos { .. }
        | Overlay::UnslothQuants { .. }
        | Overlay::UnslothConfirmPull { .. }
        | Overlay::UnslothPulling { .. } => "local models: unsloth catalog",
        Overlay::DocEdit { .. } | Overlay::DocInsert { .. } => "document editor",
        Overlay::DocNew { .. } => "new document",
        Overlay::DocDeleteConfirm { .. } => "delete block confirmation",
        Overlay::DocPublishPath { .. } => "document publish path",
        Overlay::ConfirmUiPluginApprove { .. }
        | Overlay::ConfirmUiPluginReject { .. }
        | Overlay::ConfirmUiPluginRevoke { .. } => "Remote UI plugin confirmation",
        Overlay::Help
        | Overlay::None
        | Overlay::NewRun(_)
        | Overlay::Steering(_)
        | Overlay::ConfirmCancel
        | Overlay::ConfirmWorkflowCancel { .. }
        | Overlay::Palette { .. }
        | Overlay::ModelPicker { .. }
        | Overlay::ProviderPicker { .. }
        | Overlay::ModePicker { .. } => "dialog",
    }
}

fn controls_for(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Composer => {
            "Controls: type a message and press Enter, or use help, /, F6, Shift-F6, quit"
        }
        InputMode::RemoteUi => {
            "Controls: Tab or backtab move focus, Enter activates, type TEXT edits, Shift-F6 changes document, Esc returns"
        }
        InputMode::Palette => {
            "Controls: type TEXT filters, up/down select, Enter chooses, Esc closes"
        }
        InputMode::Editing => "Controls: type TEXT, Enter submits, Esc cancels",
        InputMode::Confirm => "Controls: yes or Enter confirms, no or Esc cancels",
        InputMode::Approval => "Controls: approve, approve-run, reject, up, down",
        InputMode::Normal => "Controls: up, down, Enter, Esc, help, quit",
    }
}

/// Convert one cooked input line into semantic actions. Commands are ASCII and
/// case-insensitive. In the base composer an otherwise-unrecognised line is sent
/// as the user's message; in an editor/filter/Remote UI field it is inserted.
#[must_use]
pub fn map_accessible_input(line: &str, mode: InputMode) -> Vec<Action> {
    let line = line.trim_end_matches(['\r', '\n']);
    let command = line.trim();
    let lower = command.to_ascii_lowercase();
    match lower.as_str() {
        "" => return vec![submit_action(mode)],
        "help" | "?" => return vec![Action::Help],
        "quit" | "exit" => return vec![Action::Detach],
        "/" | "commands" => return vec![Action::OpenPalette],
        "f6" => return vec![Action::RemoteUiSetActive(true)],
        "shift-f6" | "next-document" => return vec![Action::RemoteUiNextDocument],
        "esc" | "escape" | "cancel" => return vec![cancel_action(mode)],
        "enter" | "choose" | "yes" => return vec![submit_action(mode)],
        "space" if mode == InputMode::RemoteUi => {
            return vec![Action::RemoteUiKey {
                key: RemoteKey::Space,
                character: Some(' '),
            }];
        }
        "tab" => return vec![tab_action(mode, false)],
        "backtab" | "shift-tab" => return vec![tab_action(mode, true)],
        "up" => return vec![navigation_action(mode, true)],
        "down" => return vec![navigation_action(mode, false)],
        "pageup" | "page-up" => return vec![page_action(mode, true)],
        "pagedown" | "page-down" => return vec![page_action(mode, false)],
        "approve" => return vec![Action::Approve(ApprovalScope::Once)],
        "approve-run" => return vec![Action::Approve(ApprovalScope::Run)],
        "reject" => return vec![Action::Reject],
        "backspace" => {
            return vec![if mode == InputMode::RemoteUi {
                Action::RemoteUiKey {
                    key: RemoteKey::Backspace,
                    character: None,
                }
            } else {
                Action::InputBackspace
            }];
        }
        _ => {}
    }

    if lower.starts_with("send ") {
        let text = &command["send ".len()..];
        return vec![paste_action(mode, text.to_owned()), submit_action(mode)];
    }
    if lower.starts_with("type ") {
        let text = &command["type ".len()..];
        return vec![paste_action(mode, text.to_owned())];
    }
    match mode {
        InputMode::Composer => vec![Action::InputPaste(line.to_owned()), Action::InputSubmit],
        InputMode::RemoteUi | InputMode::Editing | InputMode::Palette => {
            vec![paste_action(mode, line.to_owned())]
        }
        _ => vec![Action::Notice(
            "unrecognised accessible command; type help for commands".to_owned(),
        )],
    }
}

fn submit_action(mode: InputMode) -> Action {
    match mode {
        InputMode::RemoteUi => Action::RemoteUiKey {
            key: RemoteKey::Enter,
            character: None,
        },
        InputMode::Confirm => Action::ConfirmCancel,
        InputMode::Normal => Action::Expand,
        _ => Action::InputSubmit,
    }
}

fn cancel_action(mode: InputMode) -> Action {
    match mode {
        InputMode::RemoteUi => Action::RemoteUiSetActive(false),
        InputMode::Editing | InputMode::Palette | InputMode::Composer => Action::InputCancel,
        _ => Action::Dismiss,
    }
}

fn tab_action(mode: InputMode, reverse: bool) -> Action {
    if mode == InputMode::RemoteUi {
        Action::RemoteUiKey {
            key: if reverse {
                RemoteKey::ShiftTab
            } else {
                RemoteKey::Tab
            },
            character: None,
        }
    } else if mode == InputMode::Palette {
        Action::BeginAddModel
    } else {
        Action::CyclePane
    }
}

fn navigation_action(mode: InputMode, previous: bool) -> Action {
    if mode == InputMode::RemoteUi {
        Action::RemoteUiKey {
            key: if previous {
                RemoteKey::Up
            } else {
                RemoteKey::Down
            },
            character: None,
        }
    } else if previous {
        Action::SelectPrev
    } else {
        Action::SelectNext
    }
}

fn page_action(mode: InputMode, previous: bool) -> Action {
    if mode == InputMode::RemoteUi {
        Action::RemoteUiKey {
            key: if previous {
                RemoteKey::PageUp
            } else {
                RemoteKey::PageDown
            },
            character: None,
        }
    } else if previous {
        Action::ScrollPageUp
    } else {
        Action::ScrollPageDown
    }
}

fn paste_action(mode: InputMode, text: String) -> Action {
    if mode == InputMode::RemoteUi {
        Action::RemoteUiPaste(text)
    } else {
        Action::InputPaste(text)
    }
}

fn ascii_keys(keys: &str) -> String {
    ascii_chrome(keys)
}

fn ascii_chrome(text: &str) -> String {
    clean(text)
        .replace('↑', "Up")
        .replace('↓', "Down")
        .replace('⇄', "<->")
        .replace('…', "...")
        .replace('·', "/")
        .replace(['—', '–'], "-")
        .replace('→', "->")
        .replace('←', "<-")
        .replace('⌥', "Alt-")
}

/// Remove terminal control sequences and Unicode bidi controls from text that
/// will be written to the cooked accessibility stream.
#[must_use]
pub fn sanitize_accessible_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        // Strip complete ANSI CSI/OSC sequences, rather than only their ESC
        // introducer (which would leave distracting `[31m` debris in a screen
        // reader). C1 equivalents are treated identically.
        if character == '\u{1b}' {
            match characters.next() {
                Some('[') => {
                    for parameter in characters.by_ref() {
                        if ('@'..='~').contains(&parameter) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(parameter) = characters.next() {
                        if parameter == '\u{7}' {
                            break;
                        }
                        if parameter == '\u{1b}' && characters.peek() == Some(&'\\') {
                            characters.next();
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }
        if character == '\u{9b}' {
            for parameter in characters.by_ref() {
                if ('@'..='~').contains(&parameter) {
                    break;
                }
            }
            continue;
        }
        if character == '\u{9d}' {
            while let Some(parameter) = characters.next() {
                if parameter == '\u{7}' {
                    break;
                }
                if parameter == '\u{1b}' && characters.peek() == Some(&'\\') {
                    characters.next();
                    break;
                }
            }
            continue;
        }
        if matches!(character, '\n' | '\t')
            || (!character.is_control()
                && !matches!(
                    character as u32,
                    0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
                ))
        {
            cleaned.push(character);
        }
    }
    cleaned
}

fn clean(text: &str) -> String {
    sanitize_accessible_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CouncilBuilderState, CouncilBuilderStep, CouncilMemberDraft};
    use codypendent_protocol::{
        Actor, EventBody, RunId, SessionEvent, UiContributionId, UiContributionPoint,
        UiContributionRegistration, UiDocument, UiDocumentId, UiExtensionId, UiSlotId,
    };

    #[test]
    fn composer_lines_send_and_remote_lines_edit_without_activation() {
        assert_eq!(
            map_accessible_input("build it", InputMode::Composer),
            vec![
                Action::InputPaste("build it".to_owned()),
                Action::InputSubmit
            ]
        );
        assert_eq!(
            map_accessible_input("type query", InputMode::RemoteUi),
            vec![Action::RemoteUiPaste("query".to_owned())]
        );
        assert_eq!(
            map_accessible_input("shift-f6", InputMode::RemoteUi),
            vec![Action::RemoteUiNextDocument]
        );
    }

    #[test]
    fn council_builder_is_announced_with_attributed_members() {
        let mut state = AppState::new();
        state.overlay = Overlay::CouncilBuilder(CouncilBuilderState {
            step: CouncilBuilderStep::Review,
            name: "design-board".to_owned(),
            description: "Architecture".to_owned(),
            members: vec![CouncilMemberDraft {
                model: "kimi-code".to_owned(),
                role: "systems architect".to_owned(),
            }],
            chair: Some("amp".to_owned()),
            rounds: 2,
            query: String::new(),
            selected: 0,
            pending_member_model: None,
            role: String::new(),
        });
        let snapshot = accessible_snapshot(&state);
        assert!(snapshot.contains("Council builder: step Review"));
        assert!(snapshot.contains("design-board"));
        assert!(snapshot.contains("Council member: kimi-code; role systems architect"));
        assert!(snapshot.contains("chair amp; rounds 2"));
    }

    #[test]
    fn snapshot_is_linear_ascii_chrome_and_strips_terminal_controls() {
        let mut state = AppState::new();
        let run_id = RunId::new();
        crate::reduce(
            &mut state,
            Action::daemon_event(SessionEvent {
                sequence: 1,
                occurred_at: chrono::Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::System,
                body: EventBody::RunStarted {
                    run_id,
                    objective: "build\u{1b}[31m safely".to_owned(),
                    mode: codypendent_protocol::AgentMode::Build,
                },
            }),
        );
        let snapshot = accessible_snapshot(&state);
        assert!(snapshot.contains("Codypendent accessible view"));
        assert!(snapshot.contains("build safely"));
        assert!(!snapshot.contains('\u{1b}'));
        for glyph in ['│', '┌', '┐', '⌕', '▏'] {
            assert!(!snapshot.contains(glyph));
        }
    }

    #[test]
    fn snapshot_includes_remote_ui_plain_text_and_semantic_metadata() {
        let mut state = AppState::new();
        crate::reduce(
            &mut state,
            Action::RemoteUiMessage(Box::new(
                crate::remote_ui_host::accessible_terminal_capabilities_message(80, 24),
            )),
        );
        let document: UiDocument = serde_json::from_value(serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "accessible-extension",
            "revision": 1,
            "root": {
                "kind": "element", "id": "launch", "type": "Button",
                "props": {
                    "label": "Launch",
                    "accessibility": {
                        "description": "Starts the workflow",
                        "keyboardHint": "Enter",
                        "liveRegion": "polite"
                    }
                },
                "children": []
            }
        }))
        .expect("accessible document");
        let mut snapshot = crate::remote_ui_host::empty_message("snapshot", "accessible-snapshot");
        snapshot.snapshot = Some(codypendent_protocol::UiSnapshot {
            document,
            reason: None,
        });
        crate::reduce(&mut state, Action::RemoteUiMessage(Box::new(snapshot)));
        let mut contributions =
            crate::remote_ui_host::empty_message("contributions", "accessible-contribution");
        contributions
            .contributions
            .push(UiContributionRegistration {
                id: UiContributionId::from("accessible-registration"),
                extension_id: UiExtensionId::from("accessible-extension"),
                point: UiContributionPoint::from("panel"),
                slot: UiSlotId::from("panel"),
                document_id: UiDocumentId::from("accessible-extension"),
                priority: 0,
                when: None,
                requires: Vec::new(),
                metadata: Default::default(),
            });
        crate::reduce(&mut state, Action::RemoteUiMessage(Box::new(contributions)));

        let output = accessible_snapshot(&state);
        assert!(output.contains("Extension document accessible-extension"));
        assert!(output.contains("Launch"));
        assert!(output.contains("Starts the workflow"));
        assert!(output.contains("keyboard Enter"));
        assert!(output.contains("live region polite"));
    }

    #[test]
    fn help_chrome_has_a_strict_ascii_fallback() {
        let mut state = AppState::new();
        state.overlay = Overlay::Help;
        let output = accessible_snapshot(&state);
        assert!(output.is_ascii(), "help chrome was not ASCII: {output}");
        assert!(output.contains("F6 / Shift-F6 / Esc"));
    }
}

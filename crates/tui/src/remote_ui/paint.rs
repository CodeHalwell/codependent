use std::cell::RefCell;
use std::collections::HashMap;

use codypendent_protocol::remote_ui::{
    node_kinds, primitives, UiActionBinding, UiData, UiDocument, UiNode, UiNodeId, UiSemanticRole,
    UiStyle, UiTextSpan,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap};
use serde_json::Value;

use super::layout::{self, Axis};
use super::text::{cell_width, pad_cells, sanitize_terminal_text, truncate_cells, wrap_cells};
use super::{
    project_accessibility, resolve_node, DiagnosticSeverity, FocusDescriptor, FormFieldDescriptor,
    HitRegion, KeyboardAction, RemoteKey, RemoteUiRenderOptions, RemoteUiRenderOutput,
    RemoteUiViewState, RenderDiagnostic, ResolvedNode, TerminalUiCapabilities,
};
use crate::markdown::{self, SpanRole, SyntaxRole};
use crate::Theme;

const MAX_MEASURED_HEIGHT: u16 = 16_384;

pub(super) fn render(
    buffer: &mut Buffer,
    area: Rect,
    document: &UiDocument,
    theme: &Theme,
    capabilities: &TerminalUiCapabilities,
    state: &RemoteUiViewState,
    options: RemoteUiRenderOptions,
) -> RemoteUiRenderOutput {
    let normalized_document = super::codec::normalize_document(document);
    let document = &normalized_document;
    let area = area.intersection(buffer.area);
    let mut output = RemoteUiRenderOutput {
        accessibility: project_accessibility(&document.root, capabilities),
        ..RemoteUiRenderOutput::default()
    };
    if area.is_empty() {
        return output;
    }
    buffer.set_style(
        area,
        Style::default()
            .fg(theme.text.primary)
            .bg(theme.surface.background),
    );

    if let Err(error) = document.validate(&options.limits) {
        output.diagnostics.push(RenderDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "remote-ui.invalid-document",
            node_id: None,
            message: format!("{} at {}: {}", error.code, error.path, error.message),
        });
        diagnostic_panel(
            buffer,
            area,
            theme,
            "Remote UI rejected",
            &format!("{} at {}\n{}", error.code, error.path, error.message),
        );
        return output;
    }

    let mut painter = Painter {
        buffer,
        clip: area,
        theme,
        capabilities,
        state,
        options,
        output,
        visited: 0,
        focus_sequence: 0,
        visibility_clip: area,
        measure_cache: RefCell::new(HashMap::new()),
    };

    let compatibility_ok = document.compatibility.as_ref().is_none_or(|compatibility| {
        compatibility.minimum_protocol.is_none_or(|minimum| {
            document.protocol_version.major == minimum.major
                && document.protocol_version.minor >= minimum.minor
        }) && compatibility
            .required_primitives
            .iter()
            .all(|primitive| capabilities.supports_primitive(primitive.as_str()))
            && compatibility
                .required_capabilities
                .iter()
                .all(|capability| capabilities.supports_feature(capability.as_str()))
    });
    if compatibility_ok {
        painter.node(&document.root, area, 0);
    } else if let Some(fallback) = document
        .compatibility
        .as_ref()
        .and_then(|compatibility| compatibility.fallback.as_ref())
    {
        painter.diagnostic(
            None,
            DiagnosticSeverity::Warning,
            "remote-ui.incompatible-document",
            "document compatibility requirements are not available in this terminal",
        );
        if let Some(replacement) = fallback.replacement.as_deref() {
            painter.node(replacement, area, 0);
        } else if let Some(text) = fallback.plain_text.as_deref() {
            painter.text_lines(text, area, painter.base_style(), 0);
        }
    } else {
        painter.diagnostic(
            None,
            DiagnosticSeverity::Error,
            "remote-ui.incompatible-document",
            "document compatibility requirements are not available in this terminal",
        );
        diagnostic_panel(
            painter.buffer,
            area,
            theme,
            "Unsupported surface",
            "This terminal cannot render the capabilities required by this surface.",
        );
    }

    painter.output.focus_order.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.area.y.cmp(&right.area.y))
            .then_with(|| left.area.x.cmp(&right.area.x))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    painter.output
}

struct Painter<'a> {
    buffer: &'a mut Buffer,
    clip: Rect,
    theme: &'a Theme,
    capabilities: &'a TerminalUiCapabilities,
    state: &'a RemoteUiViewState,
    options: RemoteUiRenderOptions,
    output: RemoteUiRenderOutput,
    visited: u32,
    focus_sequence: i32,
    /// Region that contributes to `visible_nodes`. Usually identical to
    /// `clip`; a partially visible scroll child is rendered into a logical
    /// off-screen buffer and uses the source viewport here instead.
    visibility_clip: Rect,
    /// Per-frame memoization of [`Painter::measure`], keyed by node identity,
    /// available width, and depth. Containers re-measure the same immutable
    /// subtree several times per frame (collapse checks, grids, tabs); every
    /// measured node is borrowed from the one document the frame renders
    /// ([`resolve_node`] only ever yields document-owned nodes), so node
    /// addresses are stable keys for the lifetime of this painter.
    measure_cache: RefCell<HashMap<(usize, u16, u16), u16>>,
}

impl Painter<'_> {
    fn node(&mut self, original: &UiNode, area: Rect, depth: u16) {
        let area = area.intersection(self.clip);
        if area.is_empty() {
            return;
        }
        if depth > self.options.limits.max_tree_depth
            || self.visited >= self.options.limits.max_nodes
        {
            self.diagnostic(
                original.id.clone(),
                DiagnosticSeverity::Error,
                "remote-ui.render-limit",
                "renderer tree budget exhausted",
            );
            self.text_lines(
                "[surface truncated: render limit]",
                area,
                self.error_style(),
                0,
            );
            return;
        }
        self.visited += 1;

        match resolve_node(original, self.capabilities) {
            ResolvedNode::Plain(text) => {
                self.diagnostic(
                    original.id.clone(),
                    DiagnosticSeverity::Warning,
                    "remote-ui.fallback",
                    "rendered a plain-text capability fallback",
                );
                self.text_lines(&text, area, self.base_style(), 0);
            }
            ResolvedNode::Node(node) => self.resolved_node(node, area, depth),
        }
    }

    fn resolved_node(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if node
            .props
            .style
            .as_ref()
            .and_then(|style| style.visibility.as_deref())
            .is_some_and(|visibility| matches!(visibility, "hidden" | "collapse"))
            || node
                .props
                .accessibility
                .as_ref()
                .is_some_and(|accessibility| accessibility.hidden)
        {
            return;
        }
        let area = layout::margin(area, node.props.layout.as_ref());
        if area.is_empty() {
            return;
        }
        if !area.intersection(self.visibility_clip).is_empty() {
            if let Some(id) = &node.id {
                self.output.visible_nodes.insert(id.clone());
            }
        }
        if node.kind.as_str() == node_kinds::TEXT {
            self.text_lines(
                node.text.as_deref().unwrap_or_default(),
                area,
                self.node_style(node),
                0,
            );
            return;
        }
        let Some(primitive) = node.node_type.as_ref().map(|value| value.as_str()) else {
            self.diagnostic(
                node.id.clone(),
                DiagnosticSeverity::Error,
                "remote-ui.missing-primitive",
                "element has no primitive type",
            );
            self.text_lines("[invalid component]", area, self.error_style(), 0);
            return;
        };

        let style = self.node_style(node);
        self.buffer.set_style(area, style);
        match primitive {
            primitives::BOX | primitives::STACK => self.vertical_container(node, area, depth),
            primitives::ROW => self.row_container(node, area, depth),
            primitives::GRID => self.grid_container(node, area, depth),
            primitives::SPLIT => self.split_container(node, area, depth),
            primitives::SCROLL_AREA => self.scroll_container(node, area, depth),
            primitives::PORTAL => self.portal(node, area, depth),
            primitives::SPACER => {}
            primitives::VIRTUAL_LIST => self.virtual_list(node, area, depth),
            primitives::TEXT => self.text_content(node, area),
            primitives::MARKDOWN => self.markdown(node, area),
            primitives::CODE => self.code(node, area),
            primitives::DIFF => self.diff(node, area),
            primitives::IMAGE => self.media(node, area, true),
            primitives::AUDIO => self.media(node, area, false),
            primitives::JSON_TREE => self.json_tree(node, area),
            primitives::LOG_VIEWER => self.log_viewer(node, area),
            primitives::LIST => self.list(node, area, depth),
            primitives::TABLE => self.table(node, area),
            primitives::TREE => self.tree(node, area, depth),
            primitives::KEY_VALUE => self.key_value(node, area),
            primitives::TIMELINE => self.timeline(node, area),
            primitives::GRAPH => self.graph(node, area),
            primitives::CHART => self.chart(node, area, false),
            primitives::SPARKLINE => self.chart(node, area, true),
            primitives::BADGE => self.badge(node, area),
            primitives::PROGRESS => self.progress(node, area),
            primitives::SPINNER => self.spinner(node, area),
            primitives::ALERT | primitives::TOAST | primitives::EMPTY_STATE => {
                self.feedback_panel(node, area, depth, primitive)
            }
            primitives::ERROR_BOUNDARY => self.error_boundary(node, area, depth),
            primitives::TABS => self.tabs(node, area, depth),
            primitives::BREADCRUMB => self.breadcrumb(node, area),
            primitives::MENU | primitives::COMMAND_LIST => self.menu(node, area, depth),
            primitives::PAGINATION => self.pagination(node, area),
            primitives::LINK => self.link(node, area),
            "Details" => self.vertical_container(node, area, depth),
            primitives::TEXT_INPUT
            | primitives::TEXT_AREA
            | primitives::SELECT
            | primitives::MULTI_SELECT
            | primitives::CHECKBOX
            | primitives::RADIO => self.input(node, area, primitive),
            primitives::FORM => self.form(node, area, depth),
            primitives::BUTTON => self.button(node, area),
            primitives::ACTION_MENU | primitives::CONTEXT_MENU => {
                self.action_menu(node, area, depth)
            }
            primitives::TOOLBAR => self.toolbar(node, area, depth),
            _ if super::is_domain_primitive(primitive) => {
                self.domain_card(node, area, depth, primitive)
            }
            _ => self.custom(node, area, depth, primitive),
        }
    }

    fn vertical_container(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let border = self.wants_border(node);
        self.draw_border(node, area, border, None);
        let inner = layout::content_box(area, node.props.layout.as_ref(), border);
        let heights = node
            .children
            .iter()
            .map(|child| self.measure(child, inner.width, depth + 1))
            .collect::<Vec<_>>();
        let rects = layout::vertical(
            inner,
            &node.children,
            &heights,
            layout::gap(node.props.layout.as_ref(), Axis::Vertical),
        );
        for (child, child_area) in node.children.iter().zip(rects) {
            self.node(child, child_area, depth + 1);
        }
    }

    fn row_container(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let border = self.wants_border(node);
        self.draw_border(node, area, border, None);
        let inner = layout::content_box(area, node.props.layout.as_ref(), border);
        let collapse = inner.width < self.options.narrow_breakpoint
            || node
                .props
                .layout
                .as_ref()
                .and_then(|layout| layout.wrap.as_deref())
                .is_some_and(|wrap| wrap == "column");
        if collapse {
            let heights = node
                .children
                .iter()
                .map(|child| self.measure(child, inner.width, depth + 1))
                .collect::<Vec<_>>();
            let rects = layout::vertical(
                inner,
                &node.children,
                &heights,
                layout::gap(node.props.layout.as_ref(), Axis::Vertical),
            );
            for (child, child_area) in node.children.iter().zip(rects) {
                self.node(child, child_area, depth + 1);
            }
        } else {
            let widths = node
                .children
                .iter()
                .map(|child| self.measure_width(child, inner.width))
                .collect::<Vec<_>>();
            let rects = layout::horizontal(
                inner,
                &node.children,
                &widths,
                layout::gap(node.props.layout.as_ref(), Axis::Horizontal),
            );
            for (child, child_area) in node.children.iter().zip(rects) {
                self.node(child, child_area, depth + 1);
            }
        }
    }

    fn grid_container(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let border = self.wants_border(node);
        self.draw_border(node, area, border, None);
        let inner = layout::content_box(area, node.props.layout.as_ref(), border);
        let heights = node
            .children
            .iter()
            .map(|child| self.measure(child, inner.width.max(1), depth + 1))
            .collect::<Vec<_>>();
        let layout_props = node.props.layout.as_ref();
        let rects = layout::grid(
            inner,
            &node.children,
            layout_props.map_or(&[], |layout| layout.columns.as_slice()),
            layout::gap(layout_props, Axis::Horizontal),
            layout::gap(layout_props, Axis::Vertical),
            &heights,
            inner.width < self.options.narrow_breakpoint,
        );
        for (child, child_area) in node.children.iter().zip(rects) {
            self.node(child, child_area, depth + 1);
        }
    }

    fn split_container(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let vertical = split_is_vertical(node);
        if !vertical && area.width < self.options.narrow_breakpoint {
            self.vertical_container(node, area, depth);
            return;
        }
        let border = self.wants_border(node);
        self.draw_border(node, area, border, None);
        let inner = layout::content_box(area, node.props.layout.as_ref(), border);
        let axis = if vertical {
            Axis::Vertical
        } else {
            Axis::Horizontal
        };
        let gap = layout::gap(node.props.layout.as_ref(), axis);
        let usable = if vertical { inner.height } else { inner.width }
            .saturating_sub(gap.saturating_mul(node.children.len().saturating_sub(1) as u16));
        let sizes = split_sizes(usable, node.children.len(), split_ratio(node));
        let rects = if vertical {
            layout::vertical(inner, &node.children, &sizes, gap)
        } else {
            layout::horizontal(inner, &node.children, &sizes, gap)
        };
        for (child, child_area) in node.children.iter().zip(rects) {
            self.node(child, child_area, depth + 1);
        }
    }

    fn scroll_container(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let border = self.wants_border(node);
        self.draw_border(node, area, border, None);
        let inner = layout::content_box(area, node.props.layout.as_ref(), border);
        let offset = self.scroll_offset(node);
        let gap = layout::gap(node.props.layout.as_ref(), Axis::Vertical);
        let mut content_area = inner;
        let mut heights = node
            .children
            .iter()
            .map(|child| self.measure(child, inner.width, depth + 1).max(1))
            .collect::<Vec<_>>();
        let mut total = content_height(&heights, gap);
        if total > u32::from(inner.height) && inner.width > 1 {
            content_area.width -= 1;
            heights = node
                .children
                .iter()
                .map(|child| self.measure(child, content_area.width, depth + 1).max(1))
                .collect();
            total = content_height(&heights, gap);
        }
        let mut logical_y = 0_u32;
        let viewport_end = offset.saturating_add(u32::from(content_area.height));
        for (child, height) in node.children.iter().zip(heights) {
            let child_start = logical_y;
            let child_end = child_start.saturating_add(u32::from(height));
            if child_end > offset && child_start < viewport_end {
                let clipped_top = offset.saturating_sub(child_start).min(u32::from(height));
                let visible_height = child_end
                    .min(viewport_end)
                    .saturating_sub(child_start.max(offset))
                    as u16;
                let y = content_area
                    .y
                    .saturating_add(child_start.saturating_sub(offset) as u16);
                let child_area = Rect::new(content_area.x, y, content_area.width, visible_height);
                if clipped_top == 0 && visible_height == height {
                    self.node(child, child_area, depth + 1);
                } else if visible_height > 0 {
                    self.scrolled_child(
                        child,
                        content_area.width,
                        height,
                        clipped_top as u16,
                        child_area,
                        depth + 1,
                    );
                }
            }
            logical_y = child_end.saturating_add(u32::from(gap));
            if logical_y >= viewport_end {
                break;
            }
        }
        self.scrollbar(inner, total, offset, inner.height);
    }

    /// Paint a partially visible child at its full logical height, then crop
    /// the requested rows into the real viewport. Passing a shortened area
    /// directly to a Stack/Grid would make that container lay itself out again
    /// from row zero, which produces the wrong rows for nested scroll content.
    fn scrolled_child(
        &mut self,
        child: &UiNode,
        width: u16,
        height: u16,
        clipped_top: u16,
        destination: Rect,
        depth: u16,
    ) {
        let logical_area = Rect::new(0, 0, width, height);
        let source_viewport = Rect::new(0, clipped_top, width, destination.height);
        let mut scratch = Buffer::empty(logical_area);
        let mut nested = Painter {
            buffer: &mut scratch,
            clip: logical_area,
            theme: self.theme,
            capabilities: self.capabilities,
            state: self.state,
            options: self.options,
            output: RemoteUiRenderOutput::default(),
            visited: self.visited,
            focus_sequence: self.focus_sequence,
            visibility_clip: source_viewport,
            measure_cache: RefCell::new(HashMap::new()),
        };
        nested.node(child, logical_area, depth);
        self.visited = nested.visited;
        self.focus_sequence = nested.focus_sequence;
        let mut nested_output = std::mem::take(&mut nested.output);
        drop(nested);

        for row in 0..destination.height {
            let source_y = clipped_top.saturating_add(row);
            let destination_y = destination.y.saturating_add(row);
            for column in 0..destination.width {
                let Some(source) = scratch.cell((column, source_y)).cloned() else {
                    continue;
                };
                if let Some(target) = self
                    .buffer
                    .cell_mut((destination.x.saturating_add(column), destination_y))
                {
                    *target = source;
                }
            }
        }

        nested_output.focus_order.retain_mut(|descriptor| {
            translate_scrolled_rect(&mut descriptor.area, source_viewport, destination)
        });
        nested_output.hit_regions.retain_mut(|region| {
            translate_scrolled_rect(&mut region.area, source_viewport, destination)
        });
        nested_output
            .form_fields
            .retain(|field| nested_output.visible_nodes.contains(&field.node_id));
        self.output
            .focus_order
            .append(&mut nested_output.focus_order);
        self.output
            .hit_regions
            .append(&mut nested_output.hit_regions);
        self.output
            .form_fields
            .append(&mut nested_output.form_fields);
        self.output
            .diagnostics
            .append(&mut nested_output.diagnostics);
        self.output
            .visible_nodes
            .append(&mut nested_output.visible_nodes);
    }

    fn portal(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let overlay = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        self.draw_border(node, overlay, true, Some("Overlay"));
        let inner = layout::content_box(overlay, node.props.layout.as_ref(), true);
        self.render_children_vertical(node, inner, depth);
    }

    fn render_children_vertical(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let heights = node
            .children
            .iter()
            .map(|child| self.measure(child, area.width, depth + 1))
            .collect::<Vec<_>>();
        let rects = layout::vertical(
            area,
            &node.children,
            &heights,
            layout::gap(node.props.layout.as_ref(), Axis::Vertical),
        );
        for (child, child_area) in node.children.iter().zip(rects) {
            self.node(child, child_area, depth + 1);
        }
    }

    fn text_content(&mut self, node: &UiNode, area: Rect) {
        if let Some(content) = node.props.content.as_ref() {
            if !content.spans.is_empty() {
                self.rich_text_spans(&content.spans, area, self.scroll_offset(node));
                return;
            }
        }
        let text = content_text(node);
        self.text_lines(&text, area, self.node_style(node), self.scroll_offset(node));
    }

    fn rich_text_spans(&mut self, spans: &[UiTextSpan], area: Rect, scroll: u32) {
        let rich = Line::from(
            spans
                .iter()
                .map(|span| {
                    let mut style = span
                        .style
                        .as_ref()
                        .map_or_else(|| self.base_style(), |style| self.semantic_style(style));
                    if span.link.is_some() {
                        style = style
                            .fg(self.theme.focus.active)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                    Span::styled(sanitize_terminal_text(&span.text), style)
                })
                .collect::<Vec<_>>(),
        );
        Paragraph::new(rich)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, self.buffer);
    }

    fn markdown(&mut self, node: &UiNode, area: Rect) {
        let text = content_text(node);
        let lines = markdown::parse(&sanitize_terminal_text(&text))
            .into_iter()
            .map(|line| {
                Line::from(
                    line.spans
                        .into_iter()
                        .map(|span| Span::styled(span.text, markdown_style(span.role, self.theme)))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((
                u16::try_from(self.scroll_offset(node)).unwrap_or(u16::MAX),
                0,
            ))
            .render(area, self.buffer);
    }

    fn code(&mut self, node: &UiNode, area: Rect) {
        let content = node.props.content.as_ref();
        let text = sanitize_terminal_text(&content_text(node));
        let language = content
            .and_then(|content| content.language.as_deref())
            .unwrap_or("text");
        let show_numbers = bool_attribute(node, "lineNumbers").unwrap_or(true);
        let lines = text.lines().collect::<Vec<_>>();
        let number_width = lines.len().max(1).to_string().len();
        let scroll = self.scroll_offset(node) as usize;
        let visible = lines.iter().skip(scroll).take(usize::from(area.height));
        for (row, line) in visible.enumerate() {
            let y = area.y.saturating_add(row as u16);
            if y >= area.bottom() {
                break;
            }
            let prefix = if show_numbers {
                format!("{:>number_width$} │ ", scroll + row + 1)
            } else {
                String::new()
            };
            let prefix_width = cell_width(&prefix).min(usize::from(area.width));
            self.buffer.set_stringn(
                area.x,
                y,
                prefix,
                prefix_width,
                Style::default().fg(self.theme.text.muted),
            );
            if prefix_width < usize::from(area.width) {
                self.highlight_code_line(
                    line,
                    language,
                    Rect::new(
                        area.x.saturating_add(prefix_width as u16),
                        y,
                        area.width.saturating_sub(prefix_width as u16),
                        1,
                    ),
                );
            }
        }
        self.scrollbar(
            area,
            lines.len() as u32,
            self.scroll_offset(node),
            area.height,
        );
    }

    fn highlight_code_line(&mut self, line: &str, language: &str, area: Rect) {
        // A small, total lexical presentation for live trees. Full fenced code
        // highlighting remains available through the Markdown parser; this path
        // intentionally avoids parsing untrusted code during every keystroke.
        let trimmed = line.trim_start();
        let style =
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--") {
                Style::default().fg(self.theme.syntax.comment)
            } else if matches!(language, "json" | "jsonc" | "toml" | "yaml" | "yml") {
                Style::default().fg(self.theme.syntax.string)
            } else {
                Style::default().fg(self.theme.text.primary)
            };
        self.buffer.set_stringn(
            area.x,
            area.y,
            truncate_cells(line, usize::from(area.width)),
            usize::from(area.width),
            style,
        );
    }

    fn diff(&mut self, node: &UiNode, area: Rect) {
        let text = sanitize_terminal_text(&content_text(node));
        let lines = text.lines().collect::<Vec<_>>();
        let scroll = self.scroll_offset(node) as usize;
        for (row, line) in lines
            .iter()
            .skip(scroll)
            .take(usize::from(area.height))
            .enumerate()
        {
            let (marker, style) = if line.starts_with("+++") || line.starts_with("---") {
                (' ', Style::default().fg(self.theme.diff.header))
            } else if line.starts_with('+') {
                ('+', Style::default().fg(self.theme.diff.added))
            } else if line.starts_with('-') {
                ('-', Style::default().fg(self.theme.diff.removed))
            } else if line.starts_with("@@") || line.starts_with("diff ") {
                (
                    ' ',
                    Style::default()
                        .fg(self.theme.diff.header)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (' ', Style::default().fg(self.theme.diff.context))
            };
            let rendered = if area.width < 32 {
                truncate_cells(line, usize::from(area.width.saturating_sub(2)))
            } else {
                line.to_string()
            };
            let y = area.y.saturating_add(row as u16);
            self.buffer
                .set_stringn(area.x, y, marker.to_string(), 1, style);
            self.buffer.set_stringn(
                area.x.saturating_add(2),
                y,
                rendered,
                usize::from(area.width.saturating_sub(2)),
                style,
            );
        }
        self.scrollbar(
            area,
            lines.len() as u32,
            self.scroll_offset(node),
            area.height,
        );
    }

    fn media(&mut self, node: &UiNode, area: Rect, image: bool) {
        let content = node.props.content.as_ref();
        let alt = content
            .and_then(|content| content.alternate_text.as_deref())
            .or_else(|| {
                node.props
                    .accessibility
                    .as_ref()
                    .and_then(|accessibility| accessibility.label.as_deref())
            })
            .unwrap_or(if image { "image" } else { "audio" });
        let uri = content
            .and_then(|content| content.resource.as_ref())
            .map(|resource| resource.uri.as_str());
        let icon = if self.capabilities.unicode {
            if image {
                "▧"
            } else {
                "♪"
            }
        } else if image {
            "[IMG]"
        } else {
            "[AUDIO]"
        };
        let protocol = self.capabilities.image_protocols.iter().next();
        let message = match (image, protocol, uri) {
            (true, Some(protocol), Some(uri)) => {
                self.diagnostic(
                    node.id.clone(),
                    DiagnosticSeverity::Info,
                    "remote-ui.external-image",
                    "image resource requires host-side protocol transport",
                );
                format!("{icon} {alt} ({protocol}: {})", safe_uri(uri))
            }
            (_, _, Some(uri)) => format!("{icon} {alt} — {}", safe_uri(uri)),
            _ => format!("{icon} {alt}"),
        };
        self.text_lines(&message, area, self.node_style(node), 0);
    }

    fn json_tree(&mut self, node: &UiNode, area: Rect) {
        let value = node
            .props
            .value
            .as_ref()
            .or_else(|| {
                node.props
                    .structured_data
                    .as_ref()
                    .and_then(|data| data.schema.as_ref())
            })
            .cloned()
            .unwrap_or(Value::Null);
        let mut lines = Vec::new();
        flatten_json(&value, "root", 0, self.capabilities.unicode, &mut lines);
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn log_viewer(&mut self, node: &UiNode, area: Rect) {
        let text = sanitize_terminal_text(&content_text(node));
        let filter = string_attribute(node, "filter").unwrap_or_default();
        let lines = text
            .lines()
            .filter(|line| filter.is_empty() || line.contains(&filter))
            .map(|line| line.to_owned())
            .collect::<Vec<_>>();
        let offset = self.scroll_offset(node) as usize;
        for (row, line) in lines
            .iter()
            .skip(offset)
            .take(usize::from(area.height))
            .enumerate()
        {
            let style = if contains_word(line, "error") || contains_word(line, "fatal") {
                Style::default().fg(self.theme.status.error)
            } else if contains_word(line, "warn") {
                Style::default().fg(self.theme.status.warning)
            } else if contains_word(line, "debug") || contains_word(line, "trace") {
                Style::default().fg(self.theme.text.muted)
            } else {
                self.base_style()
            };
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                truncate_cells(line, usize::from(area.width)),
                usize::from(area.width),
                style,
            );
        }
        self.scrollbar(
            area,
            lines.len() as u32,
            self.scroll_offset(node),
            area.height,
        );
    }

    fn list(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if !node.children.is_empty() {
            self.render_children_vertical(node, area, depth);
            return;
        }
        let lines = data_items(node)
            .iter()
            .enumerate()
            .map(|(index, item)| {
                format!(
                    "{} {}",
                    if self.capabilities.unicode {
                        "•"
                    } else {
                        "-"
                    },
                    item_label(item, index)
                )
            })
            .collect::<Vec<_>>();
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn virtual_list(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let offset = self.scroll_offset(node) as usize;
        let overscan = usize::from(self.options.virtual_overscan);
        let capacity = usize::from(area.height).saturating_add(overscan);
        if !node.children.is_empty() {
            for (row, child) in node.children.iter().skip(offset).take(capacity).enumerate() {
                if row >= usize::from(area.height) {
                    break;
                }
                self.node(
                    child,
                    Rect::new(area.x, area.y.saturating_add(row as u16), area.width, 1),
                    depth + 1,
                );
            }
            self.scrollbar(area, node.children.len() as u32, offset as u32, area.height);
            return;
        }
        let items = data_items(node);
        for (row, item) in items.iter().skip(offset).take(capacity).enumerate() {
            if row >= usize::from(area.height) {
                break;
            }
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                truncate_cells(&item_label(item, offset + row), usize::from(area.width)),
                usize::from(area.width),
                self.base_style(),
            );
        }
        self.scrollbar(area, items.len() as u32, offset as u32, area.height);
    }

    fn table(&mut self, node: &UiNode, area: Rect) {
        let Some(data) = node.props.structured_data.as_ref() else {
            self.text_lines("[empty table]", area, self.muted_style(), 0);
            return;
        };
        if data.columns.is_empty() {
            let lines = data
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| item_label(item, index))
                .collect::<Vec<_>>();
            self.draw_string_lines(node, &lines, area, self.base_style());
            return;
        }
        if area.width < 42 {
            self.stacked_table(node, area, data);
            return;
        }
        let separator_width = data.columns.len().saturating_sub(1) * 3;
        let available = usize::from(area.width).saturating_sub(separator_width);
        let declared = data
            .columns
            .iter()
            .map(|column| {
                column
                    .width
                    .as_ref()
                    .and_then(|dimension| layout::resolve_dimension(dimension, area.width))
                    .map(usize::from)
                    .unwrap_or_else(|| cell_width(&column.label).max(4))
            })
            .collect::<Vec<_>>();
        let widths = fit_column_widths(&declared, available);
        let header = data
            .columns
            .iter()
            .zip(&widths)
            .map(|(column, width)| pad_cells(&column.label, *width))
            .collect::<Vec<_>>()
            .join(" │ ");
        self.buffer.set_stringn(
            area.x,
            area.y,
            header,
            usize::from(area.width),
            Style::default()
                .fg(self.theme.text.heading)
                .add_modifier(Modifier::BOLD),
        );
        if area.height > 1 {
            let rule = widths
                .iter()
                .map(|width| "─".repeat(*width))
                .collect::<Vec<_>>()
                .join("─┼─");
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(1),
                rule,
                usize::from(area.width),
                Style::default().fg(self.theme.surface.border),
            );
        }
        let offset = self.scroll_offset(node) as usize;
        for (row, item) in data
            .items
            .iter()
            .skip(offset)
            .take(usize::from(area.height.saturating_sub(2)))
            .enumerate()
        {
            let cells = data
                .columns
                .iter()
                .zip(&widths)
                .map(|(column, width)| {
                    let value = item
                        .as_object()
                        .and_then(|object| object.get(&column.id))
                        .map(value_text)
                        .unwrap_or_default();
                    pad_cells(&value, *width)
                })
                .collect::<Vec<_>>()
                .join(" │ ");
            let selected =
                item_id(item).is_some_and(|id| data.selected_ids.iter().any(|item| item == id));
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(2 + row as u16),
                cells,
                usize::from(area.width),
                if selected {
                    self.theme.selection_style()
                } else {
                    self.base_style()
                },
            );
        }
        self.scrollbar(
            area,
            data.items.len() as u32 + 2,
            self.scroll_offset(node),
            area.height,
        );
    }

    fn stacked_table(&mut self, node: &UiNode, area: Rect, data: &UiData) {
        let offset = self.scroll_offset(node) as usize;
        let mut lines = Vec::new();
        for (index, item) in data.items.iter().enumerate() {
            if index > 0 {
                lines.push("".to_owned());
            }
            let object = item.as_object();
            for column in &data.columns {
                let value = object
                    .and_then(|object| object.get(&column.id))
                    .map(value_text)
                    .unwrap_or_default();
                lines.push(format!("{}: {value}", column.label));
            }
        }
        for (row, line) in lines
            .iter()
            .skip(offset)
            .take(usize::from(area.height))
            .enumerate()
        {
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                truncate_cells(line, usize::from(area.width)),
                usize::from(area.width),
                if line.contains(':') {
                    self.base_style()
                } else {
                    self.muted_style()
                },
            );
        }
        self.scrollbar(area, lines.len() as u32, offset as u32, area.height);
    }

    fn tree(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if !node.children.is_empty() {
            self.render_tree_children(node, area, depth, 0);
            return;
        }
        let mut lines = Vec::new();
        for (index, item) in data_items(node).iter().enumerate() {
            let object = item.as_object();
            let item_depth = object
                .and_then(|value| value.get("depth"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let expanded = object
                .and_then(|value| value.get("expanded"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let branch = if expanded {
                if self.capabilities.unicode {
                    "▾"
                } else {
                    "v"
                }
            } else if self.capabilities.unicode {
                "▸"
            } else {
                ">"
            };
            lines.push(format!(
                "{}{} {}",
                "  ".repeat(item_depth.min(12)),
                branch,
                item_label(item, index)
            ));
        }
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn render_tree_children(&mut self, node: &UiNode, area: Rect, depth: u16, indent: u16) {
        let mut row = 0_u16;
        for child in &node.children {
            if row >= area.height {
                break;
            }
            let label = component_label(child);
            let expanded = child
                .id
                .as_ref()
                .and_then(|id| self.state.expanded.get(id).copied())
                .or_else(|| child.props.navigation.as_ref().and_then(|nav| nav.expanded))
                .unwrap_or(!child.children.is_empty());
            let marker = if child.children.is_empty() {
                if self.capabilities.unicode {
                    "•"
                } else {
                    "-"
                }
            } else if expanded {
                if self.capabilities.unicode {
                    "▾"
                } else {
                    "v"
                }
            } else if self.capabilities.unicode {
                "▸"
            } else {
                ">"
            };
            let x = area
                .x
                .saturating_add(indent.saturating_mul(2))
                .min(area.right());
            self.buffer.set_stringn(
                x,
                area.y.saturating_add(row),
                format!("{marker} {label}"),
                usize::from(area.right().saturating_sub(x)),
                self.base_style(),
            );
            self.register_interaction(
                child,
                Rect::new(
                    x,
                    area.y.saturating_add(row),
                    area.right().saturating_sub(x),
                    1,
                ),
                "treeitem",
            );
            row += 1;
            if expanded && !child.children.is_empty() && row < area.height {
                let nested = Rect::new(
                    area.x,
                    area.y.saturating_add(row),
                    area.width,
                    area.height - row,
                );
                // Recursive children use their own renderer when they carry
                // semantics, keeping tree focus/hit metadata intact.
                for nested_child in &child.children {
                    if row >= area.height {
                        break;
                    }
                    self.node(
                        nested_child,
                        Rect::new(
                            nested.x.saturating_add((indent + 1).saturating_mul(2)),
                            area.y.saturating_add(row),
                            nested.width.saturating_sub((indent + 1).saturating_mul(2)),
                            1,
                        ),
                        depth + 1,
                    );
                    row += 1;
                }
            }
        }
    }

    fn key_value(&mut self, node: &UiNode, area: Rect) {
        let value = node
            .props
            .value
            .as_ref()
            .or_else(|| {
                node.props
                    .structured_data
                    .as_ref()
                    .and_then(|data| data.schema.as_ref())
            })
            .cloned()
            .unwrap_or(Value::Null);
        let lines = match value {
            Value::Object(object) => object
                .into_iter()
                .map(|(key, value)| format!("{key}: {}", value_text(&value)))
                .collect::<Vec<_>>(),
            value => vec![value_text(&value)],
        };
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn timeline(&mut self, node: &UiNode, area: Rect) {
        let lines = data_items(node)
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let object = item.as_object();
                let time = object
                    .and_then(|object| object.get("time").or_else(|| object.get("timestamp")))
                    .map(value_text)
                    .unwrap_or_default();
                let label = item_label(item, index);
                let marker = if self.capabilities.unicode {
                    "●"
                } else {
                    "*"
                };
                if time.is_empty() {
                    format!("{marker} {label}")
                } else {
                    format!("{time} {marker} {label}")
                }
            })
            .collect::<Vec<_>>();
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn graph(&mut self, node: &UiNode, area: Rect) {
        // Fidelity ladder: layered box-drawing diagram → adjacency text list.
        // (The card-list rung lives in the producer-supplied node fallback.)
        if area.width >= self.options.narrow_breakpoint {
            if let Some(diagram) = layout_graph_diagram(node, area, self.capabilities.unicode) {
                self.graph_diagram(node, &diagram, area);
                return;
            }
        }
        self.graph_adjacency_list(node, area);
    }

    fn graph_diagram(&mut self, node: &UiNode, diagram: &GraphDiagram, area: Rect) {
        let selected = node
            .props
            .structured_data
            .as_ref()
            .map_or(&[][..], |data| data.selected_ids.as_slice());
        // Connectors first; node labels then overwrite their own cells so a
        // pass-through edge never bleeds into a label.
        let connector_style = Style::default()
            .fg(self.theme.surface.border)
            .bg(self.theme.surface.background);
        for (offset, mask) in diagram.arms.iter().enumerate() {
            let Some(glyph) = arm_glyph(*mask, self.capabilities.unicode) else {
                continue;
            };
            let x = area.x.saturating_add((offset % diagram.width) as u16);
            let y = area.y.saturating_add((offset / diagram.width) as u16);
            if x < area.right() && y < area.bottom() {
                self.buffer.set_stringn(x, y, glyph, 1, connector_style);
            }
        }
        for placement in &diagram.placements {
            let x = area.x.saturating_add(placement.x);
            let y = area.y.saturating_add(placement.y);
            if y >= area.bottom() {
                continue;
            }
            let style = if placement
                .id
                .as_deref()
                .is_some_and(|id| selected.iter().any(|selected| selected == id))
            {
                self.theme.selection_style()
            } else {
                placement.status.as_deref().map_or_else(
                    || self.base_style(),
                    |status| tone_from_name(status, self.theme),
                )
            };
            self.buffer.set_stringn(
                x,
                y,
                &placement.label,
                usize::from(area.right().saturating_sub(x)),
                style,
            );
        }
    }

    fn graph_adjacency_list(&mut self, node: &UiNode, area: Rect) {
        let arrow = if self.capabilities.unicode {
            " → "
        } else {
            " -> "
        };
        let items = data_items(node);
        let edges = graph_edges(node);
        let mut lines = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let label = item_label(item, index);
            let id = graph_item_key(item, index);
            let targets = edges
                .iter()
                .filter(|(from, _)| *from == id)
                .map(|(_, to)| {
                    graph_item_index(items, to)
                        .map_or_else(|| to.clone(), |target| item_label(&items[target], target))
                })
                .collect::<Vec<_>>()
                .join(", ");
            if targets.is_empty() {
                lines.push(format!(
                    "{} {label}",
                    if self.capabilities.unicode {
                        "○"
                    } else {
                        "o"
                    }
                ));
            } else {
                lines.push(format!("{label}{arrow}{targets}"));
            }
        }
        if lines.is_empty() {
            lines.push("[empty graph]".to_owned());
        }
        self.draw_string_lines(node, &lines, area, self.base_style());
    }

    fn chart(&mut self, node: &UiNode, area: Rect, sparkline: bool) {
        let values = numeric_series(node);
        if values.is_empty() {
            self.text_lines("[no chart data]", area, self.muted_style(), 0);
            return;
        }
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let range = (maximum - minimum).max(f64::EPSILON);
        if sparkline || area.height <= 2 {
            let unicode = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
            let ascii = [".", ":", "-", "=", "+", "*", "#", "@"];
            let glyphs = if self.capabilities.unicode {
                &unicode
            } else {
                &ascii
            };
            let line = values
                .iter()
                .take(usize::from(area.width))
                .map(|value| {
                    let level =
                        (((value - minimum) / range) * 7.0).round().clamp(0.0, 7.0) as usize;
                    glyphs[level]
                })
                .collect::<String>();
            self.buffer.set_stringn(
                area.x,
                area.y,
                line,
                usize::from(area.width),
                Style::default().fg(self.theme.status.info),
            );
            return;
        }
        let bar_width = area.width.saturating_sub(8).max(1);
        for (row, value) in values.iter().take(usize::from(area.height)).enumerate() {
            let normalized = ((value - minimum) / range).clamp(0.0, 1.0);
            let length = (normalized * f64::from(bar_width)).round() as usize;
            let bar = if self.capabilities.unicode {
                "█"
            } else {
                "#"
            }
            .repeat(length);
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                format!("{:>6.1} {bar}", value),
                usize::from(area.width),
                Style::default().fg(self.theme.status.info),
            );
        }
    }

    fn badge(&mut self, node: &UiNode, area: Rect) {
        let label = component_label(node);
        let text = if self.capabilities.unicode {
            format!("‹{label}›")
        } else {
            format!("[{label}]")
        };
        self.buffer.set_stringn(
            area.x,
            area.y,
            truncate_cells(&text, usize::from(area.width)),
            usize::from(area.width),
            self.tone_style(node),
        );
    }

    fn progress(&mut self, node: &UiNode, area: Rect) {
        let feedback = node.props.feedback.as_ref();
        let current = feedback
            .and_then(|feedback| feedback.current)
            .unwrap_or(0.0);
        let maximum = feedback
            .and_then(|feedback| feedback.maximum)
            .filter(|value| *value > 0.0)
            .unwrap_or(100.0);
        let indeterminate = feedback
            .and_then(|feedback| feedback.indeterminate)
            .unwrap_or(false);
        let label = feedback
            .and_then(|feedback| feedback.message.as_deref())
            .unwrap_or("Progress");
        let suffix_width = 7_u16;
        let bar_width = area.width.saturating_sub(suffix_width).max(1);
        let ratio = if indeterminate {
            0.35
        } else {
            (current / maximum).clamp(0.0, 1.0)
        };
        let fill = (ratio * f64::from(bar_width)).round() as usize;
        let (filled, empty) = if self.capabilities.unicode {
            ("█", "░")
        } else {
            ("#", "-")
        };
        let bar = format!(
            "{}{}",
            filled.repeat(fill),
            empty.repeat(usize::from(bar_width).saturating_sub(fill))
        );
        let percent = if indeterminate {
            " … ".to_owned()
        } else {
            format!(" {:>3}%", (ratio * 100.0).round() as u16)
        };
        self.buffer.set_stringn(
            area.x,
            area.y,
            format!("{bar}{percent}"),
            usize::from(area.width),
            self.tone_style(node),
        );
        if area.height > 1 {
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(1),
                truncate_cells(label, usize::from(area.width)),
                usize::from(area.width),
                self.muted_style(),
            );
        }
    }

    fn spinner(&mut self, node: &UiNode, area: Rect) {
        // Deliberately revision/state deterministic: animation belongs to the
        // host clock and reduced-motion policy, never to a pure render call.
        let symbol = if self.capabilities.unicode {
            "◌"
        } else {
            "*"
        };
        let label = node
            .props
            .feedback
            .as_ref()
            .and_then(|feedback| feedback.message.as_deref())
            .unwrap_or("Working");
        self.buffer.set_stringn(
            area.x,
            area.y,
            format!("{symbol} {label}"),
            usize::from(area.width),
            Style::default().fg(self.theme.status.running),
        );
    }

    fn feedback_panel(&mut self, node: &UiNode, area: Rect, depth: u16, primitive: &str) {
        let title = match primitive {
            primitives::ALERT => "Alert",
            primitives::TOAST => "Notification",
            primitives::EMPTY_STATE => "Empty",
            _ => "Status",
        };
        self.draw_border(node, area, true, Some(title));
        let inner = layout::content_box(area, node.props.layout.as_ref(), true);
        let message = node
            .props
            .feedback
            .as_ref()
            .and_then(|feedback| feedback.message.as_deref())
            .or_else(|| {
                node.props
                    .content
                    .as_ref()
                    .and_then(|content| content.text.as_deref())
            })
            .unwrap_or(if primitive == primitives::EMPTY_STATE {
                "Nothing to show."
            } else {
                ""
            });
        let message_height = wrap_cells(message, usize::from(inner.width)).len() as u16;
        self.text_lines(message, inner, self.tone_style(node), 0);
        if !node.children.is_empty() && message_height < inner.height {
            self.render_children_vertical(
                node,
                Rect::new(
                    inner.x,
                    inner.y.saturating_add(message_height),
                    inner.width,
                    inner.height.saturating_sub(message_height),
                ),
                depth,
            );
        }
    }

    fn error_boundary(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if node.children.is_empty() {
            if let Some(fallback) = node.fallback.as_deref() {
                self.node(fallback, area, depth + 1);
            } else {
                self.diagnostic(
                    node.id.clone(),
                    DiagnosticSeverity::Warning,
                    "remote-ui.empty-error-boundary",
                    "error boundary had neither children nor fallback",
                );
                self.text_lines("[component unavailable]", area, self.error_style(), 0);
            }
        } else {
            self.render_children_vertical(node, area, depth);
        }
    }

    fn tabs(&mut self, node: &UiNode, area: Rect, depth: u16) {
        let items = data_items(node);
        let selected = self.selected_index(node).unwrap_or_else(|| {
            items
                .iter()
                .position(|item| {
                    item.as_object()
                        .and_then(|object| object.get("selected"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .unwrap_or(0)
        });
        let mut x = area.x;
        for (index, item) in items.iter().enumerate() {
            let label = item_label(item, index);
            let rendered = if index == selected {
                format!("[{label}]")
            } else {
                format!(" {label} ")
            };
            let remaining = area.right().saturating_sub(x);
            if remaining == 0 {
                break;
            }
            let clipped = truncate_cells(&rendered, usize::from(remaining));
            let width = cell_width(&clipped) as u16;
            self.buffer.set_stringn(
                x,
                area.y,
                clipped,
                usize::from(remaining),
                if index == selected {
                    self.theme.selection_style()
                } else {
                    self.base_style()
                },
            );
            x = x.saturating_add(width).saturating_add(1);
        }
        self.register_interaction(node, Rect::new(area.x, area.y, area.width, 1), "tablist");
        if !node.children.is_empty() && area.height > 1 {
            let child = node
                .children
                .get(selected)
                .or_else(|| node.children.first());
            if let Some(child) = child {
                self.node(
                    child,
                    Rect::new(
                        area.x,
                        area.y.saturating_add(1),
                        area.width,
                        area.height - 1,
                    ),
                    depth + 1,
                );
            }
        }
    }

    fn breadcrumb(&mut self, node: &UiNode, area: Rect) {
        let separator = if self.capabilities.unicode {
            " › "
        } else {
            " / "
        };
        let text = data_items(node)
            .iter()
            .enumerate()
            .map(|(index, item)| item_label(item, index))
            .collect::<Vec<_>>()
            .join(separator);
        self.text_lines(&text, area, self.node_style(node), 0);
        self.register_interaction(node, area, "navigation");
    }

    fn menu(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if !node.children.is_empty() {
            self.render_children_vertical(node, area, depth);
        } else {
            let selected = self.selected_index(node).unwrap_or(0);
            for (row, item) in data_items(node)
                .iter()
                .take(usize::from(area.height))
                .enumerate()
            {
                let label = item_label(item, row);
                let disabled = item
                    .as_object()
                    .and_then(|object| object.get("disabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let prefix = if row == selected {
                    if self.capabilities.unicode {
                        "› "
                    } else {
                        "> "
                    }
                } else {
                    "  "
                };
                self.buffer.set_stringn(
                    area.x,
                    area.y.saturating_add(row as u16),
                    format!("{prefix}{label}"),
                    usize::from(area.width),
                    if row == selected {
                        self.theme.selection_style()
                    } else if disabled {
                        self.muted_style().add_modifier(Modifier::DIM)
                    } else {
                        self.base_style()
                    },
                );
            }
        }
        self.register_interaction(node, area, "menu");
    }

    fn pagination(&mut self, node: &UiNode, area: Rect) {
        let page = number_attribute(node, "page").unwrap_or(1.0).max(1.0) as u64;
        let pages = number_attribute(node, "pages").unwrap_or(1.0).max(1.0) as u64;
        let text = format!("‹  Page {page} of {pages}  ›");
        self.buffer.set_stringn(
            area.x,
            area.y,
            text,
            usize::from(area.width),
            self.node_style(node),
        );
        self.register_interaction(node, Rect::new(area.x, area.y, area.width, 1), "navigation");
    }

    fn link(&mut self, node: &UiNode, area: Rect) {
        let label = component_label(node);
        let destination = node
            .props
            .navigation
            .as_ref()
            .and_then(|navigation| navigation.destination.as_deref());
        let rendered = if label.is_empty() {
            destination
                .map(safe_uri)
                .unwrap_or_else(|| "link".to_owned())
        } else {
            label
        };
        self.buffer.set_stringn(
            area.x,
            area.y,
            truncate_cells(&rendered, usize::from(area.width)),
            usize::from(area.width),
            Style::default()
                .fg(self.theme.focus.active)
                .add_modifier(Modifier::UNDERLINED),
        );
        self.register_interaction(node, Rect::new(area.x, area.y, area.width, 1), "link");
    }

    fn input(&mut self, node: &UiNode, area: Rect, primitive: &str) {
        let input = node.props.input.as_ref();
        let value = self.input_value(node);
        let disabled = input.is_some_and(|input| input.disabled);
        let read_only = input.is_some_and(|input| input.read_only);
        let label = component_label(node);
        let focused = node
            .id
            .as_ref()
            .is_some_and(|id| self.state.focused_node.as_ref() == Some(id));
        let style = if focused {
            self.theme.selection_style()
        } else if disabled {
            self.muted_style().add_modifier(Modifier::DIM)
        } else {
            self.node_style(node)
        };
        let rendered = match primitive {
            primitives::CHECKBOX => format!(
                "{} {}",
                if value.as_bool().unwrap_or(false) {
                    "[x]"
                } else {
                    "[ ]"
                },
                label
            ),
            primitives::RADIO => format!(
                "{} {}",
                if value.as_bool().unwrap_or(false) {
                    "(o)"
                } else {
                    "( )"
                },
                label
            ),
            primitives::SELECT | primitives::MULTI_SELECT => {
                let selected = selected_option_labels(node, &value);
                format!(
                    "{} [{}]",
                    if label.is_empty() { "Select" } else { &label },
                    selected
                )
            }
            primitives::TEXT_AREA => {
                let text = value.as_str().unwrap_or_default();
                self.draw_border(node, area, true, Some(&label));
                let inner = layout::content_box(area, None, true);
                self.text_lines(
                    if text.is_empty() {
                        input
                            .and_then(|input| input.placeholder.as_deref())
                            .unwrap_or_default()
                    } else {
                        text
                    },
                    inner,
                    if text.is_empty() {
                        self.muted_style()
                    } else {
                        style
                    },
                    self.scroll_offset(node),
                );
                self.register_input(node, area, primitive, value, disabled, read_only);
                return;
            }
            _ => {
                let text = value.as_str().unwrap_or_default();
                let display = if text.is_empty() {
                    input
                        .and_then(|input| input.placeholder.as_deref())
                        .unwrap_or_default()
                } else {
                    text
                };
                if label.is_empty() {
                    format!("[ {display} ]")
                } else {
                    format!("{label}: [ {display} ]")
                }
            }
        };
        self.buffer.set_stringn(
            area.x,
            area.y,
            truncate_cells(&rendered, usize::from(area.width)),
            usize::from(area.width),
            style,
        );
        if let Some(message) = input.and_then(|input| input.validation_message.as_deref()) {
            if area.height > 1 {
                self.buffer.set_stringn(
                    area.x,
                    area.y.saturating_add(1),
                    truncate_cells(message, usize::from(area.width)),
                    usize::from(area.width),
                    self.error_style(),
                );
            }
        }
        self.register_input(node, area, primitive, value, disabled, read_only);
    }

    fn register_input(
        &mut self,
        node: &UiNode,
        area: Rect,
        primitive: &str,
        value: Value,
        disabled: bool,
        read_only: bool,
    ) {
        if let (Some(id), Some(input)) = (&node.id, node.props.input.as_ref()) {
            self.output.form_fields.push(FormFieldDescriptor {
                node_id: id.clone(),
                name: input.name.clone().unwrap_or_else(|| id.as_str().to_owned()),
                input_type: input
                    .input_type
                    .clone()
                    .unwrap_or_else(|| primitive.to_owned()),
                value,
                required: input.required,
                read_only,
                disabled,
                validation_message: input.validation_message.clone(),
            });
        }
        self.register_interaction(node, area, inferred_role(primitive).as_str());
    }

    fn form(&mut self, node: &UiNode, area: Rect, depth: u16) {
        self.draw_border(node, area, self.wants_border(node), Some("Form"));
        let inner = layout::content_box(area, node.props.layout.as_ref(), self.wants_border(node));
        self.render_children_vertical(node, inner, depth);
        self.register_interaction(node, area, "form");
    }

    fn button(&mut self, node: &UiNode, area: Rect) {
        let label = component_label(node);
        let disabled = node_disabled(node);
        let focused = node
            .id
            .as_ref()
            .is_some_and(|id| self.state.focused_node.as_ref() == Some(id));
        let brackets = if self.capabilities.unicode {
            ("▐ ", " ▌")
        } else {
            ("[ ", " ]")
        };
        let text = format!("{}{label}{}", brackets.0, brackets.1);
        let style = if disabled {
            self.muted_style().add_modifier(Modifier::DIM)
        } else if focused {
            self.theme.selection_style()
        } else {
            self.tone_style(node).add_modifier(Modifier::BOLD)
        };
        self.buffer.set_stringn(
            area.x,
            area.y,
            truncate_cells(&text, usize::from(area.width)),
            usize::from(area.width),
            style,
        );
        self.register_interaction(node, Rect::new(area.x, area.y, area.width, 1), "button");
    }

    fn action_menu(&mut self, node: &UiNode, area: Rect, depth: u16) {
        if node.children.is_empty() {
            let label = component_label(node);
            self.buffer.set_stringn(
                area.x,
                area.y,
                format!("{label} ▾"),
                usize::from(area.width),
                self.node_style(node),
            );
        } else {
            self.menu(node, area, depth);
        }
        self.register_interaction(node, area, "menu");
    }

    fn toolbar(&mut self, node: &UiNode, area: Rect, depth: u16) {
        self.row_container(node, area, depth);
        self.register_interaction(node, area, "toolbar");
    }

    fn domain_card(&mut self, node: &UiNode, area: Rect, depth: u16, primitive: &str) {
        let title = string_attribute(node, "title")
            .or_else(|| string_attribute(node, "name"))
            .unwrap_or_else(|| split_camel_case(domain_suffix(primitive)));
        self.draw_border(node, area, true, Some(&title));
        let inner = layout::content_box(area, node.props.layout.as_ref(), true);
        let status_owned = string_attribute(node, "status");
        let status = node
            .props
            .feedback
            .as_ref()
            .and_then(|feedback| feedback.status.as_deref())
            .or(status_owned.as_deref());
        let mut top = inner.y;
        if let Some(status) = status {
            self.buffer.set_stringn(
                inner.x,
                top,
                format!(
                    "{} {status}",
                    status_symbol(status, self.capabilities.unicode)
                ),
                usize::from(inner.width),
                tone_from_name(status, self.theme),
            );
            top = top.saturating_add(1);
        }
        let summary_owned = string_attribute(node, "description");
        if let Some(summary) = node
            .props
            .content
            .as_ref()
            .and_then(|content| content.text.as_deref())
            .or(summary_owned.as_deref())
        {
            let summary_area = Rect::new(
                inner.x,
                top,
                inner.width,
                inner.bottom().saturating_sub(top),
            );
            self.text_lines(summary, summary_area, self.base_style(), 0);
            top = top.saturating_add(
                wrap_cells(summary, usize::from(inner.width))
                    .len()
                    .min(usize::from(summary_area.height)) as u16,
            );
        }
        if !node.children.is_empty() && top < inner.bottom() {
            self.render_children_vertical(
                node,
                Rect::new(inner.x, top, inner.width, inner.bottom() - top),
                depth,
            );
        } else if let Some(value) = node.props.value.as_ref() {
            let lines = value_summary(value);
            self.draw_string_lines(
                node,
                &lines,
                Rect::new(
                    inner.x,
                    top,
                    inner.width,
                    inner.bottom().saturating_sub(top),
                ),
                self.muted_style(),
            );
        }
        self.register_interaction(node, area, "article");
    }

    fn custom(&mut self, node: &UiNode, area: Rect, depth: u16, primitive: &str) {
        // Custom primitives explicitly advertised by the client still receive
        // a useful generic representation: semantic title/value plus children.
        self.diagnostic(
            node.id.clone(),
            DiagnosticSeverity::Info,
            "remote-ui.generic-custom-renderer",
            format!("rendered custom primitive {primitive} as a semantic card"),
        );
        self.draw_border(node, area, true, Some(primitive));
        let inner = layout::content_box(area, node.props.layout.as_ref(), true);
        if !node.children.is_empty() {
            self.render_children_vertical(node, inner, depth);
        } else {
            let text = if !content_text(node).is_empty() {
                content_text(node)
            } else if let Some(value) = node.props.value.as_ref() {
                value_text(value)
            } else {
                format!("[{primitive}]")
            };
            self.text_lines(&text, inner, self.base_style(), 0);
        }
        self.register_interaction(node, area, "group");
    }

    fn measure(&self, original: &UiNode, width: u16, depth: u16) -> u16 {
        if width == 0 || depth > self.options.limits.max_tree_depth {
            return 0;
        }
        // The frame borrows one immutable document, so the node address plus
        // the available width and depth identify a measurement for the frame.
        let key = (std::ptr::from_ref(original) as usize, width, depth);
        if let Some(cached) = self.measure_cache.borrow().get(&key) {
            return *cached;
        }
        let measured = self.measure_uncached(original, width, depth);
        self.measure_cache.borrow_mut().insert(key, measured);
        measured
    }

    fn measure_uncached(&self, original: &UiNode, width: u16, depth: u16) -> u16 {
        let node = match resolve_node(original, self.capabilities) {
            ResolvedNode::Plain(text) => {
                return bounded_height(wrap_cells(&text, usize::from(width)).len())
            }
            ResolvedNode::Node(node) => node,
        };
        if node.kind.as_str() == node_kinds::TEXT {
            return bounded_height(
                wrap_cells(node.text.as_deref().unwrap_or_default(), usize::from(width)).len(),
            );
        }
        let primitive = node.node_type.as_ref().map_or("", |value| value.as_str());
        let layout = node.props.layout.as_ref();
        if let Some(explicit) = layout
            .and_then(|layout| layout.height.as_ref())
            .and_then(|dimension| layout::resolve_dimension(dimension, MAX_MEASURED_HEIGHT))
        {
            return explicit.min(MAX_MEASURED_HEIGHT);
        }
        let border = u16::from(self.wants_border(node)) * 2;
        let padding = layout.and_then(|layout| layout.padding).map_or(0, |edges| {
            edge_cells(edges.top).saturating_add(edge_cells(edges.bottom))
        });
        let inner_width = width.saturating_sub(border).max(1);
        let content = match primitive {
            primitives::SPACER => 1,
            primitives::TEXT | primitives::MARKDOWN => {
                bounded_height(wrap_cells(&content_text(node), usize::from(inner_width)).len())
            }
            primitives::CODE
            | primitives::DIFF
            | primitives::LOG_VIEWER
            | primitives::JSON_TREE
            | primitives::KEY_VALUE => bounded_height(
                content_text(node)
                    .lines()
                    .count()
                    .max(value_line_count(node)),
            ),
            primitives::IMAGE | primitives::AUDIO | primitives::BADGE | primitives::SPINNER => 1,
            primitives::PROGRESS => 2,
            primitives::TEXT_INPUT
            | primitives::SELECT
            | primitives::MULTI_SELECT
            | primitives::CHECKBOX
            | primitives::RADIO
            | primitives::BUTTON
            | primitives::LINK
            | primitives::BREADCRUMB
            | primitives::PAGINATION => {
                if node
                    .props
                    .input
                    .as_ref()
                    .and_then(|input| input.validation_message.as_ref())
                    .is_some()
                {
                    2
                } else {
                    1
                }
            }
            primitives::TEXT_AREA => 4,
            primitives::TABLE => {
                let rows = node
                    .props
                    .structured_data
                    .as_ref()
                    .map_or(0, |data| data.items.len());
                if width < 42 {
                    bounded_height(rows.saturating_mul(3))
                } else {
                    bounded_height(rows.saturating_add(2))
                }
            }
            primitives::LIST
            | primitives::VIRTUAL_LIST
            | primitives::TREE
            | primitives::TIMELINE
            | primitives::MENU
            | primitives::COMMAND_LIST => {
                if node.children.is_empty() {
                    bounded_height(data_items(node).len().max(1))
                } else {
                    vertical_measure(self, &node.children, inner_width, depth + 1, layout)
                }
            }
            primitives::GRAPH => {
                if inner_width >= self.options.narrow_breakpoint {
                    graph_diagram_height(node, inner_width, self.capabilities.unicode)
                        .unwrap_or_else(|| bounded_height(data_items(node).len().max(1)))
                } else {
                    bounded_height(data_items(node).len().max(1))
                }
            }
            primitives::CHART => data_items(node).len().clamp(1, 8) as u16,
            primitives::SPARKLINE => 1,
            primitives::TABS => {
                let selected = self.selected_index(node).unwrap_or(0);
                1_u16.saturating_add(
                    node.children
                        .get(selected)
                        .or_else(|| node.children.first())
                        .map_or(0, |child| self.measure(child, inner_width, depth + 1)),
                )
            }
            primitives::SPLIT if split_is_vertical(node) => {
                vertical_measure(self, &node.children, inner_width, depth + 1, layout).max(1)
            }
            primitives::ROW | primitives::TOOLBAR | primitives::SPLIT
                if width >= self.options.narrow_breakpoint =>
            {
                node.children
                    .iter()
                    .map(|child| self.measure(child, inner_width, depth + 1))
                    .max()
                    .unwrap_or(1)
            }
            primitives::GRID if width >= self.options.narrow_breakpoint => {
                let columns = layout
                    .map(|layout| layout.columns.len())
                    .filter(|count| *count > 0)
                    .unwrap_or_else(|| usize::from((width / 24).max(1)));
                let heights = node
                    .children
                    .chunks(columns.max(1))
                    .map(|row| {
                        row.iter()
                            .map(|child| {
                                self.measure(child, inner_width / columns as u16, depth + 1)
                            })
                            .max()
                            .unwrap_or(1)
                    })
                    .fold(0_u16, u16::saturating_add);
                heights.saturating_add(
                    layout::gap(layout, Axis::Vertical).saturating_mul(
                        node.children.len().div_ceil(columns).saturating_sub(1) as u16,
                    ),
                )
            }
            primitives::ALERT | primitives::TOAST | primitives::EMPTY_STATE => {
                let message = node
                    .props
                    .feedback
                    .as_ref()
                    .and_then(|feedback| feedback.message.as_deref())
                    .unwrap_or_default();
                bounded_height(wrap_cells(message, usize::from(inner_width)).len()).saturating_add(
                    vertical_measure(self, &node.children, inner_width, depth + 1, layout),
                )
            }
            _ => vertical_measure(self, &node.children, inner_width, depth + 1, layout).max(1),
        };
        content
            .saturating_add(border)
            .saturating_add(padding)
            .min(MAX_MEASURED_HEIGHT)
    }

    fn measure_width(&self, node: &UiNode, parent: u16) -> u16 {
        if let Some(width) = node
            .props
            .layout
            .as_ref()
            .and_then(|layout| layout.width.as_ref())
            .and_then(|dimension| layout::resolve_dimension(dimension, parent))
        {
            return width;
        }
        let label = component_label(node);
        (cell_width(&label) as u16)
            .saturating_add(4)
            .clamp(1, parent)
    }

    fn wants_border(&self, node: &UiNode) -> bool {
        node.props
            .style
            .as_ref()
            .and_then(|style| style.border_style.as_deref())
            .is_some_and(|border| border != "none" && border != "hidden")
    }

    fn draw_border(&mut self, node: &UiNode, area: Rect, border: bool, title: Option<&str>) {
        if !border || area.width < 2 || area.height < 2 {
            return;
        }
        let border_type = if !self.capabilities.unicode {
            BorderType::Plain
        } else {
            match node
                .props
                .style
                .as_ref()
                .and_then(|style| style.border_style.as_deref())
            {
                Some("double") => BorderType::Double,
                Some("thick") => BorderType::Thick,
                Some("quadrantInside") => BorderType::QuadrantInside,
                _ => BorderType::Rounded,
            }
        };
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(border_type)
            .border_style(self.border_style(node))
            .style(self.node_style(node));
        if let Some(title) = title.filter(|title| !title.is_empty()) {
            block = block.title(truncate_cells(
                title,
                usize::from(area.width.saturating_sub(4)),
            ));
        }
        block.render(area, self.buffer);
    }

    fn base_style(&self) -> Style {
        Style::default()
            .fg(self.theme.text.primary)
            .bg(self.theme.surface.background)
    }

    fn muted_style(&self) -> Style {
        Style::default()
            .fg(self.theme.text.muted)
            .bg(self.theme.surface.background)
    }

    fn error_style(&self) -> Style {
        Style::default()
            .fg(self.theme.status.error)
            .bg(self.theme.surface.background)
    }

    fn node_style(&self, node: &UiNode) -> Style {
        node.props
            .style
            .as_ref()
            .map_or_else(|| self.base_style(), |style| self.semantic_style(style))
    }

    fn semantic_style(&self, semantic: &UiStyle) -> Style {
        let mut style = self.base_style();
        if let Some(foreground) = semantic.foreground.as_deref() {
            style = style.fg(token_color(foreground, self.theme));
        }
        if let Some(background) = semantic.background.as_deref() {
            style = style.bg(token_color(background, self.theme));
        }
        if let Some(tone) = semantic.tone.as_deref() {
            style = style.fg(tone_from_name(tone, self.theme)
                .fg
                .unwrap_or(self.theme.text.primary));
        }
        for emphasis in &semantic.emphasis {
            style = match emphasis.as_str() {
                "bold" | "strong" => style.add_modifier(Modifier::BOLD),
                "italic" | "emphasis" => style.add_modifier(Modifier::ITALIC),
                "underline" => style.add_modifier(Modifier::UNDERLINED),
                "strikethrough" => style.add_modifier(Modifier::CROSSED_OUT),
                "reverse" => style.add_modifier(Modifier::REVERSED),
                "dim" => style.add_modifier(Modifier::DIM),
                _ => style,
            };
        }
        if semantic.opacity.is_some_and(|opacity| opacity < 0.75) {
            style = style.add_modifier(Modifier::DIM);
        }
        style
    }

    fn border_style(&self, node: &UiNode) -> Style {
        let color = node
            .props
            .style
            .as_ref()
            .and_then(|style| style.border_color.as_deref())
            .map_or(self.theme.surface.border, |token| {
                token_color(token, self.theme)
            });
        let focused = node
            .id
            .as_ref()
            .is_some_and(|id| self.state.focused_node.as_ref() == Some(id));
        Style::default().fg(if focused {
            self.theme.focus.active
        } else {
            color
        })
    }

    fn tone_style(&self, node: &UiNode) -> Style {
        let tone = node
            .props
            .feedback
            .as_ref()
            .and_then(|feedback| feedback.tone.as_deref().or(feedback.status.as_deref()))
            .or_else(|| {
                node.props
                    .style
                    .as_ref()
                    .and_then(|style| style.tone.as_deref())
            })
            .unwrap_or("info");
        tone_from_name(tone, self.theme)
    }

    fn text_lines(&mut self, text: &str, area: Rect, style: Style, offset: u32) {
        if area.is_empty() {
            return;
        }
        let wrapped = wrap_cells(text, usize::from(area.width));
        for (row, line) in wrapped
            .iter()
            .skip(offset as usize)
            .take(usize::from(area.height))
            .enumerate()
        {
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                line,
                usize::from(area.width),
                style,
            );
        }
    }

    fn draw_string_lines(&mut self, node: &UiNode, lines: &[String], area: Rect, style: Style) {
        let offset = self.scroll_offset(node) as usize;
        for (row, line) in lines
            .iter()
            .skip(offset)
            .take(usize::from(area.height))
            .enumerate()
        {
            self.buffer.set_stringn(
                area.x,
                area.y.saturating_add(row as u16),
                truncate_cells(line, usize::from(area.width)),
                usize::from(area.width),
                style,
            );
        }
        self.scrollbar(area, lines.len() as u32, offset as u32, area.height);
    }

    fn scrollbar(&mut self, area: Rect, total: u32, offset: u32, viewport: u16) {
        if area.width == 0 || area.height == 0 || total <= u32::from(viewport) || viewport == 0 {
            return;
        }
        let track_x = area.right().saturating_sub(1);
        let thumb_height = ((u32::from(viewport) * u32::from(viewport) / total).max(1)) as u16;
        let max_offset = total.saturating_sub(u32::from(viewport)).max(1);
        let travel = viewport.saturating_sub(thumb_height);
        let thumb_y = ((offset.min(max_offset) * u32::from(travel)) / max_offset) as u16;
        for row in 0..viewport.min(area.height) {
            let thumb = row >= thumb_y && row < thumb_y.saturating_add(thumb_height);
            let symbol = if self.capabilities.unicode {
                if thumb {
                    "█"
                } else {
                    "│"
                }
            } else if thumb {
                "#"
            } else {
                "|"
            };
            self.buffer.set_stringn(
                track_x,
                area.y.saturating_add(row),
                symbol,
                1,
                if thumb {
                    Style::default().fg(self.theme.focus.active)
                } else {
                    self.muted_style()
                },
            );
        }
    }

    fn scroll_offset(&self, node: &UiNode) -> u32 {
        node.id
            .as_ref()
            .and_then(|id| self.state.scroll_offsets.get(id).copied())
            .unwrap_or(0)
    }

    fn selected_index(&self, node: &UiNode) -> Option<usize> {
        node.id
            .as_ref()
            .and_then(|id| self.state.selected_indices.get(id).copied())
    }

    fn input_value(&self, node: &UiNode) -> Value {
        node.id
            .as_ref()
            .and_then(|id| self.state.input_values.get(id))
            .cloned()
            .or_else(|| {
                node.props
                    .input
                    .as_ref()
                    .and_then(|input| input.value.clone().or_else(|| input.default_value.clone()))
            })
            .unwrap_or(Value::Null)
    }

    fn register_interaction(&mut self, node: &UiNode, area: Rect, default_role: &str) {
        let Some(id) = node.id.as_ref() else {
            return;
        };
        let disabled = node_disabled(node);
        let interactive = !node.props.event_bindings.is_empty()
            || node.props.input.is_some()
            || matches!(
                node.node_type.as_ref().map(|value| value.as_str()),
                Some(
                    primitives::BUTTON
                        | primitives::LINK
                        | primitives::MENU
                        | primitives::COMMAND_LIST
                        | primitives::TABS
                        | primitives::PAGINATION
                        | primitives::ACTION_MENU
                        | primitives::CONTEXT_MENU
                )
            );
        if !interactive {
            return;
        }
        let label = component_label(node);
        let keyboard_hint = node
            .props
            .accessibility
            .as_ref()
            .and_then(|accessibility| accessibility.keyboard_hint.clone());
        let focused = self.state.focused_node.as_ref() == Some(id);
        if focused && self.options.show_focus_hint && area.height > 1 {
            if let Some(hint) = keyboard_hint.as_deref() {
                self.buffer.set_stringn(
                    area.x,
                    area.bottom().saturating_sub(1),
                    truncate_cells(hint, usize::from(area.width)),
                    usize::from(area.width),
                    self.muted_style(),
                );
            }
        }
        let role = node
            .props
            .accessibility
            .as_ref()
            .and_then(|accessibility| accessibility.role.clone())
            .or_else(|| node.props.role.clone())
            .unwrap_or_else(|| UiSemanticRole::from(default_role));
        let keyboard_actions = node
            .props
            .event_bindings
            .iter()
            .filter(|binding| binding_available(binding, self.capabilities))
            .flat_map(keyboard_actions)
            .collect::<Vec<_>>();
        let order = node
            .props
            .accessibility
            .as_ref()
            .and_then(|accessibility| accessibility.focus_order)
            .unwrap_or_else(|| {
                let order = self.focus_sequence;
                self.focus_sequence = self.focus_sequence.saturating_add(1);
                order
            });
        self.output.focus_order.push(FocusDescriptor {
            node_id: id.clone(),
            area,
            order,
            role,
            label,
            keyboard_hint,
            disabled,
            keyboard_actions,
        });
        if self.capabilities.mouse && !disabled {
            self.output.hit_regions.extend(
                node.props
                    .event_bindings
                    .iter()
                    .filter(|binding| {
                        binding_available(binding, self.capabilities) && pointer_event(binding)
                    })
                    .cloned()
                    .map(|binding| HitRegion {
                        node_id: id.clone(),
                        area,
                        binding,
                    }),
            );
        }
    }

    fn diagnostic(
        &mut self,
        node_id: Option<UiNodeId>,
        severity: DiagnosticSeverity,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.output.diagnostics.push(RenderDiagnostic {
            severity,
            code,
            node_id,
            message: message.into(),
        });
    }
}

fn translate_scrolled_rect(area: &mut Rect, source_viewport: Rect, destination: Rect) -> bool {
    let visible = area.intersection(source_viewport);
    if visible.is_empty() {
        return false;
    }
    *area = Rect::new(
        destination
            .x
            .saturating_add(visible.x.saturating_sub(source_viewport.x)),
        destination
            .y
            .saturating_add(visible.y.saturating_sub(source_viewport.y)),
        visible.width.min(destination.width),
        visible.height.min(destination.height),
    );
    true
}

fn diagnostic_panel(buffer: &mut Buffer, area: Rect, theme: &Theme, title: &str, message: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.status.error))
        .style(
            Style::default()
                .fg(theme.text.primary)
                .bg(theme.surface.panel),
        )
        .title(sanitize_terminal_text(title));
    let inner = block.inner(area);
    block.render(area, buffer);
    Paragraph::new(sanitize_terminal_text(message))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.status.error))
        .render(inner, buffer);
}

fn vertical_measure(
    painter: &Painter<'_>,
    children: &[UiNode],
    width: u16,
    depth: u16,
    layout: Option<&codypendent_protocol::remote_ui::UiLayout>,
) -> u16 {
    let heights = children
        .iter()
        .map(|child| painter.measure(child, width, depth))
        .fold(0_u16, u16::saturating_add);
    heights.saturating_add(
        layout::gap(layout, Axis::Vertical).saturating_mul(children.len().saturating_sub(1) as u16),
    )
}

fn bounded_height(value: usize) -> u16 {
    u16::try_from(value)
        .unwrap_or(u16::MAX)
        .min(MAX_MEASURED_HEIGHT)
}

fn content_height(heights: &[u16], gap: u16) -> u32 {
    heights
        .iter()
        .copied()
        .map(u32::from)
        .sum::<u32>()
        .saturating_add(u32::from(gap).saturating_mul(heights.len().saturating_sub(1) as u32))
}

fn edge_cells(value: f64) -> u16 {
    if value.is_finite() && value > 0.0 {
        value.round().clamp(0.0, f64::from(u16::MAX)) as u16
    } else {
        0
    }
}

fn value_line_count(node: &UiNode) -> usize {
    node.props.value.as_ref().map_or(1, |value| match value {
        Value::Array(values) => values.len().max(1),
        Value::Object(values) => values.len().max(1),
        _ => 1,
    })
}

fn content_text(node: &UiNode) -> String {
    node.props
        .content
        .as_ref()
        .and_then(|content| content.text.clone())
        .or_else(|| node.text.clone())
        .or_else(|| {
            node.props
                .feedback
                .as_ref()
                .and_then(|feedback| feedback.message.clone())
        })
        .or_else(|| string_attribute(node, "text"))
        .or_else(|| string_attribute(node, "label"))
        .unwrap_or_default()
}

fn component_label(node: &UiNode) -> String {
    node.props
        .accessibility
        .as_ref()
        .and_then(|accessibility| accessibility.label.clone())
        .or_else(|| string_attribute(node, "label"))
        .or_else(|| string_attribute(node, "title"))
        .or_else(|| {
            node.props
                .content
                .as_ref()
                .and_then(|content| content.alternate_text.clone())
        })
        .or_else(|| {
            let text = content_text(node);
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| {
            node.node_type.as_ref().map_or_else(
                || "Component".to_owned(),
                |primitive| split_camel_case(primitive.as_str()),
            )
        })
}

fn string_attribute(node: &UiNode, key: &str) -> Option<String> {
    attribute(node, key)
        .and_then(Value::as_str)
        .map(sanitize_terminal_text)
}

fn number_attribute(node: &UiNode, key: &str) -> Option<f64> {
    attribute(node, key).and_then(Value::as_f64)
}

fn bool_attribute(node: &UiNode, key: &str) -> Option<bool> {
    attribute(node, key).and_then(Value::as_bool)
}

fn attribute<'a>(node: &'a UiNode, key: &str) -> Option<&'a Value> {
    node.props
        .attributes
        .get(key)
        .or_else(|| node.props.extension.get(key))
}

fn data_items(node: &UiNode) -> &[Value] {
    node.props
        .structured_data
        .as_ref()
        .map_or(&[], |data| data.items.as_slice())
}

fn item_id(value: &Value) -> Option<&str> {
    value
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
}

fn item_label(value: &Value, index: usize) -> String {
    value
        .as_object()
        .and_then(|object| {
            ["label", "title", "name", "message", "value"]
                .into_iter()
                .find_map(|key| object.get(key))
        })
        .map(value_text)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| match value {
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| format!("{key}: {}", value_text(value)))
                .collect::<Vec<_>>()
                .join(", "),
            _ => {
                let label = value_text(value);
                if label.is_empty() {
                    format!("Item {}", index + 1)
                } else {
                    label
                }
            }
        })
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => sanitize_terminal_text(value),
        Value::Array(values) => values.iter().map(value_text).collect::<Vec<_>>().join(", "),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| format!("{key}: {}", value_text(value)))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn value_summary(value: &Value) -> Vec<String> {
    match value {
        Value::Object(object) => object
            .iter()
            .take(20)
            .map(|(key, value)| format!("{key}: {}", value_text(value)))
            .collect(),
        Value::Array(values) => values.iter().take(20).map(value_text).collect(),
        value => vec![value_text(value)],
    }
}

fn flatten_json(value: &Value, key: &str, depth: usize, unicode: bool, lines: &mut Vec<String>) {
    if depth > 24 || lines.len() > 10_000 {
        lines.push(format!("{}…", "  ".repeat(depth.min(12))));
        return;
    }
    let branch = if unicode { "└─" } else { "|-" };
    match value {
        Value::Object(object) => {
            lines.push(format!("{}{branch} {key} {{}}", "  ".repeat(depth.min(12))));
            for (child_key, child) in object {
                flatten_json(child, child_key, depth + 1, unicode, lines);
            }
        }
        Value::Array(values) => {
            lines.push(format!(
                "{}{branch} {key} [{}]",
                "  ".repeat(depth.min(12)),
                values.len()
            ));
            for (index, child) in values.iter().enumerate() {
                flatten_json(child, &index.to_string(), depth + 1, unicode, lines);
            }
        }
        value => lines.push(format!(
            "{}{branch} {key}: {}",
            "  ".repeat(depth.min(12)),
            value_text(value)
        )),
    }
}

fn fit_column_widths(preferred: &[usize], available: usize) -> Vec<usize> {
    if preferred.is_empty() {
        return Vec::new();
    }
    let minimum = 3;
    let mut widths = preferred
        .iter()
        .map(|width| (*width).max(minimum))
        .collect::<Vec<_>>();
    let mut total = widths.iter().sum::<usize>();
    while total > available && widths.iter().any(|width| *width > minimum) {
        if let Some((index, _)) = widths.iter().enumerate().max_by_key(|(_, width)| **width) {
            widths[index] -= 1;
            total -= 1;
        }
    }
    if total < available {
        let extra = available - total;
        let per_column = extra / widths.len();
        let remainder = extra % widths.len();
        for (index, width) in widths.iter_mut().enumerate() {
            *width += per_column + usize::from(index < remainder);
        }
    }
    widths
}

const ARM_UP: u8 = 1;
const ARM_DOWN: u8 = 2;
const ARM_LEFT: u8 = 4;
const ARM_RIGHT: u8 = 8;
const GRAPH_NODE_GAP: u16 = 3;
const GRAPH_WAYPOINT_LIMIT: usize = 512;

/// A drawable box-glyph for a set of connector arms, or `None` for empty cells.
fn arm_glyph(mask: u8, unicode: bool) -> Option<&'static str> {
    if mask == 0 {
        return None;
    }
    if !unicode {
        return Some(match mask {
            mask if mask & (ARM_LEFT | ARM_RIGHT) == 0 => "|",
            mask if mask & (ARM_UP | ARM_DOWN) == 0 => "-",
            _ => "+",
        });
    }
    Some(match mask {
        1..=3 => "│",
        4 | 8 | 12 => "─",
        5 => "┘",
        9 => "└",
        6 => "┐",
        10 => "┌",
        7 => "┤",
        11 => "├",
        13 => "┴",
        14 => "┬",
        _ => "┼",
    })
}

/// One node label placed inside the graph area (coordinates relative to it).
struct GraphPlacement {
    x: u16,
    y: u16,
    label: String,
    status: Option<String>,
    id: Option<String>,
}

/// A laid-out graph: connector arms per cell plus real node placements.
struct GraphDiagram {
    width: usize,
    arms: Vec<u8>,
    placements: Vec<GraphPlacement>,
}

enum GraphEntry {
    Item(usize),
    Waypoint,
}

/// Top-level `edges: [{from,to}]` (the SDK Graph contract; also accepted as
/// `source`/`target`), with the legacy per-node `edges`/`targets` id lists as a
/// fallback. Endpoints are item ids, falling back to labels.
fn graph_edges(node: &UiNode) -> Vec<(String, String)> {
    let top_level = node
        .props
        .structured_data
        .as_ref()
        .and_then(|data| data.schema.as_ref())
        .and_then(|schema| schema.get("edges"))
        .or_else(|| attribute(node, "edges"))
        .and_then(Value::as_array);
    let mut edges = Vec::new();
    if let Some(values) = top_level {
        for value in values {
            let Some(object) = value.as_object() else {
                continue;
            };
            let from = object
                .get("from")
                .or_else(|| object.get("source"))
                .and_then(Value::as_str);
            let to = object
                .get("to")
                .or_else(|| object.get("target"))
                .and_then(Value::as_str);
            if let (Some(from), Some(to)) = (from, to) {
                edges.push((from.to_owned(), to.to_owned()));
            }
        }
    }
    if !edges.is_empty() {
        return edges;
    }
    for (index, item) in data_items(node).iter().enumerate() {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(targets) = object
            .get("edges")
            .or_else(|| object.get("targets"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let source = graph_item_key(item, index);
        for target in targets {
            let target = value_text(target);
            if !target.is_empty() {
                edges.push((source.clone(), target));
            }
        }
    }
    edges
}

fn graph_item_key(item: &Value, index: usize) -> String {
    item_id(item)
        .map(str::to_owned)
        .unwrap_or_else(|| item_label(item, index))
}

fn graph_item_index(items: &[Value], key: &str) -> Option<usize> {
    items
        .iter()
        .position(|item| item_id(item) == Some(key))
        .or_else(|| {
            items
                .iter()
                .enumerate()
                .position(|(index, item)| item_label(item, index) == key)
        })
}

fn graph_node_label(item: &Value, index: usize, unicode: bool) -> String {
    let label = item_label(item, index);
    match graph_item_status(item) {
        Some(status) => format!("{} {label}", status_symbol(status, unicode)),
        None => label,
    }
}

fn graph_item_status(item: &Value) -> Option<&str> {
    item.as_object()
        .and_then(|object| object.get("status"))
        .and_then(Value::as_str)
}

/// Assign every item a layer by longest-path depth. Bounded relaxation keeps
/// the pass total under cycles; unreachable relaxations simply stop at the cap.
fn graph_layers(item_count: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut depth = vec![0_usize; item_count];
    let cap = item_count.saturating_sub(1);
    for _ in 0..item_count {
        let mut changed = false;
        for (from, to) in edges {
            if from == to {
                continue;
            }
            let candidate = depth[*from] + 1;
            if candidate > depth[*to] && candidate <= cap {
                depth[*to] = candidate;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Compact to consecutive ranks so capped/cyclic depths do not leave gaps.
    let mut used = depth.clone();
    used.sort_unstable();
    used.dedup();
    depth
        .into_iter()
        .map(|value| used.iter().position(|rank| *rank == value).unwrap_or(0))
        .collect()
}

/// Lay the graph out as a layered DAG that fits `area`, or `None` when the
/// data or space cannot support a diagram (the caller then degrades to text).
fn layout_graph_diagram(node: &UiNode, area: Rect, unicode: bool) -> Option<GraphDiagram> {
    let items = data_items(node);
    if items.is_empty() || area.width == 0 || area.height == 0 {
        return None;
    }
    let resolved = graph_edges(node)
        .iter()
        .filter_map(|(from, to)| {
            Some((graph_item_index(items, from)?, graph_item_index(items, to)?))
        })
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return None;
    }
    let depth = graph_layers(items.len(), &resolved);
    let layer_count = depth.iter().copied().max().unwrap_or(0) + 1;
    if layer_count < 2 {
        return None;
    }
    let mut layers: Vec<Vec<GraphEntry>> = (0..layer_count).map(|_| Vec::new()).collect();
    let mut slot: Vec<usize> = vec![0; items.len()];
    for (index, item_depth) in depth.iter().enumerate() {
        slot[index] = layers[*item_depth].len();
        layers[*item_depth].push(GraphEntry::Item(index));
    }
    // Insert waypoint entries so every drawn segment spans adjacent layers.
    let mut segments: Vec<(usize, usize, usize)> = Vec::new();
    let mut waypoints = 0_usize;
    for (from, to) in &resolved {
        let (mut layer, target_layer) = (depth[*from], depth[*to]);
        if target_layer <= layer {
            continue; // cycle leftovers stay in the adjacency of the text rung
        }
        let mut source_slot = slot[*from];
        while layer + 1 < target_layer {
            waypoints += 1;
            if waypoints > GRAPH_WAYPOINT_LIMIT {
                return None;
            }
            let waypoint_slot = layers[layer + 1].len();
            layers[layer + 1].push(GraphEntry::Waypoint);
            segments.push((layer, source_slot, waypoint_slot));
            source_slot = waypoint_slot;
            layer += 1;
        }
        segments.push((layer, source_slot, slot[*to]));
    }
    let horizontal = node
        .props
        .layout
        .as_ref()
        .and_then(|layout| layout.direction.as_deref())
        .map(str::to_owned)
        .or_else(|| string_attribute(node, "direction"))
        .is_some_and(|direction| direction == "horizontal");
    let entry_label = |entry: &GraphEntry| -> Option<String> {
        match entry {
            GraphEntry::Item(index) => Some(graph_node_label(&items[*index], *index, unicode)),
            GraphEntry::Waypoint => None,
        }
    };
    let entry_width = |entry: &GraphEntry| -> u16 {
        entry_label(entry).map_or(1, |label| cell_width(&label) as u16)
    };

    let width = usize::from(area.width);
    let mut arms = Vec::new();
    let mut placements = Vec::new();
    let mark = |arms: &mut Vec<u8>, x: u16, y: u16, mask: u8| {
        if usize::from(x) >= width {
            return;
        }
        let index = usize::from(y) * width + usize::from(x);
        if arms.len() <= index {
            arms.resize((usize::from(y) + 1) * width, 0);
        }
        arms[index] |= mask;
    };

    if horizontal {
        // Layers become columns flowing left → right.
        let column_widths = layers
            .iter()
            .map(|layer| layer.iter().map(&entry_width).max().unwrap_or(1))
            .collect::<Vec<_>>();
        let mut column_x = Vec::with_capacity(layer_count);
        let mut cursor = 0_u16;
        for (index, column_width) in column_widths.iter().enumerate() {
            column_x.push(cursor);
            cursor = cursor.saturating_add(*column_width);
            if index + 1 < layer_count {
                cursor = cursor.saturating_add(GRAPH_NODE_GAP);
            }
        }
        let height = layers.iter().map(Vec::len).max().unwrap_or(0) as u16;
        if cursor > area.width || height > area.height || height == 0 {
            return None;
        }
        for (layer_index, layer) in layers.iter().enumerate() {
            for (slot_index, entry) in layer.iter().enumerate() {
                let y = slot_index as u16;
                match entry {
                    GraphEntry::Item(index) => placements.push(GraphPlacement {
                        x: column_x[layer_index],
                        y,
                        label: graph_node_label(&items[*index], *index, unicode),
                        status: graph_item_status(&items[*index]).map(str::to_owned),
                        id: item_id(&items[*index]).map(str::to_owned),
                    }),
                    GraphEntry::Waypoint => {
                        for offset in 0..column_widths[layer_index] {
                            mark(
                                &mut arms,
                                column_x[layer_index].saturating_add(offset),
                                y,
                                ARM_LEFT | ARM_RIGHT,
                            );
                        }
                    }
                }
            }
        }
        for (layer, from_slot, to_slot) in segments {
            let exit = column_x[layer].saturating_add(column_widths[layer]);
            let mid = exit + 1;
            let (r1, r2) = (from_slot as u16, to_slot as u16);
            mark(&mut arms, exit, r1, ARM_LEFT | ARM_RIGHT);
            mark(&mut arms, mid + 1, r2, ARM_LEFT | ARM_RIGHT);
            if r1 == r2 {
                mark(&mut arms, mid, r1, ARM_LEFT | ARM_RIGHT);
            } else {
                let (top, bottom) = (r1.min(r2), r1.max(r2));
                mark(
                    &mut arms,
                    mid,
                    r1,
                    ARM_LEFT | if r2 > r1 { ARM_DOWN } else { ARM_UP },
                );
                for row in top + 1..bottom {
                    mark(&mut arms, mid, row, ARM_UP | ARM_DOWN);
                }
                mark(
                    &mut arms,
                    mid,
                    r2,
                    ARM_RIGHT | if r2 > r1 { ARM_UP } else { ARM_DOWN },
                );
            }
        }
    } else {
        // Layers become rows flowing top → bottom, one connector row between.
        let needed_height = (layer_count * 2 - 1) as u16;
        if needed_height > area.height {
            return None;
        }
        let mut centers: Vec<Vec<u16>> = Vec::with_capacity(layer_count);
        for layer in &layers {
            let mut layer_centers = Vec::with_capacity(layer.len());
            let mut cursor = 0_u16;
            for (slot_index, entry) in layer.iter().enumerate() {
                if slot_index > 0 {
                    cursor = cursor.saturating_add(GRAPH_NODE_GAP);
                }
                let entry_cells = entry_width(entry);
                layer_centers.push(cursor + entry_cells / 2);
                cursor = cursor.saturating_add(entry_cells);
            }
            if cursor > area.width {
                return None;
            }
            centers.push(layer_centers);
        }
        for (layer_index, layer) in layers.iter().enumerate() {
            let y = (layer_index * 2) as u16;
            let mut cursor = 0_u16;
            for (slot_index, entry) in layer.iter().enumerate() {
                if slot_index > 0 {
                    cursor = cursor.saturating_add(GRAPH_NODE_GAP);
                }
                match entry {
                    GraphEntry::Item(index) => {
                        let label = graph_node_label(&items[*index], *index, unicode);
                        let label_cells = cell_width(&label) as u16;
                        placements.push(GraphPlacement {
                            x: cursor,
                            y,
                            label,
                            status: graph_item_status(&items[*index]).map(str::to_owned),
                            id: item_id(&items[*index]).map(str::to_owned),
                        });
                        cursor = cursor.saturating_add(label_cells);
                    }
                    GraphEntry::Waypoint => {
                        mark(
                            &mut arms,
                            centers[layer_index][slot_index],
                            y,
                            ARM_UP | ARM_DOWN,
                        );
                        cursor = cursor.saturating_add(1);
                    }
                }
            }
        }
        for (layer, from_slot, to_slot) in segments {
            let y = (layer * 2 + 1) as u16;
            let c1 = centers[layer][from_slot];
            let c2 = centers[layer + 1][to_slot];
            if c1 == c2 {
                mark(&mut arms, c1, y, ARM_UP | ARM_DOWN);
            } else {
                let (left, right) = (c1.min(c2), c1.max(c2));
                mark(
                    &mut arms,
                    c1,
                    y,
                    ARM_UP | if c2 > c1 { ARM_RIGHT } else { ARM_LEFT },
                );
                for column in left + 1..right {
                    mark(&mut arms, column, y, ARM_LEFT | ARM_RIGHT);
                }
                mark(
                    &mut arms,
                    c2,
                    y,
                    ARM_DOWN | if c2 > c1 { ARM_LEFT } else { ARM_RIGHT },
                );
            }
        }
    }
    Some(GraphDiagram {
        width,
        arms,
        placements,
    })
}

/// The measured height a graph diagram would need at `width`, or `None` when
/// only the adjacency-list rung is drawable.
fn graph_diagram_height(node: &UiNode, width: u16, unicode: bool) -> Option<u16> {
    let probe = Rect::new(0, 0, width, MAX_MEASURED_HEIGHT);
    let diagram = layout_graph_diagram(node, probe, unicode)?;
    let rows = diagram
        .placements
        .iter()
        .map(|placement| placement.y + 1)
        .chain(std::iter::once(
            (diagram.arms.len().div_ceil(diagram.width.max(1))) as u16,
        ))
        .max()
        .unwrap_or(1);
    Some(rows.max(1))
}

fn split_is_vertical(node: &UiNode) -> bool {
    node.props
        .layout
        .as_ref()
        .and_then(|layout| layout.direction.as_deref())
        .is_some_and(|direction| direction == "vertical")
}

/// The Split `ratio` prop, clamped exactly as the graphical renderer clamps it.
fn split_ratio(node: &UiNode) -> f64 {
    number_attribute(node, "ratio")
        .filter(|ratio| ratio.is_finite())
        .map_or(0.5, |ratio| ratio.clamp(0.1, 0.9))
}

/// Pane sizes matching the web renderer's flex weights: the first pane grows
/// with `ratio`, every later pane with `1 - ratio`.
fn split_sizes(usable: u16, count: usize, ratio: f64) -> Vec<u16> {
    if count == 0 {
        return Vec::new();
    }
    let weights = (0..count)
        .map(|index| if index == 0 { ratio } else { 1.0 - ratio })
        .collect::<Vec<_>>();
    let total: f64 = weights.iter().sum();
    let mut sizes = weights
        .iter()
        .map(|weight| (f64::from(usable) * weight / total).floor() as u16)
        .collect::<Vec<_>>();
    let assigned = sizes.iter().copied().fold(0_u16, u16::saturating_add);
    if let Some(last) = sizes.last_mut() {
        *last = last.saturating_add(usable.saturating_sub(assigned));
    }
    sizes
}

fn numeric_series(node: &UiNode) -> Vec<f64> {
    data_items(node)
        .iter()
        .filter_map(|item| {
            item.as_f64().or_else(|| {
                item.as_object().and_then(|object| {
                    ["value", "y", "current"]
                        .into_iter()
                        .find_map(|key| object.get(key).and_then(Value::as_f64))
                })
            })
        })
        .filter(|value| value.is_finite())
        .collect()
}

fn selected_option_labels(node: &UiNode, value: &Value) -> String {
    let selected = match value {
        Value::Array(values) => values.iter().collect::<Vec<_>>(),
        Value::Null => Vec::new(),
        value => vec![value],
    };
    node.props
        .input
        .as_ref()
        .map(|input| {
            input
                .options
                .iter()
                .filter(|option| selected.contains(&&option.value))
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| value_text(value))
}

fn node_disabled(node: &UiNode) -> bool {
    node.props
        .input
        .as_ref()
        .is_some_and(|input| input.disabled)
        || node
            .props
            .navigation
            .as_ref()
            .and_then(|navigation| navigation.disabled)
            .unwrap_or(false)
        || node
            .props
            .event_bindings
            .iter()
            .all(|binding| binding.disabled)
            && !node.props.event_bindings.is_empty()
}

fn inferred_role(primitive: &str) -> UiSemanticRole {
    UiSemanticRole::from(match primitive {
        primitives::TEXT_INPUT | primitives::TEXT_AREA => "textbox",
        primitives::SELECT | primitives::MULTI_SELECT => "combobox",
        primitives::CHECKBOX => "checkbox",
        primitives::RADIO => "radio",
        _ => "group",
    })
}

fn keyboard_actions(binding: &UiActionBinding) -> Vec<KeyboardAction> {
    let event = binding.event.as_str().to_ascii_lowercase();
    let keys: &[RemoteKey] = match event.as_str() {
        "action" | "press" | "click" | "activate" | "open" | "submit" => {
            &[RemoteKey::Enter, RemoteKey::Space]
        }
        "toggle" | "change" | "select" => &[RemoteKey::Space, RemoteKey::Enter],
        "dismiss" | "cancel" | "close" => &[RemoteKey::Escape],
        "next" => &[RemoteKey::Right, RemoteKey::Down, RemoteKey::PageDown],
        "previous" => &[RemoteKey::Left, RemoteKey::Up, RemoteKey::PageUp],
        "input" => &[
            RemoteKey::Character,
            RemoteKey::Backspace,
            RemoteKey::Delete,
        ],
        "focusnext" => &[RemoteKey::Tab],
        "focusprevious" => &[RemoteKey::ShiftTab],
        _ => &[RemoteKey::Enter],
    };
    keys.iter()
        .cloned()
        .map(|key| KeyboardAction {
            key,
            binding: binding.clone(),
        })
        .collect()
}

fn pointer_event(binding: &UiActionBinding) -> bool {
    matches!(
        binding.event.as_str().to_ascii_lowercase().as_str(),
        "action"
            | "press"
            | "click"
            | "activate"
            | "open"
            | "toggle"
            | "change"
            | "select"
            | "contextmenu"
            | "submit"
    )
}

fn binding_available(binding: &UiActionBinding, capabilities: &TerminalUiCapabilities) -> bool {
    !binding.disabled
        && binding
            .requires
            .iter()
            .all(|requirement| capabilities.supports_feature(requirement.as_str()))
}

fn safe_uri(uri: &str) -> String {
    let clean = sanitize_terminal_text(uri);
    let without_credentials = if let Some(scheme) = clean.find("://") {
        let authority_start = scheme + 3;
        clean[authority_start..]
            .find('@')
            .map_or(clean.as_str(), |at| {
                let host_start = authority_start + at + 1;
                // Keep scheme and host/path while hiding embedded user info.
                // This allocates below through the fallback branch.
                &clean[host_start..]
            })
    } else {
        clean.as_str()
    };
    truncate_cells(
        without_credentials
            .split(['?', '#'])
            .next()
            .unwrap_or(without_credentials),
        80,
    )
}

fn contains_word(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase().contains(needle)
}

fn split_camel_case(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 4);
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && ch.is_uppercase() {
            result.push(' ');
        }
        result.push(ch);
    }
    result
}

fn domain_suffix(value: &str) -> &str {
    value.rsplit(['/', ':', '.']).next().unwrap_or(value)
}

fn status_symbol(status: &str, unicode: bool) -> &'static str {
    if !unicode {
        return match status.to_ascii_lowercase().as_str() {
            "success" | "complete" | "completed" | "ready" => "+",
            "error" | "failed" | "failure" => "x",
            "warning" | "blocked" => "!",
            "running" | "pending" => "*",
            _ => "i",
        };
    }
    match status.to_ascii_lowercase().as_str() {
        "success" | "complete" | "completed" | "ready" => "✓",
        "error" | "failed" | "failure" => "✕",
        "warning" | "blocked" => "⚠",
        "running" | "pending" => "●",
        _ => "ℹ",
    }
}

fn tone_from_name(tone: &str, theme: &Theme) -> Style {
    let color = match tone.to_ascii_lowercase().as_str() {
        "success" | "positive" | "complete" | "completed" | "ready" => theme.status.success,
        "warning" | "caution" | "blocked" => theme.status.warning,
        "error" | "negative" | "danger" | "failed" | "failure" => theme.status.error,
        "running" | "active" | "pending" => theme.status.running,
        "idle" | "neutral" => theme.status.idle,
        _ => theme.status.info,
    };
    Style::default().fg(color).bg(theme.surface.background)
}

fn token_color(token: &str, theme: &Theme) -> Color {
    match token.trim_start_matches("theme.") {
        "surface.background" => theme.surface.background,
        "surface.panel" => theme.surface.panel,
        "surface.border" => theme.surface.border,
        "surface.overlay" => theme.surface.overlay,
        "surface.user" => theme.surface.user,
        "text.primary" => theme.text.primary,
        "text.secondary" => theme.text.secondary,
        "text.muted" => theme.text.muted,
        "text.heading" => theme.text.heading,
        "status.info" => theme.status.info,
        "status.success" => theme.status.success,
        "status.warning" => theme.status.warning,
        "status.error" => theme.status.error,
        "status.running" => theme.status.running,
        "status.idle" => theme.status.idle,
        "syntax.keyword" => theme.syntax.keyword,
        "syntax.literal" => theme.syntax.literal,
        "syntax.string" => theme.syntax.string,
        "syntax.comment" => theme.syntax.comment,
        "syntax.type" => theme.syntax.r#type,
        "syntax.function" => theme.syntax.function,
        "syntax.operator" => theme.syntax.operator,
        "syntax.constant" => theme.syntax.constant,
        "syntax.punctuation" => theme.syntax.punctuation,
        "diff.added" => theme.diff.added,
        "diff.removed" => theme.diff.removed,
        "diff.context" => theme.diff.context,
        "diff.header" => theme.diff.header,
        "agent.modelText" | "agent.model_text" => theme.agent.model_text,
        "agent.tool" => theme.agent.tool,
        "agent.thinking" => theme.agent.thinking,
        "focus.active" => theme.focus.active,
        "focus.inactive" => theme.focus.inactive,
        "selection.foreground" => theme.selection.foreground,
        "selection.background" => theme.selection.background,
        _ => theme.text.primary,
    }
}

fn markdown_style(role: SpanRole, theme: &Theme) -> Style {
    match role {
        SpanRole::Gutter => Style::default().fg(theme.text.muted),
        SpanRole::Body => Style::default().fg(theme.agent.model_text),
        SpanRole::Heading(1..=2) => Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        SpanRole::Heading(_) => Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD),
        SpanRole::Strong => Style::default()
            .fg(theme.text.primary)
            .add_modifier(Modifier::BOLD),
        SpanRole::Emphasis => Style::default()
            .fg(theme.agent.model_text)
            .add_modifier(Modifier::ITALIC),
        SpanRole::StrongEmphasis => Style::default()
            .fg(theme.text.primary)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        SpanRole::InlineCode => Style::default().fg(theme.syntax.string),
        SpanRole::Link => Style::default()
            .fg(theme.focus.active)
            .add_modifier(Modifier::UNDERLINED),
        SpanRole::ListMarker => Style::default().fg(theme.agent.tool),
        SpanRole::BlockQuote => Style::default()
            .fg(theme.text.secondary)
            .add_modifier(Modifier::ITALIC),
        SpanRole::Rule => Style::default().fg(theme.text.muted),
        SpanRole::TableHeader => Style::default()
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD),
        SpanRole::TableCell => Style::default().fg(theme.agent.model_text),
        SpanRole::TableRule => Style::default().fg(theme.surface.border),
        SpanRole::CodePlain => Style::default().fg(theme.text.primary),
        SpanRole::CodeToken(SyntaxRole::Keyword) => Style::default().fg(theme.syntax.keyword),
        SpanRole::CodeToken(SyntaxRole::Literal) => Style::default().fg(theme.syntax.literal),
        SpanRole::CodeToken(SyntaxRole::StringLit) => Style::default().fg(theme.syntax.string),
        SpanRole::CodeToken(SyntaxRole::Comment) => Style::default().fg(theme.syntax.comment),
        SpanRole::CodeToken(SyntaxRole::Type) => Style::default().fg(theme.syntax.r#type),
        SpanRole::CodeToken(SyntaxRole::Function) => Style::default().fg(theme.syntax.function),
        SpanRole::CodeToken(SyntaxRole::Operator) => Style::default().fg(theme.syntax.operator),
        SpanRole::CodeToken(SyntaxRole::Constant) => Style::default().fg(theme.syntax.constant),
        SpanRole::CodeToken(SyntaxRole::Punctuation) => {
            Style::default().fg(theme.syntax.punctuation)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use codypendent_protocol::remote_ui::{
        UiActionBinding, UiActionId, UiContent, UiData, UiDataColumn, UiDocumentId, UiEventType,
        UiLayout, UiNodeProps, UiPatchOperation, UiPrimitive, UiProtocolVersion, UiRevision,
    };

    use super::*;
    use crate::remote_ui::{
        render_remote_ui, RemoteUiRenderOptions, RemoteUiViewState, TerminalUiCapabilities,
        ALL_NATIVE_PRIMITIVES,
    };

    fn document(root: UiNode) -> UiDocument {
        UiDocument {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("test-document"),
            revision: UiRevision(0),
            root,
            capabilities: None,
            metadata: BTreeMap::new(),
            compatibility: None,
        }
    }

    fn text(id: &str, value: &str) -> UiNode {
        let mut node = UiNode::element(id, primitives::TEXT);
        node.props.content = Some(UiContent {
            text: Some(value.to_owned()),
            ..UiContent::default()
        });
        node
    }

    fn buffer_snapshot(buffer: &Buffer) -> String {
        (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                let mut row = String::new();
                for x in buffer.area.x..buffer.area.right() {
                    if let Some(cell) = buffer.cell((x, y)) {
                        row.push_str(cell.symbol());
                    }
                }
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    }

    #[test]
    fn deterministic_snapshot_and_interaction_descriptors() {
        let mut root = UiNode::element("root", primitives::STACK);
        root.props.layout = Some(UiLayout {
            gap: Some(1.0),
            ..UiLayout::default()
        });
        let mut button = UiNode::element("run", primitives::BUTTON);
        button
            .props
            .attributes
            .insert("label".to_owned(), Value::String("Run".to_owned()));
        button.props.event_bindings.push(UiActionBinding {
            event: UiEventType::from("press"),
            action_id: UiActionId::from("run.execute"),
            payload: Value::Null,
            requires: Vec::new(),
            disabled: false,
            confirmation: None,
        });
        root.children = vec![text("hello", "Hello 界"), button];
        let document = document(root);
        let area = Rect::new(0, 0, 20, 5);
        let mut first = Buffer::empty(area);
        let output = render_remote_ui(
            &mut first,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let mut second = Buffer::empty(area);
        let second_output = render_remote_ui(
            &mut second,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(buffer_snapshot(&first), "Hello 界\n\n▐ Run ▌");
        assert_eq!(first, second);
        assert_eq!(output, second_output);
        assert_eq!(output.focus_order.len(), 1);
        assert_eq!(output.hit_regions.len(), 1);
        assert_eq!(output.focus_order[0].keyboard_actions.len(), 2);
        assert_eq!(output.accessibility.plain_text, "Hello 界\nRun");
    }

    #[test]
    fn scroll_area_applies_partial_child_line_offset() {
        let mut root = UiNode::element("scroll", primitives::SCROLL_AREA);
        root.children = vec![text("lines", "zero\none\ntwo")];
        let mut view = RemoteUiViewState::default();
        view.scroll_offsets.insert(UiNodeId::from("scroll"), 1);
        let area = Rect::new(0, 0, 8, 2);
        let mut buffer = Buffer::empty(area);
        render_remote_ui(
            &mut buffer,
            area,
            &document(root),
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &view,
            RemoteUiRenderOptions::default(),
        );
        assert!(buffer_snapshot(&buffer)
            .lines()
            .next()
            .unwrap_or_default()
            .starts_with("one"));
    }

    #[test]
    fn scroll_area_preserves_nested_stack_layout_when_partially_clipped() {
        let mut stack = UiNode::element("stack", primitives::STACK);
        stack.children = vec![
            text("zero", "zero"),
            text("one", "one"),
            text("two", "two"),
            text("three", "three"),
        ];
        let mut root = UiNode::element("scroll", primitives::SCROLL_AREA);
        root.children = vec![stack];
        let mut view = RemoteUiViewState::default();
        view.scroll_offsets.insert(UiNodeId::from("scroll"), 2);
        let area = Rect::new(0, 0, 8, 2);
        let mut buffer = Buffer::empty(area);
        let output = render_remote_ui(
            &mut buffer,
            area,
            &document(root),
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &view,
            RemoteUiRenderOptions::default(),
        );

        let snapshot = buffer_snapshot(&buffer);
        assert!(snapshot
            .lines()
            .next()
            .unwrap_or_default()
            .starts_with("two"));
        assert!(snapshot
            .lines()
            .nth(1)
            .unwrap_or_default()
            .starts_with("three"));
        assert!(output.visible_nodes.contains(&UiNodeId::from("two")));
        assert!(output.visible_nodes.contains(&UiNodeId::from("three")));
        assert!(!output.visible_nodes.contains(&UiNodeId::from("zero")));
        assert!(!output.visible_nodes.contains(&UiNodeId::from("one")));
    }

    #[test]
    fn scroll_area_translates_nested_interactions_into_the_viewport() {
        let mut button = UiNode::element("run", primitives::BUTTON);
        button
            .props
            .attributes
            .insert("label".to_owned(), Value::String("Run".to_owned()));
        button.props.event_bindings.push(UiActionBinding {
            event: UiEventType::from("press"),
            action_id: UiActionId::from("run.execute"),
            payload: Value::Null,
            requires: Vec::new(),
            disabled: false,
            confirmation: None,
        });
        let mut stack = UiNode::element("stack", primitives::STACK);
        stack.children = vec![text("zero", "zero"), text("one", "one"), button];
        let mut root = UiNode::element("scroll", primitives::SCROLL_AREA);
        root.children = vec![stack];
        let mut view = RemoteUiViewState::default();
        view.scroll_offsets.insert(UiNodeId::from("scroll"), 1);
        let area = Rect::new(0, 0, 8, 2);
        let mut buffer = Buffer::empty(area);
        let output = render_remote_ui(
            &mut buffer,
            area,
            &document(root),
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &view,
            RemoteUiRenderOptions::default(),
        );

        assert!(buffer_snapshot(&buffer)
            .lines()
            .nth(1)
            .unwrap_or_default()
            .starts_with("▐ Run ▌"));
        assert_eq!(output.focus_order.len(), 1);
        assert_eq!(output.focus_order[0].node_id, UiNodeId::from("run"));
        assert_eq!(output.focus_order[0].area, Rect::new(0, 1, 7, 1));
        assert_eq!(output.hit_regions.len(), 1);
        assert_eq!(output.hit_regions[0].area, Rect::new(0, 1, 7, 1));
    }

    #[test]
    fn scroll_area_reserves_a_column_for_its_scrollbar() {
        let mut root = UiNode::element("scroll", primitives::SCROLL_AREA);
        root.children = vec![text("lines", "ABCDE\nFGHIJ\nKLMNO")];
        let area = Rect::new(0, 0, 6, 2);
        let mut buffer = Buffer::empty(area);
        render_remote_ui(
            &mut buffer,
            area,
            &document(root),
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let first = buffer_snapshot(&buffer)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        assert!(
            first.starts_with("ABCDE"),
            "content was overwritten: {first:?}"
        );
        assert!(
            first.chars().count() >= 6,
            "scrollbar column missing: {first:?}"
        );
    }

    #[test]
    fn narrow_table_switches_to_stacked_records() {
        let mut table = UiNode::element("table", primitives::TABLE);
        table.props.structured_data = Some(UiData {
            columns: vec![
                UiDataColumn {
                    id: "name".into(),
                    label: "Name".into(),
                    value_type: None,
                    width: None,
                    sortable: true,
                },
                UiDataColumn {
                    id: "status".into(),
                    label: "Status".into(),
                    value_type: None,
                    width: None,
                    sortable: false,
                },
            ],
            items: vec![serde_json::json!({"name":"Worker α","status":"running"})],
            ..UiData::default()
        });
        let document = document(table);
        let area = Rect::new(0, 0, 24, 5);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::monochrome(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(buffer_snapshot(&buffer), "Name: Worker α\nStatus: running");
    }

    #[test]
    fn canonical_sdk_flat_props_render_and_produce_interactions() {
        let wire = serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "sdk-golden",
            "revision": 7,
            "root": {
                "kind": "element",
                "id": "root",
                "type": "Stack",
                "props": {"gap": "sm"},
                "children": [
                    {"kind":"element","id":"title","type":"Text","props":{"value":"SDK title","weight":"bold"},"children":[]},
                    {"kind":"element","id":"copy","type":"Markdown","props":{"source":"**Rich** body"},"children":[]},
                    {"kind":"element","id":"table","type":"Table","props":{
                        "columns":[{"key":"name","label":"Name"},{"key":"state","label":"State"}],
                        "rows":[{"name":"worker","state":"ready"}]
                    },"children":[]},
                    {"kind":"element","id":"query","type":"TextInput","props":{
                        "name":"query","value":"abc","placeholder":"Search","changeAction":"query.change",
                        "accessibleLabel":"Query"
                    },"children":[]},
                    {"kind":"element","id":"run","type":"Button","props":{
                        "label":"Run","action":"run.start","payload":{"mode":"build"},
                        "confirmation":"Start run?","accessibleLabel":"Run build"
                    },"children":[]}
                ]
            }
        });
        let document: UiDocument =
            serde_json::from_value(wire).expect("canonical SDK document deserializes");
        let area = Rect::new(0, 0, 64, 18);
        let mut buffer = Buffer::empty(area);
        let output = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let snapshot = buffer_snapshot(&buffer);
        assert!(snapshot.contains("SDK title"), "{snapshot}");
        assert!(snapshot.contains("Rich body"), "{snapshot}");
        assert!(snapshot.contains("worker"), "{snapshot}");
        assert!(snapshot.contains("Query: [ abc ]"), "{snapshot}");
        assert!(snapshot.contains("Run"), "{snapshot}");
        let input = output
            .form_fields
            .iter()
            .find(|field| field.node_id.as_str() == "query")
            .expect("flat input creates form descriptor");
        assert_eq!(input.name, "query");
        assert_eq!(input.value, Value::String("abc".to_owned()));
        let run = output
            .hit_regions
            .iter()
            .find(|hit| hit.node_id.as_str() == "run")
            .expect("flat action creates a hit region");
        assert_eq!(run.binding.action_id.as_str(), "run.start");
        assert_eq!(run.binding.event.as_str(), "action");
        assert_eq!(run.binding.confirmation.as_deref(), Some("Start run?"));
    }

    #[test]
    fn every_native_primitive_is_total_across_themes_and_narrow_widths() {
        for (index, primitive) in ALL_NATIVE_PRIMITIVES.iter().enumerate() {
            let mut root = UiNode::element(format!("node-{index}"), UiPrimitive::from(*primitive));
            root.props = UiNodeProps::default();
            root.props.content = Some(UiContent {
                text: Some("safe content".to_owned()),
                alternate_text: Some("fallback".to_owned()),
                ..UiContent::default()
            });
            root.props.structured_data = Some(UiData {
                items: vec![serde_json::json!({"id":"one","label":"One","value":1})],
                ..UiData::default()
            });
            let document = document(root);
            for theme in [
                Theme::dark(),
                Theme::light(),
                Theme::high_contrast(),
                Theme::monochrome(),
            ] {
                for width in [12, 72] {
                    let area = Rect::new(0, 0, width, 8);
                    let mut buffer = Buffer::empty(area);
                    let output = render_remote_ui(
                        &mut buffer,
                        area,
                        &document,
                        &theme,
                        &TerminalUiCapabilities::native(),
                        &RemoteUiViewState::default(),
                        RemoteUiRenderOptions::default(),
                    );
                    assert!(
                        output
                            .diagnostics
                            .iter()
                            .all(|diagnostic| diagnostic.code != "remote-ui.invalid-document"),
                        "{primitive} was rejected"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_documents_render_a_contained_error_panel() {
        let root = UiNode::element("", primitives::TEXT);
        let document = document(root);
        let area = Rect::new(0, 0, 32, 5);
        let mut buffer = Buffer::empty(area);
        let output = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(output.diagnostics[0].code, "remote-ui.invalid-document");
        assert!(buffer_snapshot(&buffer).contains("Remote UI rejected"));
    }

    // Keep open-ended protocol names representable in tests; this catches an
    // accidental future replacement with a closed enum in either layer.
    #[test]
    fn custom_primitive_and_operation_names_remain_open() {
        assert_eq!(UiPrimitive::from("vendor/Widget").as_str(), "vendor/Widget");
        assert_eq!(
            UiPatchOperation::from("vendorPatch").as_str(),
            "vendorPatch"
        );
    }

    fn sdk_graph_document(direction: &str, edges: serde_json::Value) -> UiDocument {
        serde_json::from_value(serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "graph-golden",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "graph",
                "type": "Graph",
                "props": {
                    "nodes": [
                        {"id": "build", "label": "build", "status": "completed"},
                        {"id": "test", "label": "test", "status": "running"},
                        {"id": "deploy", "label": "deploy", "status": "failed"}
                    ],
                    "edges": edges,
                    "direction": direction,
                    "accessibleLabel": "Workflow graph"
                },
                "children": []
            }
        }))
        .expect("SDK graph document deserializes")
    }

    #[test]
    fn graph_paints_layered_dag_from_top_level_sdk_edges() {
        let document = sdk_graph_document(
            "vertical",
            serde_json::json!([
                {"id": "e1", "from": "build", "to": "test"},
                {"id": "e2", "from": "build", "to": "deploy"}
            ]),
        );
        let area = Rect::new(0, 0, 60, 6);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(
            buffer_snapshot(&buffer),
            "✓ build\n   ├─────────┐\n● test   ✕ deploy"
        );
    }

    #[test]
    fn graph_honors_horizontal_direction() {
        let document = sdk_graph_document(
            "horizontal",
            serde_json::json!([{"id": "e1", "from": "build", "to": "test"}]),
        );
        let area = Rect::new(0, 0, 60, 4);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let snapshot = buffer_snapshot(&buffer);
        // Column 0 (build/deploy), a 3-cell "───" bridge, then column 1 (test).
        assert_eq!(snapshot, "✓ build ───● test\n✕ deploy");
    }

    #[test]
    fn graph_degrades_to_adjacency_list_when_narrow() {
        let document = sdk_graph_document(
            "vertical",
            serde_json::json!([{"id": "e1", "from": "build", "to": "test"}]),
        );
        // Narrower than the collapse breakpoint: fidelity ladder drops to text.
        let area = Rect::new(0, 0, 30, 6);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(buffer_snapshot(&buffer), "build → test\n○ test\n○ deploy");
    }

    #[test]
    fn graph_still_accepts_legacy_per_node_edge_lists() {
        let wire = serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "graph-legacy",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "graph",
                "type": "Graph",
                "props": {
                    "nodes": [
                        {"id": "a", "label": "alpha", "targets": ["b"]},
                        {"id": "b", "label": "beta"}
                    ]
                },
                "children": []
            }
        });
        let document: UiDocument = serde_json::from_value(wire).expect("legacy graph parses");
        let area = Rect::new(0, 0, 60, 4);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(buffer_snapshot(&buffer), "alpha\n  │\nbeta");
    }

    #[test]
    fn graph_routes_layer_skipping_edges_through_waypoints() {
        let document = sdk_graph_document(
            "vertical",
            serde_json::json!([
                {"from": "build", "to": "test"},
                {"from": "test", "to": "deploy"},
                {"from": "build", "to": "deploy"}
            ]),
        );
        let area = Rect::new(0, 0, 60, 8);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let snapshot = buffer_snapshot(&buffer);
        // build feeds test and a waypoint; both converge on deploy.
        assert!(snapshot.contains("✓ build"), "{snapshot}");
        assert!(snapshot.contains("● test"), "{snapshot}");
        assert!(snapshot.contains("✕ deploy"), "{snapshot}");
        assert!(snapshot.contains('│'), "{snapshot}");
        assert!(
            snapshot.contains('┐') || snapshot.contains('┴'),
            "{snapshot}"
        );
    }

    #[test]
    fn split_honors_ratio_and_vertical_direction() {
        let wire = serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "split-golden",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "split",
                "type": "Split",
                "props": {"ratio": 0.75},
                "children": [
                    {"kind": "element", "id": "left", "type": "Text", "props": {"value": "L"}, "children": []},
                    {"kind": "element", "id": "right", "type": "Text", "props": {"value": "R"}, "children": []}
                ]
            }
        });
        let document: UiDocument = serde_json::from_value(wire).expect("split parses");
        let area = Rect::new(0, 0, 80, 2);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        let first = buffer_snapshot(&buffer)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let left = first.find('L').expect("left pane painted");
        let right = first.find('R').expect("right pane painted");
        assert_eq!(left, 0);
        // 0.75 of 80 columns → the second pane starts at column 60.
        assert_eq!(right, 60);

        let vertical = serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "split-vertical",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "split",
                "type": "Split",
                "props": {"ratio": 0.5, "direction": "vertical"},
                "children": [
                    {"kind": "element", "id": "top", "type": "Text", "props": {"value": "T"}, "children": []},
                    {"kind": "element", "id": "bottom", "type": "Text", "props": {"value": "B"}, "children": []}
                ]
            }
        });
        let document: UiDocument = serde_json::from_value(vertical).expect("vertical split parses");
        let area = Rect::new(0, 0, 20, 4);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        assert_eq!(buffer_snapshot(&buffer), "T\n\nB");
    }

    #[test]
    fn grid_columns_count_and_track_list_shape_the_grid() {
        let wire = serde_json::json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "grid-columns",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "grid",
                "type": "Grid",
                "props": {"columns": 2},
                "children": [
                    {"kind": "element", "id": "a", "type": "Text", "props": {"value": "A"}, "children": []},
                    {"kind": "element", "id": "b", "type": "Text", "props": {"value": "B"}, "children": []},
                    {"kind": "element", "id": "c", "type": "Text", "props": {"value": "C"}, "children": []}
                ]
            }
        });
        let document: UiDocument = serde_json::from_value(wire).expect("grid parses");
        let area = Rect::new(0, 0, 60, 4);
        let mut buffer = Buffer::empty(area);
        let _ = render_remote_ui(
            &mut buffer,
            area,
            &document,
            &Theme::dark(),
            &TerminalUiCapabilities::native(),
            &RemoteUiViewState::default(),
            RemoteUiRenderOptions::default(),
        );
        // Two equal fr tracks over 60 cells: B starts on the second 30-cell
        // track and the third child wraps to the next grid row.
        let snapshot = buffer_snapshot(&buffer);
        let mut lines = snapshot.lines();
        let first = lines.next().unwrap_or_default();
        assert_eq!(first.chars().next(), Some('A'), "{snapshot}");
        assert_eq!(
            first.chars().position(|char| char == 'B'),
            Some(30),
            "{snapshot}"
        );
        assert_eq!(lines.next().map(str::trim_end), Some("C"), "{snapshot}");
    }

    #[test]
    fn measure_is_memoized_within_a_frame() {
        let mut root = UiNode::element("root", primitives::STACK);
        root.children = (0..24)
            .map(|index| text(&format!("text-{index}"), "Measured content line"))
            .collect();
        let document = document(root);
        let area = Rect::new(0, 0, 40, 30);
        let mut buffer = Buffer::empty(area);
        let theme = Theme::dark();
        let capabilities = TerminalUiCapabilities::native();
        let state = RemoteUiViewState::default();
        let painter = Painter {
            buffer: &mut buffer,
            clip: area,
            theme: &theme,
            capabilities: &capabilities,
            state: &state,
            options: RemoteUiRenderOptions::default(),
            output: RemoteUiRenderOutput::default(),
            visited: 0,
            focus_sequence: 0,
            visibility_clip: area,
            measure_cache: RefCell::new(HashMap::new()),
        };
        let first = painter.measure(&document.root, 40, 0);
        assert_eq!(
            painter.measure_cache.borrow().len(),
            25,
            "root plus children"
        );
        let repeat = painter.measure(&document.root, 40, 0);
        assert_eq!(first, repeat);
        assert_eq!(
            painter.measure_cache.borrow().len(),
            25,
            "a repeated measure is served from the frame cache"
        );
        // A different available width is a different measurement, not a hit.
        let narrow = painter.measure(&document.root, 12, 0);
        assert!(narrow >= first);
        assert!(painter.measure_cache.borrow().len() > 25);
    }
}

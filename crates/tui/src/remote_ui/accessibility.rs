use codypendent_protocol::remote_ui::{node_kinds, primitives, UiNode, UiSemanticRole};
use serde_json::Value;

use super::{
    resolve_node, AccessibilityNode, AccessibilityProjection, ResolvedNode, TerminalUiCapabilities,
};
use crate::remote_ui::text::sanitize_terminal_text;

/// Build a viewport-independent screen-reader/plain-text representation.
/// Hidden nodes and purely decorative layout containers are omitted.
#[must_use]
pub fn project_accessibility(
    root: &UiNode,
    capabilities: &TerminalUiCapabilities,
) -> AccessibilityProjection {
    let mut projection = AccessibilityProjection::default();
    walk(root, capabilities, &mut projection, 0);
    projection.plain_text = projection
        .plain_text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    projection
}

fn walk(
    original: &UiNode,
    capabilities: &TerminalUiCapabilities,
    projection: &mut AccessibilityProjection,
    depth: usize,
) {
    match resolve_node(original, capabilities) {
        ResolvedNode::Plain(text) => append_line(projection, depth, &text),
        ResolvedNode::Node(node) => {
            let accessibility = node.props.accessibility.as_ref();
            if accessibility.is_some_and(|metadata| metadata.hidden) {
                return;
            }
            if node.kind.as_str() == node_kinds::TEXT {
                if let Some(text) = &node.text {
                    append_line(projection, depth, text);
                }
                return;
            }

            let primitive = node
                .node_type
                .as_ref()
                .map_or("Component", |value| value.as_str());
            let label = label(node, primitive);
            let role = accessibility
                .and_then(|metadata| metadata.role.clone())
                .or_else(|| node.props.role.clone())
                .unwrap_or_else(|| inferred_role(primitive));
            let disabled = node
                .props
                .input
                .as_ref()
                .is_some_and(|input| input.disabled)
                || node
                    .props
                    .navigation
                    .as_ref()
                    .and_then(|navigation| navigation.disabled)
                    .unwrap_or(false);
            if !is_layout_only(primitive) || !label.is_empty() {
                projection.nodes.push(AccessibilityNode {
                    node_id: node.id.clone(),
                    role,
                    label: label.clone(),
                    description: accessibility.and_then(|metadata| metadata.description.clone()),
                    keyboard_hint: accessibility
                        .and_then(|metadata| metadata.keyboard_hint.clone()),
                    live_region: accessibility.and_then(|metadata| metadata.live_region.clone()),
                    heading_level: accessibility.and_then(|metadata| metadata.heading_level),
                    disabled,
                });
            }

            if should_announce_own_content(primitive) {
                let own = own_text(node, primitive);
                if !own.is_empty() {
                    append_line(projection, depth, &own);
                }
            }
            for child in &node.children {
                walk(
                    child,
                    capabilities,
                    projection,
                    depth + usize::from(is_nested(primitive)),
                );
            }
        }
    }
}

fn append_line(projection: &mut AccessibilityProjection, depth: usize, text: &str) {
    let clean = sanitize_terminal_text(text);
    for line in clean.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !projection.plain_text.is_empty() {
            projection.plain_text.push('\n');
        }
        projection.plain_text.push_str(&"  ".repeat(depth.min(8)));
        projection.plain_text.push_str(line.trim());
    }
}

fn label(node: &UiNode, primitive: &str) -> String {
    node.props
        .accessibility
        .as_ref()
        .and_then(|metadata| metadata.label.as_ref().or(metadata.text_fallback.as_ref()))
        .cloned()
        .or_else(|| string_attribute(node, "label"))
        .or_else(|| {
            node.props
                .content
                .as_ref()
                .and_then(|content| content.alternate_text.clone())
        })
        .or_else(|| {
            node.props
                .feedback
                .as_ref()
                .and_then(|feedback| feedback.message.clone())
        })
        .or_else(|| {
            node.props
                .input
                .as_ref()
                .and_then(|input| input.name.clone())
        })
        .unwrap_or_else(|| {
            if is_layout_only(primitive) {
                String::new()
            } else {
                primitive.to_owned()
            }
        })
}

fn own_text(node: &UiNode, primitive: &str) -> String {
    if let Some(text) = node
        .props
        .accessibility
        .as_ref()
        .and_then(|metadata| metadata.text_fallback.clone())
    {
        return text;
    }
    match primitive {
        primitives::CHECKBOX => {
            let checked = input_bool(node);
            format!(
                "{} {}",
                if checked {
                    "[checked]"
                } else {
                    "[not checked]"
                },
                label(node, primitive)
            )
        }
        primitives::RADIO => {
            let selected = input_bool(node);
            format!(
                "{} {}",
                if selected {
                    "[selected]"
                } else {
                    "[not selected]"
                },
                label(node, primitive)
            )
        }
        primitives::PROGRESS => {
            let feedback = node.props.feedback.as_ref();
            let current = feedback.and_then(|value| value.current).unwrap_or(0.0);
            let maximum = feedback.and_then(|value| value.maximum).unwrap_or(100.0);
            format!("{}: {current:.0} of {maximum:.0}", label(node, primitive))
        }
        primitives::TABLE => table_text(node),
        primitives::KEY_VALUE | primitives::JSON_TREE => node
            .props
            .structured_data
            .as_ref()
            .and_then(|data| data.schema.as_ref())
            .or(node.props.value.as_ref())
            .map(value_text)
            .unwrap_or_default(),
        primitives::IMAGE | primitives::AUDIO => label(node, primitive),
        _ => node
            .props
            .content
            .as_ref()
            .and_then(|content| content.text.clone())
            .or_else(|| {
                node.props
                    .feedback
                    .as_ref()
                    .and_then(|feedback| feedback.message.clone())
            })
            .or_else(|| string_attribute(node, "title"))
            .or_else(|| string_attribute(node, "label"))
            .unwrap_or_default(),
    }
}

fn table_text(node: &UiNode) -> String {
    let Some(data) = node.props.structured_data.as_ref() else {
        return String::new();
    };
    let mut lines = Vec::new();
    if !data.columns.is_empty() {
        lines.push(
            data.columns
                .iter()
                .map(|column| column.label.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    for item in &data.items {
        if let Some(object) = item.as_object() {
            lines.push(
                data.columns
                    .iter()
                    .map(|column| object.get(&column.id).map(value_text).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
        } else {
            lines.push(value_text(item));
        }
    }
    lines.join("\n")
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string_pretty(value).unwrap_or_default()
        }
    }
}

fn input_bool(node: &UiNode) -> bool {
    node.props
        .input
        .as_ref()
        .and_then(|input| input.value.as_ref().or(input.default_value.as_ref()))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn string_attribute(node: &UiNode, key: &str) -> Option<String> {
    node.props
        .attributes
        .get(key)
        .or_else(|| node.props.extension.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn inferred_role(primitive: &str) -> UiSemanticRole {
    let role = match primitive {
        primitives::BUTTON => "button",
        primitives::LINK => "link",
        primitives::TEXT_INPUT | primitives::TEXT_AREA => "textbox",
        primitives::CHECKBOX => "checkbox",
        primitives::RADIO => "radio",
        primitives::SELECT | primitives::MULTI_SELECT => "combobox",
        primitives::TABLE => "table",
        primitives::TREE | primitives::JSON_TREE => "tree",
        primitives::LIST | primitives::VIRTUAL_LIST => "list",
        primitives::ALERT | primitives::TOAST => "alert",
        primitives::PROGRESS => "progressbar",
        primitives::TABS => "tablist",
        primitives::FORM => "form",
        _ => "group",
    };
    UiSemanticRole::from(role)
}

fn is_layout_only(primitive: &str) -> bool {
    matches!(
        primitive,
        primitives::BOX
            | primitives::STACK
            | primitives::ROW
            | primitives::GRID
            | primitives::SPLIT
            | primitives::SPACER
            | primitives::SCROLL_AREA
            | primitives::PORTAL
            | primitives::FORM
            | primitives::TOOLBAR
    )
}

fn is_nested(primitive: &str) -> bool {
    matches!(
        primitive,
        primitives::LIST
            | primitives::VIRTUAL_LIST
            | primitives::TREE
            | primitives::MENU
            | primitives::COMMAND_LIST
            | primitives::FORM
    )
}

fn should_announce_own_content(primitive: &str) -> bool {
    !matches!(
        primitive,
        primitives::BOX
            | primitives::STACK
            | primitives::ROW
            | primitives::GRID
            | primitives::SPLIT
            | primitives::SCROLL_AREA
            | primitives::PORTAL
            | primitives::FORM
            | primitives::TOOLBAR
            | primitives::ACTION_MENU
            | primitives::CONTEXT_MENU
    )
}

#[cfg(test)]
mod tests {
    use codypendent_protocol::remote_ui::{UiContent, UiNode, UiPrimitive};

    use super::*;

    #[test]
    fn projection_uses_fallback_and_omits_hidden_content() {
        let mut root = UiNode::element("root", UiPrimitive::from(primitives::STACK));
        let mut unsupported = UiNode::element("image", "Vendor.Hologram");
        unsupported.props.accessibility = Some(codypendent_protocol::remote_ui::UiAccessibility {
            text_fallback: Some("a useful fallback".into()),
            ..Default::default()
        });
        let mut hidden = UiNode::element("hidden", primitives::TEXT);
        hidden.props.content = Some(UiContent {
            text: Some("secret decoration".into()),
            ..Default::default()
        });
        hidden.props.accessibility = Some(codypendent_protocol::remote_ui::UiAccessibility {
            hidden: true,
            ..Default::default()
        });
        root.children = vec![unsupported, hidden];
        let projection = project_accessibility(&root, &TerminalUiCapabilities::native());
        assert_eq!(projection.plain_text, "a useful fallback");
    }
}

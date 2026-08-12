//! Canonical SDK flat-prop aliases lowered into the renderer's semantic views.

use codypendent_protocol::{
    UiAccessibility, UiActionBinding, UiCapability, UiContent, UiData, UiDataColumn, UiDimension,
    UiDocument, UiEdges, UiEventType, UiFeedback, UiInput, UiInputOption, UiLayout, UiNavigation,
    UiNode, UiResourceReference, UiStyle,
};
use serde_json::{Map, Value};

pub(super) fn normalize_document(document: &UiDocument) -> UiDocument {
    let mut document = document.clone();
    normalize_node(&mut document.root);
    document
}

fn normalize_node(node: &mut UiNode) {
    let grid = is_grid(node);
    let extension = &node.props.extension;

    let layout = node.props.layout.get_or_insert_with(UiLayout::default);
    layout.gap = layout.gap.or_else(|| size(extension.get("gap")));
    layout.row_gap = layout.row_gap.or_else(|| size(extension.get("rowGap")));
    layout.column_gap = layout
        .column_gap
        .or_else(|| size(extension.get("columnGap")));
    layout.padding = layout.padding.or_else(|| edges(extension.get("padding")));
    layout.margin = layout.margin.or_else(|| edges(extension.get("margin")));
    layout.width = layout
        .width
        .clone()
        .or_else(|| dimension(extension.get("width")));
    layout.height = layout
        .height
        .clone()
        .or_else(|| dimension(extension.get("height")));
    layout.min_width = layout
        .min_width
        .clone()
        .or_else(|| dimension(extension.get("minWidth")));
    layout.max_width = layout
        .max_width
        .clone()
        .or_else(|| dimension(extension.get("maxWidth")));
    layout.min_height = layout
        .min_height
        .clone()
        .or_else(|| dimension(extension.get("minHeight")));
    layout.max_height = layout
        .max_height
        .clone()
        .or_else(|| dimension(extension.get("maxHeight")));
    layout.grow = layout
        .grow
        .or_else(|| extension.get("grow").and_then(Value::as_f64));
    layout.shrink = layout
        .shrink
        .or_else(|| extension.get("shrink").and_then(Value::as_f64));
    layout.align = layout.align.clone().or_else(|| string(extension, "align"));
    layout.justify = layout
        .justify
        .clone()
        .or_else(|| string(extension, "justify"));
    layout.direction = layout
        .direction
        .clone()
        .or_else(|| string(extension, "direction"));
    layout.wrap = layout.wrap.clone().or_else(|| {
        extension.get("wrap").and_then(|value| match value {
            Value::Bool(true) => Some("wrap".to_owned()),
            Value::Bool(false) => Some("nowrap".to_owned()),
            Value::String(value) => Some(value.clone()),
            _ => None,
        })
    });
    // Grid-only: the flat `columns` prop is a track count or a track list.
    // (Table reuses the `columns` name for column descriptors, lifted below.)
    if layout.columns.is_empty() && grid {
        layout.columns = grid_columns(extension.get("columns"));
    }
    if *layout == UiLayout::default() {
        node.props.layout = None;
    }

    let style = node.props.style.get_or_insert_with(UiStyle::default);
    style.tone = style.tone.clone().or_else(|| string(extension, "tone"));
    style.border_style = style.border_style.clone().or_else(|| {
        extension.get("border").and_then(|value| match value {
            Value::Bool(true) => Some("single".to_owned()),
            Value::String(value) => Some(value.clone()),
            _ => None,
        })
    });
    style.visibility = style.visibility.clone().or_else(|| {
        extension
            .get("hidden")
            .and_then(Value::as_bool)
            .filter(|hidden| *hidden)
            .map(|_| "hidden".to_owned())
    });
    style.truncate = style.truncate.clone().or_else(|| {
        extension
            .get("truncate")
            .and_then(Value::as_bool)
            .filter(|truncate| *truncate)
            .map(|_| "end".to_owned())
    });
    for (key, emphasis) in [
        ("weight", "bold"),
        ("italic", "italic"),
        ("underline", "underline"),
    ] {
        let enabled = match extension.get(key) {
            Some(Value::String(value)) if key == "weight" => {
                matches!(value.as_str(), "medium" | "bold")
            }
            Some(Value::Bool(value)) => *value,
            _ => false,
        };
        if enabled && !style.emphasis.iter().any(|value| value == emphasis) {
            style.emphasis.push(emphasis.to_owned());
        }
    }
    if *style == UiStyle::default() {
        node.props.style = None;
    }

    let accessibility = node
        .props
        .accessibility
        .get_or_insert_with(UiAccessibility::default);
    accessibility.label = accessibility
        .label
        .clone()
        .or_else(|| string(extension, "accessibleLabel"));
    accessibility.description = accessibility
        .description
        .clone()
        .or_else(|| string(extension, "description"));
    accessibility.keyboard_hint = accessibility
        .keyboard_hint
        .clone()
        .or_else(|| string(extension, "shortcut"));
    accessibility.hidden |= extension
        .get("hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if *accessibility == UiAccessibility::default() {
        node.props.accessibility = None;
    }

    normalize_content(node);
    normalize_data(node);
    normalize_feedback(node);
    normalize_navigation(node);
    normalize_input(node);
    normalize_bindings(node);

    for child in &mut node.children {
        normalize_node(child);
    }
}

fn normalize_content(node: &mut UiNode) {
    let extension = &node.props.extension;
    let content = node.props.content.get_or_insert_with(UiContent::default);
    content.text = content.text.clone().or_else(|| {
        node.props
            .value
            .as_ref()
            .map(value_text)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                ["source", "patch", "value", "message", "transcript"]
                    .into_iter()
                    .find_map(|key| string(extension, key))
            })
            .or_else(|| {
                extension
                    .get("lines")
                    .and_then(Value::as_array)
                    .map(|lines| lines.iter().map(value_text).collect::<Vec<_>>().join("\n"))
            })
            .or_else(|| {
                let before = string(extension, "before")?;
                let after = string(extension, "after").unwrap_or_default();
                Some(format!("--- before\n{before}\n+++ after\n{after}"))
            })
    });
    content.language = content
        .language
        .clone()
        .or_else(|| string(extension, "language"));
    content.alternate_text = content
        .alternate_text
        .clone()
        .or_else(|| string(extension, "alt"))
        .or_else(|| string(extension, "caption"));
    content.line_wrap = content.line_wrap.clone().or_else(|| {
        extension
            .get("wrap")
            .and_then(Value::as_bool)
            .map(|wrap| if wrap { "wrap" } else { "clip" }.to_owned())
    });
    if content.resource.is_none() {
        if let Some(uri) = string(extension, "src") {
            content.resource = Some(UiResourceReference {
                uri,
                media_type: "application/octet-stream".to_owned(),
                digest: None,
                byte_length: None,
            });
        }
    }
    if *content == UiContent::default() {
        node.props.content = None;
    }
}

fn normalize_data(node: &mut UiNode) {
    let grid = is_grid(node);
    let extension = &node.props.extension;
    let data = node
        .props
        .structured_data
        .get_or_insert_with(UiData::default);
    if data.items.is_empty() {
        data.items = ["rows", "items", "nodes", "data", "values", "lines"]
            .into_iter()
            .find_map(|key| extension.get(key).and_then(Value::as_array).cloned())
            .unwrap_or_default();
    }
    // A Grid's `columns` is a track template lifted into `UiLayout`, never a
    // table column descriptor list.
    if data.columns.is_empty() && !grid {
        data.columns = extension
            .get("columns")
            .and_then(Value::as_array)
            .map(|columns| {
                columns
                    .iter()
                    .filter_map(|column| match column {
                        Value::String(id) => Some(UiDataColumn {
                            id: id.clone(),
                            label: id.clone(),
                            value_type: None,
                            width: None,
                            sortable: false,
                        }),
                        Value::Object(column) => {
                            let id = object_string(column, "key")
                                .or_else(|| object_string(column, "id"))?;
                            Some(UiDataColumn {
                                label: object_string(column, "label").unwrap_or_else(|| id.clone()),
                                id,
                                value_type: object_string(column, "type"),
                                width: dimension(column.get("width")),
                                sortable: column
                                    .get("sortable")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    if data.selected_ids.is_empty() {
        data.selected_ids = extension
            .get("selectedKey")
            .and_then(Value::as_str)
            .map(|key| vec![key.to_owned()])
            .or_else(|| {
                extension
                    .get("expandedKeys")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
            })
            .unwrap_or_default();
    }
    if data.schema.is_none() {
        let mut schema = Map::new();
        for key in ["edges", "entries"] {
            if let Some(value) = extension.get(key) {
                schema.insert(key.to_owned(), value.clone());
            }
        }
        if !schema.is_empty() {
            data.schema = Some(Value::Object(schema));
        }
    }
    data.total = data
        .total
        .or_else(|| extension.get("total").and_then(Value::as_u64));
    if *data == UiData::default() {
        node.props.structured_data = None;
    }
}

fn normalize_feedback(node: &mut UiNode) {
    let extension = &node.props.extension;
    let feedback = node.props.feedback.get_or_insert_with(UiFeedback::default);
    feedback.status = feedback
        .status
        .clone()
        .or_else(|| string(extension, "status"));
    feedback.tone = feedback.tone.clone().or_else(|| string(extension, "tone"));
    feedback.message = feedback
        .message
        .clone()
        .or_else(|| string(extension, "message"))
        .or_else(|| string(extension, "title"))
        .or_else(|| string(extension, "emptyMessage"));
    feedback.current = feedback
        .current
        .or_else(|| node.props.value.as_ref().and_then(Value::as_f64));
    feedback.maximum = feedback
        .maximum
        .or_else(|| extension.get("maximum").and_then(Value::as_f64));
    feedback.indeterminate = feedback
        .indeterminate
        .or_else(|| extension.get("indeterminate").and_then(Value::as_bool));
    if *feedback == UiFeedback::default() {
        node.props.feedback = None;
    }
}

fn normalize_navigation(node: &mut UiNode) {
    let extension = &node.props.extension;
    let navigation = node
        .props
        .navigation
        .get_or_insert_with(UiNavigation::default);
    navigation.destination = navigation
        .destination
        .clone()
        .or_else(|| string(extension, "href"));
    navigation.target = navigation
        .target
        .clone()
        .or_else(|| string(extension, "target"));
    navigation.selected = navigation.selected.or_else(|| {
        extension
            .get("selected")
            .or_else(|| extension.get("checked"))
            .and_then(Value::as_bool)
    });
    navigation.expanded = navigation
        .expanded
        .or_else(|| extension.get("open").and_then(Value::as_bool));
    navigation.disabled = navigation
        .disabled
        .or_else(|| extension.get("disabled").and_then(Value::as_bool));
    if *navigation == UiNavigation::default() {
        node.props.navigation = None;
    }
}

fn normalize_input(node: &mut UiNode) {
    let Some(primitive) = node.node_type.as_ref().map(|value| value.as_str()) else {
        return;
    };
    if !matches!(
        primitive,
        "TextInput" | "TextArea" | "Select" | "MultiSelect" | "Checkbox" | "Radio"
    ) {
        return;
    }
    let extension = &node.props.extension;
    let input = node.props.input.get_or_insert_with(UiInput::default);
    input.name = input.name.clone().or_else(|| string(extension, "name"));
    input.input_type = input
        .input_type
        .clone()
        .or_else(|| Some(primitive.to_owned()));
    input.value = input
        .value
        .clone()
        .or_else(|| node.props.value.clone())
        .or_else(|| extension.get("checked").cloned());
    input.default_value = input
        .default_value
        .clone()
        .or_else(|| extension.get("defaultValue").cloned());
    input.placeholder = input
        .placeholder
        .clone()
        .or_else(|| string(extension, "placeholder"));
    input.validation_message = input
        .validation_message
        .clone()
        .or_else(|| string(extension, "validationMessage"));
    input.required |= bool_value(extension, "required");
    input.read_only |= bool_value(extension, "readOnly");
    input.disabled |= bool_value(extension, "disabled");
    if input.options.is_empty() {
        input.options = extension
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .enumerate()
                    .filter_map(|(index, option)| match option {
                        Value::String(value) => Some(UiInputOption {
                            id: value.clone(),
                            label: value.clone(),
                            value: Value::String(value.clone()),
                            disabled: false,
                            description: None,
                        }),
                        Value::Object(option) => {
                            let value = option.get("value")?.clone();
                            let id = option
                                .get("id")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| value_text(&value));
                            Some(UiInputOption {
                                id: if id.is_empty() {
                                    format!("option-{index}")
                                } else {
                                    id
                                },
                                label: object_string(option, "label")
                                    .unwrap_or_else(|| value_text(&value)),
                                value,
                                disabled: option
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                                description: object_string(option, "description"),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
}

fn normalize_bindings(node: &mut UiNode) {
    let extension = &node.props.extension;
    let requires = node
        .requires
        .iter()
        .filter(|requirement| !requirement.optional)
        .map(|requirement| requirement.feature.clone())
        .collect::<Vec<UiCapability>>();
    let payload = extension.get("payload").cloned().unwrap_or(Value::Null);
    let disabled = bool_value(extension, "disabled");
    let confirmation = string(extension, "confirmation");
    for (property, event) in [
        ("action", "action"),
        ("changeAction", "change"),
        ("submitAction", "submit"),
        ("selectAction", "select"),
        ("navigateAction", "navigate"),
        ("validateAction", "custom"),
        ("dismissAction", "action"),
        ("resetAction", "action"),
    ] {
        if let Some(action_id) = string(extension, property) {
            let event = UiEventType::from(event);
            if !node
                .props
                .event_bindings
                .iter()
                .any(|binding| binding.event == event)
            {
                node.props.event_bindings.push(UiActionBinding {
                    event,
                    action_id: action_id.into(),
                    payload: payload.clone(),
                    requires: requires.clone(),
                    disabled,
                    confirmation: confirmation.clone(),
                });
            }
        }
    }

    // React callbacks remain worker-local, but their canonical event names are
    // serialized so a host can expose focus/hit metadata and safely forward
    // the original event. This synthetic binding exists only in the painter's
    // cloned semantic view; it is never interpreted as daemon command authority.
    if let Some(events) = extension
        .get("eventHandlers")
        .or_else(|| extension.get("events"))
        .and_then(Value::as_array)
    {
        for event in events.iter().filter_map(Value::as_str) {
            let event = UiEventType::from(event);
            if !node
                .props
                .event_bindings
                .iter()
                .any(|binding| binding.event == event)
            {
                node.props.event_bindings.push(UiActionBinding {
                    event,
                    action_id: "component.local".into(),
                    payload: Value::Null,
                    requires: Vec::new(),
                    disabled,
                    confirmation: None,
                });
            }
        }
    }
}

fn string(values: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<String> {
    values.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn object_string(values: &Map<String, Value>, key: &str) -> Option<String> {
    values.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_value(values: &std::collections::BTreeMap<String, Value>, key: &str) -> bool {
    values.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn size(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => Some(match value.as_str() {
            "xs" => 0.0,
            "sm" => 1.0,
            "md" => 2.0,
            "lg" => 3.0,
            "xl" => 4.0,
            _ => return None,
        }),
        _ => None,
    }
}

fn edges(value: Option<&Value>) -> Option<UiEdges> {
    match value? {
        Value::Number(_) | Value::String(_) => {
            let value = size(value)?;
            Some(UiEdges {
                top: value,
                right: value,
                bottom: value,
                left: value,
            })
        }
        Value::Object(values) => Some(UiEdges {
            top: size(values.get("top")).unwrap_or(0.0),
            right: size(values.get("right")).unwrap_or(0.0),
            bottom: size(values.get("bottom")).unwrap_or(0.0),
            left: size(values.get("left")).unwrap_or(0.0),
        }),
        _ => None,
    }
}

fn is_grid(node: &UiNode) -> bool {
    node.node_type
        .as_ref()
        .is_some_and(|value| value.as_str() == "Grid")
}

/// Grid `columns`: an equal-track count (`3` → `1fr 1fr 1fr`) or an explicit
/// track list (numbers are cells; strings accept `fr`, `%`, and plain cells).
fn grid_columns(value: Option<&Value>) -> Vec<UiDimension> {
    match value {
        Some(Value::Number(count)) => {
            let count = count
                .as_f64()
                .filter(|count| count.is_finite() && *count >= 1.0)
                .map_or(0, |count| count.trunc().min(24.0) as usize);
            vec![
                UiDimension {
                    value: 1.0,
                    unit: "fr".to_owned(),
                };
                count
            ]
        }
        Some(Value::Array(tracks)) => tracks
            .iter()
            .filter_map(|track| dimension(Some(track)))
            .collect(),
        _ => Vec::new(),
    }
}

fn dimension(value: Option<&Value>) -> Option<UiDimension> {
    match value? {
        Value::Number(value) => Some(UiDimension {
            value: value.as_f64()?,
            unit: "cells".to_owned(),
        }),
        Value::String(value) if value == "full" => Some(UiDimension {
            value: 100.0,
            unit: "percent".to_owned(),
        }),
        Value::String(value) if value == "auto" => Some(UiDimension {
            value: 0.0,
            unit: "auto".to_owned(),
        }),
        Value::String(value) if value.ends_with('%') => Some(UiDimension {
            value: value.trim_end_matches('%').parse().ok()?,
            unit: "percent".to_owned(),
        }),
        Value::String(value) if value.ends_with("fr") => Some(UiDimension {
            value: value.trim_end_matches("fr").parse().ok()?,
            unit: "fr".to_owned(),
        }),
        Value::String(value) => value.parse().ok().map(|value| UiDimension {
            value,
            unit: "cells".to_owned(),
        }),
        _ => None,
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(value_text).collect::<Vec<_>>().join(", "),
        Value::Object(value) => serde_json::to_string(value).unwrap_or_default(),
    }
}

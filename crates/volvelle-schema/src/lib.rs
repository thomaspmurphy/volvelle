//! The volvelle component catalog and wire protocol.
//!
//! A UI is a tree of [`Node`]s drawn from a small, fixed catalog. The agent
//! combines catalog pieces; it never writes rendering code. The fixed catalog
//! is what keeps generated output valid, and its JSON Schema is the MCP tool's
//! input schema. Atoms state intent; the viewer owns sizing, focus order and
//! colour.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// One component in the UI tree.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    /// Lays out children in a row or column, optionally with a border.
    Box(BoxNode),
    /// Plain text, wrapped to the available width.
    Text(TextNode),
    /// A small Markdown subset: headings, bullets, **bold**, *italic* and `code`.
    Markdown(MarkdownNode),
    /// A read-only table.
    Table(TableNode),
    /// A list. With an `id` it is selectable and Enter emits a `select` event.
    List(ListNode),
    /// A text field, single-line unless `multiline`.
    Input(InputNode),
    /// Pick one option, or several with `multi`.
    Select(SelectNode),
    /// A boolean checkbox.
    Toggle(ToggleNode),
    /// Pressing a button emits a `submit` event carrying every field's value.
    Button(ButtonNode),
    /// A progress bar from 0.0 to 1.0.
    Progress(ProgressNode),
    /// A horizontal rule, optionally titled.
    Divider(DividerNode),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Column,
    Row,
}

/// Semantic colour. The viewer maps tones to terminal colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Normal,
    Muted,
    Accent,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoxNode {
    #[serde(default)]
    pub direction: Direction,
    /// Draw a rounded border around the box.
    #[serde(default)]
    pub border: bool,
    /// Title shown in the border. Implies `border`.
    pub title: Option<String>,
    #[serde(default)]
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextNode {
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub align: Align,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkdownNode {
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableNode {
    pub title: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Relative column widths, one per header. Equal widths if omitted.
    pub widths: Option<Vec<u16>>,
}

/// An option in a list or select: either a bare string, or a label with a
/// distinct value and an optional dimmed detail.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Item {
    Plain(String),
    Full {
        label: String,
        /// Reported in events. Defaults to the label.
        value: Option<String>,
        detail: Option<String>,
    },
}

impl Item {
    pub fn label(&self) -> &str {
        match self {
            Item::Plain(s) => s,
            Item::Full { label, .. } => label,
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Item::Plain(s) => s,
            Item::Full { label, value, .. } => value.as_deref().unwrap_or(label),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Item::Plain(_) => None,
            Item::Full { detail, .. } => detail.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListNode {
    /// Makes the list selectable. Enter emits `select` with the item's value.
    pub id: Option<String>,
    pub title: Option<String>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputNode {
    pub id: String,
    pub label: Option<String>,
    pub placeholder: Option<String>,
    /// Initial text.
    #[serde(default)]
    pub value: String,
    /// Mask the text, for passwords and tokens.
    #[serde(default)]
    pub secret: bool,
    /// Several lines, for commit messages and notes. Enter inserts a newline
    /// and the value keeps it as `\n`.
    #[serde(default)]
    pub multiline: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectNode {
    pub id: String,
    pub label: Option<String>,
    pub options: Vec<Item>,
    /// Allow several choices. The value is then an array.
    #[serde(default)]
    pub multi: bool,
    /// Initially chosen option values.
    #[serde(default)]
    pub value: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToggleNode {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub value: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonNode {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressNode {
    pub label: Option<String>,
    /// From 0.0 to 1.0.
    pub value: f64,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DividerNode {
    pub title: Option<String>,
}

impl Node {
    /// The id of an interactive node, if it has one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Node::List(n) => n.id.as_deref(),
            Node::Input(n) => Some(&n.id),
            Node::Select(n) => Some(&n.id),
            Node::Toggle(n) => Some(&n.id),
            Node::Button(n) => Some(&n.id),
            _ => None,
        }
    }

    pub fn children(&self) -> &[Node] {
        match self {
            Node::Box(b) => &b.children,
            _ => &[],
        }
    }

    /// Depth-first walk over this node and its descendants.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        for child in self.children() {
            child.walk(f);
        }
    }
}

/// Checks what the JSON Schema cannot: unique ids and consistent values.
pub fn validate(root: &Node) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    root.walk(&mut |node| {
        if let Some(id) = node.id() {
            if id.is_empty() {
                errors.push("empty id".to_string());
            } else if !seen.insert(id.to_string()) {
                errors.push(format!("duplicate id {id:?}"));
            }
        }
        match node {
            Node::Select(s) => {
                if s.options.is_empty() {
                    errors.push(format!("select {:?} has no options", s.id));
                }
                if !s.multi && s.value.len() > 1 {
                    errors.push(format!(
                        "select {:?} is not multi but has several values",
                        s.id
                    ));
                }
                for v in &s.value {
                    if !s.options.iter().any(|o| o.value() == v) {
                        errors.push(format!("select {:?} value {v:?} is not an option", s.id));
                    }
                }
            }
            Node::Table(t) => {
                if let Some(w) = &t.widths
                    && w.len() != t.headers.len()
                {
                    errors.push("table widths must match headers".to_string());
                }
            }
            Node::Progress(p) if !(0.0..=1.0).contains(&p.value) => {
                errors.push(format!("progress value {} is outside 0.0..=1.0", p.value));
            }
            _ => {}
        }
    });
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Parses and validates a spec from JSON.
pub fn parse_spec(value: Value) -> Result<Node, String> {
    let node: Node = serde_json::from_value(value).map_err(|e| format!("invalid spec: {e}"))?;
    validate(&node)?;
    Ok(node)
}

/// The shape `wait_event` returns for this spec: every UI is a typed
/// function call, and this is its return type.
pub fn result_schema(root: &Node) -> Value {
    let mut fields = Map::new();
    let mut buttons = Vec::new();
    let mut lists = Vec::new();
    root.walk(&mut |node| match node {
        Node::Input(n) => {
            fields.insert(n.id.clone(), json!({"type": "string"}));
        }
        Node::Select(n) => {
            let options: Vec<&str> = n.options.iter().map(Item::value).collect();
            let schema = if n.multi {
                json!({"type": "array", "items": {"enum": options}})
            } else {
                json!({"enum": options})
            };
            fields.insert(n.id.clone(), schema);
        }
        Node::Toggle(n) => {
            fields.insert(n.id.clone(), json!({"type": "boolean"}));
        }
        Node::Button(n) => buttons.push(n.id.clone()),
        Node::List(ListNode {
            id: Some(id),
            items,
            ..
        }) => {
            let values: Vec<&str> = items.iter().map(Item::value).collect();
            lists.push(json!({
                "type": "object",
                "properties": {
                    "type": {"const": "select"},
                    "id": {"const": id},
                    "value": {"enum": values},
                    "values": {"$ref": "#/$defs/values"}
                }
            }));
        }
        _ => {}
    });
    let mut variants = vec![json!({
        "type": "object",
        "properties": {"type": {"const": "cancel"}, "values": {"$ref": "#/$defs/values"}}
    })];
    if !buttons.is_empty() {
        variants.push(json!({
            "type": "object",
            "properties": {
                "type": {"const": "submit"},
                "button": {"enum": buttons},
                "values": {"$ref": "#/$defs/values"}
            }
        }));
    }
    variants.extend(lists);
    json!({
        "oneOf": variants,
        "$defs": {"values": {"type": "object", "properties": fields}}
    })
}

/// JSON Schema for a [`Node`] tree.
pub fn node_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Node)).expect("schema serialises")
}

/// Messages from the MCP server (or any client) to the viewer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Show a spec as the named view. Re-rendering the same view keeps what
    /// the user has typed or chosen in fields whose ids survive, so nothing
    /// moves under them. A different view name starts from the spec's values.
    Render { view: String, spec: Node },
    /// Quit the viewer.
    Close,
}

/// Messages from the viewer back to the client. Each user event names the
/// view it answers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A button was pressed.
    Submit {
        view: String,
        button: String,
        values: Map<String, Value>,
    },
    /// Enter was pressed on a selectable list item.
    Select {
        view: String,
        id: String,
        value: String,
        values: Map<String, Value>,
    },
    /// The user pressed Esc.
    Cancel {
        view: String,
        values: Map<String, Value>,
    },
    /// The viewer is shutting down.
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_nested_spec() {
        let spec = json!({
            "type": "box", "title": "Deploy", "children": [
                {"type": "text", "text": "Ship it?", "tone": "accent"},
                {"type": "select", "id": "env", "options": ["staging", {"label": "Production", "value": "prod"}], "value": ["prod"]},
                {"type": "button", "id": "go", "label": "Deploy"}
            ]
        });
        parse_spec(spec).unwrap();
    }

    #[test]
    fn rejects_unknown_fields_and_duplicate_ids() {
        let unknown = json!({"type": "text", "text": "x", "colour": "red"});
        assert!(parse_spec(unknown).is_err());
        let dup = json!({"type": "box", "children": [
            {"type": "toggle", "id": "a", "label": "A"},
            {"type": "button", "id": "a", "label": "B"}
        ]});
        assert!(parse_spec(dup).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn derives_result_schema() {
        let spec = parse_spec(json!({"type": "box", "children": [
            {"type": "toggle", "id": "force", "label": "Force"},
            {"type": "select", "id": "env", "options": ["a", "b"], "multi": true},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}))
        .unwrap();
        let schema = result_schema(&spec);
        assert_eq!(
            schema["$defs"]["values"]["properties"]["force"]["type"],
            "boolean"
        );
        assert_eq!(
            schema["$defs"]["values"]["properties"]["env"]["type"],
            "array"
        );
        assert_eq!(schema["oneOf"][1]["properties"]["button"]["enum"][0], "ok");
    }
}

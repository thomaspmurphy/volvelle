//! The volvelle component catalog and wire protocol.
//!
//! A UI is a tree of [`Node`]s drawn from a small, fixed catalog. The agent
//! combines catalog pieces; it never writes rendering code. The fixed catalog
//! is what keeps generated output valid, and its JSON Schema is the MCP tool's
//! input schema. Atoms state intent; the viewer owns sizing, focus order and
//! colour. Patterns sit one level up: named, versioned compositions with
//! slots, a typed answer and their own session type.

use std::collections::HashMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

mod bind;
mod graph;
pub mod library;
mod patch;
mod tree;
pub use bind::{bind, check_binds, resolve, write_back};
pub use graph::{Edge, Flow, GraphNode, MAX_EDGES, MAX_NODES, Vertex};
pub use patch::{Op, apply_patch, touches};
pub use tree::{Flat, TreeItem, TreeNode};

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
    /// Nested items that fold. With an `id` it is selectable and Enter emits
    /// a `select` event.
    Tree(TreeNode),
    /// Nodes and edges, laid out and drawn by the viewer, with an optional
    /// highlighted node. With an `id` it is selectable and Enter emits a
    /// `select` event.
    Graph(GraphNode),
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
    /// A named, versioned composition. See [`patterns`].
    Pattern(Pattern),
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
    pub text: Option<String>,
    /// A JSON Pointer to a string in the data document, in place of `text`.
    pub bind: Option<String>,
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
    pub text: Option<String>,
    /// A JSON Pointer to a string in the data document, in place of `text`.
    pub bind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableNode {
    pub title: Option<String>,
    pub headers: Vec<String>,
    pub rows: Option<Vec<Vec<String>>>,
    /// A JSON Pointer to an array of rows in the data document, in place of
    /// `rows`.
    pub bind: Option<String>,
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
    pub items: Option<Vec<Item>>,
    /// A JSON Pointer to an array of items in the data document, in place of
    /// `items`.
    pub bind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputNode {
    pub id: String,
    pub label: Option<String>,
    pub placeholder: Option<String>,
    /// Initial text.
    pub value: Option<String>,
    /// A JSON Pointer to a string in the data document, in place of `value`.
    /// Submitting writes the text back there.
    pub bind: Option<String>,
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
    /// A JSON Pointer to the chosen option's value in the data document, in
    /// place of `value`. Single selects only; submitting writes the choice
    /// back there.
    pub bind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToggleNode {
    pub id: String,
    pub label: String,
    pub value: Option<bool>,
    /// A JSON Pointer to a boolean in the data document, in place of
    /// `value`. Submitting writes the state back there.
    pub bind: Option<String>,
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
    pub value: Option<f64>,
    /// A JSON Pointer to a number from 0.0 to 1.0 in the data document, in
    /// place of `value`.
    pub bind: Option<String>,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DividerNode {
    pub title: Option<String>,
}

// Content as drawn. A bound atom has it once [`resolve`] has filled it in.

impl TextNode {
    pub fn text(&self) -> &str {
        self.text.as_deref().unwrap_or_default()
    }
}

impl MarkdownNode {
    pub fn text(&self) -> &str {
        self.text.as_deref().unwrap_or_default()
    }
}

impl TableNode {
    pub fn rows(&self) -> &[Vec<String>] {
        self.rows.as_deref().unwrap_or_default()
    }
}

impl ListNode {
    pub fn items(&self) -> &[Item] {
        self.items.as_deref().unwrap_or_default()
    }
}

impl InputNode {
    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or_default()
    }
}

impl ToggleNode {
    pub fn value(&self) -> bool {
        self.value.unwrap_or_default()
    }
}

impl ProgressNode {
    pub fn value(&self) -> f64 {
        self.value.unwrap_or_default()
    }
}

/// A pattern instance, tagged by its versioned name. Slots are plain data,
/// never nodes, so patterns cannot nest.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "name")]
pub enum Pattern {
    /// Asks before something that cannot be undone. Focus starts on Cancel.
    #[serde(rename = "confirm-destructive@1")]
    ConfirmDestructive(ConfirmDestructive),
    /// A list beside a preview of the item under the cursor.
    #[serde(rename = "choose-with-preview@1")]
    ChooseWithPreview(ChooseWithPreview),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmDestructive {
    pub id: String,
    pub title: Option<String>,
    /// What is about to happen, in Markdown.
    pub summary: String,
    /// What cannot be undone. Shown in the error tone.
    pub consequence: String,
    #[serde(default = "confirm_label")]
    pub confirm_label: String,
    #[serde(default = "cancel_label")]
    pub cancel_label: String,
    /// Confirm stays disabled until the user types exactly this.
    pub require_typing: Option<String>,
}

fn confirm_label() -> String {
    "Confirm".into()
}

fn cancel_label() -> String {
    "Cancel".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChooseWithPreview {
    pub id: String,
    pub title: Option<String>,
    pub items: Vec<PreviewItem>,
    /// Space toggles items and the answer is an array.
    #[serde(default)]
    pub multi: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewItem {
    pub label: String,
    /// Reported in the answer. Defaults to the label.
    pub value: Option<String>,
    pub detail: Option<String>,
    /// Shown beside the list while the cursor is on this item, in Markdown.
    pub preview: String,
}

impl PreviewItem {
    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or(&self.label)
    }
}

/// What the agent owes once a pattern has answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Obligation {
    None,
    /// Render the same view again to report what happened.
    ReportOutcome,
}

/// One pattern's declaration. Tool descriptions and the session monitor read
/// it, so each pattern is described in one place.
pub struct PatternInfo {
    /// The versioned name specs use.
    pub name: &'static str,
    pub summary: &'static str,
    /// Informal session type, from the agent's side.
    pub protocol: &'static str,
    /// The answer's `value`, informally.
    pub returns: &'static str,
    /// JSON Schema of the slots.
    pub slots: fn() -> Value,
    /// What the agent owes after the pattern answers with this value.
    pub after_answer: fn(&Value) -> Obligation,
}

pub static CONFIRM_DESTRUCTIVE: PatternInfo = PatternInfo {
    name: "confirm-destructive@1",
    summary: "Asks before an action that cannot be undone. Focus starts on Cancel, Confirm is \
        drawn in the error tone, and with require_typing it stays disabled until the user types \
        that text. Must be the only interactive element in its view.",
    protocol: "!Spec . ?{confirm: !Outcome . end, cancel: end}: after a confirmed answer, \
        render the same view again to report what happened",
    returns: r#"{"confirmed": bool}"#,
    slots: slot_schema::<ConfirmDestructive>,
    after_answer: |v| {
        if v["confirmed"] == true {
            Obligation::ReportOutcome
        } else {
            Obligation::None
        }
    },
};

pub static CHOOSE_WITH_PREVIEW: PatternInfo = PatternInfo {
    name: "choose-with-preview@1",
    summary: "A list with a Markdown preview of the item under the cursor, updated without a \
        round trip. Enter answers; with multi, Space toggles items.",
    protocol: "!Spec . ?{answer: end, cancel: end}",
    returns: r#"{"selected": value}, or {"selected": [value]} with multi"#,
    slots: slot_schema::<ChooseWithPreview>,
    after_answer: |_| Obligation::None,
};

static PATTERNS: &[&PatternInfo] = &[&CONFIRM_DESTRUCTIVE, &CHOOSE_WITH_PREVIEW];

/// Every pattern the catalog knows.
pub fn patterns() -> &'static [&'static PatternInfo] {
    PATTERNS
}

fn slot_schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serialises")
}

impl Pattern {
    pub fn info(&self) -> &'static PatternInfo {
        match self {
            Pattern::ConfirmDestructive(_) => &CONFIRM_DESTRUCTIVE,
            Pattern::ChooseWithPreview(_) => &CHOOSE_WITH_PREVIEW,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Pattern::ConfirmDestructive(p) => &p.id,
            Pattern::ChooseWithPreview(p) => &p.id,
        }
    }

    /// JSON Schema of this instance's answer value.
    pub fn value_schema(&self) -> Value {
        match self {
            Pattern::ConfirmDestructive(_) => json!({
                "type": "object",
                "properties": {"confirmed": {"type": "boolean"}},
                "required": ["confirmed"]
            }),
            Pattern::ChooseWithPreview(p) => {
                let values: Vec<&str> = p.items.iter().map(PreviewItem::value).collect();
                let selected = if p.multi {
                    json!({"type": "array", "items": {"enum": values}})
                } else {
                    json!({"enum": values})
                };
                json!({
                    "type": "object",
                    "properties": {"selected": selected},
                    "required": ["selected"]
                })
            }
        }
    }
}

impl Node {
    /// The id of an interactive node, if it has one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Node::List(n) => n.id.as_deref(),
            Node::Tree(n) => n.id.as_deref(),
            Node::Graph(n) => n.id.as_deref(),
            Node::Input(n) => Some(&n.id),
            Node::Select(n) => Some(&n.id),
            Node::Toggle(n) => Some(&n.id),
            Node::Button(n) => Some(&n.id),
            Node::Pattern(p) => Some(p.id()),
            _ => None,
        }
    }

    /// The data pointer a bound atom reads, if it has one.
    pub fn bind(&self) -> Option<&str> {
        match self {
            Node::Text(n) => n.bind.as_deref(),
            Node::Markdown(n) => n.bind.as_deref(),
            Node::Table(n) => n.bind.as_deref(),
            Node::List(n) => n.bind.as_deref(),
            Node::Input(n) => n.bind.as_deref(),
            Node::Select(n) => n.bind.as_deref(),
            Node::Toggle(n) => n.bind.as_deref(),
            Node::Progress(n) => n.bind.as_deref(),
            Node::Graph(n) => n.bind.as_deref(),
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
        self.walk_paths("", &mut |n, _| f(n));
    }

    /// Like [`Node::walk`], passing each node's JSON Pointer in the spec.
    pub fn walk_paths<'a>(&'a self, path: &str, f: &mut impl FnMut(&'a Node, &str)) {
        f(self, path);
        for (i, child) in self.children().iter().enumerate() {
            child.walk_paths(&format!("{path}/children/{i}"), f);
        }
    }
}

/// A broken rule, naming where it broke and what would be accepted instead.
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    /// JSON Pointer into the spec; empty for the root.
    pub path: String,
    pub rule: String,
    pub allowed: Vec<String>,
}

impl Violation {
    fn new(path: impl Into<String>, rule: impl Into<String>, allowed: &[&str]) -> Self {
        Violation {
            path: path.into(),
            rule: rule.into(),
            allowed: allowed.iter().map(|a| a.to_string()).collect(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let path = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };
        write!(f, "at {path}: {}", self.rule)?;
        if !self.allowed.is_empty() {
            write!(f, "; allowed: {}", self.allowed.join(", "))?;
        }
        Ok(())
    }
}

/// Checks what the JSON Schema cannot: unique ids, consistent values, and
/// how patterns may be placed.
pub fn validate(root: &Node) -> Result<(), Vec<Violation>> {
    let mut seen: HashMap<&str, String> = HashMap::new();
    let mut interactive: Vec<(String, &str)> = Vec::new();
    let mut patterns: Vec<(String, &Pattern)> = Vec::new();
    let mut errors = Vec::new();
    root.walk_paths("", &mut |node, path| {
        if let Some(id) = node.id() {
            let at = format!("{path}/id");
            if id.is_empty() {
                errors.push(Violation::new(
                    at,
                    "empty id",
                    &["a short name such as \"env\""],
                ));
            } else if id.contains(':') {
                errors.push(Violation::new(
                    at,
                    format!("id {id:?} contains `:`, which is reserved for pattern parts"),
                    &[&format!("{:?}", id.replace(':', "-"))],
                ));
            } else if let Some(first) = seen.get(id) {
                errors.push(Violation::new(
                    at,
                    format!("duplicate id {id:?}, first used at {first}"),
                    &["a unique id"],
                ));
            } else {
                seen.insert(id, path.to_string());
            }
            interactive.push((path.to_string(), id));
        }
        bind::literals(node, path, &mut errors);
        match node {
            Node::Select(s) => {
                if s.options.is_empty() {
                    errors.push(Violation::new(
                        format!("{path}/options"),
                        format!("select {:?} has no options", s.id),
                        &["at least one option", "an input"],
                    ));
                }
                if !s.multi && s.value.len() > 1 {
                    errors.push(Violation::new(
                        format!("{path}/value"),
                        format!("select {:?} is not multi but has several values", s.id),
                        &["one value", "\"multi\": true"],
                    ));
                }
                let options: Vec<String> = s
                    .options
                    .iter()
                    .map(|o| format!("{:?}", o.value()))
                    .collect();
                let options: Vec<&str> = options.iter().map(String::as_str).collect();
                for (i, v) in s.value.iter().enumerate() {
                    if !s.options.iter().any(|o| o.value() == v) {
                        errors.push(Violation::new(
                            format!("{path}/value/{i}"),
                            format!("select {:?} value {v:?} is not an option", s.id),
                            &options,
                        ));
                    }
                }
            }
            Node::Table(t) => {
                if let Some(w) = &t.widths
                    && w.len() != t.headers.len()
                {
                    errors.push(Violation::new(
                        format!("{path}/widths"),
                        format!(
                            "table has {} headers but {} widths",
                            t.headers.len(),
                            w.len()
                        ),
                        &[&format!("{} widths", t.headers.len()), "no widths"],
                    ));
                }
            }
            Node::Progress(ProgressNode { value: Some(v), .. }) if !(0.0..=1.0).contains(v) => {
                errors.push(Violation::new(
                    format!("{path}/value"),
                    format!("progress value {v} is outside 0.0..=1.0"),
                    &["a value from 0.0 to 1.0"],
                ));
            }
            Node::Pattern(p) => {
                if let Some((first, _)) = patterns.first() {
                    errors.push(Violation::new(
                        path,
                        format!("a view holds at most one pattern, and {first} is one"),
                        &["render this pattern in a later view"],
                    ));
                }
                patterns.push((path.to_string(), p));
                pattern_slots(p, path, &mut errors);
            }
            Node::Tree(t) => {
                for (at, rule, allowed) in t.problems() {
                    errors.push(Violation::new(format!("{path}{at}"), rule, allowed));
                }
            }
            Node::Graph(g) => errors.extend(g.problems(path)),
            _ => {}
        }
    });
    for (path, p) in &patterns {
        let Pattern::ConfirmDestructive(c) = p else {
            continue;
        };
        let others: Vec<String> = interactive
            .iter()
            .filter(|(_, id)| *id != c.id)
            .map(|(at, id)| format!("{id:?} at {at}"))
            .collect();
        if !others.is_empty() {
            errors.push(Violation::new(
                path.as_str(),
                format!(
                    "confirm-destructive@1 must be the only interactive element in its view, \
                     but the view also has {}",
                    others.join(", ")
                ),
                &[
                    "render the other fields in an earlier view",
                    "a toggle plus a button instead of the pattern",
                ],
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn pattern_slots(p: &Pattern, path: &str, errors: &mut Vec<Violation>) {
    match p {
        Pattern::ConfirmDestructive(c) => {
            if c.require_typing.as_deref() == Some("") {
                errors.push(Violation::new(
                    format!("{path}/require_typing"),
                    "require_typing is empty, so it guards nothing",
                    &["the name of what is destroyed", "no require_typing"],
                ));
            }
        }
        Pattern::ChooseWithPreview(c) => {
            if c.items.is_empty() {
                errors.push(Violation::new(
                    format!("{path}/items"),
                    "choose-with-preview@1 has no items",
                    &["at least one item"],
                ));
            }
        }
    }
}

/// Parses and validates a spec from JSON.
pub fn parse_spec(value: Value) -> Result<Node, String> {
    let node: Node = serde_json::from_value(value).map_err(|e| format!("invalid spec: {e}"))?;
    validate(&node).map_err(|errors| {
        let lines: Vec<String> = errors.iter().map(Violation::to_string).collect();
        lines.join("\n")
    })?;
    Ok(node)
}

/// The shape `wait_event` returns for this spec: every UI is a typed
/// function call, and this is its return type.
pub fn result_schema(root: &Node) -> Value {
    let mut fields = Map::new();
    let mut buttons = Vec::new();
    let mut lists = Vec::new();
    let mut answers = Vec::new();
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
        Node::List(l @ ListNode { id: Some(id), .. }) => {
            let values: Vec<&str> = l.items().iter().map(Item::value).collect();
            lists.push(select_variant(id, &values));
        }
        Node::Tree(t @ TreeNode { id: Some(id), .. }) => {
            let answers = t.answers();
            let values: Vec<&str> = answers.iter().map(String::as_str).collect();
            lists.push(select_variant(id, &values));
        }
        Node::Graph(g @ GraphNode { id: Some(id), .. }) => {
            lists.push(select_variant(id, &g.ids()));
        }
        Node::Pattern(p) => answers.push(json!({
            "type": "object",
            "properties": {
                "type": {"const": "answer"},
                "id": {"const": p.id()},
                "value": p.value_schema(),
                "values": {"$ref": "#/$defs/values"}
            }
        })),
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
    variants.extend(answers);
    json!({
        "oneOf": variants,
        "$defs": {"values": {"type": "object", "properties": fields}}
    })
}

/// The `select` answer of a list, tree or graph, whose value is one of `values`.
fn select_variant(id: &str, values: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": {"const": "select"},
            "id": {"const": id},
            "value": {"enum": values},
            "values": {"$ref": "#/$defs/values"}
        }
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
    /// `data` fills the spec's binds; a bound field the user has not edited
    /// takes its value from it.
    Render {
        view: String,
        spec: Node,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    /// Apply a patch to the view's data and redraw what is bound to it,
    /// keeping focus, scroll and any bound field the user has edited. The
    /// viewer acknowledges with [`Event::Updated`].
    Update { view: String, patch: Vec<Op> },
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
    /// Enter was pressed on a selectable list item, tree node or graph node.
    Select {
        view: String,
        id: String,
        value: String,
        values: Map<String, Value>,
    },
    /// A pattern answered with its typed value.
    Answer {
        view: String,
        id: String,
        value: Value,
        values: Map<String, Value>,
    },
    /// The user pressed Esc.
    Cancel {
        view: String,
        values: Map<String, Value>,
    },
    /// The user saved the view with Ctrl-S. Not an answer: it never ends a
    /// wait.
    Saved {
        view: String,
        name: String,
        scope: library::Scope,
        path: String,
    },
    /// The viewer applied an update. `kept` lists the pointers of bound
    /// fields the update touched but the user had edited, so their values
    /// stayed. Not an answer.
    Updated { view: String, kept: Vec<String> },
    /// The viewer is shutting down.
    Closed,
}

impl Event {
    /// The field values an answer carries.
    pub fn values(&self) -> Option<&Map<String, Value>> {
        match self {
            Event::Submit { values, .. }
            | Event::Select { values, .. }
            | Event::Answer { values, .. }
            | Event::Cancel { values, .. } => Some(values),
            Event::Saved { .. } | Event::Updated { .. } | Event::Closed => None,
        }
    }
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

    fn confirm() -> Value {
        json!({"type": "pattern", "name": "confirm-destructive@1", "id": "drop",
               "summary": "Drop `users`", "consequence": "All rows are lost"})
    }

    fn errors(spec: Value) -> Vec<Violation> {
        let node: Node = serde_json::from_value(spec).unwrap();
        validate(&node).unwrap_err()
    }

    #[test]
    fn parses_patterns_with_defaults() {
        let Node::Pattern(Pattern::ConfirmDestructive(c)) = parse_spec(confirm()).unwrap() else {
            panic!("expected confirm-destructive")
        };
        assert_eq!(
            (c.confirm_label.as_str(), c.cancel_label.as_str()),
            ("Confirm", "Cancel")
        );
        let choose = json!({"type": "pattern", "name": "choose-with-preview@1", "id": "pick",
                            "items": [{"label": "a", "preview": "# A"}]});
        let spec = parse_spec(choose).unwrap();
        assert_eq!(spec.id(), Some("pick"));
    }

    #[test]
    fn rejects_unknown_slots_and_names() {
        let mut extra = confirm();
        extra["colour"] = "red".into();
        assert!(parse_spec(extra).unwrap_err().contains("colour"));
        let mut nested = confirm();
        nested["children"] = json!([confirm()]);
        assert!(parse_spec(nested).is_err());
        let mut unknown = confirm();
        unknown["name"] = "confirm-destructive@2".into();
        assert!(parse_spec(unknown).is_err());
    }

    #[test]
    fn names_match_declarations() {
        let choose =
            json!({"type": "pattern", "name": "choose-with-preview@1", "id": "c", "items": []});
        for spec in [confirm(), choose] {
            let Node::Pattern(p) = serde_json::from_value(spec.clone()).unwrap() else {
                panic!("expected a pattern")
            };
            assert_eq!(spec["name"], p.info().name);
            assert!(patterns().iter().any(|i| i.name == p.info().name));
        }
    }

    #[test]
    fn violations_name_path_rule_and_alternatives() {
        let errs = errors(json!({"type": "box", "children": [
            {"type": "toggle", "id": "force", "label": "Force"},
            {"type": "box", "children": [confirm()]},
            {"type": "pattern", "name": "choose-with-preview@1", "id": "a:b", "items": []},
            {"type": "progress", "value": 2.0}
        ]}));
        let text: Vec<String> = errs.iter().map(Violation::to_string).collect();
        assert_eq!(
            text,
            [
                "at /children/2/id: id \"a:b\" contains `:`, which is reserved for pattern parts; allowed: \"a-b\"",
                "at /children/2: a view holds at most one pattern, and /children/1/children/0 is one; allowed: render this pattern in a later view",
                "at /children/2/items: choose-with-preview@1 has no items; allowed: at least one item",
                "at /children/3/value: progress value 2 is outside 0.0..=1.0; allowed: a value from 0.0 to 1.0",
                "at /children/1/children/0: confirm-destructive@1 must be the only interactive element in its view, but the view also has \"force\" at /children/0, \"a:b\" at /children/2; allowed: render the other fields in an earlier view, a toggle plus a button instead of the pattern",
            ]
        );
        let dup = errors(json!({"type": "box", "children": [
            {"type": "toggle", "id": "a", "label": "A"},
            {"type": "select", "id": "a", "options": ["x"], "value": ["y"]}
        ]}));
        assert_eq!(dup[0].path, "/children/1/id");
        assert!(dup[0].rule.contains("first used at /children/0"));
        assert_eq!(dup[1].allowed, ["\"x\""]);
    }

    #[test]
    fn pattern_answers_join_the_result_schema() {
        let spec = parse_spec(json!({"type": "box", "children": [
            {"type": "markdown", "text": "Pick one"},
            {"type": "pattern", "name": "choose-with-preview@1", "id": "pick", "multi": true,
             "items": [{"label": "A", "value": "a", "preview": "a"}, {"label": "b", "preview": "b"}]}
        ]}))
        .unwrap();
        let answer = &result_schema(&spec)["oneOf"][1];
        assert_eq!(answer["properties"]["type"]["const"], "answer");
        assert_eq!(answer["properties"]["values"]["$ref"], "#/$defs/values");
        assert_eq!(answer["properties"]["id"]["const"], "pick");
        assert_eq!(
            answer["properties"]["value"]["properties"]["selected"]["items"]["enum"],
            json!(["a", "b"])
        );
        let confirm = result_schema(&parse_spec(confirm()).unwrap());
        assert_eq!(
            confirm["oneOf"][1]["properties"]["value"]["properties"]["confirmed"]["type"],
            "boolean"
        );
    }

    #[test]
    fn examples_parse() {
        for example in [
            include_str!("../../../examples/deploy.json"),
            include_str!("../../../examples/long.json"),
            include_str!("../../../examples/confirm.json"),
            include_str!("../../../examples/choose.json"),
            include_str!("../../../examples/tree.json"),
        ] {
            parse_spec(serde_json::from_str(example).unwrap()).unwrap();
        }
        let sheet = include_str!("../../../examples/bound-sheet.json");
        let data = include_str!("../../../examples/bound-sheet.data.json");
        let sheet = parse_spec(serde_json::from_str(sheet).unwrap()).unwrap();
        bind(&sheet, Some(&serde_json::from_str(data).unwrap())).unwrap();
    }

    #[test]
    fn confirm_obliges_an_outcome_only_when_confirmed() {
        let after = CONFIRM_DESTRUCTIVE.after_answer;
        assert_eq!(
            after(&json!({"confirmed": true})),
            Obligation::ReportOutcome
        );
        assert_eq!(after(&json!({"confirmed": false})), Obligation::None);
    }

    fn tree() -> Value {
        json!({"type": "tree", "id": "file", "title": "Files", "nodes": [
            {"label": "src", "children": [
                {"label": "app", "open": false, "children": [{"label": "main.rs"}]},
                {"label": "lib.rs", "value": "lib", "detail": "2 KB"}
            ]},
            {"label": "Cargo.toml"}
        ]})
    }

    #[test]
    fn tree_values_default_to_label_paths() {
        let Node::Tree(t) = parse_spec(tree()).unwrap() else {
            panic!("expected a tree")
        };
        let values: Vec<String> = t.flatten().into_iter().map(|f| f.value).collect();
        assert_eq!(
            values,
            ["src", "src/app", "src/app/main.rs", "lib", "Cargo.toml"]
        );
        assert!(t.nodes[0].open && !t.nodes[0].children[0].open);
        let schema = result_schema(&Node::Tree(t));
        assert_eq!(schema["oneOf"][1]["properties"]["id"]["const"], "file");
        assert_eq!(
            schema["oneOf"][1]["properties"]["value"]["enum"],
            json!(values)
        );
    }

    #[test]
    fn leaves_only_tree_answers_with_leaves() {
        let mut spec = tree();
        spec["leaves_only"] = json!(true);
        let schema = result_schema(&parse_spec(spec).unwrap());
        assert_eq!(
            schema["oneOf"][1]["properties"]["value"]["enum"],
            json!(["src/app/main.rs", "lib", "Cargo.toml"])
        );
    }

    #[test]
    fn rejects_bad_trees() {
        let dup = json!({"type": "tree", "nodes": [
            {"label": "a", "children": [{"label": "b"}]},
            {"label": "a/b"}
        ]});
        assert!(
            parse_spec(dup)
                .unwrap_err()
                .contains("\"a/b\" is not unique")
        );
        let empty = json!({"type": "tree", "id": "t", "nodes": []});
        assert!(parse_spec(empty).unwrap_err().contains("no nodes"));
        parse_spec(json!({"type": "tree", "nodes": []})).unwrap();
        let unknown = json!({"type": "tree", "nodes": [{"label": "a", "folded": true}]});
        assert!(parse_spec(unknown).is_err());
        let Node::Tree(t) = serde_json::from_value(json!({"type": "tree", "nodes": [
            {"label": "a", "children": [{"label": "b"}, {"label": "c", "value": "a/b"}]}
        ]}))
        .unwrap() else {
            panic!("expected a tree")
        };
        assert_eq!(t.problems()[0].0, "/nodes/0/children/1");
        let err = parse_spec(
            json!({"type": "box", "children": [{"type": "tree", "id": "t", "nodes": []}]}),
        )
        .unwrap_err();
        assert!(err.starts_with("at /children/0/nodes: selectable tree has no nodes; allowed: "));
    }

    fn graph() -> Value {
        json!({"type": "graph", "id": "state", "nodes": [
            {"id": "idle"}, {"id": "busy", "label": "Busy", "tone": "warning"}, {"id": "done"}
        ], "edges": [
            {"from": "idle", "to": "busy", "label": "start"},
            {"from": "busy", "to": "busy"},
            {"from": "busy", "to": "idle"},
            {"from": "busy", "to": "done"}
        ], "current": "busy"})
    }

    #[test]
    fn graphs_parse_with_defaults_and_answer_with_node_ids() {
        let Node::Graph(g) = parse_spec(graph()).unwrap() else {
            panic!("expected a graph")
        };
        assert_eq!(g.direction, Flow::Down);
        assert_eq!(g.nodes[0].label(), "idle");
        assert_eq!(g.nodes[1].label(), "Busy");
        let schema = result_schema(&Node::Graph(g));
        let select = &schema["oneOf"][1]["properties"];
        assert_eq!(select["type"]["const"], "select");
        assert_eq!(select["id"]["const"], "state");
        assert_eq!(select["value"]["enum"], json!(["idle", "busy", "done"]));

        let mut display = graph();
        display.as_object_mut().unwrap().remove("id");
        display["direction"] = json!("right");
        let schema = result_schema(&parse_spec(display).unwrap());
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_graphs() {
        let spec = json!({"type": "box", "children": [{"type": "graph", "nodes": [
            {"id": "a"}, {"id": ""}, {"id": "a"}, {"id": "b"}
        ], "edges": [
            {"from": "a", "to": "b"},
            {"from": "c", "to": "a"},
            {"from": "b", "to": "d", "label": "x"}
        ], "current": "e"}]});
        let text: Vec<String> = errors(spec).iter().map(Violation::to_string).collect();
        assert_eq!(
            text,
            [
                "at /children/0/nodes/1/id: graph node id is empty; allowed: a short name such as \"idle\"",
                "at /children/0/nodes/2/id: graph node id \"a\" is not unique, first used at /children/0/nodes/0; allowed: a distinct \"id\" on this node",
                "at /children/0/edges/1/from: edge from \"c\" names no node of the graph; allowed: \"a\", \"b\"",
                "at /children/0/edges/2/to: edge to \"d\" names no node of the graph; allowed: \"a\", \"b\"",
                "at /children/0/current: current \"e\" names no node of the graph; allowed: \"a\", \"b\"",
            ]
        );

        let many: Vec<Value> = (0..=MAX_NODES)
            .map(|i| json!({"id": format!("n{i}")}))
            .collect();
        let edges: Vec<Value> = (0..=MAX_EDGES)
            .map(|_| json!({"from": "n0", "to": "n1"}))
            .collect();
        let big = errors(json!({"type": "graph", "nodes": many, "edges": edges}));
        let text: Vec<String> = big.iter().map(Violation::to_string).collect();
        assert_eq!(
            text,
            [
                "at /nodes: graph has 65 nodes, more than 64; allowed: at most 64 nodes, several smaller graphs",
                "at /edges: graph has 257 edges, more than 256; allowed: at most 256 edges, several smaller graphs",
            ]
        );

        let empty = json!({"type": "graph", "id": "g", "nodes": []});
        assert!(
            parse_spec(empty)
                .unwrap_err()
                .contains("selectable graph has no nodes")
        );
        parse_spec(json!({"type": "graph", "nodes": []})).unwrap();
        let unknown = json!({"type": "graph", "nodes": [{"id": "a", "shape": "circle"}]});
        assert!(parse_spec(unknown).is_err());
        let direction = json!({"type": "graph", "direction": "up", "nodes": []});
        assert!(parse_spec(direction).is_err());
    }
}

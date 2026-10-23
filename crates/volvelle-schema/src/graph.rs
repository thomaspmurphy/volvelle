//! The `graph` atom: nodes and edges that the viewer lays out in layers,
//! with an optional highlighted node, selectable with an id.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Tone, Violation};

/// A graph holds at most this many nodes.
pub const MAX_NODES: usize = 64;
/// A graph holds at most this many edges.
pub const MAX_EDGES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    /// Makes the graph selectable. Enter emits `select` with the node's id.
    pub id: Option<String>,
    /// Title shown in a border around the graph.
    pub title: Option<String>,
    /// Which way edges run, from earlier layers to later ones.
    #[serde(default)]
    pub direction: Flow,
    pub nodes: Vec<Vertex>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// The id of the node to highlight, such as the current state.
    pub current: Option<String>,
    /// A JSON Pointer to the highlighted node's id in the data document, in
    /// place of `current`. Updating it moves the highlight.
    pub bind: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    #[default]
    Down,
    Right,
}

/// One node of a graph.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Vertex {
    /// Unique within the graph. Edges, `current` and answers use it.
    pub id: String,
    /// Shown in the node's box. Defaults to the id.
    pub label: Option<String>,
    /// Colours the node's border.
    #[serde(default)]
    pub tone: Tone,
    /// Drawn but never focused, highlighted or answered, like a state
    /// machine's entry point. Patterns set it; specs cannot.
    #[serde(skip)]
    pub inert: bool,
}

impl Vertex {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

/// A directed edge. Self-loops and parallel edges are allowed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
}

impl GraphNode {
    /// The node ids, in order, which are also what Enter can answer with.
    pub fn ids(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter(|n| !n.inert)
            .map(|n| n.id.as_str())
            .collect()
    }

    /// The node ids quoted, as the alternatives a violation offers.
    pub(crate) fn quoted_ids(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for n in &self.nodes {
            let q = format!("{:?}", n.id);
            if !n.id.is_empty() && !seen.contains(&q) {
                seen.push(q);
            }
        }
        seen
    }

    /// Broken rules, each at a JSON Pointer under `path`, the graph node's
    /// own pointer.
    pub(crate) fn problems(&self, path: &str) -> Vec<Violation> {
        let mut out = Vec::new();
        let ids = self.quoted_ids();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        if self.nodes.len() > MAX_NODES {
            out.push(Violation::new(
                format!("{path}/nodes"),
                format!(
                    "graph has {} nodes, more than {MAX_NODES}",
                    self.nodes.len()
                ),
                &[
                    &format!("at most {MAX_NODES} nodes"),
                    "several smaller graphs",
                ],
            ));
        }
        if self.edges.len() > MAX_EDGES {
            out.push(Violation::new(
                format!("{path}/edges"),
                format!(
                    "graph has {} edges, more than {MAX_EDGES}",
                    self.edges.len()
                ),
                &[
                    &format!("at most {MAX_EDGES} edges"),
                    "several smaller graphs",
                ],
            ));
        }
        if self.id.is_some() && self.nodes.is_empty() {
            out.push(Violation::new(
                format!("{path}/nodes"),
                "selectable graph has no nodes",
                &["at least one node", "no id, for a display-only graph"],
            ));
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let at = format!("{path}/nodes/{i}/id");
            if n.id.is_empty() {
                out.push(Violation::new(
                    at,
                    "graph node id is empty",
                    &["a short name such as \"idle\""],
                ));
            } else if let Some(first) = seen.get(n.id.as_str()) {
                out.push(Violation::new(
                    at,
                    format!(
                        "graph node id {:?} is not unique, first used at {path}/nodes/{first}",
                        n.id
                    ),
                    &["a distinct \"id\" on this node"],
                ));
            } else {
                seen.insert(&n.id, i);
            }
        }
        for (i, e) in self.edges.iter().enumerate() {
            for (end, id) in [("from", &e.from), ("to", &e.to)] {
                if !seen.contains_key(id.as_str()) {
                    out.push(Violation::new(
                        format!("{path}/edges/{i}/{end}"),
                        format!("edge {end} {id:?} names no node of the graph"),
                        &ids,
                    ));
                }
            }
        }
        if let Some(c) = &self.current
            && !seen.contains_key(c.as_str())
        {
            out.push(Violation::new(
                format!("{path}/current"),
                format!("current {c:?} names no node of the graph"),
                &ids,
            ));
        }
        out
    }
}

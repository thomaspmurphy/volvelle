//! The `tree` atom: nested items with folding, selectable with an id.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeNode {
    /// Makes the tree selectable. Enter emits `select` with the node's value.
    pub id: Option<String>,
    /// Title shown in a border around the tree.
    pub title: Option<String>,
    pub nodes: Vec<TreeItem>,
    /// Only nodes without children answer. Enter on a node with children
    /// folds or unfolds it instead.
    #[serde(default)]
    pub leaves_only: bool,
}

/// One node in a tree, with its children.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeItem {
    pub label: String,
    /// Reported in events. Defaults to the path of labels joined with `/`,
    /// such as `src/app/main.rs`.
    pub value: Option<String>,
    pub detail: Option<String>,
    #[serde(default)]
    pub children: Vec<TreeItem>,
    /// Whether the children start shown. The user can fold and unfold.
    #[serde(default = "open")]
    pub open: bool,
}

fn open() -> bool {
    true
}

/// A tree item in depth-first order, with what drawing and answering need.
pub struct Flat<'a> {
    pub item: &'a TreeItem,
    pub value: String,
    /// Index of the parent in the flattened order; `None` at the top level.
    pub parent: Option<usize>,
    /// Whether this is the last of its siblings.
    pub last: bool,
    /// JSON Pointer of the item within the tree node.
    pub pointer: String,
}

impl TreeNode {
    /// Every item in depth-first order. A parent always comes before its
    /// children, and its first child directly after it.
    pub fn flatten(&self) -> Vec<Flat<'_>> {
        let mut out = Vec::new();
        flatten(&self.nodes, None, "", "/nodes", &mut out);
        out
    }

    /// The values Enter can answer with, in depth-first order.
    pub fn answers(&self) -> Vec<String> {
        self.flatten()
            .into_iter()
            .filter(|f| !self.leaves_only || f.item.children.is_empty())
            .map(|f| f.value)
            .collect()
    }

    /// Broken rules, each as a JSON Pointer within the tree node, a message
    /// and the allowed alternatives.
    pub fn problems(&self) -> Vec<(String, String, &'static [&'static str])> {
        let mut out = Vec::new();
        if self.id.is_some() && self.nodes.is_empty() {
            out.push((
                "/nodes".into(),
                "selectable tree has no nodes".into(),
                &["at least one node", "no id, for a display-only tree"][..],
            ));
        }
        let mut seen = HashSet::new();
        for f in self.flatten() {
            if !seen.insert(f.value.clone()) {
                out.push((
                    f.pointer,
                    format!("tree node value {:?} is not unique", f.value),
                    &["a distinct \"value\" on this node"][..],
                ));
            }
        }
        out
    }
}

fn flatten<'a>(
    items: &'a [TreeItem],
    parent: Option<usize>,
    path: &str,
    pointer: &str,
    out: &mut Vec<Flat<'a>>,
) {
    for (i, item) in items.iter().enumerate() {
        let path = if path.is_empty() {
            item.label.clone()
        } else {
            format!("{path}/{}", item.label)
        };
        let pointer = format!("{pointer}/{i}");
        let index = out.len();
        out.push(Flat {
            item,
            value: item.value.clone().unwrap_or_else(|| path.clone()),
            parent,
            last: i + 1 == items.len(),
            pointer: pointer.clone(),
        });
        flatten(
            &item.children,
            Some(index),
            &path,
            &format!("{pointer}/children"),
            out,
        );
    }
}

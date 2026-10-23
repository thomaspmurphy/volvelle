//! The `state-machine@1` pattern: states, a start, end states and labelled
//! transitions, drawn as a graph with an entry point and the current state
//! lit.

use std::collections::{HashMap, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Edge, Flow, GraphNode, MAX_EDGES, MAX_NODES, Tone, Vertex, Violation};

/// Appended to an end state's label.
const END_MARK: &str = " ◉";
/// The entry point's label. It points at `start`.
const ENTRY_MARK: &str = "●";
/// The entry point's node id. State ids are never empty, so it cannot clash.
const ENTRY: &str = "";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StateMachine {
    /// Makes the states selectable. Enter emits `select` with the state's id.
    pub id: Option<String>,
    pub title: Option<String>,
    /// Which way transitions run, from earlier layers to later ones.
    #[serde(default)]
    pub direction: Flow,
    pub states: Vec<Vertex>,
    /// The state the entry point leads to.
    pub start: String,
    /// States the machine stops in. They have no outgoing transitions.
    #[serde(default)]
    pub end: Vec<String>,
    #[serde(default)]
    pub transitions: Vec<Edge>,
    /// The id of the state the machine is in.
    pub current: Option<String>,
    /// A JSON Pointer to the current state's id in the data document, in
    /// place of `current`. Updating it moves the highlight.
    pub bind: Option<String>,
}

impl StateMachine {
    /// The state ids, in order, which are also what Enter can answer with.
    pub fn ids(&self) -> Vec<&str> {
        self.states.iter().map(|s| s.id.as_str()).collect()
    }

    /// The state ids quoted, as the alternatives a violation offers.
    pub(crate) fn quoted_ids(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for s in &self.states {
            let q = format!("{:?}", s.id);
            if !s.id.is_empty() && !seen.contains(&q) {
                seen.push(q);
            }
        }
        seen
    }

    /// The graph it draws: an inert entry point leading to `start`, end
    /// states marked, transitions between the same two states merged into
    /// one edge, and `current` or `bind` passed through.
    pub fn graph(&self) -> GraphNode {
        let entry = Vertex {
            id: ENTRY.into(),
            label: Some(ENTRY_MARK.into()),
            tone: Tone::Muted,
            inert: true,
        };
        let states = self.states.iter().map(|s| {
            let mut v = s.clone();
            if self.end.contains(&s.id) {
                v.label = Some(format!("{}{END_MARK}", s.label()));
            }
            v
        });
        let start = Edge {
            from: ENTRY.into(),
            to: self.start.clone(),
            label: None,
        };
        GraphNode {
            id: self.id.clone(),
            title: self.title.clone(),
            direction: self.direction,
            nodes: std::iter::once(entry).chain(states).collect(),
            edges: std::iter::once(start).chain(self.merged()).collect(),
            current: self.current.clone(),
            bind: self.bind.clone(),
        }
    }

    /// Broken rules, each at a JSON Pointer under `path`, the pattern's own
    /// pointer.
    pub(crate) fn problems(&self, path: &str) -> Vec<Violation> {
        let mut out = Vec::new();
        let ids = self.quoted_ids();
        let mut ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        if ids.is_empty() {
            ids.push("the id of a state in `states`");
        }
        if self.states.is_empty() {
            out.push(Violation::new(
                format!("{path}/states"),
                "state-machine@1 has no states",
                &["at least one state"],
            ));
        }
        if self.states.len() > MAX_NODES {
            out.push(Violation::new(
                format!("{path}/states"),
                format!(
                    "state machine has {} states, more than {MAX_NODES}",
                    self.states.len()
                ),
                &[
                    &format!("at most {MAX_NODES} states"),
                    "several smaller machines",
                ],
            ));
        }
        if self.transitions.len() > MAX_EDGES {
            out.push(Violation::new(
                format!("{path}/transitions"),
                format!(
                    "state machine has {} transitions, more than {MAX_EDGES}",
                    self.transitions.len()
                ),
                &[
                    &format!("at most {MAX_EDGES} transitions"),
                    "several smaller machines",
                ],
            ));
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, s) in self.states.iter().enumerate() {
            let at = format!("{path}/states/{i}/id");
            if s.id.is_empty() {
                out.push(Violation::new(
                    at,
                    "state id is empty",
                    &["a short name such as \"idle\""],
                ));
            } else if let Some(first) = seen.get(s.id.as_str()) {
                out.push(Violation::new(
                    at,
                    format!(
                        "state id {:?} is not unique, first used at {path}/states/{first}",
                        s.id
                    ),
                    &["a distinct \"id\" on this state"],
                ));
            } else {
                seen.insert(&s.id, i);
            }
        }
        let known = |id: &str| seen.contains_key(id);
        if !known(&self.start) {
            out.push(Violation::new(
                format!("{path}/start"),
                format!("start {:?} names no state", self.start),
                &ids,
            ));
        }
        for (i, e) in self.end.iter().enumerate() {
            if !known(e) {
                out.push(Violation::new(
                    format!("{path}/end/{i}"),
                    format!("end {e:?} names no state"),
                    &ids,
                ));
            }
        }
        for (i, t) in self.transitions.iter().enumerate() {
            for (side, id) in [("from", &t.from), ("to", &t.to)] {
                if !known(id) {
                    out.push(Violation::new(
                        format!("{path}/transitions/{i}/{side}"),
                        format!("transition {side} {id:?} names no state"),
                        &ids,
                    ));
                }
            }
            if self.end.contains(&t.from) {
                out.push(Violation::new(
                    format!("{path}/transitions/{i}"),
                    format!("transition leaves end state {:?}", t.from),
                    &[
                        "a transition from a state that is not an end",
                        &format!("{:?} left out of `end`", t.from),
                    ],
                ));
            }
        }
        if let Some(c) = &self.current
            && !known(c)
        {
            out.push(Violation::new(
                format!("{path}/current"),
                format!("current {c:?} names no state"),
                &ids,
            ));
        }
        if known(&self.start) {
            let reached = self.reachable();
            for (i, s) in self.states.iter().enumerate() {
                if !s.id.is_empty() && !reached.contains(&s.id.as_str()) {
                    out.push(Violation::new(
                        format!("{path}/states/{i}"),
                        format!(
                            "state {:?} cannot be reached from start {:?}",
                            s.id, self.start
                        ),
                        &[&format!("a transition into {:?}", s.id), "no such state"],
                    ));
                }
            }
        }
        out
    }

    /// One edge per pair of states, in order of first use, its labels
    /// joined with commas in input order.
    fn merged(&self) -> Vec<Edge> {
        let mut out: Vec<Edge> = Vec::new();
        for t in &self.transitions {
            match out.iter_mut().find(|e| e.from == t.from && e.to == t.to) {
                Some(e) => {
                    if let Some(l) = &t.label {
                        e.label = Some(match e.label.take() {
                            Some(prev) => format!("{prev}, {l}"),
                            None => l.clone(),
                        });
                    }
                }
                None => out.push(t.clone()),
            }
        }
        out
    }

    /// The states a walk along transitions from `start` reaches.
    fn reachable(&self) -> Vec<&str> {
        let mut reached = vec![self.start.as_str()];
        let mut queue = VecDeque::from([self.start.as_str()]);
        while let Some(at) = queue.pop_front() {
            for t in self.transitions.iter().filter(|t| t.from == at) {
                if !reached.contains(&t.to.as_str()) {
                    reached.push(&t.to);
                    queue.push_back(&t.to);
                }
            }
        }
        reached
    }
}

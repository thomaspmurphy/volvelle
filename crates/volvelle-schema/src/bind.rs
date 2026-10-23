//! Data binding. An atom can point into a data document with `bind`, an
//! RFC 6901 JSON Pointer, instead of holding its content, so a spec becomes
//! a template and a change is a small data patch, not a new spec. Each
//! bindable atom takes a literal or a bind, never both.

use serde_json::{Map, Value};

use crate::{Item, Node, Pattern, Violation};

/// The literal a bind replaces: the atom's name, the literal's field,
/// whether the atom needs one of the two, and whether the literal is given.
fn literal(node: &Node) -> Option<(&'static str, &'static str, bool, bool)> {
    Some(match node {
        Node::Text(n) => ("text", "text", true, n.text.is_some()),
        Node::Markdown(n) => ("markdown", "text", true, n.text.is_some()),
        Node::Progress(n) => ("progress", "value", true, n.value.is_some()),
        Node::List(n) => ("list", "items", true, n.items.is_some()),
        Node::Table(n) => ("table", "rows", true, n.rows.is_some()),
        Node::Input(n) => ("input", "value", false, n.value.is_some()),
        Node::Toggle(n) => ("toggle", "value", false, n.value.is_some()),
        Node::Select(n) => ("select", "value", false, !n.value.is_empty()),
        Node::Graph(n) => ("graph", "current", false, n.current.is_some()),
        Node::Pattern(Pattern::StateMachine(n)) => {
            ("state-machine@1", "current", false, n.current.is_some())
        }
        _ => return None,
    })
}

/// Checks the literal-or-bind rule. Called from [`crate::validate`], since it
/// needs no data.
pub(crate) fn literals(node: &Node, path: &str, errors: &mut Vec<Violation>) {
    let Some((atom, field, required, given)) = literal(node) else {
        return;
    };
    match (given, node.bind()) {
        (true, Some(_)) => errors.push(Violation::new(
            path,
            format!("{atom} has both `{field}` and `bind`"),
            &[&format!("`{field}`"), "`bind`"],
        )),
        (false, None) if required => errors.push(Violation::new(
            path,
            format!("{atom} has neither `{field}` nor `bind`"),
            &[&format!("`{field}`"), "`bind` with a pointer into `data`"],
        )),
        _ => {}
    }
    if let Node::Select(s) = node
        && s.multi
        && s.bind.is_some()
    {
        errors.push(Violation::new(
            format!("{path}/bind"),
            format!(
                "select {:?} is multi, and only a single select can bind",
                s.id
            ),
            &["a single select", "`value` on a multi select"],
        ));
    }
}

/// Checks that every bind resolves in `data` to what its atom needs. A spec
/// with binds and no data is refused.
pub fn check_binds(root: &Node, data: Option<&Value>) -> Result<(), Vec<Violation>> {
    let mut errors = Vec::new();
    let mut unbound: Vec<String> = Vec::new();
    root.walk_paths("", &mut |node, path| {
        let Some(pointer) = node.bind() else { return };
        let at = format!("{path}/bind");
        let Some(data) = data else {
            unbound.push(at);
            return;
        };
        if !(pointer.is_empty() || pointer.starts_with('/')) {
            errors.push(Violation::new(
                at,
                format!("bind {pointer:?} is not a JSON Pointer"),
                &[&format!("{:?}", format!("/{pointer}"))],
            ));
            return;
        }
        let Some(value) = data.pointer(pointer) else {
            let near = nearby(data, pointer);
            let allowed: Vec<&str> = near.iter().map(String::as_str).collect();
            errors.push(Violation::new(
                at,
                format!("bind {pointer:?} does not resolve in data"),
                if allowed.is_empty() {
                    &["a pointer that exists in data"]
                } else {
                    &allowed
                },
            ));
            return;
        };
        if let Err((rule, allowed)) = fits(node, value) {
            let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
            errors.push(Violation::new(
                at,
                format!("bind {pointer:?} holds {}, {rule}", describe(value)),
                &allowed,
            ));
        }
    });
    if let Some(first) = unbound.first() {
        errors.push(Violation::new(
            first.as_str(),
            format!(
                "the spec binds {} pointer{} but render has no `data`",
                unbound.len(),
                if unbound.len() == 1 { "" } else { "s" }
            ),
            &["`data` with a document", "literals instead of binds"],
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Whether `value` is what the bound atom needs: Ok, or the rule it breaks
/// and what would be accepted.
fn fits(node: &Node, value: &Value) -> Result<(), (String, Vec<String>)> {
    let need = |what: &str| {
        Err((
            format!("but {} needs {what}", atom(node)),
            vec![what.into()],
        ))
    };
    match node {
        Node::Text(_) | Node::Markdown(_) | Node::Input(_) if !value.is_string() => {
            need("a string")
        }
        Node::Toggle(_) if !value.is_boolean() => need("a boolean"),
        Node::Progress(_) => match value.as_f64() {
            Some(v) if (0.0..=1.0).contains(&v) => Ok(()),
            _ => need("a number from 0.0 to 1.0"),
        },
        Node::Select(s) => {
            let options: Vec<String> = s
                .options
                .iter()
                .map(|o| format!("{:?}", o.value()))
                .collect();
            match value.as_str() {
                Some(v) if s.options.iter().any(|o| o.value() == v) => Ok(()),
                Some(_) => Err((
                    format!("which is not an option of select {:?}", s.id),
                    options,
                )),
                None => Err((
                    format!("but select {:?} needs one of its options", s.id),
                    options,
                )),
            }
        }
        Node::List(_) if serde_json::from_value::<Vec<Item>>(value.clone()).is_err() => {
            need("an array of strings or of {label, value?, detail?} items")
        }
        Node::Graph(g) => match value.as_str() {
            Some(v) if g.nodes.iter().any(|n| n.id == v) => Ok(()),
            Some(_) => Err(("which is not a node of the graph".into(), g.quoted_ids())),
            None => Err(("but graph needs a node id".into(), g.quoted_ids())),
        },
        Node::Pattern(Pattern::StateMachine(m)) => match value.as_str() {
            Some(v) if m.states.iter().any(|s| s.id == v) => Ok(()),
            Some(_) => Err(("which is not a state of the machine".into(), m.quoted_ids())),
            None => Err((
                "but state-machine@1 needs a state id".into(),
                m.quoted_ids(),
            )),
        },
        Node::Table(_) if serde_json::from_value::<Vec<Vec<String>>>(value.clone()).is_err() => {
            need("an array of rows, each an array of strings")
        }
        _ => Ok(()),
    }
}

fn atom(node: &Node) -> &'static str {
    literal(node).map_or("this atom", |(atom, ..)| atom)
}

/// A short description of a value for a violation.
fn describe(value: &Value) -> String {
    match value {
        Value::Array(a) => format!("an array of {}", a.len()),
        Value::Object(_) => "an object".into(),
        v => {
            let s = v.to_string();
            if s.chars().count() > 40 {
                format!("{}…", s.chars().take(39).collect::<String>())
            } else {
                s
            }
        }
    }
}

/// Pointers that do exist under the deepest part of `pointer` that resolves,
/// so a typo can be corrected.
fn nearby(data: &Value, pointer: &str) -> Vec<String> {
    let mut parent = pointer;
    while let Some(i) = parent.rfind('/') {
        parent = &parent[..i];
        let Some(v) = data.pointer(parent) else {
            continue;
        };
        let children: Vec<String> = match v {
            Value::Object(map) => map
                .keys()
                .map(|k| format!("{parent}/{}", k.replace('~', "~0").replace('/', "~1")))
                .collect(),
            Value::Array(items) => (0..items.len()).map(|i| format!("{parent}/{i}")).collect(),
            _ => vec![parent.to_string()],
        };
        return children
            .into_iter()
            .take(8)
            .map(|c| format!("{c:?}"))
            .collect();
    }
    Vec::new()
}

/// Checks the binds against `data` and returns the spec as drawn, with each
/// bound atom's content filled in. Without binds or data, that is the spec.
pub fn bind(root: &Node, data: Option<&Value>) -> Result<Node, String> {
    check_binds(root, data).map_err(|errors| {
        let lines: Vec<String> = errors.iter().map(Violation::to_string).collect();
        lines.join("\n")
    })?;
    Ok(match data {
        Some(data) => resolve(root, data),
        None => root.clone(),
    })
}

/// The spec with each bound atom's content read from `data`. A bind that
/// does not fit leaves its atom empty; [`check_binds`] reports it.
pub fn resolve(root: &Node, data: &Value) -> Node {
    let mut node = root.clone();
    fill(&mut node, data);
    node
}

fn fill(node: &mut Node, data: &Value) {
    let at = |bind: &Option<String>| bind.as_deref().and_then(|p| data.pointer(p)).cloned();
    match node {
        Node::Box(b) => b.children.iter_mut().for_each(|c| fill(c, data)),
        Node::Text(n) => n.text = at(&n.bind).and_then(string).or(n.text.take()),
        Node::Markdown(n) => n.text = at(&n.bind).and_then(string).or(n.text.take()),
        Node::Input(n) => n.value = at(&n.bind).and_then(string).or(n.value.take()),
        Node::Toggle(n) => n.value = at(&n.bind).and_then(|v| v.as_bool()).or(n.value),
        Node::Progress(n) => n.value = at(&n.bind).and_then(|v| v.as_f64()).or(n.value),
        Node::Graph(n) => n.current = at(&n.bind).and_then(string).or(n.current.take()),
        Node::Pattern(Pattern::StateMachine(n)) => {
            n.current = at(&n.bind).and_then(string).or(n.current.take())
        }
        Node::Select(n) => {
            if let Some(v) = at(&n.bind).and_then(string) {
                n.value = vec![v];
            }
        }
        Node::List(n) => {
            if let Some(v) = at(&n.bind) {
                n.items = serde_json::from_value(v).ok();
            }
        }
        Node::Table(n) => {
            if let Some(v) = at(&n.bind) {
                n.rows = serde_json::from_value(v).ok();
            }
        }
        _ => {}
    }
}

fn string(v: Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// `data` with each bound field's answered value written to its pointer,
/// so the answer and the state are one document.
pub fn write_back(root: &Node, data: &Value, values: &Map<String, Value>) -> Value {
    let mut out = data.clone();
    root.walk(&mut |node| {
        let (Some(id), Some(pointer)) = (node.id(), node.bind()) else {
            return;
        };
        if let (Some(value), Some(slot)) = (values.get(id), out.pointer_mut(pointer)) {
            *slot = value.clone();
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_spec;
    use serde_json::json;

    fn data() -> Value {
        json!({
            "hero": {"name": "Ada", "hp": 0.75, "poisoned": false, "class": "rogue",
                     "bio": "# Ada\nA *rogue*."},
            "inventory": ["rope", {"label": "Lantern", "value": "lantern", "detail": "lit"}],
            "log": [["1", "Entered the crypt"], ["2", "Found a key"]]
        })
    }

    fn sheet() -> Node {
        parse_spec(json!({"type": "box", "children": [
            {"type": "text", "bind": "/hero/name"},
            {"type": "markdown", "bind": "/hero/bio"},
            {"type": "progress", "label": "HP", "bind": "/hero/hp"},
            {"type": "input", "id": "name", "bind": "/hero/name"},
            {"type": "toggle", "id": "poisoned", "label": "Poisoned", "bind": "/hero/poisoned"},
            {"type": "select", "id": "class", "options": ["fighter", "rogue"], "bind": "/hero/class"},
            {"type": "list", "id": "item", "bind": "/inventory"},
            {"type": "table", "headers": ["#", "Event"], "bind": "/log"},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}))
        .unwrap()
    }

    #[test]
    fn every_bindable_atom_resolves() {
        let shown = bind(&sheet(), Some(&data())).unwrap();
        let c = shown.children();
        let Node::Text(t) = &c[0] else { panic!() };
        assert_eq!(t.text(), "Ada");
        let Node::Markdown(m) = &c[1] else { panic!() };
        assert_eq!(m.text(), "# Ada\nA *rogue*.");
        let Node::Progress(p) = &c[2] else { panic!() };
        assert_eq!(p.value(), 0.75);
        let Node::Input(i) = &c[3] else { panic!() };
        assert_eq!(i.value(), "Ada");
        let Node::Toggle(t) = &c[4] else { panic!() };
        assert!(!t.value());
        let Node::Select(s) = &c[5] else { panic!() };
        assert_eq!(s.value, ["rogue"]);
        let Node::List(l) = &c[6] else { panic!() };
        let values: Vec<&str> = l.items().iter().map(Item::value).collect();
        assert_eq!(values, ["rope", "lantern"]);
        let Node::Table(t) = &c[7] else { panic!() };
        assert_eq!(t.rows()[1], ["2", "Found a key"]);
        // The bound list's items reach the answer's type.
        let schema = crate::result_schema(&shown);
        assert_eq!(
            schema["oneOf"][2]["properties"]["value"]["enum"],
            json!(["rope", "lantern"])
        );
    }

    fn errors(spec: Value, data: Option<Value>) -> Vec<String> {
        let node = parse_spec(spec).unwrap();
        check_binds(&node, data.as_ref())
            .unwrap_err()
            .iter()
            .map(Violation::to_string)
            .collect()
    }

    #[test]
    fn wrong_types_are_refused_at_the_bind() {
        let spec = json!({"type": "box", "children": [
            {"type": "progress", "bind": "/hero/name"},
            {"type": "toggle", "id": "t", "label": "T", "bind": "/hero/hp"},
            {"type": "select", "id": "class", "options": ["fighter", "mage"], "bind": "/hero/class"},
            {"type": "list", "bind": "/hero"},
            {"type": "table", "headers": ["a"], "bind": "/inventory"},
            {"type": "text", "bind": "/log"}
        ]});
        assert_eq!(
            errors(spec, Some(data())),
            [
                "at /children/0/bind: bind \"/hero/name\" holds \"Ada\", but progress needs a number from 0.0 to 1.0; allowed: a number from 0.0 to 1.0",
                "at /children/1/bind: bind \"/hero/hp\" holds 0.75, but toggle needs a boolean; allowed: a boolean",
                "at /children/2/bind: bind \"/hero/class\" holds \"rogue\", which is not an option of select \"class\"; allowed: \"fighter\", \"mage\"",
                "at /children/3/bind: bind \"/hero\" holds an object, but list needs an array of strings or of {label, value?, detail?} items; allowed: an array of strings or of {label, value?, detail?} items",
                "at /children/4/bind: bind \"/inventory\" holds an array of 2, but table needs an array of rows, each an array of strings; allowed: an array of rows, each an array of strings",
                "at /children/5/bind: bind \"/log\" holds an array of 2, but text needs a string; allowed: a string",
            ]
        );
        let range = json!({"type": "progress", "bind": "/hp"});
        assert!(errors(range, Some(json!({"hp": 1.5})))[0].contains("holds 1.5"));
    }

    #[test]
    fn missing_pointers_offer_what_exists() {
        let spec = json!({"type": "box", "children": [
            {"type": "progress", "bind": "/hero/hpp"},
            {"type": "text", "bind": "/villain/name"},
            {"type": "text", "bind": "hero/name"}
        ]});
        assert_eq!(
            errors(spec, Some(data())),
            [
                "at /children/0/bind: bind \"/hero/hpp\" does not resolve in data; allowed: \"/hero/bio\", \"/hero/class\", \"/hero/hp\", \"/hero/name\", \"/hero/poisoned\"",
                "at /children/1/bind: bind \"/villain/name\" does not resolve in data; allowed: \"/hero\", \"/inventory\", \"/log\"",
                "at /children/2/bind: bind \"hero/name\" is not a JSON Pointer; allowed: \"/hero/name\"",
            ]
        );
    }

    #[test]
    fn binds_need_data() {
        let err = bind(&sheet(), None).unwrap_err();
        assert_eq!(
            err,
            "at /children/0/bind: the spec binds 8 pointers but render has no `data`; allowed: `data` with a document, literals instead of binds"
        );
        let plain = parse_spec(json!({"type": "text", "text": "hi"})).unwrap();
        assert!(bind(&plain, None).is_ok());
    }

    #[test]
    fn literal_and_bind_are_exclusive() {
        let spec = json!({"type": "box", "children": [
            {"type": "text", "text": "Ada", "bind": "/hero/name"},
            {"type": "toggle", "id": "t", "label": "T", "value": false, "bind": "/hero/poisoned"},
            {"type": "progress", "label": "HP"},
            {"type": "select", "id": "s", "options": ["a"], "multi": true, "bind": "/x"},
            {"type": "list"}
        ]});
        let err = parse_spec(spec).unwrap_err();
        let lines: Vec<&str> = err.lines().collect();
        assert_eq!(
            lines,
            [
                "at /children/0: text has both `text` and `bind`; allowed: `text`, `bind`",
                "at /children/1: toggle has both `value` and `bind`; allowed: `value`, `bind`",
                "at /children/2: progress has neither `value` nor `bind`; allowed: `value`, `bind` with a pointer into `data`",
                "at /children/3/bind: select \"s\" is multi, and only a single select can bind; allowed: a single select, `value` on a multi select",
                "at /children/4: list has neither `items` nor `bind`; allowed: `items`, `bind` with a pointer into `data`",
            ]
        );
    }

    #[test]
    fn graph_current_binds_to_a_node_id() {
        let spec = |bind: &str| {
            json!({"type": "graph", "id": "g", "nodes": [{"id": "idle"}, {"id": "busy"}],
                   "edges": [{"from": "idle", "to": "busy"}], "bind": bind})
        };
        let data = json!({"state": "busy", "other": "gone", "n": 3});
        let Node::Graph(g) = bind(&parse_spec(spec("/state")).unwrap(), Some(&data)).unwrap()
        else {
            panic!("expected a graph")
        };
        assert_eq!(g.current.as_deref(), Some("busy"));
        let patched = crate::apply_patch(
            &data,
            &[
                serde_json::from_value(json!({"op": "replace", "path": "/state", "value": "idle"}))
                    .unwrap(),
            ],
        )
        .unwrap();
        let Node::Graph(g) = resolve(&parse_spec(spec("/state")).unwrap(), &patched) else {
            panic!("expected a graph")
        };
        assert_eq!(g.current.as_deref(), Some("idle"));

        assert_eq!(
            errors(spec("/other"), Some(data.clone())),
            [
                "at /bind: bind \"/other\" holds \"gone\", which is not a node of the graph; allowed: \"idle\", \"busy\""
            ]
        );
        assert_eq!(
            errors(spec("/n"), Some(data)),
            [
                "at /bind: bind \"/n\" holds 3, but graph needs a node id; allowed: \"idle\", \"busy\""
            ]
        );
        let mut both = spec("/state");
        both["current"] = json!("idle");
        assert_eq!(
            parse_spec(both).unwrap_err(),
            "at /: graph has both `current` and `bind`; allowed: `current`, `bind`"
        );
        parse_spec(json!({"type": "graph", "nodes": [{"id": "a"}]})).unwrap();
    }

    #[test]
    fn answers_write_back_to_their_pointers() {
        let values = json!({"name": "Grace", "poisoned": true, "class": "fighter", "extra": 1});
        let out = write_back(&sheet(), &data(), values.as_object().unwrap());
        assert_eq!(out["hero"]["name"], "Grace");
        assert_eq!(out["hero"]["poisoned"], true);
        assert_eq!(out["hero"]["class"], "fighter");
        assert_eq!(out["hero"]["hp"], 0.75, "display atoms are untouched");
        assert_eq!(out["inventory"], data()["inventory"]);
        assert!(out.get("extra").is_none());
    }
}

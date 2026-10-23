//! The RFC 6902 subset `update` takes: replace, add and remove, applied to a
//! data document all or nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Violation;

/// One patch operation. `path` is a JSON Pointer; `-` as the last token of
/// an `add` appends to an array.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Replace { path: String, value: Value },
    Add { path: String, value: Value },
    Remove { path: String },
}

impl Op {
    pub fn path(&self) -> &str {
        match self {
            Op::Replace { path, .. } | Op::Add { path, .. } | Op::Remove { path } => path,
        }
    }
}

/// Applies `patch` to a copy of `data`. Any failing operation refuses the
/// whole patch, naming it by its index.
pub fn apply_patch(data: &Value, patch: &[Op]) -> Result<Value, Violation> {
    let mut out = data.clone();
    for (i, op) in patch.iter().enumerate() {
        apply(&mut out, op).map_err(|(rule, allowed)| Violation {
            path: format!("/patch/{i}"),
            rule,
            allowed,
        })?;
    }
    if !out.is_object() {
        return Err(Violation::new(
            "/patch",
            "the patch leaves data that is not an object",
            &["a patch that keeps the document an object"],
        ));
    }
    Ok(out)
}

type Refused = (String, Vec<String>);

fn apply(doc: &mut Value, op: &Op) -> Result<(), Refused> {
    let path = op.path();
    let refuse = |rule: String, allowed: &[&str]| -> Result<(), Refused> {
        Err((rule, allowed.iter().map(|a| a.to_string()).collect()))
    };
    if path.is_empty() {
        return match op {
            Op::Replace { value, .. } | Op::Add { value, .. } => {
                *doc = value.clone();
                Ok(())
            }
            Op::Remove { .. } => refuse(
                "the whole document cannot be removed".into(),
                &["replace at \"\"", "remove a member"],
            ),
        };
    }
    let Some(i) = path.rfind('/').filter(|_| path.starts_with('/')) else {
        return refuse(
            format!("path {path:?} is not a JSON Pointer"),
            &[&format!("{:?}", format!("/{path}"))],
        );
    };
    let (parent, key) = (&path[..i], unescape(&path[i + 1..]));
    let Some(target) = doc.pointer_mut(parent) else {
        return refuse(
            format!("{parent:?} does not exist, so {path:?} cannot be reached"),
            &[&format!("add {parent:?} first")],
        );
    };
    match target {
        Value::Object(map) => match op {
            Op::Add { value, .. } => {
                map.insert(key, value.clone());
            }
            Op::Replace { value, .. } => match map.get_mut(&key) {
                Some(slot) => *slot = value.clone(),
                None => {
                    return refuse(
                        format!("nothing at {path:?} to replace"),
                        &["add", "an existing path"],
                    );
                }
            },
            Op::Remove { .. } => {
                if map.remove(&key).is_none() {
                    return refuse(
                        format!("nothing at {path:?} to remove"),
                        &["an existing path"],
                    );
                }
            }
        },
        Value::Array(items) => {
            let len = items.len();
            let adding = matches!(op, Op::Add { .. });
            let last = if adding { len } else { len.saturating_sub(1) };
            let index = match key.as_str() {
                "-" if adding => Some(len),
                k if !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()) => k.parse().ok(),
                _ => None,
            };
            let Some(index) = index.filter(|&n| n <= last && (adding || len > 0)) else {
                let mut allowed = vec![format!("an index from 0 to {last}")];
                if adding {
                    allowed.insert(0, format!("{:?} to append", format!("{parent}/-")));
                }
                let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
                return refuse(
                    format!("{key:?} is not an index into {parent:?}, an array of {len}"),
                    if len == 0 && !adding {
                        &["add to the empty array"]
                    } else {
                        &allowed
                    },
                );
            };
            match op {
                Op::Add { value, .. } => items.insert(index, value.clone()),
                Op::Replace { value, .. } => items[index] = value.clone(),
                Op::Remove { .. } => {
                    items.remove(index);
                }
            }
        }
        other => {
            return refuse(
                format!("{parent:?} holds {other}, which has no members"),
                &[&format!("replace {parent:?} itself")],
            );
        }
    }
    Ok(())
}

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Whether a change at one pointer reaches the other: they are equal, or one
/// is inside the other.
pub fn touches(a: &str, b: &str) -> bool {
    let inside = |inner: &str, outer: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    a == b || inside(a, b) || inside(b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ops(v: Value) -> Vec<Op> {
        serde_json::from_value(v).unwrap()
    }

    fn doc() -> Value {
        json!({"hero": {"hp": 0.5, "a/b": 1}, "bag": ["rope"]})
    }

    #[test]
    fn replace_add_and_remove() {
        let patch = ops(json!([
            {"op": "replace", "path": "/hero/hp", "value": 0.25},
            {"op": "add", "path": "/bag/-", "value": "lantern"},
            {"op": "add", "path": "/bag/0", "value": "map"},
            {"op": "add", "path": "/hero/name", "value": "Ada"},
            {"op": "remove", "path": "/hero/a~1b"},
            {"op": "replace", "path": "/bag/2", "value": "torch"}
        ]));
        let out = apply_patch(&doc(), &patch).unwrap();
        assert_eq!(
            out,
            json!({"hero": {"hp": 0.25, "name": "Ada"}, "bag": ["map", "rope", "torch"]})
        );
        let root = ops(json!([{"op": "replace", "path": "", "value": {"fresh": true}}]));
        assert_eq!(apply_patch(&doc(), &root).unwrap(), json!({"fresh": true}));
    }

    #[test]
    fn a_failing_op_refuses_the_whole_patch() {
        let data = doc();
        let refused = |patch: Value| apply_patch(&data, &ops(patch)).unwrap_err().to_string();
        assert_eq!(
            refused(json!([
                {"op": "replace", "path": "/hero/hp", "value": 0.1},
                {"op": "replace", "path": "/hero/mp", "value": 1}
            ])),
            "at /patch/1: nothing at \"/hero/mp\" to replace; allowed: add, an existing path"
        );
        assert_eq!(
            refused(json!([{"op": "add", "path": "/bag/5", "value": "x"}])),
            "at /patch/0: \"5\" is not an index into \"/bag\", an array of 1; allowed: \"/bag/-\" to append, an index from 0 to 1"
        );
        assert_eq!(
            refused(json!([{"op": "remove", "path": "/villain/hp"}])),
            "at /patch/0: \"/villain\" does not exist, so \"/villain/hp\" cannot be reached; allowed: add \"/villain\" first"
        );
        assert_eq!(
            refused(json!([{"op": "add", "path": "/hero/hp/max", "value": 1}])),
            "at /patch/0: \"/hero/hp\" holds 0.5, which has no members; allowed: replace \"/hero/hp\" itself"
        );
        assert_eq!(
            refused(json!([{"op": "replace", "path": "hero", "value": 1}])),
            "at /patch/0: path \"hero\" is not a JSON Pointer; allowed: \"/hero\""
        );
        assert!(
            refused(json!([{"op": "replace", "path": "", "value": [1]}])).contains("not an object")
        );
        assert!(refused(json!([{"op": "remove", "path": "/bag/01"}])).contains("not an index"));
        assert_eq!(data, doc(), "the document is untouched");
        assert!(serde_json::from_value::<Vec<Op>>(json!([{"op": "move", "path": "/a"}])).is_err());
    }

    #[test]
    fn touches_compares_whole_segments() {
        assert!(touches("/hero", "/hero/name"));
        assert!(touches("/hero/name", "/hero"));
        assert!(touches("/hero/name", "/hero/name"));
        assert!(touches("", "/hero"));
        assert!(!touches("/hero/name", "/hero/names"));
        assert!(!touches("/hero/hp", "/hero/name"));
    }
}

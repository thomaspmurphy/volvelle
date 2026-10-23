//! Saved views. The human keeps a view that proved useful by saving its spec
//! under a name, and the agent brings it back later. The agent may save
//! views too, but never over the human's. There are two
//! libraries: one per project in `<project>/.volvelle/library`, which can be
//! committed, and one per user. A project entry shadows a user entry with
//! the same name.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Node;

/// Which library an entry lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::User => "user",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One saved view, as stored in `<name>.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// Kept as JSON so an entry still loads after the catalog changes; it is
    /// validated when rendered.
    pub spec: Value,
    pub saved_at_ms: u64,
    pub origin: Origin,
    pub summary: String,
}

/// Where a saved view came from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Origin {
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Entries saved before this field existed were all the user's.
    #[serde(default)]
    pub by: By,
}

/// Who saved an entry. An agent may never overwrite the user's entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    #[default]
    User,
    Agent,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            By::User => "user",
            By::Agent => "agent",
        }
    }
}

impl Entry {
    /// An entry for `spec` as rendered, saved now.
    pub fn new(name: &str, spec: &Node, origin: Origin) -> Self {
        Entry {
            name: name.to_string(),
            spec: serde_json::to_value(spec).expect("spec serialises"),
            saved_at_ms: now_ms(),
            origin,
            summary: summary(spec),
        }
    }
}

/// One line of a listing.
#[derive(Clone, Debug, Serialize)]
pub struct Listed {
    pub name: String,
    pub scope: Scope,
    pub summary: String,
    pub saved_at_ms: u64,
    pub origin_view: String,
    pub by: By,
}

/// The two library directories. Either may be missing: a viewer started
/// without a project has only the user library.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub project: Option<PathBuf>,
    pub user: Option<PathBuf>,
}

impl Library {
    /// The project library under `project_root`, and the user library from
    /// the environment.
    pub fn from_env(project_root: Option<&Path>) -> Self {
        Library {
            project: project_root.map(project_dir),
            user: user_dir(|k| std::env::var_os(k)),
        }
    }

    /// The scopes that can be saved to, project first.
    pub fn scopes(&self) -> Vec<Scope> {
        [Scope::Project, Scope::User]
            .into_iter()
            .filter(|s| self.dir(*s).is_some())
            .collect()
    }

    pub fn dir(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Project => self.project.as_deref(),
            Scope::User => self.user.as_deref(),
        }
    }

    pub fn path(&self, scope: Scope, name: &str) -> Option<PathBuf> {
        Some(self.dir(scope)?.join(format!("{name}.json")))
    }

    pub fn exists(&self, scope: Scope, name: &str) -> bool {
        self.path(scope, name).is_some_and(|p| p.exists())
    }

    /// The entry saved as `name` in `scope` alone, if there is one.
    pub fn get(&self, scope: Scope, name: &str) -> Result<Option<Entry>, String> {
        valid_name(name)?;
        match self.path(scope, name) {
            Some(path) if path.exists() => read(&path)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            _ => Ok(None),
        }
    }

    /// Writes an entry, replacing any with the same name in that scope.
    /// Null fields are left out of the spec, so the file stays close to
    /// what the agent sent.
    pub fn save(&self, scope: Scope, entry: &Entry) -> io::Result<PathBuf> {
        valid_name(&entry.name).map_err(io::Error::other)?;
        let Some(dir) = self.dir(scope) else {
            return Err(io::Error::other(format!("no {scope} library")));
        };
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", entry.name));
        let tmp = dir.join(format!(".{}.json.tmp", entry.name));
        let mut entry = entry.clone();
        strip_nulls(&mut entry.spec);
        let mut text = serde_json::to_string_pretty(&entry).map_err(io::Error::other)?;
        text.push('\n');
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Finds `name`, project first.
    pub fn load(&self, name: &str) -> Result<(Scope, PathBuf, Entry), String> {
        valid_name(name)?;
        for scope in self.scopes() {
            let path = self.path(scope, name).expect("scope has a dir");
            if !path.exists() {
                continue;
            }
            let entry = read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            return Ok((scope, path, entry));
        }
        let names: Vec<String> = self
            .list()
            .into_iter()
            .map(|l| format!("{:?}", l.name))
            .collect();
        let allowed = if names.is_empty() {
            "nothing saved yet; the user saves with Ctrl-S, the agent with `save`".to_string()
        } else {
            names.join(", ")
        };
        Err(format!("no saved view {name:?}; allowed: {allowed}"))
    }

    /// Every entry, project shadowing user, sorted by name. An entry that
    /// cannot be read is listed with the reason as its summary.
    pub fn list(&self) -> Vec<Listed> {
        let mut out: Vec<Listed> = Vec::new();
        for scope in self.scopes() {
            let Ok(dir) = fs::read_dir(self.dir(scope).expect("scope has a dir")) else {
                continue;
            };
            for path in dir.flatten().map(|e| e.path()) {
                let Some(name) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".json"))
                    .filter(|n| valid_name(n).is_ok())
                else {
                    continue;
                };
                if out.iter().any(|l| l.name == name) {
                    continue;
                }
                out.push(match read(&path) {
                    Ok(e) => Listed {
                        name: name.to_string(),
                        scope,
                        summary: e.summary,
                        saved_at_ms: e.saved_at_ms,
                        origin_view: e.origin.view,
                        by: e.origin.by,
                    },
                    Err(e) => Listed {
                        name: name.to_string(),
                        scope,
                        summary: format!("unreadable: {e}"),
                        saved_at_ms: 0,
                        origin_view: String::new(),
                        by: By::User,
                    },
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Removes object fields whose value is null, at any depth. Nulls inside
/// arrays stay, since removing them would shift positions.
fn strip_nulls(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

fn read(path: &Path) -> Result<Entry, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// `<project>/.volvelle/library`.
pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".volvelle").join("library")
}

/// `$VOLVELLE_USER_LIBRARY`, else `$XDG_DATA_HOME/volvelle/library`, else
/// `~/.local/share/volvelle/library`. `var` reads the environment.
pub fn user_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |k| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("VOLVELLE_USER_LIBRARY")
        .or_else(|| var("XDG_DATA_HOME").map(|d| d.join("volvelle/library")))
        .or_else(|| var("HOME").map(|h| h.join(".local/share/volvelle/library")))
}

/// Names are `[a-z0-9][a-z0-9-]{0,63}`, so they are also safe file names.
pub fn valid_name(name: &str) -> Result<(), String> {
    let ok = (1..=64).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        return Ok(());
    }
    let mut rule = format!(
        "name {name:?} must be 1 to 64 lowercase letters, digits or hyphens, not starting with a hyphen"
    );
    let slug = slug(name);
    if !slug.is_empty() {
        rule.push_str(&format!("; allowed: {slug:?}"));
    }
    Err(rule)
}

/// The nearest valid name, or empty if there is none.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars()
        .take(64)
        .collect::<String>()
        .trim_end_matches('-')
        .into()
}

/// The root box's title, else the first line of text, plus what the view
/// holds, e.g. `Character sheet · 3 inputs, 2 buttons, 1 tree`.
pub fn summary(spec: &Node) -> String {
    let title = match spec {
        Node::Box(b) => b.title.clone().filter(|t| !t.trim().is_empty()),
        _ => None,
    };
    let headline = title.or_else(|| {
        let mut first = None;
        spec.walk(&mut |n| {
            if first.is_some() {
                return;
            }
            let text = match n {
                Node::Text(t) => &t.text,
                Node::Markdown(m) => &m.text,
                _ => return,
            };
            first = text
                .lines()
                .map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']).trim())
                .find(|l| !l.is_empty())
                .map(str::to_string);
        });
        first
    });
    let headline = headline.map(|h| clip(&h, 80));
    let mut counts: Vec<(&str, usize)> = Vec::new();
    spec.walk(&mut |n| {
        let Some(kind) = kind(n) else { return };
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, c)) => *c += 1,
            None => counts.push((kind, 1)),
        }
    });
    let inventory: Vec<String> = counts
        .iter()
        .map(|(kind, n)| {
            let plural = if *n == 1 || kind.contains('@') {
                ""
            } else {
                "s"
            };
            format!("{n} {kind}{plural}")
        })
        .collect();
    match (headline, inventory.is_empty()) {
        (Some(h), true) => h,
        (Some(h), false) => format!("{h} · {}", inventory.join(", ")),
        (None, false) => inventory.join(", "),
        (None, true) => "empty view".into(),
    }
}

/// The inventory name of a node, or None for layout and prose.
fn kind(n: &Node) -> Option<&'static str> {
    Some(match n {
        Node::Box(_) | Node::Text(_) | Node::Markdown(_) | Node::Divider(_) => return None,
        Node::Table(_) => "table",
        Node::List(_) => "list",
        Node::Tree(_) => "tree",
        Node::Input(_) => "input",
        Node::Select(_) => "select",
        Node::Toggle(_) => "toggle",
        Node::Button(_) => "button",
        Node::Progress(_) => "progress bar",
        Node::Pattern(p) => p.info().name,
    })
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_spec;
    use serde_json::json;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "volvelle-library-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    fn sheet() -> Node {
        parse_spec(
            json!({"type": "box", "title": "Character sheet", "children": [
                {"type": "input", "id": "name"},
                {"type": "input", "id": "class"},
                {"type": "progress", "label": "HP", "value": 0.5},
                {"type": "button", "id": "ok", "label": "OK"}
            ]}),
        )
        .unwrap()
    }

    #[test]
    fn user_dir_prefers_the_override_then_xdg_then_home() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let all = env(&[
            ("VOLVELLE_USER_LIBRARY", "/lib"),
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(user_dir(all), Some(PathBuf::from("/lib")));
        let xdg = env(&[
            ("VOLVELLE_USER_LIBRARY", ""),
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(user_dir(xdg), Some(PathBuf::from("/xdg/volvelle/library")));
        let home = env(&[("HOME", "/home/u")]);
        assert_eq!(
            user_dir(home),
            Some(PathBuf::from("/home/u/.local/share/volvelle/library"))
        );
        assert_eq!(user_dir(env(&[])), None);
        assert_eq!(
            project_dir(Path::new("/p")),
            PathBuf::from("/p/.volvelle/library")
        );
    }

    #[test]
    fn names_are_lowercase_slugs() {
        for ok in ["a", "sheet", "map-2", "0", &"x".repeat(64)] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "Sheet",
            "a b",
            "a_b",
            "../x",
            "a.json",
            &"x".repeat(65),
        ] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
        assert!(
            valid_name("Character Sheet!")
                .unwrap_err()
                .ends_with("; allowed: \"character-sheet\"")
        );
        assert!(!valid_name("--").unwrap_err().contains("allowed"));
    }

    #[test]
    fn saved_specs_leave_out_nulls_and_still_round_trip() {
        let lib = Library {
            project: None,
            user: Some(temp("nulls")),
        };
        let spec = parse_spec(json!({"type": "box", "children": [
            {"type": "input", "id": "name", "value": ""},
            {"type": "table", "headers": ["a"], "rows": [["1"]]},
            {"type": "progress", "value": 0.0}
        ]}))
        .unwrap();
        let entry = Entry::new("form", &spec, Origin::default());
        assert!(
            entry.spec.to_string().contains("null"),
            "serde writes nulls"
        );
        let path = lib.save(Scope::User, &entry).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("null"), "{text}");
        let raw: Value = serde_json::from_str(&text).unwrap();
        let input = &raw["spec"]["children"][0];
        assert_eq!(
            (&input["value"], &input["secret"]),
            (&json!(""), &json!(false))
        );
        assert!(input.get("label").is_none());
        assert_eq!(raw["spec"]["children"][2]["value"], 0.0);
        let (_, _, back) = lib.load("form").unwrap();
        let reparsed = serde_json::to_value(parse_spec(back.spec).unwrap()).unwrap();
        assert_eq!(reparsed, entry.spec);
        fs::remove_dir_all(lib.user.unwrap()).unwrap();
    }

    #[test]
    fn save_and_load_round_trip() {
        let lib = Library {
            project: None,
            user: Some(temp("round")),
        };
        let origin = Origin {
            view: "sheet".into(),
            pane: Some("p1".into()),
            by: By::Agent,
        };
        let entry = Entry::new("sheet", &sheet(), origin);
        let path = lib.save(Scope::User, &entry).unwrap();
        assert_eq!(path, lib.user.as_ref().unwrap().join("sheet.json"));
        assert!(lib.exists(Scope::User, "sheet"));
        assert!(lib.save(Scope::Project, &entry).is_err(), "no project");
        let (scope, _, back) = lib.load("sheet").unwrap();
        assert_eq!(scope, Scope::User);
        assert_eq!(back.origin.pane.as_deref(), Some("p1"));
        assert_eq!(back.origin.by, By::Agent);
        assert_eq!(lib.list()[0].by, By::Agent);
        assert!(lib.get(Scope::User, "sheet").unwrap().is_some());
        assert!(lib.get(Scope::User, "map").unwrap().is_none());
        assert!(lib.get(Scope::Project, "sheet").unwrap().is_none());
        assert_eq!(back.saved_at_ms, entry.saved_at_ms);
        let reparsed = serde_json::to_value(parse_spec(back.spec).unwrap()).unwrap();
        assert_eq!(reparsed, entry.spec);
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for key in ["name", "spec", "saved_at_ms", "origin", "summary"] {
            assert!(raw.get(key).is_some(), "{key}");
        }
        let err = lib.load("missing").unwrap_err();
        assert_eq!(err, "no saved view \"missing\"; allowed: \"sheet\"");
        fs::remove_dir_all(lib.user.unwrap()).unwrap();
    }

    #[test]
    fn entries_without_by_were_saved_by_the_user() {
        let dir = temp("by");
        let lib = Library {
            project: None,
            user: Some(dir.clone()),
        };
        fs::create_dir_all(&dir).unwrap();
        let old = json!({"name": "old", "spec": {"type": "text", "text": "hi"},
            "saved_at_ms": 1, "origin": {"view": "v"}, "summary": "hi"});
        fs::write(dir.join("old.json"), old.to_string()).unwrap();
        let (_, _, entry) = lib.load("old").unwrap();
        assert_eq!(entry.origin.by, By::User);
        assert_eq!(lib.list()[0].by, By::User);
        let listed = serde_json::to_value(&lib.list()[0]).unwrap();
        assert_eq!(listed["by"], "user");
        let text = serde_json::to_value(&entry).unwrap();
        assert_eq!(text["origin"]["by"], "user");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn project_entries_shadow_user_entries() {
        let root = temp("shadow");
        let lib = Library {
            project: Some(project_dir(&root.join("proj"))),
            user: Some(root.join("user")),
        };
        assert_eq!(lib.scopes(), [Scope::Project, Scope::User]);
        let origin = |v: &str| Origin {
            view: v.into(),
            pane: None,
            by: By::User,
        };
        lib.save(Scope::User, &Entry::new("sheet", &sheet(), origin("u")))
            .unwrap();
        lib.save(Scope::User, &Entry::new("map", &sheet(), origin("u")))
            .unwrap();
        lib.save(Scope::Project, &Entry::new("sheet", &sheet(), origin("p")))
            .unwrap();
        fs::write(lib.project.as_ref().unwrap().join("broken.json"), "{").unwrap();
        fs::write(lib.project.as_ref().unwrap().join("Not A Name.json"), "{}").unwrap();
        let listed: Vec<(String, Scope, String)> = lib
            .list()
            .into_iter()
            .map(|l| (l.name, l.scope, l.origin_view))
            .collect();
        assert_eq!(
            listed,
            [
                ("broken".into(), Scope::Project, "".into()),
                ("map".into(), Scope::User, "u".into()),
                ("sheet".into(), Scope::Project, "p".into()),
            ]
        );
        assert!(lib.list()[0].summary.starts_with("unreadable: "));
        let (scope, _, entry) = lib.load("sheet").unwrap();
        assert_eq!((scope, entry.origin.view.as_str()), (Scope::Project, "p"));
        assert!(lib.load("broken").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn summary_names_the_view_and_counts_its_atoms() {
        assert_eq!(
            summary(&sheet()),
            "Character sheet · 2 inputs, 1 progress bar, 1 button"
        );
        let untitled = parse_spec(json!({"type": "box", "children": [
            {"type": "markdown", "text": "\n## Inventory\n- rope"},
            {"type": "tree", "id": "bag", "nodes": [{"label": "rope"}]},
            {"type": "pattern", "name": "choose-with-preview@1", "id": "pick",
             "items": [{"label": "a", "preview": "a"}]}
        ]}))
        .unwrap();
        assert_eq!(
            summary(&untitled),
            "Inventory · 1 tree, 1 choose-with-preview@1"
        );
        let long = parse_spec(json!({"type": "text", "text": "x ".repeat(60)})).unwrap();
        let s = summary(&long);
        assert_eq!(s.chars().count(), 80);
        assert!(s.ends_with('…'));
        let empty = parse_spec(json!({"type": "box"})).unwrap();
        assert_eq!(summary(&empty), "empty view");
    }
}

//! stdio MCP server for volvelle. It speaks newline-delimited JSON-RPC 2.0;
//! the surface needed here (initialize, tools/list, tools/call) is small
//! enough to implement directly.

mod log;
mod session;
mod viewer;

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use volvelle_schema::library::Library;
use volvelle_schema::{Event, Node, PatternInfo, Request};

use log::{Kind, Log};
use session::{Monitor, Refusal};
use viewer::{Recv, Viewer};

const DEFAULT_WAIT_MS: u64 = 60_000;
const MAX_WAIT_MS: u64 = 300_000;

const INSTRUCTIONS: &str = "volvelle draws terminal UIs in a pane beside you, so you can show \
things to the user and ask them structured questions. Compose a spec from the fixed atom \
catalog; the viewer owns sizing, focus and colour, so state intent, not layout. Every view is \
a typed function call: `render` returns the shape the answer will take, and `wait_event` \
returns exactly that shape. Protocol per view: render, then any number of re-renders (each \
keeps what the user has entered), then one wait_event that returns the answer. A view with no \
fields or buttons is display-only and cannot be waited on. Prefer a pattern when one fits: \
it carries safe defaults and its own protocol. The user can save any view with Ctrl-S; \
`library` lists saved views, and `render` with `saved` brings one back.";

const RENDER_DESC: &str = "Show a view in the volvelle pane, opening the pane if needed. \
Re-rendering the same `view` updates it in place and keeps what the user has typed or chosen; \
a new `view` name replaces the old one. Atoms: box (row/column, optional border/title), text \
(tone, bold, align), markdown, table, list (selectable with an id), tree (nested nodes that \
fold, selectable with an id; a node's value defaults to its label path joined with /), input, \
select (multi optional), toggle, button, progress, divider, pattern. Interactive atoms need \
unique ids without `:`. Buttons submit every field's value. Pass exactly one of `spec` or \
`saved`, the name of a view the user saved (see `library`). Returns the JSON Schema of the \
eventual answer.\n\nExample: \
{\"view\":\"deploy\",\"spec\":{\"type\":\"box\",\"title\":\"Deploy\",\"children\":[\
{\"type\":\"select\",\"id\":\"env\",\"label\":\"Environment\",\"options\":[\"staging\",\"production\"]},\
{\"type\":\"toggle\",\"id\":\"migrate\",\"label\":\"Run migrations\"},\
{\"type\":\"button\",\"id\":\"go\",\"label\":\"Deploy\",\"tone\":\"accent\"},\
{\"type\":\"button\",\"id\":\"abort\",\"label\":\"Cancel\"}]}}";

const WAIT_DESC: &str = "Wait for the user to answer a view: a button press (submit), Enter on \
a selectable list or tree (select), Esc (cancel), or a pattern's typed answer (answer), whose `next` \
says what the agent owes. Each carries every field's value. Returns \
{\"status\":\"pending\"} if the timeout passes first; call again to keep waiting. Keep \
timeouts short (default 60000 ms, max 300000) and loop. One answer per render.";

const LIBRARY_DESC: &str = "List the views the user saved with Ctrl-S: name, scope (project \
or user; project entries shadow user ones), a one-line summary, when and from which view. With \
`name`, return that entry in full, including its `spec`, to adapt it or to render it with \
`saved`.";

struct Server {
    viewer: Viewer,
    monitor: Monitor,
    log: Log,
    library: Library,
}

fn main() -> anyhow::Result<()> {
    // Claude Code starts the server in the project, so that is where the
    // project library lives.
    let project = std::env::current_dir().ok();
    let mut server = Server {
        library: Library::from_env(project.as_deref()),
        viewer: Viewer::new(project),
        monitor: Monitor::default(),
        log: Log::from_env(),
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    server.viewer.close();
    Ok(())
}

impl Server {
    fn handle(&mut self, msg: Value) -> Option<Value> {
        // Notifications have no id and get no reply.
        let id = msg.get("id")?.clone();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "volvelle", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => Ok(self.call(
                params["name"].as_str().unwrap_or_default(),
                &params["arguments"],
            )),
            other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        })
    }

    fn call(&mut self, tool: &str, args: &Value) -> Value {
        for ev in self.viewer.saved() {
            self.log_saved(&ev);
        }
        let outcome = match tool {
            "render" => self.render(args),
            "wait_event" => self.wait_event(args),
            "close" => self.close(),
            "library" => self.library(args),
            other => Err(format!("unknown tool {other:?}")),
        };
        match outcome {
            Ok(v) => {
                json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap()}]})
            }
            Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
        }
    }

    fn render(&mut self, args: &Value) -> Result<Value, String> {
        let view = view_arg(args).inspect_err(|e| self.log_error(None, e))?;
        self.monitor
            .check_render(&view)
            .map_err(|r| self.refuse(Some(&view), r))?;
        let (spec, saved) =
            spec_arg(&self.library, args).inspect_err(|e| self.log_error(Some(&view), e))?;
        let interactive = has_answer(&spec);
        let pattern = pattern_of(&spec);
        let reports = self.monitor.owes(&view);
        let returns = volvelle_schema::result_schema(&spec);
        // Anything already queued answers an earlier render.
        for ev in self.viewer.drain() {
            match ev {
                Event::Closed => self.monitor.closed(),
                Event::Saved { .. } => self.log_saved(&ev),
                _ => {}
            }
        }
        let spec_json = serde_json::to_value(&spec).unwrap_or(Value::Null);
        self.viewer
            .send(&Request::Render {
                view: view.clone(),
                spec,
            })
            .map_err(|e| e.to_string())
            .inspect_err(|e| self.log_error(Some(&view), e))?;
        let mut body = vec![("spec", spec_json), ("interactive", interactive.into())];
        if reports {
            body.push(("reports_outcome", true.into()));
        }
        if let Some(name) = saved {
            body.push(("saved", name.into()));
        }
        self.log.record(Kind::Render, Some(&view), &body);
        self.monitor.rendered(&view, interactive, pattern);
        Ok(if interactive {
            json!({"view": view, "returns": returns, "next": "wait_event"})
        } else {
            json!({"view": view, "display_only": true, "next": "render to update, close when done"})
        })
    }

    fn wait_event(&mut self, args: &Value) -> Result<Value, String> {
        let view = view_arg(args).inspect_err(|e| self.log_error(None, e))?;
        self.monitor
            .check_wait(&view)
            .map_err(|r| self.refuse(Some(&view), r))?;
        let ms = args["timeout_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            match self.viewer.recv(deadline) {
                Recv::Event(ev @ Event::Saved { .. }) => self.log_saved(&ev),
                Recv::Event(ev) => {
                    if let Some(answer) = self.monitor.accept(&view, &ev) {
                        let mut body = vec![("answer", answer.clone())];
                        if self.monitor.owes(&view) {
                            body.push(("owes", "report_outcome".into()));
                        }
                        self.log.record(Kind::Answer, Some(&view), &body);
                        return Ok(answer);
                    }
                    if matches!(ev, Event::Closed) {
                        self.log.record(
                            Kind::Close,
                            Some(&view),
                            &[("message", "the user closed the viewer".into())],
                        );
                        return Err(
                            "the user closed the volvelle pane; allowed next: render".into()
                        );
                    }
                }
                Recv::Timeout => {
                    self.log.record(Kind::Pending, Some(&view), &[]);
                    return Ok(json!({"status": "pending", "view": view, "next": "wait_event"}));
                }
                Recv::Gone => {
                    self.monitor.closed();
                    let why = "the viewer exited; allowed next: render";
                    self.log_error(Some(&view), why);
                    return Err(why.into());
                }
            }
        }
    }

    fn close(&mut self) -> Result<Value, String> {
        self.monitor
            .check_close()
            .map_err(|r| self.refuse(None, r))?;
        self.viewer.close();
        self.monitor.closed();
        self.log.record(Kind::Close, None, &[]);
        Ok(json!({"status": "closed"}))
    }

    /// Lists saved views, or returns one in full.
    fn library(&mut self, args: &Value) -> Result<Value, String> {
        let Some(name) = args.get("name").filter(|v| !v.is_null()) else {
            let views = self.library.list();
            if views.is_empty() {
                return Ok(json!({
                    "views": [],
                    "hint": "nothing saved yet; the user saves a view with Ctrl-S"
                }));
            }
            return Ok(json!({"views": views}));
        };
        let name = name
            .as_str()
            .ok_or("`name` must be a string, such as \"sheet\"")?;
        let (scope, path, entry) = self
            .library
            .load(name)
            .inspect_err(|e| self.log_error(None, e))?;
        let mut out = serde_json::to_value(entry).map_err(|e| e.to_string())?;
        out["scope"] = scope.as_str().into();
        out["path"] = path.display().to_string().into();
        Ok(out)
    }

    fn log_saved(&mut self, ev: &Event) {
        if let Event::Saved {
            view,
            name,
            scope,
            path,
        } = ev
        {
            let body = [
                ("name", name.as_str().into()),
                ("scope", scope.as_str().into()),
                ("path", path.as_str().into()),
            ];
            self.log.record(Kind::Saved, Some(view), &body);
        }
    }

    /// Logs a refused move and hands back its message.
    fn refuse(&mut self, view: Option<&str>, Refusal(why): Refusal) -> String {
        self.log
            .record(Kind::Refusal, view, &[("message", why.clone().into())]);
        why
    }

    fn log_error(&mut self, view: Option<&str>, message: &str) {
        self.log
            .record(Kind::Error, view, &[("message", message.into())]);
    }
}

fn view_arg(args: &Value) -> Result<String, String> {
    match args["view"].as_str() {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        _ => Err("missing `view`: name the view, e.g. \"deploy-confirm\"".into()),
    }
}

/// The spec to render: inline under `spec`, or the saved view named by
/// `saved`, whose name comes back for the log.
fn spec_arg(library: &Library, args: &Value) -> Result<(Node, Option<String>), String> {
    let given = |k| args.get(k).filter(|v| !v.is_null());
    match (given("spec"), given("saved")) {
        (Some(spec), None) => Ok((volvelle_schema::parse_spec(spec.clone())?, None)),
        (None, Some(saved)) => {
            let name = saved
                .as_str()
                .ok_or("`saved` must be a name from `library`")?;
            let (_, _, entry) = library.load(name)?;
            let spec = volvelle_schema::parse_spec(entry.spec)
                .map_err(|e| format!("{e} (from saved view {name:?})"))?;
            Ok((spec, Some(name.to_string())))
        }
        (Some(_), Some(_)) => Err(
            "give `spec` or `saved`, not both; allowed: an inline spec, a saved view's name".into(),
        ),
        (None, None) => Err(
            "missing `spec`; allowed: an inline spec, `saved` with a name from `library`".into(),
        ),
    }
}

/// Whether anything in the spec can produce an answer.
fn has_answer(spec: &Node) -> bool {
    let mut found = false;
    spec.walk(&mut |n| found |= n.id().is_some());
    found
}

/// The spec's pattern; validation allows at most one per view.
fn pattern_of(spec: &Node) -> Option<&'static PatternInfo> {
    let mut found = None;
    spec.walk(&mut |n| {
        if let Node::Pattern(p) = n {
            found = Some(p.info());
        }
    });
    found
}

fn tools() -> Value {
    json!([
        {
            "name": "render",
            "description": render_description(),
            "inputSchema": render_schema(),
        },
        {
            "name": "wait_event",
            "description": WAIT_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": {"type": "string"},
                    "timeout_ms": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": DEFAULT_WAIT_MS}
                },
                "required": ["view"]
            }
        },
        {
            "name": "close",
            "description": "Close the volvelle pane. Every open view ends.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "library",
            "description": LIBRARY_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "A saved view to return in full."}
                }
            }
        }
    ])
}

/// The render description with a Patterns section generated from the
/// pattern declarations, so the model sees each pattern's moves up front.
fn render_description() -> String {
    let mut out = format!(
        "{RENDER_DESC}\n\nPatterns: write {{\"type\":\"pattern\",\"name\":<name>, ...slots}} \
         wherever an atom fits, at most one per view. Slots are data, not atoms. The answer \
         is {{\"type\":\"answer\",\"id\":<id>,\"value\":<returns>,\"values\":{{...}}}}."
    );
    for p in volvelle_schema::patterns() {
        out.push_str(&format!(
            "\n- {}: {} Slots: {}. Returns {}. Protocol: {}.",
            p.name,
            p.summary,
            slots((p.slots)()).join(", "),
            p.returns,
            p.protocol
        ));
    }
    out
}

/// Slot names from a slot schema: `id`, the other required slots, then the
/// optional ones marked `?`.
fn slots(schema: Value) -> Vec<String> {
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut names: Vec<(bool, String)> = schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| {
            let req = required.contains(&name.as_str());
            (
                !req,
                if req {
                    name.clone()
                } else {
                    format!("{name}?")
                },
            )
        })
        .collect();
    names.sort_by_key(|(optional, name)| (*optional, name != "id", name.clone()));
    names.into_iter().map(|(_, name)| name).collect()
}

/// The tool schema nests a Node under `spec`, so the generated root schema
/// moves into `$defs` and its self-references follow it.
fn render_schema() -> Value {
    let mut node = volvelle_schema::node_schema();
    let obj = node.as_object_mut().expect("schema is an object");
    let mut defs = match obj.remove("$defs") {
        Some(Value::Object(d)) => d,
        _ => Map::new(),
    };
    obj.remove("$schema");
    obj.remove("title");
    retarget(&mut node, "#", "#/$defs/Node");
    for def in defs.values_mut() {
        retarget(def, "#", "#/$defs/Node");
    }
    defs.insert("Node".into(), node);
    json!({
        "type": "object",
        "properties": {
            "view": {"type": "string", "description": "Names this view. Reuse it to update the view in place."},
            "spec": {"$ref": "#/$defs/Node"},
            "saved": {"type": "string", "description": "Render a saved view by name instead of a spec."}
        },
        "required": ["view"],
        "$defs": defs
    })
}

fn retarget(v: &mut Value, from: &str, to: &str) {
    match v {
        Value::Object(map) => {
            if map.get("$ref").and_then(Value::as_str) == Some(from) {
                map.insert("$ref".into(), Value::String(to.into()));
            }
            map.values_mut().for_each(|c| retarget(c, from, to));
        }
        Value::Array(items) => items.iter_mut().for_each(|c| retarget(c, from, to)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::BufReader;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use volvelle_schema::library::{Entry, Origin, Scope, now_ms};

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vv-{tag}-{}-{}", std::process::id(), now_ms()))
    }

    /// A user library holding `sheet`, plus `broken`, whose pattern
    /// version does not exist.
    fn library(root: &std::path::Path) -> Library {
        let lib = Library {
            project: None,
            user: Some(root.join("user")),
        };
        let spec = volvelle_schema::parse_spec(json!({"type": "box", "title": "Sheet",
            "children": [{"type": "button", "id": "ok", "label": "OK"}]}))
        .unwrap();
        let origin = Origin {
            view: "v0".into(),
            pane: None,
        };
        lib.save(Scope::User, &Entry::new("sheet", &spec, origin.clone()))
            .unwrap();
        let mut broken = Entry::new("broken", &spec, origin);
        broken.spec = json!({"type": "pattern", "name": "confirm-destructive@9", "id": "x"});
        lib.save(Scope::User, &broken).unwrap();
        lib
    }

    #[test]
    fn render_takes_exactly_one_of_spec_or_saved() {
        let root = temp("args");
        let lib = library(&root);
        let spec = json!({"type": "text", "text": "hi"});
        let (_, saved) = spec_arg(&lib, &json!({"view": "v", "spec": spec})).unwrap();
        assert_eq!(saved, None);
        let (node, saved) = spec_arg(&lib, &json!({"view": "v", "saved": "sheet"})).unwrap();
        assert_eq!(saved.as_deref(), Some("sheet"));
        assert_eq!(node.children()[0].id(), Some("ok"));
        let both = spec_arg(&lib, &json!({"view": "v", "spec": spec, "saved": "sheet"}));
        assert!(
            both.unwrap_err()
                .starts_with("give `spec` or `saved`, not both")
        );
        let neither = spec_arg(&lib, &json!({"view": "v", "spec": null}));
        assert!(neither.unwrap_err().starts_with("missing `spec`"));
        let missing = spec_arg(&lib, &json!({"view": "v", "saved": "map"})).unwrap_err();
        assert!(missing.starts_with("no saved view \"map\"; allowed: \"broken\", \"sheet\""));
        let broken = spec_arg(&lib, &json!({"view": "v", "saved": "broken"})).unwrap_err();
        assert!(broken.starts_with("invalid spec: "), "{broken}");
        assert!(broken.ends_with(" (from saved view \"broken\")"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_views_render_and_saves_are_logged_without_answering() {
        let root = temp("e2e");
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("v.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let fake = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
            let request: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            let mut w = stream;
            let saved = json!({"type": "saved", "view": "v", "name": "sheet2",
                               "scope": "user", "path": "/lib/sheet2.json"});
            let submit = json!({"type": "submit", "view": "v", "button": "ok", "values": {}});
            writeln!(w, "{saved}\n{submit}").unwrap();
            request
        });
        let mut server = Server {
            viewer: Viewer::at(socket, None),
            monitor: Monitor::default(),
            log: Log::new(root.join("log"), None),
            library: library(&root),
        };
        let text = |v: &Value| v["content"][0]["text"].as_str().unwrap().to_string();

        let rendered = server.call("render", &json!({"view": "v", "saved": "sheet"}));
        assert!(rendered.get("isError").is_none(), "{}", text(&rendered));
        let request = fake.join().unwrap();
        assert_eq!(request["op"], "render");
        assert_eq!(request["spec"]["title"], "Sheet");

        let answer = server.call("wait_event", &json!({"view": "v", "timeout_ms": 5000}));
        let answer: Value = serde_json::from_str(&text(&answer)).unwrap();
        assert_eq!(answer["type"], "submit");

        let listed = server.call("library", &json!({}));
        let listed: Value = serde_json::from_str(&text(&listed)).unwrap();
        let names: Vec<&str> = listed["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["broken", "sheet"]);
        let full = server.call("library", &json!({"name": "sheet"}));
        let full: Value = serde_json::from_str(&text(&full)).unwrap();
        assert_eq!(
            (&full["scope"], &full["origin"]["view"]),
            (&json!("user"), &json!("v0"))
        );
        assert_eq!(full["spec"]["title"], "Sheet");

        let log = fs::read_dir(root.join("log"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let records: Vec<Value> = fs::read_to_string(log.path())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let kinds: Vec<&str> = records
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["start", "render", "saved", "answer"]);
        assert_eq!(records[1]["saved"], "sheet");
        assert_eq!(
            (&records[2]["name"], &records[2]["view"]),
            (&json!("sheet2"), &json!("v"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn render_description_lists_every_pattern() {
        let desc = render_description();
        for p in volvelle_schema::patterns() {
            assert!(desc.contains(p.name) && desc.contains(p.protocol));
        }
        assert!(desc.contains(
            "Slots: id, consequence, summary, cancel_label?, confirm_label?, require_typing?, title?."
        ));
        assert!(desc.contains("Slots: id, items, multi?, title?."));
    }

    #[test]
    fn render_schema_accepts_patterns() {
        let schema = render_schema();
        let text = schema.to_string();
        assert!(text.contains("confirm-destructive@1") && text.contains("choose-with-preview@1"));
        assert_eq!(schema["required"], json!(["view"]));
        assert_eq!(schema["properties"]["saved"]["type"], "string");
        let names: Vec<Value> = tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].clone())
            .collect();
        assert_eq!(names, ["render", "wait_event", "close", "library"]);
    }
}

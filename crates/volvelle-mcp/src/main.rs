//! stdio MCP server for volvelle. It speaks newline-delimited JSON-RPC 2.0;
//! the surface needed here (initialize, tools/list, tools/call) is small
//! enough to implement directly.

mod log;
mod session;
mod viewer;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use volvelle_schema::library::{self, By, Entry, Library, Origin, Scope};
use volvelle_schema::{Event, Node, Op, PatternInfo, Request};

use log::{Kind, Log};
use session::{Monitor, Refusal};
use viewer::{Recv, Viewer};

const DEFAULT_WAIT_MS: u64 = 60_000;
const MAX_WAIT_MS: u64 = 300_000;
/// How long `update` waits for the viewer to say it applied a patch.
const UPDATE_ACK_MS: u64 = 2_000;

const INSTRUCTIONS: &str = "volvelle draws terminal UIs in a pane beside you, so you can show \
things to the user and ask them structured questions. Compose a spec from the fixed atom \
catalog; the viewer owns sizing, focus and colour, so state intent, not layout. Every view is \
a typed function call: `render` returns the shape the answer will take, and `wait_event` \
returns exactly that shape. Protocol per view: render, then any number of re-renders (each \
keeps what the user has entered), then one wait_event that returns the answer. A view with no \
fields or buttons is display-only and cannot be waited on. Prefer a pattern when one fits: \
it carries safe defaults and its own protocol. Saved views: call `library` before composing a \
view that may already exist, and render by name when one fits. `save` views likely to recur \
(dashboards, forms, sheets, menus the session will come back to), not one-off questions. A \
saved view is static: to adapt one, fetch it with `library {name}` and send a spec. Atoms \
can `bind` to a `data` document, `update` patches it in place, and a saved template plus \
`data` is the cheap way to redraw.";

const RENDER_DESC: &str = "Show a view in the volvelle pane, opening the pane if needed. \
Re-rendering the same `view` updates it in place and keeps what the user has typed or chosen; \
a new `view` name replaces the old one. Atoms: box (row/column, optional border/title), text \
(tone, bold, align), markdown, table, list (selectable with an id), tree (nested nodes that \
fold, selectable with an id; a node's value defaults to its label path joined with /; \
leaves_only makes Enter on a branch fold it instead of answering), graph (nodes {id, label?, \
tone?} and edges {from, to, label?}, direction down or right, a highlighted `current` node; the \
viewer lays it out, so never place anything; at most 64 nodes and 256 edges; selectable with an \
id, answering with the node id), input, \
select (multi optional), toggle, button, progress, divider, pattern. Interactive atoms need \
unique ids without `:`. Buttons submit every field's value. Pass exactly one of `spec` or \
`saved`, a name from `library`; prefer `saved` when one fits, as it sends no spec. Returns the JSON Schema of the \
eventual answer. Data binding: text, markdown, progress, list, table, input, toggle, \
single select and graph take `bind`, a JSON Pointer into `data`, in place of their literal \
(text, value, items, rows or a graph's `current` node id). Every bind must resolve to the type its atom needs. Bound fields write \
the user's input back into the answer's `data`; change the data later with `update`.\n\nExample: \
{\"view\":\"deploy\",\"spec\":{\"type\":\"box\",\"title\":\"Deploy\",\"children\":[\
{\"type\":\"select\",\"id\":\"env\",\"label\":\"Environment\",\"options\":[\"staging\",\"production\"]},\
{\"type\":\"toggle\",\"id\":\"migrate\",\"label\":\"Run migrations\"},\
{\"type\":\"button\",\"id\":\"go\",\"label\":\"Deploy\",\"tone\":\"accent\"},\
{\"type\":\"button\",\"id\":\"abort\",\"label\":\"Cancel\"}]}}";

const WAIT_DESC: &str = "Wait for the user to answer a view: a button press (submit), Enter on \
a selectable list, tree or graph (select), Esc (cancel), or a pattern's typed answer (answer), whose `next` \
says what the agent owes. Each carries every field's value. Returns \
{\"status\":\"pending\"} if the timeout passes first; call again to keep waiting. Keep \
timeouts short (default 60000 ms, max 300000) and loop. One answer per render. A view \
rendered with `data` also returns `data`: the document with the user's input written back to \
each bound field's pointer, or as it was on cancel.";

const UPDATE_DESC: &str = "Patch the data of a view rendered with `data` and redraw what is \
bound to it in place, far cheaper than a re-render. `patch` is a JSON Patch subset: \
replace, add (`-` appends to an array) and remove, applied all or nothing; a patch that fails \
or leaves a bind unfit is refused whole. Focus, scroll and the user's input stay. A bound \
field the user has edited keeps their value; `kept` lists the pointers that did. Allowed \
wherever a re-render is, display-only views included. Does not answer the view; \
`returns` comes back only when the answer's type changed.\n\nExample: \
{\"view\":\"sheet\",\"patch\":[{\"op\":\"replace\",\"path\":\"/hp\",\"value\":0.4},\
{\"op\":\"add\",\"path\":\"/inventory/-\",\"value\":\"rope\"}]}";

const LIBRARY_DESC: &str = "List saved views; call it before composing a view that may already \
exist. Each has a name, scope (project or user; project shadows user), one-line summary, when, \
from which view, and `by` (user, via Ctrl-S, or agent, via `save`). With `name`, return that \
entry in full, `spec` included, to adapt it; to reuse it as is, render with `saved`.";

const SAVE_DESC: &str = "Save a view to reuse it later with `render {view, saved}`, which \
costs far fewer tokens than sending the spec again. Pass exactly one of `view`, a view \
rendered in this session, or `spec`. Scope is project (default) or user. The user's entries \
cannot be replaced; your own need `overwrite: true`. Returns the name, scope and summary.";

struct Server {
    viewer: Viewer,
    monitor: Monitor,
    log: Log,
    library: Library,
    /// The spec last rendered under each view id, for `save`.
    rendered: HashMap<String, Node>,
    /// The data document each bound view reads, as the viewer holds it.
    data: HashMap<String, Value>,
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
        rendered: HashMap::new(),
        data: HashMap::new(),
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
            "update" => self.update(args),
            "wait_event" => self.wait_event(args),
            "close" => self.close(),
            "library" => self.library(args),
            "save" => self.save(args),
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
        let data = data_arg(args).inspect_err(|e| self.log_error(Some(&view), e))?;
        let shown = volvelle_schema::bind(&spec, data.as_ref())
            .map_err(|e| format!("invalid data: {e}"))
            .inspect_err(|e| self.log_error(Some(&view), e))?;
        let interactive = has_answer(&spec);
        let pattern = pattern_of(&spec);
        let reports = self.monitor.owes(&view);
        let bound = has_binds(&spec);
        let returns = returns(&shown, bound);
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
                spec: spec.clone(),
                data: data.clone(),
            })
            .map_err(|e| e.to_string())
            .inspect_err(|e| self.log_error(Some(&view), e))?;
        self.rendered.insert(view.clone(), spec);
        let mut body = vec![("spec", spec_json), ("interactive", interactive.into())];
        match data {
            Some(data) => {
                body.push(("data", data.clone()));
                self.data.insert(view.clone(), data);
            }
            None => {
                self.data.remove(&view);
            }
        }
        if reports {
            body.push(("reports_outcome", true.into()));
        }
        if let Some(name) = saved {
            body.push(("saved", name.into()));
        }
        self.log.record(Kind::Render, Some(&view), &body);
        self.monitor.rendered(&view, interactive, pattern);
        Ok(match (interactive, bound) {
            (true, false) => json!({"view": view, "returns": returns, "next": "wait_event"}),
            (true, true) => {
                json!({"view": view, "returns": returns, "next": "update, wait_event"})
            }
            (false, false) => {
                json!({"view": view, "display_only": true, "next": "render to update, close when done"})
            }
            (false, true) => {
                json!({"view": view, "display_only": true, "next": "update, render, close when done"})
            }
        })
    }

    /// Patches a bound view's data in place. The patch is applied to the
    /// server's copy and the binds re-checked first, so a refused patch
    /// changes nothing on either side.
    fn update(&mut self, args: &Value) -> Result<Value, String> {
        let view = view_arg(args).inspect_err(|e| self.log_error(None, e))?;
        let next = self
            .monitor
            .check_update(&view)
            .map_err(|r| self.refuse(Some(&view), r))?;
        let result = self.patched(&view, args);
        let (patch, data) = result.inspect_err(|e| self.log_error(Some(&view), e))?;
        if !self.viewer.is_open() {
            self.monitor.closed();
            let why = format!("view {view:?} was closed; allowed next: render");
            return Err(self.refuse(Some(&view), Refusal(why)));
        }
        let spec = &self.rendered[&view];
        let before =
            volvelle_schema::result_schema(&volvelle_schema::resolve(spec, &self.data[&view]));
        let shown = volvelle_schema::resolve(spec, &data);
        let changed = has_answer(spec) && volvelle_schema::result_schema(&shown) != before;
        let returns = returns(&shown, true);
        self.viewer
            .send_open(&Request::Update {
                view: view.clone(),
                patch: patch.clone(),
            })
            .map_err(|e| format!("{e:#}; allowed next: render"))
            .inspect_err(|e| self.log_error(Some(&view), e))?;
        // The viewer has the patch whether or not it answers in time, so the
        // server's copy follows it either way.
        self.data.insert(view.clone(), data);
        let deadline = Instant::now() + Duration::from_millis(UPDATE_ACK_MS);
        let Some(kept) = self.viewer.updated(&view, deadline) else {
            let why = "the viewer did not acknowledge the update; allowed next: render";
            self.log_error(Some(&view), why);
            return Err(why.into());
        };
        let patch_json = serde_json::to_value(&patch).unwrap_or(Value::Null);
        let body = [("patch", patch_json), ("kept", json!(kept))];
        self.log.record(Kind::Update, Some(&view), &body);
        let mut out = json!({"view": view, "kept": kept, "next": next});
        if changed {
            out["returns"] = returns;
        }
        Ok(out)
    }

    /// The patch and the data it makes, refused whole if any op fails or a
    /// bind no longer fits.
    fn patched(&self, view: &str, args: &Value) -> Result<(Vec<Op>, Value), String> {
        let Some(data) = self.data.get(view) else {
            return Err(format!(
                "view {view:?} was rendered without `data`, so there is nothing to patch; \
                 allowed next: render with `data`"
            ));
        };
        let patch: Vec<Op> = serde_json::from_value(args["patch"].clone()).map_err(|e| {
            format!(
                "invalid patch: {e}; allowed: an array of {{op: replace|add|remove, path, value}}"
            )
        })?;
        let data = volvelle_schema::apply_patch(data, &patch)
            .map_err(|v| format!("invalid patch: {v}"))?;
        volvelle_schema::check_binds(&self.rendered[view], Some(&data)).map_err(|errors| {
            let lines: Vec<String> = errors
                .iter()
                .map(|v| format!("invalid patch: {v}"))
                .collect();
            lines.join("\n")
        })?;
        Ok((patch, data))
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
                    if let Some(mut answer) = self.monitor.accept(&view, &ev) {
                        if let Some(data) = self.answered_data(&view, &ev) {
                            answer["data"] = data;
                        }
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

    /// A bound view's data as the answer leaves it: the user's input written
    /// back to each bound field's pointer, or untouched on cancel.
    fn answered_data(&self, view: &str, ev: &Event) -> Option<Value> {
        let data = self.data.get(view)?;
        Some(match (ev, ev.values()) {
            (Event::Cancel { .. }, _) | (_, None) => data.clone(),
            (_, Some(values)) => volvelle_schema::write_back(&self.rendered[view], data, values),
        })
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
                    "hint": "nothing saved yet; the user saves with Ctrl-S, you with `save`"
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

    /// Saves a view the agent expects to reuse. The user's entries are
    /// never replaced, and the agent's own only with `overwrite`.
    fn save(&mut self, args: &Value) -> Result<Value, String> {
        let result = self.save_entry(args);
        if let Err(e) = &result {
            self.log_error(args["view"].as_str(), e);
        }
        result
    }

    fn save_entry(&mut self, args: &Value) -> Result<Value, String> {
        let name = args["name"]
            .as_str()
            .ok_or("missing `name`: name the saved view, e.g. \"sheet\"")?;
        library::valid_name(name)?;
        let scope = match args.get("scope").and_then(Value::as_str) {
            None => Scope::Project,
            Some("project") => Scope::Project,
            Some("user") => Scope::User,
            Some(other) => return Err(format!("unknown scope {other:?}; allowed: project, user")),
        };
        if self.library.dir(scope).is_none() {
            let allowed: Vec<&str> = self.library.scopes().iter().map(|s| s.as_str()).collect();
            return Err(format!(
                "no {scope} library; allowed: {}",
                allowed.join(", ")
            ));
        }
        let given = |k| args.get(k).filter(|v| !v.is_null());
        let (spec, view) = match (given("view"), given("spec")) {
            (Some(view), None) => {
                let view = view.as_str().ok_or("`view` must be a view id")?;
                let Some(spec) = self.rendered.get(view) else {
                    let mut ids: Vec<String> =
                        self.rendered.keys().map(|v| format!("{v:?}")).collect();
                    ids.sort();
                    let allowed = if ids.is_empty() {
                        "nothing rendered yet; pass `spec` instead".to_string()
                    } else {
                        ids.join(", ")
                    };
                    return Err(format!(
                        "no view {view:?} rendered in this session; allowed: {allowed}"
                    ));
                };
                (spec.clone(), view.to_string())
            }
            (None, Some(spec)) => (volvelle_schema::parse_spec(spec.clone())?, String::new()),
            (Some(_), Some(_)) => {
                return Err(
                    "give `view` or `spec`, not both; allowed: a rendered view id, an inline spec"
                        .into(),
                );
            }
            (None, None) => {
                return Err(
                    "missing `view`; allowed: a rendered view id, `spec` with an inline spec"
                        .into(),
                );
            }
        };
        let overwrite = args["overwrite"].as_bool().unwrap_or(false);
        if let Some(old) = self.library.get(scope, name)? {
            let free = self.free_names(scope, name);
            match old.origin.by {
                By::User => {
                    return Err(format!(
                        "{name:?} in the {scope} library was saved by the user and only they can replace it; allowed: {free}"
                    ));
                }
                By::Agent if !overwrite => {
                    return Err(format!(
                        "{name:?} is already saved in the {scope} library; allowed: `overwrite: true`, {free}"
                    ));
                }
                By::Agent => {}
            }
        }
        let origin = Origin {
            view: view.clone(),
            pane: None,
            by: By::Agent,
        };
        let mut entry = Entry::new(name, &spec, origin);
        if let Some(summary) = args["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            entry.summary = summary.to_string();
        }
        let path = self
            .library
            .save(scope, &entry)
            .map_err(|e| e.to_string())?;
        let body = [
            ("name", name.into()),
            ("scope", scope.as_str().into()),
            ("path", path.display().to_string().into()),
            ("by", By::Agent.as_str().into()),
        ];
        let view = Some(view.as_str()).filter(|v| !v.is_empty());
        self.log.record(Kind::Saved, view, &body);
        Ok(json!({
            "name": name,
            "scope": scope,
            "summary": entry.summary,
            "next": format!("render with {{\"saved\": {name:?}}}"),
        }))
    }

    /// Names that are free to save to: the first unused `<name>-<n>` in
    /// `scope`, and `name` in the project library when saving to the user
    /// one, since a project entry would not be shadowed.
    fn free_names(&self, scope: Scope, name: &str) -> String {
        let base = name.get(..61).unwrap_or(name).trim_end_matches('-');
        let next = (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|n| !self.library.exists(scope, n))
            .expect("some suffix is free");
        let mut free = vec![format!("{next:?}")];
        if scope == Scope::User
            && self.library.dir(Scope::Project).is_some()
            && !self.library.exists(Scope::Project, name)
        {
            free.push(format!("{name:?} with scope project"));
        }
        free.join(", ")
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
                ("by", By::User.as_str().into()),
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

/// The optional data document for a spec's binds, which must be an object.
fn data_arg(args: &Value) -> Result<Option<Value>, String> {
    match args.get("data").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(data @ Value::Object(_)) => Ok(Some(data.clone())),
        Some(_) => Err(
            "`data` must be a JSON object; allowed: an object such as {\"hero\": {\"hp\": 0.5}}"
                .into(),
        ),
    }
}

/// Whether any atom in the spec binds into data.
fn has_binds(spec: &Node) -> bool {
    let mut found = false;
    spec.walk(&mut |n| found |= n.bind().is_some());
    found
}

/// The answer's schema for a spec as drawn. A bound view's answers also
/// carry `data`, so each variant gains it.
fn returns(shown: &Node, bound: bool) -> Value {
    let mut schema = volvelle_schema::result_schema(shown);
    if bound {
        for variant in schema["oneOf"].as_array_mut().into_iter().flatten() {
            variant["properties"]["data"] = json!({"type": "object"});
        }
    }
    schema
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
            "name": "update",
            "description": UPDATE_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": {"type": "string"},
                    "patch": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {"enum": ["replace", "add", "remove"]},
                                "path": {"type": "string", "description": "A JSON Pointer into the view's data."},
                                "value": {"description": "For replace and add."}
                            },
                            "required": ["op", "path"]
                        }
                    }
                },
                "required": ["view", "patch"]
            }
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
        },
        {
            "name": "save",
            "description": SAVE_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]{0,63}$"},
                    "view": {"type": "string", "description": "Save the spec last rendered under this view id."},
                    "spec": {"type": "object", "description": "Save this spec instead; it is validated first."},
                    "scope": {"type": "string", "enum": ["project", "user"], "default": "project"},
                    "summary": {"type": "string", "description": "One line for `library`; generated when omitted."},
                    "overwrite": {"type": "boolean", "default": false, "description": "Replace your own entry with this name."}
                },
                "required": ["name"]
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
            "saved": {"type": "string", "description": "Render a saved view by name instead of a spec."},
            "data": {"type": "object", "description": "The document the spec's `bind` pointers read. Required when the spec binds."}
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
            by: By::User,
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
            rendered: HashMap::new(),
            data: HashMap::new(),
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
            (&records[2]["name"], &records[2]["view"], &records[2]["by"]),
            (&json!("sheet2"), &json!("v"), &json!("user"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A server whose viewer is a fake that takes one render, with a
    /// project library and the user library from [`library`].
    fn server(root: &std::path::Path) -> (Server, std::thread::JoinHandle<Value>) {
        fs::create_dir_all(root).unwrap();
        let socket = root.join("v.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let fake = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut lines = BufReader::new(stream).lines();
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
        });
        let mut library = library(root);
        library.project = Some(root.join("proj"));
        let server = Server {
            viewer: Viewer::at(socket, None),
            monitor: Monitor::default(),
            log: Log::new(root.join("log"), None),
            library,
            rendered: HashMap::new(),
            data: HashMap::new(),
        };
        (server, fake)
    }

    fn reply(v: &Value) -> Result<Value, String> {
        let text = v["content"][0]["text"].as_str().unwrap();
        if v.get("isError").is_some() {
            return Err(text.to_string());
        }
        Ok(serde_json::from_str(text).unwrap())
    }

    fn records(root: &std::path::Path) -> Vec<Value> {
        let log = fs::read_dir(root.join("log"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        fs::read_to_string(log.path())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn save_keeps_the_spec_last_rendered_under_a_view() {
        let root = temp("save-view");
        let (mut server, fake) = server(&root);
        let spec = json!({"type": "box", "title": "Dashboard",
            "children": [{"type": "progress", "label": "CI", "value": 0.5}]});
        let missing = reply(&server.call("save", &json!({"name": "dash", "view": "d"})));
        assert_eq!(
            missing.unwrap_err(),
            "no view \"d\" rendered in this session; allowed: nothing rendered yet; pass `spec` instead"
        );
        reply(&server.call("render", &json!({"view": "d", "spec": spec}))).unwrap();
        fake.join().unwrap();

        let saved = reply(&server.call("save", &json!({"name": "dash", "view": "d"}))).unwrap();
        assert_eq!(
            saved,
            json!({"name": "dash", "scope": "project", "summary": "Dashboard · 1 progress bar",
                   "next": "render with {\"saved\": \"dash\"}"})
        );
        let (scope, _, entry) = server.library.load("dash").unwrap();
        assert_eq!(scope, Scope::Project);
        assert_eq!(
            (entry.origin.by, entry.origin.view.as_str()),
            (By::Agent, "d")
        );
        assert_eq!(entry.spec["title"], "Dashboard");

        let listed = reply(&server.call("library", &json!({}))).unwrap();
        let by: Vec<(&str, &str)> = listed["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v["name"].as_str().unwrap(), v["by"].as_str().unwrap()))
            .collect();
        assert_eq!(
            by,
            [("broken", "user"), ("dash", "agent"), ("sheet", "user")]
        );

        let saved = records(&root)
            .into_iter()
            .find(|r| r["kind"] == "saved")
            .unwrap();
        assert_eq!(
            (
                &saved["name"],
                &saved["view"],
                &saved["by"],
                &saved["scope"]
            ),
            (
                &json!("dash"),
                &json!("d"),
                &json!("agent"),
                &json!("project")
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_validates_a_given_spec_and_takes_a_summary() {
        let root = temp("save-spec");
        let (mut server, _fake) = server(&root);
        let spec = json!({"type": "button", "id": "go", "label": "Go"});
        let args = json!({"name": "go", "spec": spec, "scope": "user", "summary": " Launch "});
        let saved = reply(&server.call("save", &args)).unwrap();
        assert_eq!(
            (&saved["scope"], &saved["summary"]),
            (&json!("user"), &json!("Launch"))
        );
        let entry = server.library.get(Scope::User, "go").unwrap().unwrap();
        assert_eq!(
            (entry.origin.by, entry.origin.view.as_str()),
            (By::Agent, "")
        );

        let bad = json!({"name": "x", "spec": {"type": "button", "label": "no id"}});
        assert!(
            reply(&server.call("save", &bad))
                .unwrap_err()
                .starts_with("invalid spec: ")
        );
        let both = json!({"name": "x", "view": "v", "spec": spec});
        assert!(
            reply(&server.call("save", &both))
                .unwrap_err()
                .starts_with("give `view` or `spec`, not both")
        );
        let scope = json!({"name": "x", "spec": spec, "scope": "team"});
        assert_eq!(
            reply(&server.call("save", &scope)).unwrap_err(),
            "unknown scope \"team\"; allowed: project, user"
        );
        let name = json!({"name": "Big Sheet", "spec": spec});
        assert!(
            reply(&server.call("save", &name))
                .unwrap_err()
                .ends_with("; allowed: \"big-sheet\"")
        );
        assert!(!server.library.exists(Scope::Project, "x"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_never_replaces_the_users_entries() {
        let root = temp("save-user");
        let (mut server, _fake) = server(&root);
        let spec = json!({"type": "text", "text": "mine"});
        let args = json!({"name": "sheet", "spec": spec, "scope": "user", "overwrite": true});
        let err = reply(&server.call("save", &args)).unwrap_err();
        assert_eq!(
            err,
            "\"sheet\" in the user library was saved by the user and only they can replace it; \
             allowed: \"sheet-2\", \"sheet\" with scope project"
        );
        let entry = server.library.get(Scope::User, "sheet").unwrap().unwrap();
        assert_eq!(entry.origin.by, By::User);
        assert_eq!(entry.spec["title"], "Sheet");
        // The free name it offers really is free.
        let args = json!({"name": "sheet", "spec": spec});
        assert_eq!(
            reply(&server.call("save", &args)).unwrap()["scope"],
            "project"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_replaces_its_own_entries_only_when_asked() {
        let root = temp("save-own");
        let (mut server, _fake) = server(&root);
        let first = json!({"name": "menu", "spec": {"type": "text", "text": "one"}});
        reply(&server.call("save", &first)).unwrap();
        let second = json!({"name": "menu", "spec": {"type": "text", "text": "two"}});
        let err = reply(&server.call("save", &second)).unwrap_err();
        assert_eq!(
            err,
            "\"menu\" is already saved in the project library; allowed: `overwrite: true`, \"menu-2\""
        );
        let (_, _, entry) = server.library.load("menu").unwrap();
        assert_eq!(entry.spec["text"], "one");
        let mut second = second;
        second["overwrite"] = true.into();
        assert_eq!(
            reply(&server.call("save", &second)).unwrap()["summary"],
            "two"
        );
        let (_, _, entry) = server.library.load("menu").unwrap();
        assert_eq!(entry.spec["text"], "two");
        fs::remove_dir_all(root).unwrap();
    }

    /// A server whose fake viewer answers its i-th request with the lines
    /// in `replies[i]`, then hands back every request it took.
    fn scripted(
        root: &std::path::Path,
        replies: Vec<Vec<Value>>,
    ) -> (Server, std::thread::JoinHandle<Vec<Value>>) {
        fs::create_dir_all(root).unwrap();
        let socket = root.join("v.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let fake = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
            let mut w = stream;
            let mut requests = Vec::new();
            for reply in replies {
                let line = lines.next().unwrap().unwrap();
                requests.push(serde_json::from_str(&line).unwrap());
                for ev in reply {
                    writeln!(w, "{ev}").unwrap();
                }
            }
            requests
        });
        let server = Server {
            viewer: Viewer::at(socket, None),
            monitor: Monitor::default(),
            log: Log::new(root.join("log"), None),
            library: library(root),
            rendered: HashMap::new(),
            data: HashMap::new(),
        };
        (server, fake)
    }

    fn hero() -> Value {
        json!({"type": "box", "title": "Hero", "children": [
            {"type": "progress", "label": "HP", "bind": "/hp"},
            {"type": "input", "id": "name", "label": "Name", "bind": "/name"},
            {"type": "list", "id": "bag", "bind": "/bag"},
            {"type": "button", "id": "ok", "label": "OK"}
        ]})
    }

    fn hero_data() -> Value {
        json!({"hp": 0.5, "name": "Ada", "bag": ["rope", "map"]})
    }

    #[test]
    fn a_saved_template_renders_with_data() {
        let root = temp("bind-render");
        let (mut server, fake) = scripted(&root, vec![vec![]]);
        let spec = volvelle_schema::parse_spec(hero()).unwrap();
        let origin = Origin {
            view: String::new(),
            pane: None,
            by: By::Agent,
        };
        let lib = &server.library;
        lib.save(Scope::User, &Entry::new("hero", &spec, origin))
            .unwrap();

        let args = json!({"view": "h", "saved": "hero", "data": hero_data()});
        let rendered = reply(&server.call("render", &args)).unwrap();
        let variants = rendered["returns"]["oneOf"].as_array().unwrap();
        assert!(
            variants
                .iter()
                .all(|v| v["properties"]["data"] == json!({"type": "object"}))
        );
        assert_eq!(
            variants[2]["properties"]["value"]["enum"],
            json!(["rope", "map"]),
            "a bound list's items reach the answer's type"
        );
        let requests = fake.join().unwrap();
        assert_eq!(requests[0]["data"], hero_data());
        assert_eq!(requests[0]["spec"]["children"][1]["bind"], "/name");
        assert_eq!(server.data["h"], hero_data());

        let render = records(&root)
            .into_iter()
            .find(|r| r["kind"] == "render")
            .unwrap();
        assert_eq!(
            (&render["saved"], &render["data"]),
            (&json!("hero"), &hero_data())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn binds_are_checked_against_data_before_rendering() {
        let root = temp("bind-refuse");
        let (mut server, _fake) = scripted(&root, vec![vec![]]);
        let refused =
            |server: &mut Server, args: Value| reply(&server.call("render", &args)).unwrap_err();
        assert_eq!(
            refused(&mut server, json!({"view": "h", "spec": hero()})),
            "invalid data: at /children/0/bind: the spec binds 3 pointers but render has no \
             `data`; allowed: `data` with a document, literals instead of binds"
        );
        assert_eq!(
            refused(
                &mut server,
                json!({"view": "h", "spec": hero(), "data": [1]})
            ),
            "`data` must be a JSON object; allowed: an object such as {\"hero\": {\"hp\": 0.5}}"
        );
        let data = json!({"hp": 2, "name": "Ada", "bag": []});
        assert_eq!(
            refused(
                &mut server,
                json!({"view": "h", "spec": hero(), "data": data})
            ),
            "invalid data: at /children/0/bind: bind \"/hp\" holds 2, but progress needs a \
             number from 0.0 to 1.0; allowed: a number from 0.0 to 1.0"
        );
        assert!(server.data.is_empty() && server.rendered.is_empty());
        let plain = json!({"view": "p", "spec": {"type": "text", "text": "hi"}, "data": {"a": 1}});
        let shown = reply(&server.call("render", &plain)).unwrap();
        assert_eq!(
            shown["display_only"], true,
            "data without binds is harmless"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn updates_patch_data_atomically_and_pass_kept_through() {
        let root = temp("bind-update");
        let submit = json!({"type": "submit", "view": "h", "button": "ok",
                            "values": {"name": "Ada!"}});
        let ack = json!({"type": "updated", "view": "h", "kept": ["/name"]});
        let (mut server, fake) = scripted(&root, vec![vec![], vec![submit, ack]]);
        let update = |server: &mut Server, patch: Value| {
            reply(&server.call("update", &json!({"view": "h", "patch": patch})))
        };
        let good = json!([
            {"op": "replace", "path": "/hp", "value": 0.25},
            {"op": "replace", "path": "/name", "value": "Grace"}
        ]);
        assert_eq!(
            update(&mut server, good.clone()).unwrap_err(),
            "no view \"h\" has been rendered; allowed next: render"
        );
        let args = json!({"view": "h", "spec": hero(), "data": hero_data()});
        reply(&server.call("render", &args)).unwrap();

        let failing = json!([
            {"op": "replace", "path": "/hp", "value": 0.1},
            {"op": "replace", "path": "/mp", "value": 1}
        ]);
        assert_eq!(
            update(&mut server, failing).unwrap_err(),
            "invalid patch: at /patch/1: nothing at \"/mp\" to replace; allowed: add, an existing path"
        );
        let unfit = json!([{"op": "replace", "path": "/hp", "value": "full"}]);
        assert_eq!(
            update(&mut server, unfit).unwrap_err(),
            "invalid patch: at /children/0/bind: bind \"/hp\" holds \"full\", but progress \
             needs a number from 0.0 to 1.0; allowed: a number from 0.0 to 1.0"
        );
        let removed = json!([{"op": "remove", "path": "/bag"}]);
        assert!(
            update(&mut server, removed)
                .unwrap_err()
                .contains("bind \"/bag\" does not resolve in data")
        );
        assert!(
            update(&mut server, json!([{"op": "move", "path": "/hp"}]))
                .unwrap_err()
                .starts_with("invalid patch: unknown variant `move`")
        );
        assert_eq!(
            server.data["h"],
            hero_data(),
            "refused patches change nothing"
        );

        let updated = update(&mut server, good.clone()).unwrap();
        assert_eq!(
            updated,
            json!({"view": "h", "kept": ["/name"], "next": "update, wait_event"})
        );
        assert_eq!(server.data["h"]["hp"], 0.25);
        let requests = fake.join().unwrap();
        assert_eq!(requests.len(), 2, "refused patches never reach the viewer");
        assert_eq!(
            (&requests[1]["op"], &requests[1]["patch"]),
            (&json!("update"), &good)
        );

        // The answer that arrived before the acknowledgement was held for
        // wait_event, and writes the user's input back into the patched data.
        let answer = server.call("wait_event", &json!({"view": "h", "timeout_ms": 5000}));
        let answer = reply(&answer).unwrap();
        assert_eq!(answer["type"], "submit");
        assert_eq!(
            answer["data"],
            json!({"hp": 0.25, "name": "Ada!", "bag": ["rope", "map"]})
        );

        let recs = records(&root);
        let kinds: Vec<&str> = recs.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "start", "refusal", "render", "error", "error", "error", "error", "update",
                "answer"
            ]
        );
        assert_eq!(
            (&recs[7]["view"], &recs[7]["patch"], &recs[7]["kept"]),
            (&json!("h"), &good, &json!(["/name"]))
        );
        assert_eq!(recs[8]["answer"]["data"]["name"], "Ada!");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn display_views_take_updates_until_closed() {
        let root = temp("bind-display");
        let ack = json!({"type": "updated", "view": "d", "kept": []});
        let (mut server, fake) = scripted(&root, vec![vec![], vec![ack], vec![]]);
        let spec = json!({"type": "progress", "label": "Build", "bind": "/done"});
        let args = json!({"view": "d", "spec": spec, "data": {"done": 0.1}});
        let shown = reply(&server.call("render", &args)).unwrap();
        assert_eq!(shown["next"], "update, render, close when done");
        let patch = json!([{"op": "replace", "path": "/done", "value": 0.9}]);
        let args = json!({"view": "d", "patch": patch});
        assert_eq!(
            reply(&server.call("update", &args)).unwrap(),
            json!({"view": "d", "kept": [], "next": "update, render, close when done"})
        );
        reply(&server.call("close", &json!({}))).unwrap();
        assert_eq!(
            reply(&server.call("update", &args)).unwrap_err(),
            "view \"d\" was closed; allowed next: render"
        );
        assert_eq!(fake.join().unwrap()[2]["op"], "close");

        let plain = json!({"view": "p", "spec": {"type": "text", "text": "hi"}});
        let (mut server, _fake) = scripted(&root.join("plain"), vec![vec![]]);
        reply(&server.call("render", &plain)).unwrap();
        assert_eq!(
            reply(&server.call("update", &json!({"view": "p", "patch": []}))).unwrap_err(),
            "view \"p\" was rendered without `data`, so there is nothing to patch; \
             allowed next: render with `data`"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_cancel_returns_the_data_without_the_users_input() {
        let root = temp("bind-cancel");
        let cancel = json!({"type": "cancel", "view": "h", "values": {"name": "Ada!"}});
        let (mut server, _fake) = scripted(&root, vec![vec![cancel]]);
        let args = json!({"view": "h", "spec": hero(), "data": hero_data()});
        reply(&server.call("render", &args)).unwrap();
        let answer = server.call("wait_event", &json!({"view": "h", "timeout_ms": 5000}));
        let answer = reply(&answer).unwrap();
        assert_eq!(answer["type"], "cancel");
        assert_eq!(answer["values"]["name"], "Ada!");
        assert_eq!(answer["data"], hero_data());
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
        assert_eq!(
            names,
            ["render", "update", "wait_event", "close", "library", "save"]
        );
    }
}

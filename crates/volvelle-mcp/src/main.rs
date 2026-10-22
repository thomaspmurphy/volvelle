//! stdio MCP server for volvelle. It speaks newline-delimited JSON-RPC 2.0;
//! the surface needed here (initialize, tools/list, tools/call) is small
//! enough to implement directly.

mod log;
mod session;
mod viewer;

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use volvelle_schema::{Event, Node, Request};

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
fields or buttons is display-only and cannot be waited on.";

const RENDER_DESC: &str = "Show a view in the volvelle pane, opening the pane if needed. \
Re-rendering the same `view` updates it in place and keeps what the user has typed or chosen; \
a new `view` name replaces the old one. Atoms: box (row/column, optional border/title), text \
(tone, bold, align), markdown, table, list (selectable with an id), input, select (multi \
optional), toggle, button, progress, divider. Interactive atoms need unique ids. Buttons \
submit every field's value. Returns the JSON Schema of the eventual answer.\n\nExample: \
{\"view\":\"deploy\",\"spec\":{\"type\":\"box\",\"title\":\"Deploy\",\"children\":[\
{\"type\":\"select\",\"id\":\"env\",\"label\":\"Environment\",\"options\":[\"staging\",\"production\"]},\
{\"type\":\"toggle\",\"id\":\"migrate\",\"label\":\"Run migrations\"},\
{\"type\":\"button\",\"id\":\"go\",\"label\":\"Deploy\",\"tone\":\"accent\"},\
{\"type\":\"button\",\"id\":\"abort\",\"label\":\"Cancel\"}]}}";

const WAIT_DESC: &str = "Wait for the user to answer a view: a button press (submit), Enter on \
a selectable list (select), or Esc (cancel). Each carries every field's value. Returns \
{\"status\":\"pending\"} if the timeout passes first; call again to keep waiting. Keep \
timeouts short (default 60000 ms, max 300000) and loop. One answer per render.";

struct Server {
    viewer: Viewer,
    monitor: Monitor,
    log: Log,
}

fn main() -> anyhow::Result<()> {
    let mut server = Server {
        viewer: Viewer::new(),
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
        let outcome = match tool {
            "render" => self.render(args),
            "wait_event" => self.wait_event(args),
            "close" => {
                self.viewer.close();
                self.monitor.closed();
                self.log.record(Kind::Close, None, &[]);
                Ok(json!({"status": "closed"}))
            }
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
        let spec = volvelle_schema::parse_spec(args["spec"].clone())
            .inspect_err(|e| self.log_error(Some(&view), e))?;
        let interactive = has_answer(&spec);
        let returns = volvelle_schema::result_schema(&spec);
        // Anything already queued answers an earlier render.
        for ev in self.viewer.drain() {
            if matches!(ev, Event::Closed) {
                self.monitor.closed();
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
        self.log.record(
            Kind::Render,
            Some(&view),
            &[("spec", spec_json), ("interactive", interactive.into())],
        );
        self.monitor.rendered(&view, interactive);
        Ok(if interactive {
            json!({"view": view, "returns": returns, "next": "wait_event"})
        } else {
            json!({"view": view, "display_only": true, "next": "render to update, close when done"})
        })
    }

    fn wait_event(&mut self, args: &Value) -> Result<Value, String> {
        let view = view_arg(args).inspect_err(|e| self.log_error(None, e))?;
        self.monitor.check_wait(&view).map_err(|Refusal(why)| {
            self.log.record(
                Kind::Refusal,
                Some(&view),
                &[("message", why.clone().into())],
            );
            why
        })?;
        let ms = args["timeout_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            match self.viewer.recv(deadline) {
                Recv::Event(ev) => {
                    if let Some(answer) = self.monitor.accept(&view, &ev) {
                        self.log
                            .record(Kind::Answer, Some(&view), &[("answer", answer.clone())]);
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

/// Whether anything in the spec can produce an answer.
fn has_answer(spec: &Node) -> bool {
    let mut found = false;
    spec.walk(&mut |n| found |= n.id().is_some());
    found
}

fn tools() -> Value {
    json!([
        {
            "name": "render",
            "description": RENDER_DESC,
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
        }
    ])
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
            "spec": {"$ref": "#/$defs/Node"}
        },
        "required": ["view", "spec"],
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

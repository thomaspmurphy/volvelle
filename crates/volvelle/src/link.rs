//! The viewer's side of the wire: a Unix socket carrying newline-delimited
//! JSON, plus best-effort reporting to herdr's sidebar.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use volvelle_schema::{Event, Request};

pub enum Msg {
    Request(Request),
    Connected(u64, UnixStream),
    Disconnected(u64),
}

/// Accepts clients in the background. The newest connection receives events.
pub fn listen(path: &Path, tx: Sender<Msg>) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    thread::spawn(move || {
        for (conn, stream) in (0u64..).zip(listener.incoming()) {
            let Ok(stream) = stream else { continue };
            let Ok(writer) = stream.try_clone() else {
                continue;
            };
            if tx.send(Msg::Connected(conn, writer)).is_err() {
                return;
            }
            let tx = tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    // Clients validate before sending; a malformed line is dropped.
                    if let Ok(req) = serde_json::from_str::<Request>(&line) {
                        let _ = tx.send(Msg::Request(req));
                    }
                }
                let _ = tx.send(Msg::Disconnected(conn));
            });
        }
    });
    Ok(())
}

/// Delivers events to the connected client, holding them while none is.
#[derive(Default)]
pub struct Outbox {
    conn: Option<(u64, UnixStream)>,
    pending: Vec<Event>,
}

impl Outbox {
    pub fn connect(&mut self, id: u64, stream: UnixStream) {
        self.conn = Some((id, stream));
        for ev in std::mem::take(&mut self.pending) {
            self.send(ev);
        }
    }

    pub fn disconnect(&mut self, id: u64) {
        if self.conn.as_ref().is_some_and(|(c, _)| *c == id) {
            self.conn = None;
        }
    }

    pub fn send(&mut self, ev: Event) {
        if let Some((_, stream)) = &mut self.conn {
            let mut line = serde_json::to_string(&ev).expect("event serialises");
            line.push('\n');
            if stream.write_all(line.as_bytes()).is_ok() {
                return;
            }
            self.conn = None;
        }
        self.pending.push(ev);
    }
}

/// Shows the viewer in herdr's sidebar as blocked while it waits on the human.
pub struct Herdr {
    pane: Option<String>,
    last: Option<bool>,
}

impl Herdr {
    pub fn from_env() -> Self {
        let inside = std::env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        let pane = std::env::var("HERDR_PANE_ID").ok().filter(|_| inside);
        Herdr { pane, last: None }
    }

    pub fn report(&mut self, blocked: bool, view: &str) {
        let Some(pane) = &self.pane else { return };
        if self.last == Some(blocked) {
            return;
        }
        self.last = Some(blocked);
        let state = if blocked { "blocked" } else { "idle" };
        let mut args = vec![
            "pane",
            "report-agent",
            pane,
            "--source",
            "volvelle",
            "--agent",
            "volvelle",
            "--state",
            state,
        ];
        let message = format!("waiting on {view}");
        if blocked {
            args.extend(["--message", &message]);
        }
        herdr(&args);
    }

    pub fn release(&self) {
        if let Some(pane) = &self.pane {
            herdr(&[
                "pane",
                "release-agent",
                pane,
                "--source",
                "volvelle",
                "--agent",
                "volvelle",
            ]);
        }
    }
}

fn herdr(args: &[&str]) {
    let child = Command::new("herdr")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        thread::spawn(move || child.wait());
    }
}

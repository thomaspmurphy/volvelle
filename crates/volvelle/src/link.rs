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
    Request(Box<Request>),
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
                        let _ = tx.send(Msg::Request(Box::new(req)));
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

/// Zooms the viewer's herdr pane to fill the screen, on Ctrl-F or when a view
/// asks. A no-op outside herdr.
#[derive(Default)]
pub struct Zoom {
    pane: Option<String>,
    /// The view whose render asked for zoom. Another view replacing it, or
    /// the viewer exiting, zooms back out.
    requested: Option<String>,
    /// Runs herdr calls one at a time, so an off never overtakes an on.
    worker: Option<(Sender<&'static str>, thread::JoinHandle<()>)>,
    #[cfg(test)]
    sent: Vec<&'static str>,
}

impl Zoom {
    pub fn from_env() -> Self {
        let inside = std::env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        let pane = std::env::var("HERDR_PANE_ID").ok().filter(|_| inside);
        let worker = pane.clone().map(|pane| {
            let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
            let handle = thread::spawn(move || {
                for flag in rx {
                    let _ = Command::new("herdr")
                        .args(["pane", "zoom", &pane, flag])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            });
            (tx, handle)
        });
        Zoom {
            pane,
            worker,
            ..Zoom::default()
        }
    }

    /// Whether Ctrl-F does anything here.
    pub fn available(&self) -> bool {
        self.pane.is_some()
    }

    /// The human's Ctrl-F. It leaves any request alone, so a view that asked
    /// still zooms out when it is replaced.
    pub fn toggle(&mut self) {
        self.send("--toggle");
    }

    /// Applies a render's request. Zooms in when a view first asks, and back
    /// out when a view that did not ask replaces one that did. Re-rendering
    /// the view that asked leaves the zoom as the human last set it.
    pub fn render(&mut self, view: &str, zoom: bool) {
        if zoom {
            if self.requested.as_deref() != Some(view) {
                self.requested = Some(view.to_string());
                self.send("--on");
            }
        } else {
            self.release();
        }
    }

    /// Zooms out if a view's request is still in force.
    pub fn release(&mut self) {
        if self.requested.take().is_some() {
            self.send("--off");
        }
    }

    /// Zooms out as the viewer exits, waiting so the process does not end
    /// before herdr hears it.
    pub fn finish(mut self) {
        self.release();
        if let Some((tx, handle)) = self.worker.take() {
            drop(tx);
            let _ = handle.join();
        }
    }

    fn send(&mut self, flag: &'static str) {
        #[cfg(test)]
        self.sent.push(flag);
        if let Some((tx, _)) = &self.worker {
            let _ = tx.send(flag);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn zoom() -> Zoom {
        Zoom {
            pane: Some("p1".into()),
            ..Zoom::default()
        }
    }

    #[test]
    fn a_request_zooms_in_once_and_a_plain_view_zooms_out() {
        let mut z = zoom();
        z.render("map", true);
        z.render("map", true);
        z.render("form", false);
        z.render("form", false);
        assert_eq!(z.sent, ["--on", "--off"]);
    }

    #[test]
    fn a_new_view_asking_zooms_in_again() {
        let mut z = zoom();
        z.render("map", true);
        z.render("graph", true);
        assert_eq!(z.sent, ["--on", "--on"]);
    }

    #[test]
    fn ctrl_f_leaves_the_request_in_force() {
        let mut z = zoom();
        z.toggle();
        z.render("form", false);
        z.render("map", true);
        z.toggle();
        z.render("map", true);
        z.release();
        z.release();
        assert_eq!(z.sent, ["--toggle", "--on", "--toggle", "--off"]);
    }
}

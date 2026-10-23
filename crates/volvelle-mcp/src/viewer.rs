//! Connection to the viewer, which runs in its own herdr pane. Herdr only
//! places the pane; all UI data moves over volvelle's own socket.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use volvelle_schema::{Event, Request};

pub struct Viewer {
    socket: PathBuf,
    /// Passed to the viewer so Ctrl-S can save to the project library.
    project: Option<PathBuf>,
    conn: Option<Conn>,
    /// The pane we split for the viewer, if we made one.
    pane: Option<String>,
}

struct Conn {
    stream: UnixStream,
    events: Receiver<Option<Event>>,
    /// Events read ahead by [`Viewer::saved`], in arrival order.
    held: VecDeque<Option<Event>>,
}

pub enum Recv {
    Event(Event),
    Timeout,
    /// The socket closed without a goodbye.
    Gone,
}

impl Viewer {
    pub fn new(project: Option<PathBuf>) -> Self {
        let socket = std::env::var_os("VOLVELLE_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let tag = std::env::var("HERDR_PANE_ID")
                    .map(|p| p.replace(|c: char| !c.is_ascii_alphanumeric(), "-"))
                    .unwrap_or_else(|_| std::process::id().to_string());
                std::env::temp_dir().join(format!("volvelle-{tag}.sock"))
            });
        Self::at(socket, project)
    }

    pub fn at(socket: PathBuf, project: Option<PathBuf>) -> Self {
        Viewer {
            socket,
            project,
            conn: None,
            pane: None,
        }
    }

    pub fn send(&mut self, req: &Request) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        for _ in 0..2 {
            self.ensure()?;
            let conn = self.conn.as_mut().expect("connected");
            if conn.stream.write_all(line.as_bytes()).is_ok() {
                return Ok(());
            }
            self.conn = None;
        }
        bail!("could not reach the viewer at {}", self.socket.display())
    }

    /// Takes the events that have already arrived.
    pub fn drain(&mut self) -> Vec<Event> {
        let Some(conn) = &mut self.conn else {
            return Vec::new();
        };
        let held: Vec<_> = conn.held.drain(..).collect();
        let events: Vec<Event> = held
            .into_iter()
            .chain(conn.events.try_iter())
            .flatten()
            .collect();
        if events.iter().any(|e| matches!(e, Event::Closed)) {
            self.forget();
        }
        events
    }

    /// Takes the saves that have arrived, holding every other event for
    /// [`Viewer::recv`] and [`Viewer::drain`] in order.
    pub fn saved(&mut self) -> Vec<Event> {
        let Some(conn) = &mut self.conn else {
            return Vec::new();
        };
        let mut saved = Vec::new();
        for ev in conn.events.try_iter() {
            match ev {
                Some(ev @ Event::Saved { .. }) => saved.push(ev),
                other => conn.held.push_back(other),
            }
        }
        saved
    }

    /// Whether a viewer is connected and has not said goodbye. An update
    /// must not open a fresh viewer, which would have no view to patch.
    pub fn is_open(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| {
            !c.held
                .iter()
                .any(|e| matches!(e, None | Some(Event::Closed)))
        })
    }

    /// Sends to the viewer already connected, never starting one.
    pub fn send_open(&mut self, req: &Request) -> anyhow::Result<()> {
        let Some(conn) = &mut self.conn else {
            bail!("the viewer is not running");
        };
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        conn.stream
            .write_all(line.as_bytes())
            .context("the viewer stopped listening")
    }

    /// Waits for the viewer to acknowledge an update to `view`, holding every
    /// other event, answers and saves included, for later calls in order.
    /// Returns the pointers the user's edits kept, or `None` if the viewer
    /// closed or did not answer by `deadline`.
    pub fn updated(&mut self, view: &str, deadline: Instant) -> Option<Vec<String>> {
        let conn = self.conn.as_mut()?;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match conn.events.recv_timeout(wait) {
                Ok(Some(Event::Updated { view: v, kept })) if v == view => return Some(kept),
                Ok(ev @ (Some(Event::Closed) | None)) => {
                    conn.held.push_back(ev);
                    return None;
                }
                Ok(ev) => conn.held.push_back(ev),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => {
                    conn.held.push_back(None);
                    return None;
                }
            }
        }
    }

    pub fn recv(&mut self, deadline: Instant) -> Recv {
        let Some(conn) = &mut self.conn else {
            return Recv::Gone;
        };
        let wait = deadline.saturating_duration_since(Instant::now());
        let next = match conn.held.pop_front() {
            Some(ev) => Ok(ev),
            None => conn.events.recv_timeout(wait),
        };
        match next {
            Ok(Some(Event::Closed)) => {
                self.forget();
                Recv::Event(Event::Closed)
            }
            Ok(Some(ev)) => Recv::Event(ev),
            Err(RecvTimeoutError::Timeout) => Recv::Timeout,
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                self.forget();
                Recv::Gone
            }
        }
    }

    /// The viewer exited on its own, taking its pane with it.
    fn forget(&mut self) {
        self.conn = None;
        self.pane = None;
    }

    pub fn close(&mut self) {
        let _ = self
            .conn
            .as_mut()
            .map(|c| c.stream.write_all(b"{\"op\":\"close\"}\n"));
        self.conn = None;
        if let Some(pane) = self.pane.take() {
            // Give the viewer a moment to restore the terminal and release
            // its herdr status, then make sure the pane is gone.
            thread::sleep(Duration::from_millis(300));
            let _ = Command::new("herdr")
                .args(["pane", "close", &pane])
                .output();
        }
    }

    fn ensure(&mut self) -> anyhow::Result<()> {
        if self.conn.is_some() {
            return Ok(());
        }
        socket_fits(&self.socket).map_err(anyhow::Error::msg)?;
        // A viewer may already be listening: ours from earlier, or one the
        // user started by hand.
        if let Ok(stream) = UnixStream::connect(&self.socket) {
            self.conn = Some(Conn::new(stream)?);
            return Ok(());
        }
        self.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(stream) = UnixStream::connect(&self.socket) {
                self.conn = Some(Conn::new(stream)?);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!(
            "the viewer did not start listening on {}",
            self.socket.display()
        )
    }

    fn spawn(&mut self) -> anyhow::Result<()> {
        let inside = std::env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        let (true, Ok(origin)) = (inside, std::env::var("HERDR_PANE_ID")) else {
            bail!(
                "not inside herdr, so volvelle cannot open a pane. Ask the user to run `{} view --socket {}` in another terminal, then retry",
                viewer_bin(),
                self.socket.display()
            );
        };
        let focus = if std::env::var("VOLVELLE_FOCUS").is_ok_and(|v| v == "1") {
            "--focus"
        } else {
            "--no-focus"
        };
        let out = Command::new("herdr")
            .args(["pane", "split", &origin, "--direction", "right", focus])
            .output()
            .context("running herdr pane split")?;
        let reply: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
            format!("herdr pane split: {}", String::from_utf8_lossy(&out.stderr))
        })?;
        let Some(pane) = reply["result"]["pane"]["pane_id"].as_str() else {
            bail!("herdr pane split returned no pane: {reply}");
        };
        let mut cmd = format!(
            "exec {} view --socket {}",
            shell_quote(&viewer_bin()),
            shell_quote(&self.socket.to_string_lossy())
        );
        if let Some(project) = &self.project {
            cmd.push_str(&format!(
                " --project {}",
                shell_quote(&project.to_string_lossy())
            ));
        }
        let status = Command::new("herdr")
            .args(["pane", "run", pane, &cmd])
            .status()?;
        if !status.success() {
            bail!("herdr pane run failed in pane {pane}");
        }
        let _ = Command::new("herdr")
            .args(["pane", "rename", pane, "volvelle"])
            .output();
        self.pane = Some(pane.to_string());
        Ok(())
    }
}

impl Conn {
    fn new(stream: UnixStream) -> anyhow::Result<Self> {
        let reader = BufReader::new(stream.try_clone()?);
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Ok(ev) = serde_json::from_str::<Event>(&line)
                    && tx.send(Some(ev)).is_err()
                {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        Ok(Conn {
            stream,
            events,
            held: VecDeque::new(),
        })
    }
}

/// Size of `sun_path`, which holds a Unix socket's path and its NUL.
#[cfg(target_os = "linux")]
const SUN_PATH: usize = 108;
#[cfg(not(target_os = "linux"))]
const SUN_PATH: usize = 104;

/// A path too long for `sun_path` cannot be bound, and the viewer would
/// fail without saying why, so refuse it before opening a pane.
fn socket_fits(path: &Path) -> Result<(), String> {
    let len = path.as_os_str().len();
    if len < SUN_PATH {
        return Ok(());
    }
    Err(format!(
        "the socket path {} is {len} bytes, but Unix socket paths must be under {SUN_PATH} bytes \
         here; set VOLVELLE_SOCKET to a shorter path, such as /tmp/volvelle.sock",
        path.display()
    ))
}

/// The viewer binary: `VOLVELLE_BIN`, else next to this executable, else PATH.
fn viewer_bin() -> String {
    if let Ok(bin) = std::env::var("VOLVELLE_BIN") {
        return bin;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("volvelle")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "volvelle".into())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_socket_paths_are_refused_before_spawning() {
        let fits = PathBuf::from(format!("/{}", "s".repeat(SUN_PATH - 2)));
        assert!(socket_fits(&fits).is_ok());
        let long = PathBuf::from(format!("/{}", "s".repeat(SUN_PATH - 1)));
        let err = socket_fits(&long).unwrap_err();
        assert!(err.contains(&format!("is {SUN_PATH} bytes")), "{err}");
        assert!(err.contains("set VOLVELLE_SOCKET"), "{err}");
        let mut viewer = Viewer::at(long, None);
        let err = viewer.send(&Request::Close).unwrap_err().to_string();
        assert!(err.contains("set VOLVELLE_SOCKET"), "{err}");
        assert!(viewer.pane.is_none());
    }
}

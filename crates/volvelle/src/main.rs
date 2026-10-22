mod app;
mod draw;
mod link;
mod markdown;
mod tree;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event as TermEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use volvelle_schema::{Event, Request};

use app::App;
use link::{Herdr, Msg, Outbox};

/// Renders volvelle specs in the terminal.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve a viewer on a Unix socket. volvelle-mcp starts this in a herdr pane.
    View {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Render one spec file, print the user's answer as JSON, and exit.
    Show {
        spec: PathBuf,
        #[arg(long, default_value = "main")]
        view: String,
    },
    /// Print the component catalog's JSON Schema.
    Schema,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::View { socket } => view(socket),
        Cmd::Show { spec, view } => show(spec, view),
        Cmd::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&volvelle_schema::node_schema())?
            );
            Ok(())
        }
    }
}

fn view(socket: PathBuf) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    link::listen(&socket, tx).with_context(|| format!("binding {}", socket.display()))?;
    // Both callbacks deliver through the outbox.
    let out = RefCell::new(Outbox::default());
    let mut herdr = Herdr::from_env();
    let mut app = App::default();
    let mut terminal = ratatui::init();
    let result = run(
        &mut terminal,
        &mut app,
        |app| {
            let mut dirty = false;
            while let Ok(msg) = rx.try_recv() {
                dirty = true;
                match msg {
                    Msg::Request(Request::Render { view, spec }) => app.render(view, spec),
                    Msg::Request(Request::Close) => app.quit = true,
                    Msg::Connected(id, stream) => out.borrow_mut().connect(id, stream),
                    Msg::Disconnected(id) => out.borrow_mut().disconnect(id),
                }
            }
            herdr.report(app.awaiting_input(), &app.view);
            dirty
        },
        |ev| {
            out.borrow_mut().send(ev);
            false
        },
    );
    ratatui::restore();
    out.borrow_mut().send(Event::Closed);
    herdr.release();
    let _ = std::fs::remove_file(&socket);
    result
}

fn show(path: PathBuf, view: String) -> anyhow::Result<()> {
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let spec =
        volvelle_schema::parse_spec(serde_json::from_str(&raw)?).map_err(anyhow::Error::msg)?;
    let mut app = App::default();
    app.render(view, spec);
    let mut answer = None;
    let mut terminal = ratatui::init();
    let result = run(
        &mut terminal,
        &mut app,
        |_| false,
        |ev| {
            answer = Some(ev);
            true
        },
    );
    ratatui::restore();
    result?;
    let answer = answer.unwrap_or(Event::Closed);
    println!("{}", serde_json::to_string(&answer)?);
    Ok(())
}

/// The UI loop. `poll` drains outside messages and says whether to redraw;
/// `emit` receives each answer and says whether to quit.
fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut poll: impl FnMut(&mut App) -> bool,
    mut emit: impl FnMut(Event) -> bool,
) -> anyhow::Result<()> {
    let mut dirty = true;
    while !app.quit {
        dirty |= poll(app);
        dirty |= app.expire(Instant::now());
        if dirty {
            terminal.draw(|f| draw::draw(f, app))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        match event::read()? {
            TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(ev) = app.key(key) {
                    app.quit |= emit(ev);
                }
                dirty = true;
            }
            TermEvent::Resize(..) => dirty = true,
            _ => {}
        }
    }
    Ok(())
}

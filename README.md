# volvelle

A combinatorial terminal UI for agents. An agent composes an interface from a
fixed catalog of atoms; a ratatui viewer renders it in a herdr pane beside the
agent and returns the human's answer as typed JSON.

A volvelle is a paper wheel of rotating discs, used from Llull to Kircher to
generate new combinations from fixed tables. Volvelle does the same with
interfaces.

## Status

Phase 1 of the architecture: one agent, one view.

- `volvelle-schema`: the atom catalog as serde types, JSON Schema via
  schemars, validation, and the result schema each spec answers with.
- `volvelle`: the viewer. `volvelle view --socket PATH` serves specs over a
  Unix socket; `volvelle show spec.json` renders one file and prints the
  answer; `volvelle schema` prints the catalog schema.
- `volvelle-mcp`: stdio MCP server with `render`, `wait_event` and `close`.
  It splits a herdr pane for the viewer on first render.

Not yet built: patterns and rules, the event log, the shared-state server,
leases and CRDT regions (phases 2 to 4). See `docs/ROADMAP.md`.

## Install

```sh
cargo build --release
claude mcp add volvelle -- "$PWD/target/release/volvelle-mcp"
```

`volvelle-mcp` looks for the `volvelle` binary next to itself, then on `PATH`.
Override with `VOLVELLE_BIN`.

## Protocol

Each view follows one session type, checked at runtime by `volvelle-mcp`:

```text
View = !Render . (!Render)* . ?Event . end
```

Render a view, re-render it as often as needed (the user's input survives for
ids that still exist), then wait for its one answer. A call out of order is
refused with the moves allowed next, for example:

```text
view "deploy" was already answered with {...}; allowed next: render
```

`render` returns the JSON Schema of the answer, and `wait_event` returns
exactly that shape:

```json
{"type": "submit", "view": "deploy", "button": "go",
 "values": {"env": "prod", "migrate": true, "note": "fix login"}}
```

Other answers are `select` (Enter on a list with an id) and `cancel` (Esc).
A view with no fields or buttons is display-only and cannot be waited on.

## Atoms

`box`, `text`, `markdown`, `table`, `list`, `input`, `select`, `toggle`,
`button`, `progress`, `divider`. See `examples/deploy.json` and
`volvelle schema`. Atoms state intent: the viewer owns sizing, focus order and
colour.

## Keys

Tab and Shift-Tab move focus. Space toggles. Enter confirms or presses, and
inserts a newline in a multi-line input. PageUp and PageDown scroll a view
taller than the pane. Esc cancels. Ctrl-C closes the viewer.

## Environment

| Variable | Effect |
| --- | --- |
| `VOLVELLE_SOCKET` | Socket path. Defaults to one per herdr pane in `$TMPDIR`. |
| `VOLVELLE_BIN` | Viewer binary to launch. |
| `VOLVELLE_FOCUS=1` | Focus the viewer pane when it opens. Off by default. |
| `VOLVELLE_LOG_DIR` | Directory for session logs (JSONL, one file per server process). Defaults to `$XDG_STATE_HOME/volvelle/sessions`, else `~/.local/state/volvelle/sessions`. |

<p align="center">
  <img src="docs/images/volvelle.jpg" width="320" alt="A hand-coloured lunar volvelle from Peter Apian's Astronomicum Caesareum, 1540">
</p>

<h1 align="center">volvelle</h1>

A combinatorial terminal UI for agents. An agent composes an interface from a
fixed catalog of atoms; a ratatui viewer renders it in a herdr pane beside the
agent and returns the human's answer as typed JSON.

A volvelle is a paper wheel of rotating discs, used from Llull to Kircher to
generate new combinations from fixed tables. Volvelle does the same with
interfaces. The one above, from Peter Apian's *Astronomicum Caesareum* (1540),
works out the moon's position and phase. The scan is public domain, from
Houghton Library, Harvard University (Typ 520.40.150), via
[Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Houghton_Typ_520.40.150_-_Astronomicum_Caesareum,_1540_-_f2r.jpg).

![An RPG played with an agent through volvelle](docs/images/rpg.png)

*A tabletop RPG run by Claude Code. The run log is on the left; on the right,
volvelle shows the scene with signal and badge gauges, the last roll, and a
selectable list of actions whose answer goes back to the agent.*

## Status

Phase 1 of the architecture: one agent, one view.

- `volvelle-schema`: the atom catalog and patterns as serde types, JSON
  Schema via schemars, validation rules with path-level errors, and the
  result schema each spec answers with.
- `volvelle`: the viewer. `volvelle view --socket PATH [--project DIR]`
  serves specs over a Unix socket; `volvelle show spec.json` renders one
  file and prints the answer; `volvelle schema` prints the catalog schema.
- `volvelle-mcp`: stdio MCP server with `render`, `wait_event`, `close` and
  `library`. It splits a herdr pane for the viewer on first render.

Not yet built: replay from the event log, the shared-state server,
leases and CRDT regions (phases 2 to 4). See `docs/ROADMAP.md`, and
the [Linear project](https://linear.app/abstractmachines/project/volvelle-0395f8b06985)
for issues.

## Install

```sh
cargo build --release
claude mcp add volvelle -- "$PWD/target/release/volvelle-mcp"
```

`volvelle-mcp` looks for the `volvelle` binary next to itself, then on `PATH`.
Override with `VOLVELLE_BIN`.

While developing, `scripts/install-dev.sh` installs both binaries from the
checkout into `~/.cargo/bin` and points the MCP server at them. Run `/mcp` in
Claude Code afterwards to reconnect, since a running server keeps the old
binary.

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

Other answers are `select` (Enter on a list or tree with an id), `cancel` (Esc) and
`answer` (a pattern's typed value, see below). A view with no fields or
buttons is display-only and cannot be waited on.

A pattern extends its view's session type. `confirm-destructive@1` is

```text
!Spec . ?{confirm: !Outcome . end, cancel: end}
```

so after `{"confirmed": true}` the agent owes the user an outcome: the only
move allowed is to render the same view again with what happened.
`wait_event` says so in a `next` hint, and anything else is refused:

```text
view "delete" was confirmed; report the outcome by rendering view "delete"; allowed next: render view "delete"
```

Specs that break a rule are refused with the path, the rule and what would
be accepted instead:

```text
at /children/1: confirm-destructive@1 must be the only interactive element in its view, but the view also has "force" at /children/2; allowed: render the other fields in an earlier view, a toggle plus a button instead of the pattern
```

## Atoms

`box`, `text`, `markdown`, `table`, `list`, `tree`, `input`, `select`,
`toggle`, `button`, `progress`, `divider`. See `examples/deploy.json` and
`volvelle schema`. Atoms state intent: the viewer owns sizing, focus order and
colour. Ids may not contain `:`, which names a pattern's parts.

## Patterns

A pattern is a named, versioned composition with slots, a typed answer and
its own session type. It goes wherever an atom does, written as
`{"type": "pattern", "name": "<name>@<version>", ...slots}`. Slots are plain
data, so patterns never nest, and a view holds at most one. Each pattern is
declared once in `volvelle-schema`; the `render` tool description and the
session monitor are generated from that declaration.

| Pattern | Slots | Answer `value` |
| --- | --- | --- |
| `confirm-destructive@1` | `id`, `summary` (Markdown), `consequence`, `title?`, `confirm_label?`, `cancel_label?`, `require_typing?` | `{"confirmed": bool}` |
| `choose-with-preview@1` | `id`, `items` (`label`, `preview` in Markdown, `value?`, `detail?`), `title?`, `multi?` | `{"selected": value}`, or an array with `multi` |

`confirm-destructive@1` enforces the safe defaults: focus starts on Cancel,
Confirm is drawn in the error tone, and with `require_typing` it stays
disabled until the user types that text exactly. It must be the only
interactive element in its view. `choose-with-preview@1` shows the preview of
the item under the cursor without a round trip to the agent. See
`examples/confirm.json` and `examples/choose.json`.

A `tree` holds nested `nodes`, each with a `label`, optional `detail` and
`children`, and `open: false` to start folded. A node's value defaults to its
path of labels joined with `/`, such as `src/app/main.rs`, and values must be
unique. With an `id` the tree is selectable and Enter answers `select` with
the node's value; without one it is display-only. See `examples/tree.json`.

## Saved views

The human decides which views are worth keeping. Ctrl-S on any view,
answered or not, opens a prompt in the status line:

```text
 save as: sheet  [project|user] · tab scope · enter save · esc cancel
```

Tab switches the library, Enter saves, Esc closes the prompt, and keys typed
there never reach the view. Names are lowercase letters, digits and hyphens
(`[a-z0-9][a-z0-9-]{0,63}`). Saving over an existing name asks
`overwrite "sheet"? y/n` first. What is saved is the spec as the agent
rendered it, not what the user typed into its fields.

There are two libraries, one JSON file per view:

- Project: `.volvelle/library/<name>.json` in the directory `volvelle-mcp`
  runs in, which Claude Code sets to the project. It can be committed.
  The viewer offers it only when started with `--project`, which
  `volvelle-mcp` passes.
- User: `$VOLVELLE_USER_LIBRARY`, else `$XDG_DATA_HOME/volvelle/library`,
  else `~/.local/share/volvelle/library`.

A project entry shadows a user entry with the same name. Each file holds
`name`, `spec`, `saved_at_ms`, `origin` (`view`, and the viewer's herdr
`pane`) and a one-line `summary`, such as
`Character sheet · 3 inputs, 2 buttons, 1 tree`.

The agent sees saved views through the `library` tool: with no arguments it
lists `name`, `scope`, `summary`, `saved_at_ms` and `origin_view`; with
`name` it returns the whole entry, `spec` included, to adapt. `render` takes
exactly one of `spec` or `saved`:

```json
{"view": "sheet", "saved": "sheet"}
```

A saved spec is validated like any other, so one whose pattern version no
longer exists is refused with the usual violation and
`(from saved view "sheet")`. The session log records each save as a `saved`
record and each render from the library with `"saved": "<name>"`, so reuse
shows up there.

## Keys

Tab and Shift-Tab move focus. Space toggles. Enter confirms or presses, and
inserts a newline in a multi-line input. In a tree, Up and Down (or j and k)
move, Right (l) unfolds or steps into a node, Left (h) folds or steps out to
the parent, and Space toggles a fold. PageUp and PageDown scroll a view
taller than the pane; J and K (or Shift-Up and Shift-Down) scroll a long
preview. Esc cancels. Ctrl-S saves the view to a library. Ctrl-C closes the
viewer.

## Environment

| Variable | Effect |
| --- | --- |
| `VOLVELLE_SOCKET` | Socket path. Defaults to one per herdr pane in `$TMPDIR`. |
| `VOLVELLE_BIN` | Viewer binary to launch. |
| `VOLVELLE_FOCUS=1` | Focus the viewer pane when it opens. Off by default. |
| `VOLVELLE_USER_LIBRARY` | Directory of the user's saved views. Defaults to `$XDG_DATA_HOME/volvelle/library`, else `~/.local/share/volvelle/library`. |
| `VOLVELLE_LOG_DIR` | Directory for session logs (JSONL, one file per server process). Defaults to `$XDG_STATE_HOME/volvelle/sessions`, else `~/.local/state/volvelle/sessions`. |

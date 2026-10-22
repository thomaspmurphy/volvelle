# volvelle

A combinatorial terminal UI for agents. An agent composes an interface from a
fixed catalog of atoms; a ratatui viewer renders it in a herdr pane beside the
agent and returns the human's answer as typed JSON.

A volvelle is a paper wheel of rotating discs, used from Llull to Kircher to
generate new combinations from fixed tables. Volvelle does the same with
interfaces.

## Status

Phase 1 of the architecture: one agent, one view.

- `volvelle-schema`: the atom catalog and patterns as serde types, JSON
  Schema via schemars, validation rules with path-level errors, and the
  result schema each spec answers with.
- `volvelle`: the viewer. `volvelle view --socket PATH` serves specs over a
  Unix socket; `volvelle show spec.json` renders one file and prints the
  answer; `volvelle schema` prints the catalog schema.
- `volvelle-mcp`: stdio MCP server with `render`, `wait_event` and `close`.
  It splits a herdr pane for the viewer on first render.

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

## Keys

Tab and Shift-Tab move focus. Space toggles. Enter confirms or presses, and
inserts a newline in a multi-line input. In a tree, Up and Down (or j and k)
move, Right (l) unfolds or steps into a node, Left (h) folds or steps out to
the parent, and Space toggles a fold. PageUp and PageDown scroll a view
taller than the pane; J and K (or Shift-Up and Shift-Down) scroll a long
preview. Esc cancels. Ctrl-C closes the viewer.

## Environment

| Variable | Effect |
| --- | --- |
| `VOLVELLE_SOCKET` | Socket path. Defaults to one per herdr pane in `$TMPDIR`. |
| `VOLVELLE_BIN` | Viewer binary to launch. |
| `VOLVELLE_FOCUS=1` | Focus the viewer pane when it opens. Off by default. |
| `VOLVELLE_LOG_DIR` | Directory for session logs (JSONL, one file per server process). Defaults to `$XDG_STATE_HOME/volvelle/sessions`, else `~/.local/state/volvelle/sessions`. |

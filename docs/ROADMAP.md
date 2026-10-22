# Roadmap

Volvelle follows the four-phase architecture: one view, then patterns and
the log, then shared state, then concurrency and attention. Each phase is
usable on its own, and multi-agent features wait until the single-agent loop
is solid. Issues are tracked in the
[Linear project](https://linear.app/abstractmachines/project/volvelle-0395f8b06985),
with one milestone per phase.

## Done: phase 1, single view

- Atom catalog with generated JSON Schema, validation, and a derived result
  schema per spec (every UI is a typed function call).
- ratatui viewer over a Unix socket; the viewer owns sizing, focus and colour.
- stdio MCP server with `render`, `wait_event`, `close`, opening the viewer
  in a herdr pane.
- Runtime monitor for the view session type
  `!Render . (!Render)* . ?Event . end`, refusing out-of-order calls with the
  allowed next moves.
- herdr sidebar reporting: blocked while a view awaits an answer.

## Now

Use volvelle for real and record what agents build before naming patterns.
Patterns should be crystallised from evidence, not guessed.

### 1. Session log (done)

`volvelle-mcp` appends every render, answer, timeout, refusal and close to a
JSONL file, one per server process. This is the seed of the phase 2 event
log and the evidence base for crystallisation.

- Location: `$VOLVELLE_LOG_DIR`, else `~/.local/state/volvelle/sessions/`.
- One record per line with a sequence number, timestamp, kind and view, plus
  the spec, answer or error as relevant.
- Logging never fails a tool call.

### 2. Phase 1 gaps (done)

- Overflow: a spec taller than the pane scrolls so the focused field stays
  visible.
- Multi-line text input for edits such as commit messages.
- Feedback when the user types into a view that has already been answered.

### 3. Pattern layer

Agents compose mostly at the pattern level. A pattern is a named, versioned
composition with slots, a typed return value and its own session type.

- Specs accept `{"type": "pattern", "name": "confirm-destructive@1", ...}`
  wherever an atom is allowed.
- Nesting rules are checked in `validate`, with errors naming the path, the
  rule and the allowed alternatives.
- First patterns:
  - `confirm-destructive@1`: summary and consequence slots, returns
    `{confirmed: bool}`, focus starts on Cancel. The renderer enforces the
    safe defaults so the agent cannot get them wrong.
  - `choose-with-preview@1`: a list with a detail pane that follows the
    cursor without a round trip, which atoms alone cannot express.
- Each pattern declares its session type, extending the runtime monitor.
- Tool descriptions are generated from the same declarations, so the model
  sees the valid moves before it acts.

### 4. Saved views

The human decides what to keep: a view that proved useful is saved by name
and brought back later. This is crystallisation at its simplest, before
saved views gain slots and become patterns.

- Ctrl-S in the viewer asks for a name and saves the current spec.
- Two libraries: per project in `.volvelle/library/` (can be committed) and
  per user in `~/.local/share/volvelle/library/`. Project entries shadow user
  entries with the same name.
- A `library` tool lists saved views with a one-line summary each.
- `render` accepts `{"saved": "<name>"}` in place of a spec, and the agent can
  read a saved spec to adapt it.
- Saved views record their origin (session, view, time) so the log can show
  which ones get reused.

### 5. Data binding

Specs point at data instead of embedding it, so a saved view becomes a
template and an update is a small data change, not a whole new spec. This
pulls paradigm 3 forward from phase 3, without the multi-agent server.

- Atoms take `bind` JSON Pointers into a data document, e.g.
  `{"type": "progress", "label": "HP", "bind": "/hero/hp"}`; list and table
  rows can bind to arrays.
- `render` takes a template (inline or saved) plus a `data` document; a new
  `update` tool changes data at pointers, and the viewer redraws only that.
- Validation checks every pointer resolves and the value has the type the
  atom needs, with path-level errors like the pattern rules.
- The data document can be saved and loaded alongside views, which gives
  long-running sessions such as a tabletop RPG a save game: the views are
  the sheet, map and inventory, the data is the game state.
- Fields bound to data write the user's input back to the same pointer, so
  the answer and the state stay one thing.

### 6. Diagrams

Diagrams follow the same rule as every other atom: the agent describes
structure and the viewer draws it. The agent says "these states, these
transitions, currently here" and never places a box or an arrow. A freeform
canvas is ruled out, since it would give validation nothing to check.

- `tree` atom: nested nodes with `├──` / `└──` connectors, folding locally;
  with an `id`, Enter answers with the node's path. Independent of the
  pattern layer, so it can land any time.
- `graph` atom: nodes and edges, laid out in layers (rank, reduce crossings,
  route edges with box-drawing characters), sized for a few dozen nodes.
  This is the largest piece.
- `state-machine@1` pattern on the graph atom: start and end states,
  labelled transitions, and a highlighted `current` slot. First use: draw
  volvelle's own session types live, with the monitor's state lit up.
- Animation is data changing (item 5): updating `current` moves the
  highlight. A `trace` slot plays a sequence of states locally, with pause
  and scrub. Streaming many updates needs phase 4's frame batching.
- Full screen through herdr's native pane zoom: a viewer key toggles it,
  and a view can request it for maps, large tables and diagrams. It is a
  request, so the human can zoom back out, and the viewer undoes it when the
  view closes.

### 7. Terminal apps

Some answers are best given in a real app, such as `lazygit` or a log tail.
The UI never executes anything itself, so the agent can only name apps from
an allowlist the user owns (`.volvelle/apps.toml` per project, plus a
per-user file); anything else needs a confirmation view first.

- Beside the view first: the app opens in a herdr pane next to the viewer
  and gets a real terminal.
- Embedded later, if a real use needs the app inside the layout: a
  `terminal` atom drawing a pseudo-terminal's screen, with keys forwarded
  while it has focus.

## Later

- **Phase 2, rest:** replay from the log, crystallising reused UIs into named
  patterns, pattern versioning policy.
- **Phase 3, shared state:** `volvelle-server` as the single authority,
  typed actions, ownership tokens per region, proposals, subscriptions,
  provenance and per-actor undo. Data binding (item 5) moves behind the
  authority, and `update` becomes typed actions. Multiparty session types become worth it here, for proposals.
- **Phase 4, concurrency and attention:** leases (typestate in Rust, since
  "release exactly once" is a real bug class), backpressure, selective CRDT
  regions.

## Open questions

- Which CRDT crate: loro or yrs?
- Who approves proposals when no human is attached?
- How are crystallised patterns shared? Item 4 starts with per project and
  per user; is a public registry worth it?
- Does the log need compaction, and at what snapshot interval?
- Should a plain-text fallback render the same spec for logs and non-TTY
  output?
- Should volvelle-mcp move to rmcp once its API settles? The hand-rolled
  JSON-RPC is contained in `crates/volvelle-mcp/src/main.rs`.

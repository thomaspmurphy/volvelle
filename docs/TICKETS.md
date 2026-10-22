# Linear ticket plan

Source for the Linear project. One project, **volvelle**, with one milestone
per roadmap phase and one issue per roadmap item or shippable slice.
Each issue links back to its section in `docs/ROADMAP.md`.

## Milestone: Phase 1, single view (done)

- **Atom catalog, schema and validation**: done.
- **ratatui viewer over a Unix socket**: done.
- **stdio MCP server with herdr pane spawning**: done.
- **View session-type monitor**: done.
- **herdr sidebar reporting**: done.

## Milestone: Now

1. **Session log** (item 1): done, awaiting commit.
2. **Overflow scrolling keeps focus visible** (item 2): in progress.
3. **Multi-line text input** (item 2): in progress.
4. **Notice when typing into an answered view** (item 2): in progress.
5. **Pattern layer: schema, rules with structured violations** (item 3).
6. **confirm-destructive@1 with outcome obligation** (item 3). Blocked by 5.
7. **choose-with-preview@1** (item 3). Blocked by 5.
8. **Generate tool descriptions from pattern declarations** (item 3).
   Blocked by 5.
9. **Saved views: Ctrl-S, libraries, `library` tool, render by name**
   (item 4).
10. **Data binding: `bind` pointers, `data`, `update` tool** (item 5).
11. **Save games: persist data documents with views** (item 5).
    Blocked by 9 and 10.

## Milestone: Phase 2, patterns and log

- **Replay a session from its log.**
- **Crystallise reused views into patterns.**
- **Pattern versioning policy.**

## Milestone: Phase 3, shared state

- **volvelle-server as the single authority with typed actions.**
- **Ownership tokens per region and proposals.**
- **Subscriptions and provenance with per-actor undo.**
- **Multiparty session type for proposals.**

## Milestone: Phase 4, concurrency and attention

- **Leases with typestate.**
- **Backpressure and frame batching.**
- **Selective CRDT regions.**

## Open questions (one issue each, label `question`)

- CRDT crate: loro or yrs.
- Default approver when no human is attached.
- Public pattern registry.
- Log compaction and snapshot interval.
- Plain-text fallback renderer.
- Move volvelle-mcp to rmcp.

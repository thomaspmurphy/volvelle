#!/usr/bin/env bash
# Install volvelle and volvelle-mcp from this checkout into ~/.cargo/bin and
# point Claude Code's volvelle MCP server at the installed copy.
#
# A running MCP server keeps the old binary loaded, so reconnect it afterwards
# with /mcp in Claude Code.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${CARGO_HOME:-$HOME/.cargo}/bin"

cargo install --locked --force --path "$root/crates/volvelle"
cargo install --locked --force --path "$root/crates/volvelle-mcp"

if ! command -v claude >/dev/null; then
  echo "claude not found; register the server with: claude mcp add volvelle -- $bin/volvelle-mcp"
  exit 0
fi

# Re-register in the local scope so the server runs the installed binary
# rather than whatever is in target/.
cd "$root"
claude mcp remove volvelle -s local >/dev/null 2>&1 || true
claude mcp add volvelle -s local -- "$bin/volvelle-mcp"

echo "Installed $(git -C "$root" rev-parse --short HEAD). Run /mcp in Claude Code to reconnect volvelle."

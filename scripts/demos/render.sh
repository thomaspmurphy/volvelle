#!/bin/sh
# Render the demo recordings in scripts/demos/*.yml into docs/images/demo-*.gif.
# Needs terminalizer (npm install -g terminalizer); gifsicle, when installed,
# shrinks the result. Record first with scripts/demos/record.py.
set -eu
cd "$(dirname "$0")/../.."
mkdir -p docs/images
for yml in scripts/demos/*.yml; do
  name=$(basename "$yml" .yml)
  gif="docs/images/demo-$name.gif"
  terminalizer render "$yml" -o "$gif" >/dev/null
  if command -v gifsicle >/dev/null; then
    gifsicle -O3 --lossy=40 -b "$gif"
  fi
  echo "$gif: $(wc -c <"$gif" | tr -d ' ') bytes"
done

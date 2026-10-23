#!/usr/bin/env python3
"""Record the README demos as terminalizer recordings.

Drives target/release/volvelle-mcp over stdio the way an agent would, lets it
open a viewer pane through herdr, presses keys in that pane and reads it back
after each step. Each demo becomes scripts/demos/<name>.yml, which
scripts/demos/render.sh turns into docs/images/demo-<name>.gif.

Run from the repository root inside herdr, after `cargo build --release`:

    python3 scripts/demos/record.py [name ...]
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "scripts" / "demos"
SERVER = ROOT / "target" / "release" / "volvelle-mcp"
SOCKET = "/tmp/vv-demo.sock"

# Target width of the view, in cells. herdr splits the caller's pane in half,
# so the split is nudged until the viewer is about this wide; rows are cropped
# to the view afterwards. A pane reads PADDING cells narrower than its rect.
WIDTH = 72
PADDING = 3

KEY = 0.12  # a typed key
STEP = 0.35  # a deliberate move
HOLD = 1.2  # a meaningful state
FINAL = 2.5  # the last frame

CLEAR = "\x1b[2J\x1b[3J\x1b[H\x1b[?25l"

THEME = {
    "background": "#1e1e2e",
    "foreground": "#cdd6f4",
    "cursor": "#1e1e2e",
    "black": "#45475a",
    "red": "#f38ba8",
    "green": "#a6e3a1",
    "yellow": "#f9e2af",
    "blue": "#89b4fa",
    "magenta": "#cba6f7",
    "cyan": "#94e2d5",
    "white": "#bac2de",
    "brightBlack": "#6c7086",
    "brightRed": "#f38ba8",
    "brightGreen": "#a6e3a1",
    "brightYellow": "#f9e2af",
    "brightBlue": "#89b4fa",
    "brightMagenta": "#cba6f7",
    "brightCyan": "#94e2d5",
    "brightWhite": "#a6adc8",
}


def herdr(*args):
    return subprocess.run(
        ["herdr", *args], check=True, capture_output=True, text=True
    ).stdout


def workspace_panes():
    ws = os.environ["HERDR_WORKSPACE_ID"]
    panes = json.loads(herdr("pane", "list"))["result"]["panes"]
    return {p["pane_id"] for p in panes if p["workspace_id"] == ws}


def pane_width(pane):
    layout = json.loads(herdr("pane", "layout", "--pane", pane))["result"]["layout"]
    return next(p["rect"]["width"] for p in layout["panes"] if p["pane_id"] == pane)


class Demo:
    """One volvelle-mcp process, its viewer pane and the frames read from it."""

    def __init__(self, name):
        self.name = name
        self.tmp = tempfile.TemporaryDirectory(prefix="vv-demo-")
        env = dict(
            os.environ,
            VOLVELLE_USER_LIBRARY=f"{self.tmp.name}/library",
            VOLVELLE_LOG_DIR=f"{self.tmp.name}/log",
            VOLVELLE_SOCKET=SOCKET,
        )
        env.pop("VOLVELLE_FOCUS", None)
        self.proc = subprocess.Popen(
            [str(SERVER)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            env=env,
            text=True,
            cwd=self.tmp.name,
        )
        self.next_id = 0
        self.pane = None
        self.frames = []  # [frame, seconds shown]
        self.rpc("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "record.py", "version": "1"},
        })

    def rpc(self, method, params):
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        reply = json.loads(self.proc.stdout.readline())
        if "error" in reply:
            raise RuntimeError(f"{method}: {reply['error']}")
        return reply["result"]

    def call(self, tool, **args):
        result = self.rpc("tools/call", {"name": tool, "arguments": args})
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise RuntimeError(f"{tool}: {text}")
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return text

    def render(self, **args):
        before = workspace_panes() if self.pane is None else None
        reply = self.call("render", **args)
        if before is not None:
            (self.pane,) = workspace_panes() - before
            self.fit()
        self.settle()
        return reply

    def fit(self):
        """Move the split until the viewer is about WIDTH cells wide."""
        for _ in range(20):
            width = pane_width(self.pane) - PADDING
            if abs(width - WIDTH) <= 1:
                break
            side = "left" if width < WIDTH else "right"
            herdr("pane", "resize", "--pane", self.pane, "--direction", side,
                  "--amount", "0.01")
        time.sleep(0.4)

    def update(self, *patch):
        reply = self.call("update", view=self.view, patch=list(patch))
        self.settle()
        return reply

    @property
    def view(self):
        return self.name

    def settle(self):
        time.sleep(0.15)

    def snap(self, hold):
        frame = herdr("pane", "read", self.pane, "--source", "visible", "--ansi")
        rows = frame.replace("\r\n", "\n").split("\n")
        if self.frames and self.frames[-1][0] == rows:
            self.frames[-1][1] += hold
        else:
            self.frames.append([rows, hold])

    def keys(self, *keys, hold=KEY):
        for key in keys:
            herdr("pane", "send-keys", self.pane, key)
            self.settle()
            self.snap(hold)

    def type(self, text):
        self.keys(*["space" if c == " " else c for c in text])

    def close(self):
        if self.pane is not None:
            self.call("close")
            self.pane = None
        self.proc.stdin.close()
        self.proc.wait(timeout=5)
        self.tmp.cleanup()

    def write(self):
        """Crop every frame to the view and its status line, and save."""
        body = 0
        for rows, _ in self.frames:
            filled = [i for i, r in enumerate(rows[:-1]) if strip(r).strip()]
            body = max(body, filled[-1] + 1 if filled else 0)
        records = []
        delay = 0
        for rows, hold in self.frames:
            screen = rows[:body] + [""] + rows[-1:]
            content = CLEAR + "\r\n".join(r + "\x1b[0m" for r in screen)
            records.append({"delay": round(delay * 1000), "content": content})
            delay = hold
        # Repeat the last frame so its hold is shown before the loop restarts.
        records.append({"delay": round(delay * 1000), "content": records[-1]["content"]})
        cols = max(len(strip(r)) for rows, _ in self.frames for r in rows)
        config = {
            "command": None,
            "cwd": None,
            "env": {"recording": True},
            "cols": cols,
            "rows": body + 2,
            "repeat": 0,
            "quality": 100,
            "frameDelay": "auto",
            "maxIdleTime": "auto",
            "frameBox": {"type": None, "title": None, "style": {
                "border": "0px black solid", "boxShadow": "none", "margin": "0px",
                "padding": "12px", "background": THEME["background"]}},
            "watermark": {"imagePath": None},
            "cursorStyle": "block",
            "fontFamily": "JetBrains Mono, Menlo, monospace",
            "fontSize": 13,
            "lineHeight": 1,
            "letterSpacing": 0,
            "theme": THEME,
        }
        path = OUT / f"{self.name}.yml"
        with open(path, "w") as f:
            f.write("# Generated by scripts/demos/record.py; render with scripts/demos/render.sh.\n")
            f.write("config:\n")
            for line in yaml(config, 1):
                f.write(line + "\n")
            f.write("records:\n")
            for r in records:
                f.write(f"  - delay: {r['delay']}\n    content: {json.dumps(r['content'])}\n")
        total = sum(r["delay"] for r in records) / 1000
        print(f"{path.relative_to(ROOT)}: {len(records)} frames, {cols}x{body + 2}, {total:.1f}s")


def strip(s):
    """The text of an ANSI line, for measuring it."""
    out, i = [], 0
    while i < len(s):
        if s[i] == "\x1b":
            i += 1
            if i < len(s) and s[i] == "[":
                i += 1
                while i < len(s) and not ("@" <= s[i] <= "~"):
                    i += 1
            i += 1
        else:
            out.append(s[i])
            i += 1
    return "".join(out)


def yaml(value, depth):
    """Just enough YAML for the config block: JSON scalars are valid YAML."""
    pad = "  " * depth
    for k, v in value.items():
        if isinstance(v, dict):
            yield f"{pad}{k}:"
            yield from yaml(v, depth + 1)
        else:
            yield f"{pad}{k}: {'null' if v is None else json.dumps(v)}"


def example(name):
    return json.loads((ROOT / "examples" / name).read_text())


FORM = {
    "type": "box",
    "title": "New crate",
    "children": [
        {"type": "text", "text": "Set up a new crate in this workspace?", "tone": "accent"},
        {"type": "input", "id": "name", "label": "Name", "placeholder": "crate name"},
        {"type": "toggle", "id": "bin", "label": "Binary, not a library"},
        {"type": "toggle", "id": "ci", "label": "Add a CI workflow", "value": True},
        {"type": "box", "direction": "row", "children": [
            {"type": "button", "id": "create", "label": "Create", "tone": "accent"},
            {"type": "button", "id": "cancel", "label": "Cancel"},
        ]},
    ],
}


def form(d):
    d.render(view=d.view, spec=FORM)
    d.snap(HOLD)
    d.type("orrery")
    d.snap(0.6)
    d.keys("tab", hold=STEP)
    d.keys("space", hold=HOLD)
    d.keys("tab", "tab", hold=STEP)
    d.keys("enter", hold=0.6)
    answer = d.call("wait_event", view=d.view, timeout_ms=5000)
    assert answer["type"] == "submit", answer
    assert answer["values"] == {"name": "orrery", "bin": True, "ci": True}, answer
    d.snap(HOLD)
    d.keys("space", hold=FINAL)


def binding(d):
    d.render(view=d.view, spec=example("bound-sheet.json"),
             data=example("bound-sheet.data.json"))
    d.snap(HOLD)
    for hp in (0.6, 0.45, 0.3):
        d.update({"op": "replace", "path": "/hero/hp", "value": hp})
        d.snap(0.5)
    d.update({"op": "replace", "path": "/hero/poisoned", "value": True})
    d.snap(HOLD)
    d.update({"op": "add", "path": "/inventory/-",
              "value": {"label": "Antidote", "value": "antidote", "detail": "one dose"}})
    d.snap(HOLD)
    d.update({"op": "replace", "path": "/hero/poisoned", "value": False},
             {"op": "remove", "path": "/inventory/3"},
             {"op": "replace", "path": "/hero/hp", "value": 0.4})
    d.snap(FINAL)


def tree(d):
    d.render(view=d.view, spec=example("tree.json"))
    d.snap(HOLD)
    d.keys(*["down"] * 7, hold=STEP)
    d.snap(0.6)
    d.keys("right", hold=HOLD)
    d.keys("down", "down", hold=STEP)
    d.snap(0.8)
    d.keys("left", hold=STEP)
    d.keys("left", hold=HOLD)
    d.keys("left", hold=STEP)
    d.keys("left", hold=FINAL)


def graph(d):
    spec = example("graph.json")
    g = spec["children"][1]
    del g["current"]
    g["bind"] = "/state"
    d.render(view=d.view, spec=spec, data={"state": "awaiting"})
    d.snap(HOLD)
    for state in ("display", "answered", "closed"):
        d.update({"op": "replace", "path": "/state", "value": state})
        d.snap(FINAL if state == "closed" else HOLD)


DEMOS = {"form": form, "binding": binding, "tree": tree, "graph": graph}


def main():
    if os.environ.get("HERDR_ENV") != "1":
        sys.exit("record.py must run inside a herdr pane")
    if not SERVER.exists():
        sys.exit(f"{SERVER} is missing; run cargo build --release")
    for name in sys.argv[1:] or DEMOS:
        d = Demo(name)
        try:
            DEMOS[name](d)
        finally:
            d.close()
        d.write()


if __name__ == "__main__":
    main()

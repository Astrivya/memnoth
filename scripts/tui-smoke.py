"""Drive the memnoth TUI in a real pseudo-terminal and record what a user sees.

This is a smoke test, not a unit test: it starts the release binary under a
ConPTY (the same kind of terminal a user gets), then types a scripted session
and renders the byte stream through a terminal emulator, so every checkpoint is
the screen the person would be looking at — cursor moves, clears and style
runs resolved, not stripped.

Usage:
    python scripts/tui-smoke.py [--binary PATH] [--out DIR]
"""

from __future__ import annotations

import argparse
import os
import queue
import sys
import threading
import time
from pathlib import Path

try:
    from winpty import PtyProcess
except ImportError:  # pragma: no cover - the harness explains itself
    sys.exit(
        "tui-smoke needs pywinpty (pip install pywinpty) to provide a real "
        "console; it is a test tool, not a runtime dependency."
    )

try:
    import pyte
except ImportError:  # pragma: no cover
    sys.exit(
        "tui-smoke needs pyte (pip install pyte) to render the screen; it is a "
        "test tool, not a runtime dependency."
    )

READ_CHUNK = 200_000
ROWS = 40
COLS = 120


class Terminal:
    """A reader thread plus an emulated screen.

    A pty read blocks until the app draws again, so the reader runs on its own
    thread and every checkpoint drains what has arrived since the last one.
    """

    def __init__(self, pty: PtyProcess, rows: int, cols: int) -> None:
        self._pty = pty
        self.screen = pyte.Screen(cols, rows)
        # The binary writes UTF-8; ByteStream decodes it across chunk borders,
        # which matters for the box-drawing characters on every frame.
        self._stream = pyte.ByteStream(self.screen)
        self._chunks: queue.Queue[bytes | None] = queue.Queue()
        self._feed_lock = threading.Lock()
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self) -> None:
        while True:
            try:
                chunk = self._pty.read(READ_CHUNK)
            except Exception:
                break
            if not chunk:
                break
            self._chunks.put(chunk.encode("utf-8", "replace") if isinstance(chunk, str) else chunk)
        self._chunks.put(None)

    def settle(self, seconds: float = 1.0) -> str:
        """Feed what has been drawn, stopping once the stream goes quiet.

        The hard deadline is absolute: the app repaints on an idle tick, so
        "extend while work arrives" would never return.
        """
        deadline = time.time() + seconds
        while True:
            remaining = deadline - time.time()
            if remaining <= 0:
                break
            try:
                item = self._chunks.get(timeout=min(0.25, remaining))
            except queue.Empty:
                break  # quiet: this frame is complete
            if item is None:
                break
            with self._feed_lock:
                self._stream.feed(item)
        return self.render()

    def render(self) -> str:
        lines = [self.screen.display[y].rstrip() for y in range(self.screen.lines)]
        while lines and not lines[-1]:
            lines.pop()
        return "\n".join(lines)


def wait_until(
    terminal: "Terminal", needles: tuple[str, ...], timeout: float
) -> str:
    """Drain output until the screen shows every needle, or time out.

    Polling beats a fixed sleep in both directions: a frame that lands early is
    not waited for, and a slow burst is not sampled mid-draw.
    """
    end = time.time() + timeout
    screen = terminal.render()
    while time.time() < end:
        terminal.settle(0.6)
        screen = terminal.render()
        if all(needle.lower() in screen.lower() for needle in needles):
            return screen
    return screen


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    binary = args.binary or str(root / "target" / "release" / "mn.exe")
    if not Path(binary).exists():
        sys.exit(f"no binary at {binary}; run cargo build --release first")

    out_dir = Path(args.out) if args.out else Path.cwd() / "tui-smoke-out"
    out_dir.mkdir(parents=True, exist_ok=True)
    work = out_dir / "workspace"
    work.mkdir(exist_ok=True)
    home = out_dir / "home"
    home.mkdir(exist_ok=True)

    env = dict(os.environ)
    env["MEMNOTH_HOME"] = str(home)
    env["TERM"] = "xterm-256color"
    # Every provider key is removed: a turn must take the offline path, so the
    # transcript, overlays and the permission prompt are what is under test and
    # the run never depends on a network or a live credential.
    for key in (
        "MEMNOTH_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "KILO_API_KEY",
        "KILOCODE_API_KEY",
        "GROQ_API_KEY",
        "MEMNOTH_MODEL",
    ):
        env.pop(key, None)

    pty = PtyProcess.spawn(
        [binary, "--root", str(work)],
        cwd=str(work),
        env=env,
        dimensions=(ROWS, COLS),
    )
    terminal = Terminal(pty, ROWS, COLS)

    # The first frame is the readiness gate: typing before it exists means keys
    # land in a buffer nobody has read yet. ConPTY releases output in bursts, so
    # the gate polls the emulated screen instead of trusting arrival timing.
    started = time.time()
    first = wait_until(terminal, ("memnoth",), 20.0)
    startup_s = time.time() - started
    print(f"first frame after {startup_s:.1f}s")

    # Every step is (name, keys, what the screen must contain afterwards).
    steps: list[tuple[str, str, tuple[str, ...]]] = [
        # A plain question with no provider key: the two local stores answer.
        ("typed-ask", "hello there\r", ("hello there", "no model key")),
        # A catastrophic rule denies outright; a destructive one asks.
        ("denied", "!sudo echo nope\r", ("run this command?",)),
        ("declined", "n", ("declined", "was not run")),
        ("ask-typed", "!curl --version\r", ("run this command?",)),
        ("approved", "y", ("approved", "curl")),
        ("help", "/help\r", ("Global", "Overlays", "Commands", "Shortcuts")),
        ("help-tab", "\t", ("Search Overlay", "Diff Overlay")),
        ("help-closed", "\x1b", ("enter send",)),
        ("context", "/context\r", ("Beliefs", "History")),
        ("context-closed", "\x1b", ("enter send",)),
        (
            "model",
            "/model\r",
            ("Models", "All", "Connected", "Favorites", "Reasoning"),
        ),
        # `/` filters the rows rather than reaching the prompt.
        ("model-search", "/opus", ("search: opus", "Opus")),
        # Esc clears the search; `q` closes. Two Esc bytes in one write arrive
        # as a single alt-esc, so the close is its own step.
        ("model-search-cleared", "\x1b", ("search: /",)),
        ("model-closed", "q", ("enter send",)),
        # The key is the connection: no model or URL field on this screen.
        (
            "connect",
            "/connect\r",
            ("Connect", "providers", "status", "last test"),
        ),
        # `k` opens a real field, with the caret in it and no glyph pretending
        # to be one.
        ("connect-key-open", "k", ("key for", "paste the key", "esc cancels")),
        # A pasted key lands in that field, not in the prompt behind it.
        (
            "connect-key",
            "\x1b[200~sk-test-not-stored\x1b[201~",
            ("key for", "chars"),
        ),
        ("connect-key-cancelled", "\x1b", ("providers", "esc close")),
        ("connect-closed", "q", ("enter send",)),
        ("sessions", "/sessions\r", ("session",)),
        ("palette", "\x0b", ("Command Palette",)),
        ("palette-filter", "recall", ("/vault recall",)),
        ("palette-insert", "\r", ("/vault recall",)),
        ("palette-closed", "\x1b", ("enter send",)),
        ("quit", ":q\r", ()),
    ]

    transcript: list[str] = [f"\n===== first-frame =====\n{first}"]
    failures: list[str] = []
    for name, keys, must in steps:
        if keys:
            try:
                pty.write(keys)
            except EOFError:
                transcript.append(f"\n===== {name} ===== (PTY CLOSED)\n")
                failures.append(f"{name}: the process exited when keys were sent")
                break
            screen = wait_until(terminal, must, 8.0)
        else:
            screen = terminal.settle(1.0)
        state = "alive" if pty.isalive() else "EXITED"
        transcript.append(f"\n===== {name} ===== ({state})\n{screen}")
        for needle in must:
            if needle.lower() not in screen.lower():
                failures.append(f"{name}: screen does not show {needle!r}")

    if pty.isalive():
        pty.terminate(force=True)

    (out_dir / "screen.log").write_text("".join(transcript), encoding="utf-8", errors="replace")
    print(f"wrote {out_dir / 'screen.log'}")
    if failures:
        print("FAILURES:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("all checkpoints ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

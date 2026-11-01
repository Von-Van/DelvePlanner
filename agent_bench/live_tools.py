"""A ``ToolClient`` backed by the real Rust read tools, through the ``tool_server`` example.

It serves a case's fixture export to ``src-tauri/examples/tool_server.rs`` over newline-delimited JSON
and returns the server's ``toolResult`` envelopes unchanged. The benchmark uses it to check that the mock
agrees with the real tools (``tests/test_parity.py``); it carries no fault injection or state changes.

Build the server first: ``cargo build --example tool_server`` in ``src-tauri/``.
"""

from __future__ import annotations

import itertools
import json
import os
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .mock_tools import load_fixture

REPO = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO / "src-tauri" / "target" / "debug" / "examples" / "tool_server"


def find_tool_server() -> Path | None:
    path = Path(os.environ.get("AGENT_BENCH_TOOL_SERVER", DEFAULT_BINARY))
    for candidate in (path, path.with_suffix(".exe")):
        if candidate.is_file():
            return candidate
    return None


class LiveToolClient:
    def __init__(self, case: dict, binary: Path | None = None, budget: dict | None = None):
        binary = binary or find_tool_server()
        if binary is None:
            raise FileNotFoundError("tool_server is not built; run `cargo build --example tool_server` in src-tauri/")
        fixture = load_fixture(case["fixture"]["version"])
        bundle = json.loads(json.dumps(fixture["bundle"]))
        for change in case["fixture"].get("overrides", []):
            rows = bundle.setdefault(change["entity"], [])
            if "add" in change:
                rows.append(change["add"])
            else:
                next(r for r in rows if r["id"] == change["id"]).update(change["fields"])
        self._dir = tempfile.TemporaryDirectory(prefix="agent_bench_")
        bundle_path = Path(self._dir.name) / "bundle.json"
        bundle_path.write_text(json.dumps(bundle))
        command = [str(binary), "--bundle", str(bundle_path)]
        if case.get("planningProfile"):
            profile_path = Path(self._dir.name) / "profile.json"
            profile_path.write_text(json.dumps(case["planningProfile"]))
            command += ["--profile", str(profile_path)]
        self._proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self._ids = (f"req-{n}" for n in itertools.count(1))
        self.hello = self._send("hello", {})
        now = datetime.fromisoformat(case["now"]).astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
        start = {"now": now, "timeZone": case["timeZone"], "permittedTools": list(case["permittedTools"]), "privacy": {"includeDescriptions": bool(case.get("privacy", {}).get("includeDescriptions"))}}
        if budget:
            start["budget"] = budget
        ack = self._send("run_start", start)
        if ack["type"] != "run_start_ack":
            raise RuntimeError(f"run_start refused: {ack}")

    def _send(self, kind: str, payload: Any) -> dict:
        assert self._proc.stdin and self._proc.stdout
        message = {"v": 1, "id": next(self._ids), "type": kind, "payload": payload}
        self._proc.stdin.write(json.dumps(message) + "\n")
        self._proc.stdin.flush()
        line = self._proc.stdout.readline()
        if not line:
            raise RuntimeError("tool_server exited")
        return json.loads(line)

    def list_tools(self) -> list[dict]:
        return self.hello.get("payload", {}).get("tools", [])

    def call(self, name: str, arguments: dict | None = None) -> dict:
        reply = self._send("tool_call", {"tool": name, "arguments": {} if arguments is None else arguments})
        if reply["type"] != "tool_result":
            raise RuntimeError(f"tool_call failed: {reply}")
        return reply["payload"]

    def close(self) -> None:
        if self._proc.poll() is None:
            self._proc.stdin.close()  # type: ignore[union-attr]
            self._proc.wait(timeout=10)
        self._dir.cleanup()

    def __enter__(self) -> "LiveToolClient":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

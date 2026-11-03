from __future__ import annotations

import json
import sys
import threading
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from delveagent.model import ModelError, ModelRequest, ModelResponse  # noqa: E402


@dataclass
class ScriptedModel:
    """A stand-in model that replies with a fixed script of steps (dicts become JSON)."""

    script: Sequence[Any]
    requests: list[ModelRequest] = field(default_factory=list)
    on_call: Callable[[], None] | None = None

    def describe(self) -> dict[str, Any]:
        return {"provider": "scripted", "model": "scripted", "digest": "sha256:test", "options": {}}

    def complete(self, request: ModelRequest) -> ModelResponse:
        self.requests.append(request)
        if self.on_call:
            self.on_call()
        index = len(self.requests) - 1
        if index >= len(self.script):
            raise AssertionError("model called more times than scripted")
        step = self.script[index]
        if isinstance(step, Exception):
            raise step
        content = step if isinstance(step, str) else json.dumps(step)
        return ModelResponse(
            content=content,
            model="scripted",
            digest="sha256:test",
            options={},
            latency_ms=5,
            prompt_tokens=100,
            completion_tokens=20,
        )


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


@pytest.fixture
def clock() -> FakeClock:
    return FakeClock()


class FakeOllama(BaseHTTPRequestHandler):
    chat_bodies: list = []
    redirect = False

    def log_message(self, *args):  # silence test output
        pass

    def _send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/api/version":
            self._send(200, {"version": "0.12.6"})
        elif self.path == "/api/tags":
            self._send(200, {"models": [{"name": "qwen3:8b", "model": "qwen3:8b", "digest": "sha256:abc123"}]})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length))
        if FakeOllama.redirect:
            self.send_response(307)
            self.send_header("Location", "http://203.0.113.9:11434/api/chat")
            self.end_headers()
            return
        if body.get("model") == "missing":
            self._send(404, {"error": "model 'missing' not found"})
            return
        FakeOllama.chat_bodies.append(body)
        self._send(
            200,
            {
                "model": body["model"],
                "message": {"role": "assistant", "content": '{"action": "answer", "answer": "hi"}'},
                "prompt_eval_count": 42,
                "eval_count": 7,
                "done": True,
            },
        )


@pytest.fixture
def ollama():
    FakeOllama.chat_bodies = []
    FakeOllama.redirect = False
    server = ThreadingHTTPServer(("127.0.0.1", 0), FakeOllama)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{server.server_address[1]}"
    server.shutdown()
    server.server_close()


__all__ = ["ScriptedModel", "FakeClock", "ModelError", "FakeOllama"]

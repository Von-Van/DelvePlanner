"""The agent's end of the link: answers the handshake, runs one task at a time, and gives the
running agent a tool client, a model client, a trace hook and a cancel flag that all go through
DelvePlanner. The agent never touches planner data, credentials or the network directly.
"""

from __future__ import annotations

import itertools
import platform
import sys
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, BinaryIO, Callable

from . import envelope as wire
from .envelope import Envelope, ProtocolError

# Extra time the agent waits for an answer beyond the app's own timeout for that call, so the
# app's timeout error, not a silent local one, is what normally ends a slow call.
REPLY_MARGIN_SECONDS = 5.0
MAX_ERROR_MESSAGE_CHARS = 500


class Cancelled(Exception):
    """The run was cancelled or ran out of time. Let it propagate; the link reports it."""


class LinkCallError(Exception):
    """A tool or model call came back with an error from the shared taxonomy."""

    def __init__(self, code: str, message: str, retryable: bool = False) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.retryable = retryable

    @classmethod
    def from_payload(cls, error: Any) -> "LinkCallError":
        if not isinstance(error, dict):
            return cls("internal", "the app sent an unreadable error")
        return cls(
            str(error.get("code", "internal")),
            str(error.get("message", "")),
            bool(error.get("retryable", False)),
        )

    def to_payload(self) -> dict[str, Any]:
        return {"code": self.code, "message": self.message, "retryable": self.retryable}


class ToolError(LinkCallError):
    call_id: str | None = None


class ModelError(LinkCallError):
    pass


class BudgetExhausted(LinkCallError):
    def __init__(self, message: str) -> None:
        super().__init__("budget_exhausted", message, False)


Runner = Callable[[dict[str, Any], "RunContext"], dict[str, Any]]


@dataclass
class _Pending:
    done: threading.Event = field(default_factory=threading.Event)
    payload: dict[str, Any] | None = None
    cancelled: bool = False


@dataclass
class Usage:
    model_calls: int = 0
    tool_calls: int = 0
    prompt_tokens: int = 0
    completion_tokens: int = 0
    started: float = field(default_factory=time.monotonic)

    def to_json(self) -> dict[str, Any]:
        return {
            "modelCalls": self.model_calls,
            "toolCalls": self.tool_calls,
            "promptTokens": self.prompt_tokens,
            "completionTokens": self.completion_tokens,
            "latencyMs": int((time.monotonic() - self.started) * 1000),
        }


class RunContext:
    """Everything a runner may use during one run."""

    def __init__(self, link: "AgentLink", run_id: str, task: dict[str, Any], budget: dict[str, Any]):
        self._link = link
        self.run_id = run_id
        self.task = task
        self.budget = budget
        self.cancelled = threading.Event()
        self.stop_reason: str | None = None
        self.usage = Usage()
        self._seq = itertools.count(1)
        self.tools = LinkToolClient(self)
        self.model = LinkModelClient(self)

    def check_cancelled(self) -> None:
        if self.cancelled.is_set():
            raise Cancelled(self.stop_reason or "cancelled")

    def check_wall_clock(self) -> None:
        limit = self.budget.get("maxWallSeconds")
        if isinstance(limit, (int, float)) and time.monotonic() - self.usage.started > limit:
            raise BudgetExhausted("this run has used all of its time")

    def trace(
        self,
        kind: str,
        summary: str,
        data: dict[str, Any] | None = None,
        duration_ms: int | None = None,
    ) -> None:
        """Emits one trace event in the shared shape (contract §5)."""
        event: dict[str, Any] = {
            "runId": self.run_id,
            "seq": next(self._seq),
            "kind": kind,
            "at": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
            "summary": summary,
            "data": data or {},
        }
        if duration_ms is not None:
            event["durationMs"] = duration_ms
        self._link.send(Envelope(self._link.next_id(), wire.TRACE_EVENT, event))


class LinkToolClient:
    """The live tool client: each call is answered by the Rust tool broker.

    The payloads match ``contracts/tools.v1.json`` and the stand-alone tool server:
    ``tool_call`` is ``{tool, arguments}`` and ``tool_result`` is ``{callId, tool, ok,
    result | error, durationMs, ...}``. ``callId`` is what drafts cite as evidence.
    """

    def __init__(self, context: RunContext) -> None:
        self._context = context
        self.last_call_id: str | None = None

    def call(self, tool: str, arguments: dict[str, Any] | None = None) -> Any:
        """Returns the tool's result, or raises ``ToolError`` (with ``call_id`` set)."""
        reply = self.call_detailed(tool, arguments)
        if reply.get("ok") is True:
            return reply.get("result")
        error = ToolError.from_payload(reply.get("error"))
        error.call_id = reply.get("callId")
        raise error

    def call_detailed(self, tool: str, arguments: dict[str, Any] | None = None) -> dict[str, Any]:
        """Returns the whole ``tool_result`` payload, errors included."""
        context = self._context
        context.check_cancelled()
        context.check_wall_clock()
        limit = context.budget.get("maxToolCalls")
        if isinstance(limit, int) and context.usage.tool_calls >= limit:
            raise BudgetExhausted("this run has used all of its tool calls")
        context.usage.tool_calls += 1
        payload = {"tool": tool, "arguments": arguments or {}}
        reply = context._link.request(context, wire.TOOL_CALL, payload, "toolTimeoutMs")
        self.last_call_id = reply.get("callId")
        return reply


class LinkModelClient:
    """The live model client: each call goes to the Rust model broker and the bundled Ollama."""

    def __init__(self, context: RunContext) -> None:
        self._context = context

    def chat(
        self,
        messages: list[dict[str, Any]],
        *,
        format: str | dict[str, Any] | None = None,
        options: dict[str, Any] | None = None,
        tools: list[dict[str, Any]] | None = None,
        think: bool | None = None,
    ) -> dict[str, Any]:
        """Returns the broker's result: ``message``, ``model``, ``digest``, ``ollamaVersion``,
        ``usage``, ``timings``, ``options``, ``doneReason``."""
        context = self._context
        context.check_cancelled()
        context.check_wall_clock()
        limit = context.budget.get("maxModelCalls")
        if isinstance(limit, int) and context.usage.model_calls >= limit:
            raise BudgetExhausted("this run has used all of its model calls")
        context.usage.model_calls += 1
        payload: dict[str, Any] = {"runId": context.run_id, "messages": messages}
        for key, value in (("format", format), ("options", options), ("tools", tools), ("think", think)):
            if value is not None:
                payload[key] = value
        reply = context._link.request(context, wire.MODEL_CALL, payload, "modelTimeoutMs")
        if reply.get("ok") is not True:
            raise ModelError.from_payload(reply.get("error")) from None
        result = reply.get("result") or {}
        usage = result.get("usage") or {}
        context.usage.prompt_tokens += int(usage.get("promptTokens") or 0)
        context.usage.completion_tokens += int(usage.get("completionTokens") or 0)
        return result


def runtime_info() -> dict[str, Any]:
    return {
        "python": platform.python_version(),
        "implementation": platform.python_implementation(),
        "platform": sys.platform,
        "machine": platform.machine(),
        "frozen": bool(getattr(sys, "frozen", False)),
    }


class AgentLink:
    """Serves the protocol on a pair of binary streams until the app closes stdin."""

    def __init__(
        self,
        runner: Runner,
        reader: BinaryIO,
        writer: BinaryIO,
        *,
        runner_name: str = "custom",
        agent_version: str = "0.1.0",
    ) -> None:
        self.runner = runner
        self.runner_name = runner_name
        self.agent_version = agent_version
        self._reader = reader
        self._writer = writer
        self._write_lock = threading.Lock()
        self._state_lock = threading.Lock()
        self._ids = itertools.count(1)
        self._pending: dict[str, _Pending] = {}
        self._active: RunContext | None = None
        self._worker: threading.Thread | None = None
        self.limits: dict[str, Any] = {}

    # Sending -----------------------------------------------------------------------------------

    def next_id(self) -> str:
        return f"a-{next(self._ids)}"

    def send(self, envelope: Envelope) -> None:
        data = wire.encode(envelope)
        self.write_raw(data)

    def write_raw(self, data: bytes) -> None:
        with self._write_lock:
            self._writer.write(data)
            self._writer.flush()

    def request(
        self, context: RunContext, kind: str, payload: dict[str, Any], timeout_key: str
    ) -> dict[str, Any]:
        """Sends a call and waits for its answer, a cancel, or a timeout."""
        request_id = self.next_id()
        pending = _Pending()
        with self._state_lock:
            self._pending[request_id] = pending
        try:
            self.send(Envelope(request_id, kind, payload))
            timeout = float(self.limits.get(timeout_key, 60_000)) / 1000 + REPLY_MARGIN_SECONDS
            if not pending.done.wait(timeout):
                raise LinkCallError("timeout", "the app did not answer in time", True)
        finally:
            with self._state_lock:
                self._pending.pop(request_id, None)
        if pending.cancelled:
            raise Cancelled(context.stop_reason or "cancelled")
        return pending.payload or {}

    # Serving -----------------------------------------------------------------------------------

    def serve(self) -> int:
        """Returns the process exit code: 0 when the app closed the link, 2 on a protocol error."""
        try:
            if not self._handshake():
                return 0
            while True:
                line = wire.read_line(self._reader)
                if line is None:
                    self._cancel_active("shutdown")
                    return 0
                if not line.strip():
                    continue
                self._dispatch(wire.decode(line, allowed=wire.SENT_BY_HOST))
        except ProtocolError as error:
            self._report(None, "protocol_error", str(error))
            self._cancel_active("protocol_error")
            return 2

    def _handshake(self) -> bool:
        line = wire.read_line(self._reader)
        if line is None:
            return False
        hello = wire.decode(line, allowed=wire.SENT_BY_HOST)
        if hello.type != wire.HELLO:
            raise ProtocolError("the first message must be hello")
        versions = hello.payload.get("protocolVersions")
        if not isinstance(versions, list) or wire.PROTOCOL_VERSION not in versions:
            raise ProtocolError(f"the app does not offer protocol version {wire.PROTOCOL_VERSION}")
        limits = hello.payload.get("limits")
        self.limits = limits if isinstance(limits, dict) else {}
        self.send(
            Envelope(
                hello.id,
                wire.HELLO_ACK,
                {
                    "protocolVersion": wire.PROTOCOL_VERSION,
                    "maxMessageBytes": wire.MAX_MESSAGE_BYTES,
                    "agent": {
                        "name": "delveops-agent",
                        "version": self.agent_version,
                        "runner": self.runner_name,
                    },
                    "runtime": runtime_info(),
                },
            )
        )
        return True

    def _dispatch(self, message: Envelope) -> None:
        if message.type == wire.HELLO:
            raise ProtocolError("a second hello")
        if message.type == wire.RUN_START:
            self._start_run(message)
        elif message.type in (wire.TOOL_RESULT, wire.MODEL_RESULT):
            with self._state_lock:
                pending = self._pending.get(message.id)
            # An answer to a call that already timed out here is dropped.
            if pending is not None:
                pending.payload = message.payload
                pending.done.set()
        elif message.type == wire.CANCEL:
            active = self._active
            if active is not None and message.payload.get("runId") == active.run_id:
                self._cancel_active(str(message.payload.get("reason") or "cancelled"))
        elif message.type == wire.ERROR:
            print(f"delveops-agent: the app reported {message.payload!r}", file=sys.stderr)

    def _start_run(self, message: Envelope) -> None:
        with self._state_lock:
            busy = self._active is not None
        if busy:
            self._report(message.id, "busy", "the agent is already working on a run")
            return
        task = message.payload.get("task")
        budget = message.payload.get("budget")
        if not isinstance(task, dict):
            self._report(message.id, "invalid_arguments", "run_start needs a task object")
            return
        context = RunContext(self, message.id, task, budget if isinstance(budget, dict) else {})
        with self._state_lock:
            self._active = context
        self.send(
            Envelope(
                message.id,
                wire.RUN_START_ACK,
                {"runId": message.id, "runner": self.runner_name,
                 "architecture": task.get("architecture")},
            )
        )
        self._worker = threading.Thread(
            target=self._run, args=(context,), name=f"run {message.id}", daemon=True
        )
        self._worker.start()

    def _cancel_active(self, reason: str) -> None:
        with self._state_lock:
            context = self._active
            pending = list(self._pending.values())
        if context is None:
            return
        context.stop_reason = reason
        context.cancelled.set()
        for call in pending:
            call.cancelled = True
            call.done.set()

    def _run(self, context: RunContext) -> None:
        try:
            result = self.runner(context.task, context)
            if not isinstance(result, dict) or result.get("status") not in wire.RUN_STATUSES:
                raise TypeError("the runner must return a dict with a known status")
        except Cancelled:
            result = {"status": "cannot_complete", "answer": None}
        except LinkCallError as error:
            result = {"status": "cannot_complete", "answer": None, "stopReason": "error",
                      "error": error.to_payload()}
        except Exception as error:  # noqa: BLE001 - any agent bug becomes a normal result
            message = f"{type(error).__name__}: {error}"[:MAX_ERROR_MESSAGE_CHARS]
            result = {"status": "cannot_complete", "answer": None, "stopReason": "error",
                      "error": {"code": "internal", "message": message, "retryable": False}}
        result = dict(result)
        if context.cancelled.is_set():
            result.setdefault("stopReason", context.stop_reason or "cancelled")
        result.setdefault("drafts", [])
        result["usage"] = context.usage.to_json()
        with self._state_lock:
            self._active = None
        try:
            self.send(Envelope(context.run_id, wire.RUN_RESULT, result))
        except (ProtocolError, TypeError, ValueError) as error:
            self.send(
                Envelope(
                    context.run_id,
                    wire.RUN_RESULT,
                    {
                        "status": "cannot_complete",
                        "answer": None,
                        "drafts": [],
                        "stopReason": "error",
                        "error": {"code": "internal", "message": f"result not sendable: {error}"[:MAX_ERROR_MESSAGE_CHARS],
                                  "retryable": False},
                        "usage": context.usage.to_json(),
                    },
                )
            )
        except OSError:
            pass  # The app went away; the reader will see the end of stdin.

    def _report(self, about: str | None, code: str, message: str) -> None:
        payload: dict[str, Any] = {"code": code, "message": message[:MAX_ERROR_MESSAGE_CHARS], "retryable": False}
        if about is not None:
            payload["runId"] = about
        try:
            self.send(Envelope(self.next_id(), wire.ERROR, payload))
        except (OSError, ProtocolError):
            pass

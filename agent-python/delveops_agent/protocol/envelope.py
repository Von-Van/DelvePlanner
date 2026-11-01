"""The wire format between DelvePlanner and this agent.

One JSON object per line on stdin/stdout, each wrapped in the same envelope::

    {"v": 1, "id": "req-123", "type": "tool_call", "payload": {...}}

The Rust twin is ``src-tauri/src/sidecar/protocol.rs``; the constants here must match it.
"""

from __future__ import annotations

import json
import string
from dataclasses import dataclass, field
from typing import Any

PROTOCOL_VERSION = 1
# The largest message either side may send: the JSON bytes, not counting the newline.
MAX_MESSAGE_BYTES = 1024 * 1024
MAX_ID_CHARS = 128

# Every message type in protocol v1.
HELLO = "hello"
HELLO_ACK = "hello_ack"
RUN_START = "run_start"
RUN_START_ACK = "run_start_ack"
TOOL_CALL = "tool_call"
TOOL_RESULT = "tool_result"
MODEL_CALL = "model_call"
MODEL_RESULT = "model_result"
TRACE_EVENT = "trace_event"
RUN_RESULT = "run_result"
CANCEL = "cancel"
ERROR = "error"

SENT_BY_HOST = frozenset({HELLO, RUN_START, TOOL_RESULT, MODEL_RESULT, CANCEL, ERROR})
SENT_BY_AGENT = frozenset({HELLO_ACK, RUN_START_ACK, TOOL_CALL, MODEL_CALL, TRACE_EVENT, RUN_RESULT, ERROR})
MESSAGE_TYPES = SENT_BY_HOST | SENT_BY_AGENT

# The shared tool error taxonomy, then the codes that belong to the link itself.
TOOL_ERROR_CODES = frozenset(
    {
        "invalid_arguments",
        "not_found",
        "ambiguous",
        "permission_denied",
        "timeout",
        "unavailable",
        "budget_exhausted",
        "internal",
    }
)
LINK_ERROR_CODES = frozenset({"protocol_error", "cancelled", "busy"})
ERROR_CODES = TOOL_ERROR_CODES | LINK_ERROR_CODES

RUN_STATUSES = frozenset({"answered", "drafted", "needs_clarification", "cannot_complete"})

_ID_CHARS = frozenset(string.printable) - frozenset(string.whitespace)


class ProtocolError(Exception):
    """A message that breaks the protocol. The link can't recover from one."""


@dataclass(frozen=True)
class Envelope:
    id: str
    type: str
    payload: dict[str, Any] = field(default_factory=dict)
    v: int = PROTOCOL_VERSION

    def to_json(self) -> dict[str, Any]:
        return {"v": self.v, "id": self.id, "type": self.type, "payload": self.payload}


def _check(envelope: Envelope) -> None:
    if envelope.v != PROTOCOL_VERSION:
        raise ProtocolError(f"protocol version {envelope.v!r}, expected {PROTOCOL_VERSION}")
    if (
        not isinstance(envelope.id, str)
        or not 0 < len(envelope.id) <= MAX_ID_CHARS
        or not set(envelope.id) <= _ID_CHARS
    ):
        raise ProtocolError("message id must be 1-128 printable ASCII characters")
    if envelope.type not in MESSAGE_TYPES:
        raise ProtocolError(f"unknown message type {envelope.type!r}")
    if not isinstance(envelope.payload, dict):
        raise ProtocolError("message payload must be a JSON object")


def encode(envelope: Envelope) -> bytes:
    """One line, newline included. Raises ``ProtocolError`` past the size limit."""
    _check(envelope)
    data = json.dumps(
        envelope.to_json(), ensure_ascii=False, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    if len(data) > MAX_MESSAGE_BYTES:
        raise ProtocolError(f"message of {len(data)} bytes is over the {MAX_MESSAGE_BYTES}-byte limit")
    return data + b"\n"


def decode(line: bytes, *, allowed: frozenset[str] = MESSAGE_TYPES) -> Envelope:
    """Decodes one line and checks the envelope. ``allowed`` narrows the accepted types."""
    line = line.rstrip(b"\r\n")
    if len(line) > MAX_MESSAGE_BYTES:
        raise ProtocolError(f"message of {len(line)} bytes is over the {MAX_MESSAGE_BYTES}-byte limit")
    try:
        raw = json.loads(line.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as error:
        raise ProtocolError(f"message is not JSON: {error}") from None
    if not isinstance(raw, dict) or set(raw) != {"v", "id", "type", "payload"}:
        raise ProtocolError("message must have exactly v, id, type and payload")
    envelope = Envelope(id=raw["id"], type=raw["type"], payload=raw["payload"], v=raw["v"])
    _check(envelope)
    if envelope.type not in allowed:
        raise ProtocolError(f"{envelope.type!r} is not allowed from this side")
    return envelope


def read_line(stream: Any, limit: int = MAX_MESSAGE_BYTES) -> bytes | None:
    """Reads one line of at most ``limit`` bytes from a binary stream.

    Returns ``None`` at end of stream and raises ``ProtocolError`` for a longer line.
    """
    line = stream.readline(limit + 2)
    if not line:
        return None
    if not line.endswith(b"\n"):
        if len(line) > limit:
            raise ProtocolError(f"a message passed the {limit}-byte limit")
        # The stream ended mid-line: the other side went away.
        return None
    if len(line.rstrip(b"\r\n")) > limit:
        raise ProtocolError(f"a message passed the {limit}-byte limit")
    return line

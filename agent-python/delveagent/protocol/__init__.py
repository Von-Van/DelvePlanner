"""DelveAgent's link to DelvePlanner (piece D of the overhaul).

DelvePlanner starts the agent as a child process and talks to it over versioned
newline-delimited JSON on stdin/stdout. See ``docs/delveagent/sidecar.md``.

A runner is any callable ``run(task: dict, ctx: RunContext) -> dict`` that returns a
``run_result`` payload (``status``, ``answer``, ``drafts``...). ``ctx.tools.call(...)`` and
``ctx.model.chat(...)`` are the only ways out of the process.
"""

from .envelope import (
    MAX_MESSAGE_BYTES,
    PROTOCOL_VERSION,
    Envelope,
    ProtocolError,
    decode,
    encode,
)
from .link import (
    AgentLink,
    BudgetExhausted,
    Cancelled,
    LinkCallError,
    LinkModelClient,
    LinkToolClient,
    ModelError,
    RunContext,
    ToolError,
)

__all__ = [
    "MAX_MESSAGE_BYTES",
    "PROTOCOL_VERSION",
    "AgentLink",
    "BudgetExhausted",
    "Cancelled",
    "Envelope",
    "LinkCallError",
    "LinkModelClient",
    "LinkToolClient",
    "ModelError",
    "ProtocolError",
    "RunContext",
    "ToolError",
    "decode",
    "encode",
]

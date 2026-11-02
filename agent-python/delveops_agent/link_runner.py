"""Runs the A1 agent inside the desktop app, over the sidecar link (overhaul piece I).

The app starts ``python -m delveops_agent.protocol --runner delveops_agent.link_runner:run``. This
module only adapts: the link's live tool and model clients are wrapped in the interfaces the agent
already uses (``ToolClient`` and ``ModelClient``), and every trace event the agent records is also
streamed to the app as it happens, so the Assistant can show the run step by step.

Nothing here can write planner data. Drafts leave as drafts; the app re-validates them and asks the
person before anything is applied.
"""

from __future__ import annotations

import time
from typing import Any

from . import agent as agent_module
from .agent import A1Agent
from .model import ModelError, ModelRequest, ModelResponse
from .protocol.link import (
    BudgetExhausted,
    LinkCallError,
    RunContext,
)
from .protocol.link import (
    ModelError as LinkModelError,
)
from .protocol.link import (
    ToolError as LinkToolError,
)
from .trace import Tracer
from .types import AgentTask, Budget, ToolError, ToolResult


class LinkTools:
    """``ToolClient`` over the link. Remembers the app's call id for each call, in order."""

    def __init__(self, context: RunContext) -> None:
        self._context = context
        # The app's call id for the agent's n-th tool call (the agent numbers them tc-1, tc-2...).
        self.call_ids: list[str | None] = []

    def call(self, name: str, arguments: dict[str, Any]) -> ToolResult:
        try:
            data = self._context.tools.call(name, arguments)
        except LinkToolError as error:
            self.call_ids.append(error.call_id)
            return ToolResult(error=ToolError.from_dict(error.to_payload()))
        except BudgetExhausted as error:
            self.call_ids.append(None)
            return ToolResult(error=ToolError("budget_exhausted", error.message))
        self.call_ids.append(self._context.tools.last_call_id)
        return ToolResult(data=data)

    def app_call_id(self, agent_call_id: str) -> str:
        """The app's id for one of the agent's own ``tc-N`` ids, or the id unchanged."""
        prefix, _, number = agent_call_id.partition("-")
        if prefix == "tc" and number.isdigit():
            index = int(number) - 1
            if 0 <= index < len(self.call_ids) and self.call_ids[index]:
                return str(self.call_ids[index])
        return agent_call_id


class LinkModel:
    """``ModelClient`` over the link: the app's model broker answers every call."""

    def __init__(self, context: RunContext) -> None:
        self._context = context
        self._identity: dict[str, Any] = {}

    def describe(self) -> dict[str, Any]:
        return dict(self._identity)

    def complete(self, request: ModelRequest) -> ModelResponse:
        started = time.monotonic()
        try:
            reply = self._context.model.chat(
                request.messages,
                format=request.format,
                options=request.options or None,
            )
        except LinkModelError as error:
            raise ModelError(error.message or error.code) from None
        except BudgetExhausted as error:
            raise ModelError(error.message) from None
        message = reply.get("message") or {}
        usage = reply.get("usage") or {}
        self._identity = {
            "model": reply.get("model"),
            "digest": reply.get("digest"),
            "runtimeVersion": reply.get("ollamaVersion"),
        }
        return ModelResponse(
            content=str(message.get("content") or ""),
            model=str(reply.get("model") or ""),
            digest=reply.get("digest"),
            options=dict(reply.get("options") or request.options or {}),
            latency_ms=int((time.monotonic() - started) * 1000),
            prompt_tokens=int(usage.get("promptTokens") or 0),
            completion_tokens=int(usage.get("completionTokens") or 0),
            runtime_version=reply.get("ollamaVersion"),
        )


class StreamingTracer(Tracer):
    """Keeps the agent's own trace and also sends each event to the app as it is recorded."""

    def __init__(self, run_id: str, context: RunContext, debug: bool = False) -> None:
        super().__init__(run_id, debug=debug)
        self._context = context

    def emit(self, kind, summary, data=None, duration_ms=None, debug_data=None):  # noqa: ANN001, ANN201
        event = super().emit(kind, summary, data, duration_ms=duration_ms, debug_data=debug_data)
        try:
            self._context.trace(kind, summary, event["data"], duration_ms=duration_ms)
        except LinkCallError:
            pass  # A trace that can't be sent never ends the run.
        return event


def run(task: dict[str, Any], ctx: RunContext) -> dict[str, Any]:
    agent_task = AgentTask.from_dict(task)
    budget = Budget.from_dict(ctx.budget or task.get("budget") or {})
    tools = LinkTools(ctx)
    model = LinkModel(ctx)
    agent = A1Agent()
    # The agent builds its own tracer; swapping it for a streaming one is the only change.
    active = agent_module._Run(agent, agent_task, tools, model, budget)
    active.tracer = StreamingTracer(active.run_id, ctx, debug=agent.debug_trace)
    result = active.execute()

    out = result.to_dict()
    # Already streamed event by event; leaving it out keeps the result well inside the size limit.
    out.pop("trace", None)
    out["agentRunId"] = out.pop("runId", None)
    out["agentUsage"] = out.pop("usage", None)
    for draft in out.get("drafts", []):
        draft["evidence"] = [tools.app_call_id(item) for item in draft.get("evidence", [])]
    return out

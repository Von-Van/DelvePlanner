"""The Agent interface (overhaul plan, contract 4) and the A1 single-agent-with-tools loop.

A1 is deliberately small: the local model picks one read tool at a time, sees the real result, and
finishes with an answer, a set of drafts, a clarifying question, or `cannot_complete`. The loop is a
plain state machine so every decision shows up in the trace, and it stops on hard limits rather than
trusting the model to stop itself.
"""

from __future__ import annotations

import json
import time
import uuid
from collections.abc import Callable
from typing import Any, Protocol

from .drafts import MEMORY_OP_SPECS, DraftError, observed_records, observed_references, parse_draft
from .model import ModelClient, ModelError, ModelRequest
from .prompts import PROMPT_VERSION, STEP_SCHEMA, system_prompt, task_message
from .tools import ToolClient, ToolSpec, check_arguments, default_catalog
from .trace import Tracer
from .types import AgentResult, AgentTask, Budget, Draft, ResultStatus, ToolError, ToolResult, Usage


class Agent(Protocol):
    architecture_id: str

    def run(self, task: AgentTask, tools: ToolClient, model: ModelClient, budget: Budget) -> AgentResult: ...


# Stop reasons recorded on results, so the benchmark can score *why* a run ended.
STOP_REASONS = frozenset(
    {
        "model_call_budget",
        "tool_call_budget",
        "wall_clock_budget",
        "context_budget",
        "model_error",
        "invalid_model_output",
        "invalid_draft",
        "tool_not_permitted",
        "permission_denied",
        "ambiguous_target",
        "repeated_tool_error",
        "model_declined",
        "model_asked",
    }
)

# Two bad replies in a row from the model (unparseable step or invalid draft) end the run.
MAX_CONSECUTIVE_REPAIRS = 2
# The same tool + arguments failing with the same error this many times ends the run.
MAX_IDENTICAL_ERRORS = 2


def estimate_tokens(messages: list[dict[str, str]]) -> int:
    """Rough, model-independent size estimate (about four characters per token)."""
    return sum(len(m.get("content", "")) + 4 for m in messages) // 4


class _Stop(Exception):
    def __init__(self, status: ResultStatus, reason: str, answer: str | None = None) -> None:
        super().__init__(reason)
        self.status = status
        self.reason = reason
        self.answer = answer


class A1Agent:
    """Single local model choosing bounded read tools, then answering or drafting."""

    architecture_id = "A1"

    def __init__(
        self,
        catalog: tuple[ToolSpec, ...] | None = None,
        debug_trace: bool = False,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.catalog = {spec.name: spec for spec in (catalog if catalog is not None else default_catalog())}
        self.debug_trace = debug_trace
        self.clock = clock

    def run(self, task: AgentTask, tools: ToolClient, model: ModelClient, budget: Budget) -> AgentResult:
        return _Run(self, task, tools, model, budget).execute()


class _Run:
    def __init__(self, agent: A1Agent, task: AgentTask, tools: ToolClient, model: ModelClient, budget: Budget):
        self.agent = agent
        self.task = task
        self.tools = tools
        self.model = model
        self.budget = budget
        self.run_id = f"run-{uuid.uuid4().hex[:12]}"
        self.tracer = Tracer(self.run_id, debug=agent.debug_trace)
        self.usage = Usage()
        self.started = agent.clock()
        # Only tools that are both permitted for this task and known to the catalog are offered.
        self.offered = [agent.catalog[n] for n in task.permitted_tools if n in agent.catalog]
        self.offered_names = {spec.name for spec in self.offered}
        self.messages: list[dict[str, str]] = [
            {"role": "system", "content": system_prompt(self.offered)},
            {"role": "user", "content": task_message(task)},
        ]
        self.tool_results: dict[str, Any] = {}  # tool call id -> data, successful calls only
        self.error_counts: dict[str, int] = {}
        self.repairs_in_a_row = 0
        self.model_info: dict[str, Any] = {}

    # -- run ------------------------------------------------------------------------------------

    def execute(self) -> AgentResult:
        try:
            self.model_info = dict(self.model.describe())
        except Exception:  # noqa: BLE001 - metadata is best-effort, never fatal
            self.model_info = {}
        self.tracer.emit(
            "plan",
            "run started",
            {
                "architectureId": A1Agent.architecture_id,
                "promptVersion": PROMPT_VERSION,
                "permittedTools": sorted(self.offered_names),
                "budget": self.budget.to_dict(),
                "model": self.model_info,
            },
        )
        try:
            while True:
                status, answer, drafts = self.step()
                if status is not None:
                    return self.finish(status, answer, drafts, None)
        except _Stop as stop:
            return self.finish(stop.status, stop.answer, [], stop.reason)

    def finish(self, status: ResultStatus, answer: str | None, drafts: list[Draft], reason: str | None) -> AgentResult:
        self.usage.wall_ms = int((self.agent.clock() - self.started) * 1000)
        self.tracer.emit(
            "final",
            f"{status}" + (f" ({reason})" if reason else ""),
            {"status": status, "stopReason": reason, "draftCount": len(drafts), "usage": self.usage.to_dict()},
            duration_ms=self.usage.wall_ms,
        )
        return AgentResult(
            status=status,
            answer=answer,
            drafts=drafts,
            trace=self.tracer.events,
            usage=self.usage,
            stop_reason=reason,
            run_id=self.run_id,
            architecture_id=A1Agent.architecture_id,
            model=self.model_info,
        )

    # -- one model step -------------------------------------------------------------------------

    def check_budget(self) -> None:
        if self.agent.clock() - self.started >= self.budget.max_wall_seconds:
            raise _Stop("cannot_complete", "wall_clock_budget")
        if self.usage.model_calls >= self.budget.max_model_calls:
            raise _Stop("cannot_complete", "model_call_budget")
        if estimate_tokens(self.messages) > self.budget.max_context_tokens:
            raise _Stop("cannot_complete", "context_budget")

    def step(self) -> tuple[ResultStatus | None, str | None, list[Draft]]:
        self.check_budget()
        remaining = self.budget.max_wall_seconds - (self.agent.clock() - self.started)
        request = ModelRequest(messages=list(self.messages), format=STEP_SCHEMA, timeout_seconds=remaining)
        self.usage.model_calls += 1
        started = self.agent.clock()
        try:
            response = self.model.complete(request)
        except ModelError as error:
            self.tracer.emit("error", "model call failed", {"error": str(error)})
            raise _Stop("cannot_complete", "model_error") from error
        self.usage.prompt_tokens += response.prompt_tokens
        self.usage.completion_tokens += response.completion_tokens
        self.usage.model_latency_ms += response.latency_ms
        self.tracer.emit(
            "model_call",
            f"model call {self.usage.model_calls}",
            response.metadata(),
            duration_ms=int((self.agent.clock() - started) * 1000),
            debug_data={"messages": request.messages, "response": response.content},
        )
        self.messages.append({"role": "assistant", "content": response.content})
        if self.agent.clock() - self.started >= self.budget.max_wall_seconds:
            raise _Stop("cannot_complete", "wall_clock_budget")

        try:
            step = json.loads(response.content)
            if not isinstance(step, dict):
                raise ValueError("reply is not a JSON object")
        except ValueError as error:
            self.repair("invalid_model_output", f"Your reply was not one valid JSON step object ({error}).")
            return None, None, []

        action = step.get("action")
        if action == "call_tool":
            self.call_tool(step.get("tool"), step.get("arguments", {}))
            return None, None, []
        if action == "answer" and isinstance(step.get("answer"), str) and step["answer"].strip():
            self.repairs_in_a_row = 0
            return "answered", step["answer"], []
        if action == "draft":
            drafts = self.parse_drafts(step.get("drafts"))
            if drafts is None:
                return None, None, []
            self.repairs_in_a_row = 0
            summary = step.get("summary") if isinstance(step.get("summary"), str) else ""
            if drafts and summary.strip() and all(d.op in MEMORY_OP_SPECS for d in drafts):
                # Only "remember this?" suggestions: the run answered and proposes no planner change.
                return "answered", summary, drafts
            return "drafted", summary or None, drafts
        if action == "clarify" and isinstance(step.get("question"), str) and step["question"].strip():
            raise _Stop("needs_clarification", "model_asked", step["question"])
        if action == "cannot_complete":
            raise _Stop("cannot_complete", "model_declined", step.get("reason") or None)
        self.repair("invalid_model_output", f"Unknown or incomplete action {action!r}.")
        return None, None, []

    def repair(self, reason: str, message: str) -> None:
        """Tell the model what was wrong and let it try again, within a small bound."""
        self.repairs_in_a_row += 1
        self.tracer.emit("repair", message, {"reason": reason, "attempt": self.repairs_in_a_row})
        if self.repairs_in_a_row >= MAX_CONSECUTIVE_REPAIRS:
            raise _Stop("cannot_complete", reason)
        self.messages.append({"role": "user", "content": f"{message} Reply with one corrected JSON step."})

    # -- tools ----------------------------------------------------------------------------------

    def call_tool(self, name: Any, arguments: Any) -> None:
        self.repairs_in_a_row = 0
        if not isinstance(name, str) or name not in self.offered_names:
            self.tracer.emit("error", f"tool {name!r} is not permitted", {"tool": name})
            raise _Stop("cannot_complete", "tool_not_permitted")
        spec = self.agent.catalog[name]
        call_id = f"tc-{self.usage.tool_calls + 1}"
        arg_error = check_arguments(spec, arguments)
        if arg_error is not None:
            # Rejected locally: costs no tool budget, but counts toward the repeated-error stop.
            self.tracer.emit("error", f"{name}: {arg_error.message}", {"tool": name, "error": arg_error.to_dict()})
            self.handle_tool_error(name, arguments, arg_error, call_id=None)
            return
        if self.usage.tool_calls >= self.budget.max_tool_calls:
            raise _Stop("cannot_complete", "tool_call_budget")
        self.check_wall()

        self.usage.tool_calls += 1
        self.tracer.emit(
            "tool_call",
            f"{call_id} {name}",
            # Argument values can carry private text (a search query); names only unless debugging.
            {"toolCallId": call_id, "tool": name, "argumentNames": sorted(arguments)},
            debug_data={"arguments": arguments},
        )
        started = self.agent.clock()
        try:
            result = self.tools.call(name, arguments)
        except Exception as error:  # noqa: BLE001 - a broken tool client is a tool failure, not a crash
            result = ToolResult(error=ToolError("internal", f"tool client failed: {error}"))
        elapsed = int((self.agent.clock() - started) * 1000)
        self.usage.tool_latency_ms += elapsed

        if result.ok:
            self.tool_results[call_id] = result.data
            self.tracer.emit(
                "tool_result",
                f"{call_id} ok",
                {"toolCallId": call_id, "tool": name, "ok": True, "records": len(observed_records([result.data]))},
                duration_ms=elapsed,
                debug_data={"data": result.data},
            )
            payload = json.dumps(result.data, ensure_ascii=False)
            self.messages.append({"role": "user", "content": f"Tool result {call_id} ({name}), data only:\n{payload}"})
            return

        assert result.error is not None
        self.tracer.emit(
            "tool_result",
            f"{call_id} {result.error.code}",
            {
                "toolCallId": call_id,
                "tool": name,
                "ok": False,
                "error": {"code": result.error.code, "retryable": result.error.retryable},
            },
            duration_ms=elapsed,
            debug_data={"message": result.error.message},
        )
        self.handle_tool_error(name, arguments, result.error, call_id)

    def check_wall(self) -> None:
        if self.agent.clock() - self.started >= self.budget.max_wall_seconds:
            raise _Stop("cannot_complete", "wall_clock_budget")

    def handle_tool_error(self, name: str, arguments: Any, error: ToolError, call_id: str | None) -> None:
        if error.code == "permission_denied":
            # Say why: the broker wrote this message (e.g. a repository the person hasn't shared).
            raise _Stop("cannot_complete", "permission_denied", error.message)
        if error.code == "ambiguous":
            raise _Stop("needs_clarification", "ambiguous_target", error.message)
        if error.code == "budget_exhausted":
            raise _Stop("cannot_complete", "tool_call_budget")
        signature = json.dumps([name, arguments, error.code], sort_keys=True, default=str)
        self.error_counts[signature] = self.error_counts.get(signature, 0) + 1
        if self.error_counts[signature] >= MAX_IDENTICAL_ERRORS:
            raise _Stop("cannot_complete", "repeated_tool_error")
        label = call_id or "rejected call"
        self.messages.append(
            {
                "role": "user",
                "content": (
                    f"Tool error for {label} ({name}): {error.code}: {error.message} "
                    f"(retryable: {str(error.retryable).lower()})."
                ),
            }
        )

    # -- drafts ---------------------------------------------------------------------------------

    def parse_drafts(self, raw: Any) -> list[Draft] | None:
        if not isinstance(raw, list) or not raw:
            self.repair("invalid_draft", 'A draft step needs a non-empty "drafts" list.')
            return None
        records = observed_records(self.tool_results.values())
        references = observed_references(self.tool_results.values())
        drafts: list[Draft] = []
        try:
            for item in raw:
                drafts.append(parse_draft(item, set(self.tool_results), records, references))
        except DraftError as error:
            self.repair("invalid_draft", f"Draft rejected: {error}.")
            return None
        for draft in drafts:
            self.tracer.emit(
                "draft",
                f"draft {draft.op}",
                {"op": draft.op, "target": draft.target, "evidence": list(draft.evidence)},
                debug_data={"payload": draft.payload},
            )
        return drafts

"""The A1 agent run through the real sidecar entry point, with this test playing the app's side."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

from delveops_agent.protocol import envelope as wire
from delveops_agent.protocol.envelope import Envelope

AGENT_DIR = Path(__file__).resolve().parents[1]
RUNNER = "delveops_agent.link_runner:run"

TASK = {
    "goal": "Schedule my 'Draft grant budget' task for Thursday.",
    "now": "2026-11-03T09:00:00-06:00",
    "timeZone": "America/Chicago",
    "permittedTools": ["search_tasks", "get_day_context"],
}
BUDGET = {"maxModelCalls": 4, "maxToolCalls": 3, "maxWallSeconds": 30, "maxContextTokens": 12000}
TASK_RECORD = {"id": "task-42", "revision": 3, "planId": None, "milestoneId": None, "title": "Draft grant budget"}


class App:
    def __init__(self) -> None:
        self.process = subprocess.Popen(
            [sys.executable, "-s", "-m", "delveops_agent.protocol", "--runner", RUNNER],
            cwd=AGENT_DIR,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={"PYTHONUTF8": "1", **({"SYSTEMROOT": os.environ["SYSTEMROOT"]} if "SYSTEMROOT" in os.environ else {})},
        )
        limits = {"toolTimeoutMs": 5000, "modelTimeoutMs": 5000}
        self.send(Envelope("h-1", wire.HELLO, {"protocolVersions": [1], "limits": limits}))
        assert self.receive().type == wire.HELLO_ACK

    def send(self, message: Envelope) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(wire.encode(message))
        self.process.stdin.flush()

    def receive(self) -> Envelope:
        line = wire.read_line(self.process.stdout)
        assert line is not None, self.process.stderr.read().decode() if self.process.stderr else ""
        return wire.decode(line, allowed=wire.SENT_BY_AGENT)

    def close(self) -> None:
        assert self.process.stdin is not None
        self.process.stdin.close()
        self.process.wait(timeout=5)
        for stream in (self.process.stdout, self.process.stderr):
            if stream:
                stream.close()

    def serve(
        self, run_id: str, steps: list[dict], tool_results: dict[str, dict]
    ) -> tuple[Envelope, list[dict], list[dict]]:
        """Starts a run, answers its calls, and returns the result, trace events, and tool calls."""
        self.send(Envelope(run_id, wire.RUN_START, {"task": TASK, "budget": BUDGET}))
        traces: list[dict] = []
        tool_calls: list[dict] = []
        model_calls = 0
        tool_count = 0
        while True:
            message = self.receive()
            if message.type == wire.RUN_START_ACK:
                continue
            if message.type == wire.TRACE_EVENT:
                traces.append(message.payload)
            elif message.type == wire.MODEL_CALL:
                step = steps[model_calls]
                model_calls += 1
                self.send(
                    Envelope(
                        message.id,
                        wire.MODEL_RESULT,
                        {
                            "ok": True,
                            "result": {
                                "model": "test-model",
                                "digest": "a" * 64,
                                "ollamaVersion": "0.0.0",
                                "message": {"role": "assistant", "content": json.dumps(step)},
                                "usage": {"promptTokens": 50, "completionTokens": 10},
                            },
                        },
                    )
                )
            elif message.type == wire.TOOL_CALL:
                tool_count += 1
                tool_calls.append(message.payload)
                name = message.payload["tool"]
                self.send(
                    Envelope(
                        message.id,
                        wire.TOOL_RESULT,
                        {
                            "callId": f"call-{tool_count}",
                            "tool": name,
                            "ok": True,
                            "result": tool_results[name],
                            "durationMs": 1,
                        },
                    )
                )
            elif message.type == wire.RUN_RESULT:
                return message, traces, tool_calls
            else:
                raise AssertionError(f"unexpected {message.type}")


def test_a_draft_cites_the_apps_call_ids_and_every_step_is_streamed() -> None:
    app = App()
    try:
        result, traces, tool_calls = app.serve(
            "run-1",
            [
                {"action": "call_tool", "tool": "search_tasks", "arguments": {"query": "grant budget", "limit": 5}},
                {
                    "action": "draft",
                    "summary": "Schedule the grant budget for Thursday.",
                    "drafts": [
                        {
                            "op": "schedule_task",
                            "target": {"id": "task-42", "revision": 3},
                            "payload": {"scheduledDay": {"set": "2026-11-05"}},
                            "evidence": ["tc-1"],
                        }
                    ],
                },
            ],
            {"search_tasks": {"tasks": [TASK_RECORD]}},
        )
    finally:
        app.close()

    assert result.id == "run-1"
    payload = result.payload
    assert payload["status"] == "drafted"
    assert payload["architectureId"] == "A1"
    assert payload["drafts"][0]["evidence"] == ["call-1"]
    assert payload["drafts"][0]["target"] == {"id": "task-42", "revision": 3}
    assert "trace" not in payload
    assert payload["usage"]["modelCalls"] == 2 and payload["usage"]["toolCalls"] == 1
    assert payload["agentUsage"]["toolCalls"] == 1
    assert payload["model"] == {}  # describe() runs before the first call
    assert [call["tool"] for call in tool_calls] == ["search_tasks"]

    kinds = [event["kind"] for event in traces]
    assert kinds == ["plan", "model_call", "tool_call", "tool_result", "model_call", "draft", "final"]
    assert [event["seq"] for event in traces] == list(range(1, len(traces) + 1))
    assert all(event["runId"] == "run-1" for event in traces)
    assert traces[1]["data"]["model"] == "test-model"
    # Routine traces carry argument names, never the values the person typed.
    assert traces[2]["data"]["argumentNames"] == ["limit", "query"]
    assert "grant budget" not in json.dumps(traces)


def test_an_answer_needs_no_tools() -> None:
    app = App()
    try:
        result, traces, _ = app.serve("run-2", [{"action": "answer", "answer": "You have nothing due."}], {})
    finally:
        app.close()
    assert result.payload["status"] == "answered"
    assert result.payload["answer"] == "You have nothing due."
    assert traces[-1]["kind"] == "final"

"""Drives the real entry point as a child process, playing the app's side of the protocol."""

import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from delveops_agent.protocol import envelope as wire
from delveops_agent.protocol.envelope import Envelope

AGENT_DIR = Path(__file__).resolve().parents[3]


class Child:
    def __init__(self, limits=None):
        self.process = subprocess.Popen(
            [sys.executable, "-s", "-m", "delveops_agent.protocol", "--runner", "diagnostic"],
            cwd=AGENT_DIR,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={"PYTHONUTF8": "1", **({"SYSTEMROOT": os.environ["SYSTEMROOT"]} if "SYSTEMROOT" in os.environ else {})},
        )
        self.send(Envelope("h-1", wire.HELLO, {"protocolVersions": [1], "limits": limits or {"toolTimeoutMs": 2000, "modelTimeoutMs": 2000}}))
        self.hello = self.receive()

    def send(self, message):
        self.process.stdin.write(wire.encode(message))
        self.process.stdin.flush()

    def receive(self):
        line = wire.read_line(self.process.stdout)
        if line is None:
            return None
        return wire.decode(line, allowed=wire.SENT_BY_AGENT)

    def start(self, run_id, goal, budget=None):
        self.send(Envelope(run_id, wire.RUN_START, {"task": {"goal": goal}, "budget": budget or {}}))
        ack = self.receive()
        assert ack.type == wire.RUN_START_ACK and ack.id == run_id, ack

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            code = self.process.wait(timeout=5)
        finally:
            self.process.stdout.close()
            self.process.stderr.close()
        return code


class LinkTests(unittest.TestCase):
    def setUp(self):
        self.child = Child()
        self.addCleanup(self._close)

    def _close(self):
        if self.child.process.poll() is None:
            self.child.close()

    def test_handshake(self):
        hello = self.child.hello
        self.assertEqual(hello.type, wire.HELLO_ACK)
        self.assertEqual(hello.id, "h-1")
        self.assertEqual(hello.payload["protocolVersion"], 1)
        self.assertEqual(hello.payload["agent"]["runner"], "diagnostic")

    def test_echo_and_clean_exit(self):
        self.child.start("run-1", "echo:hi")
        result = self.child.receive()
        self.assertEqual((result.type, result.id), (wire.RUN_RESULT, "run-1"))
        self.assertEqual(result.payload["answer"], "hi")
        self.assertEqual(result.payload["usage"]["toolCalls"], 0)
        self.assertEqual(self.child.close(), 0)

    def test_tool_round_trip_uses_the_tool_contract_shape(self):
        self.child.start("run-1", "tool:get_day_context")
        call = self.child.receive()
        self.assertEqual(call.type, wire.TOOL_CALL)
        self.assertEqual(call.payload, {"tool": "get_day_context", "arguments": {}})
        self.child.send(Envelope(call.id, wire.TOOL_RESULT, {"callId": "call-1", "tool": "get_day_context", "ok": True, "result": {"events": []}, "durationMs": 3}))
        result = self.child.receive()
        self.assertEqual(json.loads(result.payload["answer"]), {"callId": "call-1", "result": {"events": []}})

    def test_tool_error_becomes_cannot_complete(self):
        self.child.start("run-1", "tool:get_day_context")
        call = self.child.receive()
        self.child.send(Envelope(call.id, wire.TOOL_RESULT, {"callId": "call-1", "tool": "get_day_context", "ok": False, "error": {"code": "not_found", "message": "gone", "retryable": False}}))
        result = self.child.receive()
        self.assertEqual(result.payload["status"], "cannot_complete")
        self.assertEqual(result.payload["error"]["code"], "not_found")

    def test_model_call_carries_the_run(self):
        self.child.start("run-1", "model")
        call = self.child.receive()
        self.assertEqual(call.type, wire.MODEL_CALL)
        self.assertEqual(call.payload["runId"], "run-1")
        self.child.send(Envelope(call.id, wire.MODEL_RESULT, {"ok": True, "result": {"message": {"role": "assistant", "content": "ok"}, "usage": {"promptTokens": 4, "completionTokens": 1}}}))
        result = self.child.receive()
        self.assertEqual(result.payload["answer"], "ok")
        self.assertEqual(result.payload["usage"]["promptTokens"], 4)

    def test_budget_is_enforced_locally(self):
        self.child.start("run-1", "tool:get_day_context", budget={"maxToolCalls": 0})
        result = self.child.receive()
        self.assertEqual(result.type, wire.RUN_RESULT)
        self.assertEqual(result.payload["error"]["code"], "budget_exhausted")

    def test_cancel_during_a_call(self):
        self.child.start("run-1", "tool:get_day_context")
        self.child.receive()  # the tool call, never answered
        self.child.send(Envelope("h-2", wire.CANCEL, {"runId": "run-1", "reason": "cancelled"}))
        result = self.child.receive()
        self.assertEqual(result.payload["status"], "cannot_complete")
        self.assertEqual(result.payload["stopReason"], "cancelled")

    def test_trace_events(self):
        self.child.start("run-1", "trace")
        kinds = []
        while True:
            message = self.child.receive()
            if message.type == wire.RUN_RESULT:
                break
            self.assertEqual(message.payload["runId"], "run-1")
            kinds.append(message.payload["kind"])
        self.assertEqual(kinds, ["plan", "verify", "final"])

    def test_stray_output_never_reaches_the_protocol(self):
        self.child.start("run-1", "stdout")
        result = self.child.receive()
        self.assertEqual(result.payload["answer"], "printed")

    def test_second_run_while_busy_is_refused(self):
        self.child.start("run-1", "sleep:5")
        self.child.send(Envelope("run-2", wire.RUN_START, {"task": {"goal": "echo:x"}, "budget": {}}))
        error = self.child.receive()
        self.assertEqual(error.type, wire.ERROR)
        self.assertEqual(error.payload["code"], "busy")

    def test_protocol_violation_exits(self):
        self.child.process.stdin.write(b"this is not json\n")
        self.child.process.stdin.flush()
        error = self.child.receive()
        self.assertEqual(error.payload["code"], "protocol_error")
        self.assertEqual(self.child.close(), 2)


class HandshakeTests(unittest.TestCase):
    def test_refuses_an_app_without_version_one(self):
        process = subprocess.Popen(
            [sys.executable, "-s", "-m", "delveops_agent.protocol"],
            cwd=AGENT_DIR, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        out, _ = process.communicate(wire.encode(Envelope("h-1", wire.HELLO, {"protocolVersions": [2]})), timeout=10)
        reply = wire.decode(out.splitlines()[0], allowed=wire.SENT_BY_AGENT)
        self.assertEqual(reply.type, wire.ERROR)
        self.assertEqual(process.returncode, 2)


if __name__ == "__main__":
    unittest.main()

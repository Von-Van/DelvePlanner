"""A runner that exercises the link, not an agent. It is what the packaging probe and the
Rust tests start. The goal is a command:

- ``echo:<text>``       answer with the text
- ``tool:<name>``       call one tool with ``{}`` and answer with its call ID and result
- ``model``             make one model call and answer with the reply text
- ``trace``             emit three trace events, then answer
- ``sleep:<seconds>``   wait, checking for cancel, then answer
- ``hang``              ignore cancel and never answer
- ``crash``             exit the process at once
- ``stdout``            print to stdout (which must not reach the protocol), then answer
- ``oversize``          write one line past the 1 MiB limit straight to the protocol stream
- ``fail``              raise an ordinary exception
- ``env:<NAME>``        say whether an environment variable reached the process
"""

from __future__ import annotations

import json
import os
import time
from typing import Any

from .envelope import MAX_MESSAGE_BYTES
from .link import RunContext


def _answer(text: str) -> dict[str, Any]:
    return {"status": "answered", "answer": text}


def run(task: dict[str, Any], ctx: RunContext) -> dict[str, Any]:
    command, _, argument = str(task.get("goal", "")).partition(":")
    if command == "echo":
        return _answer(argument)
    if command == "tool":
        result = ctx.tools.call(argument, {})
        return _answer(json.dumps({"callId": ctx.tools.last_call_id, "result": result}, sort_keys=True))
    if command == "model":
        reply = ctx.model.chat([{"role": "user", "content": "Reply with the word ok."}])
        return _answer(str((reply.get("message") or {}).get("content", "")))
    if command == "trace":
        for step in ("plan", "verify", "final"):
            ctx.trace(step, f"diagnostic {step}", {"step": step}, duration_ms=1)
        return _answer("traced")
    if command == "sleep":
        end = time.monotonic() + float(argument or 1)
        while time.monotonic() < end:
            ctx.check_cancelled()
            time.sleep(0.02)
        return _answer("slept")
    if command == "hang":
        while True:
            time.sleep(1)
    if command == "crash":
        os._exit(3)
    if command == "stdout":
        print("this goes to stderr, not the protocol")
        os.write(1, b"so does this\n")
        return _answer("printed")
    if command == "oversize":
        ctx._link.write_raw(b'{"v":1,"id":"x","type":"trace_event","payload":{"pad":"'
                            + b"x" * MAX_MESSAGE_BYTES + b'"}}\n')
        time.sleep(5)
        return _answer("unreachable")
    if command == "env":
        return _answer("present" if argument in os.environ else "absent")
    if command == "fail":
        raise RuntimeError("diagnostic failure")
    return {"status": "cannot_complete", "answer": None, "stopReason": "unknown_command"}

"""Entry point DelvePlanner starts: ``python -m delveops_agent.protocol --runner <runner>``.

``<runner>`` is ``module:function`` or ``diagnostic`` (the link's own test runner).
"""

from __future__ import annotations

import argparse
import importlib
import os
import sys
from typing import Any, BinaryIO

from .link import AgentLink, Runner

BUILTIN_RUNNERS = {"diagnostic": "delveops_agent.protocol.diagnostic:run"}


def load_runner(spec: str) -> Runner:
    target = BUILTIN_RUNNERS.get(spec, spec)
    module_name, _, attribute = target.partition(":")
    if not module_name or not attribute:
        raise SystemExit(f"--runner must be module:function, got {spec!r}")
    runner: Any = getattr(importlib.import_module(module_name), attribute)
    if not callable(runner):
        raise SystemExit(f"{target} is not callable")
    return runner


def claim_stdio() -> tuple[BinaryIO, BinaryIO]:
    """Takes stdin/stdout for the protocol alone.

    Anything else that prints, including C extensions writing to file descriptor 1, lands on
    stderr instead, so a stray print can never corrupt a message.
    """
    protocol_in = os.fdopen(os.dup(sys.stdin.fileno()), "rb", buffering=0)
    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "wb", buffering=0)
    if sys.platform == "win32":
        import msvcrt

        msvcrt.setmode(protocol_in.fileno(), os.O_BINARY)
        msvcrt.setmode(protocol_out.fileno(), os.O_BINARY)
    sys.stdout.flush()
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, sys.stdin.fileno())
    os.close(null)
    return protocol_in, protocol_out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="delveops-agent")
    parser.add_argument("--runner", default="diagnostic")
    args = parser.parse_args(argv)
    runner = load_runner(args.runner)
    reader, writer = claim_stdio()
    return AgentLink(runner, reader, writer, runner_name=args.runner).serve()


if __name__ == "__main__":
    sys.exit(main())

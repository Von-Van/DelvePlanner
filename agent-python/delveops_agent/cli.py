"""Run the A1 agent once against fixture tool responses and a local Ollama model.

    python -m delveops_agent.cli examples/overdue_tasks.json --model qwen3:8b --port 11434

The fixture holds the task, an optional budget, and canned responses for each tool (an
`{"error": {...}}` object makes that tool fail). The result, including the trace, is printed as JSON.
This never touches planner data; it is for trying the agent by hand until the benchmark (piece B)
and the live sidecar link (pieces C/D) land.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from .agent import A1Agent
from .model import ModelError, OllamaModelClient
from .tools import MockToolClient, default_catalog, load_catalog
from .types import AgentTask, Budget, ToolError


def _fixture_responses(raw: dict[str, Any]) -> dict[str, Any]:
    responses: dict[str, Any] = {}
    for name, value in raw.items():
        if isinstance(value, dict) and set(value) == {"error"}:
            responses[name] = ToolError.from_dict(value["error"])
        else:
            responses[name] = value
    return responses


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--model", default="qwen3:8b")
    parser.add_argument("--host", default="127.0.0.1", help="loopback address of the Ollama server")
    parser.add_argument("--port", type=int, default=11434)
    parser.add_argument("--catalog", type=Path, help="tools contract (default: contracts/tools.v1.json)")
    parser.add_argument("--debug-trace", action="store_true", help="include prompt/response text in the trace")
    args = parser.parse_args(argv)

    fixture = json.loads(args.fixture.read_text(encoding="utf-8"))
    task = AgentTask.from_dict(fixture["task"])
    budget = Budget.from_dict(fixture.get("budget", {}))
    tools = MockToolClient(_fixture_responses(fixture.get("toolResponses", {})))
    host = f"[{args.host}]" if ":" in args.host else args.host
    try:
        model = OllamaModelClient(f"http://{host}:{args.port}", model=args.model)
    except ModelError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    catalog = load_catalog(args.catalog) if args.catalog else default_catalog()
    agent = A1Agent(catalog=catalog, debug_trace=args.debug_trace)
    result = agent.run(task, tools, model, budget)
    json.dump(result.to_dict(), sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

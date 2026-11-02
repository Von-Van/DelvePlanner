"""Does the assistant remember what matters, and only what is still true?

    python -m agent_bench.configs.memory_check --out agent_bench/results/memory
    python -m agent_bench.configs.memory_check --out agent_bench/results/memory --split all --i-know-this-is-heldout

Runs the assistant agent with its memory (recent conversation plus the ``get_memory`` tool) on the
``memory`` and ``continuity`` cases against a local Ollama, writes an ordinary benchmark result
folder, and prints three numbers: how often it got the right answer, how often it recalled what it
should, and how often it used something outdated (false recall). False recall should be zero.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from ..cases import load_cases
from ..runner import run_suite
from .adapters import _submodule, _ToolBridge, a1_model

FAMILIES = ("memory", "continuity")
MODEL = json.loads((Path(__file__).parent / "a0_vs_a1.json").read_text())["model"]


class MemoryBenchAgent:
    """The agent package's ``MemoryAgent`` around the assistant agent, speaking agent_bench's interface."""

    def __init__(self, debug_trace: bool = False, window_turns: int | None = None) -> None:
        memory = _submodule("memory")
        agent_mod = _submodule("agent")
        self._types = _submodule("types")
        self._Turn = memory.Turn
        turns = memory.DEFAULT_WINDOW_TURNS if window_turns is None else window_turns
        self.agent = memory.MemoryAgent(lambda catalog: agent_mod.A1Agent(catalog=catalog, debug_trace=debug_trace), window_turns=turns)
        self.architecture_id = self.agent.architecture_id
        self.prompt_version = f"{_submodule('prompts').PROMPT_VERSION}+{memory.MEMORY_PROMPT_VERSION}"

    def run(self, task: Any, tools: Any, model: Any, budget: Any) -> dict[str, Any]:
        if model is None:
            raise RuntimeError("the memory check needs a model client")
        types = self._types
        agent_task = types.AgentTask.from_dict(
            {"goal": task.goal, "now": task.now, "timeZone": task.timeZone, "permittedTools": list(task.permittedTools)}
        )
        agent_budget = types.Budget.from_dict(
            {
                "maxModelCalls": budget.maxModelCalls,
                "maxToolCalls": budget.maxToolCalls,
                "maxWallSeconds": budget.maxWallSeconds,
                "maxContextTokens": budget.maxContextTokens,
            }
        )
        conversation = [self._Turn.from_dict(turn) for turn in getattr(task, "conversation", ())]
        result = self.agent.run(agent_task, _ToolBridge(tools), model, agent_budget, conversation=conversation)
        usage = result.usage
        return {
            "status": result.status,
            "answer": result.answer or "",
            "drafts": [d.to_dict() for d in result.drafts],
            "trace": [{k: v for k, v in event.items() if not (k == "durationMs" and v is None)} for event in result.trace],
            "usage": {
                "modelCalls": usage.model_calls,
                "toolCalls": usage.tool_calls,
                "tokens": usage.prompt_tokens + usage.completion_tokens,
                "latencyMs": usage.wall_ms,
            },
            "stopReason": result.stop_reason,
        }


def memory_cases(split: str) -> list[dict]:
    return [c for c in load_cases(split) if c["family"] in FAMILIES]


def headline(rows: list[dict]) -> dict:
    """Success, recall (memory cases answered correctly) and false recall (outdated memory used)."""

    def rate(flags: list[bool]) -> float | None:
        return round(sum(flags) / len(flags), 3) if flags else None

    memory_rows = [r for r in rows if r["family"] == "memory"]
    judged = [r["metrics"]["outdatedRecall"] for r in rows if r["metrics"].get("outdatedRecall") is not None]
    return {
        "runs": len(rows),
        "successRate": rate([r["success"] for r in rows]),
        "recallRate": rate([r["success"] for r in memory_rows]),
        "falseRecallRate": rate(judged),
        "unsafeRuns": sum(not r["safe"] for r in rows),
        "falseRecallCases": sorted({r["caseId"] for r in rows if r["metrics"].get("outdatedRecall")}),
    }


def _progress(row: dict) -> None:
    mark = "PASS" if row["success"] else "fail"
    outdated = "  OUTDATED" if row["metrics"].get("outdatedRecall") else ""
    print(f"{mark}  r{row['repeat']}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}{outdated}", flush=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench.configs.memory_check", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", required=True)
    parser.add_argument("--split", default="dev", choices=("dev", "heldout", "all"))
    parser.add_argument("--i-know-this-is-heldout", action="store_true")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--port", type=int, default=11434)
    parser.add_argument("--model", default=MODEL["name"])
    parser.add_argument("--allow-other-digest", action="store_true")
    args = parser.parse_args(argv)
    if args.split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print("This includes held-out cases. Re-run with --i-know-this-is-heldout to confirm.", file=sys.stderr)
        return 2
    model = a1_model(port=args.port, model=args.model, options={"temperature": 0, "seed": 7, "num_ctx": 16384})
    digest = model.describe().get("digest")
    if digest is None:
        print(f"Cannot reach {args.model} at 127.0.0.1:{args.port}; is Ollama running with the model installed?", file=sys.stderr)
        return 2
    if digest != MODEL["expectedDigest"] and not args.allow_other_digest:
        print(f"Model digest {digest} differs from the frozen {MODEL['expectedDigest']}. Pass --allow-other-digest to run anyway.", file=sys.stderr)
        return 2
    agent = MemoryBenchAgent()
    out = Path(args.out)
    run_suite(memory_cases(args.split), agent, model=model, repeats=args.repeats, out_dir=out, agent_spec=f"{agent.architecture_id} ({agent.prompt_version})", progress=_progress)
    rows = [json.loads(line) for line in (out / "results.jsonl").read_text().splitlines() if line.strip()]
    result = headline(rows)
    (out / "memory_check.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())

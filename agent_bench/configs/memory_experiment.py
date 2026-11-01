"""The memory comparison M0–M3 (handoff P7), end to end.

    python -m agent_bench.configs.memory_experiment run --strategy all --out agent_bench/results/memory --i-know-this-is-heldout
    python -m agent_bench.configs.memory_experiment compare --results agent_bench/results/memory --out agent_bench/results/memory/compare

``run`` scores one directory per strategy (``M0/`` … ``M3/``) with the ordinary benchmark runner;
``compare`` reads them back and writes ``comparison.json`` and ``comparison.md``. Settings come from
``memory_m0_m3.json`` next to this file. Every strategy wraps the same, unchanged A1 agent.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from collections import Counter
from pathlib import Path
from typing import Any

from ..cases import load_cases
from ..runner import run_suite
from .adapters import _submodule, _ToolBridge, a1_model

HERE = Path(__file__).parent
CONFIG = json.loads((HERE / "memory_m0_m3.json").read_text())
STRATEGIES = ("M0", "M1", "M2", "M3")
SPLIT_CHOICES = ("dev", "heldout", "all")


class MemoryBenchAgent:
    """The agent package's ``MemoryAgent`` around ``A1Agent``, speaking agent_bench's interface."""

    def __init__(self, strategy: str, debug_trace: bool = False) -> None:
        memory = _submodule("memory")
        agent_mod = _submodule("agent")
        self._types = _submodule("types")
        self._Turn = memory.Turn
        self.strategy = strategy
        self.agent = memory.MemoryAgent(strategy, lambda catalog: agent_mod.A1Agent(catalog=catalog, debug_trace=debug_trace))
        self.architecture_id = self.agent.architecture_id
        self.prompt_version = f"{_submodule('prompts').PROMPT_VERSION}+{memory.MEMORY_PROMPT_VERSION}"

    def run(self, task: Any, tools: Any, model: Any, budget: Any) -> dict[str, Any]:
        if model is None:
            raise RuntimeError("the memory experiment needs a model client")
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
    families = set(CONFIG["benchmark"]["families"])
    return [c for c in load_cases(split) if c["family"] in families]


def _progress(row: dict) -> None:
    mark = "PASS" if row["success"] else "fail"
    recall = "  OUTDATED" if row["metrics"].get("outdatedRecall") else ""
    print(f"{mark}  r{row['repeat']}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}{recall}", flush=True)


def cmd_run(args: argparse.Namespace) -> int:
    if args.split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print("This includes held-out cases. Re-run with --i-know-this-is-heldout to confirm.", file=sys.stderr)
        return 2
    cases = memory_cases(args.split)
    generation = CONFIG["agent"]["generation"]
    options = {k: v for k, v in generation.items() if k != "think"}
    model = a1_model(port=args.port, model=args.model, options=options)
    described = model.describe()
    expected = CONFIG["model"]["expectedDigest"]
    if described.get("digest") is None:
        print(f"Cannot reach {args.model} at 127.0.0.1:{args.port}; is Ollama running with the model installed?", file=sys.stderr)
        return 2
    if described["digest"] != expected and not args.allow_other_digest:
        print(f"Model digest {described['digest']} differs from the frozen {expected}. Pass --allow-other-digest to run anyway.", file=sys.stderr)
        return 2
    strategies = STRATEGIES if args.strategy == "all" else (args.strategy,)
    repeats = args.repeats or CONFIG["benchmark"]["repeats"]
    for strategy in strategies:
        agent = MemoryBenchAgent(strategy)
        out = Path(args.out) / strategy
        print(f"== {strategy}", flush=True)
        run_suite(cases, agent, model=model, repeats=repeats, out_dir=out, agent_spec=f"{agent.architecture_id} ({agent.prompt_version})", progress=_progress)
    print(f"results in {args.out}")
    return 0


# ---------------------------------------------------------------------------------------------------------
# Comparing


def load_strategy(directory: Path) -> tuple[dict, list[dict]]:
    summary = json.loads((directory / "summary.json").read_text())
    rows = [json.loads(line) for line in (directory / "results.jsonl").read_text().splitlines() if line.strip()]
    return summary["meta"], rows


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, int(len(ordered) * q))], 1)


def _rate(flags: list[bool]) -> float | None:
    return round(sum(flags) / len(flags), 4) if flags else None


def block(rows: list[dict]) -> dict:
    recall_rows = [r for r in rows if r["metrics"].get("outdatedRecall") is not None]
    usage = [r["metrics"]["agentUsage"] for r in rows]
    walls = [r["metrics"]["wallMs"] for r in rows]
    return {
        "runs": len(rows),
        "successRate": _rate([r["success"] for r in rows]),
        "falseRecallRate": _rate([bool(r["metrics"]["outdatedRecall"]) for r in recall_rows]),
        "falseRecallRuns": len(recall_rows),
        "abstentionRate": _rate([r["status"] in ("needs_clarification", "cannot_complete") for r in rows]),
        "unsafeRuns": sum(not r["safe"] for r in rows),
        "meanModelCalls": round(statistics.mean(u["modelCalls"] for u in usage), 2) if usage else None,
        "meanToolCalls": round(statistics.mean(u["toolCalls"] for u in usage), 2) if usage else None,
        "meanTokens": round(statistics.mean(u["tokens"] for u in usage), 1) if usage else None,
        "wallMsP50": _pct(walls, 0.5),
        "wallMsP95": _pct(walls, 0.95),
        "statuses": dict(Counter(r["status"] for r in rows)),
    }


def compare(results: Path) -> dict:
    runs: dict[str, tuple[dict, list[dict]]] = {}
    for strategy in STRATEGIES:
        directory = results / strategy
        if (directory / "summary.json").exists():
            runs[strategy] = load_strategy(directory)
    if not runs:
        raise FileNotFoundError(f"no strategy results under {results}")
    cases = {c["case_id"]: c for c in load_cases("all")}
    case_ids = sorted({r["caseId"] for _, rows in runs.values() for r in rows}, key=lambda c: (cases[c]["split"], cases[c]["family"], c))
    per_case = []
    for case_id in case_ids:
        case = cases[case_id]
        entry: dict[str, Any] = {"caseId": case_id, "family": case["family"], "split": case["split"], "goal": case["goal"], "tags": case.get("tags", [])}
        for strategy, (_, rows) in runs.items():
            mine = [r for r in rows if r["caseId"] == case_id]
            if not mine:
                continue
            entry[strategy] = {
                "runs": len(mine),
                "passes": sum(r["success"] for r in mine),
                "solved": sum(r["success"] for r in mine) * 2 > len(mine),
                "outdatedRecall": sum(bool(r["metrics"].get("outdatedRecall")) for r in mine),
                "unsafeRuns": sum(not r["safe"] for r in mine),
                "statuses": dict(Counter(r["status"] for r in mine)),
                "failedChecks": sorted({name for r in mine for name, c in r["checks"].items() if not c["passed"]}),
            }
        per_case.append(entry)
    overall = {s: block(rows) for s, (_, rows) in runs.items()}
    by_family = {
        fam: {s: block([r for r in rows if r["family"] == fam]) for s, (_, rows) in runs.items()}
        for fam in sorted({r["family"] for _, rows in runs.values() for r in rows})
    }
    by_split = {
        split: {s: block([r for r in rows if r["split"] == split]) for s, (_, rows) in runs.items()}
        for split in sorted({r["split"] for _, rows in runs.values() for r in rows})
    }
    metas = {s: meta for s, (meta, _) in runs.items()}
    models = {s: (m.get("model") or {}) for s, m in metas.items()}
    digests = {m.get("digest") for m in models.values()}
    return {
        "experimentId": CONFIG["experimentId"],
        "strategies": list(runs),
        "model": next(iter(models.values()), {}),
        "sameModelDigest": len(digests) == 1 and CONFIG["model"]["expectedDigest"] in digests,
        "caseSetHash": {s: m.get("caseSetHash") for s, m in metas.items()},
        "repeats": {s: m.get("repeats") for s, m in metas.items()},
        "platform": next(iter(metas.values())).get("platform"),
        "overall": overall,
        "byFamily": by_family,
        "bySplit": by_split,
        "solvedCases": {s: sum(1 for e in per_case if e.get(s, {}).get("solved")) for s in runs},
        "cases": per_case,
        "safetyViolations": [
            {"strategy": s, "caseId": r["caseId"], "repeat": r["repeat"], **v} for s, (_, rows) in runs.items() for r in rows for v in r["safety"]["violations"]
        ],
    }


def _fmt(value: float | None) -> str:
    return "—" if value is None else f"{value:.0%}"


def render(result: dict) -> str:
    strategies = result["strategies"]
    model = result["model"]
    lines = [
        f"# {result['experimentId']}: memory strategies on agent_bench",
        "",
        f"- Model: `{model.get('model')}` `{(model.get('digest') or '?')[:12]}`" + ("" if result["sameModelDigest"] else "  **(digest differs from the frozen one)**"),
        f"- Repeats: {result['repeats']}; case sets {result['caseSetHash']}",
        "",
        "## Headline (memory + continuity cases)",
        "",
        "| Strategy | Success | False recall | Abstained | Unsafe runs | Solved cases | Mean model calls | Mean tool calls | Mean tokens | p50 ms |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for s in strategies:
        o = result["overall"][s]
        lines.append(
            f"| {s} | {_fmt(o['successRate'])} | {_fmt(o['falseRecallRate'])} | {_fmt(o['abstentionRate'])} | {o['unsafeRuns']} | {result['solvedCases'][s]} "
            f"| {o['meanModelCalls']} | {o['meanToolCalls']} | {o['meanTokens']} | {o['wallMsP50']} |"
        )
    lines += ["", "## By family", "", "| Family | " + " | ".join(f"{s} success / false recall" for s in strategies) + " |", "| --- |" + " --- |" * len(strategies)]
    for fam, per in result["byFamily"].items():
        lines.append(f"| {fam} | " + " | ".join(f"{_fmt(per[s]['successRate'])} / {_fmt(per[s]['falseRecallRate'])}" for s in strategies) + " |")
    lines += ["", "## Per case (passes / runs; ⚠ = outdated recall in some run)", "", "| Case | Split | " + " | ".join(strategies) + " |", "| --- | --- |" + " --- |" * len(strategies)]
    for e in result["cases"]:
        cells = []
        for s in strategies:
            c = e.get(s)
            cells.append("—" if c is None else f"{c['passes']}/{c['runs']}" + (" ⚠" if c["outdatedRecall"] else "") + (" UNSAFE" if c["unsafeRuns"] else ""))
        lines.append(f"| `{e['caseId']}` | {e['split']} | " + " | ".join(cells) + " |")
    lines += ["", "## Safety violations", ""]
    lines += [f"- {v['strategy']} `{v['caseId']}` r{v['repeat']}: {v['rule']} — {v['detail']}" for v in result["safetyViolations"]] or ["None."]
    return "\n".join(lines) + "\n"


def cmd_compare(args: argparse.Namespace) -> int:
    result = compare(Path(args.results))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "comparison.json").write_text(json.dumps(result, indent=2, sort_keys=True))
    (out / "comparison.md").write_text(render(result))
    print(render(result))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench.configs.memory_experiment", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("run", help="run A1 under one or all memory strategies against a local Ollama")
    p.add_argument("--strategy", default="all", choices=(*STRATEGIES, "all"))
    p.add_argument("--split", default="all", choices=SPLIT_CHOICES)
    p.add_argument("--i-know-this-is-heldout", action="store_true")
    p.add_argument("--out", required=True)
    p.add_argument("--port", type=int, default=11434)
    p.add_argument("--model", default=CONFIG["model"]["name"])
    p.add_argument("--repeats", type=int)
    p.add_argument("--allow-other-digest", action="store_true")
    p.set_defaults(fn=cmd_run)

    p = sub.add_parser("compare", help="compare scored strategy runs")
    p.add_argument("--results", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_compare)

    args = parser.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

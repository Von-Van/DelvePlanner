"""Runs an agent over benchmark cases and writes results, traces and a summary."""

from __future__ import annotations

import hashlib
import importlib
import json
import platform
import statistics
import time
import uuid
from dataclasses import asdict
from pathlib import Path
from typing import Any, Callable

from . import __version__
from .cases import case_set_hash
from .interfaces import AgentResult, AgentTask, Budget, coerce_result
from .mock_tools import CONTRACT_PATH, MockToolClient
from .scoring import score_run
from .trace import TraceRecorder, utc_now, validate_events

RESULT_SCHEMA_VERSION = "agent_bench.result.v1"


def load_agent(spec: str, **kwargs: Any):
    """``module:attr`` -> an agent instance. ``attr`` may be a class or a zero-arg factory."""
    module_name, _, attr = spec.partition(":")
    factory = getattr(importlib.import_module(module_name), attr or "create_agent")
    return factory(**kwargs)


def task_for(case: dict) -> AgentTask:
    return AgentTask(
        goal=case["goal"],
        now=case["now"],
        timeZone=case["timeZone"],
        permittedTools=tuple(case["permittedTools"]),
        conversation=tuple(dict(turn) for turn in case.get("conversation", [])),
    )


def budget_for(case: dict, override: dict | None = None) -> Budget:
    return Budget(**{**case.get("budget", {}), **(override or {})})


def run_case(case: dict, agent: Any, model: Any = None, *, repeat: int = 0, debug: bool = False, budget_override: dict | None = None) -> dict:
    run_id = f"{case['case_id']}#{repeat}-{uuid.uuid4().hex[:8]}"
    trace = TraceRecorder(run_id, debug=debug)
    budget = budget_for(case, budget_override)
    tools = MockToolClient(case, trace=trace, max_tool_calls=budget.maxToolCalls)
    started = time.perf_counter()
    crashed = None
    try:
        result = coerce_result(agent.run(task_for(case), tools, model, budget))
    except Exception as exc:  # noqa: BLE001 - an agent crash is a scored outcome, not a harness failure
        crashed = f"{type(exc).__name__}: {exc}"
        trace.emit("error", "agent raised", {"error": crashed})
        result = AgentResult(status="error")
    wall = time.perf_counter() - started

    for i, draft in enumerate(result.drafts):
        trace.emit("draft", f"draft {i}: {draft.get('op')}", {"op": draft.get("op"), "target": draft.get("target")})
    trace.emit("final", f"status {result.status}", {"status": result.status, "drafts": len(result.drafts), "answerChars": len(result.answer)})

    score = score_run(case, result, tools)
    if wall > budget.maxWallSeconds:
        score["success"] = False
        score["checks"]["wall_time"] = {"passed": False, "detail": f"{wall:.1f}s over the {budget.maxWallSeconds}s budget"}
    if crashed:
        score["checks"]["agent_crash"] = {"passed": False, "detail": crashed}
        score["success"] = False

    agent_trace_problems = validate_events(result.trace)
    return {
        "schema": RESULT_SCHEMA_VERSION,
        "runId": run_id,
        "caseId": case["case_id"],
        "family": case["family"],
        "split": case["split"],
        "repeat": repeat,
        "status": result.status,
        "success": score["success"],
        "safe": score["safety"]["passed"],
        "checks": score["checks"],
        "safety": score["safety"],
        "metrics": {**score["metrics"], "wallMs": round(wall * 1000, 3), "agentUsage": asdict(result.usage)},
        "answer": result.answer if debug else None,
        "drafts": result.drafts,
        "calls": [c.to_json(debug) for c in tools.calls],
        "trace": trace.events,
        "agentTrace": result.trace,
        "agentTraceProblems": agent_trace_problems,
    }


def _file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:16]


def summarize(results: list[dict]) -> dict:
    def block(rows: list[dict]) -> dict:
        walls = sorted(r["metrics"]["wallMs"] for r in rows)
        return {
            "runs": len(rows),
            "successRate": round(sum(r["success"] for r in rows) / len(rows), 4) if rows else None,
            "unsafeRuns": sum(not r["safe"] for r in rows),
            "meanToolCalls": round(statistics.mean(r["metrics"]["toolCalls"] for r in rows), 2) if rows else None,
            "wallMsP50": walls[len(walls) // 2] if walls else None,
            "wallMsP95": walls[min(len(walls) - 1, int(len(walls) * 0.95))] if walls else None,
        }

    families = sorted({r["family"] for r in results})
    per_case: dict[str, list[bool]] = {}
    for r in results:
        per_case.setdefault(r["caseId"], []).append(r["success"])
    return {
        "overall": block(results),
        "byFamily": {f: block([r for r in results if r["family"] == f]) for f in families},
        "bySplit": {s: block([r for r in results if r["split"] == s]) for s in sorted({r["split"] for r in results})},
        "flakyCases": sorted(c for c, v in per_case.items() if len(set(v)) > 1),
        "safetyViolations": [{"caseId": r["caseId"], "repeat": r["repeat"], **v} for r in results for v in r["safety"]["violations"]],
    }


def run_suite(
    cases: list[dict],
    agent: Any,
    *,
    model: Any = None,
    repeats: int = 1,
    debug: bool = False,
    out_dir: Path | None = None,
    agent_spec: str = "",
    budget_override: dict | None = None,
    progress: Callable[[dict], None] | None = None,
) -> dict:
    meta = {
        "benchVersion": __version__,
        "startedAt": utc_now(),
        "agent": agent_spec,
        "architectureId": getattr(agent, "architecture_id", None),
        "model": model.describe() if model is not None and hasattr(model, "describe") else None,
        "caseSetHash": case_set_hash(cases),
        "toolDefinitionsHash": _file_hash(CONTRACT_PATH),
        "cases": len(cases),
        "splits": sorted({c["split"] for c in cases}),
        "repeats": repeats,
        "budgetOverride": budget_override,
        "debug": debug,
        "python": platform.python_version(),
        "platform": platform.platform(),
    }
    results = []
    for repeat in range(repeats):
        for case in cases:
            row = run_case(case, agent, model, repeat=repeat, debug=debug, budget_override=budget_override)
            results.append(row)
            if progress:
                progress(row)
    meta["finishedAt"] = utc_now()
    summary = {"meta": meta, "summary": summarize(results)}

    if out_dir is not None:
        out_dir.mkdir(parents=True, exist_ok=True)
        with (out_dir / "results.jsonl").open("w") as fh:
            for row in results:
                slim = {k: v for k, v in row.items() if k not in ("trace", "agentTrace")}
                fh.write(json.dumps(slim, sort_keys=True) + "\n")
        traces = out_dir / "traces"
        traces.mkdir(exist_ok=True)
        for row in results:
            name = f"{row['caseId']}.r{row['repeat']}.json"
            (traces / name).write_text(json.dumps({"runId": row["runId"], "trace": row["trace"], "agentTrace": row["agentTrace"]}, indent=2))
        (out_dir / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True))
        (out_dir / "report.md").write_text(render_report(summary, results))
    summary["results"] = results
    return summary


def render_report(summary: dict, results: list[dict]) -> str:
    meta, s = summary["meta"], summary["summary"]
    lines = [
        f"# agent_bench run: {meta['agent'] or meta['architectureId']}",
        "",
        f"- Bench {meta['benchVersion']}, case set `{meta['caseSetHash']}`, tools `{meta['toolDefinitionsHash']}`",
        f"- Splits: {', '.join(meta['splits'])}; {meta['cases']} cases x {meta['repeats']} repeats",
        f"- Model: {json.dumps(meta['model']) if meta['model'] else 'none'}",
        "",
        "## Safety (stop-ship if not zero)",
        "",
        f"Unsafe runs: **{s['overall']['unsafeRuns']}**",
        "",
    ]
    for v in s["safetyViolations"]:
        lines.append(f"- `{v['caseId']}` repeat {v['repeat']}: {v['rule']} — {v['detail']}")
    lines += ["", "## Task success by family", "", "| Family | Runs | Success | Mean tool calls |", "| --- | --- | --- | --- |"]
    for fam, b in s["byFamily"].items():
        lines.append(f"| {fam} | {b['runs']} | {b['successRate']:.0%} | {b['meanToolCalls']} |")
    o = s["overall"]
    lines += [f"| **all** | {o['runs']} | {o['successRate']:.0%} | {o['meanToolCalls']} |", "", "## Failed checks", ""]
    for r in results:
        failed = [f"{k}: {v['detail']}" for k, v in r["checks"].items() if not v["passed"]]
        if failed:
            lines.append(f"- `{r['caseId']}` r{r['repeat']} — " + "; ".join(failed))
    if s["flakyCases"]:
        lines += ["", "Flaky across repeats: " + ", ".join(f"`{c}`" for c in s["flakyCases"])]
    return "\n".join(lines) + "\n"

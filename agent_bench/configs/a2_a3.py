"""Run and report the A2/A3 experiment (config: ``a2_a3.json``).

    python -m agent_bench.configs.a2_a3 run --arm main --port 11435 --out R/main --i-know-this-is-heldout
    python -m agent_bench.configs.a2_a3 run --arm ablations --port 11435 --out R/ablations
    python -m agent_bench.configs.a2_a3 run --arm faults --port 11435 --out R/faults
    python -m agent_bench.configs.a2_a3 report --results R --out R/report

Each agent's runs go to ``<out>/<agent>/`` (fault runs to ``<out>/<profile>/<agent>/``) in the normal
agent_bench layout, so the in-app Evaluation view can read them like any other run. ``report`` reads them
all back and writes ``report.json`` and ``report.md``.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
from collections import Counter
from pathlib import Path
from typing import Any

from ..cases import load_cases
from ..faults import fault_cases, graceful, summarize_faults
from ..runner import run_suite
from .adapters import A1BenchAgent, _submodule, a1_model, agent_package

HERE = Path(__file__).parent
CONFIG = json.loads((HERE / "a2_a3.json").read_text())
SPLIT_CHOICES = ("dev", "heldout", "all")


# ---------------------------------------------------------------------------------------------------------
# Agents


def _options(spec: dict) -> dict[str, Any]:
    """Config options (camelCase) as agent keyword arguments."""
    review = _submodule("verify.review")
    out: dict[str, Any] = {}
    for key, value in (spec.get("options") or {}).items():
        if key == "verifier":
            out["verifier"] = review.VerifierConfig.from_dict(value or {})
        else:
            out[re.sub(r"(?<!^)(?=[A-Z])", "_", key).lower()] = value
    return out


class ArchBenchAgent(A1BenchAgent):
    """Any architecture from the agent package behind agent_bench's interface (A1's adapter, another agent)."""

    def __init__(self, name: str, debug_trace: bool = False) -> None:
        super().__init__(debug_trace=debug_trace)
        spec = CONFIG["agents"][name]
        if spec["class"] != "A1Agent":
            cls = getattr(agent_package(), spec["class"])
            self.agent = cls(debug_trace=debug_trace, architecture_id=name, **_options(spec))
        self.architecture_id = name
        describe = getattr(self.agent, "describe", None)
        self.details = describe() if callable(describe) else {"architectureId": name, "promptVersion": self.prompt_version}
        self.prompt_version = self.details["promptVersion"]


def slug(name: str) -> str:
    return name.replace("+", "-plus-")


# ---------------------------------------------------------------------------------------------------------
# Running


def _progress(row: dict) -> None:
    mark = "PASS" if row["success"] else "fail"
    print(f"{mark}  r{row['repeat']}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}", flush=True)


def _model(args: argparse.Namespace) -> Any:
    options = {k: v for k, v in CONFIG["generation"].items() if k != "think"}
    model = a1_model(port=args.port, model=args.model, options=options)
    described = model.describe()
    if described.get("digest") is None:
        print(f"Cannot reach {args.model} at 127.0.0.1:{args.port}; is Ollama running with the model installed?", file=sys.stderr)
        return None
    expected = CONFIG["model"]["expectedDigest"]
    if args.model == CONFIG["model"]["name"] and described["digest"] != expected and not args.allow_other_digest:
        print(f"Model digest {described['digest']} differs from the frozen {expected}. Pass --allow-other-digest to run anyway.", file=sys.stderr)
        return None
    return model


def arm_cases(arm: str, split: str) -> dict[str, list[dict]]:
    """Cases per output folder: one group for main/ablations, one per fault profile."""
    cases = load_cases(split)
    if arm != "faults":
        return {"": cases}
    return {profile: fault_cases(cases, profile) for profile in CONFIG["arms"]["faults"]["profiles"]}


def run_arm(arm_name: str, out_dir: Path, model: Any, *, agents: list[str] | None = None, split: str | None = None, repeats: int | None = None, progress: Any = _progress) -> list[Path]:
    """Run every agent of an arm; returns the result folders written."""
    arm = CONFIG["arms"][arm_name]
    names = agents or arm["agents"]
    unknown = [n for n in names if n not in CONFIG["agents"]]
    if unknown:
        raise KeyError(f"unknown agents {unknown}; known: {', '.join(CONFIG['agents'])}")
    repeats = repeats or arm["repeats"]
    written = []
    for group, cases in arm_cases(arm_name, split or arm["split"]).items():
        for name in names:
            agent = ArchBenchAgent(name)
            out = out_dir / group / slug(name) if group else out_dir / slug(name)
            print(f"== {arm_name} {group} {name}: {len(cases)} cases x {repeats}".replace("  ", " "), flush=True)
            run_suite(cases, agent, model=model, repeats=repeats, out_dir=out, agent_spec=f"{name} ({agent.prompt_version})", progress=progress)
            path = out / "summary.json"
            summary = json.loads(path.read_text())
            summary["meta"].update({"experimentId": CONFIG["experimentId"], "arm": arm_name, "faultProfile": group or None, "agentDetails": agent.details})
            path.write_text(json.dumps(summary, indent=2, sort_keys=True))
            written.append(out)
    return written


def cmd_run(args: argparse.Namespace) -> int:
    split = args.split or CONFIG["arms"][args.arm]["split"]
    if split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print("This includes held-out cases. Re-run with --i-know-this-is-heldout to confirm.", file=sys.stderr)
        return 2
    agents = args.agents.split(",") if args.agents else None
    if agents and any(n not in CONFIG["agents"] for n in agents):
        print(f"unknown agent in {agents}; known: {', '.join(CONFIG['agents'])}", file=sys.stderr)
        return 2
    model = _model(args)
    if model is None:
        return 2
    run_arm(args.arm, Path(args.out), model, agents=agents, split=split, repeats=args.repeats)
    print(f"results in {args.out}")
    return 0


# ---------------------------------------------------------------------------------------------------------
# Reporting


def load_dir(directory: Path) -> tuple[dict, list[dict]]:
    summary = json.loads((directory / "summary.json").read_text())
    rows = [json.loads(line) for line in (directory / "results.jsonl").read_text().splitlines() if line.strip()]
    return summary["meta"], rows


def app_ready(row: dict) -> bool:
    """Drafted, and every draft would get past Rust: the right shape and a current revision."""
    if row["status"] != "drafted" or not row.get("drafts"):
        return False
    return all(row["checks"].get(k, {"passed": True})["passed"] for k in ("draft_shapes", "fresh_revisions"))


def _rate(values: list[bool]) -> float | None:
    return round(sum(values) / len(values), 4) if values else None


def _p50(values: list[float]) -> float | None:
    return round(statistics.median(values), 1) if values else None


def block(rows: list[dict]) -> dict[str, Any]:
    drafted = [r for r in rows if r["status"] == "drafted"]
    usage = [r["metrics"]["agentUsage"] for r in rows]
    return {
        "runs": len(rows),
        "success": _rate([r["success"] for r in rows]),
        "unsafeRuns": sum(not r["safe"] for r in rows),
        "draftedRuns": len(drafted),
        "appReady": _rate([app_ready(r) for r in drafted]),
        "statuses": dict(Counter(r["status"] for r in rows)),
        "latencyMsP50": _p50([r["metrics"]["wallMs"] for r in rows]),
        "meanModelCalls": round(statistics.mean(u["modelCalls"] for u in usage), 2) if usage else None,
        "meanToolCalls": round(statistics.mean(u["toolCalls"] for u in usage), 2) if usage else None,
        "meanTokens": round(statistics.mean(u["tokens"] for u in usage), 1) if usage else None,
    }


def _solved(rows: list[dict]) -> bool:
    return sum(r["success"] for r in rows) * 2 > len(rows)


def report(results: Path) -> dict[str, Any]:
    cases = {c["case_id"]: c for c in load_cases("all")}
    out: dict[str, Any] = {"experimentId": CONFIG["experimentId"], "arms": {}}
    models: set[tuple] = set()

    for arm_name in ("main", "ablations"):
        arm_dir = results / arm_name
        if not arm_dir.is_dir():
            continue
        agents: dict[str, list[dict]] = {}
        for name in CONFIG["agents"]:
            directory = arm_dir / slug(name)
            if (directory / "summary.json").is_file():
                meta, rows = load_dir(directory)
                agents[name] = rows
                model = meta.get("model") or {}
                models.add((model.get("model"), model.get("digest"), model.get("runtimeVersion"), meta.get("platform")))
        if not agents:
            continue
        splits = sorted({cases[r["caseId"]]["split"] for rows in agents.values() for r in rows})
        families = sorted({cases[r["caseId"]]["family"] for rows in agents.values() for r in rows})
        per_case = []
        for case_id in sorted({r["caseId"] for rows in agents.values() for r in rows}, key=lambda c: (cases[c]["split"], cases[c]["family"], c)):
            entry: dict[str, Any] = {"caseId": case_id, "split": cases[case_id]["split"], "family": cases[case_id]["family"]}
            for name, rows in agents.items():
                mine = [r for r in rows if r["caseId"] == case_id]
                if mine:
                    entry[name] = {
                        "passes": sum(r["success"] for r in mine),
                        "runs": len(mine),
                        "solved": _solved(mine),
                        "failedChecks": sorted({k for r in mine for k, c in r["checks"].items() if not c["passed"]}),
                    }
            per_case.append(entry)
        arm: dict[str, Any] = {
            "agents": list(agents),
            "bySplit": {s: {n: block([r for r in rows if cases[r["caseId"]]["split"] == s]) for n, rows in agents.items()} for s in splits},
            "byFamily": {f: {n: block([r for r in rows if cases[r["caseId"]]["family"] == f]) for n, rows in agents.items()} for f in families},
            "solvedCases": {n: sum(1 for e in per_case if e.get(n, {}).get("solved")) for n in agents},
            "cases": per_case,
            "safetyViolations": [
                {"agent": n, "caseId": r["caseId"], "repeat": r["repeat"], **v} for n, rows in agents.items() for r in rows for v in r["safety"]["violations"]
            ],
        }
        out["arms"][arm_name] = arm

    fault_dir = results / "faults"
    if fault_dir.is_dir():
        faults: dict[str, Any] = {}
        for profile in CONFIG["arms"]["faults"]["profiles"]:
            for name in CONFIG["agents"]:
                directory = fault_dir / profile / slug(name)
                if (directory / "summary.json").is_file():
                    _, rows = load_dir(directory)
                    summary = summarize_faults(rows)["overall"]
                    summary["appReady"] = block(rows)["appReady"]
                    summary["gracefulCases"] = sorted(r["caseId"] for r in rows if graceful(r))
                    faults.setdefault(profile, {})[name] = summary
        out["arms"]["faults"] = faults

    out["models"] = [{"model": m[0], "digest": m[1], "runtimeVersion": m[2], "platform": m[3]} for m in sorted(models, key=str)]
    out["sameModel"] = len({(m[0], m[1]) for m in models}) <= 1
    return out


def _fmt(value: float | None) -> str:
    return "—" if value is None else f"{value:.0%}"


def render(result: dict[str, Any]) -> str:
    lines = [f"# {result['experimentId']}: A2 and A3 against A1", ""]
    for m in result["models"]:
        lines.append(f"- Model `{m['model']}` `{(m['digest'] or '?')[:12]}`, Ollama {m['runtimeVersion']}, {m['platform']}")
    if not result["sameModel"]:
        lines.append("- **Runs used different models or digests; do not compare them directly.**")
    for arm_name, arm in result["arms"].items():
        if arm_name == "faults":
            continue
        names = arm["agents"]
        lines += ["", f"## {arm_name.capitalize()}", ""]
        lines += ["| Split | Agent | Runs | Success | App-ready drafts | Unsafe | Model calls | Tool calls | p50 ms |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
        for split, by_agent in arm["bySplit"].items():
            for name in names:
                b = by_agent[name]
                lines.append(
                    f"| {split} | {name} | {b['runs']} | {_fmt(b['success'])} | {_fmt(b['appReady'])} ({b['draftedRuns']}) | {b['unsafeRuns']} "
                    f"| {b['meanModelCalls']} | {b['meanToolCalls']} | {b['latencyMsP50']} |"
                )
        lines += ["", "Solved cases: " + ", ".join(f"{n} {arm['solvedCases'][n]}" for n in names), ""]
        lines += ["| Family | " + " | ".join(names) + " |", "| --- |" + " --- |" * len(names)]
        for fam, by_agent in arm["byFamily"].items():
            lines.append(f"| {fam} | " + " | ".join(_fmt(by_agent[n]["success"]) for n in names) + " |")
        lines += ["", "| Case | Split | " + " | ".join(names) + " |", "| --- | --- |" + " --- |" * len(names)]
        for e in arm["cases"]:
            cells = [f"{e[n]['passes']}/{e[n]['runs']}" if n in e else "—" for n in names]
            lines.append(f"| `{e['caseId']}` | {e['split']} | " + " | ".join(cells) + " |")
        lines += ["", "Safety violations:", ""]
        lines += [f"- {v['agent']} `{v['caseId']}` r{v['repeat']}: {v['rule']} — {v['detail']}" for v in arm["safetyViolations"]] or ["- none"]
    faults = result["arms"].get("faults")
    if faults:
        lines += ["", "## Fault injection", "", "| Profile | Agent | Runs | Success | Graceful | Honest stops | Unsafe |", "| --- | --- | --- | --- | --- | --- | --- |"]
        for profile, by_agent in faults.items():
            for name, b in by_agent.items():
                lines.append(f"| {profile} | {name} | {b['runs']} | {_fmt(b['success'])} | {_fmt(b['graceful'])} | {b['honestStops']} | {b['unsafeRuns']} |")
    return "\n".join(lines) + "\n"


def cmd_report(args: argparse.Namespace) -> int:
    result = report(Path(args.results))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.json").write_text(json.dumps(result, indent=2, sort_keys=True))
    (out / "report.md").write_text(render(result))
    print(render(result))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench.configs.a2_a3", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("run", help="run one arm of the experiment against a local Ollama")
    p.add_argument("--arm", required=True, choices=sorted(CONFIG["arms"]))
    p.add_argument("--out", required=True)
    p.add_argument("--agents", help="comma-separated subset of the arm's agents")
    p.add_argument("--split", choices=SPLIT_CHOICES, help="override the arm's split")
    p.add_argument("--repeats", type=int)
    p.add_argument("--port", type=int, default=11434)
    p.add_argument("--model", default=CONFIG["model"]["name"], help="a second model for the two-model comparison")
    p.add_argument("--allow-other-digest", action="store_true")
    p.add_argument("--i-know-this-is-heldout", action="store_true")
    p.set_defaults(fn=cmd_run)
    p = sub.add_parser("report", help="summarise every arm found under a results folder")
    p.add_argument("--results", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_report)
    args = parser.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())

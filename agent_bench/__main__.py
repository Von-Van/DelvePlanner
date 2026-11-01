"""Command line: ``python -m agent_bench {validate,list,run}``."""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path

from .cases import case_files, case_problems, load_cases
from .runner import load_agent, run_suite

DEFAULT_OUT = Path(__file__).parent / "results"


def cmd_validate(args: argparse.Namespace) -> int:
    bad = 0
    seen: set[str] = set()
    for path in case_files("all"):
        case = json.loads(path.read_text())
        problems = case_problems(case, path)
        if case.get("case_id") in seen:
            problems.append("duplicate case_id")
        seen.add(case.get("case_id"))
        for p in problems:
            print(f"{path.relative_to(path.parents[2])}: {p}")
        bad += bool(problems)
    print(f"{len(seen)} cases, {bad} with problems")
    return 1 if bad else 0


def cmd_list(args: argparse.Namespace) -> int:
    for c in load_cases(args.split):
        print(f"{c['split']:8} {c['family']:16} {c['case_id']:28} {c['goal']}")
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    if args.split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print("Held-out cases are for final evaluation only. Re-run with --i-know-this-is-heldout to include them.", file=sys.stderr)
        return 2
    cases = load_cases(args.split, family=args.family, ids=args.case or None)
    if not cases:
        print("no cases selected", file=sys.stderr)
        return 2
    agent = load_agent(args.agent)
    out = Path(args.out) if args.out else DEFAULT_OUT / f"{datetime.now():%Y%m%d-%H%M%S}-{getattr(agent, 'architecture_id', 'agent')}"
    budget = {"maxToolCalls": args.max_tool_calls} if args.max_tool_calls else None

    def progress(row: dict) -> None:
        mark = "PASS" if row["success"] else "fail"
        safe = "" if row["safe"] else "  UNSAFE"
        print(f"{mark}  r{row['repeat']}  {row['caseId']}{safe}")

    summary = run_suite(cases, agent, repeats=args.repeats, debug=args.debug, out_dir=out, agent_spec=args.agent, budget_override=budget, progress=progress)
    overall = summary["summary"]["overall"]
    print(f"\nsuccess {overall['successRate']:.0%} over {overall['runs']} runs; unsafe runs: {overall['unsafeRuns']}")
    print(f"results in {out}")
    if args.require_success is not None and overall["successRate"] < args.require_success:
        return 1
    return 1 if overall["unsafeRuns"] else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench", description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("validate", help="check every case against the schema").set_defaults(fn=cmd_validate)
    p_list = sub.add_parser("list", help="list cases")
    p_list.add_argument("--split", default="all", choices=["dev", "heldout", "all"])
    p_list.set_defaults(fn=cmd_list)
    p_run = sub.add_parser("run", help="run an agent over cases")
    p_run.add_argument("--agent", default="agent_bench.agents.reference:create_agent", help="module:factory")
    p_run.add_argument("--split", default="dev", choices=["dev", "heldout", "all"])
    p_run.add_argument("--family")
    p_run.add_argument("--case", action="append", help="run only this case id (repeatable)")
    p_run.add_argument("--repeats", type=int, default=1)
    p_run.add_argument("--max-tool-calls", type=int)
    p_run.add_argument("--debug", action="store_true", help="keep full tool results and answers in the output")
    p_run.add_argument("--out", help="output directory (default: agent_bench/results/<time>-<agent>)")
    p_run.add_argument("--require-success", type=float, help="exit non-zero below this success rate (0-1)")
    p_run.add_argument("--i-know-this-is-heldout", action="store_true")
    p_run.set_defaults(fn=cmd_run)
    args = parser.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

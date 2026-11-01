"""Run the assistant against the fault suite with the local model (milestone M4's real-model check).

    python -m agent_bench.faults run --port 11435 --out agent_bench/results/m4/on
    python -m agent_bench.faults run --port 11435 --out agent_bench/results/m4/off --no-self-check
    python -m agent_bench.faults report agent_bench/results/m4/on agent_bench/results/m4/off

`run` asks every dev case as written, then again under each fault profile, once each: at temperature 0
with a fixed seed, repeats gave identical results in the A0-vs-A1 run. Model, digest and generation
settings are the ones frozen in `configs/a0_vs_a1.json`. `report` prints one table answering M4's
question: with the self-check on, does the assistant recover more often, and never less safely?
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from ..cases import load_cases
from ..runner import run_suite
from . import PROFILES, fault_cases, graceful, profile_of, summarize_faults


def suite() -> list[dict]:
    cases = load_cases("dev")
    return cases + [c for profile in PROFILES for c in fault_cases(cases, profile)]


def _progress(row: dict) -> None:
    mark = "PASS" if row["success"] else ("ok  " if graceful(row) else "fail")
    print(f"{mark}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}", flush=True)


def cmd_run(args: argparse.Namespace) -> int:
    from ..configs.adapters import A1BenchAgent, a1_model
    from ..configs.experiment import CONFIG

    generation = CONFIG["architectures"]["A1"]["generation"]
    model = a1_model(port=args.port, model=args.model, options={k: v for k, v in generation.items() if k != "think"})
    digest = model.describe().get("digest")
    if digest is None:
        print(f"Cannot reach {args.model} at 127.0.0.1:{args.port}; is Ollama running with the model installed?", file=sys.stderr)
        return 2
    if digest != CONFIG["model"]["expectedDigest"] and not args.allow_other_digest:
        print(f"Model digest {digest} differs from the frozen one. Pass --allow-other-digest to run anyway.", file=sys.stderr)
        return 2
    agent = A1BenchAgent(self_check=not args.no_self_check)
    label = f"assistant ({agent.prompt_version}, self-check {'off' if args.no_self_check else 'on'})"
    summary = run_suite(suite(), agent, model=model, out_dir=Path(args.out), agent_spec=label, progress=_progress)
    faults = summarize_faults(summary["results"])
    (Path(args.out) / "faults.json").write_text(json.dumps(faults, indent=2, sort_keys=True))
    print(json.dumps(faults["byProfile"], indent=2))
    return 0


def _rows(directory: Path) -> list[dict]:
    return [json.loads(line) for line in (directory / "results.jsonl").read_text().splitlines() if line.strip()]


def report(directories: list[Path]) -> str:
    lines = ["| Run | Profile | Runs | Success | Graceful | Unsafe |", "| --- | --- | --- | --- | --- | --- |"]
    for directory in directories:
        rows = _rows(directory)
        label = json.loads((directory / "summary.json").read_text())["meta"]["agent"]
        for profile, block in summarize_faults(rows)["byProfile"].items():
            lines.append(
                f"| {label} | {profile} | {block['runs']} | {block['success']:.0%} | {block['graceful']:.0%} | "
                f"{block['unsafeRuns']} |"
            )
        failed = sorted(r["caseId"] for r in rows if not graceful(r) and profile_of(r["caseId"]) != "outage")
        if failed:
            lines.append(f"| {label} | not graceful | {len(failed)} | {', '.join(failed)} | | |")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench.faults")
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="run the fault suite against a local Ollama")
    run.add_argument("--port", type=int, default=11434)
    run.add_argument("--model", default="qwen3:8b")
    run.add_argument("--out", required=True)
    run.add_argument("--no-self-check", action="store_true", help="the loop as it was before M4, for comparison")
    run.add_argument("--allow-other-digest", action="store_true")
    rep = sub.add_parser("report", help="one table from one or more run folders")
    rep.add_argument("dirs", nargs="+", type=Path)
    args = parser.parse_args(argv)
    if args.command == "run":
        return cmd_run(args)
    print(report(args.dirs))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

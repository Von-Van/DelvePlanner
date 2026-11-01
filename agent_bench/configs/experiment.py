"""The A0-vs-A1 comparison (handoff P4), end to end.

    python -m agent_bench.configs.experiment a0-input --out agent_bench/results/a0-vs-a1/a0-input.json --i-know-this-is-heldout
    cargo run --release --manifest-path src-tauri/Cargo.toml --example a0_bench -- \\
        --input agent_bench/results/a0-vs-a1/a0-input.json --runs 3 --output agent_bench/results/a0-vs-a1/a0-output.json
    python -m agent_bench.configs.experiment a0 --a0-output agent_bench/results/a0-vs-a1/a0-output.json --out agent_bench/results/a0-vs-a1/A0 --i-know-this-is-heldout
    python -m agent_bench.configs.experiment a1 --out agent_bench/results/a0-vs-a1/A1 --i-know-this-is-heldout
    python -m agent_bench.configs.experiment compare --a0 agent_bench/results/a0-vs-a1/A0 --a1 agent_bench/results/a0-vs-a1/A1 --out agent_bench/results/a0-vs-a1/compare

Settings come from ``a0_vs_a1.json`` next to this file. ``compare`` writes ``comparison.json`` and
``comparison.md``; the experiment report in ``docs/research/experiments/`` is written from them.
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
from .adapters import A0ReplayAgent, A1BenchAgent, a0_applicable, a0_input, a1_model, known_records, starting_state

HERE = Path(__file__).parent
CONFIG = json.loads((HERE / "a0_vs_a1.json").read_text())
PROCESS_CHECKS = frozenset(CONFIG["scoring"]["processChecks"])
SPLIT_CHOICES = ("dev", "heldout", "all")


def _cases(split: str) -> list[dict]:
    return load_cases(split)


def _heldout_guard(args: argparse.Namespace) -> bool:
    if args.split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print("This includes held-out cases. Re-run with --i-know-this-is-heldout to confirm.", file=sys.stderr)
        return False
    return True


def _progress(row: dict) -> None:
    mark = "PASS" if row["success"] else "fail"
    print(f"{mark}  r{row['repeat']}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}", flush=True)


# ---------------------------------------------------------------------------------------------------------
# Running


def cmd_a0_input(args: argparse.Namespace) -> int:
    if not _heldout_guard(args):
        return 2
    cases = _cases(args.split)
    payload = a0_input(cases)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2))
    skipped = len(cases) - len(payload["cases"])
    print(f"{len(payload['cases'])} cases for A0 in {out} ({skipped} skipped: they test behaviour between tool calls)")
    return 0


def cmd_a0(args: argparse.Namespace) -> int:
    if not _heldout_guard(args):
        return 2
    output = json.loads(Path(args.a0_output).read_text())
    all_cases = _cases(args.split)
    agent = A0ReplayAgent(output, all_cases)
    cases = [c for c in all_cases if c["case_id"] in agent.case_ids()]
    if not cases:
        print("the A0 output has no runs for the selected cases", file=sys.stderr)
        return 2
    summary = run_suite(cases, agent, repeats=agent.repeats(), out_dir=Path(args.out), agent_spec="A0 (a0_bench replay)", progress=_progress)
    meta = {**summary["meta"], "a0": agent.meta}
    path = Path(args.out) / "summary.json"
    written = json.loads(path.read_text())
    written["meta"] = meta
    path.write_text(json.dumps(written, indent=2, sort_keys=True))
    print(f"A0 replies scored in {args.out}")
    return 0


def cmd_a1(args: argparse.Namespace) -> int:
    if not _heldout_guard(args):
        return 2
    cases = _cases(args.split)
    generation = CONFIG["architectures"]["A1"]["generation"]
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
    agent = A1BenchAgent()
    repeats = args.repeats or CONFIG["benchmark"]["repeats"]
    run_suite(cases, agent, model=model, repeats=repeats, out_dir=Path(args.out), agent_spec=f"A1 ({agent.prompt_version})", progress=_progress)
    print(f"A1 results in {args.out}")
    return 0


# ---------------------------------------------------------------------------------------------------------
# Comparing


def load_rows(directory: Path) -> tuple[dict, list[dict]]:
    summary = json.loads((directory / "summary.json").read_text())
    rows = [json.loads(line) for line in (directory / "results.jsonl").read_text().splitlines() if line.strip()]
    return summary["meta"], rows


def outcome_success(row: dict) -> bool:
    """Success on everything except how the tools were used, which only means something for A1."""
    return all(check["passed"] for name, check in row["checks"].items() if name not in PROCESS_CHECKS)


def a0_safety(row: dict, state: dict) -> list[dict]:
    """A0's safety verdict.

    The harness flags any draft whose target no *tool* returned. A0 reads no tools: Rust gives it a
    snapshot of real records instead. So for A0 a draft is grounded when its target is a real record in
    the case's starting state; every other safety rule applies unchanged.
    """
    kept = [v for v in row["safety"]["violations"] if v["rule"] != "ungrounded_draft"]
    known = known_records(state)
    for i, draft in enumerate(row.get("drafts", [])):
        target = draft.get("target")
        if isinstance(target, dict) and target.get("id") not in known:
            kept.append({"rule": "ungrounded_draft", "detail": f"draft {i} targets {target.get('id')!r}, which is not in the planner"})
    return kept


def _pct(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, int(len(ordered) * q))], 1)


def _rate(rows: list[dict], key: str) -> float | None:
    return round(sum(r[key] for r in rows) / len(rows), 4) if rows else None


def _solved(rows: list[dict], key: str) -> bool:
    return sum(r[key] for r in rows) * 2 > len(rows)


def compare(a0_dir: Path, a1_dir: Path) -> dict:
    a0_meta, a0_rows = load_rows(a0_dir)
    a1_meta, a1_rows = load_rows(a1_dir)
    cases = {c["case_id"]: c for c in load_cases("all")}

    for row in a0_rows:
        row["arch"] = "A0"
        row["outcome"] = outcome_success(row)
        row["violations"] = a0_safety(row, starting_state(cases[row["caseId"]]))
        row["latencyMs"] = row["metrics"]["agentUsage"]["latencyMs"]
    for row in a1_rows:
        row["arch"] = "A1"
        row["outcome"] = outcome_success(row)
        row["violations"] = row["safety"]["violations"]
        row["latencyMs"] = row["metrics"]["wallMs"]
    for row in a0_rows + a1_rows:
        row["safeAdj"] = not row["violations"]

    by_case: dict[str, dict[str, list[dict]]] = {}
    for row in a0_rows + a1_rows:
        by_case.setdefault(row["caseId"], {"A0": [], "A1": []})[row["arch"]].append(row)

    per_case = []
    for case_id in sorted(by_case, key=lambda c: (cases[c]["split"], cases[c]["family"], c)):
        case, runs = cases[case_id], by_case[case_id]
        applicable = a0_applicable(case) and bool(runs["A0"])
        entry: dict[str, Any] = {
            "caseId": case_id,
            "family": case["family"],
            "split": case["split"],
            "goal": case["goal"],
            "a0Applicable": applicable,
        }
        for arch in ("A0", "A1"):
            rows = runs[arch]
            if not rows:
                continue
            entry[arch] = {
                "runs": len(rows),
                "outcomePasses": sum(r["outcome"] for r in rows),
                "fullPasses": sum(r["success"] for r in rows) if arch == "A1" else None,
                "statuses": dict(Counter(r["status"] for r in rows)),
                "unsafeRuns": sum(not r["safeAdj"] for r in rows),
                "failedChecks": sorted({name for r in rows for name, c in r["checks"].items() if not c["passed"] and (arch == "A1" or name not in PROCESS_CHECKS)}),
            }
        if applicable and runs["A1"]:
            a0_ok, a1_ok = _solved(runs["A0"], "outcome"), _solved(runs["A1"], "outcome")
            entry["verdict"] = {(True, True): "both", (False, True): "A1 only", (True, False): "A0 only", (False, False): "neither"}[(a0_ok, a1_ok)]
        else:
            # Nothing to compare against, so A1 is held to every check, tool use included.
            entry["verdict"] = "A1 only tested" if runs["A1"] and _solved(runs["A1"], "success") else "A1 only tested, not solved"
        per_case.append(entry)

    def block(rows: list[dict], arch: str) -> dict:
        lat = [r["latencyMs"] for r in rows]
        out = {
            "runs": len(rows),
            "outcomeRate": _rate(rows, "outcome"),
            "fullRate": _rate(rows, "success") if arch == "A1" else None,
            "unsafeRuns": sum(not r["safeAdj"] for r in rows),
            "latencyMsP50": _pct(lat, 0.5),
            "latencyMsP95": _pct(lat, 0.95),
            "statuses": dict(Counter(r["status"] for r in rows)),
        }
        if arch == "A1" and rows:
            usage = [r["metrics"]["agentUsage"] for r in rows]
            out["meanModelCalls"] = round(statistics.mean(u["modelCalls"] for u in usage), 2)
            out["meanToolCalls"] = round(statistics.mean(u["toolCalls"] for u in usage), 2)
            out["meanTokens"] = round(statistics.mean(u["tokens"] for u in usage), 1)
        return out

    overlap = {e["caseId"] for e in per_case if e["a0Applicable"]}
    groups: dict[str, Any] = {}
    for split in ("dev", "heldout"):
        split_cases = {e["caseId"] for e in per_case if e["split"] == split}
        if not split_cases:
            continue
        groups[split] = {
            "overlap": {
                "cases": len(split_cases & overlap),
                "A0": block([r for r in a0_rows if r["caseId"] in split_cases & overlap], "A0"),
                "A1": block([r for r in a1_rows if r["caseId"] in split_cases & overlap], "A1"),
            },
            "allCasesA1": {"cases": len(split_cases), "A1": block([r for r in a1_rows if r["caseId"] in split_cases], "A1")},
        }
    families = sorted({e["family"] for e in per_case})
    by_family = {}
    for fam in families:
        ids = {e["caseId"] for e in per_case if e["family"] == fam}
        by_family[fam] = {
            "cases": len(ids),
            "overlapCases": len(ids & overlap),
            "A0": block([r for r in a0_rows if r["caseId"] in ids & overlap], "A0"),
            "A1overlap": block([r for r in a1_rows if r["caseId"] in ids & overlap], "A1"),
            "A1all": block([r for r in a1_rows if r["caseId"] in ids], "A1"),
        }
    a0_model = a0_meta.get("a0", {})
    a1_model = a1_meta.get("model") or {}
    expected = CONFIG["model"]["expectedDigest"]
    return {
        "experimentId": CONFIG["experimentId"],
        "models": {
            "A0": {"name": a0_model.get("modelName"), "digest": a0_model.get("modelDigest"), "runtimeVersion": a0_model.get("ollamaVersion")},
            "A1": {"name": a1_model.get("model"), "digest": a1_model.get("digest"), "runtimeVersion": a1_model.get("runtimeVersion"), "options": a1_model.get("options")},
            "expectedDigest": expected,
            "sameDigest": a0_model.get("modelDigest") == a1_model.get("digest") == expected,
        },
        # A0's hash is of the case set its input was made from, so it matches A1's for the same cases.
        "caseSetHash": {"A0": a0_model.get("caseSetHash"), "A1": a1_meta.get("caseSetHash")},
        "repeats": {"A0": a0_meta.get("repeats"), "A1": a1_meta.get("repeats")},
        "platform": {"A0": a0_meta.get("platform"), "A1": a1_meta.get("platform")},
        "startedAt": {"A0": a0_model.get("generatedAt"), "A1": a1_meta.get("startedAt")},
        "verdicts": dict(Counter(e["verdict"] for e in per_case)),
        "bySplit": groups,
        "byFamily": by_family,
        "cases": per_case,
        "safetyViolations": [
            {"arch": r["arch"], "caseId": r["caseId"], "repeat": r["repeat"], **v} for r in a0_rows + a1_rows for v in r["violations"]
        ],
    }


def _fmt_rate(value: float | None) -> str:
    return "—" if value is None else f"{value:.0%}"


def render(result: dict) -> str:
    m = result["models"]
    lines = [
        f"# {result['experimentId']}: A0 vs A1 on agent_bench",
        "",
        f"- Model: A0 `{m['A0']['name']}` `{(m['A0']['digest'] or '?')[:12]}`, A1 `{m['A1']['name']}` `{(m['A1']['digest'] or '?')[:12]}`"
        + ("" if m["sameDigest"] else "  **(digests differ from each other or from the frozen one)**"),
        f"- Runtime: A0 {m['A0']['runtimeVersion']}, A1 {m['A1']['runtimeVersion']}",
        f"- Repeats: A0 {result['repeats']['A0']}, A1 {result['repeats']['A1']}; case sets A0 `{result['caseSetHash']['A0']}`, A1 `{result['caseSetHash']['A1']}`",
        "",
        "## Verdicts per case (solved = most repeats pass)",
        "",
        "| Verdict | Cases |",
        "| --- | --- |",
    ]
    lines += [f"| {k} | {v} |" for k, v in sorted(result["verdicts"].items())]
    lines += ["", "## Headline", "", "| Split | Cases | A0 outcome | A1 outcome | A1 full | Unsafe A0 / A1 | Latency p50 A0 / A1 (ms) |", "| --- | --- | --- | --- | --- | --- | --- |"]
    for split, g in result["bySplit"].items():
        o = g["overlap"]
        lines.append(
            f"| {split}, overlap | {o['cases']} | {_fmt_rate(o['A0']['outcomeRate'])} | {_fmt_rate(o['A1']['outcomeRate'])} | {_fmt_rate(o['A1']['fullRate'])} "
            f"| {o['A0']['unsafeRuns']} / {o['A1']['unsafeRuns']} | {o['A0']['latencyMsP50']} / {o['A1']['latencyMsP50']} |"
        )
        a = g["allCasesA1"]
        lines.append(f"| {split}, all (A1 only) | {a['cases']} | — | {_fmt_rate(a['A1']['outcomeRate'])} | {_fmt_rate(a['A1']['fullRate'])} | — / {a['A1']['unsafeRuns']} | — / {a['A1']['latencyMsP50']} |")
    lines += ["", "## By family", "", "| Family | Cases (overlap) | A0 outcome | A1 outcome, overlap | A1 outcome, all | A1 full, all |", "| --- | --- | --- | --- | --- | --- |"]
    for fam, f in result["byFamily"].items():
        lines.append(
            f"| {fam} | {f['cases']} ({f['overlapCases']}) | {_fmt_rate(f['A0']['outcomeRate'])} | {_fmt_rate(f['A1overlap']['outcomeRate'])} | {_fmt_rate(f['A1all']['outcomeRate'])} | {_fmt_rate(f['A1all']['fullRate'])} |"
        )
    lines += ["", "## Per case", "", "| Case | Split | A0 | A1 outcome | A1 full | Verdict | Failed checks (A0; A1) |", "| --- | --- | --- | --- | --- | --- | --- |"]
    for e in result["cases"]:
        a0, a1 = e.get("A0"), e.get("A1")
        a0_txt = f"{a0['outcomePasses']}/{a0['runs']}" if a0 and e["a0Applicable"] else "n/a"
        a1_txt = f"{a1['outcomePasses']}/{a1['runs']}" if a1 else "—"
        full = f"{a1['fullPasses']}/{a1['runs']}" if a1 else "—"
        fails = f"{', '.join(a0['failedChecks']) if a0 and e['a0Applicable'] else '—'}; {', '.join(a1['failedChecks']) if a1 else '—'}"
        lines.append(f"| `{e['caseId']}` | {e['split']} | {a0_txt} | {a1_txt} | {full} | {e['verdict']} | {fails} |")
    lines += ["", "## Safety violations", ""]
    lines += [f"- {v['arch']} `{v['caseId']}` r{v['repeat']}: {v['rule']} — {v['detail']}" for v in result["safetyViolations"]] or ["None."]
    return "\n".join(lines) + "\n"


def cmd_compare(args: argparse.Namespace) -> int:
    result = compare(Path(args.a0), Path(args.a1))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "comparison.json").write_text(json.dumps(result, indent=2, sort_keys=True))
    (out / "comparison.md").write_text(render(result))
    print(render(result))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m agent_bench.configs.experiment", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)

    def with_split(p: argparse.ArgumentParser) -> None:
        p.add_argument("--split", default="all", choices=SPLIT_CHOICES)
        p.add_argument("--i-know-this-is-heldout", action="store_true")

    p = sub.add_parser("a0-input", help="write each case's starting state for a0_bench")
    with_split(p)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_a0_input)

    p = sub.add_parser("a0", help="score a0_bench output with the benchmark")
    with_split(p)
    p.add_argument("--a0-output", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_a0)

    p = sub.add_parser("a1", help="run A1 against a local Ollama")
    with_split(p)
    p.add_argument("--out", required=True)
    p.add_argument("--port", type=int, default=11434)
    p.add_argument("--model", default=CONFIG["model"]["name"])
    p.add_argument("--repeats", type=int)
    p.add_argument("--allow-other-digest", action="store_true")
    p.set_defaults(fn=cmd_a1)

    p = sub.add_parser("compare", help="compare scored A0 and A1 runs")
    p.add_argument("--a0", required=True)
    p.add_argument("--a1", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_compare)

    args = parser.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

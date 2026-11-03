"""The assistant check: one command that answers "is the assistant more reliable than last time?"

    python -m agent_bench check                    # every dev case, the assistant as the app runs it
    python -m agent_bench check --with-faults      # also every case re-asked with a lookup failing
    python -m agent_bench check --compare <folder> # compare with a particular earlier check

It runs the assistant exactly as the app does (the agent package's ``A1Agent`` with its self-check, the
conversation put in front of the request by ``link_runner.with_conversation``, memory through
``get_memory``, lessons and preferences through ``get_preferences``) against the local model, on the
benchmark's made-up planner. Then it:

* scores every case with the benchmark's own scorers and writes an ordinary run folder, straight into
  the app's results folder when the app is installed, so the Evaluation screen shows it after
  *Reload results*;
* prints a scorecard by skill ("Follow-ups in a conversation 3/3"), with safety failures on their own;
* compares with the previous check in the same folder: which cases got better, which got worse, and
  whether the model, prompt or case set changed in between (so a change in score is read fairly).

It replaces running the separate milestone checks one by one (``configs/memory_check``,
``faults run``, the M3 and M6 cases): every one of those cases is in the dev split. Nothing here writes
planner data or touches the person's own plans.

Exit status: 1 if any run was unsafe, or (with ``--fail-on-worse``) if any case got worse; 2 if the
model can't be reached or the settings are refused; 0 otherwise.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

from .cases import load_cases
from .runner import load_agent, run_suite
from .skills import BY_KEY, SKILLS, skill_key

CHECK_FILE = "check.json"
REPORT_FILE = "check.md"
CHECK_SCHEMA = "agent_bench.check.v1"
REPO = Path(__file__).resolve().parents[1]
LOCAL_RESULTS = Path(__file__).parent / "results"
TAURI_CONFIG = REPO / "src-tauri" / "tauri.conf.json"
#: The folder inside the app's data folder that the Evaluation screen reads (evaluation.rs RESULTS_FOLDER).
APP_RESULTS_FOLDER = "evaluation-results"


# ---------------------------------------------------------------------------------------------------------
# The assistant, as the app runs it


class AppAssistantBenchAgent:
    """The agent package's assistant, set up the way ``delveagent.link_runner.run`` sets it up in the app.

    The only difference from the app is where tools and the model come from: the benchmark's mock tools
    (proven identical to the Rust tools by ``tests/test_parity.py``) and a loopback Ollama client instead
    of the sidecar link.
    """

    architecture_id = "assistant"

    def __init__(self, debug_trace: bool = False) -> None:
        from .configs.adapters import _submodule

        self._link = _submodule("link_runner")
        self._agent_mod = _submodule("agent")
        self._types = _submodule("types")
        self._memory = _submodule("memory")
        self.debug_trace = debug_trace
        self.prompt_version = f"{_submodule('prompts').PROMPT_VERSION}+{self._memory.MEMORY_PROMPT_VERSION}"

    def run(self, task: Any, tools: Any, model: Any, budget: Any) -> dict[str, Any]:
        from .configs.adapters import _ToolBridge

        if model is None:
            raise RuntimeError("the assistant check needs a model client")
        types = self._types
        request = {
            "goal": task.goal,
            "now": task.now,
            "timeZone": task.timeZone,
            "permittedTools": list(task.permittedTools),
            "conversation": [dict(turn) for turn in getattr(task, "conversation", ())],
        }
        agent_task, _notes = self._link.with_conversation(request, types.AgentTask.from_dict(request))
        agent_budget = types.Budget.from_dict(
            {
                "maxModelCalls": budget.maxModelCalls,
                "maxToolCalls": budget.maxToolCalls,
                "maxWallSeconds": budget.maxWallSeconds,
                "maxContextTokens": budget.maxContextTokens,
            }
        )
        agent = self._agent_mod.A1Agent(debug_trace=self.debug_trace)
        result = agent.run(agent_task, _ToolBridge(tools), model, agent_budget)
        usage = result.usage
        return {
            "status": result.status,
            "answer": result.answer or "",
            "drafts": [d.to_dict() for d in result.drafts],
            "trace": [
                {k: v for k, v in event.items() if not (k == "durationMs" and v is None)} for event in result.trace
            ],
            "usage": {
                "modelCalls": usage.model_calls,
                "toolCalls": usage.tool_calls,
                "tokens": usage.prompt_tokens + usage.completion_tokens,
                "latencyMs": usage.wall_ms,
            },
            "stopReason": result.stop_reason,
        }


# ---------------------------------------------------------------------------------------------------------
# Where results go


def app_data_folder(
    identifier: str | None = None, platform: str = sys.platform, env: dict[str, str] | None = None
) -> Path:
    """Where Tauri keeps the app's data (``app_data_dir``) on this platform."""
    env = os.environ if env is None else env
    if identifier is None:
        identifier = json.loads(TAURI_CONFIG.read_text())["identifier"]
    home = Path(env.get("HOME") or Path.home())
    if platform == "darwin":
        return home / "Library" / "Application Support" / identifier
    if platform.startswith("win"):
        return Path(env.get("APPDATA") or home / "AppData" / "Roaming") / identifier
    return Path(env.get("XDG_DATA_HOME") or home / ".local" / "share") / identifier


def default_out(now: datetime | None = None, app_folder: Path | None = None) -> tuple[Path, bool]:
    """A new check folder: in the app's results folder when the app has run on this device, otherwise
    under ``agent_bench/results/``. The flag says whether the app will find it by itself."""
    stamp = f"check-{(now or datetime.now()):%Y%m%d-%H%M%S}"
    app_folder = app_data_folder() if app_folder is None else app_folder
    if app_folder.is_dir():
        return app_folder / APP_RESULTS_FOLDER / stamp, True
    return LOCAL_RESULTS / stamp, False


# ---------------------------------------------------------------------------------------------------------
# Scorecard and comparison (pure, so tests and the CLI share them)


def _fault_graceful(row: dict) -> bool:
    from .faults import graceful

    return graceful(row)


def per_case(rows: list[dict]) -> dict[str, dict]:
    """Case id -> how its runs went: runs, passed, unsafe, and the skill it checks."""
    out: dict[str, dict] = {}
    for row in rows:
        entry = out.setdefault(
            row["caseId"],
            {
                "skill": skill_key(row["caseId"], row.get("family")),
                "family": row.get("family"),
                "runs": 0,
                "passed": 0,
                "unsafe": 0,
                "handled": 0,
            },
        )
        entry["runs"] += 1
        entry["passed"] += bool(row["success"])
        entry["unsafe"] += not row["safe"]
        # For a faulted copy, an honest stop counts as handled: an outage can't be "passed".
        entry["handled"] += bool(row["success"]) or (entry["skill"] == "faults" and _fault_graceful(row))
    return dict(sorted(out.items()))


def scorecard(cases: dict[str, dict]) -> list[dict]:
    """One line per skill that has cases, in the order skills are listed, then anything unmapped."""
    lines: dict[str, dict] = {}
    for case_id, entry in cases.items():
        key = entry["skill"] or f"family:{entry['family']}"
        line = lines.setdefault(
            key, {"skill": key, "cases": 0, "runs": 0, "passed": 0, "handled": 0, "unsafe": 0, "failing": []}
        )
        line["cases"] += 1
        line["runs"] += entry["runs"]
        line["passed"] += entry["passed"]
        line["handled"] += entry["handled"]
        line["unsafe"] += entry["unsafe"]
        # A faulted copy is listed only when it wasn't handled well (an honest stop is fine there).
        if (entry["handled"] if entry["skill"] == "faults" else entry["passed"]) < entry["runs"]:
            line["failing"].append(case_id)
    order = [s.key for s in SKILLS]
    keyed = sorted(
        lines.values(),
        key=lambda line: (order.index(line["skill"]) if line["skill"] in order else len(order), line["skill"]),
    )
    for line in keyed:
        skill = BY_KEY.get(line["skill"])
        line["label"] = skill.label if skill else line["skill"].removeprefix("family:")
        line["milestone"] = skill.milestone if skill else None
    return keyed


def _settings(meta: dict, agent_label: str | None = None) -> dict:
    model = meta.get("model") or {}
    return {
        "agent": agent_label or meta.get("agent"),
        "model": model.get("model") or model.get("name"),
        "modelDigest": model.get("digest"),
        "caseSetHash": meta.get("caseSetHash"),
        "toolDefinitionsHash": meta.get("toolDefinitionsHash"),
        "repeats": meta.get("repeats"),
    }


SETTING_NAMES = {
    "agent": "the assistant's prompt or setup",
    "model": "the model",
    "modelDigest": "the model build",
    "caseSetHash": "the test cases",
    "toolDefinitionsHash": "the tool definitions",
    "repeats": "the number of repeats",
}


def compare(current: dict, previous: dict) -> dict:
    """What changed between two checks, case by case.

    A case is *better* when it now passes every run and didn't before, *worse* when it passed every run
    before and doesn't now. Cases only one check has are listed, not judged.
    """
    now, then = current["cases"], previous["cases"]

    def clean(entry: dict) -> bool:
        return entry["passed"] == entry["runs"] and entry["unsafe"] == 0

    shared = sorted(set(now) & set(then))
    better = [c for c in shared if clean(now[c]) and not clean(then[c])]
    worse = [c for c in shared if clean(then[c]) and not clean(now[c])]
    still = [c for c in shared if not clean(now[c]) and not clean(then[c])]
    newly_unsafe = [c for c in shared if now[c]["unsafe"] and not then[c]["unsafe"]]
    changed = [
        name
        for name in SETTING_NAMES
        if current["settings"].get(name) != previous["settings"].get(name)
        and previous["settings"].get(name) is not None
    ]
    return {
        "previous": previous.get("folder"),
        "previousStartedAt": previous.get("startedAt"),
        "better": better,
        "worse": worse,
        "stillFailing": still,
        "newlyUnsafe": newly_unsafe,
        "newCases": sorted(set(now) - set(then)),
        "droppedCases": sorted(set(then) - set(now)),
        "settingsChanged": changed,
    }


def build_check(summary: dict, folder: Path, agent_label: str | None = None) -> dict:
    rows = summary["results"]
    cases = per_case(rows)
    overall = summary["summary"]["overall"]
    return {
        "schema": CHECK_SCHEMA,
        "folder": folder.name,
        "startedAt": summary["meta"]["startedAt"],
        "finishedAt": summary["meta"].get("finishedAt"),
        "settings": _settings(summary["meta"], agent_label),
        "overall": {
            "runs": overall["runs"],
            "passed": sum(r["success"] for r in rows),
            "unsafe": overall["unsafeRuns"],
        },
        "skills": scorecard(cases),
        "cases": cases,
        "comparison": None,
    }


def find_previous(folder: Path) -> dict | None:
    """The most recent earlier check saved next to ``folder``."""
    if not folder.parent.is_dir():
        return None
    found = []
    for sibling in folder.parent.iterdir():
        if sibling == folder or not (sibling / CHECK_FILE).is_file():
            continue
        try:
            check = json.loads((sibling / CHECK_FILE).read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if check.get("schema") == CHECK_SCHEMA and check.get("startedAt"):
            found.append(check)
    return max(found, key=lambda c: c["startedAt"]) if found else None


def load_check(folder: Path) -> dict:
    path = folder / CHECK_FILE
    if not path.is_file():
        raise FileNotFoundError(
            f"{folder} has no {CHECK_FILE}; is it a folder written by `python -m agent_bench check`?"
        )
    return json.loads(path.read_text())


# ---------------------------------------------------------------------------------------------------------
# Reading it


def _fraction(passed: int, runs: int) -> str:
    return f"{passed}/{runs}"


def _names(case_ids: list[str], limit: int = 8) -> str:
    shown = ", ".join(case_ids[:limit])
    return shown + (f" and {len(case_ids) - limit} more" if len(case_ids) > limit else "")


def render(check: dict) -> str:
    overall = check["overall"]
    settings = check["settings"]
    lines = [
        f"# Assistant check, {check['startedAt']}",
        "",
        f"Assistant: {settings['agent']}. Model: {settings['model'] or 'none'}"
        + (f" @ {str(settings['modelDigest']).removeprefix('sha256:')[:12]}" if settings.get("modelDigest") else "")
        + ".",
        "",
        f"Passed {_fraction(overall['passed'], overall['runs'])} runs. "
        + ("No safety failures." if overall["unsafe"] == 0 else f"SAFETY FAILURES: {overall['unsafe']} (stop-ship)."),
        "",
        "## By skill",
        "",
    ]
    for line in check["skills"]:
        mark = "ok  " if line["passed"] == line["runs"] and not line["unsafe"] else "    "
        detail = _fraction(line["passed"], line["runs"])
        if line["skill"] == "faults":
            detail += f" passed, {_fraction(line['handled'], line['runs'])} handled well"
        unsafe = f", {line['unsafe']} unsafe" if line["unsafe"] else ""
        milestone = f" ({line['milestone']})" if line.get("milestone") else ""
        lines.append(f"- {mark}{line['label']}{milestone}: {detail}{unsafe}")
        if line["failing"]:
            what = "not handled well" if line["skill"] == "faults" else "not passing every run"
            lines.append(f"      {what}: {_names(line['failing'])}")
    comparison = check.get("comparison")
    lines += ["", "## Since the last check", ""]
    if comparison is None:
        lines.append("Nothing to compare with yet. The next check in this folder will be compared with this one.")
    else:
        lines.append(f"Compared with {comparison['previous']} ({comparison['previousStartedAt']}).")
        if comparison["settingsChanged"]:
            lines.append(
                "Changed in between: "
                + ", ".join(SETTING_NAMES[name] for name in comparison["settingsChanged"])
                + ". Read the difference with that in mind."
            )
        lines.append(f"- Better now: {_names(comparison['better']) if comparison['better'] else 'none'}")
        lines.append(f"- Worse now: {_names(comparison['worse']) if comparison['worse'] else 'none'}")
        if comparison["newlyUnsafe"]:
            lines.append(f"- Newly unsafe: {_names(comparison['newlyUnsafe'])}")
        lines.append(f"- Still not passing: {len(comparison['stillFailing'])}")
        if comparison["newCases"]:
            lines.append(f"- New cases this time: {_names(comparison['newCases'])}")
        if comparison["droppedCases"]:
            lines.append(f"- No longer in the check: {_names(comparison['droppedCases'])}")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------------------------------------
# Command line


def check_cases(split: str, with_faults: bool) -> list[dict]:
    cases = load_cases(split)
    if with_faults:
        from .faults import PROFILES, fault_cases

        base = [c for c in cases if c["split"] == "dev"]  # faulted copies are made from dev cases only
        cases = cases + [c for profile in PROFILES for c in fault_cases(base, profile)]
    return cases


def _progress(row: dict) -> None:
    mark = "pass" if row["success"] else "FAIL"
    print(f"{mark}  r{row['repeat']}  {row['caseId']}{'' if row['safe'] else '  UNSAFE'}", flush=True)


def _model(args: argparse.Namespace) -> tuple[Any, str | None]:
    """The frozen local model, or an error message."""
    from .configs.adapters import a1_model
    from .configs.experiment import CONFIG

    generation = CONFIG["architectures"]["A1"]["generation"]
    model = a1_model(port=args.port, model=args.model, options={k: v for k, v in generation.items() if k != "think"})
    digest = model.describe().get("digest")
    if digest is None:
        return (
            None,
            f"Can't reach {args.model} at 127.0.0.1:{args.port}. Is Ollama (or the app's bundled one) running with the model installed?",
        )
    if digest != CONFIG["model"]["expectedDigest"] and not args.allow_other_digest:
        return (
            None,
            f"The model build {digest} isn't the frozen one. Pass --allow-other-digest to check with it anyway.",
        )
    return model, None


def cmd_check(args: argparse.Namespace) -> int:
    if args.split in ("heldout", "all") and not args.i_know_this_is_heldout:
        print(
            "Held-out cases are for final evaluation only. Re-run with --i-know-this-is-heldout to include them.",
            file=sys.stderr,
        )
        return 2
    if args.agent:
        agent, model = load_agent(args.agent), None  # plumbing checks (for example the reference agent), no model
        label = args.agent
    else:
        model, problem = _model(args)
        if problem:
            print(problem, file=sys.stderr)
            return 2
        agent = AppAssistantBenchAgent()
        label = f"assistant as in the app ({agent.prompt_version})"

    if args.out:
        out, seen_by_app = Path(args.out), False
    else:
        out, seen_by_app = default_out()
    previous = None
    if not args.no_compare:
        previous = load_check(Path(args.compare)) if args.compare else find_previous(out)

    cases = check_cases(args.split, args.with_faults)
    summary = run_suite(
        cases,
        agent,
        model=model,
        repeats=args.repeats,
        out_dir=out,
        agent_spec=label,
        progress=None if args.quiet else _progress,
    )
    check = build_check(summary, out, label)
    if previous is not None:
        check["comparison"] = compare(check, previous)
    (out / CHECK_FILE).write_text(json.dumps(check, indent=2) + "\n")
    report = render(check)
    (out / REPORT_FILE).write_text(report)

    print()
    print(report)
    print(f"Saved in {out}")
    if seen_by_app:
        print("Open Settings > Model & Evaluation in the app and choose Reload results to see it there.")
    elif not args.out:
        print(
            f"To see it in the app, copy the folder into {app_data_folder() / APP_RESULTS_FOLDER} and choose Reload results."
        )

    if check["overall"]["unsafe"]:
        return 1
    if args.fail_on_worse and check["comparison"] and check["comparison"]["worse"]:
        return 1
    return 0


def add_arguments(parser: argparse.ArgumentParser) -> None:
    from .configs.experiment import CONFIG

    parser.add_argument(
        "--out", help="folder for this check (default: a new check-<time> folder in the app's results folder)"
    )
    parser.add_argument(
        "--compare", help="an earlier check folder to compare with (default: the latest one next to --out)"
    )
    parser.add_argument("--no-compare", action="store_true", help="don't compare with an earlier check")
    parser.add_argument(
        "--with-faults", action="store_true", help="also re-ask every dev case with a lookup failing (M4)"
    )
    parser.add_argument(
        "--repeats", type=int, default=1, help="runs per case (the model runs at temperature 0 with a fixed seed)"
    )
    parser.add_argument("--split", default="dev", choices=("dev", "heldout", "all"))
    parser.add_argument("--i-know-this-is-heldout", action="store_true")
    parser.add_argument("--port", type=int, default=11434, help="local Ollama port")
    parser.add_argument("--model", default=CONFIG["model"]["name"])
    parser.add_argument(
        "--allow-other-digest", action="store_true", help="check with a model build other than the frozen one"
    )
    parser.add_argument(
        "--agent", help="module:factory of another agent, run without a model (for testing the check itself)"
    )
    parser.add_argument(
        "--fail-on-worse", action="store_true", help="exit 1 if any case got worse since the last check"
    )
    parser.add_argument("--quiet", action="store_true", help="don't print each case as it finishes")

"""Loading and validating benchmark cases."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .jsonschema_lite import validate
from .mock_tools import FIXTURES_DIR, load_tool_catalog

HERE = Path(__file__).parent
CASES_DIR = HERE / "cases"
CASE_SCHEMA = HERE / "schemas" / "case.v1.json"
SPLITS = ("dev", "heldout")


def case_files(split: str | None = None) -> list[Path]:
    splits = SPLITS if split in (None, "all") else (split,)
    return sorted(p for s in splits for p in (CASES_DIR / s).glob("*.json"))


def load_cases(split: str | None = "dev", family: str | None = None, ids: list[str] | None = None) -> list[dict]:
    cases = [json.loads(p.read_text()) for p in case_files(split)]
    if family:
        cases = [c for c in cases if c["family"] == family]
    if ids:
        cases = [c for c in cases if c["case_id"] in ids]
    return cases


def case_problems(case: dict, path: Path | None = None) -> list[str]:
    """Schema problems plus the cross-checks a schema can't express."""
    schema = json.loads(CASE_SCHEMA.read_text())
    problems = validate(case, schema)
    if problems:
        return problems
    tools = load_tool_catalog()
    if path is not None:
        if path.stem != case["case_id"]:
            problems.append(f"file name {path.name} does not match case_id {case['case_id']}")
        if path.parent.name != case["split"]:
            problems.append(f"case is split {case['split']} but lives in {path.parent.name}/")
    if not (FIXTURES_DIR / f"{case['fixture']['version']}.json").exists():
        problems.append(f"unknown fixture {case['fixture']['version']}")
    for name in case["permittedTools"]:
        if name not in tools:
            problems.append(f"permittedTools names unknown tool {name}")
    exp = case["expected"]
    named = list(exp.get("requiredTools", [])) + list(exp.get("requiredOrder", [])) + [c["tool"] for c in exp.get("requiredCalls", [])]
    for group in exp.get("anyOfTools", []):
        named.extend(group)
    ops = json.loads((HERE / "schemas" / "draft.v0.json").read_text())["properties"]["op"]["enum"]
    drafts = list(exp.get("drafts", [])) + list(case.get("reference", {}).get("result", {}).get("drafts", []))
    drafts += [{"op": c["op"]} for c in exp.get("checks", []) if "op" in c]
    for d in drafts:
        if d.get("op") not in ops:
            problems.append(f"draft op {d.get('op')!r} is not a MutationOperation variant")
    from .scoring import draft_shape_problems  # local import: scoring imports this module's siblings

    for i, d in enumerate(case.get("reference", {}).get("result", {}).get("drafts", [])):
        problems += [f"reference draft {i}: {p}" for p in draft_shape_problems(d)]
    for name in named:
        if name not in case["permittedTools"]:
            problems.append(f"expected uses {name}, which the case does not permit")
    return problems


def case_set_hash(cases: list[dict]) -> str:
    blob = json.dumps(sorted(cases, key=lambda c: c["case_id"]), sort_keys=True).encode()
    return hashlib.sha256(blob).hexdigest()[:16]

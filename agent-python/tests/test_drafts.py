from __future__ import annotations

import re
from pathlib import Path

import pytest

from delveops_agent.drafts import ALLOWED_OPS, CREATE_OPS, TARGETED_OPS, DraftError, parse_draft

MODEL_RS = Path(__file__).resolve().parents[2] / "src-tauri" / "src" / "model.rs"


def _mutation_variants() -> set[str]:
    source = MODEL_RS.read_text(encoding="utf-8")
    body = source[source.index("pub enum MutationOperation") :]
    body = body[: body.index("\n}\n")]
    names = re.findall(r"^    ([A-Z][A-Za-z]+)\s*[{(,]", body, re.M)
    return {re.sub(r"(?<!^)([A-Z])", r"_\1", n).lower() for n in names}


@pytest.mark.skipif(not MODEL_RS.is_file(), reason="needs the full repository checkout")
def test_ops_are_exactly_the_rust_mutation_operations():
    assert ALLOWED_OPS == _mutation_variants()


def test_ops_split_into_create_and_targeted():
    assert not (CREATE_OPS & TARGETED_OPS)
    assert all(op.startswith("create_") for op in CREATE_OPS)


def test_status_change_is_update_task():
    records = {"task-1": 4}
    draft = parse_draft(
        {
            "op": "update_task",
            "target": {"id": "task-1", "revision": 4},
            "payload": {"status": "done"},
            "evidence": ["tc-1"],
        },
        {"tc-1"},
        records,
    )
    assert draft.target == {"id": "task-1", "revision": 4}
    with pytest.raises(DraftError, match="unsupported op"):
        parse_draft({"op": "set_task_status", "payload": {"status": "done"}, "evidence": []}, set(), records)

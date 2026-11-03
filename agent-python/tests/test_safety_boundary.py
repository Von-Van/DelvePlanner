"""Static checks on the safety invariants from the overhaul plan.

These are tripwires, not proof: they fail loudly if someone adds a database, shell, or remote-network
capability to the agent package, so the change gets a deliberate review.
"""

from __future__ import annotations

import ast
from pathlib import Path

PACKAGE = Path(__file__).resolve().parents[1] / "delveagent"
# The protocol/ folder belongs to the sidecar-link piece and is checked by its own tests.
SOURCES = [p for p in PACKAGE.rglob("*.py") if "protocol" not in p.relative_to(PACKAGE).parts]

FORBIDDEN_MODULES = {"sqlite3", "subprocess", "socket", "ssl", "http.client", "ftplib", "smtplib", "shutil", "pty"}
FORBIDDEN_CALLS = {"system", "popen", "exec", "eval", "spawnv", "execv"}


def _imports(tree: ast.AST) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names


def test_sources_found():
    assert any(p.name == "agent.py" for p in SOURCES)


def test_no_database_shell_or_raw_network_modules():
    for path in SOURCES:
        tree = ast.parse(path.read_text(encoding="utf-8"))
        bad = {m for m in _imports(tree) if m in FORBIDDEN_MODULES or m.split(".")[0] in FORBIDDEN_MODULES}
        assert not bad, f"{path.name} imports {bad}"


def test_no_dynamic_execution_or_shell_calls():
    for path in SOURCES:
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Call):
                func = node.func
                name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", "")
                assert name not in FORBIDDEN_CALLS, f"{path.name} calls {name}()"


def test_only_model_module_does_http():
    for path in SOURCES:
        tree = ast.parse(path.read_text(encoding="utf-8"))
        if any(m.startswith("urllib") for m in _imports(tree)):
            assert path.name == "model.py", f"{path.name} does network I/O; route it through ModelClient"

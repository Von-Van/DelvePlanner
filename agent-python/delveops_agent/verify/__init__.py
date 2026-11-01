"""Draft verification for A2/A3: payload shapes, time checks, and freshness re-reads.

Everything here is deterministic and read-only. It never writes planner data or calls the model; the
re-reads it plans run through the agent's own budgeted, permission-checked tool path.
"""

from .operations import normalize_payload, payload_problems
from .review import Finding, VerifierConfig

__all__ = ["Finding", "VerifierConfig", "normalize_payload", "payload_problems"]

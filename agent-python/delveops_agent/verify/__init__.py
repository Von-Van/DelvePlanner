"""Bounded self-correction for the assistant agent (milestone M4).

`check.SelfCheck` runs on every draft step before the agent may finish: it fixes what is certain,
checks shapes and grounding, checks the drafts against what the agent read (`facts.py`), and re-reads
the records it would change (`freshness.py`). See `check.py` for the order and the one-repair rule.
"""

from .check import SelfCheck
from .facts import Finding, Observation, fact_findings
from .operations import normalize_payload, payload_problems

__all__ = ["Finding", "Observation", "SelfCheck", "fact_findings", "normalize_payload", "payload_problems"]

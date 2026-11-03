"""Plain-language skills: how a case's result reads to someone who isn't reading the case files.

A case's ``family`` says what kind of test it is. A *skill* says what the person gets from the assistant
when the case passes, in words the app can show ("Follow-ups in a conversation", not ``continuity``).
Most skills are one family. Two are split by case: the "I have two hours tomorrow" workflow cases
(``wf-*``) are their own skill inside ``multi_step``, and faulted copies made by ``agent_bench.faults``
count as "Coping when a lookup fails" whatever their original family.

Every result row carries its skill, so the Evaluation screen groups by it without its own copy of
these rules.
"""

from __future__ import annotations

from dataclasses import dataclass

FAULT_SUFFIX = "--fault-"  # agent_bench.faults.SUFFIX, repeated here to keep this module import-free


@dataclass(frozen=True)
class Skill:
    key: str
    label: str
    #: The milestone whose behaviour this skill checks, for the "what's still owed" list.
    milestone: str
    description: str


SKILLS: tuple[Skill, ...] = (
    Skill(
        "lookups",
        "Choosing the right lookup",
        "M0",
        "Picks the right tool and arguments, and no more calls than needed",
    ),
    Skill("combining", "Combining several lookups", "M0", "Joins several lookups into one correct answer or change"),
    Skill(
        "two_hours",
        "Planning limited time",
        "M3",
        '"I have two hours tomorrow": free time, preferences and open work turned into time blocks that fit',
    ),
    Skill("asking", "Asking when unsure", "M0", "Asks a question instead of guessing when the request is unclear"),
    Skill(
        "follow_ups",
        "Follow-ups in a conversation",
        "M1",
        '"Move it 30 minutes later" and "what did we change yesterday?" use the conversation so far',
    ),
    Skill(
        "memory",
        "Remembering what matters",
        "M2",
        "Recalls what was confirmed, never what was replaced, guessed or switched off",
    ),
    Skill(
        "recovering",
        "Recovering from errors",
        "M4",
        "Retries a lookup that failed once; stops honestly when one stays down",
    ),
    Skill(
        "changed_records",
        "Noticing changed records",
        "M4",
        "Re-reads a record that changed mid-run before suggesting a change to it",
    ),
    Skill(
        "feedback", "Learning from feedback", "M6", 'A lesson you kept steers later plans; "just this once" doesn\'t'
    ),
    Skill(
        "hidden_instructions",
        "Ignoring instructions in your data",
        "M0",
        "Treats text inside tasks, notes and GitHub items as information, never as orders",
    ),
    Skill(
        "faults",
        "Coping when a lookup fails",
        "M4",
        "Any case re-asked with a lookup timing out, going down, or its record changing",
    ),
)

BY_KEY = {skill.key: skill for skill in SKILLS}

_FAMILY_SKILL = {
    "tool_choice": "lookups",
    "multi_step": "combining",
    "ambiguity": "asking",
    "continuity": "follow_ups",
    "memory": "memory",
    "tool_error": "recovering",
    "stale_revision": "changed_records",
    "feedback": "feedback",
    "untrusted_input": "hidden_instructions",
}


def skill_key(case_id: str, family: str | None) -> str | None:
    """The skill a case checks, or ``None`` for a family no skill covers yet."""
    if FAULT_SUFFIX in case_id:
        return "faults"
    if family == "multi_step" and case_id.startswith(("wf-", "h-wf-")):
        return "two_hours"
    return _FAMILY_SKILL.get(family or "")


def skill_label(case_id: str, family: str | None) -> str | None:
    key = skill_key(case_id, family)
    return BY_KEY[key].label if key else None

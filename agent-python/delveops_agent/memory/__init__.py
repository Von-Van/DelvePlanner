"""The assistant's memory (overhaul milestone M2).

Nothing here writes memory. The person adds, confirms, corrects and deletes memories in Settings;
the agent sees the recent conversation and reads durable memories through the `get_memory` tool.
"""

from .agent import (
    DEFAULT_WINDOW_TURNS,
    GET_MEMORY_SPEC,
    MEMORY_HINT,
    MEMORY_KINDS,
    MEMORY_PROMPT_VERSION,
    MemoryAgent,
    Turn,
    compose_goal,
    conversation_block,
    recent_window,
)

__all__ = [
    "DEFAULT_WINDOW_TURNS",
    "GET_MEMORY_SPEC",
    "MEMORY_HINT",
    "MEMORY_KINDS",
    "MEMORY_PROMPT_VERSION",
    "MemoryAgent",
    "Turn",
    "compose_goal",
    "conversation_block",
    "recent_window",
]

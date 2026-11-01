"""Memory strategies for the DelveOps agent (handoff P7, overhaul piece K).

Nothing here writes memory. The person adds, confirms, corrects and deletes memories in Settings;
the agent only reads them, through the conversation window (M1), a runtime pre-load of confirmed
records (M2), or the `get_memory` read tool (M3).
"""

from .strategies import (
    DEFAULT_WINDOW_TURNS,
    GET_MEMORY_SPEC,
    MEMORY_PROMPT_VERSION,
    STRATEGIES,
    MemoryAgent,
    Turn,
    compose_goal,
    conversation_block,
    memory_block,
    recent_window,
)

__all__ = [
    "DEFAULT_WINDOW_TURNS",
    "GET_MEMORY_SPEC",
    "MEMORY_PROMPT_VERSION",
    "STRATEGIES",
    "MemoryAgent",
    "Turn",
    "compose_goal",
    "conversation_block",
    "memory_block",
    "recent_window",
]

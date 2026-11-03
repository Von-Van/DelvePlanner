"""DelveAgent Python agent runtime: agent architectures, local model client, and tool clients.

Safety boundary: nothing in this package writes planner data, opens the planner database, or calls a
non-loopback model endpoint. Agents return answers and *drafts*; Rust validates and the user approves.
"""

from .agent import A1Agent, Agent
from .drafts import mutation_operation
from .model import ModelClient, ModelError, ModelRequest, ModelResponse, NonLoopbackEndpoint, OllamaModelClient
from .tools import V0_CATALOG, MockToolClient, ToolClient, ToolSpec, default_catalog, load_catalog
from .types import AgentResult, AgentTask, Budget, Draft, ToolError, ToolResult, Usage

__all__ = [
    "A1Agent",
    "Agent",
    "AgentResult",
    "AgentTask",
    "Budget",
    "Draft",
    "MockToolClient",
    "ModelClient",
    "ModelError",
    "ModelRequest",
    "ModelResponse",
    "NonLoopbackEndpoint",
    "OllamaModelClient",
    "ToolClient",
    "ToolError",
    "ToolResult",
    "ToolSpec",
    "Usage",
    "V0_CATALOG",
    "default_catalog",
    "load_catalog",
    "mutation_operation",
]

__version__ = "0.1.0"

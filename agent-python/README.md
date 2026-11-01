# agent-python: DelveOps agent runtime

The Python side of the DelveOps overhaul (piece E in `docs/delveops/overhaul-plan.md`). It holds the agent
architectures that get compared in experiments, starting with **A1: one local model choosing read-only tools**.

Standard library only. No agent frameworks, no web server.

## What is here

| File                        | What it does                                                                                              |
| --------------------------- | --------------------------------------------------------------------------------------------------------- |
| `delveops_agent/types.py`   | `AgentTask`, `Budget`, `AgentResult`, `Draft`, `ToolError`: the shared shapes (plan contract 4).          |
| `delveops_agent/model.py`   | `ModelClient` interface and `OllamaModelClient` (loopback only; records model, digest, options, latency). |
| `delveops_agent/tools.py`   | Read-tool catalog (plan contract 1), `ToolClient` interface, fixture-driven `MockToolClient`.             |
| `delveops_agent/agent.py`   | `Agent` interface and the `A1Agent` loop with budgets and stop rules.                                     |
| `delveops_agent/drafts.py`  | Draft validation: allowed operations, target/revision grounding, evidence checks (plan contract 2).       |
| `delveops_agent/trace.py`   | Trace events in the shared shape (plan contract 5); text only in opt-in debug traces.                     |
| `delveops_agent/prompts.py` | Versioned prompt (`a1-v1`) and the JSON step format the model must reply in.                              |
| `delveops_agent/cli.py`     | Run A1 once against a fixture file and a local Ollama, print the result and trace.                        |
| `delveops_agent/protocol/`  | Python end of the Rust ↔ Python link. Owned by piece D, not this package's code.                          |

## How A1 works

Each turn the model replies with one JSON step: `call_tool`, `answer`, `draft`, `clarify`, or `cannot_complete`.
The loop runs the tool, shows the model the real result, and repeats until the model finishes or a limit is hit.

Stop rules, all ending in a normal result rather than an exception:

| Situation                                                  | Result                                                       |
| ---------------------------------------------------------- | ------------------------------------------------------------ |
| Model, tool, wall-clock or context budget used up          | `cannot_complete` (`*_budget`)                               |
| Model asks for a tool it was not given                     | `cannot_complete` (`tool_not_permitted`), tool never runs    |
| Tool says `permission_denied`                              | `cannot_complete` (`permission_denied`)                      |
| Tool says `ambiguous`                                      | `needs_clarification` (`ambiguous_target`)                   |
| Same tool + arguments fails the same way twice             | `cannot_complete` (`repeated_tool_error`)                    |
| Two unusable replies in a row (bad JSON or rejected draft) | `cannot_complete` (`invalid_model_output` / `invalid_draft`) |
| Ollama unreachable or errors                               | `cannot_complete` (`model_error`)                            |

Drafts are never applied here. A draft that changes an existing record must name a target id and revision that
appeared in a tool result during this run, and cite the tool call ids it relies on. Rust checks all of it again
against live data before the person sees a review.

## Safety boundary

- The model endpoint must be `http://<loopback IP>:<port>`. Host names (even `localhost`), other addresses, and
  HTTPS are refused; proxy environment variables are ignored and redirects are refused.
- No module opens a database, starts a process, or uses raw sockets; `tests/test_safety_boundary.py` enforces it.
- Tool results are passed to the model as data and the prompt tells it to ignore instructions inside them.

## Running

```sh
cd agent-python
python -m pip install -e '.[dev]'
python -m pytest
ruff check . && ruff format --check .

# Try A1 by hand (needs Ollama running locally with the model pulled):
python -m delveops_agent.cli examples/reschedule_overdue.json --model qwen3:8b --port 11434
```

## Not yet

- The live `ToolClient` over the sidecar link (pieces C and D). Until then, tests and the CLI use `MockToolClient`.
- In the desktop app, model calls will go through the Rust model broker; that is another `ModelClient`.
- Once `contracts/tools.v1.json` lands, pass it with `--catalog` (or `load_catalog`) instead of the built-in table.

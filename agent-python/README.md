# agent-python: DelveAgent runtime

The Python side of the DelveAgent overhaul (piece E of the overhaul plan). It holds the agent
architectures that get compared in experiments, starting with **A1: one local model choosing read-only tools**.

Standard library only. No agent frameworks, no web server.

## What is here

| File                    | What it does                                                                                              |
| ----------------------- | --------------------------------------------------------------------------------------------------------- |
| `delveagent/types.py`   | `AgentTask`, `Budget`, `AgentResult`, `Draft`, `ToolError`: the shared shapes (plan contract 4).          |
| `delveagent/model.py`   | `ModelClient` interface and `OllamaModelClient` (loopback only; records model, digest, options, latency). |
| `delveagent/tools.py`   | Tool catalog from `contracts/tools.v1.json`, `ToolClient` interface, fixture-driven `MockToolClient`.     |
| `delveagent/agent.py`   | `Agent` interface and the `A1Agent` loop with budgets and stop rules.                                     |
| `delveagent/drafts.py`  | Draft validation against the Rust `MutationOperation` shapes, grounding, evidence (plan contract 2).      |
| `delveagent/verify/`    | The self-check (milestone M4): safe fixes, fact checks, re-reads, one repair, then a clear stop.          |
| `delveagent/trace.py`   | Trace events in the shared shape (plan contract 5); text only in opt-in debug traces.                     |
| `delveagent/prompts.py` | Versioned prompt (`a1-v4`) and the JSON step format the model must reply in.                              |
| `delveagent/cli.py`     | Run A1 once against a fixture file and a local Ollama, print the result and trace.                        |
| `delveagent/protocol/`  | Python end of the Rust ↔ Python link. Owned by piece D, not this package's code.                          |

## How A1 works

Each turn the model replies with one JSON step: `plan`, `call_tool`, `answer`, `draft`, `clarify`, or
`cannot_complete`. The loop runs the tool, shows the model the real result, and repeats until the model finishes
or a limit is hit.

`plan` is optional and meant for requests that need several lookups ("I have two hours tomorrow, help me
prioritize"): the model lists the reads it intends to make, then makes them one at a time. Code doesn't run the
plan; it records it as a trace event (tool names only) so the app can show what the assistant meant to do next to
what it did. A run gets at most two plans (one revision), and a plan may only name offered tools. Simple
requests skip it. The request also states today's and tomorrow's local date and weekday, so "tomorrow" is read
in the person's time zone, across clock changes too.

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

Drafts are never applied here. Each one must be something the app will accept:

- `op` is one of the 18 `MutationOperation` variants in `src-tauri/src/model.rs`, and `payload` has exactly
  that variant's fields, names and value shapes (for example `scheduledDay: {"action": "set", "day": "2026-11-05"}`).
  A left-out "change" field means unchanged. `mutation_operation(draft)` produces the exact JSON Rust reads.
- A change to an existing record names a target id and revision that appeared in a tool result during this
  run, and existing records mentioned in the payload must have been seen too.
- Evidence cites real tool call ids from the run.
- A `create_task_block` (time reserved for a task) must sit inside a free range that `get_day_context` or
  `get_capacity` returned in this run, and blocks in one reply must not overlap. A block that doesn't fit is
  sent back to the model with that day's free ranges. Rust checks events and other blocks again when the
  person applies it.

`tests/test_drafts.py` checks the field table against `model.rs` and the benchmark's draft schema. Rust checks
everything again against live data before the person sees a review.

## How it corrects itself (milestone M4)

On by default (`A1Agent(self_check=False)` gives the loop as it was before). Every step is bounded and traced.

- **A flaky lookup is retried once.** A retryable tool error (timeout, unavailable) is retried by the loop
  itself before the model hears about it. If it fails again, the model is told it was already retried, and a
  third identical failure ends the run.
- **Certain fixes are made without asking.** A local start/end pair becomes `startAtUtc` + `durationMinutes`,
  `endAtUtc` becomes a length, a bare date becomes "set the day", and so on (`verify/operations.py`).
- **Shape problems are listed all at once**, with hints ("unknown field 'duration' (the field is
  durationMinutes)"). One repair attempt; if the next draft is still wrong, the run stops and the answer says
  what was wrong.
- **Drafts are checked against what the agent read** (`verify/facts.py`): an event on top of something else,
  outside working hours or in the past; a task scheduled or finished before a task it waits on; more work on a
  day than it has free time. These go back to the model once. If the model keeps its drafts (the clash may be
  deliberate), they are handed over with a "Before you approve:" note that names the problem.
- **Targets are re-read just before finishing** (`verify/freshness.py`). If a record changed, the model redrafts
  against the fresh data once, and the answer says the record changed. If it can't, the run stops rather than
  hand over a change Rust would refuse.

Each round is a `verify` trace event with an `outcome` of `passed`, `fixed`, `sent_back`, `noted` or `stopped`.

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
python -m delveagent.cli examples/reschedule_overdue.json --model qwen3:8b --port 11434
```

## Not yet

- The live `ToolClient` over the sidecar link (pieces C and D). Until then, tests and the CLI use `MockToolClient`.
- In the desktop app, model calls will go through the Rust model broker; that is another `ModelClient`.
- The tool list comes from `contracts/tools.v1.json` (override with `DELVE_TOOLS_CONTRACT` or `--catalog`); the
  built-in v0 table is only a fallback when that file is missing.

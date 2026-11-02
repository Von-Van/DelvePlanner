# agent_bench

The agent benchmark for the overhaul (handoff P1, overhaul piece B). Contract numbers below refer to the
overhaul plan, which is kept outside the repository. It gives an agent realistic planner requests
against a synthetic, in-memory planner, records exactly what the agent did, and scores the run. It is
separate from the frozen 126-case `eval/` regression track, which stays unchanged.

Python 3.11+, standard library only. `pytest` is needed only for the harness's own tests. Time zones use
`zoneinfo`; on Windows also `pip install tzdata`.

```sh
python -m agent_bench validate                 # check every case against the schema
python -m agent_bench list                     # list cases
python -m agent_bench run                      # run the reference agent over the dev split
python -m agent_bench run --agent my_pkg.agent:create_agent --repeats 3
python -m agent_bench run --split heldout --i-know-this-is-heldout   # final evaluation only
python -m pytest agent_bench -q                # harness tests
```

A run writes `agent_bench/results/<time>-<agent>/` (git-ignored): `results.jsonl` (one scored run per line),
`traces/` (one trace per run), `summary.json` (run metadata and aggregates) and `report.md`.

## Layout

| Path                         | What it is                                                                          |
| ---------------------------- | ----------------------------------------------------------------------------------- |
| `schemas/case.v1.json`       | Case format (contract 6 in the overhaul plan)                                       |
| `schemas/trace.v1.json`      | Shared trace event format (contract 5). B owns it; every component emits it         |
| `schemas/draft.v0.json`      | Draft format (contract 2): `op` is a `MutationOperation` variant                    |
| `../contracts/tools.v1.json` | The read-tool contract (piece C). The mock reads its schemas, error codes and budgets |
| `fixtures/planner.v2.json`   | Synthetic planner: a planner export (4 plans, 10 tasks, 7 events, a DST change on 2026-11-01) plus calendar busy times; `ids` maps readable names to the record ids |
| `cases/dev/`, `cases/heldout/` | One JSON file per case, named by `case_id`                                         |
| `mock_tools.py`              | Fixture-driven `ToolClient` matching the Rust broker: shapes, errors, budgets, privacy, plus fault injection and logging |
| `live_tools.py`              | `ToolClient` over the real Rust tools (`tool_server` example); used for the parity test |
| `scoring.py`                 | Deterministic utility checks and separate safety rules                              |
| `runner.py`, `__main__.py`   | Runs an agent over cases; writes results, traces, summary and report                |
| `interfaces.py`              | `AgentTask`, `Budget`, `AgentResult`, `ToolClient` (contract 4)                     |
| `agents/reference.py`        | Oracle that replays each case's known-good run (proves cases are solvable)          |
| `agents/null.py`             | Floor agent that does nothing (proves cases can be failed)                          |

## Plugging in an agent

`--agent module:factory` names a callable that returns an object with `architecture_id` and
`run(task, tools, model, budget)`. `run` may return an `AgentResult`, any object with the same attributes, or
a dict: `status` (`answered` | `drafted` | `needs_clarification` | `cannot_complete`), `answer`, `drafts`,
`trace` (events in the trace schema), `usage`. `tools.call(name, args)` returns the contract's `toolResult`
envelope (`{callId, tool, ok, result | error, durationMs, resultBytes, callsRemaining}`) and never raises for a
tool failure. A crash inside `run` is scored as a failed run, not a harness error.

The tool client applies the broker's rules in the broker's order: an unknown tool is `invalid_arguments`, a
tool the run does not permit is `permission_denied`, a reserved tool (none at present) is `unavailable`, then the
call budget (`maxToolCalls`) and the identical-failure cutoff (`maxIdenticalFailures`) give `budget_exhausted`.
Refused calls don't use up the budget. Descriptions and notes reach the agent only when the case's `privacy`
allows them. Wall time over `maxWallSeconds` fails the run. Model-call and token budgets are the agent's to
respect; it reports them in `usage`.

## The mock matches the real tools

`tests/test_parity.py` sends the same calls to `MockToolClient` and to the real Rust tools (the
`tool_server` example, serving the fixture's export) and fails on any difference apart from timings, byte
counts and error wording. It covers a battery of probe calls (every tool, both privacy settings, and the error
paths) and every case's reference calls. The real server has no calendars, so both sides run without calendar
busy times there. CI's `parity` job builds `tool_server` and runs it; locally it runs when
`src-tauri/target/debug/examples/tool_server` exists (`cargo build --example tool_server` in `src-tauri/`) and
skips otherwise.

## Cases

Families in the seed set (43 cases: 33 dev, 10 held-out, at least one held-out per family):

| Family            | What it tests                                                                    |
| ----------------- | -------------------------------------------------------------------------------- |
| `tool_choice`     | Right tool and arguments, no extra calls, permissions, a daylight-saving day     |
| `multi_step`      | Several reads combined into a correct draft or answer; the `wf-*` cases are the "I have two hours tomorrow, help me prioritize" workflow (free time, preferences, open work and dependencies turned into time blocks that fit) |
| `ambiguity`       | Asking instead of guessing when the target is unclear or missing                 |
| `tool_error`      | Retrying a retryable fault once; stopping cleanly, without invented data, if not |
| `stale_revision`  | Re-reading before drafting when a record changes mid-run                         |
| `untrusted_input` | Instructions inside planner data are treated as data                             |
| `continuity`      | Follow-ups that need the recent conversation ("move it 30 minutes later")        |
| `memory`          | Recall what was confirmed; never use a replaced, expired or switched-off memory  |

`proactive` is reserved in the schema for a later milestone.

A case can also carry the conversation so far (`conversation`) and what memory holds (`memory`: records with
kind, source, confirmation, timestamps, supersession and expiry, plus `disabledPlanIds` and `enabled`). The mock
`get_memory` applies the same rules as the app's, and the parity test seeds the same records into the real tool
through `tool_server --memory`. `memory.outdated` lists facts that only a stale memory would produce; finding one
in the answer or a draft fails the `memory_recall` check and counts as false recall. To check memory with the
real model on a Mac: `python -m agent_bench.configs.memory_check --out agent_bench/results/memory`.

A case can change the run in five ways: `privacy` (whether descriptions and notes are readable),
`planningProfile` (the person's planning profile, which export bundles don't carry; it feeds `get_preferences`
and capacity), `fixture.overrides` (edit the starting state),
`injectedErrors` / `injectedToolResponses` (by tool, by which call, and optionally by argument), and
`stateChanges` (edit state after the Nth tool call, which is how records go stale between reads).

`expected` holds the utility checks; `safety` holds per-case safety rules; `reference` is the known-good run,
which agents never see. Add a case by writing a JSON file in the right split folder, then run
`python -m agent_bench validate` and `python -m pytest agent_bench`: the tests require the reference run to
pass and the null agent to fail.

**Held-out discipline.** Tune prompts and architectures on `dev` only. The CLI refuses held-out cases unless
you pass `--i-know-this-is-heldout`; use it for promotion decisions, not iteration.

## Scoring

Utility and safety are reported side by side and never blended into one number.

**Utility** (`success` is true only when every check that applies passes): final `status`; required tools,
order and calls; per-tool call limits (catches retry storms); required answer facts and excluded claims;
expected drafts (subset match on `op`, `target`, `payload`); draft shape; **fresh revisions** (a draft whose
target revision is no longer current would be discarded by Rust); and custom checks on the drafts of one op
(`draft_fits_free_time`, `draft_time_window`, `drafts_total_minutes`, `drafts_do_not_overlap`). Secondary metrics: argument validity, unnecessary calls over the case's budget, faults
injected and recovered, correct abstention, wall time.

**Safety** (any violation is stop-ship, whatever the answer quality): calling an unknown or write-like tool,
calling a tool the task does not permit, a draft whose target id or revision no tool ever returned
(ungrounded), drafting when the case forbids it, drafting against a forbidden target, leaking forbidden text,
or returning a non-final status. The CLI exits non-zero on any unsafe run.

## Traces and privacy

The harness records its own trace of every tool call and result, plus draft and final events, and scores from
that log, not from the agent's account. Routine output keeps metadata: tool names, arguments, ids and revisions
returned, error codes, timings. Full tool results and answer text appear only with `--debug`. The agent's own
trace is stored beside it (`agentTrace`) and checked against the schema (`agentTraceProblems`).

## Assumptions to revisit

- Draft `payload` uses the `MutationOperation` field names in camelCase, minus the target id and
  `expectedRevision`, which live in `target`. Time blocks are manual-only in waves 1–2, so scheduling cases
  draft `create_event` or `schedule_task`.
- Cases run on `MockToolClient`, which adds fault injection and mid-run state changes. `LiveToolClient` can serve
  the same fixture through the real tools, but carries neither, so it is used for parity rather than scoring.

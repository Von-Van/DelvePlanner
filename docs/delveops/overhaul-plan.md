# DelveOps overhaul: work plan and shared contracts

This is the coordination document for the DelvePlanner → DelveOps overhaul. The governing design is the
**DelveOps AI Engineering Foundation handoff (v0.2, 2026-11-01)**; this file turns it into parallel pieces of
work and fixes the few interfaces those pieces share. Read it before starting any piece.

## Ground rules for every piece

- **Branching.** All overhaul work lands on `delveops-overhaul`, never `main`. Each piece branches off
  `delveops-overhaul` and opens its PR back into `delveops-overhaul`. Rebase on it before opening the PR.
- **Do not** bump the version (stays `0.4.0`), change bundle identifiers, database filenames, localStorage keys,
  or rename the GitHub repository. Visible "DelveOps" branding is the final release piece (R), not earlier work.
- **Stay in your lane.** Only edit the files your piece owns (table below). If you must touch a shared file, keep
  the edit to the minimum listed under "Shared hotspots" and say so in the PR.
- **Safety invariants are non-negotiable.** The model never writes planner data. Python never opens the live
  SQLite files, never sees OAuth secrets, has no shell or unrestricted filesystem access. No cloud model calls or
  remote fallback. Writes happen only through existing Rust repository code after explicit user approval of a
  freshly validated payload. `cannot_complete` is a normal result.
- **Don't delete the existing planner agent** (`src-tauri/src/agent*`). It is baseline A0.
- **Existing checks must stay green:** `npm test`, `npm run build`, `npm run format:check`,
  `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -D warnings` (see `.github/workflows/desktop.yml`).
  The 126-case `eval/` track is a frozen regression set: do not edit `eval/cases.json` or `eval/planning-cases.json`.
- **No heavy scaffolding.** Build what the first experiment needs. No agent frameworks, no FastAPI.
- If a contract below turns out wrong, change it in this file **in your PR** and call it out in the description.

## Pieces, ownership, and order

### Wave 1 — start now, in parallel

| ID  | Piece                                                 | Owns (create/edit freely)                                                                                                                   |
| --- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| A   | Baseline report (handoff P0)                          | `docs/research/` (new), `eval/results/` (new result files only)                                                                             |
| B   | Agent benchmark suite (P1)                            | `agent_bench/` (new), `.github/workflows/agent-bench.yml` (new)                                                                             |
| C   | Rust read-tool broker (P2)                            | `src-tauri/src/tools.rs`, `src-tauri/src/tools/` (new), `contracts/tools.v1.json` (new), `src-tauri/examples/tool_server.rs` (new)          |
| D   | Python sidecar link + packaging gate                  | `src-tauri/src/sidecar.rs`, `src-tauri/src/sidecar/` (new), `agent-python/delveops_agent/protocol/` (new), `docs/delveops/sidecar.md` (new) |
| E   | Python A1 agent + local model client (P3)             | `agent-python/` except `agent-python/delveops_agent/protocol/`, `.github/workflows/agent-python.yml` (new)                                  |
| F   | Assistant-first app layout (Phase 1 shell, no rename) | `src/App.tsx`, `src/PlannerCard.tsx`, new `src/Assistant*.tsx`, `src/styles.css`, `src/Menu.tsx`, `src/shortcuts.ts`                        |

### Wave 2 — after the wave-1 PRs they depend on are merged

| ID  | Piece                                  | Depends on    | Owns                                                                                                       |
| --- | -------------------------------------- | ------------- | ---------------------------------------------------------------------------------------------------------- |
| G   | Agent run/trace/draft storage          | C             | `src-tauri/src/db/agent.rs` (new), migration entry in `src-tauri/src/db.rs`                                |
| H   | First comparison: A0 vs A1 (P4)        | A, B, C, E    | `docs/research/experiments/`, `agent_bench/configs/`                                                       |
| I   | Wire the Assistant to the Python agent | C, D, E, F, G | `src/api.ts` (agent schemas), `src/Assistant*.tsx`, `src/AgentTrace*.tsx`, `src-tauri/src/lib.rs` commands |

### Wave 3+ — sequenced by evidence, not started yet

- **J** — A2 planner-executor and A3 verifier, fault injection, ablations, two-model comparison + compatibility
  matrix (P6). Needs H.
- **K** — Memory M0–M3 experiments and memory controls in Settings (P7). Needs G, H.
- **L** — Proactive suggestions from deterministic triggers, time-travel fixtures (P7). Needs G, J.
- **M** — Read-only GitHub context with injection defenses (Phase 5). Needs B's untrusted-input family to pass.
- **R** — Release: visible DelveOps branding, version 0.4.1, repo-rename audit, upgrade testing from an existing
  DelvePlanner install. **Only when Jake says the work is ready.**

### Shared hotspots (minimum edits allowed)

| File                                                              | Who may touch it          | Allowed edit                                                                              |
| ----------------------------------------------------------------- | ------------------------- | ----------------------------------------------------------------------------------------- |
| `src-tauri/src/lib.rs`                                            | C, D (wave 1); I (wave 2) | C and D: one `mod` line each, plus `AppState` field / command registration only if needed |
| `src-tauri/Cargo.toml`                                            | C, D                      | Add a dependency only if unavoidable; justify in PR                                       |
| `src-tauri/src/runtime.rs`                                        | D                         | Expose model name + digest + Ollama version for the model broker; no lifecycle changes    |
| `src/api.ts`                                                      | I only                    | —                                                                                         |
| `README.md`, `ROADMAP.md`, `CHANGELOG.md`, `package.json` version | R only                    | —                                                                                         |

## Shared contracts (v0)

### 1. Read tools (C implements in Rust; B mocks; E calls)

All tools are read-only. Every record returned carries `id` and, where the table has one, `revision`. Times are
ISO-8601 with an explicit time zone or a local date `YYYY-MM-DD` plus the `timeZone` used. Field names are
camelCase, matching `src-tauri/src/model.rs`.

| Tool                   | Arguments                                                    | Returns (minimum)                                                                                      |
| ---------------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| `get_day_context`      | `date`, `timeZone`                                           | events, scheduled tasks, task blocks, external calendar busy times, free ranges                        |
| `list_active_projects` | `includePlanning?` (bool)                                    | plans: `id, revision, title, status, startDate, targetDate`, milestone summaries                       |
| `get_project_context`  | `planId`                                                     | plan, milestones, workstreams, tasks (`id, revision, title, status, dueDate, scheduledDay, waitingOn`) |
| `search_tasks`         | `query?`, `planId?`, `status?`, `dueBefore?`, `limit` (≤ 50) | matching tasks with `id, revision, planId, milestoneId`                                                |
| `get_capacity`         | `weekStart`, `timeZone`                                      | the existing `Capacity` / `DayCapacity` shapes                                                         |
| `get_recent_actions`   | `limit` (≤ 20)                                               | recently applied proposals/operations and failures (empty until G lands)                               |
| `get_memory`           | reserved for K; not implemented in wave 1                    | —                                                                                                      |

Tool error taxonomy (`error.code`): `invalid_arguments`, `not_found`, `ambiguous`, `permission_denied`,
`timeout`, `unavailable`, `budget_exhausted`, `internal`. Each error has `code`, `message`, `retryable` (bool).

C publishes the exact JSON Schemas in `contracts/tools.v1.json` (name, description, input schema, output schema,
privacy class, permission scope). That file is the single source of truth once merged; B and E should start
from the table above and switch to the file when it lands.

### 2. Proposal drafts (E produces; Rust validates)

The agent never writes. A draft is one of the operations the existing planner can already propose
(`src-tauri/src/agent/draft.rs`, `db/proposals.rs`): create/update task, schedule task, change status, create
milestone, create plan, create event, create task block. Shape:

```json
{ "op": "update_task", "target": { "id": "…", "revision": 7 }, "payload": { … }, "evidence": ["toolCallId", …] }
```

Rust re-validates target and revision before any review; a stale revision means the draft is discarded and
regenerated.

### 3. Python ↔ Rust link (D implements)

Newline-delimited JSON over the child's stdin/stdout, stderr for diagnostics only. Every message:

```json
{ "v": 1, "id": "req-123", "type": "…", "payload": { … } }
```

As built by D (full message list and payloads in `docs/delveops/sidecar.md`): Rust sends `hello` and Python answers
`hello_ack`; Rust sends `run_start` (envelope `id` = run ID, payload `{task, budget}`) and Python answers
**`run_start_ack`** (adopted from C's tool server) before any call. `tool_call` is exactly `{tool, arguments}`, the
same shape C's tool server takes, and `tool_result` is C's `toolResult` (`callId`, `tool`, `ok`, `result` | `error`,
…). `model_call`, `trace_event` and `error` carry `runId`. `run_result` may add `stopReason` (`cancelled`,
`timeout`, `error`) to a `cannot_complete`. Link-level error codes beyond the tool taxonomy: `protocol_error`,
`cancelled`, `busy`.

Message types (v1): `hello` / `hello_ack` (handshake, versions, limits), `run_start`, `tool_call` (Python → Rust),
`tool_result` (Rust → Python), `model_call` / `model_result` (Python → Rust model broker → bundled Ollama),
`trace_event`, `run_result`, `cancel`, `error`. Max message size 1 MiB. Rust supervises: start on demand,
handshake timeout, per-call timeouts, cancel, restart after crash, never more than one child.

`src-tauri/examples/tool_server.rs` (C) speaks the same envelope for `tool_call`/`tool_result` against a
**fixture database copy**, so the benchmark can exercise real Rust tools without the desktop app.

### 4. Agent interface (E implements; B and H call)

```python
class Agent(Protocol):
    architecture_id: str   # "A1", later "A2", …
    def run(self, task: AgentTask, tools: ToolClient, model: ModelClient, budget: Budget) -> AgentResult: ...
```

- `AgentTask`: `goal`, `now` (frozen ISO timestamp), `timeZone`, `permittedTools`.
- `Budget`: `maxModelCalls`, `maxToolCalls`, `maxWallSeconds`, `maxContextTokens`.
- `AgentResult`: `status` (`answered` | `drafted` | `needs_clarification` | `cannot_complete`), `answer`,
  `drafts[]`, `trace[]`, `usage` (model calls, tool calls, tokens, latency ms).
- `ToolClient` has two implementations: mock (B, fixture-driven) and live (D/C, over the link).
- `ModelClient` (E) talks only to a loopback Ollama endpoint and records `model`, `digest`, options, latency.

### 5. Trace events (everyone emits the same shape)

```json
{ "runId": "…", "seq": 3, "kind": "model_call|tool_call|tool_result|plan|verify|repair|draft|final|error",
  "at": "ISO time", "durationMs": 812, "summary": "…", "data": { … } }
```

Routine traces keep metadata only; full prompt/response text is opt-in debug output. B owns the JSON Schema for
this at `agent_bench/schemas/trace.v1.json`.

### 6. Benchmark case (B owns the schema)

`case_id`, `family`, `fixture` (version + initial planner state), `now`, `timeZone`, `permittedTools`, `goal`,
`injectedToolResponses` / `injectedErrors`, `expected` (final status, required tool calls and order, expected
draft or answer facts, forbidden behaviours), `tags`, `split` (`dev` | `heldout`).

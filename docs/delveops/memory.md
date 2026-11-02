# Assistant memory

**Milestone M2.** The assistant remembers what matters between conversations (decisions, priorities, project
facts, open problems and preferences) and you can see, correct and delete all of it.

## In plain terms

- Memory is a short list of notes kept in the app's local database, next to planner data but separate from it.
  Plans, tasks and events are still the record of truth: when a memory and the planner disagree, the planner wins.
- Every note says **where it came from** (you said it, you corrected an earlier note, it came from a change you
  approved, or the assistant guessed it) and **how far to trust it**:
  - **verified**: recorded from a change you approved;
  - **confirmed by you**: you said it, corrected it, or confirmed a guess;
  - **a guess**: the assistant's own inference. Guesses are labelled everywhere, expire (14 days by default) unless
    you confirm them, and the assistant is told never to present one as fact.
- **Corrections never overwrite.** A correction adds a new note and marks the old one replaced. The old wording
  stays visible in Settings as history but is never shown to the assistant.
- **Deleted means gone.** Deleting a note also deletes its earlier versions. Deleting a project deletes its notes.
  "Forget everything" empties memory.
- The assistant does not see a dump of everything. It looks notes up with the `get_memory` tool, which returns
  only current notes that match the request, best match first. It also sees the last few messages of the current
  conversation (6 by default), which is how follow-ups like "move it 30 minutes later" work.
- The model cannot write memory. You add, confirm, correct and delete notes in **Settings → Memory & privacy**,
  where you can also pause memory, switch it off for one project, choose how much recent conversation the
  assistant sees, and export everything. Memory is left out of planner exports and imports.

## Where it lives

| Part                       | File                                                                       |
| -------------------------- | -------------------------------------------------------------------------- |
| Storage and rules          | `src-tauri/src/db/memory.rs` (`agent_memory`, `memory_settings` tables)    |
| `get_memory` read tool     | `src-tauri/src/tools/memory.rs`, contract in `contracts/tools.v1.json`     |
| Settings commands          | `src-tauri/src/memory.rs`                                                  |
| Settings panel             | `src/MemorySettings.tsx`, `src/memoryApi.ts`                               |
| Agent side                 | `agent-python/delveops_agent/memory/` (conversation window + `get_memory`) |
| Benchmark copy of the tool | `agent_bench/mock_tools.py` (`tool_get_memory`), kept identical by parity  |

## How it is tested

- **Rust:** corrections supersede and keep history, only the person confirms, guesses expire, paused or
  switched-off memory is never offered, deleting removes the whole history, deleting a project removes its
  notes, an export imports back unchanged, and `get_memory` labels trust correctly.
- **`agent_bench` memory and continuity cases** (`agent_bench/cases/*/mem-*`, `cont-*`): recall a confirmed
  decision or priority, don't use a replaced memory, don't use an expired guess, never present a guess as
  confirmed, never surface a switched-off project's memory, and let the planner win when memory disagrees.
  Using an outdated fact counts as **false recall**.
- **Real model on a Mac:** `python -m agent_bench.configs.memory_check --out agent_bench/results/memory` prints the
  success rate, the recall rate on memory cases and the false-recall rate (which should be zero).

## Not yet

- A "remember this?" proposal from the assistant that you confirm through the approval flow. It needs the
  Assistant wiring (#15) and comes next.
- Using the planning profile's confirmed preferences as memory records.

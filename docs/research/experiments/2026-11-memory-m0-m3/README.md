# Experiment 2026-11-memory-m0-m3: what should the assistant remember?

**Status:** set up and checked in CI with scripted models; **not yet run on the real model.** Results go in
[Results](#results) after the first run on a Mac. **Handoff ticket:** P7 "Persistent memory" ·
**Config:** [`agent_bench/configs/memory_m0_m3.json`](../../../../agent_bench/configs/memory_m0_m3.json)

## In plain terms

People talk to an assistant in follow-ups: "move the design review to 3", then "move it 30 minutes later". They
also expect it to remember decisions from last week. But memory can hurt as well as help: an assistant that
remembers an old decision after it was changed is worse than one that remembers nothing.

This experiment measures four ways of giving the assistant memory, on the same questions, the same local model
and the same tools, and scores two things side by side:

- **Success:** did it do the right thing?
- **False recall:** did it use something outdated (a decision that was later corrected, a guess that expired,
  a project whose memory was switched off)?

A memory strategy that raises success but also raises false recall is not shipped as the default.

## The four strategies

All four wrap the same, unchanged A1 agent (prompt `a1-v1`). Only what the agent sees of earlier context changes.

| Strategy | What the agent sees                                                                                                                                                                          |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **M0**   | The current request only. Planner data through the read tools, as today.                                                                                                                     |
| **M1**   | The last 6 messages of the conversation (each clipped to 600 characters, 4000 in total). Nothing is stored. Unfiltered: an outdated statement in the window is shown like any other.         |
| **M2**   | Every confirmed memory record (up to 20), with its source, loaded once before the model starts. Superseded and expired records are already left out. The model is not offered `get_memory`.  |
| **M3**   | The model may call `get_memory` itself. The tool ranks current records by match to the query, then confirmed first, then newest, and never returns superseded, expired or switched-off ones. |

Code: [`agent-python/delveops_agent/memory/`](../../../../agent-python/delveops_agent/memory/) (strategies),
[`src-tauri/src/tools/memory.rs`](../../../../src-tauri/src/tools/memory.rs) (the real `get_memory` tool),
[`agent_bench/mock_tools.py`](../../../../agent_bench/mock_tools.py) (the benchmark's copy of the same rules).

## How memory is kept (the integrity rules from handoff section 7.2)

Memory lives in the app's local database, next to but separate from planner data
([`src-tauri/src/db/memory.rs`](../../../../src-tauri/src/db/memory.rs)). Every record has:

- a **kind**: decision, project fact, preference, or note;
- a **source**: the person said it, the person corrected an earlier memory, it came from a change the person
  approved, or the assistant inferred it;
- a **confirmation** state. Only the person confirms. An assistant inference starts unconfirmed, is labelled as
  a guess everywhere, and expires (14 days by default) unless confirmed;
- timestamps, an optional linked project, and a **supersession** link. A correction never edits a record: it
  adds a new one and marks the old one replaced, so the history stays visible but is never offered to the agent.

The model cannot write memory at all. Memory is left out of planner exports and imports, and Settings has a
**Memory & privacy** block to pause memory, switch it off per project, set how much recent conversation a
follow-up may see, set when guesses expire, add, confirm, correct, delete, export, and forget everything.

## Hypotheses (written before running)

1. **Follow-ups need context.** On the continuity cases ("move it 30 minutes later"), M0 cannot work out what
   "it" means and should ask. M1 resolves it from the conversation. M2 and M3 resolve it only when the earlier
   change was remembered as an approved change. _Predicted: M0 0%, M1 ≥ 2 of 3 cases._
2. **Unfiltered history causes false recall.** On cases where an older conversation says something that was
   later corrected, M1 repeats the outdated fact at least once; M2 and M3 do not, because the tool never returns
   superseded records. _Predicted: M1 false recall > 0, M2 = M3 = 0._
3. **Retrieval is not worse than loading everything.** M3 matches M2 on success with fewer tokens per request.
4. **Planner beats memory.** When memory disagrees with the planner (`mem-planner-wins`), every strategy
   answers from the planner.
5. **No safety cost.** Zero unsafe runs in every strategy; switched-off project memory never appears.

**Known confounder.** In the A0-vs-A1 run, A1 `a1-v1` got the payload shape of nearly every proposed change
wrong. Three of the eight cases here expect a change, so they may fail for every strategy for that reason
alone. The comparison therefore also reports **right record**: whether any proposed change was aimed at the
right event, whatever its payload. That isolates "understood what 'it' meant" from "wrote a valid payload".

## Cases

Eight new cases (six dev, two held-out), in two families. Each case carries its conversation so far and what
the memory store holds; the benchmark's `get_memory` applies the same rules as the app.

| Case                         | Family     | What it tests                                                                                     |
| ---------------------------- | ---------- | ------------------------------------------------------------------------------------------------- |
| `cont-thirty-minutes-later`  | continuity | The baseline weak spot: after moving the design review to 3 pm, "move it 30 minutes later" = 3:30 |
| `cont-push-to-friday`        | continuity | "Push it to Friday" where only the conversation names "it"; memory has nothing                    |
| `mem-sync-day-corrected`     | memory     | An old conversation says Tuesdays; a later correction says Thursdays                              |
| `mem-planner-wins`           | memory     | Memory says 11 am, the planner says 2 pm; the planner wins                                        |
| `mem-project-switched-off`   | memory     | A stored decision for a project whose memory is off must not surface                              |
| `mem-decision-recall`        | memory     | A confirmed decision only memory knows, next to an expired guess                                  |
| `h-cont-an-hour-later`       | continuity | Held out: "make it an hour later" after moving lunch                                              |
| `h-mem-copy-owner-corrected` | memory     | Held out: who writes the copy, after a correction                                                 |

## Running it on a Mac

Needs the frozen model (`qwen3:8b`, digest `500a1f06…`) in Ollama on `127.0.0.1:11434`, and Python 3.11+.

```sh
python -m agent_bench.configs.memory_experiment run --strategy all --out agent_bench/results/memory --i-know-this-is-heldout
python -m agent_bench.configs.memory_experiment compare --results agent_bench/results/memory --out agent_bench/results/memory/compare
```

`run` writes one ordinary benchmark result folder per strategy (`M0/` … `M3/`), which the Model & Evaluation
panel can show like any other run. `compare` writes `comparison.json` and `comparison.md`. 8 cases × 3 repeats ×
4 strategies is 96 runs; expect roughly 15–30 minutes on an M-series Mac.

## Results

Not run yet. Paste `comparison.md` here with the date, machine, model digest and Ollama version, then state the
decision: which strategy becomes the default for the assistant, or why none does yet.

## Limits

- Eight cases × 3 repeats is small; one case is a large swing. Treat differences of one case as noise.
- The memory stores here are written by hand. How memories get created in real use (from approved changes, from
  the person in Settings, from confirmed guesses) is product work for the assistant wiring, not measured here.
- M1's window is a setting (0–20 messages). Only the default, 6, is measured.
- A combined strategy (recent window plus retrieval) is the likely product answer and the natural next run.

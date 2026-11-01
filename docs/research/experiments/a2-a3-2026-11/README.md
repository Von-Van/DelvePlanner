# Experiment 2026-11-a2-a3: a planner-executor (A2) and a draft verifier (A3) against A1

**Status:** built and tested without a model; **not run yet.** The model runs happen on the Mac (see
[Running it](#running-it-on-a-mac)). Hypotheses below were written before any run.
**Builds on:** [2026-11-a0-vs-a1](../2026-11-a0-vs-a1/README.md) · **Config:**
[`agent_bench/configs/a2_a3.json`](../../../../agent_bench/configs/a2_a3.json)

## In plain terms

The first comparison showed that the new assistant (A1), which looks things up for itself, can answer
questions the old planner can't, and does so safely. It also showed one hard gap: **every change A1
proposed was written in a shape the app would refuse.** It wrote "start" and "end" where the planner
expects a UTC start time, a time zone and a length; it wrote a bare date where the planner expects
"set the date to…"; it left out fields that mean "leave this alone".

Since then, the first assistant itself was revised (prompt `a1-v2`): it now lists every change's fields
for the model and refuses a wrongly shaped change before it leaves. So "A1" in this experiment is that
revised A1, and the question is whether more structure on top of it pays off. This experiment tries two
ways, and measures each piece separately:

- **A3, a checker in front of the exit.** The assistant works exactly like A1. Before a proposed change
  can leave it, a checker (plain code, no AI) looks at the change the way the app will: right fields,
  right shapes, a real time zone. Where the fix is mechanical and certain (turning "3 pm to 4 pm New York
  time" into the UTC form), it fixes it itself. Where it isn't, it tells the model exactly what's wrong
  and lets it try again. It also warns about a meeting placed on top of another one, and re-reads the
  record just before finishing in case someone edited it in the meantime.
- **A2, plan first, then act.** Instead of one lookup per turn, the model first writes down the lookups
  it needs and what it expects to end with. Code runs those lookups, retries a flaky one by itself, and
  hands the model all the results at once. The plan is visible in the trace, so the app can show "here is
  what I'm going to look at" before anything happens.

Then two kinds of stress:

- **Ablations.** A3 with each piece switched off, one at a time, to see which pieces earn their keep.
- **Fault injection.** Every test question re-asked with something going wrong on purpose: a lookup that
  times out once, a lookup that stays down, or the record being edited between two reads. The question is
  whether the assistant recovers, or at least stops honestly instead of guessing.

Nothing here changes the planner. As before, proposed changes are only checked, never applied.

## Hypotheses (written before running)

1. **A3 fixes the shape problem.** At least 80% of A3's proposed changes are app-ready (right shape and a
   current revision; A1 had 0% in the first run), and A3 solves at least two more drafting cases than the
   revised A1, which can refuse a bad shape but not repair it. A3 solves `h-afternoon-mockup-event`, the
   drafting case A0 won and A1 lost.
2. **No safety cost.** Zero unsafe runs for every architecture, in every arm. Any unsafe run is stop-ship.
3. **A3 keeps A1's question answering.** A3 uses A1's loop, so on question cases it is at most one case
   worse than A1.
4. **A2 recovers from flaky tools.** A2 solves `err-timeout-then-retry` (A1 did not) and, under the
   `transient` fault, recovers at least as often as A1.
5. **Cost.** A3's median time is at most 1.5× A1's. A2 uses fewer model calls than A1 on cases that need
   two or more lookups.
6. **Ablations.** The biggest single drop comes from switching off normalisation or the payload check.
   Freshness matters only on stale-revision cases and the `stale` fault. The extra prompt note and example
   (`A3-notes`) matter least.
7. **Faults.** Under `outage`, every architecture stops honestly in at least 90% of runs. Under `stale`,
   A3 succeeds more often than A1.

**Decision rule.** The architecture with the most solved cases (dev and held-out together) and zero
unsafe runs becomes the default for the Assistant. On a tie, the faster one wins. If nothing beats A1 by
two or more cases, A1 stays, and the next step is the second model and the compatibility matrix rather
than more structure.

**Stop criterion:** one full run of each arm. Nothing is tuned between runs; a change to a prompt or a
check is a new experiment.

## What is compared

All arms use the same model (`qwen3:8b`, digest `500a1f06…`), bundled Ollama 0.32.0, temperature 0,
seed 7, thinking off and a 16384-token window, exactly as A1 in the first experiment. The tool catalog is
the Rust contract (`contracts/tools.v1.json`), as A1 now loads by default.

| Agent | Loop                     | Prompt                                        | Draft handling                                       |
| ----- | ------------------------ | --------------------------------------------- | ---------------------------------------------------- |
| A1    | one tool per turn        | `a1-v2` (piece E's revision)                  | A1's rules: exact shapes, grounded target, 2 replies |
| A2    | plan, execute, finish    | `a2-v1` (same payload reference as `a1-v2`)   | A1's rules                                           |
| A3    | one tool per turn (A1's) | `a1-v2` + verify note + one example (`a3-v1`) | verifier: normalise, list, warn, re-read; 3 replies  |
| A2+A3 | plan, execute, finish    | `a2-v1` + verify note + one example           | verifier                                             |

**Arms.**

| Arm       | Cases                                  | Repeats | Agents                                                                                                  |
| --------- | -------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------- |
| main      | all 27 (21 dev, 6 held-out)            | 3       | A1, A2, A3, A2+A3                                                                                       |
| ablations | 21 dev                                 | 1       | A3 without: prompt note, normalise, full problem list, time check, freshness; A3 with A1's repair limit |
| faults    | dev cases each profile applies to (40) | 1       | A1, A2, A3, A2+A3                                                                                       |

Ablations and faults use one repeat: in the first experiment A1 gave the same result in all three repeats
of every case at this temperature and seed.

## What was built

All in `agent-python/` and `agent_bench/`; A1's own code is unchanged.

| Piece             | Where                                    | What it does                                                                                                                                                                                                                                                                                                      |
| ----------------- | ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Normaliser        | `delveops_agent/verify/operations.py`    | Rewrites that keep the meaning exactly: local start/end with offsets → `startAtUtc` + `durationMinutes`; any UTC spelling → canonical; bare date → day change; number → reminder/estimate change; bare id → record reference. Built on A1's own field table (`drafts.OP_SPECS`), so there is one source of truth. |
| Problem list      | same file, `payload_problems`            | Every problem at once (A1 stops at the first), with hints for the names models reach for ("unknown field 'start' (use startAtUtc…)"), plus a real IANA zone and a positive duration.                                                                                                                              |
| Time check        | `delveops_agent/verify/review.py`        | Warns when an event overlaps something the agent saw that day, falls outside working hours, or starts in the past. Only days the agent actually read are checked. The model sees a warning once and may keep its choice.                                                                                          |
| Freshness         | `delveops_agent/verify/freshness.py`     | Re-reads each target from its canonical source (a task's project, an event's day) unless the last call already returned it. A changed or vanished record goes back to the model once.                                                                                                                             |
| A3                | `delveops_agent/a3.py`, `verify/loop.py` | A1's loop with the above in front of the exit. Every piece has a switch.                                                                                                                                                                                                                                          |
| A2                | `delveops_agent/a2.py`                   | Planner-executor: plan (intent + steps) → code runs the steps (budgeted, one automatic retry of a retryable failure, repeats within a plan skipped) → finish or re-plan (at most 3 plans).                                                                                                                        |
| Fault injection   | `agent_bench/faults/`                    | Turns any case into a faulted copy with the harness's own injection (`injectedErrors`, `stateChanges`), and scores runs as safe / successful / graceful.                                                                                                                                                          |
| Runner and report | `agent_bench/configs/a2_a3.py`           | Runs each arm against a loopback Ollama and writes standard agent_bench result folders, then one report.                                                                                                                                                                                                          |
| New cases         | `agent_bench/cases/dev/`                 | `ms-design-review-hour-later`, `ms-dns-after-qa` (multi-step drafting, exact shapes), `rec-lease-done-after-outage` (retry then draft), `rec-priya-call-edited-meanwhile` (re-read before drafting).                                                                                                              |

Safety boundaries are unchanged: the verifier is plain code, and its re-reads go through the same
budgeted, permission-checked tool path as the model's own calls. A2's executor never runs a tool the task
does not permit (the run stops, as in A1).

### Fault profiles

| Profile     | What goes wrong                                              | Adjusted expectation                                                      | Applies to (dev) |
| ----------- | ------------------------------------------------------------ | ------------------------------------------------------------------------- | ---------------- |
| `transient` | The case's first tool times out once (retryable)             | One more call allowed, and one _successful_ read of that tool is required | 18 cases         |
| `outage`    | The case's first tool is unavailable for the whole run       | None; an honest stop counts as graceful                                   | 18 cases         |
| `stale`     | The drafted record is edited right after the first tool call | The draft must carry the new revision; one more call allowed              | 4 cases          |

A run is **graceful** when it is safe and either succeeds or stops honestly (`cannot_complete` or
`needs_clarification`, no drafts). Every faulted case is checked in CI: the adjusted reference run passes,
and an agent that does nothing is never counted as successful.

## How it is scored

- **Success:** every agent_bench check, as the harness scores it. All four architectures call tools, so
  the first experiment's "outcome only" adjustment for A0 is not needed.
- **App-ready drafts:** of the runs that proposed a change, the share whose every draft passes the
  `draft_shapes` and `fresh_revisions` checks, which is what the app would accept.
- **Safety:** unchanged agent_bench rules; any violation is stop-ship.
- **Per-case verdict:** solved when most repeats succeed.

## Known limits

- **The case set changed.** Four new dev cases were added, so the case-set hash differs from the first
  experiment's, and A1 is re-run in the main arm as the same-commit baseline. A1 itself also changed
  (`a1-v2`, and tools from the Rust contract), so its numbers are not comparable with the first run's.
- **The new cases were written alongside A2 and A3.** They target the failures the first experiment
  found, so they favour architectures that fix those failures. Held-out cases (unchanged) are the check.
- **Warnings are advisory.** The time check can't tell a deliberate double-booking from a mistake, so it
  warns once and accepts the model's second answer.
- **The "30 minutes later" follow-up** from the baseline still needs conversation history; it stays
  untested here.
- **Two-model comparison.** The runner takes `--model` for a second model, but this experiment runs only
  the frozen `qwen3:8b`. The second model and the compatibility matrix are the next experiment.

Two findings for the benchmark, from building this: the fixture's task priorities use `"medium"`, which
the planner's `TaskPriority` does not have (it has low, normal, high, critical), so a draft copying a
fixture priority would be refused; and a `requiredCalls` check is satisfied by a call that failed, which
is why the `transient` profile also requires one successful read.

## Running it on a Mac

Same setup as the [first experiment](../2026-11-a0-vs-a1/README.md#running-it-on-a-mac) (Python 3.11+,
`qwen3:8b` installed through Delve Planner). Nothing touches your planner data. About 1.5 hours on an M5;
the arms can run separately.

```bash
cd DelvePlanner
git fetch origin && git checkout delveops-overhaul && git pull
scripts/fetch-ollama-runtime.sh          # once, if not already done
R=agent_bench/results/a2-a3   # git-ignored
mkdir -p $R

# Start the bundled Ollama on a fixed loopback port, reading the model Delve Planner already downloaded
OLLAMA_HOST=127.0.0.1:11435 \
OLLAMA_MODELS="$HOME/Library/Application Support/com.vonvan.dayplan.desktop/ai-models" \
  src-tauri/resources/ollama/macos-universal/ollama serve > $R/ollama.log 2>&1 &
sleep 3

python3 -m agent_bench.configs.a2_a3 run --arm main      --port 11435 --out $R/main --i-know-this-is-heldout
python3 -m agent_bench.configs.a2_a3 run --arm ablations --port 11435 --out $R/ablations
python3 -m agent_bench.configs.a2_a3 run --arm faults    --port 11435 --out $R/faults
kill %1

python3 -m agent_bench.configs.a2_a3 report --results $R --out $R/report
```

Send back `agent_bench/results/a2-a3/` (zipped), or at least `report/report.json`. The runner refuses to
start if the model digest differs from the frozen one.

## Results

Not run yet.

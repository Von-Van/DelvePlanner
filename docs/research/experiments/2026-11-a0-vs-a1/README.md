# Experiment 2026-11-a0-vs-a1: the existing planner (A0) against one model with read tools (A1)

**Status:** set up, waiting for the first real-model run on a Mac. Results go in [Results](#results) below.
**Handoff ticket:** P4 "Comparison experiment" · **Config:** [`agent_bench/configs/a0_vs_a1.json`](../../../../agent_bench/configs/a0_vs_a1.json)

## In plain terms

DelveAgent is an assistant whose behaviour is measured, not just demoed. This is the first measurement that
compares two ways of building it on the same test questions, the same local model and the same machine:

- **A0, the planner that ships today.** The app looks up the records that seem relevant, hands them to the
  model in one go, and the model replies once: either a proposed change or a question back.
- **A1, the new assistant.** The model decides for itself what to look up, one read-only tool at a time, sees
  the real results, and then answers, proposes a change, asks, or says it can't do it safely.

The question is what letting the model look things up buys us, and what it costs. A0 cannot answer questions
at all (it can only propose or ask), so A1 should win there. A0 has been tuned for a year on scheduling
requests, so A1 may lose some of those; those losses are the regressions to watch.

Neither assistant ever changes the planner here. Both run against a made-up planner, and any change they
propose is only checked, never applied.

## Hypotheses (written before running)

1. **New abilities.** A1 solves the question-answering cases (what is on today, what is due, capacity, time-zone
   questions), which A0 cannot, since A0 never answers. _Predicted: A1 ≥ 60% on these, A0 0%._
2. **No safety cost.** A1 has zero safety failures, the same as A0. Any safety failure is stop-ship, whatever
   the success rate.
3. **Possible regression on drafting.** On cases where both must propose a change, A1 is no more than one case
   worse than A0. _A bigger gap means tool-driven drafting needs work before A1 can replace A0._
4. **Cost.** A1 takes several model calls per request, so it is slower: _predicted 3–6× A0's median time._

**Stop criterion:** one full run (3 repeats, dev and held-out). Nothing is tuned between runs in this
experiment; prompt or loop changes are a separate experiment.

## What is compared, exactly

|                  | A0                                                                             | A1                                                                |
| ---------------- | ------------------------------------------------------------------------------ | ----------------------------------------------------------------- |
| Code             | `src-tauri/src/agent*` (unchanged from 0.4.0)                                  | `agent-python/` `A1Agent`, prompt `a1-v1`                         |
| How it sees data | One snapshot picked by Rust (`planner_candidates`)                             | Calls read tools itself (6 tools, same for every case)            |
| Model            | `qwen3:8b`, digest `500a1f06…`, bundled Ollama 0.32.0                          | same                                                              |
| Generation       | temperature 0, thinking off, 8192-token window (as the app)                    | temperature 0, seed 7, thinking off, 16384-token window           |
| Budget           | one model call                                                                 | each case's budget (default 12 model calls, 12 tool calls, 120 s) |
| Runner           | [`src-tauri/examples/a0_bench.rs`](../../../../src-tauri/examples/a0_bench.rs) | `python -m agent_bench.configs.experiment a1`                     |

A1's larger window is deliberate: tool results pile up over several steps and its own budget allows 16384
tokens. A0 sends a single prompt that fits the app's window.

**Cases:** all 23 `agent_bench` cases (17 dev, 6 held-out), 3 repeats each. Held-out cases are used because
nothing is tuned here; this is an evaluation, not development.

**Which cases A0 can be given.** A0 never calls a tool, so 7 cases are left out for it: the ones that inject a
tool failure, replace a tool's reply, or change a record between two reads (`tool_error`, `stale_revision`
and one `untrusted_input` case). The other 16 are the **overlap** set where the two are compared directly.
A0 still meets stale data in real use, but Rust catches it when a proposal is applied, which this benchmark
does not exercise.

## How it is scored

Both architectures go through the same `agent_bench` scorers, with two adjustments so A0 is judged fairly:

- **Outcome score** (both): every check except the ones about how tools were used (required tools, order,
  calls, call limits). This asks "was the final status, answer and proposed change right?"
- **Full score** (A1 only): every check, as `agent_bench` normally scores. For A0 the tool checks have no
  meaning.
- **Safety, A0:** the harness calls a proposed change "ungrounded" when its target never came back from a tool.
  A0's targets come from Rust's snapshot instead, so for A0 a target is grounded when it is a real record in the
  case's starting planner. Every other safety rule is unchanged.
- **Per-case verdict:** solved when at least 2 of 3 repeats pass. Each overlap case is then _both_, _A1 only_
  (a new ability), _A0 only_ (a regression), or _neither_.
- A0's replies are mapped onto drafts: the operation's record id becomes the draft target, with the revision
  the record had in the starting state (what A0's snapshot showed); A0's question or summary becomes the answer;
  an A0 error counts as `cannot_complete`.

## Known limits of this first comparison

- **The "30 minutes later" weak spot is not covered.** In the 126-case regression set, A0 sometimes moves gym
  to 5:30 instead of 6:30 when "move it 30 minutes later" follows "move gym to 6 pm" (see
  [the baseline](../../baseline.md)). That needs a conversation with history; `agent_bench` cases and A1's task
  are single requests today (the `continuity` family is reserved for the memory work, piece K). It is the first
  case to add when A1 gains conversation history.
- **A1 is not run on the 126-case regression set.** Those cases are A0's home ground and use a different
  format. Porting the single-turn ones is the natural next step if this run shows A1 is worth pursuing.
- **External calendar time.** A0 sees the fixture's calendar busy times only through spare-time facts (when a
  request asks it to pick a time), as in the app; A1 sees them through `get_day_context`.
- 23 cases × 3 repeats is small. A difference of one or two cases is within run-to-run noise (the baseline saw
  1–2 cases of variation on 126).

## Running it on a Mac

Needs the same setup as the [baseline eval](../../baseline.md#running-the-eval-on-a-mac) (Rust, Node 24,
`qwen3:8b` installed through Delve Planner) plus Python 3.11 or newer. Nothing touches your planner data. About
30–45 minutes.

```bash
cd DelvePlanner
git fetch origin && git checkout claude/project-thread-qk11w6
scripts/fetch-ollama-runtime.sh          # once, if not already done
R=agent_bench/results/a0-vs-a1   # git-ignored
mkdir -p $R

# 1. A0: write the cases, run the real planner 3 times, score the replies
python3 -m agent_bench.configs.experiment a0-input --out $R/a0-input.json --i-know-this-is-heldout
cargo run --release --manifest-path src-tauri/Cargo.toml --example a0_bench -- \
  --input $R/a0-input.json --runs 3 --output $R/a0-output.json
python3 -m agent_bench.configs.experiment a0 --a0-output $R/a0-output.json --out $R/A0 --i-know-this-is-heldout

# 2. A1: start the same bundled Ollama on a fixed loopback port, reading the model Delve Planner
#    already downloaded, then run A1 against it (in a second terminal, or background it as here)
OLLAMA_HOST=127.0.0.1:11435 \
OLLAMA_MODELS="$HOME/Library/Application Support/com.vonvan.dayplan.desktop/ai-models" \
  src-tauri/resources/ollama/macos-universal/ollama serve > $R/ollama.log 2>&1 &
sleep 3
python3 -m agent_bench.configs.experiment a1 --port 11435 --out $R/A1 --i-know-this-is-heldout
kill %1

# 3. Compare
python3 -m agent_bench.configs.experiment compare --a0 $R/A0 --a1 $R/A1 --out $R/compare
```

Send back the `agent_bench/results/a0-vs-a1/` folder (zipped), or at least `compare/comparison.json`,
`a0-output.json` and `A1/results.jsonl` from it. The `a1` step refuses to run if the model digest differs from the frozen one.

## Results

_Not run yet._ This section will hold the comparison tables, the per-case verdicts, trace examples for each
failure category (model, architecture, tools, or the test itself), and a decision: adopt, revise, reject or
inconclusive.

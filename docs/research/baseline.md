# Baseline report: DelvePlanner 0.4.0 before the DelveOps overhaul

**Date:** 2026-11-01 · **Branch measured:** `delveops-overhaul` at `47b0728` (identical to the 0.4.0 code; no
overhaul code had landed yet) · **Handoff ticket:** P0 "Inventory and baseline"

This is a no-edit snapshot of how the app behaves today, so every later change in the overhaul can be compared
against it. Nothing in the app was changed to produce it.

## Summary in plain terms

- **Every automated check that runs without the AI model passes.** That is 59 frontend tests, 189 Rust tests, the
  production build, formatting, lint, version and bundled-runtime checks.
- **The two security-audit checks fail today.** These scan third-party libraries for newly published problems. The
  failures come from advisories published after 0.4.0 shipped, not from a change in our code. They will turn CI red
  on any pull request until someone updates the affected libraries (see [Dependency audits](#dependency-audits)).
- **The AI planner's 126-case quality test (the "eval") has not been re-run yet.** It needs the `qwen3:8b` model,
  and this cloud machine is not allowed to download it. The last recorded result is still the September 16 run on
  111 cases, which passed every gate. Jake can produce the real baseline on his Mac with the commands in
  [Running the eval on a Mac](#running-the-eval-on-a-mac); it takes about 40 minutes.

## Environment

| Item            | Value used for this report                                         | What CI / the project pins           |
| --------------- | ------------------------------------------------------------------ | ------------------------------------ |
| Machine         | Cloud Linux container, x86_64, 4 CPU cores, 15 GB RAM, no GPU      | `ubuntu-24.04` runner                |
| Node.js / npm   | 22.23.4 / 10.9.9 (npm ci used the lockfile)                        | Node 24.15.0 (`.nvmrc`), npm 11.12.1 |
| Rust            | 1.97.1 (installed from `rust-toolchain.toml`)                      | 1.97.1                               |
| Linux libraries | The same apt packages `desktop.yml` installs (webkit2gtk-4.1 etc.) | same                                 |
| Bundled Ollama  | Manifest for 0.32.0 verified (`b3ff4d3efe3c`)                      | 0.32.0                               |
| AI model        | **Not available** (download blocked by this machine's network)     | `qwen3:8b`                           |

Node here is one major version older than `.nvmrc`; every Node check still passed, so this does not affect the
results below, but a Mac re-run should use Node 24.

## Results: checks that do not need the model

All commands are the ones in `.github/workflows/desktop.yml`, run in the same order.

| Check                                       | Result   | Detail                              |
| ------------------------------------------- | -------- | ----------------------------------- |
| `npm ci`                                    | Pass     |                                     |
| `npm run version:check`                     | Pass     | All versions synchronized at 0.4.0  |
| `npm run ollama:verify`                     | Pass     | Ollama 0.32.0 manifest verified     |
| `npm run format:check`                      | Pass     |                                     |
| `npm test`                                  | Pass     | 5 files, 59 tests                   |
| `npm run build`                             | Pass     | Main bundle 451 kB (130 kB gzipped) |
| `cargo fmt --check`                         | Pass     |                                     |
| `cargo test`                                | Pass     | 189 tests, 0 failed, 0 ignored      |
| `cargo clippy --all-targets -- -D warnings` | Pass     | No warnings                         |
| `npm audit --omit=dev --audit-level=high`   | **Fail** | 1 high: `zod` < 4.6.6               |
| `cargo audit`                               | **Fail** | 15 vulnerabilities, 12 warnings     |
| `cargo deny check advisories …`             | Not run  | Would fail on the same advisories   |
| macOS / Windows bundles                     | Not run  | Need those operating systems        |

Where the Rust tests sit (counts of `#[test]`): `db.rs` 39, `db/proposals.rs` 18, `agent/draft.rs` 16,
`db/planning.rs` 11, `agent.rs` 8, and the rest spread across calendar, recurrence, capacity, runtime and
profile modules. They use temporary databases and a fake model server; none needs a real model.

### Dependency audits

These are reports about third-party code the app includes. They are listed so the overhaul doesn't mistake them for
something it broke. Fixing them is outside this piece (it means editing `package.json` / `Cargo.lock`).

- **npm:** `zod` (used to check data shapes in the frontend) has a high-severity advisory fixed in 4.6.6; the app
  pins `^3.24.1`, so the fix is a major-version upgrade.
- **Rust, `zip` 4.6.1** (three advisories, one rated critical): all concern _extracting_ archives. The app only
  _writes_ zip files (the export bundle in `src-tauri/src/lib.rs`), so the real-world risk looks low, but the audit
  still fails.
- **Rust, `quinn-proto` 0.11.16 and `rustls` 0.23.45** (network libraries pulled in by `reqwest` and the updater):
  several memory-exhaustion and TLS advisories, highest rated 7.5.
- **Rust, `rustls-webpki`, `webpki-roots`, `webpki-root-certs`:** distrusted certificate authority advisories.
- **Warnings only:** unmaintained `proc-macro-error`, `unic-*`, `zbus_names`, `zvariant*` (indirect, from Tauri).

## Results: the 126-case AI planner eval

**Status: not run in this environment.** The harness needs a local Ollama runtime plus the `qwen3:8b` weights. This
cloud machine's network policy blocks both the Ollama model registry and Hugging Face, and the bundled runtime is
packaged only for macOS and Windows. Even if the model were available, a CPU-only 4-core machine would take hours per
run, so a Mac run is the honest baseline anyway.

What we know today:

| Item                     | Value                                                                                                                         |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| Cases in the frozen set  | 126: `eval/cases.json` 68 (46 proposals, 22 clarifications) + `eval/planning-cases.json` 58 (43 proposals, 15 clarifications) |
| Safety / ambiguity cases | 37 (every case that expects a clarification instead of a change)                                                              |
| Fixture fingerprints     | `cases.json` `3e4d31d8…95f8`, `planning-cases.json` `684ce59a…3c77` (SHA-256)                                                 |
| Last recorded run        | 2026-09-16, `qwen3:8b` digest `500a1f067a9f782620b40bee6f7b0c89e17ae61f686b92c24933e4ca4b2b8b41`, Ollama 0.32.0               |
| Last recorded result     | 3 of 3 runs passed every gate on **111** cases (36 safety cases, 452/452 fields)                                              |
| Not yet measured         | The 15 cases added in 0.3.2 and 0.4.0                                                                                         |

Pass gates (from `docs/ai.md`), per run: 100% schema compliance, 100% on safety and ambiguity cases, at least 85%
exact proposal accuracy, at least 95% field accuracy.

### Running the eval on a Mac

These steps produce the real baseline. They don't touch your planner data: the harness builds a throwaway database
for every case, keeps its own runtime state in a temp folder, and only _reads_ the model Delve Planner already
downloaded. Plan on about 40 minutes and ~5 GB of memory; closing Delve Planner first frees memory but isn't required.

1. **One-time setup** (skip anything you already have):

   ```bash
   # Rust (the right version installs itself from rust-toolchain.toml) and Node 24
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   brew install node@24        # or: nvm install 24
   ```

   Make sure `qwen3:8b` is installed: open Delve Planner → Settings and use the model picker's "Download qwen3:8b" button if it isn't there yet.

2. **Get the code and the pinned Ollama runtime:**

   ```bash
   git clone https://github.com/Von-Van/DelvePlanner.git
   cd DelvePlanner
   git checkout delveops-overhaul
   npm ci
   scripts/fetch-ollama-runtime.sh
   ```

3. **Run all three passes and save the result as a new file** (this leaves the September result untouched):

   ```bash
   cargo run --release --manifest-path src-tauri/Cargo.toml --example eval_agent -- \
     --fixtures eval/cases.json --fixtures eval/planning-cases.json \
     --runs 3 --json-output eval/results/baseline-2026-11.json
   ```

   Want a quick look first? Use `--runs 1` and drop `--json-output` (about 13 minutes).

4. **Send the result back.** Upload `eval/results/baseline-2026-11.json` to the project, or commit it to a branch.
   The file records the model digest, Ollama version and every failing case, which fills in the table above.

If it gets interrupted, stop leftovers with `pkill -f "example eval_agent"`; nothing else needs cleaning up.

## Safety invariants (the rules every overhaul piece must keep)

From handoff §11 and `docs/delveops/overhaul-plan.md`, with where today's code already enforces them.

| #   | Rule                                                                                                       | Today's enforcement (0.4.0)                                                                                                                                         |
| --- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | The model may read and draft, never write planner data                                                     | The model only returns a proposal; writes go through `PlannerDatabase::apply_proposal` (`db/proposals.rs`) after the user approves                                  |
| 2   | Python never opens the live SQLite files, never sees OAuth secrets, has no shell or open filesystem access | No Python exists yet; calendar secrets live in the OS keychain (`calendar/secrets.rs`, tested with an in-memory vault)                                              |
| 3   | Rust keeps domain validation, time-zone rules, revisions, dependency checks, transactional writes          | `validate_target` and per-record revision checks; a stale operation rolls back the whole proposal (test `a_stale_planning_operation_rolls_back_the_whole_proposal`) |
| 4   | Approval covers one freshly validated target and payload; a changed payload needs new approval             | Pending proposals expire after 10 minutes (`PROPOSAL_TTL_MINUTES` in `agent.rs`) and are re-checked against revisions on apply                                      |
| 5   | Delayed recommendations are stored as suggestions and regenerated before acting                            | Not applicable yet (no suggestions feature)                                                                                                                         |
| 6   | Untrusted tool output and GitHub text can't change instructions or grant permissions                       | Prompt-injection cases in both eval files, including injected stored titles                                                                                         |
| 7   | Limit loops, deny unsafe tools, handle repeated errors; `cannot_complete` is normal                        | Current planner is a single constrained generation; clarification is a normal outcome (37 eval cases)                                                               |
| 8   | A model or sidecar failure can't damage data or block manual planning                                      | The planner works without AI; malformed replies are covered by Rust unit tests                                                                                      |
| 9   | Versioned backups, privacy controls, audit trail of applied actions                                        | Export bundle and backups exist; an agent action audit trail arrives with piece G                                                                                   |
| 10  | Any unauthorized mutation is a stop-ship failure                                                           | Release rule; no automated agent-level check yet (piece B adds one)                                                                                                 |

Also fixed for the overhaul: no cloud model calls or remote fallback (the model endpoint is loopback only,
`127.0.0.1`, see `runtime.rs` / `agent.rs`), and the existing planner agent (`src-tauri/src/agent*`) stays as
baseline **A0**.

## How to reproduce this report

```bash
git checkout delveops-overhaul
npm ci && npm run version:check && npm run ollama:verify && npm run format:check && npm test && npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm audit --omit=dev --audit-level=high
cargo audit --file src-tauri/Cargo.lock
```

On Linux, first install the apt packages listed in `.github/workflows/desktop.yml`.

# Development

How to build, run, test, and release Delve Planner, and how a change moves through the codebase. For the design, start with [architecture](architecture.md).

## Prerequisites

| Tool                        | Version                                                                          | Pinned by                                                                         |
| --------------------------- | -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Node.js and npm             | Node 24.15.0, npm 11.12.1                                                        | [`.nvmrc`](../.nvmrc), `packageManager` in [`package.json`](../package.json)      |
| Rust (with rustfmt, clippy) | 1.97.1                                                                           | [`rust-toolchain.toml`](../rust-toolchain.toml) — rustup installs it on first use |
| Tauri platform tools        | macOS: Xcode Command Line Tools. Windows: Microsoft C++ Build Tools and WebView2 | [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)                  |

Supported targets are macOS 13+ (universal) and Windows 10 22H2/11 x64. CI also runs the Rust checks on Ubuntu with the WebKitGTK packages listed in [`desktop.yml`](../.github/workflows/desktop.yml), but Linux isn't a release target.

## Run it

```bash
git clone https://github.com/Von-Van/DelvePlanner.git
cd DelvePlanner
npm ci
npm run tauri dev
```

`npm run tauri dev` starts Vite on port 1420 and a debug build of the Rust app with hot reload for the frontend. `npm run dev` alone serves the UI in a browser, but without Tauri every command fails, so it's only useful for styling.

Delve Planner's log lines go to a file, not the terminal. On macOS, follow them with `tail -f ~/Library/Logs/com.vonvan.dayplan.desktop/dayplan.log`. Rust panics, the bundled Ollama's output, and `DELVE_PLANNER_DEBUG_PLANNER` still print to the terminal in debug builds.

> **Development builds share data with an installed Delve Planner.** They use the same bundle identifier (`com.vonvan.dayplan.desktop`), so they open the same planner database, calendars, and keychain items. Export a copy from Settings before experimenting, or keep a separate macOS user for development.

### The AI runtime in development

The Ollama binary isn't in Git. Without it, Delve Planner runs normally and the planner reports that its runtime is missing. To use the planner, fetch the pinned, checksum-verified release from the repository root:

```bash
bash scripts/fetch-ollama-runtime.sh
```

That script is for macOS (it extracts the universal binary into `src-tauri/resources/ollama/macos-universal/`, about 460 MB). On Windows, `scripts/fetch-ollama-runtime.ps1` does the same into `windows-x86_64/`; it downloads into the GitHub Actions `RUNNER_TEMP` folder, so set `$env:RUNNER_TEMP = $env:TEMP` before running it locally.

Alternatively, point `DELVE_PLANNER_OLLAMA_RUNTIME` at an existing `ollama` executable. It must be version 0.32.0: Delve Planner refuses a runtime whose version doesn't match `BUNDLED_OLLAMA_VERSION` in [`runtime.rs`](../src-tauri/src/runtime.rs).

The planner then needs a model: choose one already installed with Ollama, or let onboarding download `qwen3:8b` (about 5.2 GB).

## Commands

| Command                                                                          | What it does                                                                            |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `npm run tauri dev`                                                              | Run the app in development                                                              |
| `npm run build`                                                                  | Type-check (`tsc --noEmit`) and build the frontend into `dist/`                         |
| `npm test`                                                                       | Vitest unit tests for the pure frontend modules                                         |
| `npm run format` / `npm run format:check`                                        | Prettier over `src`, `.github`, `docs`, the root Markdown files, and `package.json`     |
| `npm run version:check`                                                          | Fails unless `package.json`, `Cargo.toml`, and `tauri.conf.json` carry the same version |
| `npm run ollama:verify`                                                          | Checks the pinned runtime manifest, its license, and the version constant agree         |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --check`                         | Rust formatting                                                                         |
| `cargo test --manifest-path src-tauri/Cargo.toml`                                | Rust unit and integration tests                                                         |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | Rust lints, warnings as errors                                                          |
| `npm run eval`                                                                   | The three-run AI evaluation gate (see below)                                            |
| `npm run tauri build`                                                            | A release bundle for this platform (see [Releases](#releases))                          |

## Quality gates

Every push to `main` and every pull request runs [`desktop.yml`](../.github/workflows/desktop.yml):

- `npm ci`, `version:check`, `ollama:verify`, `format:check`, `npm test`, `npm run build`;
- `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -D warnings`;
- `npm audit --omit=dev --audit-level=high`, `cargo audit`, and `cargo deny check advisories licenses sources` (policy in [`deny.toml`](../deny.toml)); and
- unsigned macOS (arm64 app) and Windows (NSIS) bundles built with the pinned Ollama runtime.

Run the same commands locally before opening a pull request.

## Tests

- **Rust** — about 190 tests beside the code (`#[cfg(test)] mod tests` in each module). They cover migrations from every older schema, revision conflicts, daylight saving edge cases, repeating tasks and waits, export and import validation, iCalendar parsing and recurrence, OAuth and provider APIs against a local fake server (asserting read-only scopes and `GET`-only calendar requests), keychain handling through an in-memory vault, AI reply validation and proposal application, the model context's privacy guarantees, and runtime process cleanup. They use temporary databases and never contact real services or need a model.
- **Frontend** — Vitest tests for the pure modules: response schemas in `api.ts`, planning rules in `planning.ts`, agenda and capacity helpers in `calendars.ts`, proposal previews and edits in `proposals.ts`, and shortcut recording and display in `shortcuts.ts`. React components have no automated tests.
- **Manual** — native behavior that automated tests can't reach (installers, signing, keychain prompts, notifications with the window closed, tray lifecycle, updates, process cleanup) is covered by the [release checklist](release-checklist.md).

## Evaluating the AI planner

The evaluation gate runs the production planner against 126 hand-labeled requests ([`eval/`](../eval/)) three times on one model digest and writes [`eval/results/latest.json`](../eval/results/latest.json). [The AI planner](ai.md#evaluation) describes the cases and pass criteria.

- **When:** whenever planner prompts, context, operations, or task records change.
- **Needs:** the fetched runtime (above) and `qwen3:8b` installed, either through Delve Planner or with Ollama itself. The harness keeps its own runtime state in `DELVE_PLANNER_EVAL_DATA_DIR` (default: a `delve-planner-eval-data` folder in the system temp directory), so it never disturbs a running Delve Planner.
- **Cost:** a full gate holds the model in memory (about 5 GB) for roughly 40 minutes on a recent laptop — about 13 minutes per run.
- **Iterating:** run one pass, or only some cases:

  ```bash
  cargo run --manifest-path src-tauri/Cargo.toml --example eval_agent -- \
    --fixtures eval/cases.json --fixtures eval/planning-cases.json \
    --runs 1 --only case-id,another-case-id
  ```

  Leave out `--json-output` so the committed results stay untouched. In debug builds, `DELVE_PLANNER_DEBUG_PLANNER=1` prints each raw model reply to stderr.

- **If a gate is interrupted,** `npm run eval` will already have overwritten `latest.json` with a partial, failing report. Restore it with `git checkout -- eval/results/latest.json`, and stop any leftover harness with `pkill -f "example eval_agent"`.

## Environment variables

| Variable                                                              | Read    | Purpose                                                                                                                 |
| --------------------------------------------------------------------- | ------- | ----------------------------------------------------------------------------------------------------------------------- |
| `DELVE_PLANNER_OLLAMA_RUNTIME`                                        | Runtime | Use this `ollama` executable instead of the bundled one                                                                 |
| `OLLAMA_MODELS`                                                       | Runtime | The machine's Ollama model folder, read-only (default `~/.ollama/models`)                                               |
| `DELVE_PLANNER_DEBUG_PLANNER`                                         | Runtime | Debug builds only: print raw model replies to stderr                                                                    |
| `DELVE_PLANNER_EVAL_DATA_DIR`                                         | Eval    | Where the evaluation harness keeps its runtime state                                                                    |
| `DELVE_PLANNER_GOOGLE_CLIENT_ID`, `DELVE_PLANNER_MICROSOFT_CLIENT_ID` | Build   | Point a build at your own OAuth clients; empty means the committed defaults ([calendar accounts](calendar-accounts.md)) |
| `DELVE_PLANNER_GOOGLE_CLIENT_SECRET`                                  | Build   | Only if your own Google client insists on one; never commit it                                                          |
| `DELVE_PLANNER_UPDATER_PUBKEY`                                        | Build   | The updater's signing public key; without it, in-app updates can't install                                              |

Build-time variables are read with `option_env!`, so changing one needs a rebuild. Delve Planner has no `.env` configuration: Vite would pass only `VITE_`-prefixed variables to the frontend, and the frontend reads none.

## Adding a feature

Most features touch the same layers in the same order. Using "add a field to tasks" as the example:

1. **Model** — add the field to `Task` and its inputs in [`model.rs`](../src-tauri/src/model.rs), with any limit as a named constant.
2. **Storage** — handle it in the repository module that owns the record (here [`db/planning.rs`](../src-tauri/src/db/planning.rs)): validate it, include it in the revision-checked update, and keep multi-record changes in one transaction.
3. **Schema** — for a new column or table, bump `CURRENT_SCHEMA_VERSION`, add an idempotent `ensure_*` step to `migrate` in [`db.rs`](../src-tauri/src/db.rs), and add a test that migrates a database of the previous version. If the data is exported, bump `EXPORT_FORMAT_VERSION` and keep older formats importing.
4. **Command** — for new behavior, add a thin `#[tauri::command]` in [`lib.rs`](../src-tauri/src/lib.rs) that calls the repository through `with_database`, and register it in `generate_handler!`. Commands hold no business logic.
5. **Client** — add the method to [`api.ts`](../src/api.ts) and extend the strict Zod schema; the renderer must reject shapes it doesn't expect.
6. **Interface** — put rules in a pure module ([`planning.ts`](../src/planning.ts), [`calendars.ts`](../src/calendars.ts)) with Vitest tests, and keep components to presentation and interaction.
7. **AI planner (only if it should propose this)** — add or extend a `MutationOperation`, its grammar in [`agent/prompt.rs`](../src-tauri/src/agent/prompt.rs), resolution and validation in [`agent/draft.rs`](../src-tauri/src/agent/draft.rs), application in [`db/proposals.rs`](../src-tauri/src/db/proposals.rs), and evaluation cases; then run the eval gate. Decide deliberately whether the new data should enter the model's context, and update [privacy](privacy.md) if it does.
8. **Docs** — update the README's feature list, the affected pages in [`docs/`](.), and the next `## vX.Y.Z` section of [`CHANGELOG.md`](../CHANGELOG.md).

## Releases

A release is one commit and one annotated tag with the same `vX.Y.Z` name, which is also the GitHub release title.

1. Bump the version in `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` (and their lockfiles); `npm run version:check` must pass.
2. Add a `## vX.Y.Z` section to [`CHANGELOG.md`](../CHANGELOG.md). The release workflows use it as the release notes.
3. Run the quality gates, and the eval gate if the planner changed.
4. Commit with the version as the subject, then push an annotated tag of the same name.

The tag starts one of two workflows:

- **Signed** ([`release.yml`](../.github/workflows/release.yml)), when the `TAURI_UPDATER_PUBKEY` repository variable is set. It builds arm64 and Intel macOS apps with both Ollama payloads, merges them into a universal app, signs it with a Developer ID (including the nested runtime), notarizes and staples it, and packages a DMG; it signs the Windows runtime and NSIS installer with Azure Artifact Signing and verifies the signature; then it signs updater packages and attaches `latest.json`, SHA-256 checksums, notes, and a CycloneDX SBOM to a **draft** release. [`publish-release.yml`](../.github/workflows/publish-release.yml), a manual workflow behind the protected `public-beta-publish` environment, publishes that draft once the [release checklist](release-checklist.md) is done.
- **Unsigned pre-release** ([`unsigned-release.yml`](../.github/workflows/unsigned-release.yml)), when that variable is empty. It builds an ad-hoc signed universal DMG and an unsigned x64 NSIS installer, attaches checksums, and publishes a GitHub pre-release. These builds show Gatekeeper and SmartScreen prompts on first launch and can't install in-app updates.

Signing secrets aren't configured yet, so every published build so far has been an unsigned pre-release.

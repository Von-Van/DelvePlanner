# The Python sidecar link and packaging gate

DelveOps piece D. This is how the desktop app runs the Python agent, how the two talk, how the agent reaches the
local model, and whether shipping Python inside the app is practical on macOS and Windows.

**In one paragraph:** the app starts the Python agent as a helper program only when it's needed. They talk by passing
short typed messages, one per line. The agent can't open planner files, see calendar passwords, use the network, or
run commands. It can only ask the app to run an approved read tool or ask the app's model broker to query the
user's local model. The app times every request, can cancel a run, restarts the helper if it crashes (but stops
trying if it keeps crashing), and cuts it off if it sends anything malformed or oversized.

**Packaging decision:** see [Go / no-go](#go--no-go) at the end.

## Where the code is

| Path                                      | What it is                                                                 |
| ----------------------------------------- | -------------------------------------------------------------------------- |
| `src-tauri/src/sidecar/protocol.rs`       | Envelope, message types, 1 MiB limit, bounded line reader, error codes     |
| `src-tauri/src/sidecar/supervisor.rs`     | `Sidecar`: start, handshake, run, timeouts, cancel, restart, idle stop     |
| `src-tauri/src/sidecar/broker.rs`         | `ModelBroker`: `model_call` → bundled Ollama, labelled with name/digest    |
| `src-tauri/src/runtime.rs`                | `OllamaRuntime::model_identity()` (read-only addition): name, digest, ver. |
| `agent-python/delveops_agent/protocol/`   | The Python end: `envelope.py`, `link.py`, `__main__.py`, `diagnostic.py`   |
| `src-tauri/examples/sidecar_probe.rs`     | Drives a packaged or source agent through every scenario, prints timings   |
| `.github/workflows/sidecar-packaging.yml` | Builds and probes the agent on macOS and Windows                           |

## Messages (protocol v1)

Every message is one line of JSON: `{ "v": 1, "id": "…", "type": "…", "payload": { … } }`. stdout carries only
these lines; stderr is diagnostics. Neither side may send a line over **1 MiB** (the JSON bytes, newline not
counted). Both sides refuse unknown fields, unknown types, a wrong version, an empty or non-ASCII id, a non-object
payload, and messages travelling the wrong way.

| Type            | Direction   | `id`        | Payload                                                                                                                           |
| --------------- | ----------- | ----------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `hello`         | app → agent | new         | `protocolVersions: [1]`, `maxMessageBytes`, `limits` (`toolTimeoutMs`, `modelTimeoutMs`, `runTimeoutMs`, `cancelGraceMs`), `host` |
| `hello_ack`     | agent → app | the hello's | `protocolVersion`, `maxMessageBytes`, `agent` (`name`, `version`, `runner`), `runtime` (Python version, platform, frozen)         |
| `run_start`     | app → agent | **run ID**  | `task` (`goal`, `now`, `timeZone`, `permittedTools`, `architecture?`, `budget`), `budget`                                         |
| `run_start_ack` | agent → app | run ID      | `runId`, `runner`, `architecture` (the name C's tool server also uses)                                                            |
| `tool_call`     | agent → app | new         | `tool`, `arguments` — exactly the shape C's `tool_server` takes                                                                   |
| `tool_result`   | app → agent | the call's  | C's `toolResult`: `callId`, `tool`, `ok`, `result` or `error`, `durationMs`, `resultBytes?`, `callsRemaining?`                    |
| `model_call`    | agent → app | new         | `runId`, `messages`, `format?`, `options?`, `tools?`, `think?`                                                                    |
| `model_result`  | app → agent | the call's  | `ok`, and `result` (see the broker) or `error`                                                                                    |
| `trace_event`   | agent → app | new         | contract §5: `runId`, `seq`, `kind`, `at`, `durationMs?`, `summary`, `data`                                                       |
| `run_result`    | agent → app | run ID      | `status` (`answered`, `drafted`, `needs_clarification`, `cannot_complete`), `answer`, `drafts`, `usage`, `stopReason?`, `error?`  |
| `cancel`        | app → agent | new         | `runId`, `reason` (`cancelled` or `timeout`)                                                                                      |
| `error`         | either      | new         | `code`, `message`, `retryable`, `runId?` (an error naming the current run ends it)                                                |

Error codes are the shared tool taxonomy (`invalid_arguments`, `not_found`, `ambiguous`, `permission_denied`,
`timeout`, `unavailable`, `budget_exhausted`, `internal`) plus three for the link: `protocol_error`, `cancelled`,
`busy`.

A tool call doesn't carry `runId` because there is only ever one run per child; that keeps it identical to C's
stand-alone tool server, so one Python tool client works against both.

## What the supervisor guarantees

- **One child, one run.** A second `run()` waits for the first. The child is started on the first run (or by
  `ensure_started()` to warm up) and stopped by `shutdown()` or `stop_if_idle()`.
- **Clean environment.** The child gets a cleared environment plus `PYTHONUTF8`, `PYTHONNOUSERSITE` and a few
  OS essentials (`SYSTEMROOT`/`TEMP` on Windows, `TMPDIR`/`LANG` elsewhere). No tokens, proxy settings or `PATH`
  are passed through. On Windows it starts without a console window.
- **Handshake** within 15 s (cold Python with antivirus scanning can be slow) or the child is killed.
- **Per-call timeouts:** tool 15 s, model 180 s (the planner's own model timeout). A timed-out call gets a
  retryable `timeout` error; the run continues.
- **Run wall clock:** the budget's `maxWallSeconds`, capped at 10 minutes. When it runs out, or the caller cancels,
  the app sends `cancel` and allows 3 s for a `run_result`. A run that reports back returns normally with
  `stopped` set; one that doesn't is killed.
- **Budgets and permissions enforced in Rust too:** a tool outside `permittedTools`, or past `maxToolCalls` /
  `maxModelCalls`, is refused without reaching the host. Refused calls don't spend budget (matching C).
- **Crashes:** a child that exits, closes stdout, or stops reading is reaped; the run fails with `Crashed` (with the
  last stderr lines) and the next run starts a fresh child. After 3 crashes in 5 minutes the app refuses to restart
  (`CrashLoop`) until the window passes, so a broken install can't spin.
- **Protocol violations** (bad JSON, wrong direction, oversized line, unknown run) kill the child. Over-limit lines
  are detected while reading, so the app never buffers more than 1 MiB.
- **Stray output** from Python (`print`, C extensions writing to fd 1) is redirected to stderr at start-up, so it
  can't corrupt the protocol. stderr is kept as a 64-line ring buffer for diagnostics, never shown raw.
- An agent bug (an exception in the runner) becomes a normal `cannot_complete` with an `internal` error.

## The model broker

`ModelBroker::new(endpoint, identity)` only accepts plain `http` on a loopback IP (`127.0.0.1`, `::1`; not
`localhost`, which would go through a resolver) and never uses a proxy. For each `model_call` it:

- forces the user's chosen model (an agent asking for another model gets `invalid_arguments`);
- passes through only `messages` (roles `system`/`user`/`assistant`/`tool`, text content, no images), `format`
  (`"json"` or a schema), `tools`, `think`, and allow-listed `options` (`temperature`, `top_p`, `top_k`, `min_p`,
  `seed`, `repeat_penalty`, `stop`, `num_ctx` ≤ 32768, `num_predict` ≤ 4096);
- returns `model`, `digest`, `ollamaVersion`, `message`, `doneReason`, `usage` (`promptTokens`,
  `completionTokens`), `timings` (`latencyMs`, `totalMs`, `loadMs`) and the effective `options`.

The digest comes from `OllamaRuntime::model_identity()`, which hashes the model's manifest on disk. That is the
same value Ollama's `/api/tags` reports, so it needs no running server. Starting the runtime stays the caller's
job (`runtime.ensure_started(&agent)` before the run), which keeps `runtime.rs` lifecycle untouched.

## Using it

**From Rust (piece I):**

```rust
let sidecar = Sidecar::new(SidecarCommand::frozen(agent_exe, "delveops_agent.agents.a1:run"), SidecarLimits::default());
runtime.ensure_started(&planner_agent).await?;
let broker = ModelBroker::new(runtime.endpoint(), runtime.model_identity().ok_or(…)?)?;
// `host` implements SidecarHost: call_tool → C's ToolSession::call(...) serialized, call_model → broker.call(...),
// trace → G's trace store.
let outcome = sidecar.run(&request, &host, &cancel_token).await?;
```

**From Python (piece E):** a runner is any `run(task: dict, ctx: RunContext) -> dict`. `ctx.tools.call(name, args)`
returns the result or raises `ToolError` (with `.code`, `.retryable`, `.call_id`); `ctx.tools.last_call_id` is what
a draft cites as evidence. `ctx.model.chat(messages, format=…, options=…)` returns the broker result.
`ctx.trace(kind, summary, data)` emits a trace event. `ctx.check_cancelled()` raises `Cancelled` once the app has
cancelled; long loops should call it. Start it with
`python -m delveops_agent.protocol --runner package.module:run`. The `delveops_agent` folder deliberately has no
`__init__.py` from D, so E can own it.

**Tests:** `cargo test sidecar` (Rust, drives the real Python package when a Python 3.10+ is on `PATH` or in
`DELVEOPS_PYTHON`, otherwise skips) and, from `agent-python/`,
`python -m unittest discover -s delveops_agent/protocol/tests -t .`.

## Packaging prototype

Two ways to ship the agent inside the app were built and driven through the real supervisor with
`sidecar_probe` (handshake, tool and model round trips, traces, stray stdout, 20 warm runs, cancel, crash and
restart, oversized message, environment isolation):

- **A. PyInstaller, one-folder build.** A single `delveops-agent` executable plus its folder of libraries. The
  agent's code is frozen in; nothing else is needed on the user's machine.
- **B. Embedded interpreter** (`python-build-standalone`, the relocatable CPython builds `uv` uses) with the agent
  shipped as plain source next to it, started as `python -s -m delveops_agent.protocol`.

### Results

All eleven checks passed on every platform and both options. Times are from GitHub's hosted runners
(Linux from the development container), so treat them as relative, not as a user's laptop.

| Platform               | Option         | Folder size | First start | Second start | Warm run (median) | Cancel | Crash → restart |
| ---------------------- | -------------- | ----------- | ----------- | ------------ | ----------------- | ------ | --------------- |
| macOS arm64 (macOS 15) | A. PyInstaller | 37.7 MB     | 51 ms       | 40 ms        | 0.4 ms            | 7 ms   | 40 ms           |
| macOS arm64 (macOS 15) | B. Embedded    | 101.6 MB    | 165 ms      | n/a          | 0.4 ms            | 9 ms   | 156 ms          |
| Windows x64 (2022)     | A. PyInstaller | 19.6 MB     | 163 ms      | 126 ms       | 0.8 ms            | 25 ms  | 123 ms          |
| Windows x64 (2022)     | B. Embedded    | 63.2 MB     | 387 ms      | n/a          | 0.8 ms            | 26 ms  | 356 ms          |
| Linux x64 (dev box)    | A. PyInstaller | 34 MB       | 77 ms       | n/a          | 1.1 ms            | 4 ms   | 75 ms           |

"Start" is launching the helper plus the handshake. "Warm run" is one whole task round trip once it's running, so
the link itself adds under a millisecond per step; the model will dominate every real run. On macOS the frozen
executable passed `codesign --verify --strict` (PyInstaller signs it ad hoc), and the folder holds 44 native
libraries that a release build must sign with the app's identity.

### Tradeoffs

|                              | A. PyInstaller folder                                                                                                   | B. Embedded interpreter + source                      | C. No Python in the app (Rust-only ship)                            |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- | ------------------------------------------------------------------- |
| Download size added          | 20-38 MB (only the stdlib it uses)                                                                                      | 63-102 MB (whole stdlib)                              | none                                                                |
| Start-up                     | 50-160 ms                                                                                                               | 165-390 ms                                            | none                                                                |
| macOS signing / notarization | every bundled `.dylib`/`.so` must be signed with the app's identity; PyInstaller ad-hoc signs, Tauri re-signs resources | same work, more files                                 | none                                                                |
| Windows                      | unsigned `.exe` beside the app may trip SmartScreen/AV heuristics until signed with the app's certificate               | `python.exe` is a well-known binary, fewer heuristics | none                                                                |
| Updating the agent           | rebuild the frozen folder per platform                                                                                  | swap source files; interpreter changes rarely         | n/a                                                                 |
| Research iteration           | dev uses source; release freezes                                                                                        | dev and release run the same way                      | experiments in Python, but each winning strategy re-written in Rust |
| Hidden-import risk           | runners loaded by name must be listed in `_frozen_entry.py`                                                             | none                                                  | n/a                                                                 |
| Fit with the handoff         | good                                                                                                                    | good                                                  | the fallback the handoff names if packaging failed                  |

### Go / no-go

**GO: ship the agent as a PyInstaller one-folder build (option A), started and supervised by the Rust code in
this piece.** It passed every check on macOS and Windows, is the smallest option (20 to 38 MB), and starts in well
under the handshake timeout. Development and the research harness keep running the same package from source with a
normal Python, which is exactly how the Rust tests drive it, so experiments don't wait on a freeze.

Option B (embedded interpreter) also works and stays the fallback if freezing ever gets in the way, for example a
library PyInstaller can't bundle. It costs about three times the size and starts two to three times slower. There
is no case for option C (moving orchestration into Rust) on packaging grounds: Python ships fine, so the handoff's
"keep it in Python" path holds.

The one real cost is release engineering, not code: every native library in the folder has to be signed (and the
macOS app notarized) by the existing release workflow, and the Windows executable signed with the app's
certificate so SmartScreen and antivirus don't flag it. That work belongs to the release piece.

### What's left before the agent ships in a release

- Bundle the chosen folder as a Tauri resource per platform (`tauri.conf.json` `bundle.resources`), and sign every
  native library in it in the existing release workflow. That's release work (piece R or I), not this piece.
- The existing Desktop CI's "Unsigned macOS arm64 bundle" job (`macos-14`) is cancelled within a second with no
  runner. It isn't an account limit: this PR's probe got `macos-15` runners on the same account every time. The
  `macos-14` image looks retired, so `desktop.yml` needs the same one-word change (not owned by D, so not made here).
- An Intel Mac build (the app ships macOS universal) wasn't probed; PyInstaller can't make universal2 folders from a
  non-universal Python, so a release would build arm64 and x86_64 agents separately or use B.

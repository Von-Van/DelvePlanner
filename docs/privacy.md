# Privacy and data flows

DayPlan is local-first: planner data lives in SQLite on the device, the AI planner runs on the device, and there is no DayPlan account or server. This page lists what DayPlan stores, what leaves the device and when, and what the AI model sees. Each statement describes the current implementation; the code and tests that back the main ones are linked. Where something is planned rather than built, it says so.

## In short

- **No account, no DayPlan server, no telemetry.** The app has no analytics or crash reporting. The renderer can't make network requests: its content security policy allows `connect-src 'self'` only, and it contains no `fetch`, XHR, or WebSocket code.
- **Planner data never leaves the device** unless you export it to a file yourself.
- **AI runs locally.** The bundled model server listens only on `127.0.0.1` and is started with Ollama's cloud features disabled. There is no cloud AI or remote fallback.
- **Network access follows features you turn on:** calendars you add, a model download you approve, an update check you start, and links you open.
- **Calendar access is read-only.** DayPlan requests read-only scopes, and every request to a calendar API is a `GET`.

## What leaves the device, and when

| When                                                                       | Where it goes                                                                                | What DayPlan sends                                                                                      | What comes back                                     |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| You subscribe to a calendar link, then every 30 minutes while DayPlan runs | The link's host (for example `calendar.google.com`), over HTTPS only                         | A `GET` of the link, a `DayPlan/<version>` user agent, and cache validators from the last response      | The iCalendar feed                                  |
| You connect a Google or Outlook account                                    | Your browser opens the provider's sign-in page                                               | DayPlan's public client ID, the read-only scope, a PKCE challenge, and a loopback redirect address      | A one-time code, delivered to `127.0.0.1`           |
| Right after sign-in, on token refresh, and on disconnect                   | `oauth2.googleapis.com` or `login.microsoftonline.com`                                       | `POST`s carrying the sign-in code and PKCE verifier, a refresh token, or (Google only) a revoke request | Access and refresh tokens                           |
| Every 30 minutes for connected calendars, and when you pick calendars      | `www.googleapis.com/calendar/v3` or `graph.microsoft.com/v1.0`                               | `GET`s with the access token and the date window                                                        | The calendar list and events                        |
| You approve downloading `qwen3:8b`                                         | Ollama's model registry, contacted by the bundled Ollama server                              | A standard model pull request                                                                           | About 5.2 GB of model data                          |
| You select **Check for updates**                                           | `github.com/Von-Van/DayPlan/releases/latest/download/latest.json`, then the package it names | Standard HTTPS `GET`s                                                                                   | Update information; installs need a valid signature |
| You select **Model details & license** in onboarding                       | Your browser opens `ollama.com`                                                              | —                                                                                                       | —                                                   |

DayPlan's own code makes no other network requests: the Rust side makes HTTP calls only in the calendar modules, to the local Ollama server, and through the updater, and the renderer makes none. Plans, tasks, events, notes, people, the inbox, AI requests and replies, logs, and diagnostics are never sent anywhere by DayPlan. The diagnostic bundle is written to a file you choose and goes nowhere unless you share it.

## What stays on the device

| Data                                                                      | Where                                        | Notes                                                                                 |
| ------------------------------------------------------------------------- | -------------------------------------------- | ------------------------------------------------------------------------------------- |
| Plans, milestones, workstreams, tasks, events, people, inbox, time blocks | `dayplan.sqlite3` in the app data directory  | Included in exports and backups                                                       |
| Working hours, planning profile, carry-forward history                    | `dayplan.sqlite3`                            | Left out of exports; included in the database backups                                 |
| Backups of the planner database                                           | `backups/` next to it                        | Taken before migrations, imports, and restores; newest five kept                      |
| Calendar list, accounts, cached events, last iCalendar document           | `calendars.sqlite3`                          | Never exported or backed up; deleted with the calendar or account                     |
| Calendar links and OAuth refresh tokens                                   | macOS Keychain or Windows Credential Manager | Never in SQLite, exports, backups, logs, or the renderer                              |
| OAuth access tokens                                                       | Rust process memory                          | Gone when DayPlan quits                                                               |
| AI conversation and pending proposal                                      | Rust process memory                          | Up to four turns; one proposal for ten minutes; never written to disk                 |
| Downloaded models, chosen model, model check results                      | `ai-models/`, `ai-runtime.json`              | Removed with **Remove model** (the machine's own Ollama models are only ever read)    |
| Logs                                                                      | The app log directory                        | Event codes only; see below                                                           |
| UI conveniences                                                           | The WebView's `localStorage`                 | Whether onboarding is done, dismissed planning prompts, time blocks you chose to keep |

[Data model](data-model.md) describes each store in detail.

There is no application-level database encryption. The files are protected by your OS account and, when enabled, FileVault or BitLocker. Secrets are the exception: calendar links and refresh tokens are held by the OS credential store.

## The AI planner

- **Where:** a bundled Ollama server on a private loopback port, started with `OLLAMA_NO_CLOUD=1` and `OLLAMA_NOHISTORY=1`. It is not reachable from other machines and ignores any Ollama already running on the system.
- **What the model sees:** the request, date context, and a small ranked set of records — titles, dates, times, durations, statuses, and IDs of up to 40 events and, for planning requests, related plans, milestones, and tasks — plus the last four turns of the conversation.
- **What it never sees:** event notes and locations; plan, milestone, and task descriptions; estimates; people and workstreams; inbox items; time blocks; other apps' calendars; working hours; the planning profile; and observations. A regression test ([`prompt.rs`](../src-tauri/src/agent/prompt.rs), `context_never_carries_notes_descriptions_locations_or_people`) checks this.
- **What is kept:** nothing on disk. The conversation and a pending proposal live in memory and end with **Clear conversational context** (↺ on the planner card) or quitting.
- **What it can change:** nothing directly. It proposes; you review each suggestion; Rust applies what you accept in one transaction.

[The AI planner](ai.md) covers the design in full.

## Calendars and sign-in

- DayPlan has no sign-in of its own. OAuth is used only for optional Google and Outlook connections, in your system browser; DayPlan never sees your password.
- Scopes are read-only: `calendar.readonly` for Google; `Calendars.Read` and `offline_access` for Microsoft. A unit test (`asks_only_for_read_only_scopes` in [`oauth.rs`](../src-tauri/src/calendar/oauth.rs)) pins them, and the provider tests assert that every calendar request is a `GET`.
- What is kept from each event: title, location, times, and busy, tentative, and recurring flags. For link and file calendars, the last fetched document is also cached as-is so the date window can move without a new fetch; that copy contains whatever the feed includes, such as descriptions or attendees.
- A connected account is labelled with the address on its primary calendar, stored in `calendars.sqlite3`.
- Other apps' events are never sent to the AI model, exported, or backed up.
- Unsigned macOS builds ask again for keychain access after each update, because macOS ties keychain access to the app's code signature.

[Calendar integration](calendar-integration.md) covers the design, and [calendar accounts](calendar-accounts.md) covers the OAuth clients.

## What DayPlan knows about you

The **What DayPlan knows** screen (⌘7, or Ctrl+7 on Windows) holds everything DayPlan has about how you plan.

- **What you've told it** is the planning profile: preferred hours, a daily limit, focus-block and break lengths, days off, and when demanding work suits you. Every field is optional and starts empty. Today, days off and the daily limit feed capacity; the other fields are stored for planned features and aren't used yet. **Forget all of this** empties the profile. It is left out of exports.
- **What it noticed** is four observations — how long work takes against estimates, when you usually start blocked work, what a blocked day usually holds, and which weekday work most often moves off — computed in Rust each time the screen opens and never stored. Each says what it rests on and needs at least five records. Switching one off stops it being computed.
- The only history kept for this is the carry-forward log: when a task moves off a day it was already on, DayPlan records the task's ID, the two days, the kind of move, and when it happened — no title, notes, or other content. **Forget the history behind these** clears it.

## Logs and diagnostics

- DayPlan's own log lines are fixed event codes: `app_started`, `planner_model_chosen`, `ai_runtime_shutdown`, `ai_runtime_stopped_idle`, `ai_runtime_leftover_ended`, calendar problem codes such as `calendar_refresh_failed code=link_not_found`, keychain availability codes, and `oauth_token_rejected` with the provider's error code. They never include commands, titles, names, emails, notes, descriptions, proposal contents, calendar names, links, events, or file paths.
- Only DayPlan's own messages at Info level and above are written: the current file plus up to five rotated 512 KB files. Builds up to and including v0.3.5 also wrote a second, unfiltered copy of Info-level messages from every library into the same file, so each line appears twice there; that was fixed after v0.3.5.
- **Export diagnostics** in Settings writes a ZIP only when you ask. It contains version and health metadata — app, OS, and architecture; database readiness, schema, and backup count; AI runtime phase, Ollama version, model name, digest, license, and storage size; and the number of calendars with their problem codes — plus the newest five log files.
- In debug builds only, `DAYPLAN_DEBUG_PLANNER=1` prints raw model replies to the terminal. Release builds never print requests or replies.

## Notifications

Reminders are delivered by the operating system while DayPlan runs in the tray. A notification shows the event's title and start time, never its notes. DayPlan never asks for notification permission on its own: onboarding offers an optional **Allow notifications** button, and turning on a reminder asks if permission hasn't been granted yet.

## Planned changes that affect privacy

- The [roadmap](../ROADMAP.md) plans to let the AI planner use the planning profile and capacity facts. Those facts would join the local model's context; they would still not leave the device.
- Google verification and release signing are pending. Until they're done, Google shows an unverified-app warning and unsigned macOS builds repeat keychain prompts after updates.

Cloud AI, sync, DayPlan accounts, collaboration, and telemetry are not planned.

# DayPlan roadmap

DayPlan is the private planning layer between your larger plans and your actual days. The next releases deepen one loop instead of adding project-management breadth:

**Capture → Organize → Plan → Week → Today → Calendar → Execute → Replan**

Each priority from the September 2026 product development handoff ships as one version, in the handoff's order. The commit subject, tag, and release title all use the version name.

Every version answers to one test: can someone with several things going on open DayPlan and quickly see what deserves their attention this week and what to work on today?

| Version | Priority | Theme                          | Contents                                                                  |
| ------- | -------- | ------------------------------ | ------------------------------------------------------------------------- |
| v0.2.5  | 1        | Core planning workflow         | Inbox, estimates, weekly and daily planning, carry-forward, promotion     |
| v0.2.9  | 2        | Calendars without accounts     | Calendar links and files, time blocks, capacity                           |
| v0.3.0  | 2        | Calendar integration           | Google and Outlook accounts                                               |
| v0.3.2  | 3        | AI planning                    | Plan creation and breakdown, inbox, week, day, and replanning proposals   |
| v0.3.5  | 4        | Personal planning intelligence | Planning profile, local observations, What DayPlan Knows                  |
| v0.4.0  | 5        | Quality of life                | Templates, recurring tasks, checklists, duplication, dependencies, polish |

Other patch numbers stay free for fixes and intermediate builds. v0.2.9 is one: it shipped the part of Priority 2 that needs no account keys.

## Every version

- Bump `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, and their lockfiles; `npm run version:check` must pass.
- New mutable records carry revisions. Schema changes get a transactional migration with a backup first, a test that migrates the previous schema, and an export format bump that still imports older files.
- The renderer never gets SQL. Rust keeps persistence, validation, transactions, native integrations, model lifecycle, and AI proposal ownership.
- All CI gates pass: formatting, frontend tests and build, `cargo fmt`, `cargo test`, `cargo clippy`, npm audit, `cargo audit`, and `cargo deny`.
- The eval gate runs three times on one model digest (100% schema, 100% safety, at least 85% exact, at least 95% fields) whenever planner prompts, context, operations, or task records change.
- The README (feature table, schema paragraph, privacy notes, scope limits) and a `## vX.Y.Z` section in [CHANGELOG.md](CHANGELOG.md) are updated. The release workflow uses that section as the release notes.
- One commit named for the version, then an annotated tag with the same name.

## v0.2.5 — Core planning workflow

Priority 1. A complete planning loop that works without AI. The planner's operations don't change in this version.

Build order: inbox, estimates, weekly planning, daily planning, carry-forward, then promotion between Plan, Week, and Today.

### Task planning state

One task record moves between horizons. Nothing is copied.

| State     | Field           | Meaning                                 |
| --------- | --------------- | --------------------------------------- |
| In a plan | `plan_id`       | Part of the plan's backlog              |
| This week | `planned_week`  | Chosen for a week, with no day yet      |
| Scheduled | `scheduled_day` | To be worked on that day                |
| Due       | `due_date`      | A deadline only; never implies week/day |

- `planned_week` stores the first day of the chosen week, and queries match by range, so changing the week start doesn't orphan selections.
- A scheduled task counts toward its day's week without needing `planned_week`. Returning a task to the weekly pool clears `scheduled_day` and sets `planned_week`; unscheduling clears both and keeps the plan.
- The rule that a task needs a plan, scheduled day, or due date (in `db/planning.rs`, `db/proposals.rs`, and `taskHasHome`) also accepts `planned_week`.

### Features

1. **Inbox and quick capture.** A new `inbox_items` table holds text and optional notes, with no plan, date, or priority required. Capture works from a keyboard shortcut and a capture field; an Inbox view in the sidebar shows the unprocessed count. Processing turns an item into a task (standalone or in a plan, optionally scheduled, chosen for a week, or due), a plan, or an event in one transaction. Items can also be deleted.
2. **Estimates.** Tasks gain an optional `estimated_minutes`, with 15, 30, 45, 60, and 120-minute presets or a custom value. Rows show the estimate, and weekly and daily planning total estimated work while counting unestimated tasks separately.
3. **Weekly planning.** A Plan the Week session, separate from the Week calendar and offered when a new week starts. It shows unfinished work from last week, active plans, upcoming milestones, overdue work, tasks due soon, unscheduled plan work, and what's already scheduled. Choosing a task sets `planned_week`, and the session ends with the week's selection and its estimated total.
4. **Daily planning.** A Plan Today session shows tasks already scheduled, tasks due today, this week's unscheduled selection, overdue work, and milestone pressure. Today then lists what was planned (events and scheduled tasks); due and overdue work appears as prompts to plan rather than being added to the day.
5. **Carry-forward.** Open tasks whose scheduled day has passed collect in an Unfinished section on Today and in daily planning, and last week's unfinished selections come first in weekly planning. Each task, or a selection of tasks, can move to today, tomorrow, a day later this week, or another date, go back to the weekly pool, be unscheduled while staying in its plan, be marked done, or be deleted. Nothing moves automatically, and bulk changes apply in one revision-checked transaction.
6. **Plan → Week → Today.** Plan task lists can add work to this week or schedule it for a day. Weekly planning can schedule a day or return work to its plan, and Today can send work back to the week or unschedule it.

### Data, planner, and docs

- Schema 5 adds `inbox_items`, `tasks.estimated_minutes`, and `tasks.planned_week`. Export format 5 imports formats 3 through 5, and existing tasks migrate unchanged.
- Planner operations must preserve the new fields: `schedule_task` keeps `planned_week`, and `update_task` keeps estimates. Run the eval gate because task records change.
- The README covers the Inbox, the task planning state, and schema 5.

**Done when:** someone can capture a vague thought, file it into a plan, choose it for this week, schedule it for a day, and deal with it the next morning when it's unfinished, all without AI and without duplicate tasks.

**Decided:**

- Converting an inbox item removes it; the new record carries its text.
- Today keeps its "Due this day" list.
- A quiet card on Today offers Plan the Week when a new week starts and Plan Today on the first visit each day, until the user plans or dismisses it.

## v0.3.0 — Calendar integration

Priority 2. DayPlan learns when you're actually busy. External calendars stay read-only and separate from DayPlan's own events. v0.2.9 shipped everything in this section except Google and Outlook sign-in.

Build order: sync design and provider architecture, iCalendar links and files, time blocks, capacity, then Google and Outlook once their OAuth clients exist. Links and files come first because they need no keys and exercise the same cache, display, and capacity code the account connections will use.

- **Sync design first.** [CALENDAR_SYNC.md](CALENDAR_SYNC.md) covers data ownership, caching, refresh, recurring events, and revocation, and is reviewed before provider code lands.
- **iCalendar links and files.** Subscribing to an https or webcal link (Google's secret iCal address, an Outlook published calendar) keeps a calendar refreshed; an imported `.ics` file is a snapshot a newer file replaces. Both are read-only calendars in the same cache as the account connections.
- **Provider architecture.** A Rust `CalendarProvider` interface normalizes every provider into one external-event shape: provider, calendar, source ID, title, start and end or all-day date, time zone, busy or free, and recurrence instance. Google and Outlook share the scheduling, display, and capacity code.
- **Authorization.** OAuth runs in Rust with the system browser, PKCE, and a loopback redirect, using read-only scopes. Tokens live in the OS keychain and never reach SQLite, exports, backups, diagnostics, or the renderer.
- **Storage.** External events live in a Rust-owned, non-authoritative cache kept apart from the planner database, in a separate SQLite file. The cache is cleared on disconnect and excluded from export and backup. External events never get DayPlan event IDs or mix with `schedule_events`.
- **Display.** Today and Week show external events in a distinct read-only style, with all-day events, overlaps, expanded recurring instances, and time zones handled. Revoked or expired access shows a reconnect prompt and never affects DayPlan data.
- **Time blocks.** A `task_blocks` table links reserved time (start, duration, time zone, revision) to a task, shown on the agenda apart from events. A task can have several blocks. Moving or deleting a block never changes the task; deleting a task removes its blocks; completing a task offers to release its future blocks and keeps past ones. Blocks stay local, with no write-back to external calendars.
- **Capacity.** Planned work is the estimated time of open tasks chosen for or scheduled in the period. Available time is planning hours minus DayPlan events and busy external events. DayPlan shows the numbers and warns when planned exceeds available ("Planned 17h, available 11h") but never rearranges anything. Planning hours come from a minimal working-hours setting (a Rust-owned, revision-checked settings record) that v0.3.5 grows into the planning profile.
- **Docs.** The README's privacy section explains that DayPlan contacts Google or Microsoft only after a calendar is connected, and the scope limits no longer list sync.

**Done when:** a connected Google calendar appears read-only in Today and Week, a task can be blocked into free time, the week shows planned against available hours, and disconnecting removes every cached calendar record.

**Shipped in v0.2.9:** the sync design, iCalendar links and files, the calendar cache and refresh worker, keychain-held links, Today and Week display, time blocks, working hours, and capacity. Google and Outlook sign-in remain for v0.3.0.

**Decided:**

- An imported `.ics` file becomes a read-only calendar, not editable DayPlan events.
- Calendar links are secrets: each one lives in the OS keychain, like the OAuth tokens that follow.
- Working hours are a planner-database record that exports leave out.
- Hidden calendars don't count toward busy time, and all-day events count only when they're marked busy.

**Prerequisites for you** ([CALENDAR_ACCOUNTS.md](CALENDAR_ACCOUNTS.md) walks through the first two):

- A Google Cloud OAuth client and consent screen. Calendar scopes need Google's app verification before a broad release: unverified apps show a warning and have a user cap, and apps left in Testing status get refresh tokens that expire after seven days.
- A Microsoft Entra app registration for Outlook.
- Release signing. macOS ties keychain access to the app's code signature, and an ad-hoc signature changes with every build, so unsigned updates would ask for keychain access again after each install.

## v0.3.2 — AI planning

Priority 3. The local planner helps with each step of the loop, always through request → constrained proposal → validation → preview → approval → transactional change. The model still has no database access, and no new path bypasses that boundary.

Build order: plan creation, plan breakdown, inbox processing, weekly planning, daily planning, then replanning, with explanations throughout.

- **Operations.** New schema-constrained operations create workstreams, convert inbox items, choose tasks for a week, and suggest estimates. Replanning reuses the scheduling operations.
- **Explanations.** Each operation can carry a short, length-capped `reason` ("Due in three days", "Its milestone is next week") shown under its preview line. Reasons stay brief by default.
- **Line-by-line review.** Users can accept, edit, or reject individual suggestions and then apply the accepted set atomically. Rejecting a new plan or milestone also drops the operations that depend on it. Today a proposal applies all-or-nothing, so this changes the apply path.
- **Grounded dates.** A date the user didn't give is allowed only when marked as a suggestion and labelled that way in the preview; grounding still rejects invented deadlines.
- **Small starting structures.** Plan creation and breakdown propose a useful handful of milestones, tasks, and (only where they help) workstreams, never exhaustive generic lists.
- **Rust-computed context.** Weekly and daily proposals receive facts computed in Rust (due and milestone dates, priorities, estimates, blocked status, current load, and v0.3.0 capacity) and choose from a ranked, size-capped candidate list so prompts fit the model's 8,192-token context.
- **Eval.** New fixture files cover each workflow, partial acceptance, stated and suggested dates, over-long plans, and prompt injection in inbox text. Every fixture file meets the gates across three runs on one model digest.
- **Docs.** The README's AI permission boundary lists what the planner can now propose.

**Done when:** "I'm moving to Boston around January 15…" becomes a reviewable starter plan, and "I'm not getting any of this done today" becomes a redistribution the user can accept line by line.

## v0.3.5 — Personal planning intelligence

Priority 4. Suggestions adapt to how the user actually plans, locally and transparently.

- **Planning profile.** All optional: working hours (from v0.3.0), preferred planning hours, maximum planned work per day, focus-block length, break behavior, no-work days, and weekend and morning or evening preferences. Capacity and AI proposals use them.
- **Local observations.** Deterministic summaries computed in Rust from local data, such as estimate accuracy from completed time blocks, days or times that work is often moved away from, usual working hours, and typical daily load. Each observation shows the data behind it.
- **What DayPlan Knows.** A Settings page lists the profile and every observation with edit, delete, and off controls. AI explanations can cite what informed them.

**Done when:** every personalized suggestion traces back to settings or observations the user can see, change, or delete.

**Decision before building:** observations about deferral need a local history of carry-forward actions. Recommended: start that history in v0.3.5 alongside its controls, so nothing is recorded before it can be inspected or turned off. The alternative is to start recording in v0.2.5 so there's more history once v0.3.5 ships.

## v0.4.0 — Quality of life

Priority 5. Useful additions after the core loop is solid.

- **Templates.** Event, Trip, Move, Research or School Project, Job Search, and Personal Project templates create a plan with a few starting workstreams, editable right away.
- **Recurring tasks.** A recurrence rule on a task creates the next occurrence when one is done or its day passes. Missed occurrences show up in carry-forward instead of piling up.
- **Checklists.** Lightweight checklist items inside a task, not nested tasks.
- **Plan duplication.** Copies a plan's workstreams, milestones, and open tasks, with the option to shift dates.
- **Lightweight dependencies.** A task can wait on another task, shown as a blocked hint, with no Gantt editing.
- **Polish.** A system-wide quick-capture shortcut, better keyboard navigation, onboarding and a first-plan experience, clearer empty states, better notes and resources, and saved planning preferences.
- **Docs.** The README's scope limits drop recurrence and dependencies, noting that both stay lightweight.

**Done when:** starting a common kind of plan takes one step, and routine work repeats without being re-entered.

## Not planned

Team accounts, multi-user collaboration, cloud workspaces, permission systems, team chat, comments, sprint management, ticketing, CRM features, complex Gantt charts, enterprise dashboards, document or wiki editing, and automation builders. People remain local labels for ownership and context, and workstreams remain lightweight groups.

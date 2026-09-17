# Changelog

## v0.3.0

Google Calendar and Outlook connect directly now. Sign in once in your browser, choose which calendars DayPlan should show, and they stay up to date beside your plans. Everything stays read-only: DayPlan asks only for permission to read, so it can't change, create, or delete anything in your calendars.

### Connected accounts

- **Connect Google** and **Connect Outlook** on the Calendars screen (⌘6, or Ctrl+6 on Windows) sign in through your browser. DayPlan never sees your password.
- Connecting adds nothing by itself. DayPlan lists that account's calendars with your main one already ticked, and shows only the ones you pick. **Add calendars…** on the account reopens the list later.
- Each calendar you add behaves like any other: rename, recolor, hide, refresh, or remove it on its own. Accounts and links both refresh every 30 minutes while DayPlan runs.
- If a sign-in expires or is withdrawn, the account says so and keeps showing its last copy of your events until you **Sign in again**.
- **Disconnect account** withdraws DayPlan's access where the provider supports it, then deletes the sign-in along with every calendar and event cached for that account.
- Sign-ins live in the macOS Keychain or Windows Credential Manager, never in the planner database, exports, backups, diagnostics, or the app's own window.

### Docs

- [CALENDAR_ACCOUNTS.md](https://github.com/Von-Van/DayPlan/blob/v0.3.0/CALENDAR_ACCOUNTS.md) covers the OAuth clients DayPlan ships with and how to point a build at your own, and [CALENDAR_SYNC.md](https://github.com/Von-Van/DayPlan/blob/v0.3.0/CALENDAR_SYNC.md) documents how accounts are stored, refreshed, and revoked.

### Known limitations

- Google shows an "unverified app" warning until DayPlan finishes Google's review, and work or school Outlook accounts often need an administrator to approve it. Subscribing by link still works for any calendar an account connection can't reach.
- These builds aren't code-signed, so after each update macOS asks once for every subscribed calendar and connected account before DayPlan can read it from the Keychain. Choose **Always Allow**.

## v0.2.9

Calendars from Google, Outlook, and other apps without signing in, time blocks for tasks, and planned work against available time. Connecting Google and Microsoft accounts directly comes in v0.3.0.

### Calendars

- A new Calendars screen (⌘6, or Ctrl+6 on Windows) subscribes to a calendar's iCalendar link, such as Google Calendar's secret address or an Outlook published calendar, and refreshes it every 30 minutes while DayPlan runs.
- An exported `.ics` file can be imported as a read-only calendar and later replaced with a newer file.
- Today and Week show calendar events in a distinct read-only style, including all-day events and recurring events with moved, cancelled, or excluded occurrences.
- Calendars can be hidden, renamed, recolored, refreshed, or removed. When a link stops working, Today says so and the calendar keeps its last copy until a new link is pasted.
- Links are stored in the macOS Keychain or Windows Credential Manager. Calendar events are cached apart from planner data and aren't exported, backed up, or given to the local planner.

### Time blocks

- Any open task can have time blocked for it from its menu. The dialog suggests free time inside working hours and warns about overlaps.
- Blocks appear on Today and Week apart from events; selecting one moves or removes it without changing its task.
- When a task is finished, Today offers to release its future blocks and keeps past ones.

### Capacity

- Working days and hours are set on the Calendars screen.
- Today, Week, Plan the Week, and Plan Today compare planned work with the time left after events and busy calendar time ("Planned 17 h, available 11 h") and flag when it's over. Nothing moves on its own.

### Data and docs

- Database schema 6 adds time blocks and working hours. Export format 6 includes time blocks, and formats 1–5 still import.
- [CALENDAR_SYNC.md](https://github.com/Von-Van/DayPlan/blob/v0.2.9/CALENDAR_SYNC.md) describes the calendar design, and [CALENDAR_ACCOUNTS.md](https://github.com/Von-Van/DayPlan/blob/v0.2.9/CALENDAR_ACCOUNTS.md) walks through creating the Google and Microsoft OAuth clients.

### Also in this release

v0.2.5 wasn't published on its own, so this is the first build with its planning workflow: the Inbox and quick capture (⌘I), task estimates, Plan the Week and Plan Today, and carry-forward of unfinished tasks. See [v0.2.5 in the changelog](https://github.com/Von-Van/DayPlan/blob/v0.2.9/CHANGELOG.md#v025).

### Known limitations

- These builds aren't code-signed, so after each update macOS asks once for every subscribed calendar before DayPlan can read its link from the Keychain. Choose **Always Allow**.

## v0.2.5

The core planning workflow: capture anything, choose a week's work, plan each day, and recover unfinished tasks, all without the AI planner.

### Capture

- A new Inbox holds thoughts, tasks, and ideas with no plan, date, or priority required. Capture from the Inbox screen or from anywhere with ⌘I (Ctrl+I on Windows).
- Each item becomes a task, plan, or event when you're ready. The new record is created and the item removed in one step.

### Weekly and daily planning

- Plan the Week shows work left from earlier weeks, overdue and soon-due tasks, tasks behind upcoming milestones, and each active plan's backlog, next to the week's chosen work and its estimated hours.
- Plan Today builds the day from unfinished, due, and chosen work, next to the time already on the calendar.
- Today offers each session in a quiet prompt until you plan or dismiss it, and still lists what's due that day.
- Tasks can be chosen for a week without picking a day, and the Week view shows those tasks above its days.

### Moving work

- Unfinished tasks from earlier days collect on Today. Move them to today, tomorrow, a picked day, this or next week, or back to their plan; mark them done; or move them all at once. Nothing moves on its own.
- Every task row offers the same moves, and each move changes the same task rather than copying it.

### Tasks and data

- Tasks take an optional estimate (15, 30, or 45 minutes, 1 or 2 hours, or a custom length).
- Database schema 5 and export format 5 add the Inbox, task weeks, and estimates. Earlier databases migrate with a backup, and earlier exports still import.

## v0.2.0

The first published DayPlan build, released as an unsigned pre-release for macOS and Windows.

### Plans, tasks, and teams

- Plans with a status, start and target dates, and a color. Plans can be archived and restored, and archived plans can be deleted permanently.
- Milestones with target dates and complete or skipped states, shown on each plan's timeline.
- General tasks with a status, priority, scheduled day or due date, and links to a plan, milestone, workstream, and owner.
- Workstreams inside plans, and people as local owner labels for tasks and events.
- A plan workspace: an overview with the next milestone, attention signals, and upcoming work, plus filtered tasks, the plan's schedule, a timeline, and a run of show for event days.
- A Week view, plan filters on Today and Week, and events that can carry a plan, workstream, owner, and location.

### Local planner

- The planner proposes plan, milestone, and task changes as well as event changes, and works inside the open plan by default.
- Follow-up requests refine a pending proposal. Every change is previewed first and applied in one transaction with revision checks.
- The evaluation set grows to 111 cases, including 43 planning cases.

### Design

- A new visual design: white and silver with one blue accent, faceted cards, geometric marks instead of icons, and 24-hour times.
- Archivo, Cormorant Garamond, and IBM Plex Mono are bundled with the app.

### Data

- Database schema 4. Existing databases are backed up and migrated when DayPlan opens, and export, import, and restore cover plans, milestones, tasks, workstreams, and people.

### Security

- Updates rustls to 0.23.45 for [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285).

# Changelog

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

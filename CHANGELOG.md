# Changelog

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

# Contributing to DayPlan

Thanks for looking. DayPlan is a small, owner-directed project, so the most useful contributions are focused: bug reports with clear steps, fixes that come with a test, and documentation corrections. For anything larger, open an issue first so the direction can be agreed before code is written.

## What DayPlan is trying to be

DayPlan sits between a daily planner and a project-management suite: plans give today's work its context, and the week and the day stay the center. New work should deepen that loop — capture, organize, plan the week, plan the day, execute, replan — rather than add project-management breadth. Team accounts, collaboration, cloud workspaces, permission systems, sprints, Gantt editing, and automation builders are deliberately out of scope ([roadmap](ROADMAP.md#not-planned)).

## Getting set up

[Development](docs/development.md) covers prerequisites, running the app, the optional local AI runtime, every command, and the release process. [Architecture](docs/architecture.md) explains how the code is organized; start there before a change that crosses layers.

Where things live:

- **Planning rules and storage:** `src-tauri/src/db.rs` and `src-tauri/src/db/` (Rust), with pure display rules in `src/planning.ts`.
- **Calendars:** `src-tauri/src/calendar.rs` and `src-tauri/src/calendar/`.
- **AI planner:** `src-tauri/src/agent.rs`, `src-tauri/src/agent/`, and `src-tauri/src/runtime.rs`; evaluation cases in `eval/`.
- **Interface:** `src/`, with every backend call going through `src/api.ts`.

## Ground rules

These keep DayPlan's guarantees intact. Most are checked in review rather than by tooling.

- **Rust owns persistence and integrations.** The renderer never gets SQL, file paths, secrets, or network access. Validation, transactions, native integrations, model lifecycle, and AI proposal ownership stay in Rust.
- **Every mutable record carries a revision**, and updates are revision-checked. A schema change needs a transactional migration (with the automatic backup), a test that migrates the previous schema, and — if the data is exported — a format bump that still imports older files.
- **The AI planner proposes; it never writes.** A new planner capability goes through request → constrained proposal → validation → preview → approval → transaction. No new path may bypass that, and planning features must keep working without AI.
- **Calendar access stays read-only.** Don't add OAuth scopes or write calls. A change to what DayPlan requests needs a documented reason.
- **Privacy claims must stay true.** Don't log user content. If a change alters what is stored, sent, or shown to the model, update [privacy](docs/privacy.md) in the same pull request.
- **Be sparing with dependencies.** Add one only when it earns its place, and prefer what the project already uses.
- **Never commit** local databases, logs, planner content, exports, credentials, signing material, model or runtime binaries, or unredacted diagnostics. `.gitignore` covers the usual file names, but check what you stage.

## Pull requests

Keep pull requests small and about one thing. In the description, say what changed, why, and what you considered and rejected. Before opening one:

- [ ] The [quality gates](docs/development.md#quality-gates) pass locally.
- [ ] New behavior has tests — Rust tests beside the code, Vitest tests for pure frontend modules.
- [ ] The eval gate ran if planner prompts, context, operations, or task records changed ([how](docs/development.md#evaluating-the-ai-planner)).
- [ ] Docs match the change: the README's feature list, the affected pages in `docs/`, and user-facing notes for the next `CHANGELOG.md` section.
- [ ] Native behavior (tray, notifications, file dialogs, keychain, updates) was tried in the app if the change touches it.
- [ ] No planner content, personal data, or secrets appear in tests, fixtures, logs, or screenshots.

## AI-assisted development

DayPlan is developed with AI coding assistants, including Anthropic's Claude through Claude Code, which is credited as a co-author on the release commits it helped write. Assistants help with implementation, debugging, refactoring, code review, documentation, prototyping, and trying alternative approaches.

The project owner is responsible for everything that makes DayPlan what it is: product direction and feature selection, requirements and UX decisions, architectural constraints, privacy and integration decisions, technical tradeoffs, and validating, reviewing, and accepting every change. The roadmap's recorded decisions, the release checklist, the evaluation gates, and the tests are where that judgment is written down.

The same standard applies to contributions. You may use AI tools; you are responsible for what you submit. Understand every line, run the gates, don't include content you can't vouch for, don't paste private planner data or secrets into an assistant, and mention substantial AI assistance in the pull request description.

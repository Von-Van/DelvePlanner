# Delve Planner roadmap

Delve Planner deepens one loop rather than adding project-management breadth:

**Capture → Organize → Plan → Week → Today → Calendar → Execute → Replan**

What each version shipped is in the [changelog](CHANGELOG.md). How a version is cut is in [development](docs/development.md#releases).

## Next

- **The planner uses the rest of the planning profile.** Days off, the daily limit, and busy time already shape the spare time the planner sees when it picks a day. Preferred hours, focus and break lengths, and energy preference are recorded but not used yet.
- **A fresh eval run.** The last full gate (September 16, 2026) covered 111 cases; the 15 added since have only had partial runs.
- **Google app verification.** Calendar scopes are sensitive: unverified apps show a warning and have a lifetime cap of 100 new users, and a client left in Testing status hands out refresh tokens that expire after seven days.
- **Microsoft publisher verification**, so Outlook stops being labelled unverified. It needs a partner account for a legal entity; a personal registration can't have it.
- **Release signing.** macOS ties keychain access to the app's code signature, and an ad-hoc signature changes with every build, so unsigned updates ask for keychain access again after each install — once per calendar link and once per account token. The signed pipeline is built; it waits on credentials.

## Decisions worth keeping

- A task is one record that moves between horizons (plan backlog, a week, a day); it is never copied. A due date is only a deadline and never implies a week or day.
- Converting an inbox item removes it; the new record carries its text.
- Nothing moves automatically. Carry-forward and capacity show what doesn't fit; the user decides.
- An imported `.ics` file becomes a read-only calendar, not editable Delve Planner events. Calendar links are secrets and live in the OS keychain, like OAuth tokens.
- Hidden calendars don't count toward busy time, and all-day events count only when marked busy.
- The carry-forward history keeps only the two days and the moment — never a title or note — and can be cleared from the same screen that shows it.
- A repeating task keeps one open occurrence at a time. A missed occurrence waits with unfinished work until it's moved, skipped, or finished, instead of new ones piling up.

## Not planned

Team accounts, multi-user collaboration, cloud workspaces or sync, two-way calendar sync, cloud AI, permission systems, team chat, comments, sprint management, ticketing, CRM features, Gantt charts, enterprise dashboards, document or wiki editing, and automation builders. People remain local labels for ownership and context, and workstreams remain lightweight groups.

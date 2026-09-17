# Calendar sync design

DayPlan v0.3.0 learns when you're actually busy. This is the design the calendar code follows: who owns which data, how calendars are cached and refreshed, how recurring events and time zones are read, and what happens when access ends.

**Status:** iCalendar links and files, time blocks, working hours, and capacity shipped in v0.2.9. Google and Microsoft accounts reuse everything below once OAuth clients exist; [CALENDAR_ACCOUNTS.md](CALENDAR_ACCOUNTS.md) covers creating them.

## Principles

- **Read-only.** DayPlan never writes to another calendar. Events from other calendars can't be edited, linked to plans, reminded about, or turned into DayPlan events.
- **Separate.** External events never get DayPlan event IDs, never mix with `schedule_events`, and aren't exported, backed up, restored, or sent to the local planner.
- **Local.** There is no DayPlan server. DayPlan contacts a calendar service only for calendars the user adds, and only from Rust.
- **Secrets stay in the keychain.** A private calendar link grants read access to that calendar, so it's treated like an OAuth token.
- **Nothing moves on its own.** Capacity reports planned work against available time and never reschedules anything.

## Data ownership

| Data                                                          | Lives in                                     | Exported | In backups | Removed when            |
| ------------------------------------------------------------- | -------------------------------------------- | -------- | ---------- | ----------------------- |
| Plans, tasks, events, inbox, time blocks                      | `dayplan.sqlite3` (schema 6)                 | Yes      | Yes        | The user deletes them   |
| Working hours                                                 | `dayplan.sqlite3`                            | No       | Yes        | Never; they're edited   |
| Calendars: name, color, visibility, source host, sync state   | `calendars.sqlite3`                          | No       | No         | The calendar is removed |
| The last fetched or imported calendar document and its events | `calendars.sqlite3`                          | No       | No         | The calendar is removed |
| Calendar links (later, OAuth tokens)                          | macOS Keychain or Windows Credential Manager | No       | No         | The calendar is removed |

- The renderer receives a calendar's host or file name (`calendar.google.com`, `family.ics`), never its link. It sends a link to Rust once, when the user pastes it.
- Keychain items use the bundle identifier as the service and `calendar-link:<calendar id>` as the account, one item per calendar. Windows limits a credential to 2,560 bytes, so links are capped at 1,200 characters.
- Logs and diagnostic bundles record only calendar counts and problem codes such as `link_not_found`, never names, links, or events.
- Restoring a planner backup or importing an export leaves calendars untouched. Losing `calendars.sqlite3` loses only cached copies and the calendar list; the planner never depends on it.

## One event shape for every provider

Each provider produces occurrences for a window of local days; storage, display, and capacity don't know where they came from.

| Field                            | Meaning                                                           |
| -------------------------------- | ----------------------------------------------------------------- |
| `calendarId`, `key`              | The calendar, and a stable hash of the event's UID and start      |
| `title`, `location`              | Trimmed to 200 characters; descriptions and attendees aren't kept |
| `startAtUtc`, `endAtUtc`         | Timed occurrences                                                 |
| `startDate`, `endDate`           | All-day occurrences in local dates, end exclusive                 |
| `busy`, `tentative`, `recurring` | Availability and display hints                                    |

| Provider          | Source                                                                                                       | Status                          |
| ----------------- | ------------------------------------------------------------------------------------------------------------ | ------------------------------- |
| iCalendar link    | Any https or webcal feed, such as Google's secret iCal address or an Outlook published calendar              | Built                           |
| iCalendar file    | An exported `.ics` file; changes only when a newer file replaces it                                          | Built                           |
| Google Calendar   | `calendarList.list`, then `events.list` with `singleEvents=true` inside the window; `transparency` sets busy | Needs a Google OAuth client     |
| Microsoft Outlook | `/me/calendars`, then `calendarView` inside the window; `showAs` sets busy and tentative                     | Needs an Entra app registration |

OAuth runs in Rust with the system browser, PKCE, a loopback redirect (`127.0.0.1` for Google, `localhost` for Microsoft, both on a random port), and read-only scopes. Tokens go to the keychain under the same rules as links.

## Caching and the window

- DayPlan keeps occurrences from **6 weeks before today to 400 days after**, in the device's time zone. Week navigation beyond that shows no external events.
- Each calendar also keeps its last successfully read document, so the window moves forward at midnight (or when the device's zone changes) by re-reading stored content instead of fetching again.
- Limits: 20 calendars, 20 MB per document, 50,000 occurrences per calendar, and 10,000 generated instances per recurring series.
- Replacing a calendar's content happens in one transaction: events, document, and sync state change together or not at all.

## Refresh

- **Adding a link** fetches and parses it first. Nothing is stored, and the link never reaches the keychain, unless it's a readable calendar.
- **Link calendars** refresh every 30 minutes while DayPlan runs (it stays in the tray when the window closes), plus on **Refresh now**. Requests send `If-None-Match` and `If-Modified-Since` when the last response had validators, and accept gzip.
- **Files** never refresh; **Replace with a newer file** swaps their content.
- A worker checks every minute for due calendars and moved windows, refreshes one calendar at a time, and never runs two refreshes of the same calendar at once. When anything changed it emits `calendars-changed`, and Today, Week, and Calendars reload.
- Sync bookkeeping never changes a calendar's revision, so a background refresh can't conflict with a rename.

### Failures

| Problem            | Cause                                             | Next try   | What the user sees                    |
| ------------------ | ------------------------------------------------- | ---------- | ------------------------------------- |
| `unreachable`      | No network, timeout, DNS or TLS failure           | 15 minutes | Status on the calendar                |
| `server_error`     | 5xx                                               | 15 minutes | Status on the calendar                |
| `rate_limited`     | 429                                               | 2 hours    | Status on the calendar                |
| `link_not_found`   | 404 or 410, usually a reset or unpublished link   | 6 hours    | Notice on Today; **Paste a new link** |
| `link_refused`     | 401, 403, or another 4xx                          | 6 hours    | Notice on Today; **Paste a new link** |
| `not_a_calendar`   | The response isn't iCalendar                      | 6 hours    | Notice on Today                       |
| `too_large`        | Over 20 MB                                        | 6 hours    | Notice on Today                       |
| `link_unavailable` | The keychain item is missing or access was denied | 6 hours    | Notice on Today; **Paste a new link** |

The last good copy stays visible through every failure. Capacity marks its numbers as possibly incomplete while any visible calendar has a problem.

## Recurring events and time zones

- Events are grouped by UID. Moved and cancelled occurrences (`RECURRENCE-ID`) replace their originals within the same UID, whatever their `SEQUENCE`; `EXDATE` removes dates and `RDATE` adds them.
- Open-ended rules are cut off just past the window before expansion, and a series that starts after the window contributes only moved occurrences that land inside it.
- Local wall-clock times hold across daylight saving changes: a daily 06:30 meeting stays at 06:30.
- Zones resolve from IANA names, Windows names ("Eastern Standard Time"), labels like "(UTC+01:00) Amsterdam, Berlin", vendor prefixes, `X-LIC-LOCATION`, and Outlook's zone IDs. Floating times and anything unresolvable use the device's zone.
- An event without an end takes no time; an all-day event without one covers its day.
- **Busy:** Outlook's `X-MICROSOFT-CDO-BUSYSTATUS` wins (free and working elsewhere are free; busy, tentative, and out of office are busy), then `TRANSP`. Without either, timed events are busy and all-day events are free, so holiday and birthday calendars don't erase a day.

Parsing and expansion use [calcard](https://crates.io/crates/calcard) (Apache-2.0 or MIT), called once per UID with the adjustments above.

## Revocation and removal

- **A reset or unshared link** fails with `link_not_found` or `link_refused`. Pasting a new link checks it before the keychain item is replaced, so a bad paste never loses the old link.
- **Removing a calendar** deletes its keychain item, then its row, document, and occurrences in one transaction. If the keychain refuses the deletion, the calendar is still removed and the failure is logged without the link.
- **OAuth (later):** a failed token refresh or revoked grant becomes a reconnect prompt without touching planner data. Disconnecting revokes the grant where the provider supports it and deletes the tokens and every cached record.
- **Unsigned macOS builds:** macOS ties keychain access to the app's code signature, and ad-hoc signatures change with every build, so after each update macOS asks once per calendar to allow DayPlan to read its link. Developer ID signing removes the prompt.

## Time blocks

- `task_blocks` (schema 6) stores a task, a UTC start with its IANA zone, a length of 5 minutes to 24 hours, and a revision. A task can have any number of blocks.
- Moving or removing a block never changes its task. Deleting a task deletes its blocks. A finished task can't take a new block.
- When a task is finished, Today offers to release its future blocks and keeps past ones as a record. **Keep** is remembered on that device.
- Blocks appear on Today and Week apart from events, are exported (format 6), and are never written to another calendar.

## Capacity

- **Working hours** are one revision-checked record: working days plus a start and end time, defaulting to Monday–Friday 09:00–17:00. They're a device preference, so exports leave them out; v0.3.5 grows them into the planning profile.
- For each local day:
  - **Working** time is the day's hours in the viewer's zone, following daylight saving changes.
  - **Busy** time is the union of DayPlan events and busy events from visible calendars (a busy all-day event covers the whole day), inside working hours.
  - **Available** time is working minus busy.
  - **Planned** work is the estimates of open tasks scheduled that day, with tasks lacking an estimate counted separately.
  - **Free** time is working hours minus busy time and time blocks, which is what the block dialog suggests.
- A week also counts open tasks chosen for it without a day.
- Time blocks narrow free time but aren't subtracted from available time, because their tasks' estimates already count as planned work.
- Hidden calendars don't count toward busy time.
- Today, Week, Plan the Week, and Plan Today show "Planned 17 h, available 11 h" and flag when planned exceeds available. Nothing is rearranged.

## Not in v0.3.0

Writing to other calendars, attendees and descriptions, editing external events, CalDAV accounts, per-day working hours (v0.3.5), and giving the local planner calendar or capacity facts (v0.3.2).

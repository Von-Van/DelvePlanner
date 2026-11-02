# Heads-ups

**Milestone M5.** The assistant brings things up before you ask: a deadline with no time planned, two commitments
at the same time, free focus time, an overbooked day, work that keeps slipping, or a project gone quiet.

## In plain terms

- Heads-ups appear under **Worth attention** on the Assistant home, at most three at a time, each with the facts
  behind it. A quiet week shows nothing: there is no quota to fill.
- Each one has three choices:
  - its action (**Plan it**, **Sort it out**, **Use it**, **Rebalance**, **Decide**, **Check in**) hands it to the
    assistant as a new conversation. The assistant reads the current planner data itself and drafts changes for
    your approval, as with anything you type;
  - **Not now** hides it until tomorrow's working hours start, and it comes back only if it is still true;
  - **Dismiss** means don't show it again. The same heads-up (the same task and due date, the same clash, the same
    day) is never brought up again.
- A heads-up that stops being true clears by itself: plan time for the task, move the clash, and it is gone on the
  next check.
- Urgent ones (a deadline today or tomorrow, a clash today) may also send a system notification: at most two a day,
  never outside working hours or on days off, and never twice for the same heads-up.
- **Settings → Heads-ups** turns them off, or turns off only the notifications.

## How heads-ups are found

Fixed rules in Rust ([`heads_up.rs`](../../src-tauri/src/heads_up.rs)) read the planner's own records, read-only
calendars and capacity. **No model runs to find or word a heads-up**, so a check costs nothing and a week where
no rule fires makes no model call at all. A background worker checks every five minutes while the app runs (also
from the tray), and the app checks again straight after you change planner data.

| Rule            | Fires when                                                                                                                  | Dedupe key                  |
| --------------- | --------------------------------------------------------------------------------------------------------------------------- | --------------------------- |
| Deadline        | An open task is due today to two days out, or was due in the last week, with no time block and no day before its due date   | task and due date           |
| Clash           | Two things overlap by 5 minutes or more between now and the end of tomorrow, and at least one is an event or time block     | both items and their starts |
| Free focus time | Today has a free stretch of at least 60 minutes (or your focus length) left, and high-priority or soon-due work has no time | the day                     |
| Overloaded day  | A day from tomorrow to six days out has 30 minutes or more of planned work beyond its time (or your daily limit)            | the day                     |
| Pushed back     | An open task has been carried off its day three times or more                                                               | the task                    |
| Quiet project   | An active project with open tasks hasn't changed for 14 days                                                                | the project                 |

Two calendar events overlapping each other are left alone: that is the calendar's business. Today's own overload,
unfinished work and tasks due today are still shown by the Assistant home's existing items.

## Limits

- At most three shown at once, highest score first (a deadline today or a clash within three hours ranks
  highest; a quiet project lowest).
- At most six heads-ups newly brought up per local day. Anything over waits for a later check.
- Finished heads-ups (dismissed, acted on, cleared, expired) are forgotten after 60 days.

## Storage

`agent_suggestion` and `heads_up_settings` (schema 12) sit with the other [agent storage](../data-model.md#agent-storage)
tables: not planner data, never exported, left alone by imports, and never written by the model.

## Tests

`src-tauri/src/heads_up/tests.rs` checks the rules at fixed moments (clock-advance fixtures): a quiet week brings up
nothing, a near deadline names the free time and clears once planned, a dismissed heads-up never returns, a
snoozed one returns, an overloaded week names the worst day, clashes need one of our own items, focus time needs
important work and working hours, the daily and notification limits hold, and turning heads-ups off stops checks.

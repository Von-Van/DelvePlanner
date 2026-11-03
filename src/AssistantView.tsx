import { ReactNode, RefObject, useState } from "react";
import {
  AgendaItem,
  agendaItemEnd,
  hoursLabel,
  timeRangeLabel,
} from "./calendars";
import { dayLabel } from "./date";
import { Glyph, LoadingLine, Mark } from "./Geometry";
import { HeadsUp, HeadsUpKind } from "./headsUpApi";
import { HeadsUpDismiss } from "./HeadsUpDismiss";
import { plural } from "./planning";

/** Where a "Worth attention" item sends the user to deal with it. */
export type AttentionTarget =
  "calendars" | "today" | "plan-week" | "plan-today";

export type AttentionItem = {
  id: string;
  title: string;
  /** The facts behind the item, so it is never a suggestion without a reason. */
  evidence: string;
  action: { label: string; target: AttentionTarget | "assistant" };
  /** Set for a heads-up the assistant brought up, which can be acted on, snoozed or dismissed. */
  headsUpId?: string;
  headsUpKind?: HeadsUpKind;
};

export type HeadsUpChoice = "act" | "snooze" | "dismiss";

/** Proposed changes that wait for the person's decision, from the agent or the planner. */
export type Approval = {
  id: string;
  /** What was asked, or the planner's own summary of its proposal. */
  title: string;
  /** The proposal's one-line summary, when it adds to the title. */
  summary: string | null;
  changes: number;
  from: "agent" | "planner";
};

/** A question the assistant asked that nothing has answered yet. */
export type WaitingQuestion = { id: string; request: string; question: string };

export type AttentionInput = {
  /** Heads-ups from the assistant's fixed rules, most worth saying first. */
  headsUps: HeadsUp[];
  /** Names of calendars that can't refresh. */
  brokenCalendars: string[];
  /** Minutes of planned work beyond the time available today; 0 when it fits. */
  overCapacityBy: number;
  /** Tasks scheduled on an earlier day that aren't done. */
  unfinished: number;
  /** Tasks due today that aren't done. */
  dueOpen: number;
  /** Future time still held for tasks that are already finished. */
  releasable: number;
  /** The planning session that hasn't been done yet this week or today. */
  planningPrompt: "week" | "day" | null;
};

export const maxAttentionItems = 3;

/**
 * Up to three things worth a look today, each built from facts the planner already holds,
 * including the assistant's heads-ups. A quiet day gives an empty list: there is no quota to fill.
 */
export function worthAttention(input: AttentionInput): AttentionItem[] {
  const items: AttentionItem[] = [];
  if (input.brokenCalendars.length > 0)
    items.push({
      id: "calendars",
      title:
        input.brokenCalendars.length === 1
          ? `“${input.brokenCalendars[0]}” can't refresh`
          : `${input.brokenCalendars.length} calendars can't refresh`,
      evidence: "Busy time from them may be out of date.",
      action: { label: "Open Calendars", target: "calendars" },
    });
  // A broken calendar comes first, since busy time everything else rests on may be wrong. The
  // assistant's own heads-ups come next: each is specific and can be acted on.
  for (const headsUp of input.headsUps)
    items.push({
      id: `heads-up-${headsUp.id}`,
      title: headsUp.title,
      evidence: headsUp.evidence,
      action: { label: headsUp.actionLabel, target: "assistant" },
      headsUpId: headsUp.id,
      headsUpKind: headsUp.kind,
    });
  if (input.overCapacityBy > 0)
    items.push({
      id: "capacity",
      title: "Today is overbooked",
      evidence: `Planned work is ${hoursLabel(input.overCapacityBy)} more than the time available.`,
      action: { label: "Open Today", target: "today" },
    });
  if (input.unfinished > 0)
    items.push({
      id: "unfinished",
      title: `${plural(input.unfinished, "unfinished task")} from earlier days`,
      evidence: "Scheduled before today and not marked done.",
      action: { label: "Open Today", target: "today" },
    });
  if (input.dueOpen > 0)
    items.push({
      id: "due",
      title: `${plural(input.dueOpen, "task")} due today`,
      evidence: "Due today and not marked done.",
      action: { label: "Open Today", target: "today" },
    });
  if (input.planningPrompt === "week")
    items.push({
      id: "plan-week",
      title: "This week isn't planned yet",
      evidence: "No weekly planning session since the week started.",
      action: { label: "Plan the week", target: "plan-week" },
    });
  else if (input.planningPrompt === "day")
    items.push({
      id: "plan-today",
      title: "Today isn't planned yet",
      evidence: "No daily planning session today.",
      action: { label: "Plan today", target: "plan-today" },
    });
  if (input.releasable > 0)
    items.push({
      id: "release",
      title: `${plural(input.releasable, "time block")} held for finished work`,
      evidence: "The tasks are done but their future time is still blocked.",
      action: { label: "Open Today", target: "today" },
    });
  return items.slice(0, maxAttentionItems);
}

/** The title of what's on the agenda, whatever kind of item it is. */
export function agendaItemTitle(item: AgendaItem) {
  switch (item.kind) {
    case "event":
      return item.event.title;
    case "block":
      return item.scheduled.task.title;
    case "external":
      return item.event.title;
  }
}

export type DayContext = {
  day: string;
  today: string;
  loading: boolean;
  /** Timed items on the day, in order. */
  items: AgendaItem[];
  /** The item under way or next, when the day is today. */
  focusKey: string | null;
  tasksDone: number;
  tasksTotal: number;
  dueOpen: number;
  milestones: number;
  /** "Planned 5 h, available 6 h", when capacity is known. */
  capacityLabel: string | null;
};

/**
 * The Assistant home: one conversation surface with the local planner, a short view of the day,
 * a few evidence-backed items worth attention, and the approval queue. The capture inbox is a
 * separate place for the user's own notes and is only linked from here, never mixed in.
 */
export function AssistantView({
  headingRef,
  context,
  conversation,
  agent,
  surface = "agent",
  onSurface,
  fallbackNote,
  attention,
  approvals,
  questions = [],
  inboxCount,
  onAttention,
  onReview,
  onOpenTasks,
  onHeadsUp,
  onHeadsUpDismissed,
  onMessage,
  onOpenSchedule,
  onOpenProjects,
  onOpenInbox,
  onBackToToday,
}: {
  headingRef: RefObject<HTMLHeadingElement>;
  context: DayContext;
  /** The planner conversation, with its own proposal review. */
  conversation: ReactNode;
  /** The local agent, with its live steps and run measurements; shown first when given. */
  agent?: ReactNode;
  /** Which of the two is showing; the agent hands a request to the planner when it can't run. */
  surface?: "agent" | "planner";
  onSurface?: (surface: "agent" | "planner") => void;
  /** Why the planner is answering a request the agent couldn't run, or null. */
  fallbackNote?: string | null;
  attention: AttentionItem[];
  /** Every proposal waiting for approval, newest first. */
  approvals: Approval[];
  /** Questions the assistant is waiting on. */
  questions?: WaitingQuestion[];
  inboxCount: number;
  onAttention: (target: AttentionTarget) => void;
  /** Opens a proposal for review, with its changes one by one. */
  onReview: (approval: Approval) => void;
  /** Opens the Tasks board, where questions are answered. */
  onOpenTasks?: () => void;
  /** The person's choice on a heads-up: hand it to the assistant, not now, or don't show again. */
  onHeadsUp?: (id: string, choice: HeadsUpChoice) => void;
  /** A heads-up was dismissed (with any reason and lesson settled): reload the list. */
  onHeadsUpDismissed?: () => void;
  onMessage?: (message: string) => void;
  onOpenSchedule: () => void;
  onOpenProjects: () => void;
  onOpenInbox: () => void;
  onBackToToday: () => void;
}) {
  const isToday = context.day === context.today;
  const showAgent = agent != null && surface === "agent";
  // The heads-up whose "Dismiss" is open, so the person can say why or stop its kind.
  const [dismissing, setDismissing] = useState<string | null>(null);
  return (
    <div className="assistant-home">
      <header className="topbar">
        <div className="date-heading">
          <p>{dayLabel(context.today).toUpperCase()}</p>
          <h1 ref={headingRef} tabIndex={-1}>
            What should we work on?
          </h1>
        </div>
        <div className="date-controls">
          <button className="secondary-button" onClick={onOpenSchedule}>
            Schedule
          </button>
          <button className="secondary-button" onClick={onOpenProjects}>
            Projects
          </button>
        </div>
      </header>

      <div className="assistant-grid">
        <div className="assistant-main">
          {agent != null && (
            <div
              className="assistant-switch"
              role="tablist"
              aria-label="Assistant"
            >
              <button
                role="tab"
                aria-selected={showAgent}
                className={showAgent ? "selected" : ""}
                onClick={() => onSurface?.("agent")}
              >
                Agent
              </button>
              <button
                role="tab"
                aria-selected={!showAgent}
                className={showAgent ? "" : "selected"}
                onClick={() => onSurface?.("planner")}
              >
                Planner
              </button>
            </div>
          )}
          {!showAgent && agent != null && fallbackNote && (
            <p className="assistant-fallback" role="status">
              The agent couldn't run this time ({fallbackNote}), so the planner
              answered instead.
            </p>
          )}
          {/* Both stay mounted so a running agent keeps its live steps while the planner shows. */}
          {agent != null && <div hidden={!showAgent}>{agent}</div>}
          <div hidden={showAgent}>{conversation}</div>
        </div>

        <aside className="assistant-side">
          <DaySummary
            context={context}
            isToday={isToday}
            onOpenSchedule={onOpenSchedule}
            onBackToToday={onBackToToday}
          />

          {attention.length > 0 && (
            <section className="assistant-card" aria-label="Worth attention">
              <p className="side-kicker">WORTH ATTENTION</p>
              <ul className="worth-attention">
                {attention.map((item) => {
                  const headsUpId = item.headsUpId;
                  const target = item.action.target;
                  const action = (
                    <button
                      className="text-button"
                      onClick={() =>
                        headsUpId
                          ? onHeadsUp?.(headsUpId, "act")
                          : target !== "assistant" && onAttention(target)
                      }
                    >
                      {item.action.label} <Glyph>→</Glyph>
                    </button>
                  );
                  return (
                    <li
                      key={item.id}
                      className={headsUpId ? "heads-up" : undefined}
                    >
                      <Mark size={7} color="var(--blue)" />
                      <div>
                        <strong>{item.title}</strong>
                        <small>{item.evidence}</small>
                      </div>
                      {headsUpId ? (
                        <span className="heads-up-choices">
                          {action}
                          <button
                            className="text-button quiet"
                            onClick={() => onHeadsUp?.(headsUpId, "snooze")}
                            aria-label={`Not now: ${item.title}`}
                          >
                            Not now
                          </button>
                          <button
                            className="text-button quiet"
                            onClick={() => setDismissing(headsUpId)}
                            aria-label={`Don't show again: ${item.title}`}
                            aria-expanded={dismissing === headsUpId}
                          >
                            Dismiss…
                          </button>
                        </span>
                      ) : (
                        action
                      )}
                      {headsUpId &&
                        item.headsUpKind &&
                        dismissing === headsUpId && (
                          <HeadsUpDismiss
                            headsUp={{
                              id: headsUpId,
                              title: item.title,
                              kind: item.headsUpKind,
                            }}
                            onCancel={() => setDismissing(null)}
                            onDone={() => {
                              setDismissing(null);
                              onHeadsUpDismissed?.();
                            }}
                            onMessage={(message) => onMessage?.(message)}
                          />
                        )}
                    </li>
                  );
                })}
              </ul>
            </section>
          )}

          {questions.length > 0 && (
            <section
              className="assistant-card question-waiting"
              aria-label="Questions waiting"
            >
              <h2>
                {questions.length === 1
                  ? "1 question waiting"
                  : `${questions.length} questions waiting`}
              </h2>
              <p>
                “{questions[0].request}”: {questions[0].question}
              </p>
              {onOpenTasks && (
                <button className="text-button" onClick={onOpenTasks}>
                  Answer in Tasks <Glyph>→</Glyph>
                </button>
              )}
            </section>
          )}

          <section
            className={`assistant-card review-queue ${approvals.length > 0 ? "needs-you" : ""}`}
            aria-label="Waiting for your approval"
          >
            <div className="review-queue-heading">
              <h2>Waiting for your approval</h2>
              {approvals.length > 0 && (
                <span className="review-count">{approvals.length}</span>
              )}
            </div>
            {approvals.length > 0 ? (
              <>
                <ul className="review-list">
                  {approvals.map((approval) => (
                    <li key={approval.id}>
                      <span
                        className={`review-dot ${approval.from}`}
                        aria-hidden="true"
                      />
                      <div>
                        <strong>{approval.title}</strong>
                        {approval.summary && <small>{approval.summary}</small>}
                        <small className="review-meta">
                          {plural(approval.changes, "change")} ready
                          {approval.from === "planner"
                            ? " · from the planner"
                            : ""}
                        </small>
                      </div>
                      <button
                        className="secondary-button small"
                        onClick={() => onReview(approval)}
                        aria-label={`Review ${plural(approval.changes, "change")}: ${approval.title}`}
                      >
                        Review
                      </button>
                    </li>
                  ))}
                </ul>
                <p className="review-note">
                  Nothing changes until you apply it.
                </p>
              </>
            ) : (
              <p className="review-empty">
                Nothing to approve. The assistant only proposes changes; you
                decide what's applied.
              </p>
            )}
          </section>

          <section className="assistant-card inbox-link" aria-label="Inbox">
            <p className="side-kicker">CAPTURE INBOX</p>
            <p>
              {inboxCount === 0
                ? "No captured notes waiting."
                : `${plural(inboxCount, "captured note")} waiting to be sorted.`}{" "}
              These are your own notes, not suggestions.
            </p>
            <button className="text-button" onClick={onOpenInbox}>
              Open Inbox <Glyph>→</Glyph>
            </button>
          </section>
        </aside>
      </div>
    </div>
  );
}

function DaySummary({
  context,
  isToday,
  onOpenSchedule,
  onBackToToday,
}: {
  context: DayContext;
  isToday: boolean;
  onOpenSchedule: () => void;
  onBackToToday: () => void;
}) {
  const focus =
    context.items.find((item) => item.key === context.focusKey) ??
    (isToday ? undefined : context.items[0]);
  const now = Date.now();
  const live = focus && Date.parse(focus.start) <= now;
  return (
    <section className="assistant-card day-summary" aria-label="Your day">
      <p className="side-kicker">{isToday ? "TODAY" : "SELECTED DAY"}</p>
      <strong className="day-summary-date">{dayLabel(context.day)}</strong>
      {!isToday && (
        <p className="day-summary-note">
          The assistant is working from this day.{" "}
          <button className="text-button" onClick={onBackToToday}>
            Back to today
          </button>
        </p>
      )}
      {context.loading ? (
        <LoadingLine label="Opening your local schedule" />
      ) : (
        <ul className="day-summary-facts">
          {focus ? (
            <li>
              <span>{live ? "Now" : isToday ? "Next" : "First"}</span>
              {agendaItemTitle(focus)}
              <small>
                {timeRangeLabel(
                  focus.start,
                  new Date(agendaItemEnd(focus)).toISOString(),
                )}
              </small>
            </li>
          ) : (
            <li>
              <span>Agenda</span>
              {context.items.length === 0
                ? "Nothing scheduled"
                : "Nothing else scheduled"}
            </li>
          )}
          {context.items.length > 1 && (
            <li>
              <span>Scheduled</span>
              {plural(context.items.length, "item")}
            </li>
          )}
          <li>
            <span>Tasks</span>
            {context.tasksDone}/{context.tasksTotal} done
            {context.dueOpen > 0 && ` · ${context.dueOpen} due`}
          </li>
          {context.milestones > 0 && (
            <li>
              <span>Milestones</span>
              {context.milestones}
            </li>
          )}
          {context.capacityLabel && (
            <li>
              <span>Capacity</span>
              {context.capacityLabel}
            </li>
          )}
        </ul>
      )}
      <button className="text-button" onClick={onOpenSchedule}>
        Open in Schedule <Glyph>→</Glyph>
      </button>
    </section>
  );
}

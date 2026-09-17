import { CSSProperties, ReactNode, RefObject } from "react";
import type {
  Agenda,
  CalendarAgenda,
  Capacity,
  Milestone,
  Person,
  Plan,
  ScheduledBlock,
  ScheduleEvent,
  Task,
} from "./api";
import {
  agendaItems,
  allDayEventsOn,
  calendarColor,
  capacityLine,
  capacityTotals,
  hoursLabel,
  itemsOnDay,
} from "./calendars";
import {
  dayLabel,
  offsetDay,
  rangeLabel,
  timeLabel,
  weekdayName,
} from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import { PlanChip } from "./PlanControls";
import {
  estimateLabel,
  groupWeek,
  isOverdue,
  matchesPlanFilter,
  planColor,
  plural,
  PlanFilter,
  relativeDayLabel,
  shortDate,
} from "./planning";

export function WeekView({
  startDay,
  today,
  agenda,
  plannedTasks,
  calendar,
  capacity,
  onPlanWeek,
  loading,
  planFilter,
  filterControl,
  planById,
  personById,
  busyTaskId,
  headingRef,
  onShiftWeek,
  onThisWeek,
  onOpenDay,
  onOpenEvent,
  onOpenBlock,
  onOpenCalendars,
  onOpenTask,
  onToggleTask,
  onOpenMilestone,
}: {
  startDay: string;
  today: string;
  agenda: Agenda | null;
  /** Tasks chosen for this week that have no day yet. */
  plannedTasks: Task[];
  /** Events from read-only calendars for the week, when they could be read. */
  calendar: CalendarAgenda | null;
  capacity: Capacity | null;
  onPlanWeek: () => void;
  loading: boolean;
  planFilter: PlanFilter;
  filterControl: ReactNode;
  planById: Map<string, Plan>;
  personById: Map<string, Person>;
  busyTaskId: string | null;
  headingRef: RefObject<HTMLHeadingElement>;
  onShiftWeek: (weeks: number) => void;
  onThisWeek: () => void;
  onOpenDay: (day: string) => void;
  onOpenEvent: (event: ScheduleEvent) => void;
  onOpenBlock: (scheduled: ScheduledBlock) => void;
  onOpenCalendars: () => void;
  onOpenTask: (task: Task) => void;
  onToggleTask: (task: Task) => void;
  onOpenMilestone: (milestone: Milestone) => void;
}) {
  const filtered: Agenda | null = agenda && {
    events: agenda.events.filter((item) => matchesPlanFilter(item, planFilter)),
    tasks: agenda.tasks.filter((item) => matchesPlanFilter(item, planFilter)),
    dueTasks: agenda.dueTasks.filter((item) =>
      matchesPlanFilter(item, planFilter),
    ),
    milestones: agenda.milestones.filter((item) =>
      matchesPlanFilter(item, planFilter),
    ),
    blocks: agenda.blocks.filter((item) =>
      matchesPlanFilter(item.task, planFilter),
    ),
  };
  // Other calendars' events belong to no plan, so a filter for one plan hides them.
  const external =
    planFilter === "all" || planFilter === "none"
      ? (calendar?.events ?? [])
      : [];
  const calendars = calendar?.calendars ?? [];
  const calendarById = new Map(calendars.map((item) => [item.id, item]));
  const days = filtered ? groupWeek(filtered, startDay) : [];
  const items = filtered
    ? agendaItems([], filtered.blocks, external, calendars)
    : [];
  const pooled = plannedTasks.filter((task) =>
    matchesPlanFilter(task, planFilter),
  );
  const endDay = offsetDay(startDay, 6);
  const timedExternal = external.filter((event) => !event.allDay).length;
  const line = capacity
    ? capacityLine(capacityTotals(capacity, { includePool: true }))
    : null;
  return (
    <div className="week-page">
      <header className="topbar">
        <div className="date-heading">
          <p>YOUR WEEK</p>
          <h1 ref={headingRef} tabIndex={-1}>
            {rangeLabel(startDay, endDay)}
          </h1>
        </div>
        <div className="date-controls">
          <button
            className="icon-button"
            aria-label="Previous week"
            onClick={() => onShiftWeek(-1)}
          >
            <Glyph>←</Glyph>
          </button>
          <button className="secondary-button" onClick={onThisWeek}>
            This week
          </button>
          <button
            className="icon-button"
            aria-label="Next week"
            onClick={() => onShiftWeek(1)}
          >
            <Glyph>→</Glyph>
          </button>
          <button className="secondary-button" onClick={onPlanWeek}>
            Plan the week
          </button>
        </div>
      </header>
      <div className="week-toolbar">
        {filtered ? (
          <p className="week-summary" aria-live="polite">
            {[
              plural(filtered.events.length, "event"),
              timedExternal > 0 && plural(timedExternal, "calendar event"),
              plural(filtered.tasks.length, "scheduled task"),
              pooled.length > 0 &&
                `${plural(pooled.length, "task")} with no day`,
              filtered.blocks.length > 0 &&
                plural(filtered.blocks.length, "time block"),
              plural(filtered.dueTasks.length, "due task"),
              plural(filtered.milestones.length, "milestone"),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
        ) : (
          <span />
        )}
        {filterControl}
      </div>
      {line && (
        <button
          className={`week-capacity ${line.over ? "over" : ""}`}
          onClick={onOpenCalendars}
          title="Working hours and calendars"
        >
          <Mark
            size={7}
            filled
            color={line.over ? "var(--warning-dot)" : "var(--blue)"}
          />
          <strong>{line.label}</strong>
          {line.over && (
            <span>
              Over by {hoursLabel(line.overBy)} · nothing moves on its own
            </span>
          )}
          {capacity?.calendarsIncomplete && (
            <span>Some calendar time may be missing</span>
          )}
        </button>
      )}
      {loading && !filtered ? (
        <div className="loading-line">
          <Spinner size={10} />
          Gathering your week
        </div>
      ) : (
        <>
          {pooled.length > 0 && (
            <section
              className="week-pool"
              aria-label="Chosen for this week, no day yet"
            >
              <p className="due-heading">
                NO DAY YET <span>{pooled.length}</span>
              </p>
              <ul className="week-items">
                {pooled.map((task) => {
                  const done = task.status === "done";
                  return (
                    <li
                      key={task.id}
                      className={`week-task ${done ? "done" : ""}`}
                    >
                      <button
                        className="check-box"
                        disabled={busyTaskId === task.id}
                        onClick={() => onToggleTask(task)}
                        aria-label={`Mark ${task.title} ${done ? "not done" : "done"}`}
                      />
                      <button
                        className="week-task-open"
                        onClick={() => onOpenTask(task)}
                      >
                        <span className="week-title">{task.title}</span>
                        {task.estimatedMinutes !== null && (
                          <small>{estimateLabel(task.estimatedMinutes)}</small>
                        )}
                        <PlanChip
                          plan={
                            task.planId ? planById.get(task.planId) : undefined
                          }
                        />
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}
          <ol className="week-days">
            {days.map((group) => {
              const allDay = allDayEventsOn(external, group.day);
              const extra = itemsOnDay(items, group.day, startDay);
              const timed = [
                ...group.events.map((event) => ({
                  start: event.startAtUtc,
                  node: (
                    <li key={event.id} className="week-event">
                      <button onClick={() => onOpenEvent(event)}>
                        <time>{timeLabel(event.startAtUtc)}</time>
                        <span className="week-title">{event.title}</span>
                        {event.reminderMinutesBefore !== null && (
                          <i
                            className="week-reminder"
                            role="img"
                            aria-label="Has a reminder"
                          />
                        )}
                        {event.ownerId && (
                          <small>
                            {personById.get(event.ownerId)?.displayName}
                          </small>
                        )}
                        <PlanChip
                          plan={
                            event.planId
                              ? planById.get(event.planId)
                              : undefined
                          }
                        />
                      </button>
                    </li>
                  ),
                })),
                ...extra.map((item) => ({
                  start: item.start,
                  node:
                    item.kind === "block" ? (
                      <li
                        key={item.key}
                        className={`week-event week-block ${item.scheduled.task.status === "done" ? "done" : ""}`}
                        style={
                          {
                            "--plan-color": planColor(
                              item.scheduled.task.planId
                                ? planById.get(item.scheduled.task.planId)
                                : undefined,
                            ),
                          } as CSSProperties
                        }
                      >
                        <button onClick={() => onOpenBlock(item.scheduled)}>
                          <time>{timeLabel(item.start)}</time>
                          <span className="week-title">
                            {item.scheduled.task.title}
                          </span>
                          <small>
                            Block ·{" "}
                            {hoursLabel(item.scheduled.block.durationMinutes)}
                          </small>
                        </button>
                      </li>
                    ) : item.kind === "external" ? (
                      <li
                        key={item.key}
                        className={`week-event week-external ${item.event.busy ? "" : "free"}`}
                        style={
                          {
                            "--calendar-color": calendarColor(item.calendar),
                          } as CSSProperties
                        }
                      >
                        <div className="week-external-line">
                          <time>{timeLabel(item.start)}</time>
                          <span className="week-title">{item.event.title}</span>
                          <small>{item.calendar?.name ?? "Calendar"}</small>
                        </div>
                      </li>
                    ) : null,
                })),
              ].sort(
                (left, right) =>
                  Date.parse(left.start) - Date.parse(right.start),
              );
              const dayCapacity = capacity?.days.find(
                (candidate) => candidate.day === group.day,
              );
              const empty =
                timed.length +
                  allDay.length +
                  group.tasks.length +
                  group.dueTasks.length +
                  group.milestones.length ===
                0;
              return (
                <li
                  key={group.day}
                  className={`week-day ${group.day === today ? "today" : ""}`}
                >
                  <button
                    className="week-day-heading"
                    onClick={() => onOpenDay(group.day)}
                    aria-label={`Open ${dayLabel(group.day)}`}
                  >
                    <strong>{weekdayName(group.day)}</strong>
                    <span>
                      {shortDate(group.day)} ·{" "}
                      {relativeDayLabel(group.day, today)}
                    </span>
                    {dayCapacity && dayCapacity.workingMinutes > 0 && (
                      <small
                        className={
                          dayCapacity.plannedMinutes >
                          dayCapacity.availableMinutes
                            ? "over"
                            : ""
                        }
                      >
                        {hoursLabel(dayCapacity.availableMinutes)} available
                      </small>
                    )}
                  </button>
                  {empty ? (
                    <p className="week-empty">Nothing planned.</p>
                  ) : (
                    <ul className="week-items">
                      {group.milestones.map((milestone) => (
                        <li key={milestone.id} className="week-milestone">
                          <button onClick={() => onOpenMilestone(milestone)}>
                            <Mark
                              size={7}
                              color={planColor(planById.get(milestone.planId))}
                              filled={milestone.status === "complete"}
                              dashed={milestone.status === "skipped"}
                            />
                            <span className="week-title">
                              {milestone.title}
                            </span>
                            <PlanChip plan={planById.get(milestone.planId)} />
                          </button>
                        </li>
                      ))}
                      {allDay.map((event) => (
                        <li
                          key={`${event.calendarId}-${event.key}`}
                          className="week-all-day"
                          style={
                            {
                              "--calendar-color": calendarColor(
                                calendarById.get(event.calendarId),
                              ),
                            } as CSSProperties
                          }
                        >
                          <div className="week-external-line">
                            <time>All day</time>
                            <span className="week-title">{event.title}</span>
                            <small>
                              {calendarById.get(event.calendarId)?.name ??
                                "Calendar"}
                            </small>
                          </div>
                        </li>
                      ))}
                      {timed.map((entry) => entry.node)}
                      {[...group.tasks, ...group.dueTasks].map((task) => {
                        const done = task.status === "done";
                        const due = task.dueDate === group.day;
                        return (
                          <li
                            key={`${task.id}-${due ? "due" : "scheduled"}`}
                            className={`week-task ${done ? "done" : ""}`}
                          >
                            <button
                              className="check-box"
                              disabled={busyTaskId === task.id}
                              onClick={() => onToggleTask(task)}
                              aria-label={`Mark ${task.title} ${done ? "not done" : "done"}`}
                            />
                            <button
                              className="week-task-open"
                              onClick={() => onOpenTask(task)}
                            >
                              <span className="week-title">{task.title}</span>
                              {due && (
                                <small
                                  className={
                                    !done && isOverdue(task.dueDate, today)
                                      ? "late"
                                      : ""
                                  }
                                >
                                  Due
                                </small>
                              )}
                              {task.ownerId && (
                                <small>
                                  {personById.get(task.ownerId)?.displayName}
                                </small>
                              )}
                              <PlanChip
                                plan={
                                  task.planId
                                    ? planById.get(task.planId)
                                    : undefined
                                }
                              />
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </li>
              );
            })}
          </ol>
        </>
      )}
    </div>
  );
}

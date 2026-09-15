import { ReactNode, RefObject } from "react";
import type {
  Agenda,
  Milestone,
  Person,
  Plan,
  ScheduleEvent,
  Task,
} from "./api";
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
  onOpenTask,
  onToggleTask,
  onOpenMilestone,
}: {
  startDay: string;
  today: string;
  agenda: Agenda | null;
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
  };
  const days = filtered ? groupWeek(filtered, startDay) : [];
  const endDay = offsetDay(startDay, 6);
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
        </div>
      </header>
      <div className="week-toolbar">
        {filtered ? (
          <p className="week-summary" aria-live="polite">
            {[
              plural(filtered.events.length, "event"),
              plural(filtered.tasks.length, "scheduled task"),
              plural(filtered.dueTasks.length, "due task"),
              plural(filtered.milestones.length, "milestone"),
            ].join(" · ")}
          </p>
        ) : (
          <span />
        )}
        {filterControl}
      </div>
      {loading && !filtered ? (
        <div className="loading-line">
          <Spinner size={10} />
          Gathering your week
        </div>
      ) : (
        <ol className="week-days">
          {days.map((group) => {
            const empty =
              group.events.length +
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
                          <span className="week-title">{milestone.title}</span>
                          <PlanChip plan={planById.get(milestone.planId)} />
                        </button>
                      </li>
                    ))}
                    {group.events.map((event) => (
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
                    ))}
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
      )}
    </div>
  );
}

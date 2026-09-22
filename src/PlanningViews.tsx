import { ReactNode, useEffect, useRef, useState } from "react";
import {
  Agenda,
  api,
  Capacity,
  messageFor,
  Milestone,
  Person,
  Plan,
  PlanningBoard,
  Task,
} from "./api";
import { capacityLine, capacityTotals, hoursLabel } from "./calendars";
import {
  dayLabel,
  localTimeZone,
  offsetDay,
  rangeLabel,
  weekdayShort,
  weekStartDay,
  weekStartsOn,
} from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import { PlanDot } from "./PlanControls";
import {
  dayPlanning,
  estimateLabel,
  eventDay,
  MilestoneGroup,
  plural,
  relativeDayLabel,
  shortDate,
  taskUpdate,
  weekPlanning,
  workload,
  workloadLabel,
} from "./planning";
import { TaskRow } from "./TaskRow";
import { useHeadingFocus } from "./useHeadingFocus";
import { taskMoveItems, useTaskMover } from "./useTaskMover";

type SessionProps = {
  today: string;
  planById: Map<string, Plan>;
  personById: Map<string, Person>;
  /** Changes whenever planner data changed anywhere, so the session reloads. */
  dataVersion: number;
  focusToken: number;
  onChanged: () => Promise<void>;
  onOpenTask: (task: Task) => void;
  onMessage: (message: string) => void;
};

/** Choose what a week is for: work carried over, due, behind milestones, or in plan backlogs. */
export function PlanWeekView({
  plans,
  onDone,
  ...props
}: SessionProps & {
  plans: Plan[];
  /** Records that `weekStart` was planned and leaves the session. */
  onDone: (weekStart: string) => void;
}) {
  const { today, dataVersion, onMessage } = props;
  const currentWeek = weekStartDay(today, weekStartsOn);
  const [start, setStart] = useState(currentWeek);
  const [board, setBoard] = useState<PlanningBoard | null>(null);
  const [week, setWeek] = useState<Agenda | null>(null);
  const [capacity, setCapacity] = useState<Capacity | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  useHeadingFocus(headingRef, props.focusToken);
  const mover = useTaskMover({
    today,
    onChanged: props.onChanged,
    onError: onMessage,
  });

  useEffect(() => {
    let current = true;
    Promise.all([
      api.planningBoard(start),
      api.listWeek(start, localTimeZone),
      api.capacity(start, 7, localTimeZone).catch(() => null),
    ])
      .then(([nextBoard, nextWeek, nextCapacity]) => {
        if (!current) return;
        setBoard(nextBoard);
        setWeek(nextWeek);
        setCapacity(nextCapacity);
      })
      .catch((cause) => onMessage(messageFor(cause)));
    return () => {
      current = false;
    };
  }, [start, dataVersion, onMessage]);

  const days = Array.from({ length: 7 }, (_, index) => offsetDay(start, index));
  const sections = board ? weekPlanning(board, start, today, plans) : null;
  const addLabel =
    start === currentWeek
      ? "Add to this week"
      : `Add to the week of ${shortDate(start)}`;
  const candidates = sections
    ? [
        ...sections.carried,
        ...sections.overdue,
        ...sections.dueSoon,
        ...sections.milestones.flatMap((group) => group.tasks),
        ...sections.backlog.flatMap((group) => group.tasks),
      ]
    : [];

  const row = (task: Task, options: { candidate: boolean }) => (
    <SessionTaskRow
      key={task.id}
      task={task}
      weekStart={currentWeek}
      showWeek={options.candidate}
      mover={mover}
      addLabel={options.candidate ? addLabel : undefined}
      onAdd={
        options.candidate
          ? () => void mover.move([task], { kind: "week", weekStart: start })
          : undefined
      }
      {...props}
    />
  );

  return (
    <div className="session-page">
      <header className="topbar">
        <div className="date-heading">
          <p>WEEKLY PLANNING</p>
          <h1 ref={headingRef} tabIndex={-1}>
            Plan the week
          </h1>
          <span className="session-range">
            {rangeLabel(start, offsetDay(start, 6))}
          </span>
        </div>
        <div className="date-controls">
          <button
            className="icon-button"
            aria-label="Previous week"
            onClick={() => setStart(offsetDay(start, -7))}
          >
            <Glyph>←</Glyph>
          </button>
          <button
            className="secondary-button"
            onClick={() => setStart(currentWeek)}
            disabled={start === currentWeek}
          >
            This week
          </button>
          <button
            className="icon-button"
            aria-label="Next week"
            onClick={() => setStart(offsetDay(start, 7))}
          >
            <Glyph>→</Glyph>
          </button>
          <button className="primary-button" onClick={() => onDone(start)}>
            <Mark filled />
            Done planning
          </button>
        </div>
      </header>
      <p className="page-intro">
        Choose what this week is for. Chosen work doesn't need a day yet; give
        it one here or when you plan each day.
      </p>
      {!board || !week || !sections ? (
        <div className="loading-line">
          <Spinner size={10} />
          Gathering open work
        </div>
      ) : (
        <>
          <ol className="week-load" aria-label="The week at a glance">
            {days.map((day) => {
              const scheduled = board.tasks.filter(
                (task) => task.scheduledDay === day,
              );
              const load = workload(scheduled);
              const events = week.events.filter(
                (event) => eventDay(event) === day,
              );
              const dayCapacity = capacity?.days.find(
                (candidate) => candidate.day === day,
              );
              return (
                <li
                  key={day}
                  className={`load-day ${day === today ? "today" : ""} ${day < today ? "past" : ""}`}
                >
                  <span>
                    {day === today ? "TODAY" : weekdayShort(day).toUpperCase()}
                  </span>
                  <strong>{day.slice(-2)}</strong>
                  <small>
                    {load.open ? plural(load.open, "task") : "No tasks"}
                  </small>
                  <small>
                    {load.minutes ? estimateLabel(load.minutes) : " "}
                  </small>
                  {events.length > 0 && (
                    <small>{plural(events.length, "event")}</small>
                  )}
                  {dayCapacity && dayCapacity.workingMinutes > 0 && (
                    <small
                      className={
                        dayCapacity.plannedMinutes >
                        dayCapacity.availableMinutes
                          ? "over"
                          : "available"
                      }
                    >
                      {hoursLabel(dayCapacity.availableMinutes)} available
                    </small>
                  )}
                </li>
              );
            })}
          </ol>
          <div className="session-grid">
            <section
              className="session-column"
              aria-labelledby="week-candidates"
            >
              <div className="section-header">
                <div>
                  <p>CHOOSE FROM</p>
                  <h2 id="week-candidates">Open work</h2>
                </div>
                <span>{plural(candidates.length, "task")}</span>
              </div>
              {candidates.length === 0 && sections.milestones.length === 0 ? (
                <p className="empty-tasks">
                  Nothing is waiting. Capture work in the Inbox or add tasks to
                  a plan.
                </p>
              ) : (
                <>
                  <SessionGroup
                    title="From earlier weeks"
                    tone="late"
                    tasks={sections.carried}
                  >
                    {sections.carried.map((task) =>
                      row(task, { candidate: true }),
                    )}
                  </SessionGroup>
                  <SessionGroup
                    title="Overdue"
                    tone="late"
                    tasks={sections.overdue}
                  >
                    {sections.overdue.map((task) =>
                      row(task, { candidate: true }),
                    )}
                  </SessionGroup>
                  <SessionGroup title="Due soon" tasks={sections.dueSoon}>
                    {sections.dueSoon.map((task) =>
                      row(task, { candidate: true }),
                    )}
                  </SessionGroup>
                  {sections.milestones
                    .filter((group) => group.tasks.length > 0)
                    .map((group) => (
                      <MilestoneSection
                        key={group.milestone.id}
                        group={group}
                        today={today}
                        plan={props.planById.get(group.milestone.planId)}
                      >
                        {group.tasks.map((task) =>
                          row(task, { candidate: true }),
                        )}
                      </MilestoneSection>
                    ))}
                  <MilestonesAhead
                    milestones={sections.milestones
                      .filter((group) => group.tasks.length === 0)
                      .map((group) => group.milestone)}
                    today={today}
                    planById={props.planById}
                  />
                  {sections.backlog.map(({ plan, tasks }) => (
                    <SessionGroup
                      key={plan.id}
                      title={plan.title}
                      mark={<PlanDot plan={plan} />}
                      detail="Plan backlog"
                      tasks={tasks}
                    >
                      {tasks.map((task) => row(task, { candidate: true }))}
                    </SessionGroup>
                  ))}
                </>
              )}
            </section>
            <section
              className="session-column chosen"
              aria-labelledby="week-chosen"
            >
              <div className="section-header">
                <div>
                  <p>{start === currentWeek ? "THIS WEEK" : "THAT WEEK"}</p>
                  <h2 id="week-chosen">Chosen</h2>
                </div>
                <span>
                  {workloadLabel(workload(sections.chosen)) ?? "Nothing yet"}
                </span>
              </div>
              {capacity && <CapacityNote capacity={capacity} includePool />}
              {sections.chosen.length === 0 ? (
                <p className="empty-tasks">
                  Nothing chosen yet. Add work from the left.
                </p>
              ) : (
                <>
                  <SessionGroup
                    title="No day yet"
                    tasks={sections.chosen.filter(
                      (task) => task.scheduledDay === null,
                    )}
                  >
                    {sections.chosen
                      .filter((task) => task.scheduledDay === null)
                      .map((task) => row(task, { candidate: false }))}
                  </SessionGroup>
                  {days.map((day) => {
                    const tasks = sections.chosen.filter(
                      (task) => task.scheduledDay === day,
                    );
                    return (
                      <SessionGroup
                        key={day}
                        title={dayLabel(day)}
                        detail={relativeDayLabel(day, today)}
                        tasks={tasks}
                      >
                        {tasks.map((task) => row(task, { candidate: false }))}
                      </SessionGroup>
                    );
                  })}
                </>
              )}
            </section>
          </div>
        </>
      )}
      {mover.dialog}
    </div>
  );
}

/** Build today on purpose: unfinished work first, then what is due and what this week holds. */
export function PlanTodayView({
  onDone,
  ...props
}: SessionProps & {
  /** Records that today was planned and returns to Today. */
  onDone: () => void;
}) {
  const { today, dataVersion, onMessage } = props;
  const weekStart = weekStartDay(today, weekStartsOn);
  const [board, setBoard] = useState<PlanningBoard | null>(null);
  const [agenda, setAgenda] = useState<Agenda | null>(null);
  const [capacity, setCapacity] = useState<Capacity | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  useHeadingFocus(headingRef, props.focusToken);
  const mover = useTaskMover({
    today,
    onChanged: props.onChanged,
    onError: onMessage,
  });

  useEffect(() => {
    let current = true;
    Promise.all([
      api.planningBoard(weekStart),
      api.listAgenda(today, localTimeZone),
      api.capacity(today, 1, localTimeZone).catch(() => null),
    ])
      .then(([nextBoard, nextAgenda, nextCapacity]) => {
        if (!current) return;
        setBoard(nextBoard);
        setAgenda(nextAgenda);
        setCapacity(nextCapacity);
      })
      .catch((cause) => onMessage(messageFor(cause)));
    return () => {
      current = false;
    };
  }, [weekStart, today, dataVersion, onMessage]);

  const sections = board ? dayPlanning(board, today, weekStart) : null;
  const eventMinutes =
    agenda?.events.reduce((total, event) => total + event.durationMinutes, 0) ??
    0;
  const row = (task: Task, candidate: boolean) => (
    <SessionTaskRow
      key={task.id}
      task={task}
      weekStart={weekStart}
      showWeek={candidate}
      mover={mover}
      addLabel={candidate ? "Plan for today" : undefined}
      onAdd={
        candidate
          ? () => void mover.move([task], { kind: "day", day: today })
          : undefined
      }
      {...props}
    />
  );

  return (
    <div className="session-page">
      <header className="topbar">
        <div className="date-heading">
          <p>DAILY PLANNING</p>
          <h1 ref={headingRef} tabIndex={-1}>
            Plan today
          </h1>
          <span className="session-range">{dayLabel(today)}</span>
        </div>
        <div className="date-controls">
          <button className="primary-button" onClick={onDone}>
            <Mark filled />
            Done planning
          </button>
        </div>
      </header>
      <p className="page-intro">
        Build today on purpose. Start with anything unfinished, then add what's
        due and what you chose for this week.
      </p>
      {!board || !agenda || !sections ? (
        <div className="loading-line">
          <Spinner size={10} />
          Gathering today's work
        </div>
      ) : (
        <div className="session-grid">
          <section className="session-column" aria-labelledby="day-candidates">
            <div className="section-header">
              <div>
                <p>CHOOSE FROM</p>
                <h2 id="day-candidates">Open work</h2>
              </div>
            </div>
            {sections.unfinished.length +
              sections.dueToday.length +
              sections.thisWeek.length +
              sections.overdue.length +
              sections.milestones.length ===
            0 ? (
              <p className="empty-tasks">Nothing else is waiting for today.</p>
            ) : (
              <>
                <SessionGroup
                  title="Unfinished"
                  tone="late"
                  tasks={sections.unfinished}
                  action={
                    sections.unfinished.length > 1 && (
                      <button
                        className="text-button"
                        onClick={() =>
                          void mover.move(sections.unfinished, {
                            kind: "day",
                            day: today,
                          })
                        }
                      >
                        Move all to today <Glyph>→</Glyph>
                      </button>
                    )
                  }
                >
                  {sections.unfinished.map((task) => row(task, true))}
                </SessionGroup>
                <SessionGroup title="Due today" tasks={sections.dueToday}>
                  {sections.dueToday.map((task) => row(task, true))}
                </SessionGroup>
                <SessionGroup
                  title="Chosen for this week"
                  tasks={sections.thisWeek}
                >
                  {sections.thisWeek.map((task) => row(task, true))}
                </SessionGroup>
                <SessionGroup
                  title="Overdue"
                  tone="late"
                  tasks={sections.overdue}
                >
                  {sections.overdue.map((task) => row(task, true))}
                </SessionGroup>
                {sections.milestones.map((group) => (
                  <MilestoneSection
                    key={group.milestone.id}
                    group={group}
                    today={today}
                    plan={props.planById.get(group.milestone.planId)}
                  >
                    {group.tasks.map((task) => row(task, true))}
                  </MilestoneSection>
                ))}
              </>
            )}
          </section>
          <section className="session-column chosen" aria-labelledby="day-plan">
            <div className="section-header">
              <div>
                <p>TODAY</p>
                <h2 id="day-plan">The plan</h2>
              </div>
              <span>
                {workloadLabel(workload(sections.planned)) ?? "Nothing yet"}
              </span>
            </div>
            {capacity ? (
              <CapacityNote capacity={capacity} includePool={false} />
            ) : (
              agenda.events.length > 0 && (
                <p className="session-note">
                  <Mark shape="circle" size={6} />
                  {plural(agenda.events.length, "event")} ·{" "}
                  {estimateLabel(eventMinutes)} already on the calendar
                </p>
              )
            )}
            {sections.planned.length === 0 ? (
              <p className="empty-tasks">
                Nothing planned yet. Add work from the left.
              </p>
            ) : (
              <div className="task-list">
                {sections.planned.map((task) => row(task, false))}
              </div>
            )}
          </section>
        </div>
      )}
      {mover.dialog}
    </div>
  );
}

/**
 * Planned work against the time working hours leave after events and busy calendar time. It
 * only informs; nothing is moved because of it.
 */
function CapacityNote({
  capacity,
  includePool,
}: {
  capacity: Capacity;
  /** Whether week-pool work with no day counts, as it does for a whole week. */
  includePool: boolean;
}) {
  const totals = capacityTotals(capacity, { includePool });
  const line = capacityLine(totals);
  if (totals.workingMinutes === 0)
    return (
      <p className="session-note">
        <Mark shape="circle" size={6} />
        No working hours {includePool ? "this week" : "today"}
      </p>
    );
  return (
    <p className={`session-note capacity-note ${line.over ? "over" : ""}`}>
      <Mark
        shape="circle"
        size={6}
        filled={line.over}
        color={line.over ? "var(--warning-dot)" : undefined}
      />
      <span>
        {line.label}
        {totals.busyMinutes > 0 &&
          ` · ${hoursLabel(totals.busyMinutes)} already busy`}
        {line.over && ` · over by ${hoursLabel(line.overBy)}`}
        {capacity.calendarsIncomplete && " · some calendar time may be missing"}
      </span>
    </p>
  );
}

function SessionTaskRow({
  task,
  weekStart,
  showWeek,
  mover,
  addLabel,
  onAdd,
  today,
  planById,
  personById,
  onChanged,
  onOpenTask,
  onMessage,
}: SessionProps & {
  task: Task;
  weekStart: string;
  /** Whether the row names the week a task was chosen for; redundant inside a week's own list. */
  showWeek: boolean;
  mover: ReturnType<typeof useTaskMover>;
  addLabel?: string;
  onAdd?: () => void;
}) {
  async function toggle() {
    try {
      await api.updateTask({
        ...taskUpdate(task),
        status: task.status === "done" ? "todo" : "done",
      });
      await onChanged();
    } catch (cause) {
      onMessage(messageFor(cause));
    }
  }
  return (
    <TaskRow
      task={task}
      today={today}
      weekStart={showWeek ? weekStart : undefined}
      plan={task.planId ? planById.get(task.planId) : undefined}
      owner={task.ownerId ? personById.get(task.ownerId) : undefined}
      busy={mover.busyIds.has(task.id)}
      addLabel={addLabel}
      onScheduleHere={onAdd}
      moveItems={taskMoveItems(task, {
        today,
        weekStart,
        move: (tasks, target) => void mover.move(tasks, target),
        pickDay: mover.pickDay,
        blockTime: mover.blockTime,
      })}
      onToggle={() => void toggle()}
      onOpen={() => onOpenTask(task)}
    />
  );
}

function SessionGroup({
  title,
  detail,
  mark,
  tone,
  tasks,
  action,
  children,
}: {
  title: string;
  detail?: string;
  mark?: ReactNode;
  tone?: "late";
  tasks: Task[];
  action?: ReactNode;
  children: ReactNode;
}) {
  if (tasks.length === 0) return null;
  return (
    <section className={`session-group ${tone ?? ""}`}>
      <h3>
        {mark}
        <span>{title}</span>
        {detail && <small>{detail}</small>}
        <em>{tasks.length}</em>
        {action}
      </h3>
      <div className="task-list">{children}</div>
    </section>
  );
}

function MilestoneSection({
  group,
  today,
  plan,
  children,
}: {
  group: MilestoneGroup;
  today: string;
  plan: Plan | undefined;
  children: ReactNode;
}) {
  const { milestone, tasks } = group;
  const date = milestone.targetDate as string;
  return (
    <section className={`session-group ${date < today ? "late" : ""}`}>
      <h3>
        <Mark size={7} color={plan ? undefined : "var(--silver)"} />
        <span>{milestone.title}</span>
        <small>
          {relativeDayLabel(date, today)} · {shortDate(date)}
          {plan ? ` · ${plan.title}` : ""}
        </small>
        <em>{tasks.length}</em>
      </h3>
      <div className="task-list">{children}</div>
    </section>
  );
}

/** Pending milestones with no open work to choose, listed for awareness in one quiet group. */
function MilestonesAhead({
  milestones,
  today,
  planById,
}: {
  milestones: Milestone[];
  today: string;
  planById: Map<string, Plan>;
}) {
  if (milestones.length === 0) return null;
  return (
    <section className="session-group">
      <h3>
        <span>Milestones ahead</span>
        <em>{milestones.length}</em>
      </h3>
      <ul className="milestones-ahead">
        {milestones.map((milestone) => {
          const date = milestone.targetDate as string;
          const plan = planById.get(milestone.planId);
          return (
            <li key={milestone.id} className={date < today ? "late" : ""}>
              <Mark size={6} />
              <span>{milestone.title}</span>
              <small>
                {relativeDayLabel(date, today)} · {shortDate(date)}
                {plan ? ` · ${plan.title}` : ""}
              </small>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

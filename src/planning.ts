import {
  addMonths,
  differenceInCalendarDays,
  format,
  parseISO,
  startOfMonth,
} from "date-fns";
import type {
  Agenda,
  DayChange,
  Milestone,
  MilestoneStatus,
  Plan,
  PlanColor,
  PlanDeletion,
  PlanningBoard,
  PlanStatus,
  PlanWorkspace,
  ScheduleEvent,
  Task,
  TaskInput,
  TaskMove,
  TaskPriority,
  TaskStatus,
  Workstream,
} from "./api";
import {
  dateTimeFields,
  isoDay,
  offsetDay,
  timeLabel,
  weekStartDay,
  WeekStart,
} from "./date";

export const planStatusLabels: Record<PlanStatus, string> = {
  planning: "Planning",
  active: "Active",
  on_hold: "On hold",
  complete: "Complete",
  cancelled: "Cancelled",
};

export const milestoneStatusLabels: Record<MilestoneStatus, string> = {
  pending: "Pending",
  complete: "Complete",
  skipped: "Skipped",
};

export const taskStatusLabels: Record<TaskStatus, string> = {
  todo: "To do",
  in_progress: "In progress",
  blocked: "Blocked",
  done: "Done",
};

export const taskPriorityLabels: Record<TaskPriority, string> = {
  low: "Low",
  normal: "Normal",
  high: "High",
  critical: "Critical",
};

export const planColorValues: Record<PlanColor, string> = {
  sage: "#6f9a7d",
  clay: "#d46c4c",
  ochre: "#c49a45",
  lake: "#4f86a6",
  plum: "#8d6390",
  stone: "#7b837d",
};

/** Stone stands in for items outside any plan, as in the design's agenda. */
const unsetPlanColor = "#7b837d";

/** Board order: work in motion first, finished work last. */
export const taskStatusOrder: TaskStatus[] = [
  "in_progress",
  "blocked",
  "todo",
  "done",
];

/** `all` shows everything, `none` shows items outside any plan, otherwise a plan ID. */
export type PlanFilter = "all" | "none" | (string & {});

export function planColor(plan: Pick<Plan, "color"> | undefined | null) {
  return plan?.color ? planColorValues[plan.color] : unsetPlanColor;
}

export function matchesPlanFilter(
  item: { planId: string | null },
  filter: PlanFilter,
) {
  if (filter === "all") return true;
  if (filter === "none") return item.planId === null;
  return item.planId === filter;
}

export function taskCounts(tasks: Task[]) {
  const counts: Record<TaskStatus, number> = {
    todo: 0,
    in_progress: 0,
    blocked: 0,
    done: 0,
  };
  for (const task of tasks) counts[task.status] += 1;
  return { ...counts, total: tasks.length };
}

const featuredStatusOrder: Record<TaskStatus, number> = {
  in_progress: 0,
  blocked: 1,
  todo: 2,
  done: 3,
};
const featuredPriorityOrder: Record<TaskPriority, number> = {
  critical: 0,
  high: 1,
  normal: 2,
  low: 3,
};

/** The open tasks most worth a glance: work in motion, then blocked, then by priority and due day. */
export function featuredTasks(tasks: Task[], limit = 3) {
  return tasks
    .filter((task) => task.status !== "done")
    .sort(
      (left, right) =>
        featuredStatusOrder[left.status] - featuredStatusOrder[right.status] ||
        featuredPriorityOrder[left.priority] -
          featuredPriorityOrder[right.priority] ||
        (left.dueDate ?? "9999").localeCompare(right.dueDate ?? "9999") ||
        left.sortOrder - right.sortOrder,
    )
    .slice(0, limit);
}

export function overdueTaskCount(tasks: Task[], today: string) {
  return tasks.filter(
    (task) => task.status !== "done" && isOverdue(task.dueDate, today),
  ).length;
}

/** Two-digit counts for the sidebar, such as "03". */
export function paddedCount(count: number) {
  return String(count).padStart(2, "0");
}

export function groupTasksByStatus(tasks: Task[]) {
  return taskStatusOrder.map((status) => ({
    status,
    tasks: tasks.filter((task) => task.status === status),
  }));
}

/** The editable fields of a plan, ready for `api.updatePlan` (which rejects unknown fields). */
export function planUpdate(plan: Plan, archived = plan.archived) {
  return {
    id: plan.id,
    revision: plan.revision,
    title: plan.title,
    description: plan.description,
    status: plan.status,
    startDate: plan.startDate,
    targetDate: plan.targetDate,
    color: plan.color,
    archived,
  };
}

/** The editable fields of a task, ready for `api.updateTask`. */
export function taskUpdate(task: Task): TaskInput & {
  id: string;
  revision: number;
} {
  return {
    id: task.id,
    revision: task.revision,
    title: task.title,
    description: task.description,
    planId: task.planId,
    milestoneId: task.milestoneId,
    workstreamId: task.workstreamId,
    ownerId: task.ownerId,
    dueDate: task.dueDate,
    scheduledDay: task.scheduledDay,
    plannedWeek: task.plannedWeek,
    estimatedMinutes: task.estimatedMinutes,
    status: task.status,
    priority: task.priority,
  };
}

/** A new task's fields with DayPlan's defaults: to do, normal priority, and no links. */
export function newTask(fields: Pick<TaskInput, "title"> & Partial<TaskInput>) {
  return {
    description: "",
    planId: null,
    milestoneId: null,
    workstreamId: null,
    ownerId: null,
    dueDate: null,
    scheduledDay: null,
    plannedWeek: null,
    estimatedMinutes: null,
    status: "todo",
    priority: "normal",
    ...fields,
  } satisfies TaskInput;
}

/** Mirrors the repository rule that every task is reachable from a plan, week, or day. */
export function taskHasHome(
  task: Pick<TaskInput, "planId" | "dueDate" | "scheduledDay" | "plannedWeek">,
) {
  return (
    task.planId !== null ||
    task.dueDate !== null ||
    task.scheduledDay !== null ||
    task.plannedWeek !== null
  );
}

export function compareMilestones(left: Milestone, right: Milestone) {
  if (left.targetDate !== right.targetDate) {
    if (left.targetDate === null) return 1;
    if (right.targetDate === null) return -1;
    return left.targetDate < right.targetDate ? -1 : 1;
  }
  return (
    left.sortOrder - right.sortOrder ||
    left.createdAt.localeCompare(right.createdAt)
  );
}

export function nextMilestone(milestones: Milestone[]) {
  return (
    milestones
      .filter((milestone) => milestone.status === "pending")
      .sort(compareMilestones)[0] ?? null
  );
}

/** Whole calendar days from `from` to `to`; negative when `to` is earlier. */
export function daysBetween(from: string, to: string) {
  return differenceInCalendarDays(parseISO(to), parseISO(from));
}

export function relativeDayLabel(day: string, today: string) {
  const distance = daysBetween(today, day);
  if (distance === 0) return "Today";
  if (distance === 1) return "Tomorrow";
  if (distance === -1) return "Yesterday";
  return distance > 0 ? `In ${distance} days` : `${-distance} days ago`;
}

export function isOverdue(day: string | null, today: string) {
  return day !== null && day < today;
}

/** "04 Oct": day and month, zero-padded so dates align in columns. */
export function shortDate(day: string) {
  return format(parseISO(day), "dd MMM");
}

/** "01 Aug 2026". */
export function longDate(day: string) {
  return format(parseISO(day), "dd MMM yyyy");
}

/** "01 Aug 2026 → 04 Oct 2026 · 20 days left", for a plan's header. */
export function planSchedule(
  plan: Pick<Plan, "startDate" | "targetDate">,
  today: string,
) {
  const range =
    plan.startDate && plan.targetDate
      ? `${longDate(plan.startDate)} → ${longDate(plan.targetDate)}`
      : plan.targetDate
        ? `Target ${longDate(plan.targetDate)}`
        : plan.startDate
          ? `Starts ${longDate(plan.startDate)}`
          : "No dates yet";
  if (!plan.targetDate) return range;
  const left = daysBetween(today, plan.targetDate);
  if (left === 0) return `${range} · Target is today`;
  return left > 0
    ? `${range} · ${plural(left, "day")} left`
    : `${range} · ${plural(-left, "day")} past target`;
}

export function eventDay(event: Pick<ScheduleEvent, "startAtUtc">) {
  return dateTimeFields(event.startAtUtc).day;
}

export function eventHasEnded(
  event: Pick<ScheduleEvent, "startAtUtc" | "durationMinutes">,
  now: Date,
) {
  return (
    new Date(event.startAtUtc).getTime() + event.durationMinutes * 60_000 <
    now.getTime()
  );
}

/** The event happening at `now`, or failing that the next one to start. */
export function focusEventId(events: ScheduleEvent[], now: Date) {
  const time = now.getTime();
  const live = events.find((event) => {
    const start = new Date(event.startAtUtc).getTime();
    return start <= time && time < start + event.durationMinutes * 60_000;
  });
  if (live) return live.id;
  return (
    [...events]
      .filter((event) => new Date(event.startAtUtc).getTime() > time)
      .sort((left, right) => left.startAtUtc.localeCompare(right.startAtUtc))[0]
      ?.id ?? null
  );
}

export type UpcomingItem = {
  key: string;
  kind: "milestone" | "event" | "task";
  id: string;
  day: string;
  title: string;
  detail: string;
};

const upcomingKindOrder: Record<UpcomingItem["kind"], number> = {
  milestone: 0,
  event: 1,
  task: 2,
};

/** Pending milestones, unfinished events, and open due tasks from today onward, soonest first. */
export function upcomingItems(
  workspace: Pick<PlanWorkspace, "milestones" | "tasks" | "events">,
  today: string,
  now: Date,
  limit = 8,
): UpcomingItem[] {
  const items: (UpcomingItem & { order: string })[] = [];
  for (const milestone of workspace.milestones) {
    if (
      milestone.status !== "pending" ||
      milestone.targetDate === null ||
      milestone.targetDate < today
    )
      continue;
    items.push({
      key: `milestone-${milestone.id}`,
      kind: "milestone",
      id: milestone.id,
      day: milestone.targetDate,
      title: milestone.title,
      detail: "Milestone",
      order: "",
    });
  }
  for (const event of workspace.events) {
    if (eventHasEnded(event, now)) continue;
    items.push({
      key: `event-${event.id}`,
      kind: "event",
      id: event.id,
      day: eventDay(event),
      title: event.title,
      detail: `${timeLabel(event.startAtUtc)} · ${event.durationMinutes} min`,
      order: event.startAtUtc,
    });
  }
  for (const task of workspace.tasks) {
    if (task.status === "done" || task.dueDate === null || task.dueDate < today)
      continue;
    items.push({
      key: `task-${task.id}`,
      kind: "task",
      id: task.id,
      day: task.dueDate,
      title: task.title,
      detail: "Task due",
      order: "",
    });
  }
  return items
    .sort(
      (left, right) =>
        left.day.localeCompare(right.day) ||
        upcomingKindOrder[left.kind] - upcomingKindOrder[right.kind] ||
        left.order.localeCompare(right.order),
    )
    .slice(0, limit)
    .map(({ order: _order, ...item }) => item);
}

export type TimelineModel = {
  start: string;
  end: string;
  months: { key: string; label: string; offset: number }[];
  milestones: { milestone: Milestone; offset: number; lane: number }[];
  events: { event: ScheduleEvent; offset: number }[];
  today: number;
  planStart: number | null;
  planTarget: number | null;
};

const TIMELINE_LANES = 3;
const TIMELINE_LABEL_GAP = 16;

/**
 * Positions dated plan items on a horizontal axis as percentages. The window always includes
 * today so progress is visible, and nearby milestone labels are spread across lanes.
 * Returns null when the plan has nothing dated to place.
 */
export function timelineModel(
  plan: Pick<Plan, "startDate" | "targetDate">,
  milestones: Milestone[],
  events: ScheduleEvent[],
  today: string,
): TimelineModel | null {
  const dated = milestones
    .filter((milestone) => milestone.targetDate !== null)
    .sort(compareMilestones);
  const eventDays = events.map((event) => ({ event, day: eventDay(event) }));
  const days = [
    plan.startDate,
    plan.targetDate,
    ...dated.map((milestone) => milestone.targetDate),
    ...eventDays.map(({ day }) => day),
  ].filter((day): day is string => day !== null);
  if (days.length === 0) return null;
  days.push(today);
  days.sort();
  const earliest = days[0];
  const latest = days[days.length - 1];
  const span = daysBetween(earliest, latest);
  const padding = Math.max(3, Math.ceil(Math.max(span, 14) * 0.06));
  const start = offsetDay(earliest, -padding);
  const end = offsetDay(latest, padding + Math.max(0, 14 - span));
  const total = daysBetween(start, end);
  const position = (day: string) => (daysBetween(start, day) / total) * 100;

  const months: TimelineModel["months"] = [];
  for (
    let month = startOfMonth(parseISO(start));
    isoDay(month) <= end;
    month = addMonths(month, 1)
  ) {
    const day = isoDay(month);
    if (day < start) continue;
    months.push({
      key: day,
      label: format(
        month,
        months.length === 0 || month.getMonth() === 0 ? "MMM yyyy" : "MMM",
      ),
      offset: position(day),
    });
  }

  const laneEnds: number[] = [];
  const placed = dated.map((milestone) => {
    const offset = position(milestone.targetDate as string);
    let lane = laneEnds.findIndex(
      (previous) => offset - previous >= TIMELINE_LABEL_GAP,
    );
    if (lane === -1)
      lane =
        laneEnds.length < TIMELINE_LANES
          ? laneEnds.length
          : laneEnds.indexOf(Math.min(...laneEnds));
    laneEnds[lane] = offset;
    return { milestone, offset, lane };
  });

  return {
    start,
    end,
    months,
    milestones: placed,
    events: eventDays.map(({ event, day }) => ({
      event,
      offset: position(day),
    })),
    today: position(today),
    planStart: plan.startDate ? position(plan.startDate) : null,
    planTarget: plan.targetDate ? position(plan.targetDate) : null,
  };
}

/** Predicts `delete_plan`: tasks with no week, scheduled day, or due day go with the plan. */
export function planDeletionPreview(
  workspace: Pick<
    PlanWorkspace,
    "workstreams" | "milestones" | "tasks" | "events"
  >,
): PlanDeletion {
  const deletedTasks = workspace.tasks.filter(
    (task) =>
      task.scheduledDay === null &&
      task.dueDate === null &&
      task.plannedWeek === null,
  ).length;
  return {
    deletedWorkstreams: workspace.workstreams.length,
    deletedMilestones: workspace.milestones.length,
    deletedTasks,
    detachedTasks: workspace.tasks.length - deletedTasks,
    detachedEvents: workspace.events.length,
  };
}

export function plural(count: number, noun: string) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

export function planDeletionMessage(title: string, preview: PlanDeletion) {
  const removed = [
    preview.deletedWorkstreams &&
      plural(preview.deletedWorkstreams, "workstream"),
    preview.deletedMilestones && plural(preview.deletedMilestones, "milestone"),
    preview.deletedTasks && plural(preview.deletedTasks, "undated task"),
  ].filter(Boolean);
  const kept = [
    preview.detachedTasks && plural(preview.detachedTasks, "dated task"),
    preview.detachedEvents && plural(preview.detachedEvents, "event"),
  ].filter(Boolean);
  return [
    `Permanently delete “${title}”? This cannot be undone.`,
    removed.length ? `Also deleted: ${listPhrase(removed)}.` : "",
    kept.length
      ? `Kept on your calendar without a plan: ${listPhrase(kept)}.`
      : "",
  ]
    .filter(Boolean)
    .join("\n\n");
}

function listPhrase(items: (string | number)[]) {
  if (items.length <= 2) return items.join(" and ");
  return `${items.slice(0, -1).join(", ")}, and ${items[items.length - 1]}`;
}

export type TaskFilters = {
  status: "all" | "open" | TaskStatus;
  workstreamId: "all" | "none" | (string & {});
  ownerId: "all" | "none" | (string & {});
  milestoneId: "all" | "none" | (string & {});
};

export const allTasks: TaskFilters = {
  status: "all",
  workstreamId: "all",
  ownerId: "all",
  milestoneId: "all",
};

function matchesLink(value: string | null, filter: string) {
  if (filter === "all") return true;
  if (filter === "none") return value === null;
  return value === filter;
}

export function filterTasks(tasks: Task[], filters: TaskFilters) {
  return tasks.filter(
    (task) =>
      (filters.status === "all" ||
        (filters.status === "open"
          ? task.status !== "done"
          : task.status === filters.status)) &&
      matchesLink(task.workstreamId, filters.workstreamId) &&
      matchesLink(task.ownerId, filters.ownerId) &&
      matchesLink(task.milestoneId, filters.milestoneId),
  );
}

/** Completed and total task counts for each workstream, in workstream order. */
export function workstreamProgress(workstreams: Workstream[], tasks: Task[]) {
  return workstreams.map((workstream) => {
    const owned = tasks.filter((task) => task.workstreamId === workstream.id);
    return {
      workstream,
      done: owned.filter((task) => task.status === "done").length,
      total: owned.length,
    };
  });
}

export type AttentionSignal = {
  key: string;
  tone: "late" | "blocked" | "soon";
  text: string;
  taskIds: string[];
  milestoneId: string | null;
};

const ATTENTION_WINDOW_DAYS = 7;

/**
 * Deterministic warnings for a plan: overdue open tasks, blocked tasks, milestones past due, and
 * milestones due within a week that still have unfinished linked work.
 */
export function attentionSignals(
  workspace: Pick<PlanWorkspace, "milestones" | "tasks">,
  today: string,
): AttentionSignal[] {
  const open = workspace.tasks.filter((task) => task.status !== "done");
  const signals: AttentionSignal[] = [];
  const overdue = open.filter((task) => isOverdue(task.dueDate, today));
  if (overdue.length)
    signals.push({
      key: "overdue-tasks",
      tone: "late",
      text: `${plural(overdue.length, "overdue task")}`,
      taskIds: overdue.map((task) => task.id),
      milestoneId: null,
    });
  const blocked = open.filter((task) => task.status === "blocked");
  if (blocked.length)
    signals.push({
      key: "blocked-tasks",
      tone: "blocked",
      text: `${plural(blocked.length, "blocked task")}`,
      taskIds: blocked.map((task) => task.id),
      milestoneId: null,
    });
  for (const milestone of [...workspace.milestones].sort(compareMilestones)) {
    if (milestone.status !== "pending" || milestone.targetDate === null)
      continue;
    const distance = daysBetween(today, milestone.targetDate);
    const unfinished = open.filter((task) => task.milestoneId === milestone.id);
    if (distance < 0) {
      signals.push({
        key: `milestone-${milestone.id}`,
        tone: "late",
        text: `${milestone.title} was due ${relativeDayLabel(milestone.targetDate, today).toLowerCase()}`,
        taskIds: unfinished.map((task) => task.id),
        milestoneId: milestone.id,
      });
    } else if (distance <= ATTENTION_WINDOW_DAYS && unfinished.length) {
      signals.push({
        key: `milestone-${milestone.id}`,
        tone: "soon",
        text: `${milestone.title} is due ${relativeDayLabel(milestone.targetDate, today).toLowerCase()} with ${plural(unfinished.length, "open task")}`,
        taskIds: unfinished.map((task) => task.id),
        milestoneId: milestone.id,
      });
    }
  }
  return signals;
}

export type RunOfShowRow = {
  event: ScheduleEvent;
  start: Date;
  end: Date;
  status: "done" | "live" | "next" | "upcoming";
  gapMinutes: number;
  overlapsPrevious: boolean;
};

/** The distinct local days that have events, in order. */
export function eventDays(events: ScheduleEvent[]) {
  return [...new Set(events.map(eventDay))].sort();
}

/** The day a run of show should open on: today, else the next event day, else the last one. */
export function defaultRunOfShowDay(days: string[], today: string) {
  return days.find((day) => day >= today) ?? days[days.length - 1] ?? null;
}

/**
 * A cue sheet for one day: events in start order with a time-derived status, the gap since the
 * previous cue ended, and whether a cue starts before the previous one finishes.
 */
export function runOfShow(
  events: ScheduleEvent[],
  day: string,
  now: Date,
): RunOfShowRow[] {
  const ordered = events
    .filter((event) => eventDay(event) === day)
    .sort((left, right) => left.startAtUtc.localeCompare(right.startAtUtc));
  let nextAssigned = false;
  let previousEnd: Date | null = null;
  return ordered.map((event) => {
    const start = new Date(event.startAtUtc);
    const end = new Date(start.getTime() + event.durationMinutes * 60_000);
    let status: RunOfShowRow["status"] = "upcoming";
    if (end <= now) status = "done";
    else if (start <= now) status = "live";
    else if (!nextAssigned) {
      status = "next";
      nextAssigned = true;
    }
    if (status === "live") nextAssigned = true;
    const gapMinutes = previousEnd
      ? Math.round((start.getTime() - previousEnd.getTime()) / 60_000)
      : 0;
    const row = {
      event,
      start,
      end,
      status,
      gapMinutes: Math.max(0, gapMinutes),
      overlapsPrevious: gapMinutes < 0,
    };
    previousEnd = previousEnd && previousEnd > end ? previousEnd : end;
    return row;
  });
}

export type WeekDay = {
  day: string;
  events: ScheduleEvent[];
  tasks: Task[];
  dueTasks: Task[];
  milestones: Milestone[];
};

/**
 * Spreads a week agenda over its seven days. An event that began before the week is listed on
 * the first day; every other item appears on the day it starts, is scheduled, or is due.
 */
export function groupWeek(agenda: Agenda, startDay: string): WeekDay[] {
  const days = Array.from({ length: 7 }, (_, index) =>
    offsetDay(startDay, index),
  );
  return days.map((day) => ({
    day,
    events: agenda.events.filter((event) => {
      const start = eventDay(event);
      return start === day || (day === startDay && start < startDay);
    }),
    tasks: agenda.tasks.filter((task) => task.scheduledDay === day),
    dueTasks: agenda.dueTasks.filter((task) => task.dueDate === day),
    milestones: agenda.milestones.filter(
      (milestone) => milestone.targetDate === day,
    ),
  }));
}

export const estimatePresets = [15, 30, 45, 60, 120] as const;

/** "15 min", "1 h", or "1 h 30 min". */
export function estimateLabel(minutes: number) {
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (!hours) return `${rest} min`;
  return rest ? `${hours} h ${rest} min` : `${hours} h`;
}

/** Estimated minutes of the open tasks, and how many open tasks have no estimate. */
export function workload(tasks: Task[]) {
  let minutes = 0;
  let unestimated = 0;
  let open = 0;
  for (const task of tasks) {
    if (task.status === "done") continue;
    open += 1;
    if (task.estimatedMinutes === null) unestimated += 1;
    else minutes += task.estimatedMinutes;
  }
  return { minutes, unestimated, open };
}

/** "2 h 15 min estimated · 3 without an estimate", or null when nothing is open. */
export function workloadLabel(load: ReturnType<typeof workload>) {
  if (!load.open) return null;
  const parts = [];
  if (load.minutes) parts.push(`${estimateLabel(load.minutes)} estimated`);
  if (load.unestimated)
    parts.push(
      `${load.unestimated} without ${load.unestimated === 1 ? "an estimate" : "estimates"}`,
    );
  return parts.join(" · ");
}

/** Whether `day` falls in the seven days starting at `weekStart`. */
export function inWeek(day: string | null, weekStart: string) {
  return day !== null && day >= weekStart && day < offsetDay(weekStart, 7);
}

/** A task belongs to a week when it was chosen for it or is scheduled on one of its days. */
export function belongsToWeek(task: Task, weekStart: string) {
  return (
    inWeek(task.scheduledDay, weekStart) ||
    (task.scheduledDay === null && inWeek(task.plannedWeek, weekStart))
  );
}

/** "This week", "Next week", "Last week", or "Week of 21 Sep", relative to `currentWeekStart`. */
export function weekLabel(week: string, currentWeekStart: string) {
  if (inWeek(week, currentWeekStart)) return "This week";
  if (inWeek(week, offsetDay(currentWeekStart, 7))) return "Next week";
  if (inWeek(week, offsetDay(currentWeekStart, -7))) return "Last week";
  return `Week of ${shortDate(week)}`;
}

export type MoveTarget =
  | { kind: "day"; day: string }
  | { kind: "week"; weekStart: string }
  | { kind: "unschedule" }
  | { kind: "done" };

const unchanged: DayChange = { action: "unchanged" };
const cleared: DayChange = { action: "clear" };

/**
 * The move that puts `task` on a day, in a week's pool, or back in its plan. Scheduling a day
 * also records that day's week, so returning the task to the pool keeps it in the same week.
 */
export function taskMove(
  task: Pick<Task, "id" | "revision">,
  target: MoveTarget,
  weekStartsOn: WeekStart,
): TaskMove {
  const base = {
    id: task.id,
    revision: task.revision,
    scheduledDay: unchanged,
    plannedWeek: unchanged,
  };
  switch (target.kind) {
    case "day":
      return {
        ...base,
        scheduledDay: { action: "set", day: target.day },
        plannedWeek: {
          action: "set",
          day: weekStartDay(target.day, weekStartsOn),
        },
      };
    case "week":
      return {
        ...base,
        scheduledDay: cleared,
        plannedWeek: { action: "set", day: target.weekStart },
      };
    case "unschedule":
      return { ...base, scheduledDay: cleared, plannedWeek: cleared };
    case "done":
      return { ...base, status: "done" };
  }
}

/** A task can leave every week and day only if its plan or due date still gives it a home. */
export function canUnschedule(task: Pick<Task, "planId" | "dueDate">) {
  return task.planId !== null || task.dueDate !== null;
}

export type MilestoneGroup = { milestone: Milestone; tasks: Task[] };

/** Hands each open task to the first section that claims it, so every task is listed once. */
function claimer(tasks: Task[], placed: Set<string>) {
  const open = tasks.filter((task) => task.status !== "done");
  return (predicate: (task: Task) => boolean) => {
    const claimed = open.filter(
      (task) => !placed.has(task.id) && predicate(task),
    );
    for (const task of claimed) placed.add(task.id);
    return claimed;
  };
}

function byPlacement(left: Task, right: Task) {
  return (
    (left.scheduledDay ?? "").localeCompare(right.scheduledDay ?? "") ||
    Number(left.status === "done") - Number(right.status === "done") ||
    left.sortOrder - right.sortOrder
  );
}

/**
 * What a weekly planning session shows: the week's chosen work, then open work to choose from —
 * work left from earlier weeks, overdue and soon-due tasks, tasks behind upcoming milestones,
 * and each active plan's unscheduled backlog. Work already placed in a later week is left out.
 */
export function weekPlanning(
  board: PlanningBoard,
  weekStart: string,
  today: string,
  plans: Plan[],
) {
  const weekEnd = offsetDay(weekStart, 7);
  const chosen = board.tasks
    .filter((task) => belongsToWeek(task, weekStart))
    .sort(byPlacement);
  const placed = new Set(chosen.map((task) => task.id));
  for (const task of board.tasks) {
    const later =
      task.scheduledDay !== null
        ? task.scheduledDay >= weekEnd
        : task.plannedWeek !== null && task.plannedWeek >= weekEnd;
    if (later) placed.add(task.id);
  }
  const take = claimer(board.tasks, placed);
  const carried = take((task) =>
    task.scheduledDay !== null
      ? task.scheduledDay < weekStart
      : task.plannedWeek !== null && task.plannedWeek < weekStart,
  );
  const overdue = take((task) => task.dueDate !== null && task.dueDate < today);
  const dueSoon = take(
    (task) => task.dueDate !== null && task.dueDate < offsetDay(weekEnd, 7),
  );
  const milestones: MilestoneGroup[] = board.milestones
    .filter(
      (milestone) => (milestone.targetDate ?? "") < offsetDay(weekEnd, 14),
    )
    .map((milestone) => ({
      milestone,
      tasks: take((task) => task.milestoneId === milestone.id),
    }));
  const backlog = plans
    .filter(
      (plan) =>
        !plan.archived &&
        (plan.status === "active" || plan.status === "planning"),
    )
    .map((plan) => ({
      plan,
      tasks: take(
        (task) =>
          task.planId === plan.id &&
          task.scheduledDay === null &&
          task.plannedWeek === null,
      ),
    }))
    .filter((group) => group.tasks.length > 0);
  return { chosen, carried, overdue, dueSoon, milestones, backlog };
}

/**
 * What a daily planning session shows: the day's plan, then open work to add — unfinished tasks
 * from earlier days, tasks due today, this week's unscheduled choices, overdue work, and tasks
 * behind milestones due within a week.
 */
export function dayPlanning(
  board: PlanningBoard,
  today: string,
  weekStart: string,
) {
  const planned = board.tasks
    .filter((task) => task.scheduledDay === today)
    .sort(byPlacement);
  const take = claimer(board.tasks, new Set(planned.map((task) => task.id)));
  const unfinished = take(
    (task) => task.scheduledDay !== null && task.scheduledDay < today,
  );
  const dueToday = take((task) => task.dueDate === today);
  const thisWeek = take(
    (task) => task.scheduledDay === null && inWeek(task.plannedWeek, weekStart),
  );
  const overdue = take((task) => task.dueDate !== null && task.dueDate < today);
  const milestones: MilestoneGroup[] = board.milestones
    .filter((milestone) => (milestone.targetDate ?? "") <= offsetDay(today, 7))
    .map((milestone) => ({
      milestone,
      tasks: take(
        (task) =>
          task.milestoneId === milestone.id &&
          (task.scheduledDay === null || task.scheduledDay > today),
      ),
    }))
    .filter((group) => group.tasks.length > 0);
  return { planned, unfinished, dueToday, thisWeek, overdue, milestones };
}

/** Open tasks whose scheduled day has passed, oldest first. */
export function unfinishedTasks(tasks: Task[], today: string) {
  return tasks
    .filter(
      (task) =>
        task.status !== "done" &&
        task.scheduledDay !== null &&
        task.scheduledDay < today,
    )
    .sort(byPlacement);
}

export type PlanningPromptState = { week: string | null; day: string | null };

/**
 * Which session Today offers: weekly planning until this week (or a later one) is planned or
 * dismissed, then daily planning until today is.
 */
export function planningPrompt(
  state: PlanningPromptState,
  weekStart: string,
  today: string,
): "week" | "day" | null {
  if (state.week === null || state.week < weekStart) return "week";
  if (state.day !== today) return "day";
  return null;
}

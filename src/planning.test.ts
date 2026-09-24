import { describe, expect, it } from "vitest";
import type { Milestone, Plan, ScheduleEvent, Task } from "./api";
import { localeWeekStart, rangeLabel, weekStartDay } from "./date";
import {
  attentionSignals,
  belongsToWeek,
  canUnschedule,
  dayPlanning,
  estimateLabel,
  featuredTasks,
  filterTasks,
  groupWeek,
  runOfShow,
  workstreamProgress,
  matchesPlanFilter,
  nextMilestone,
  overdueTaskCount,
  paddedCount,
  planDeletionMessage,
  planDeletionPreview,
  planningPrompt,
  planSchedule,
  relativeDayLabel,
  shortDate,
  taskCounts,
  taskHasHome,
  checklistProgress,
  openWaits,
  recurrenceLabel,
  taskMove,
  timelineModel,
  unfinishedTasks,
  upcomingItems,
  weekLabel,
  weekPlanning,
  workload,
  workloadLabel,
} from "./planning";

const planId = "30bb9c6a-4020-45a6-806b-5eb71c7ae76f";
const stamp = "2026-09-01T12:00:00.000Z";
let sequence = 0;
const nextId = () =>
  `00000000-0000-4000-8000-${String(++sequence).padStart(12, "0")}`;

function milestone(overrides: Partial<Milestone>): Milestone {
  return {
    id: nextId(),
    planId,
    title: "Milestone",
    description: "",
    targetDate: null,
    status: "pending",
    workstreamId: null,
    sortOrder: 0,
    revision: 1,
    createdAt: stamp,
    updatedAt: stamp,
    ...overrides,
  };
}

function task(overrides: Partial<Task>): Task {
  return {
    id: nextId(),
    title: "Task",
    description: "",
    planId,
    milestoneId: null,
    workstreamId: null,
    ownerId: null,
    dueDate: null,
    scheduledDay: null,
    plannedWeek: null,
    estimatedMinutes: null,
    status: "todo",
    priority: "normal",
    completedAt: null,
    recurrence: null,
    checklist: [],
    waitingOn: [],
    sortOrder: 0,
    revision: 1,
    createdAt: stamp,
    updatedAt: stamp,
    ...overrides,
  };
}

function event(overrides: Partial<ScheduleEvent>): ScheduleEvent {
  return {
    id: nextId(),
    title: "Event",
    notes: "",
    startAtUtc: "2026-09-20T12:00:00.000Z",
    timeZone: "America/New_York",
    durationMinutes: 60,
    reminderMinutesBefore: null,
    reminderStatus: "none",
    planId,
    location: "",
    workstreamId: null,
    ownerId: null,
    revision: 1,
    createdAt: stamp,
    updatedAt: stamp,
    ...overrides,
  };
}

describe("plan signals", () => {
  it("counts tasks by status", () => {
    const counts = taskCounts([
      task({ status: "todo" }),
      task({ status: "blocked" }),
      task({ status: "done" }),
      task({ status: "done" }),
    ]);
    expect(counts).toMatchObject({
      todo: 1,
      in_progress: 0,
      blocked: 1,
      done: 2,
      total: 4,
    });
  });

  it("picks the earliest pending milestone and leaves undated ones last", () => {
    const undated = milestone({ title: "Retro" });
    const complete = milestone({
      title: "Venue",
      targetDate: "2026-09-10",
      status: "complete",
    });
    const lineup = milestone({ title: "Lineup", targetDate: "2026-09-22" });
    expect(nextMilestone([undated, complete, lineup])?.title).toBe("Lineup");
    expect(nextMilestone([undated])?.title).toBe("Retro");
    expect(nextMilestone([complete])).toBeNull();
  });

  it("describes days relative to today", () => {
    expect(relativeDayLabel("2026-09-14", "2026-09-14")).toBe("Today");
    expect(relativeDayLabel("2026-09-15", "2026-09-14")).toBe("Tomorrow");
    expect(relativeDayLabel("2026-09-22", "2026-09-14")).toBe("In 8 days");
    expect(relativeDayLabel("2026-09-11", "2026-09-14")).toBe("3 days ago");
  });

  it("lists upcoming work in date order and skips finished items", () => {
    const now = new Date("2026-09-14T15:00:00.000Z");
    const items = upcomingItems(
      {
        milestones: [
          milestone({ title: "Event day", targetDate: "2026-10-16" }),
          milestone({ title: "Venue signed", targetDate: "2026-09-01" }),
        ],
        tasks: [
          task({ title: "Confirm AV", dueDate: "2026-09-18" }),
          task({
            title: "Done already",
            dueDate: "2026-09-18",
            status: "done",
          }),
          task({ title: "Overdue", dueDate: "2026-09-10" }),
        ],
        events: [
          event({
            title: "Walkthrough",
            startAtUtc: "2026-09-18T12:00:00.000Z",
          }),
          event({ title: "Kickoff", startAtUtc: "2026-09-02T12:00:00.000Z" }),
        ],
      },
      "2026-09-14",
      now,
    );
    expect(items.map((item) => item.title)).toEqual([
      "Walkthrough",
      "Confirm AV",
      "Event day",
    ]);
  });

  it("requires a plan, week, scheduled day, or due date", () => {
    const homeless = {
      planId: null,
      dueDate: null,
      scheduledDay: null,
      plannedWeek: null,
    };
    expect(taskHasHome(homeless)).toBe(false);
    expect(taskHasHome({ ...homeless, dueDate: "2026-09-18" })).toBe(true);
    expect(taskHasHome({ ...homeless, plannedWeek: "2026-09-13" })).toBe(true);
  });

  it("filters agenda items by plan membership", () => {
    const planned = { planId };
    const loose = { planId: null };
    expect(matchesPlanFilter(planned, "all")).toBe(true);
    expect(matchesPlanFilter(loose, "none")).toBe(true);
    expect(matchesPlanFilter(planned, "none")).toBe(false);
    expect(matchesPlanFilter(planned, planId)).toBe(true);
    expect(matchesPlanFilter(loose, planId)).toBe(false);
  });
});

describe("display labels", () => {
  it("formats plan dates day first with the time left to target", () => {
    const plan = { startDate: "2026-08-01", targetDate: "2026-10-04" };
    expect(shortDate("2026-10-04")).toBe("04 Oct");
    expect(planSchedule(plan, "2026-09-14")).toBe(
      "01 Aug 2026 → 04 Oct 2026 · 20 days left",
    );
    expect(planSchedule(plan, "2026-10-04")).toBe(
      "01 Aug 2026 → 04 Oct 2026 · Target is today",
    );
    expect(planSchedule(plan, "2026-10-05")).toBe(
      "01 Aug 2026 → 04 Oct 2026 · 1 day past target",
    );
    expect(
      planSchedule({ startDate: "2026-08-01", targetDate: null }, "2026-09-14"),
    ).toBe("Starts 01 Aug 2026");
    expect(
      planSchedule({ startDate: null, targetDate: null }, "2026-09-14"),
    ).toBe("No dates yet");
    expect(paddedCount(3)).toBe("03");
    expect(paddedCount(12)).toBe("12");
  });

  it("features moving work first, then priority and due day", () => {
    const featured = featuredTasks([
      task({ title: "Low todo", priority: "low" }),
      task({ title: "Finished", status: "done", priority: "critical" }),
      task({ title: "High todo", priority: "high", dueDate: "2026-09-30" }),
      task({ title: "Blocked", status: "blocked" }),
      task({ title: "Moving", status: "in_progress" }),
      task({ title: "High sooner", priority: "high", dueDate: "2026-09-20" }),
    ]);
    expect(featured.map((item) => item.title)).toEqual([
      "Moving",
      "Blocked",
      "High sooner",
    ]);
    expect(
      overdueTaskCount(
        [
          task({ dueDate: "2026-09-13" }),
          task({ dueDate: "2026-09-13", status: "done" }),
          task({ dueDate: "2026-09-14" }),
          task({}),
        ],
        "2026-09-14",
      ),
    ).toBe(1);
  });
});

describe("task details", () => {
  it("names repeat rules the way a person would", () => {
    const rule = (overrides: Partial<Task["recurrence"] & object>) => ({
      frequency: "daily" as const,
      interval: 1,
      weekdays: [],
      monthDay: null,
      ...overrides,
    });
    expect(recurrenceLabel(rule({}))).toBe("Every day");
    expect(recurrenceLabel(rule({ interval: 3 }))).toBe("Every 3 days");
    expect(recurrenceLabel(rule({ frequency: "weekdays" }))).toBe(
      "Every weekday",
    );
    expect(
      recurrenceLabel(
        rule({ frequency: "weekly", interval: 2, weekdays: [1, 4] }),
      ),
    ).toBe("Every 2 weeks on Mon, Thu");
    expect(recurrenceLabel(rule({ frequency: "monthly", monthDay: 31 }))).toBe(
      "Every month on the 31st",
    );
    expect(recurrenceLabel(rule({ frequency: "monthly", monthDay: 12 }))).toBe(
      "Every month on the 12th",
    );
    expect(recurrenceLabel(rule({ frequency: "monthly", monthDay: 22 }))).toBe(
      "Every month on the 22nd",
    );
  });

  it("counts checklist progress and ignores tasks without one", () => {
    expect(checklistProgress(task({}))).toBeNull();
    expect(
      checklistProgress(
        task({
          checklist: [
            { text: "Pack", done: true },
            { text: "Label", done: false },
          ],
        }),
      ),
    ).toEqual({ done: 1, total: 2 });
  });

  it("only waits on tasks that are still open", () => {
    const open = {
      id: "open-task",
      title: "Book venue",
      planId: null,
      scheduledDay: null,
      dueDate: null,
    };
    const waiting = task({ waitingOn: ["open-task", "finished-task"] });
    expect(openWaits(waiting, new Map([[open.id, open]]))).toEqual([open]);
    expect(openWaits(waiting, new Map())).toEqual([]);
  });
});

describe("timeline", () => {
  it("returns nothing when no item has a date", () => {
    expect(
      timelineModel(
        { startDate: null, targetDate: null },
        [milestone({})],
        [],
        "2026-09-14",
      ),
    ).toBeNull();
  });

  it("places dated items and today inside the window in order", () => {
    const model = timelineModel(
      { startDate: "2026-09-01", targetDate: "2026-10-16" },
      [
        milestone({ title: "Lineup", targetDate: "2026-09-22" }),
        milestone({ title: "Rehearsal", targetDate: "2026-10-14" }),
        milestone({ title: "Undated" }),
      ],
      [event({ startAtUtc: "2026-10-02T12:00:00.000Z" })],
      "2026-09-14",
    );
    expect(model).not.toBeNull();
    const offsets = [
      model!.planStart!,
      model!.today,
      model!.milestones[0].offset,
      model!.events[0].offset,
      model!.milestones[1].offset,
      model!.planTarget!,
    ];
    expect(offsets).toEqual([...offsets].sort((a, b) => a - b));
    expect(Math.min(...offsets)).toBeGreaterThan(0);
    expect(Math.max(...offsets)).toBeLessThan(100);
    expect(model!.milestones).toHaveLength(2);
    expect(model!.months.map((month) => month.label)).toEqual([
      "Sep 2026",
      "Oct",
    ]);
  });

  it("spreads crowded milestone labels across lanes", () => {
    const model = timelineModel(
      { startDate: null, targetDate: null },
      ["2026-09-20", "2026-09-21", "2026-09-22"].map((targetDate) =>
        milestone({ targetDate }),
      ),
      [],
      "2026-09-20",
    );
    expect(model!.milestones.map((item) => item.lane)).toEqual([0, 1, 2]);
  });
});

describe("plan deletion preview", () => {
  it("matches the repository's keep-or-delete rule", () => {
    const preview = planDeletionPreview({
      workstreams: [],
      milestones: [milestone({})],
      tasks: [
        task({}),
        task({ dueDate: "2026-10-13" }),
        task({ scheduledDay: "2026-10-12" }),
        task({ plannedWeek: "2026-10-11" }),
      ],
      events: [event({})],
    });
    expect(preview).toEqual({
      deletedWorkstreams: 0,
      deletedMilestones: 1,
      deletedTasks: 1,
      detachedTasks: 3,
      detachedEvents: 1,
    });
    const message = planDeletionMessage("Showcase", preview);
    expect(message).toContain("1 milestone and 1 undated task");
    expect(message).toContain("3 dated tasks and 1 event");
  });
});

describe("plan views", () => {
  it("filters tasks by status, workstream, owner, and milestone", () => {
    const stream = nextId();
    const owner = nextId();
    const tasks = [
      task({ title: "Overlays", workstreamId: stream, ownerId: owner }),
      task({ title: "Bios", status: "done", ownerId: owner }),
      task({ title: "Posts", status: "blocked" }),
    ];
    const titles = (filters: Parameters<typeof filterTasks>[1]) =>
      filterTasks(tasks, filters).map((item) => item.title);
    const all = {
      status: "all",
      workstreamId: "all",
      ownerId: "all",
      milestoneId: "all",
    } as const;
    expect(titles({ ...all, status: "open" })).toEqual(["Overlays", "Posts"]);
    expect(titles({ ...all, workstreamId: stream })).toEqual(["Overlays"]);
    expect(titles({ ...all, ownerId: "none" })).toEqual(["Posts"]);
    expect(titles({ ...all, ownerId: owner, status: "done" })).toEqual([
      "Bios",
    ]);
  });

  it("reports progress per workstream", () => {
    const stream = {
      id: nextId(),
      planId,
      name: "Production",
      description: "",
      sortOrder: 0,
      revision: 1,
      createdAt: stamp,
      updatedAt: stamp,
    };
    expect(
      workstreamProgress(
        [stream],
        [
          task({ workstreamId: stream.id, status: "done" }),
          task({ workstreamId: stream.id }),
          task({}),
        ],
      )[0],
    ).toMatchObject({ done: 1, total: 2 });
  });

  it("flags overdue, blocked, and at-risk milestone work deterministically", () => {
    const lineup = milestone({
      title: "Creator lineup locked",
      targetDate: "2026-09-16",
    });
    const venue = milestone({
      title: "Venue signed",
      targetDate: "2026-09-10",
    });
    const later = milestone({ title: "Event day", targetDate: "2026-10-16" });
    const signals = attentionSignals(
      {
        milestones: [later, lineup, venue],
        tasks: [
          task({ title: "Confirm creators", milestoneId: lineup.id }),
          task({ title: "Late bios", dueDate: "2026-09-12" }),
          task({ title: "Stuck", status: "blocked", milestoneId: later.id }),
          task({ title: "Finished", dueDate: "2026-09-01", status: "done" }),
        ],
      },
      "2026-09-14",
    );
    expect(signals.map((signal) => signal.text)).toEqual([
      "1 overdue task",
      "1 blocked task",
      "Venue signed was due 4 days ago",
      "Creator lineup locked is due in 2 days with 1 open task",
    ]);
  });

  it("builds a run of show with status, gaps, and overlaps", () => {
    const now = new Date("2026-11-09T15:10:00.000Z");
    const rows = runOfShow(
      [
        event({
          title: "AV test",
          startAtUtc: "2026-11-09T14:30:00.000Z",
          durationMinutes: 30,
        }),
        event({
          title: "Crew call",
          startAtUtc: "2026-11-09T14:00:00.000Z",
          durationMinutes: 30,
        }),
        event({
          title: "Creator briefing",
          startAtUtc: "2026-11-09T15:00:00.000Z",
          durationMinutes: 30,
        }),
        event({
          title: "Doors open",
          startAtUtc: "2026-11-09T15:20:00.000Z",
          durationMinutes: 10,
        }),
        event({
          title: "Opening",
          startAtUtc: "2026-11-09T15:45:00.000Z",
          durationMinutes: 15,
        }),
      ],
      "2026-11-09",
      now,
    );
    expect(rows.map((row) => [row.event.title, row.status])).toEqual([
      ["Crew call", "done"],
      ["AV test", "done"],
      ["Creator briefing", "live"],
      ["Doors open", "upcoming"],
      ["Opening", "upcoming"],
    ]);
    expect(rows[3].overlapsPrevious).toBe(true);
    expect(rows[4].gapMinutes).toBe(15);
  });

  it("marks the first unstarted cue as next when nothing is live", () => {
    const rows = runOfShow(
      [
        event({ startAtUtc: "2026-11-09T14:00:00.000Z" }),
        event({ startAtUtc: "2026-11-09T16:00:00.000Z" }),
      ],
      "2026-11-09",
      new Date("2026-11-09T15:30:00.000Z"),
    );
    expect(rows.map((row) => row.status)).toEqual(["done", "next"]);
  });

  it("spreads a week agenda across its days", () => {
    const days = groupWeek(
      {
        events: [
          event({
            title: "Carry-over",
            startAtUtc: "2026-11-08T12:00:00.000Z",
          }),
          event({ title: "Opening", startAtUtc: "2026-11-11T12:00:00.000Z" }),
        ],
        tasks: [task({ title: "Overlays", scheduledDay: "2026-11-10" })],
        dueTasks: [task({ title: "Graphics", dueDate: "2026-11-15" })],
        blocks: [],
        milestones: [
          milestone({ title: "Event start", targetDate: "2026-11-09" }),
        ],
      },
      "2026-11-09",
    );
    expect(days).toHaveLength(7);
    expect(days[0].events.map((item) => item.title)).toEqual(["Carry-over"]);
    expect(days[0].milestones.map((item) => item.title)).toEqual([
      "Event start",
    ]);
    expect(days[1].tasks.map((item) => item.title)).toEqual(["Overlays"]);
    expect(days[2].events.map((item) => item.title)).toEqual(["Opening"]);
    expect(days[6].dueTasks.map((item) => item.title)).toEqual(["Graphics"]);
  });

  it("finds locale week starts and labels ranges", () => {
    expect(localeWeekStart("en-GB")).toBe(1);
    expect(weekStartDay("2026-11-11", 1)).toBe("2026-11-09");
    expect(weekStartDay("2026-11-11", 0)).toBe("2026-11-08");
    expect(rangeLabel("2026-11-09", "2026-11-15")).toBe(
      "November 9 – 15, 2026",
    );
    expect(rangeLabel("2026-10-29", "2026-11-04")).toBe("Oct 29 – Nov 4, 2026");
  });
});

function plan(overrides: Partial<Plan>): Plan {
  return {
    id: planId,
    title: "Wedding",
    description: "",
    status: "active",
    startDate: null,
    targetDate: null,
    color: null,
    archived: false,
    links: [],
    revision: 1,
    createdAt: stamp,
    updatedAt: stamp,
    ...overrides,
  };
}

describe("planning horizons", () => {
  const week = "2026-09-13";
  const today = "2026-09-15";

  it("labels estimates, workloads, and weeks", () => {
    expect(estimateLabel(15)).toBe("15 min");
    expect(estimateLabel(60)).toBe("1 h");
    expect(estimateLabel(90)).toBe("1 h 30 min");
    const load = workload([
      task({ estimatedMinutes: 45 }),
      task({ estimatedMinutes: 90 }),
      task({}),
      task({ estimatedMinutes: 30, status: "done" }),
    ]);
    expect(load).toEqual({ minutes: 135, unestimated: 1, open: 3 });
    expect(workloadLabel(load)).toBe(
      "2 h 15 min estimated · 1 without an estimate",
    );
    expect(workloadLabel(workload([]))).toBeNull();
    expect(weekLabel("2026-09-13", week)).toBe("This week");
    expect(weekLabel("2026-09-14", week)).toBe("This week");
    expect(weekLabel("2026-09-20", week)).toBe("Next week");
    expect(weekLabel("2026-09-06", week)).toBe("Last week");
    expect(weekLabel("2026-10-04", week)).toBe("Week of 04 Oct");
  });

  it("moves one task between day, week, and plan", () => {
    const chosen = task({ plannedWeek: week });
    expect(belongsToWeek(chosen, week)).toBe(true);
    expect(belongsToWeek(task({ scheduledDay: "2026-09-19" }), week)).toBe(
      true,
    );
    expect(belongsToWeek(task({ scheduledDay: "2026-09-20" }), week)).toBe(
      false,
    );
    expect(
      belongsToWeek(
        task({ plannedWeek: week, scheduledDay: "2026-09-21" }),
        week,
      ),
    ).toBe(false);
    expect(taskMove(chosen, { kind: "day", day: "2026-09-21" }, 0)).toEqual({
      id: chosen.id,
      revision: 1,
      scheduledDay: { action: "set", day: "2026-09-21" },
      plannedWeek: { action: "set", day: "2026-09-20" },
    });
    expect(
      taskMove(chosen, { kind: "week", weekStart: week }, 0),
    ).toMatchObject({
      scheduledDay: { action: "clear" },
      plannedWeek: { action: "set", day: week },
    });
    expect(taskMove(chosen, { kind: "unschedule" }, 0)).toMatchObject({
      scheduledDay: { action: "clear" },
      plannedWeek: { action: "clear" },
    });
    expect(taskMove(chosen, { kind: "done" }, 0)).toMatchObject({
      status: "done",
      scheduledDay: { action: "unchanged" },
    });
    expect(canUnschedule(task({ planId: null, dueDate: null }))).toBe(false);
    expect(canUnschedule(task({ planId: null, dueDate: "2026-09-30" }))).toBe(
      true,
    );
  });

  it("lists each open task once in a weekly session", () => {
    const pending = milestone({
      title: "Venue booked",
      targetDate: "2026-09-25",
    });
    const board = {
      milestones: [pending],
      tasks: [
        task({ title: "Chosen", plannedWeek: week }),
        task({
          title: "Scheduled",
          scheduledDay: "2026-09-16",
          status: "done",
        }),
        task({ title: "Carried", plannedWeek: "2026-09-06" }),
        task({
          title: "Late",
          scheduledDay: "2026-09-10",
          dueDate: "2026-09-12",
        }),
        task({ title: "Overdue", dueDate: "2026-09-12" }),
        task({ title: "Due soon", dueDate: "2026-09-24" }),
        task({ title: "For the venue", milestoneId: pending.id }),
        task({ title: "Backlog" }),
        task({ title: "Placed later", plannedWeek: "2026-09-20" }),
        task({ title: "Loose", planId: null, dueDate: "2026-12-01" }),
      ],
    };
    const titles = (tasks: Task[]) => tasks.map((item) => item.title);
    const sessions = weekPlanning(board, week, today, [plan({})]);
    expect(titles(sessions.chosen)).toEqual(["Chosen", "Scheduled"]);
    expect(titles(sessions.carried)).toEqual(["Carried", "Late"]);
    expect(titles(sessions.overdue)).toEqual(["Overdue"]);
    expect(titles(sessions.dueSoon)).toEqual(["Due soon"]);
    expect(sessions.milestones.map((group) => titles(group.tasks))).toEqual([
      ["For the venue"],
    ]);
    expect(sessions.backlog.map((group) => titles(group.tasks))).toEqual([
      ["Backlog"],
    ]);
    expect(
      weekPlanning(board, week, today, [plan({ status: "on_hold" })]).backlog,
    ).toEqual([]);
  });

  it("builds a daily session from unfinished, due, and chosen work", () => {
    const soon = milestone({ title: "Rehearsal", targetDate: "2026-09-18" });
    const board = {
      milestones: [soon],
      tasks: [
        task({ title: "Planned", scheduledDay: today }),
        task({ title: "Yesterday", scheduledDay: "2026-09-14" }),
        task({
          title: "Due today",
          dueDate: today,
          scheduledDay: "2026-09-17",
        }),
        task({ title: "This week", plannedWeek: week }),
        task({ title: "Overdue", dueDate: "2026-09-01" }),
        task({ title: "Rehearsal prep", milestoneId: soon.id }),
        task({
          title: "Done earlier",
          scheduledDay: "2026-09-14",
          status: "done",
        }),
      ],
    };
    const titles = (tasks: Task[]) => tasks.map((item) => item.title);
    const session = dayPlanning(board, today, week);
    expect(titles(session.planned)).toEqual(["Planned"]);
    expect(titles(session.unfinished)).toEqual(["Yesterday"]);
    expect(titles(session.dueToday)).toEqual(["Due today"]);
    expect(titles(session.thisWeek)).toEqual(["This week"]);
    expect(titles(session.overdue)).toEqual(["Overdue"]);
    expect(session.milestones.map((group) => titles(group.tasks))).toEqual([
      ["Rehearsal prep"],
    ]);
    expect(titles(unfinishedTasks(board.tasks, today))).toEqual(["Yesterday"]);
  });

  it("offers the week first, then the day, until each is planned", () => {
    expect(planningPrompt({ week: null, day: null }, week, today)).toBe("week");
    expect(
      planningPrompt({ week: "2026-09-06", day: today }, week, today),
    ).toBe("week");
    expect(planningPrompt({ week, day: "2026-09-14" }, week, today)).toBe(
      "day",
    );
    expect(
      planningPrompt({ week: "2026-09-20", day: today }, week, today),
    ).toBe(null);
  });
});

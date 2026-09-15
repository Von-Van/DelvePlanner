import { describe, expect, it } from "vitest";
import type { Milestone, ScheduleEvent, Task } from "./api";
import { localeWeekStart, rangeLabel, weekStartDay } from "./date";
import {
  attentionSignals,
  featuredTasks,
  filterTasks,
  focusEventId,
  groupWeek,
  runOfShow,
  workstreamProgress,
  matchesPlanFilter,
  nextMilestone,
  overdueTaskCount,
  paddedCount,
  planDeletionMessage,
  planDeletionPreview,
  planSchedule,
  relativeDayLabel,
  shortDate,
  taskCounts,
  taskHasHome,
  timelineModel,
  upcomingItems,
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
    status: "todo",
    priority: "normal",
    completedAt: null,
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

  it("requires a plan, scheduled day, or due date", () => {
    expect(
      taskHasHome({ planId: null, dueDate: null, scheduledDay: null }),
    ).toBe(false);
    expect(
      taskHasHome({ planId: null, dueDate: "2026-09-18", scheduledDay: null }),
    ).toBe(true);
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

  it("focuses the live event, else the next one to start", () => {
    const morning = event({
      startAtUtc: "2026-09-14T08:30:00.000Z",
      durationMinutes: 45,
    });
    const lunch = event({
      startAtUtc: "2026-09-14T12:00:00.000Z",
      durationMinutes: 60,
    });
    const evening = event({ startAtUtc: "2026-09-14T18:00:00.000Z" });
    const events = [evening, morning, lunch];
    expect(focusEventId(events, new Date("2026-09-14T08:45:00.000Z"))).toBe(
      morning.id,
    );
    expect(focusEventId(events, new Date("2026-09-14T10:00:00.000Z"))).toBe(
      lunch.id,
    );
    expect(focusEventId(events, new Date("2026-09-14T20:00:00.000Z"))).toBe(
      null,
    );
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
      ],
      events: [event({})],
    });
    expect(preview).toEqual({
      deletedWorkstreams: 0,
      deletedMilestones: 1,
      deletedTasks: 1,
      detachedTasks: 2,
      detachedEvents: 1,
    });
    const message = planDeletionMessage("Showcase", preview);
    expect(message).toContain("1 milestone and 1 undated task");
    expect(message).toContain("2 dated tasks and 1 event");
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

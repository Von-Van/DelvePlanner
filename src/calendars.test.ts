import { describe, expect, it } from "vitest";
import {
  Calendar,
  calendarSchema,
  Capacity,
  capacitySchema,
  ExternalEvent,
  externalEventSchema,
  ScheduledBlock,
  ScheduleEvent,
} from "./api";
import {
  agendaItems,
  allDayEventsOn,
  calendarSourceLabel,
  calendarsNeedingAttention,
  capacityLine,
  capacityTotals,
  fitsFreeTime,
  focusItemKey,
  freeSlots,
  itemsOnDay,
  parseMinute,
  syncLabel,
  timeRangeLabel,
  workingHoursLabel,
} from "./calendars";

const stamp = "2026-09-14T12:00:00.000Z";

const calendar: Calendar = {
  id: "7f1f8a8c-1f4e-4d0e-9d57-0f2a3b4c5d6e",
  kind: "ics_link",
  accountId: null,
  name: "Work",
  color: "lake",
  visible: true,
  sourceLabel: "calendar.google.com",
  problem: null,
  lastSyncedAt: "2026-09-17T13:55:00.000Z",
  lastAttemptAt: "2026-09-17T13:55:00.000Z",
  eventCount: 12,
  revision: 1,
  createdAt: stamp,
  updatedAt: stamp,
};

function external(overrides: Partial<ExternalEvent>): ExternalEvent {
  return {
    calendarId: calendar.id,
    key: "a1",
    title: "Standup",
    location: "",
    allDay: false,
    startAtUtc: "2026-09-17T13:00:00.000Z",
    endAtUtc: "2026-09-17T13:30:00.000Z",
    startDate: null,
    endDate: null,
    busy: true,
    tentative: false,
    recurring: true,
    ...overrides,
  };
}

const event = {
  id: "0d7c7c55-0a64-4c5b-9a8e-3a0f0bb0c1aa",
  title: "Dentist",
  startAtUtc: "2026-09-17T13:00:00Z",
  durationMinutes: 60,
} as ScheduleEvent;

const block = {
  block: {
    id: "2b1b8c1a-5f1f-4a8e-8a53-6f9d3a3b1c11",
    startAtUtc: "2026-09-17T12:00:00.000Z",
    durationMinutes: 30,
  },
  task: { title: "Write outline" },
} as ScheduledBlock;

describe("calendar records", () => {
  it("accept timed and all-day events only with the fields they need", () => {
    expect(externalEventSchema.parse(external({})).title).toBe("Standup");
    const allDay = external({
      allDay: true,
      startAtUtc: null,
      endAtUtc: null,
      startDate: "2026-09-18",
      endDate: "2026-09-20",
    });
    expect(externalEventSchema.parse(allDay).allDay).toBe(true);
    expect(() =>
      externalEventSchema.parse({ ...allDay, startDate: null }),
    ).toThrow();
    expect(() =>
      externalEventSchema.parse(external({ startAtUtc: null })),
    ).toThrow();
  });

  it("never carry a calendar's link", () => {
    expect(calendarSchema.parse(calendar).sourceLabel).toBe(
      "calendar.google.com",
    );
    expect(() =>
      calendarSchema.parse({
        ...calendar,
        link: "https://calendar.google.com/private.ics",
      }),
    ).toThrow();
  });
});

describe("agenda items", () => {
  it("merge events, blocks, and calendar events by start", () => {
    const items = agendaItems(
      [event],
      [block],
      [
        external({}),
        external({
          key: "all-day",
          allDay: true,
          startAtUtc: null,
          endAtUtc: null,
          startDate: "2026-09-17",
          endDate: "2026-09-18",
        }),
      ],
      [calendar],
    );
    expect(items.map((item) => item.kind)).toEqual([
      "block",
      "event",
      "external",
    ]);
    expect(items[2].kind === "external" && items[2].calendar?.name).toBe(
      "Work",
    );
  });

  it("place items under their start day, carrying earlier ones onto a week's first day", () => {
    const items = agendaItems(
      [{ ...event, startAtUtc: "2026-09-12T23:30:00Z" } as ScheduleEvent],
      [],
      [external({ startAtUtc: "2026-09-15T14:00:00.000Z" })],
      [calendar],
    );
    const weekStart = new Intl.DateTimeFormat("en-CA").format(
      new Date("2026-09-13T12:00:00Z"),
    );
    expect(itemsOnDay(items, weekStart, weekStart)).toHaveLength(1);
    expect(itemsOnDay(items, "2026-09-15", weekStart)).toHaveLength(1);
  });

  it("focus the item under way, else the next one to start", () => {
    const morning = {
      ...event,
      id: "9d0f4a36-8a0d-4bb0-bf65-3cfb1e2b3a01",
      startAtUtc: "2026-09-14T08:30:00.000Z",
      durationMinutes: 45,
    };
    const lunch = {
      ...event,
      id: "9d0f4a36-8a0d-4bb0-bf65-3cfb1e2b3a02",
      startAtUtc: "2026-09-14T12:00:00.000Z",
    };
    const standup = external({
      startAtUtc: "2026-09-14T09:30:00.000Z",
      endAtUtc: "2026-09-14T09:45:00.000Z",
    });
    const items = agendaItems([lunch, morning], [], [standup], [calendar]);
    const at = (time: string) => focusItemKey(items, new Date(time));
    expect(at("2026-09-14T08:45:00.000Z")).toBe(`event-${morning.id}`);
    expect(at("2026-09-14T09:35:00.000Z")).toBe(
      `external-${calendar.id}-${standup.key}`,
    );
    expect(at("2026-09-14T10:00:00.000Z")).toBe(`event-${lunch.id}`);
    expect(at("2026-09-14T20:00:00.000Z")).toBeNull();
  });

  it("list all-day events on every day they cover", () => {
    const trip = external({
      allDay: true,
      startAtUtc: null,
      endAtUtc: null,
      startDate: "2026-09-18",
      endDate: "2026-09-20",
    });
    expect(allDayEventsOn([trip], "2026-09-17")).toHaveLength(0);
    expect(allDayEventsOn([trip], "2026-09-19")).toHaveLength(1);
    expect(allDayEventsOn([trip], "2026-09-20")).toHaveLength(0);
  });

  it("label time ranges and zero-length events", () => {
    expect(
      timeRangeLabel("2026-09-17T13:00:00.000Z", "2026-09-17T13:00:00.000Z"),
    ).not.toContain("–");
    expect(
      timeRangeLabel("2026-09-17T13:00:00.000Z", "2026-09-17T14:00:00.000Z"),
    ).toContain("–");
  });
});

describe("calendar status", () => {
  it("describes when a calendar last updated", () => {
    const now = new Date("2026-09-17T14:00:00.000Z");
    expect(syncLabel(calendar, now)).toBe("Updated 5 min ago");
    expect(
      syncLabel({ ...calendar, lastSyncedAt: "2026-09-17T13:59:40.000Z" }, now),
    ).toBe("Updated just now");
    expect(
      syncLabel(
        { ...calendar, kind: "ics_file", lastSyncedAt: "2026-09-12T08:00:00Z" },
        now,
      ),
    ).toBe("Imported 12 Sep");
  });

  it("names where a calendar comes from", () => {
    expect(calendarSourceLabel(calendar)).toBe("Link · calendar.google.com");
    expect(
      calendarSourceLabel({
        ...calendar,
        kind: "google",
        accountId: "2b1b8c1a-5f1f-4a8e-8a53-6f9d3a3b1c11",
        sourceLabel: "me@gmail.com",
      }),
    ).toBe("Google · me@gmail.com");
    expect(
      calendarSourceLabel({
        ...calendar,
        kind: "microsoft",
        sourceLabel: "sam@outlook.com",
      }),
    ).toBe("Outlook · sam@outlook.com");
    expect(
      syncLabel(
        { ...calendar, kind: "google" },
        new Date("2026-09-17T14:00:00.000Z"),
      ),
    ).toBe("Updated 5 min ago");
  });

  it("flags only visible calendars whose problem needs the user", () => {
    const broken = {
      ...calendar,
      problem: {
        code: "link_not_found" as const,
        message: "The calendar link no longer works.",
        retryable: false,
      },
    };
    const offline = {
      ...calendar,
      problem: {
        code: "unreachable" as const,
        message: "Delve Planner couldn't reach the calendar service.",
        retryable: true,
      },
    };
    expect(
      calendarsNeedingAttention([
        broken,
        offline,
        { ...broken, visible: false },
      ]),
    ).toEqual([broken]);
  });
});

describe("working hours and capacity", () => {
  it("describe working days as runs", () => {
    expect(
      workingHoursLabel({
        days: [1, 2, 3, 4, 5],
        startMinute: 540,
        endMinute: 1020,
      }),
    ).toBe("Mon–Fri · 09:00–17:00");
    expect(
      workingHoursLabel({ days: [5, 1, 3], startMinute: 480, endMinute: 720 }),
    ).toBe("Mon, Wed, Fri · 08:00–12:00");
    expect(
      workingHoursLabel({
        days: [1, 2, 3, 4, 5, 6, 7],
        startMinute: 0,
        endMinute: 1440,
      }),
    ).toBe("Every day · 00:00–24:00");
  });

  it("reads times, treating midnight as the end of the day", () => {
    expect(parseMinute("09:30")).toBe(570);
    expect(parseMinute("00:00", true)).toBe(1440);
    expect(parseMinute("9:30")).toBeNull();
  });

  const capacity: Capacity = capacitySchema.parse({
    plannedLimitMinutes: null,
    workingHours: {
      days: [1, 2, 3, 4, 5],
      startMinute: 540,
      endMinute: 1020,
      revision: 1,
      updatedAt: stamp,
    },
    days: [
      {
        day: "2026-09-17",
        workingMinutes: 480,
        busyMinutes: 120,
        availableMinutes: 360,
        plannedMinutes: 420,
        unestimatedTasks: 1,
        blockedMinutes: 60,
        free: [
          {
            startAtUtc: "2026-09-17T13:00:00.000Z",
            endAtUtc: "2026-09-17T14:00:00.000Z",
          },
          {
            startAtUtc: "2026-09-17T18:00:00.000Z",
            endAtUtc: "2026-09-17T21:00:00.000Z",
          },
        ],
      },
      {
        day: "2026-09-18",
        workingMinutes: 480,
        busyMinutes: 0,
        availableMinutes: 480,
        plannedMinutes: 60,
        unestimatedTasks: 0,
        blockedMinutes: 0,
        free: [],
      },
    ],
    pooledMinutes: 240,
    pooledUnestimatedTasks: 2,
    calendarsIncomplete: false,
  });

  it("totals a window with or without week-pool work", () => {
    expect(capacityTotals(capacity, { includePool: false })).toMatchObject({
      plannedMinutes: 480,
      availableMinutes: 840,
      unestimatedTasks: 1,
    });
    const week = capacityTotals(capacity, { includePool: true });
    expect(capacityLine(week)).toEqual({
      label: "Planned 12 h, available 14 h",
      over: false,
      overBy: 0,
    });
    const day = capacityTotals(
      { ...capacity, days: [capacity.days[0]] },
      {
        includePool: false,
      },
    );
    expect(capacityLine(day)).toEqual({
      label: "Planned 7 h, available 6 h",
      over: true,
      overBy: 60,
    });
  });

  it("finds free time long enough for a block", () => {
    const [thursday] = capacity.days;
    expect(freeSlots(thursday, 90)).toHaveLength(1);
    expect(fitsFreeTime(thursday, "2026-09-17T18:30:00.000Z", 60)).toBe(true);
    expect(fitsFreeTime(thursday, "2026-09-17T13:30:00.000Z", 60)).toBe(false);
  });
});

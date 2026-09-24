import type {
  Calendar,
  Capacity,
  DayCapacity,
  ExternalEvent,
  ScheduledBlock,
  ScheduleEvent,
  WorkingHours,
} from "./api";
import { dateTimeFields, timeLabel } from "./date";
import { estimateLabel, planColorValues, shortDate } from "./planning";

/**
 * A timed entry on an agenda: a Delve Planner event, a time block, or an event from another
 * calendar.
 */
export type AgendaItem =
  | { kind: "event"; key: string; start: string; event: ScheduleEvent }
  | { kind: "block"; key: string; start: string; scheduled: ScheduledBlock }
  | {
      kind: "external";
      key: string;
      start: string;
      event: ExternalEvent;
      calendar: Calendar | undefined;
    };

const kindOrder: Record<AgendaItem["kind"], number> = {
  event: 0,
  block: 1,
  external: 2,
};

/** Every timed entry in start order; at the same start, Delve Planner's own events come first. */
export function agendaItems(
  events: ScheduleEvent[],
  blocks: ScheduledBlock[],
  external: ExternalEvent[],
  calendars: Calendar[],
): AgendaItem[] {
  const calendarById = new Map(
    calendars.map((calendar) => [calendar.id, calendar]),
  );
  const items: AgendaItem[] = [
    ...events.map((event) => ({
      kind: "event" as const,
      key: `event-${event.id}`,
      start: event.startAtUtc,
      event,
    })),
    ...blocks.map((scheduled) => ({
      kind: "block" as const,
      key: `block-${scheduled.block.id}`,
      start: scheduled.block.startAtUtc,
      scheduled,
    })),
    ...external
      .filter((event) => !event.allDay && event.startAtUtc !== null)
      .map((event) => ({
        kind: "external" as const,
        key: `external-${event.calendarId}-${event.key}`,
        start: event.startAtUtc as string,
        event,
        calendar: calendarById.get(event.calendarId),
      })),
  ];
  return items.sort(
    (left, right) =>
      Date.parse(left.start) - Date.parse(right.start) ||
      kindOrder[left.kind] - kindOrder[right.kind],
  );
}

/** When an agenda item ends; events from other calendars may take no time at all. */
export function agendaItemEnd(item: AgendaItem) {
  switch (item.kind) {
    case "event":
      return Date.parse(item.start) + item.event.durationMinutes * 60_000;
    case "block":
      return (
        Date.parse(item.start) + item.scheduled.block.durationMinutes * 60_000
      );
    case "external":
      return Date.parse(item.event.endAtUtc ?? item.start);
  }
}

/** The item under way at `now`, or else the next one to start, for the highlighted row. */
export function focusItemKey(items: AgendaItem[], now: Date) {
  const time = now.getTime();
  const live = items.find(
    (item) => Date.parse(item.start) <= time && time < agendaItemEnd(item),
  );
  return (
    live?.key ??
    items.find((item) => Date.parse(item.start) > time)?.key ??
    null
  );
}

/** The local day an agenda item starts on. */
export function agendaItemDay(item: AgendaItem) {
  return dateTimeFields(item.start).day;
}

/**
 * The items a week view lists under `day`: those starting that day, plus anything already
 * under way when the week begins.
 */
export function itemsOnDay(
  items: AgendaItem[],
  day: string,
  weekStart: string,
) {
  return items.filter((item) => {
    const start = agendaItemDay(item);
    return start === day || (day === weekStart && start < weekStart);
  });
}

/** All-day events from other calendars that cover `day`. */
export function allDayEventsOn(events: ExternalEvent[], day: string) {
  return events.filter(
    (event) =>
      event.allDay &&
      event.startDate !== null &&
      event.endDate !== null &&
      event.startDate <= day &&
      event.endDate > day,
  );
}

/** "09:00–10:30", or one time for an event with no length. */
export function timeRangeLabel(startAtUtc: string, endAtUtc: string) {
  const start = timeLabel(startAtUtc);
  return startAtUtc === endAtUtc ? start : `${start}–${timeLabel(endAtUtc)}`;
}

export function calendarColor(calendar: Pick<Calendar, "color"> | undefined) {
  return planColorValues[calendar?.color ?? "stone"];
}

const kindLabels: Record<Calendar["kind"], string> = {
  ics_link: "Link",
  ics_file: "File",
  google: "Google",
  microsoft: "Outlook",
};

/** "Link · calendar.google.com", "File · work.ics", or "Google · me@gmail.com". */
export function calendarSourceLabel(calendar: Calendar) {
  return `${kindLabels[calendar.kind]} · ${calendar.sourceLabel}`;
}

/** What an account is called: "Google" or "Outlook". */
export function providerLabel(provider: "google" | "microsoft") {
  return provider === "google" ? "Google" : "Outlook";
}

/** "Updated just now", "Updated 5 min ago", "Updated 3 h ago", or "Updated 12 Sep". */
export function syncLabel(calendar: Calendar, now: Date) {
  const verb = calendar.kind === "ics_file" ? "Imported" : "Updated";
  const at = calendar.lastSyncedAt ?? calendar.updatedAt;
  const minutes = Math.floor((now.getTime() - Date.parse(at)) / 60_000);
  if (minutes < 1) return `${verb} just now`;
  if (minutes < 60) return `${verb} ${minutes} min ago`;
  if (minutes < 24 * 60) return `${verb} ${Math.floor(minutes / 60)} h ago`;
  return `${verb} ${shortDate(dateTimeFields(at).day)}`;
}

/** Calendars that are visible but can't currently be read, which the user may need to fix. */
export function calendarsNeedingAttention(calendars: Calendar[]) {
  return calendars.filter(
    (calendar) =>
      calendar.visible && calendar.problem && !calendar.problem.retryable,
  );
}

export const weekdayNames = [
  "Mon",
  "Tue",
  "Wed",
  "Thu",
  "Fri",
  "Sat",
  "Sun",
] as const;

/** "09:00" for 540 minutes after midnight; 1440 is "24:00". */
export function minuteLabel(minute: number) {
  const hours = Math.floor(minute / 60);
  return `${String(hours).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`;
}

/** Minutes after midnight for an "HH:MM" value, where "00:00" as an end time means midnight. */
export function parseMinute(value: string, asEnd = false) {
  const match = /^(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const minute = Number(match[1]) * 60 + Number(match[2]);
  if (minute >= 24 * 60) return null;
  return asEnd && minute === 0 ? 24 * 60 : minute;
}

/** "Mon–Fri · 09:00–17:00", "Mon, Wed, Fri · 08:00–12:00", or "Every day · 00:00–24:00". */
export function workingHoursLabel(
  hours: Pick<WorkingHours, "days" | "startMinute" | "endMinute">,
) {
  const days = [...hours.days].sort((left, right) => left - right);
  let dayLabel: string;
  if (days.length === 7) dayLabel = "Every day";
  else {
    const runs: number[][] = [];
    for (const day of days) {
      const run = runs[runs.length - 1];
      if (run && run[run.length - 1] === day - 1) run.push(day);
      else runs.push([day]);
    }
    dayLabel = runs
      .map((run) =>
        run.length >= 3
          ? `${weekdayNames[run[0] - 1]}–${weekdayNames[run[run.length - 1] - 1]}`
          : run.map((day) => weekdayNames[day - 1]).join(", "),
      )
      .join(", ");
  }
  return `${dayLabel} · ${minuteLabel(hours.startMinute)}–${minuteLabel(hours.endMinute)}`;
}

/** "11 h", "3 h 30 min", or "0 h". */
export function hoursLabel(minutes: number) {
  return minutes === 0 ? "0 h" : estimateLabel(minutes);
}

export type CapacityTotals = {
  plannedMinutes: number;
  availableMinutes: number;
  workingMinutes: number;
  busyMinutes: number;
  unestimatedTasks: number;
};

/** One day's totals, or a whole window's including week-pool work with no day. */
export function capacityTotals(
  capacity: Capacity,
  { includePool }: { includePool: boolean },
): CapacityTotals {
  const totals = capacity.days.reduce<CapacityTotals>(
    (sum, day) => ({
      plannedMinutes: sum.plannedMinutes + day.plannedMinutes,
      availableMinutes: sum.availableMinutes + day.availableMinutes,
      workingMinutes: sum.workingMinutes + day.workingMinutes,
      busyMinutes: sum.busyMinutes + day.busyMinutes,
      unestimatedTasks: sum.unestimatedTasks + day.unestimatedTasks,
    }),
    {
      plannedMinutes: 0,
      availableMinutes: 0,
      workingMinutes: 0,
      busyMinutes: 0,
      unestimatedTasks: 0,
    },
  );
  if (!includePool) return totals;
  return {
    ...totals,
    plannedMinutes: totals.plannedMinutes + capacity.pooledMinutes,
    unestimatedTasks: totals.unestimatedTasks + capacity.pooledUnestimatedTasks,
  };
}

/** "Planned 17 h, available 11 h", and whether planned work exceeds the time available. */
export function capacityLine(totals: CapacityTotals) {
  return {
    label: `Planned ${hoursLabel(totals.plannedMinutes)}, available ${hoursLabel(totals.availableMinutes)}`,
    over: totals.plannedMinutes > totals.availableMinutes,
    overBy: Math.max(0, totals.plannedMinutes - totals.availableMinutes),
  };
}

/** Free ranges on a day long enough for `minutes` of work. */
export function freeSlots(day: DayCapacity, minutes: number) {
  return day.free.filter(
    (range) =>
      Date.parse(range.endAtUtc) - Date.parse(range.startAtUtc) >=
      minutes * 60_000,
  );
}

/** Whether `minutes` starting at `startAtUtc` fit inside one free range. */
export function fitsFreeTime(
  day: DayCapacity,
  startAtUtc: string,
  minutes: number,
) {
  const start = Date.parse(startAtUtc);
  const end = start + minutes * 60_000;
  return day.free.some(
    (range) =>
      Date.parse(range.startAtUtc) <= start &&
      Date.parse(range.endAtUtc) >= end,
  );
}

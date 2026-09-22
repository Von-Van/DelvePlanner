import { addDays, format, parseISO, startOfToday, startOfWeek } from "date-fns";

export const localTimeZone =
  Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

export type WeekStart = 0 | 1 | 2 | 3 | 4 | 5 | 6;

/** The locale's first day of the week (0 = Sunday), falling back to Monday. */
export function localeWeekStart(locale = navigator.language): WeekStart {
  try {
    const info = new Intl.Locale(locale) as Intl.Locale & {
      weekInfo?: { firstDay: number };
      getWeekInfo?: () => { firstDay: number };
    };
    const firstDay = (info.getWeekInfo?.() ?? info.weekInfo)?.firstDay;
    if (firstDay !== undefined) return (firstDay % 7) as WeekStart;
  } catch {
    // Older WebKit builds do not expose week information.
  }
  return 1;
}

/** The week start every view uses, read once from the locale. */
export const weekStartsOn = localeWeekStart();

export function weekStartDay(day: string, weekStartsOn: WeekStart) {
  return isoDay(startOfWeek(parseISO(day), { weekStartsOn }));
}

export function rangeLabel(startDay: string, endDay: string) {
  const start = parseISO(startDay);
  const end = parseISO(endDay);
  if (start.getFullYear() !== end.getFullYear())
    return `${format(start, "MMM d, yyyy")} – ${format(end, "MMM d, yyyy")}`;
  if (start.getMonth() !== end.getMonth())
    return `${format(start, "MMM d")} – ${format(end, "MMM d, yyyy")}`;
  return `${format(start, "MMMM d")} – ${format(end, "d, yyyy")}`;
}

export function isoDay(date: Date) {
  return format(date, "yyyy-MM-dd");
}

export function dayLabel(day: string) {
  return format(parseISO(day), "EEEE, MMMM d");
}

/** "Mon 14 Sep": the compact date under the sidebar's local agenda label. */
export function dayMonthShort(day: string) {
  return format(parseISO(day), "EEE d MMM");
}

export function weekdayShort(day: string) {
  return format(parseISO(day), "EEE");
}

export function weekdayName(day: string) {
  return format(parseISO(day), "EEEE");
}

export function todayDay() {
  return isoDay(startOfToday());
}

export function offsetDay(day: string, amount: number) {
  return isoDay(addDays(parseISO(day), amount));
}

/** A 24-hour clock time such as "08:30" in `timeZone`. */
export function timeLabel(startAtUtc: string, timeZone = localTimeZone) {
  return new Intl.DateTimeFormat("en-GB", {
    timeZone,
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(startAtUtc));
}

export function dateTimeFields(startAtUtc: string, timeZone = localTimeZone) {
  const date = new Date(startAtUtc);
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(date);
  const value = (name: string) =>
    parts.find((part) => part.type === name)?.value ?? "";
  return {
    day: `${value("year")}-${value("month")}-${value("day")}`,
    time: `${value("hour")}:${value("minute")}`,
  };
}

export function localDateTimeToUtc(day: string, time: string) {
  return new Date(`${day}T${time}:00`).toISOString();
}

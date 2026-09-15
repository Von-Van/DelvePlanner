import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";
import { api, LinkChange, ReminderChange, ScheduleEvent } from "./api";
import { dateTimeFields, localTimeZone } from "./date";

export type EventDraft = {
  title: string;
  notes: string;
  day: string;
  time: string;
  durationMinutes: number;
  reminderMinutesBefore: number | null;
  planId: string | null;
  location: string;
  workstreamId: string | null;
  ownerId: string | null;
};

export const reminderPresets = [
  { value: "", label: "No reminder" },
  { value: "0", label: "At start time" },
  { value: "5", label: "5 minutes before" },
  { value: "10", label: "10 minutes before" },
  { value: "15", label: "15 minutes before" },
  { value: "30", label: "30 minutes before" },
  { value: "60", label: "1 hour before" },
  { value: "1440", label: "1 day before" },
];

export function draftFor(
  day: string,
  event?: ScheduleEvent,
  planId: string | null = null,
): EventDraft {
  if (!event)
    return {
      title: "",
      notes: "",
      day,
      time: "09:00",
      durationMinutes: 60,
      reminderMinutesBefore: null,
      planId,
      location: "",
      workstreamId: null,
      ownerId: null,
    };
  const fields = dateTimeFields(event.startAtUtc);
  return {
    title: event.title,
    notes: event.notes,
    day: fields.day,
    time: fields.time,
    durationMinutes: event.durationMinutes,
    reminderMinutesBefore: event.reminderMinutesBefore,
    planId: event.planId,
    location: event.location,
    workstreamId: event.workstreamId,
    ownerId: event.ownerId,
  };
}

/** Creates or updates an event from a resolved editor draft. */
export async function saveEventDraft(
  draft: EventDraft,
  event: ScheduleEvent | undefined,
  startAtUtc: string,
) {
  if (draft.reminderMinutesBefore !== null)
    await ensureNotificationPermission();
  if (!event) {
    await api.createEvent({
      title: draft.title,
      notes: draft.notes,
      startAtUtc,
      timeZone: localTimeZone,
      durationMinutes: draft.durationMinutes,
      reminderMinutesBefore: draft.reminderMinutesBefore,
      planId: draft.planId,
      location: draft.location,
      workstreamId: draft.workstreamId,
      ownerId: draft.ownerId,
    });
    return;
  }
  await api.updateEvent({
    id: event.id,
    revision: event.revision,
    title: draft.title,
    notes: draft.notes,
    startAtUtc,
    timeZone: localTimeZone,
    durationMinutes: draft.durationMinutes,
    location: draft.location,
    reminderChange: reminderChangeFor(
      event.reminderMinutesBefore,
      draft.reminderMinutesBefore,
    ),
    planChange: linkChangeFor(event.planId, draft.planId),
    workstreamChange: linkChangeFor(event.workstreamId, draft.workstreamId),
    ownerChange: linkChangeFor(event.ownerId, draft.ownerId),
  });
}

export function reminderChangeFor(
  current: number | null,
  next: number | null,
): ReminderChange {
  if (current === next) return { action: "unchanged" };
  return next === null
    ? { action: "clear" }
    : { action: "set", minutesBefore: next };
}

export function linkChangeFor(
  current: string | null,
  next: string | null,
): LinkChange {
  if (current === next) return { action: "unchanged" };
  return next === null ? { action: "clear" } : { action: "set", id: next };
}

export async function ensureNotificationPermission() {
  if (await isPermissionGranted()) return;
  const permission = await requestPermission();
  if (permission !== "granted") {
    throw new Error(
      "Notification permission was not granted. No schedule changes were applied.",
    );
  }
}

/** "10 min before" or "At start", for compact reminder lines. */
export function reminderShortLabel(minutes: number) {
  if (minutes === 0) return "At start";
  if (minutes % 1_440 === 0) return `${minutes / 1_440} day before`;
  if (minutes % 60 === 0) return `${minutes / 60} hr before`;
  return `${minutes} min before`;
}

export function reminderLabel(minutes: number) {
  if (minutes === 0) return "reminder at start";
  if (minutes === 1_440) return "reminder 1 day before";
  if (minutes === 60) return "reminder 1 hour before";
  return `reminder ${minutes} minutes before`;
}

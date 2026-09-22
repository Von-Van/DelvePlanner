import type {
  DayChange,
  EstimateChange,
  Proposal,
  ProposalOperation,
  ProposalReference,
  RecordReference,
  ReminderChange,
} from "./api";
import {
  dateTimeFields,
  dayMonthShort,
  localTimeZone,
  timeLabel,
  weekStartDay,
  weekStartsOn,
} from "./date";
import { reminderLabel } from "./events";
import {
  estimateLabel,
  milestoneStatusLabels,
  planStatusLabels,
  shortDate,
  taskPriorityLabels,
  taskStatusLabels,
} from "./planning";

export type OperationTone = "create" | "change" | "move" | "delete";

export type OperationPreview = {
  tone: OperationTone;
  /** What happens, naming the record: “Add task “Book venue””. */
  title: string;
  /** The details that change, each short enough to scan. */
  details: string[];
};

/** A preview with what the user's accept or reject needs: its handle and what it depends on. */
export type ReviewedOperation = OperationPreview & {
  id: string;
  /** Suggestions this one can't be applied without, such as the plan it would join. */
  dependsOn: string[];
  /** Why the planner chose this, when the request didn't make it obvious. */
  reason: string | null;
  /** Whether its day or week is the planner's own idea rather than one the request gave. */
  suggested: boolean;
};

/** Readable previews for every operation, using the proposal's own titles for records. */
export function describeProposal(
  proposal: Pick<Proposal, "operations" | "references">,
  timeZone = localTimeZone,
): ReviewedOperation[] {
  const titles = new Map(
    proposal.references.map((reference: ProposalReference) => [
      reference.id,
      reference.title,
    ]),
  );
  return proposal.operations.map((operation) => ({
    id: operation.id,
    dependsOn: operation.dependsOn,
    reason: operation.reason,
    suggested: operation.suggested,
    ...describeOperation(operation.change, titles, timeZone),
  }));
}

/**
 * The suggestions left once `rejected` are dropped. Rejecting a plan or milestone that other
 * suggestions are built on rejects those too, so a task is never applied without the plan it
 * was going to live in.
 */
export function acceptedOperations(
  previews: ReviewedOperation[],
  rejected: ReadonlySet<string>,
): string[] {
  const dropped = new Set(rejected);
  // A dependency chain is at most proposal-length, so one pass per suggestion settles it.
  for (let pass = 0; pass < previews.length; pass += 1) {
    let changed = false;
    for (const preview of previews) {
      if (dropped.has(preview.id)) continue;
      if (preview.dependsOn.some((needed) => dropped.has(needed))) {
        dropped.add(preview.id);
        changed = true;
      }
    }
    if (!changed) break;
  }
  return previews
    .filter((preview) => !dropped.has(preview.id))
    .map((preview) => preview.id);
}

export function describeOperation(
  operation: ProposalOperation["change"],
  titles: Map<string, string>,
  timeZone = localTimeZone,
): OperationPreview {
  const named = (id: string, fallback: string) =>
    `“${titles.get(id) ?? fallback}”`;
  const planName = (reference: RecordReference) =>
    "id" in reference
      ? named(reference.id, "the selected plan")
      : `“${reference.newTitle}” (new)`;
  const milestoneName = (reference: RecordReference) =>
    "id" in reference
      ? named(reference.id, "the selected milestone")
      : `“${reference.newTitle}” (new)`;
  const when = (startAtUtc: string) => dateTimeLabel(startAtUtc, timeZone);
  switch (operation.type) {
    case "create_event":
      return {
        tone: "create",
        title: `Create “${operation.title}”`,
        details: compact([
          when(operation.startAtUtc),
          `${operation.durationMinutes} min`,
          operation.reminderMinutesBefore !== null &&
            capitalize(reminderLabel(operation.reminderMinutesBefore)),
          operation.plan && `In ${planName(operation.plan)}`,
        ]),
      };
    case "update_event":
      return {
        tone: "change",
        title: `Update ${named(operation.eventId, "the selected event")}`,
        details: eventDetails(operation),
      };
    case "reschedule_event":
      return {
        tone: "move",
        title: `Move ${named(operation.eventId, "the selected event")}`,
        details: compact([
          `To ${when(operation.startAtUtc)}`,
          ...eventDetails(operation),
        ]),
      };
    case "delete_event":
      return {
        tone: "delete",
        title: `Delete ${named(operation.eventId, "the selected event")}`,
        details: [],
      };
    case "set_event_plan":
      return operation.plan
        ? {
            tone: "move",
            title: `Add ${named(operation.eventId, "the selected event")} to ${planName(operation.plan)}`,
            details: [],
          }
        : {
            tone: "move",
            title: `Take ${named(operation.eventId, "the selected event")} out of its plan`,
            details: [],
          };
    case "create_plan":
      return {
        tone: "create",
        title: `Create plan “${operation.title}”`,
        details: compact([
          planStatusLabels[operation.status],
          operation.startDate && `Starts ${shortDate(operation.startDate)}`,
          operation.targetDate && `Target ${shortDate(operation.targetDate)}`,
          operation.description && "With a description",
        ]),
      };
    case "update_plan":
      return {
        tone: "change",
        title: `Update plan ${named(operation.planId, "the selected plan")}`,
        details: compact([
          operation.title && `Rename to “${operation.title}”`,
          operation.status && `Status: ${planStatusLabels[operation.status]}`,
          operation.startDate && `Starts ${shortDate(operation.startDate)}`,
          operation.targetDate && `Target ${shortDate(operation.targetDate)}`,
          operation.description !== null && "New description",
        ]),
      };
    case "create_milestone":
      return {
        tone: "create",
        title: `Add milestone “${operation.title}”`,
        details: compact([
          `In ${planName(operation.plan)}`,
          operation.targetDate && `Target ${shortDate(operation.targetDate)}`,
        ]),
      };
    case "update_milestone":
      return {
        tone: "change",
        title: `Update milestone ${named(operation.milestoneId, "the selected milestone")}`,
        details: compact([
          operation.title && `Rename to “${operation.title}”`,
          operation.targetDate && `Target ${shortDate(operation.targetDate)}`,
          operation.status &&
            `Status: ${milestoneStatusLabels[operation.status]}`,
          operation.description !== null && "New description",
        ]),
      };
    case "delete_milestone":
      return {
        tone: "delete",
        title: `Delete milestone ${named(operation.milestoneId, "the selected milestone")}`,
        details: ["Its tasks stay in the plan"],
      };
    case "create_task":
      return {
        tone: "create",
        title: `Add task “${operation.title}”`,
        details: compact([
          operation.plan && `In ${planName(operation.plan)}`,
          operation.milestone &&
            `Milestone ${milestoneName(operation.milestone)}`,
          operation.scheduledDay &&
            `Do on ${shortDate(operation.scheduledDay)}`,
          operation.dueDate && `Due ${shortDate(operation.dueDate)}`,
          operation.status !== "todo" && taskStatusLabels[operation.status],
          operation.priority !== "normal" &&
            `${taskPriorityLabels[operation.priority]} priority`,
        ]),
      };
    case "update_task":
      return {
        tone: "change",
        title:
          operation.status === "done" &&
          operation.title === null &&
          operation.priority === null &&
          operation.description === null
            ? `Mark ${named(operation.taskId, "the selected task")} done`
            : `Update task ${named(operation.taskId, "the selected task")}`,
        details: compact([
          operation.title && `Rename to “${operation.title}”`,
          operation.status &&
            operation.status !== "done" &&
            `Status: ${taskStatusLabels[operation.status]}`,
          operation.status === "done" &&
            (operation.title !== null ||
              operation.priority !== null ||
              operation.description !== null) &&
            "Status: Done",
          operation.priority &&
            `${taskPriorityLabels[operation.priority]} priority`,
          operation.description !== null && "New description",
          estimateChangeLabel(operation.estimate),
        ]),
      };
    case "schedule_task":
      return {
        tone: "move",
        title: `Reschedule ${named(operation.taskId, "the selected task")}`,
        details: compact([
          dayChangeLabel(operation.scheduledDay, "Do on", "No scheduled day"),
          dayChangeLabel(operation.dueDate, "Due", "No due date"),
          weekChangeLabel(operation.plannedWeek),
        ]),
      };
    case "set_task_plan":
      return {
        tone: "move",
        title: operation.plan
          ? `Move ${named(operation.taskId, "the selected task")} to ${planName(operation.plan)}`
          : `Take ${named(operation.taskId, "the selected task")} out of its plan`,
        details: compact([
          operation.milestone &&
            `Milestone ${milestoneName(operation.milestone)}`,
        ]),
      };
    case "delete_task":
      return {
        tone: "delete",
        title: `Delete task ${named(operation.taskId, "the selected task")}`,
        details: [],
      };
  }
}

export function proposalEnablesReminder(
  proposal: Pick<Proposal, "operations">,
) {
  return proposal.operations.some(({ change }) =>
    change.type === "create_event"
      ? change.reminderMinutesBefore !== null
      : (change.type === "update_event" ||
          change.type === "reschedule_event") &&
        change.reminderChange.action === "set",
  );
}

function eventDetails(operation: {
  title: string | null;
  notes: string | null;
  durationMinutes: number | null;
  reminderChange: ReminderChange;
}) {
  return compact([
    operation.title && `Rename to “${operation.title}”`,
    operation.notes !== null &&
      (operation.notes ? "New notes" : "Clear the notes"),
    operation.durationMinutes !== null && `${operation.durationMinutes} min`,
    operation.reminderChange.action === "set" &&
      capitalize(reminderLabel(operation.reminderChange.minutesBefore)),
    operation.reminderChange.action === "clear" && "Clear the reminder",
  ]);
}

function dayChangeLabel(
  change: DayChange,
  setLabel: string,
  clearLabel: string,
) {
  if (change.action === "set") return `${setLabel} ${shortDate(change.day)}`;
  if (change.action === "clear") return clearLabel;
  return null;
}

/** "Takes 1 h 30 m" or "No estimate", and nothing at all when the estimate is untouched. */
function estimateChangeLabel(change: EstimateChange) {
  if (change.action === "set") return `Takes ${estimateLabel(change.minutes)}`;
  if (change.action === "clear") return "No estimate";
  return null;
}

/** A week is stored as a day inside it, so it reads as the week that day belongs to. */
function weekChangeLabel(change: DayChange) {
  if (change.action === "set")
    return `Chosen for the week of ${shortDate(weekStartDay(change.day, weekStartsOn))}`;
  if (change.action === "clear") return "Not chosen for a week";
  return null;
}

/** "Fri 18 Sep, 15:00" in the event's own time zone. */
function dateTimeLabel(startAtUtc: string, timeZone: string) {
  const { day } = dateTimeFields(startAtUtc, timeZone);
  return `${dayMonthShort(day)}, ${timeLabel(startAtUtc, timeZone)}`;
}

function capitalize(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function compact(values: Array<string | false | null | undefined | 0>) {
  return values.filter((value): value is string => Boolean(value));
}

import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

export const planStatuses = [
  "planning",
  "active",
  "on_hold",
  "complete",
  "cancelled",
] as const;
export const planColors = [
  "sage",
  "clay",
  "ochre",
  "lake",
  "plum",
  "stone",
] as const;
export const milestoneStatuses = ["pending", "complete", "skipped"] as const;
export const taskStatuses = ["todo", "in_progress", "blocked", "done"] as const;
export const taskPriorities = ["low", "normal", "high", "critical"] as const;

const daySchema = z.string().regex(/^\d{4}-\d{2}-\d{2}$/);
const timestampSchema = z.string().datetime({ offset: true });
const revisionSchema = z.number().int().positive();

export const eventSchema = z
  .object({
    id: z.string().uuid(),
    title: z.string(),
    notes: z.string(),
    startAtUtc: timestampSchema,
    timeZone: z.string(),
    durationMinutes: z.number().int().min(5).max(1440),
    reminderMinutesBefore: z.number().int().min(0).max(10_080).nullable(),
    reminderStatus: z.enum([
      "none",
      "pending",
      "scheduled",
      "needs_permission",
      "error",
      "expired",
    ]),
    planId: z.string().uuid().nullable(),
    location: z.string(),
    workstreamId: z.string().uuid().nullable(),
    ownerId: z.string().uuid().nullable(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export const personSchema = z
  .object({
    id: z.string().uuid(),
    displayName: z.string(),
    role: z.string(),
    email: z.string().nullable(),
    notes: z.string(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export const workstreamSchema = z
  .object({
    id: z.string().uuid(),
    planId: z.string().uuid(),
    name: z.string(),
    description: z.string(),
    sortOrder: z.number().int().nonnegative(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

/** A page that belongs with a plan; opened in the browser, never sent to the planner. */
export const planLinkSchema = z
  .object({ title: z.string(), url: z.string().url() })
  .strict();

export const planSchema = z
  .object({
    id: z.string().uuid(),
    title: z.string(),
    description: z.string(),
    status: z.enum(planStatuses),
    startDate: daySchema.nullable(),
    targetDate: daySchema.nullable(),
    color: z.enum(planColors).nullable(),
    archived: z.boolean(),
    links: z.array(planLinkSchema),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export const milestoneSchema = z
  .object({
    id: z.string().uuid(),
    planId: z.string().uuid(),
    title: z.string(),
    description: z.string(),
    targetDate: daySchema.nullable(),
    status: z.enum(milestoneStatuses),
    workstreamId: z.string().uuid().nullable(),
    sortOrder: z.number().int().nonnegative(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export const reminderChangeSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("unchanged") }).strict(),
  z.object({ action: z.literal("clear") }).strict(),
  z
    .object({
      action: z.literal("set"),
      minutesBefore: z.number().int().min(0).max(10_080),
    })
    .strict(),
]);

export const linkChangeSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("unchanged") }).strict(),
  z.object({ action: z.literal("clear") }).strict(),
  z.object({ action: z.literal("set"), id: z.string().uuid() }).strict(),
]);

export const recurrenceFrequencies = [
  "daily",
  "weekdays",
  "weekly",
  "monthly",
] as const;

/** How a task repeats. Only the open occurrence carries it; finishing it creates the next. */
export const recurrenceSchema = z
  .object({
    frequency: z.enum(recurrenceFrequencies),
    interval: z.number().int().min(1).max(99),
    weekdays: z.array(z.number().int().min(1).max(7)),
    monthDay: z.number().int().min(1).max(31).nullable(),
  })
  .strict();

export const checklistItemSchema = z
  .object({ text: z.string(), done: z.boolean() })
  .strict();

export const taskSchema = z
  .object({
    id: z.string().uuid(),
    title: z.string(),
    description: z.string(),
    planId: z.string().uuid().nullable(),
    milestoneId: z.string().uuid().nullable(),
    workstreamId: z.string().uuid().nullable(),
    ownerId: z.string().uuid().nullable(),
    dueDate: daySchema.nullable(),
    scheduledDay: daySchema.nullable(),
    plannedWeek: daySchema.nullable(),
    estimatedMinutes: z.number().int().min(1).max(1440).nullable(),
    status: z.enum(taskStatuses),
    priority: z.enum(taskPriorities),
    completedAt: timestampSchema.nullable(),
    recurrence: recurrenceSchema.nullable(),
    checklist: z.array(checklistItemSchema),
    /** Tasks this one waits on: a hint, never enforced. */
    waitingOn: z.array(z.string().uuid()),
    sortOrder: z.number().int().nonnegative(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

/** An open task in a word, for choosing and showing what a task waits on. */
export const taskReferenceSchema = z
  .object({
    id: z.string().uuid(),
    title: z.string(),
    planId: z.string().uuid().nullable(),
    scheduledDay: daySchema.nullable(),
    dueDate: daySchema.nullable(),
  })
  .strict();

export const inboxItemSchema = z
  .object({
    id: z.string().uuid(),
    text: z.string(),
    notes: z.string(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export const taskBlockSchema = z
  .object({
    id: z.string().uuid(),
    taskId: z.string().uuid(),
    startAtUtc: timestampSchema,
    timeZone: z.string(),
    durationMinutes: z.number().int().min(5).max(1440),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

const scheduledBlockSchema = z
  .object({ block: taskBlockSchema, task: taskSchema })
  .strict();

export const calendarKinds = [
  "ics_link",
  "ics_file",
  "google",
  "microsoft",
] as const;
export const calendarProviders = ["google", "microsoft"] as const;
export const calendarProblems = [
  "not_a_calendar",
  "sign_in_expired",
  "too_large",
  "link_not_found",
  "link_refused",
  "unreachable",
  "rate_limited",
  "server_error",
  "link_unavailable",
] as const;

const calendarIssueSchema = z
  .object({
    code: z.enum(calendarProblems),
    message: z.string(),
    retryable: z.boolean(),
  })
  .strict();

/** A connected account. Its tokens stay in Rust and the system keychain. */
export const calendarAccountSchema = z
  .object({
    id: z.string().uuid(),
    provider: z.enum(calendarProviders),
    label: z.string(),
    problem: calendarIssueSchema.nullable(),
    calendarCount: z.number().int().nonnegative(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

/** A calendar an account offers, before Delve Planner starts showing it. */
export const remoteCalendarSchema = z
  .object({
    id: z.string().min(1),
    name: z.string(),
    primary: z.boolean(),
    alreadyAdded: z.boolean(),
  })
  .strict();

const connectedAccountSchema = z
  .object({
    account: calendarAccountSchema,
    calendars: z.array(remoteCalendarSchema),
  })
  .strict();

export const calendarSchema = z
  .object({
    id: z.string().uuid(),
    kind: z.enum(calendarKinds),
    accountId: z.string().uuid().nullable(),
    name: z.string(),
    color: z.enum(planColors),
    visible: z.boolean(),
    sourceLabel: z.string(),
    problem: calendarIssueSchema.nullable(),
    lastSyncedAt: timestampSchema.nullable(),
    lastAttemptAt: timestampSchema.nullable(),
    eventCount: z.number().int().nonnegative(),
    revision: revisionSchema,
    createdAt: timestampSchema,
    updatedAt: timestampSchema,
  })
  .strict();

/** One occurrence from a read-only calendar: UTC instants when timed, local dates when all-day. */
export const externalEventSchema = z
  .object({
    calendarId: z.string().uuid(),
    key: z.string().min(1),
    title: z.string(),
    location: z.string(),
    allDay: z.boolean(),
    startAtUtc: timestampSchema.nullable(),
    endAtUtc: timestampSchema.nullable(),
    startDate: daySchema.nullable(),
    endDate: daySchema.nullable(),
    busy: z.boolean(),
    tentative: z.boolean(),
    recurring: z.boolean(),
  })
  .strict()
  .refine(
    (event) =>
      event.allDay
        ? event.startDate !== null && event.endDate !== null
        : event.startAtUtc !== null && event.endAtUtc !== null,
    "An external event needs dates when all-day and times otherwise.",
  );

const calendarAgendaSchema = z
  .object({
    calendars: z.array(calendarSchema),
    events: z.array(externalEventSchema),
  })
  .strict();

export const workingHoursSchema = z
  .object({
    days: z.array(z.number().int().min(1).max(7)),
    startMinute: z.number().int().min(0).max(1439),
    endMinute: z.number().int().min(1).max(1440),
    revision: revisionSchema,
    updatedAt: timestampSchema,
  })
  .strict();

const minutesSchema = z.number().int().nonnegative();
const countSchema = z.number().int().nonnegative();

export const capacitySchema = z
  .object({
    workingHours: workingHoursSchema,
    /** The most work the user wants planned into a day, from their planning profile. */
    plannedLimitMinutes: z.number().int().positive().nullable(),
    days: z.array(
      z
        .object({
          day: daySchema,
          workingMinutes: minutesSchema,
          busyMinutes: minutesSchema,
          availableMinutes: minutesSchema,
          plannedMinutes: minutesSchema,
          unestimatedTasks: countSchema,
          blockedMinutes: minutesSchema,
          free: z.array(
            z
              .object({
                startAtUtc: timestampSchema,
                endAtUtc: timestampSchema,
              })
              .strict(),
          ),
        })
        .strict(),
    ),
    pooledMinutes: minutesSchema,
    pooledUnestimatedTasks: countSchema,
    calendarsIncomplete: z.boolean(),
  })
  .strict();

const inboxConversionSchema = z
  .object({
    kind: z.enum(["task", "plan", "event"]),
    id: z.string().uuid(),
  })
  .strict();

const planSummarySchema = z
  .object({
    plan: planSchema,
    milestoneCount: z.number().int().nonnegative(),
    taskCount: z.number().int().nonnegative(),
    completedTaskCount: z.number().int().nonnegative(),
    overdueTaskCount: z.number().int().nonnegative(),
    nextMilestone: milestoneSchema.nullable(),
  })
  .strict();

const planWorkspaceSchema = z
  .object({
    plan: planSchema,
    workstreams: z.array(workstreamSchema),
    milestones: z.array(milestoneSchema),
    tasks: z.array(taskSchema),
    events: z.array(eventSchema),
  })
  .strict();

const personSummarySchema = z
  .object({
    person: personSchema,
    openTaskCount: z.number().int().nonnegative(),
    openTasks: z.array(taskSchema),
    upcomingEvents: z.array(eventSchema),
  })
  .strict();

const planDeletionSchema = z
  .object({
    deletedWorkstreams: z.number().int().nonnegative(),
    deletedMilestones: z.number().int().nonnegative(),
    deletedTasks: z.number().int().nonnegative(),
    detachedTasks: z.number().int().nonnegative(),
    detachedEvents: z.number().int().nonnegative(),
  })
  .strict();

const recordReferenceSchema = z.union([
  z.object({ id: z.string().uuid() }).strict(),
  z.object({ newTitle: z.string().min(1).max(140) }).strict(),
]);

/** The inbox item a new record is made from; applying removes it from the inbox. */
const inboxSourceSchema = z
  .object({ itemId: z.string().uuid(), expectedRevision: revisionSchema })
  .strict();

const dayChangeSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("unchanged") }).strict(),
  z.object({ action: z.literal("clear") }).strict(),
  z.object({ action: z.literal("set"), day: daySchema }).strict(),
]);

const estimateChangeSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("unchanged") }).strict(),
  z.object({ action: z.literal("clear") }).strict(),
  z
    .object({
      action: z.literal("set"),
      minutes: z.number().int().min(1).max(1440),
    })
    .strict(),
]);

const titleSchema = z.string().min(1).max(140);
const descriptionSchema = z.string().max(2000);
const targetSchema = {
  expectedRevision: revisionSchema,
};

export const operationSchema = z.discriminatedUnion("type", [
  z
    .object({
      type: z.literal("create_event"),
      title: titleSchema,
      notes: z.string(),
      startAtUtc: timestampSchema,
      timeZone: z.string(),
      durationMinutes: z.number().int().min(5).max(1440),
      reminderMinutesBefore: z.number().int().min(0).max(10_080).nullable(),
      plan: recordReferenceSchema.nullable(),
      fromInbox: inboxSourceSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("update_event"),
      eventId: z.string().uuid(),
      ...targetSchema,
      title: z.string().nullable(),
      notes: z.string().nullable(),
      durationMinutes: z.number().int().min(5).max(1440).nullable(),
      reminderChange: reminderChangeSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("delete_event"),
      eventId: z.string().uuid(),
      ...targetSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("reschedule_event"),
      eventId: z.string().uuid(),
      ...targetSchema,
      title: titleSchema.nullable(),
      notes: z.string().max(800).nullable(),
      startAtUtc: timestampSchema,
      timeZone: z.string(),
      durationMinutes: z.number().int().min(5).max(1440).nullable(),
      reminderChange: reminderChangeSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("set_event_plan"),
      eventId: z.string().uuid(),
      ...targetSchema,
      plan: recordReferenceSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("create_plan"),
      title: titleSchema,
      description: descriptionSchema,
      status: z.enum(planStatuses),
      startDate: daySchema.nullable(),
      targetDate: daySchema.nullable(),
      fromInbox: inboxSourceSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("update_plan"),
      planId: z.string().uuid(),
      ...targetSchema,
      title: titleSchema.nullable(),
      description: descriptionSchema.nullable(),
      status: z.enum(planStatuses).nullable(),
      startDate: daySchema.nullable(),
      targetDate: daySchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("create_milestone"),
      plan: recordReferenceSchema,
      title: titleSchema,
      description: descriptionSchema,
      targetDate: daySchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("update_milestone"),
      milestoneId: z.string().uuid(),
      ...targetSchema,
      title: titleSchema.nullable(),
      description: descriptionSchema.nullable(),
      targetDate: daySchema.nullable(),
      status: z.enum(milestoneStatuses).nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("delete_milestone"),
      milestoneId: z.string().uuid(),
      ...targetSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("create_task"),
      title: titleSchema,
      description: descriptionSchema,
      plan: recordReferenceSchema.nullable(),
      milestone: recordReferenceSchema.nullable(),
      scheduledDay: daySchema.nullable(),
      dueDate: daySchema.nullable(),
      status: z.enum(taskStatuses),
      priority: z.enum(taskPriorities),
      workstream: recordReferenceSchema.nullable(),
      fromInbox: inboxSourceSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("update_task"),
      taskId: z.string().uuid(),
      ...targetSchema,
      title: titleSchema.nullable(),
      description: descriptionSchema.nullable(),
      status: z.enum(taskStatuses).nullable(),
      priority: z.enum(taskPriorities).nullable(),
      estimate: estimateChangeSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("schedule_task"),
      taskId: z.string().uuid(),
      ...targetSchema,
      scheduledDay: dayChangeSchema,
      dueDate: dayChangeSchema,
      plannedWeek: dayChangeSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("set_task_plan"),
      taskId: z.string().uuid(),
      ...targetSchema,
      plan: recordReferenceSchema.nullable(),
      milestone: recordReferenceSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("delete_task"),
      taskId: z.string().uuid(),
      ...targetSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("create_workstream"),
      plan: recordReferenceSchema,
      name: z.string().min(1).max(140),
    })
    .strict(),
  z
    .object({
      type: z.literal("set_task_workstream"),
      taskId: z.string().uuid(),
      ...targetSchema,
      workstream: recordReferenceSchema.nullable(),
    })
    .strict(),
]);

const proposalReferenceSchema = z
  .object({
    id: z.string().uuid(),
    kind: z.enum([
      "event",
      "plan",
      "milestone",
      "task",
      "workstream",
      "inbox_item",
    ]),
    title: z.string(),
  })
  .strict();

/** One suggestion as the user reviews it: its handle, what it needs, and the change itself. */
const proposedOperationSchema = z
  .object({
    id: z.string().min(1),
    /** Suggestions this one can't be applied without, such as the plan it would join. */
    dependsOn: z.array(z.string()),
    /** Why the planner chose this, when the request didn't make it obvious. */
    reason: z.string().max(140).nullable(),
    /** Whether the day or week in it is the planner's own idea rather than one you gave. */
    suggested: z.boolean(),
    change: operationSchema,
  })
  .strict();

export const plannerResponseSchema = z.discriminatedUnion("kind", [
  z
    .object({
      kind: z.literal("proposal"),
      proposalId: z.string().uuid(),
      summary: z.string().min(1).max(280),
      operations: z.array(proposedOperationSchema).min(1).max(12),
      references: z.array(proposalReferenceSchema),
      expiresAt: timestampSchema,
    })
    .strict(),
  z
    .object({
      kind: z.literal("clarification"),
      question: z.string().min(1).max(280),
    })
    .strict(),
]);

const appliedProposalSchema = z
  .object({
    eventIds: z.array(z.string().uuid()),
    planIds: z.array(z.string().uuid()),
    milestoneIds: z.array(z.string().uuid()),
    taskIds: z.array(z.string().uuid()),
    workstreamIds: z.array(z.string().uuid()),
  })
  .strict();

const agendaSchema = z
  .object({
    events: z.array(eventSchema),
    tasks: z.array(taskSchema),
    dueTasks: z.array(taskSchema),
    milestones: z.array(milestoneSchema),
    blocks: z.array(scheduledBlockSchema),
  })
  .strict();

const planningBoardSchema = z
  .object({
    tasks: z.array(taskSchema),
    milestones: z.array(milestoneSchema),
  })
  .strict();
const statusSchema = z
  .object({
    phase: z.enum([
      "unavailable",
      "stopped",
      "starting",
      "ready_without_model",
      "downloading",
      "model_ready",
      "update_required",
      "error",
    ]),
    running: z.boolean(),
    modelInstalled: z.boolean(),
    modelName: z.string(),
    modelDigest: z.string().nullable(),
    ollamaVersion: z.string().nullable(),
    modelLicense: z.string().nullable(),
    detail: z.string(),
    download: z
      .object({
        completed: z.number().nonnegative(),
        total: z.number().nonnegative().nullable(),
        percent: z.number().int().min(0).max(100).nullable(),
        status: z.string(),
      })
      .strict()
      .nullable(),
    storageBytes: z.number().nonnegative().nullable(),
  })
  .strict();
const commandErrorSchema = z
  .object({
    code: z.string(),
    message: z.string(),
    retryable: z.boolean(),
    details: z.unknown().optional(),
  })
  .strict();
const localTimeOptionSchema = z
  .object({
    startAtUtc: z.string().datetime({ offset: true }),
    utcOffsetMinutes: z.number().int(),
    label: z.string(),
  })
  .strict();
const localDateTimeResolutionSchema = z.discriminatedUnion("kind", [
  z
    .object({
      kind: z.literal("resolved"),
      startAtUtc: z.string().datetime({ offset: true }),
    })
    .strict(),
  z
    .object({
      kind: z.literal("ambiguous"),
      options: z.array(localTimeOptionSchema).length(2),
    })
    .strict(),
  z.object({ kind: z.literal("nonexistent"), message: z.string() }).strict(),
]);
const backupSchema = z
  .object({
    name: z.string(),
    createdAt: z.string().datetime({ offset: true }),
    sizeBytes: z.number().nonnegative(),
  })
  .strict();
const databaseStatusSchema = z
  .object({
    ready: z.boolean(),
    schemaVersion: z.number().int().nonnegative(),
    error: commandErrorSchema.nullable(),
    backups: z.array(backupSchema),
  })
  .strict();
const importPreviewSchema = z
  .object({
    personCount: z.number().int().nonnegative(),
    planCount: z.number().int().nonnegative(),
    workstreamCount: z.number().int().nonnegative(),
    milestoneCount: z.number().int().nonnegative(),
    eventCount: z.number().int().nonnegative(),
    taskCount: z.number().int().nonnegative(),
    inboxItemCount: z.number().int().nonnegative(),
    taskBlockCount: z.number().int().nonnegative(),
    earliestDay: z.string().nullable(),
    latestDay: z.string().nullable(),
  })
  .strict();
const importSelectionSchema = z
  .object({ token: z.string().uuid(), preview: importPreviewSchema })
  .strict();
const fileActionResultSchema = z
  .object({ completed: z.boolean(), fileName: z.string().nullable() })
  .strict();

export type ScheduleEvent = z.infer<typeof eventSchema>;
export type Plan = z.infer<typeof planSchema>;
export type PlanStatus = Plan["status"];
export type PlanColor = NonNullable<Plan["color"]>;
export type PlanSummary = z.infer<typeof planSummarySchema>;
export type PlanWorkspace = z.infer<typeof planWorkspaceSchema>;
export type PlanDeletion = z.infer<typeof planDeletionSchema>;
export type LinkChange = z.infer<typeof linkChangeSchema>;
export type Person = z.infer<typeof personSchema>;
export type PersonSummary = z.infer<typeof personSummarySchema>;
export type Workstream = z.infer<typeof workstreamSchema>;
export type Agenda = z.infer<typeof agendaSchema>;
export type PlanningBoard = z.infer<typeof planningBoardSchema>;
export type InboxItem = z.infer<typeof inboxItemSchema>;
export type InboxConversion = z.infer<typeof inboxConversionSchema>;
export type Milestone = z.infer<typeof milestoneSchema>;
export type MilestoneStatus = Milestone["status"];
export type Task = z.infer<typeof taskSchema>;
export type Recurrence = z.infer<typeof recurrenceSchema>;
export type RecurrenceFrequency = Recurrence["frequency"];
export type ChecklistItem = z.infer<typeof checklistItemSchema>;
export type TaskReference = z.infer<typeof taskReferenceSchema>;
export type PlanLink = z.infer<typeof planLinkSchema>;
export const planTemplates = [
  "event",
  "trip",
  "move",
  "research",
  "job_search",
  "personal_project",
] as const;
export type PlanTemplate = (typeof planTemplates)[number];
const planTemplateInfoSchema = z
  .object({
    template: z.enum(planTemplates),
    label: z.string(),
    workstreams: z.array(z.string()),
  })
  .strict();
export type PlanTemplateInfo = z.infer<typeof planTemplateInfoSchema>;
export type TaskBlock = z.infer<typeof taskBlockSchema>;
export type ScheduledBlock = z.infer<typeof scheduledBlockSchema>;
export type Calendar = z.infer<typeof calendarSchema>;
export type CalendarKind = Calendar["kind"];
export type CalendarAccount = z.infer<typeof calendarAccountSchema>;
export type CalendarProvider = CalendarAccount["provider"];
export type RemoteCalendar = z.infer<typeof remoteCalendarSchema>;
export type CalendarProblem = NonNullable<Calendar["problem"]>;
export type ExternalEvent = z.infer<typeof externalEventSchema>;
export type CalendarAgenda = z.infer<typeof calendarAgendaSchema>;
export type WorkingHours = z.infer<typeof workingHoursSchema>;
export type Capacity = z.infer<typeof capacitySchema>;
export type DayCapacity = Capacity["days"][number];
export type TaskStatus = Task["status"];
export type TaskPriority = Task["priority"];
export type PlannerResponse = z.infer<typeof plannerResponseSchema>;
export type Proposal = Extract<PlannerResponse, { kind: "proposal" }>;
export type ProposalOperation = Proposal["operations"][number];
export type ProposalChange = ProposalOperation["change"];
/** A suggestion changed in review before applying; Rust checks it still targets the same record. */
export type SuggestionEdit = { id: string; change: ProposalChange };
export type ProposalReference = z.infer<typeof proposalReferenceSchema>;
export type RecordReference = z.infer<typeof recordReferenceSchema>;
export type DayChange = z.infer<typeof dayChangeSchema>;
export type EstimateChange = z.infer<typeof estimateChangeSchema>;
export type AppliedProposal = z.infer<typeof appliedProposalSchema>;
export type ReminderChange = z.infer<typeof reminderChangeSchema>;

export type PlanInput = Pick<
  Plan,
  | "title"
  | "description"
  | "status"
  | "startDate"
  | "targetDate"
  | "color"
  | "links"
>;
export type MilestoneInput = Pick<
  Milestone,
  "title" | "description" | "targetDate" | "status" | "workstreamId"
>;
export type TaskInput = Pick<
  Task,
  | "title"
  | "description"
  | "planId"
  | "milestoneId"
  | "workstreamId"
  | "ownerId"
  | "dueDate"
  | "scheduledDay"
  | "plannedWeek"
  | "estimatedMinutes"
  | "status"
  | "priority"
  | "recurrence"
  | "checklist"
  | "waitingOn"
>;
export type InboxInput = Pick<InboxItem, "text" | "notes">;
export type EventInput = {
  title: string;
  notes: string;
  startAtUtc: string;
  timeZone: string;
  durationMinutes: number;
  reminderMinutesBefore: number | null;
  planId: string | null;
  location: string;
  workstreamId: string | null;
  ownerId: string | null;
};
/** What an inbox item becomes; the record is created and the item removed together. */
export type InboxTarget =
  | { kind: "task"; task: TaskInput }
  | { kind: "plan"; plan: PlanInput }
  | { kind: "event"; event: EventInput };
export type TaskBlockInput = {
  startAtUtc: string;
  timeZone: string;
  durationMinutes: number;
};
export type WorkingHoursInput = Pick<
  WorkingHours,
  "days" | "startMinute" | "endMinute"
>;
/** Moves one task between Plan, Week, and Today; a batch applies in one transaction. */
export type TaskMove = {
  id: string;
  revision: number;
  scheduledDay: DayChange;
  plannedWeek: DayChange;
  status?: TaskStatus;
};
export type PersonInput = Pick<
  Person,
  "displayName" | "role" | "email" | "notes"
>;
export type WorkstreamInput = Pick<Workstream, "name" | "description">;
export const dayPreferences = ["morning", "afternoon", "evening"] as const;
export type DayPreference = (typeof dayPreferences)[number];

export const observationKinds = [
  "estimate_accuracy",
  "usual_start",
  "typical_daily_load",
  "deferred_days",
  "repeatedly_moved",
  "slipping_plan",
] as const;
export type ObservationKind = (typeof observationKinds)[number];

const minutesOfDaySchema = z.number().int().min(0).max(1440).nullable();
const lengthSchema = z.number().int().min(1).max(1440).nullable();

/** How the user plans, as far as they have chosen to say. Every field is optional. */
const planningProfileSchema = z
  .object({
    preferredStartMinute: minutesOfDaySchema,
    preferredEndMinute: minutesOfDaySchema,
    maxPlannedMinutes: lengthSchema,
    focusMinutes: lengthSchema,
    breakMinutes: lengthSchema,
    noWorkDays: z.array(z.number().int().min(1).max(7)),
    energy: z.enum(dayPreferences).nullable(),
    /** Observations the user has switched off; they are neither computed nor shown. */
    mutedObservations: z.array(z.enum(observationKinds)),
    revision: revisionSchema,
    updatedAt: timestampSchema,
  })
  .strict();

export type PlanningProfile = z.infer<typeof planningProfileSchema>;

/** Something Delve Planner worked out from local records, with what it rests on. */
const observationSchema = z
  .object({
    kind: z.enum(observationKinds),
    summary: z.string(),
    evidence: z.string(),
    sample: z.number().int().nonnegative(),
  })
  .strict();

export type Observation = z.infer<typeof observationSchema>;

export type OllamaStatus = z.infer<typeof statusSchema>;

export const modelSources = ["delve_planner", "system"] as const;
export type ModelSource = (typeof modelSources)[number];

const installedModelSchema = z
  .object({
    name: z.string(),
    sizeBytes: z.number().nonnegative(),
    source: z.enum(modelSources),
    /** Whether Delve Planner's own evaluations run against this model. */
    tested: z.boolean(),
    /** Whether it has answered Delve Planner's reply-format check on this machine. */
    checked: z.boolean(),
    selected: z.boolean(),
  })
  .strict();

export type InstalledModel = z.infer<typeof installedModelSchema>;
export type CommandErrorPayload = z.infer<typeof commandErrorSchema>;
export type LocalDateTimeResolution = z.infer<
  typeof localDateTimeResolutionSchema
>;
export type DatabaseStatus = z.infer<typeof databaseStatusSchema>;
export type ImportSelection = z.infer<typeof importSelectionSchema>;

export class DelvePlannerError extends Error {
  code: string;
  retryable: boolean;
  details?: unknown;

  constructor(payload: CommandErrorPayload) {
    super(payload.message);
    this.name = "DelvePlannerError";
    this.code = payload.code;
    this.retryable = payload.retryable;
    this.details = payload.details;
  }
}

export function messageFor(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

async function invokeCommand<T>(name: string, args?: Record<string, unknown>) {
  try {
    return await invoke<T>(name, args);
  } catch (cause) {
    const parsed = commandErrorSchema.safeParse(cause);
    if (parsed.success) throw new DelvePlannerError(parsed.data);
    throw cause instanceof Error ? cause : new Error(String(cause));
  }
}

export const api = {
  async listAgenda(day: string, timeZone: string) {
    return agendaSchema.parse(
      await invokeCommand("list_agenda", { day, timeZone }),
    );
  },
  async listWeek(startDay: string, timeZone: string) {
    return agendaSchema.parse(
      await invokeCommand("list_week", { startDay, timeZone }),
    );
  },
  async listPeople() {
    return z
      .array(personSummarySchema)
      .parse(await invokeCommand("list_people"));
  },
  async createPerson(input: PersonInput) {
    return personSchema.parse(await invokeCommand("create_person", { input }));
  },
  async updatePerson(input: PersonInput & { id: string; revision: number }) {
    return personSchema.parse(await invokeCommand("update_person", { input }));
  },
  async deletePerson(id: string, revision: number) {
    await invokeCommand("delete_person", { id, revision });
  },
  async createWorkstream(input: WorkstreamInput & { planId: string }) {
    return workstreamSchema.parse(
      await invokeCommand("create_workstream", { input }),
    );
  },
  async updateWorkstream(
    input: WorkstreamInput & { id: string; revision: number },
  ) {
    return workstreamSchema.parse(
      await invokeCommand("update_workstream", { input }),
    );
  },
  async deleteWorkstream(id: string, revision: number) {
    await invokeCommand("delete_workstream", { id, revision });
  },
  /** Plans with progress; overdue counts are relative to `today`, a local day. */
  async listPlans(today: string) {
    return z
      .array(planSummarySchema)
      .parse(await invokeCommand("list_plans", { today }));
  },
  async getPlanWorkspace(id: string) {
    return planWorkspaceSchema.parse(
      await invokeCommand("get_plan_workspace", { id }),
    );
  },
  async createPlan(input: PlanInput) {
    return planSchema.parse(await invokeCommand("create_plan", { input }));
  },
  async updatePlan(
    input: PlanInput & { id: string; revision: number; archived: boolean },
  ) {
    return planSchema.parse(await invokeCommand("update_plan", { input }));
  },
  async deletePlan(id: string, revision: number) {
    return planDeletionSchema.parse(
      await invokeCommand("delete_plan", { id, revision }),
    );
  },
  /** The templates a new plan can start from, with the workstreams each creates. */
  async listPlanTemplates() {
    return z
      .array(planTemplateInfoSchema)
      .parse(await invokeCommand("list_plan_templates"));
  },
  /** Creates a plan with a template's starting workstreams in one step. */
  async createPlanFromTemplate(input: PlanInput, template: PlanTemplate) {
    return planSchema.parse(
      await invokeCommand("create_plan_from_template", { input, template }),
    );
  },
  /** Copies a plan's workstreams, milestones, and open tasks, moving dates by `shiftDays`. */
  async duplicatePlan(input: { id: string; title: string; shiftDays: number }) {
    return planSchema.parse(await invokeCommand("duplicate_plan", { input }));
  },
  /** Opens a saved plan link in the browser; links are named by position, never by address. */
  async openPlanLink(planId: string, index: number) {
    await invokeCommand("open_plan_link", { planId, index });
  },
  async createMilestone(input: MilestoneInput & { planId: string }) {
    return milestoneSchema.parse(
      await invokeCommand("create_milestone", { input }),
    );
  },
  async updateMilestone(
    input: MilestoneInput & { id: string; revision: number },
  ) {
    return milestoneSchema.parse(
      await invokeCommand("update_milestone", { input }),
    );
  },
  async deleteMilestone(id: string, revision: number) {
    await invokeCommand("delete_milestone", { id, revision });
  },
  async createEvent(input: EventInput) {
    return eventSchema.parse(await invokeCommand("create_event", { input }));
  },
  async updateEvent(input: {
    id: string;
    revision: number;
    title?: string;
    notes?: string;
    startAtUtc?: string;
    timeZone?: string;
    durationMinutes?: number;
    location?: string;
    reminderChange?: ReminderChange;
    planChange?: LinkChange;
    workstreamChange?: LinkChange;
    ownerChange?: LinkChange;
  }) {
    return eventSchema.parse(await invokeCommand("update_event", { input }));
  },
  async deleteEvent(id: string, revision: number) {
    await invokeCommand("delete_event", { id, revision });
  },
  async rescheduleEvent(input: {
    id: string;
    revision: number;
    startAtUtc: string;
    timeZone: string;
    durationMinutes: number;
    reminderChange?: ReminderChange;
  }) {
    return eventSchema.parse(
      await invokeCommand("reschedule_event", { input }),
    );
  },
  async createTask(input: TaskInput) {
    return taskSchema.parse(await invokeCommand("create_task", { input }));
  },
  async updateTask(input: TaskInput & { id: string; revision: number }) {
    return taskSchema.parse(await invokeCommand("update_task", { input }));
  },
  async deleteTask(id: string, revision: number) {
    await invokeCommand("delete_task", { id, revision });
  },
  /** Moves a repeating task's open occurrence to its next date without finishing it. */
  async skipTaskOccurrence(id: string, revision: number) {
    return taskSchema.parse(
      await invokeCommand("skip_task_occurrence", { id, revision }),
    );
  },
  /** Every open task in a word; one that isn't listed is finished. */
  async listOpenTaskReferences() {
    return z
      .array(taskReferenceSchema)
      .parse(await invokeCommand("list_open_task_references"));
  },
  async listInbox() {
    return z
      .array(inboxItemSchema)
      .parse(await invokeCommand("list_inbox_items"));
  },
  async createInboxItem(input: InboxInput) {
    return inboxItemSchema.parse(
      await invokeCommand("create_inbox_item", { input }),
    );
  },
  async updateInboxItem(input: InboxInput & { id: string; revision: number }) {
    return inboxItemSchema.parse(
      await invokeCommand("update_inbox_item", { input }),
    );
  },
  async deleteInboxItem(id: string, revision: number) {
    await invokeCommand("delete_inbox_item", { id, revision });
  },
  async processInboxItem(item: InboxItem, target: InboxTarget) {
    return inboxConversionSchema.parse(
      await invokeCommand("process_inbox_item", {
        input: { id: item.id, revision: item.revision, target },
      }),
    );
  },
  /** Open work, plus the finished work chosen for or scheduled in the week starting `startDay`. */
  async planningBoard(startDay: string) {
    return planningBoardSchema.parse(
      await invokeCommand("get_planning_board", { startDay }),
    );
  },
  async moveTasks(moves: TaskMove[]) {
    return z
      .array(taskSchema)
      .parse(await invokeCommand("move_tasks", { moves }));
  },
  async createTaskBlock(taskId: string, input: TaskBlockInput) {
    return scheduledBlockSchema.parse(
      await invokeCommand("create_task_block", { input: { taskId, ...input } }),
    );
  },
  async updateTaskBlock(block: TaskBlock, input: TaskBlockInput) {
    return scheduledBlockSchema.parse(
      await invokeCommand("update_task_block", {
        input: { id: block.id, revision: block.revision, ...input },
      }),
    );
  },
  /** Deletes every listed block in one transaction, or none if any changed. */
  async deleteTaskBlocks(blocks: Pick<TaskBlock, "id" | "revision">[]) {
    await invokeCommand("delete_task_blocks", {
      blocks: blocks.map(({ id, revision }) => ({ id, revision })),
    });
  },
  async taskBlocks(taskId: string) {
    return z
      .array(taskBlockSchema)
      .parse(await invokeCommand("list_task_blocks", { taskId }));
  },
  /** Future blocks of finished tasks, which Delve Planner offers to release. */
  async releasableBlocks() {
    return z
      .array(scheduledBlockSchema)
      .parse(await invokeCommand("list_releasable_blocks"));
  },
  async workingHours() {
    return workingHoursSchema.parse(await invokeCommand("get_working_hours"));
  },
  async updateWorkingHours(revision: number, input: WorkingHoursInput) {
    return workingHoursSchema.parse(
      await invokeCommand("update_working_hours", {
        input: { revision, ...input },
      }),
    );
  },
  /** `excludedBlockId` leaves out a block being moved, so its current time counts as free. */
  async capacity(
    startDay: string,
    days: number,
    timeZone: string,
    excludedBlockId: string | null = null,
  ) {
    return capacitySchema.parse(
      await invokeCommand("get_capacity", {
        startDay,
        days,
        timeZone,
        excludedBlockId,
      }),
    );
  },
  async listCalendars() {
    return z.array(calendarSchema).parse(await invokeCommand("list_calendars"));
  },
  async calendarEvents(startDay: string, days: number, timeZone: string) {
    return calendarAgendaSchema.parse(
      await invokeCommand("list_calendar_events", {
        startDay,
        days,
        timeZone,
      }),
    );
  },
  /** Fetches the link once to check it, then keeps it in the system keychain. */
  async subscribeCalendar(input: {
    name: string;
    link: string;
    color: PlanColor;
  }) {
    return calendarSchema.parse(
      await invokeCommand("subscribe_calendar", { input }),
    );
  },
  async replaceCalendarLink(calendar: Calendar, link: string) {
    return calendarSchema.parse(
      await invokeCommand("replace_calendar_link", {
        input: { id: calendar.id, revision: calendar.revision, link },
      }),
    );
  },
  /** Opens a file dialog; null when cancelled. */
  async importCalendarFile(color: PlanColor) {
    return calendarSchema
      .nullable()
      .parse(await invokeCommand("import_calendar_file", { color }));
  },
  /** Opens a file dialog; null when cancelled. */
  async replaceCalendarFile(calendar: Calendar) {
    return calendarSchema.nullable().parse(
      await invokeCommand("replace_calendar_file", {
        id: calendar.id,
        revision: calendar.revision,
      }),
    );
  },
  async updateCalendar(
    calendar: Calendar,
    changes: Partial<Pick<Calendar, "name" | "color" | "visible">>,
  ) {
    return calendarSchema.parse(
      await invokeCommand("update_calendar", {
        input: {
          id: calendar.id,
          revision: calendar.revision,
          name: changes.name ?? calendar.name,
          color: changes.color ?? calendar.color,
          visible: changes.visible ?? calendar.visible,
        },
      }),
    );
  },
  async refreshCalendar(id: string) {
    return calendarSchema.parse(
      await invokeCommand("refresh_calendar", { id }),
    );
  },
  async listCalendarAccounts() {
    return z
      .array(calendarAccountSchema)
      .parse(await invokeCommand("list_calendar_accounts"));
  },
  /** Opens the provider's sign-in page in the browser and waits for read-only access. */
  async connectCalendarAccount(provider: CalendarProvider) {
    return connectedAccountSchema.parse(
      await invokeCommand("connect_calendar_account", { provider }),
    );
  },
  async accountCalendars(accountId: string) {
    return z
      .array(remoteCalendarSchema)
      .parse(await invokeCommand("list_account_calendars", { accountId }));
  },
  async addAccountCalendars(accountId: string, remoteIds: string[]) {
    return z
      .array(calendarSchema)
      .parse(
        await invokeCommand("add_account_calendars", { accountId, remoteIds }),
      );
  },
  async disconnectCalendarAccount(account: CalendarAccount) {
    await invokeCommand("disconnect_calendar_account", {
      id: account.id,
      revision: account.revision,
    });
  },
  async removeCalendar(calendar: Calendar) {
    await invokeCommand("remove_calendar", {
      id: calendar.id,
      revision: calendar.revision,
    });
  },
  async planningProfile() {
    return planningProfileSchema.parse(
      await invokeCommand("get_planning_profile"),
    );
  },
  async updatePlanningProfile(
    profile: PlanningProfile,
    changes: Partial<
      Omit<PlanningProfile, "revision" | "updatedAt" | "mutedObservations">
    > & { mutedObservations?: ObservationKind[] },
  ) {
    const { updatedAt: _updatedAt, ...current } = profile;
    return planningProfileSchema.parse(
      await invokeCommand("update_planning_profile", {
        input: { ...current, ...changes },
      }),
    );
  },
  async clearPlanningProfile() {
    return planningProfileSchema.parse(
      await invokeCommand("clear_planning_profile"),
    );
  },
  async observations() {
    return z
      .array(observationSchema)
      .parse(await invokeCommand("list_observations"));
  },
  async clearPlanningHistory() {
    await invokeCommand("clear_planning_history");
  },
  async status() {
    return statusSchema.parse(await invokeCommand("current_ollama_status"));
  },
  /** Starts the local runtime because the user asked for something that needs it. */
  async startModelRuntime() {
    return statusSchema.parse(await invokeCommand("start_ollama_runtime"));
  },
  /** Lets the model out of memory without stopping the runtime. */
  async releaseModel() {
    await invokeCommand("release_ollama_model");
  },
  async installedModels() {
    return z
      .array(installedModelSchema)
      .parse(await invokeCommand("list_installed_models"));
  },
  async chooseModel(model: string) {
    await invokeCommand("choose_planner_model", { model });
  },
  async checkModel(model: string) {
    await invokeCommand("check_planner_model", { model });
  },
  async downloadModel() {
    await invokeCommand("download_ollama_model");
  },
  async cancelModelDownload() {
    await invokeCommand("cancel_ollama_model_download");
  },
  async restartModelRuntime() {
    await invokeCommand("restart_ollama_runtime");
  },
  async removeModel() {
    await invokeCommand("remove_ollama_model");
  },
  /** The system-wide quick-capture shortcut saved on this computer, or null while it's off. */
  async getQuickCaptureShortcut() {
    return z
      .string()
      .nullable()
      .parse(await invokeCommand("get_quick_capture_shortcut"));
  },
  /** Records a new quick-capture shortcut, or switches it off with null; returns what's saved. */
  async setQuickCaptureShortcut(shortcut: string | null) {
    return z
      .string()
      .nullable()
      .parse(await invokeCommand("set_quick_capture_shortcut", { shortcut }));
  },
  async propose(
    command: string,
    day: string,
    timeZone: string,
    activePlanId: string | null = null,
    /** 0 = Sunday, as the week-start setting stores it. */
    weekStartsOn = 1,
  ) {
    return plannerResponseSchema.parse(
      await invokeCommand("propose_schedule_changes", {
        command,
        day,
        timeZone,
        activePlanId,
        weekStartsOn,
      }),
    );
  },
  /**
   * Applies the accepted suggestions, with any the user edited first; leaving `accepted` out
   * applies all of them.
   */
  async apply(
    proposalId: string,
    accepted?: string[],
    edits: SuggestionEdit[] = [],
  ) {
    return appliedProposalSchema.parse(
      await invokeCommand("apply_schedule_changes", {
        proposalId,
        accepted,
        edits,
      }),
    );
  },
  async discardProposal(proposalId: string) {
    await invokeCommand("discard_schedule_proposal", { proposalId });
  },
  async cancelPlannerRequest() {
    await invokeCommand("cancel_planner_request");
  },
  async clearContext() {
    await invokeCommand("clear_planner_context");
  },
  async resolveLocalDateTime(day: string, time: string, timeZone: string) {
    return localDateTimeResolutionSchema.parse(
      await invokeCommand("resolve_local_datetime", {
        input: { day, time, timeZone },
      }),
    );
  },
  async databaseStatus() {
    return databaseStatusSchema.parse(await invokeCommand("database_status"));
  },
  async exportFile() {
    return fileActionResultSchema.parse(
      await invokeCommand("export_planner_file"),
    );
  },
  async selectImport() {
    return importSelectionSchema
      .nullable()
      .parse(await invokeCommand("select_planner_import"));
  },
  async applySelectedImport(token: string) {
    return importPreviewSchema.parse(
      await invokeCommand("apply_selected_import", { token }),
    );
  },
  async discardSelectedImport() {
    await invokeCommand("discard_selected_import");
  },
  async exportDiagnostics() {
    return fileActionResultSchema.parse(
      await invokeCommand("export_diagnostic_bundle"),
    );
  },
  async restoreBackup(backupName: string) {
    await invokeCommand("restore_database_backup", { backupName });
  },
};

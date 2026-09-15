import { describe, expect, it } from "vitest";
import {
  eventSchema,
  linkChangeSchema,
  planSchema,
  plannerResponseSchema,
  taskSchema,
} from "./api";

describe("planning record boundary", () => {
  const stamp = "2026-09-14T12:00:00.000Z";
  const task = {
    id: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
    title: "Confirm AV vendor",
    description: "",
    planId: "f67fcad6-2827-4668-829f-1950f441d054",
    milestoneId: null,
    workstreamId: null,
    ownerId: null,
    dueDate: "2026-10-13",
    scheduledDay: null,
    status: "todo",
    priority: "high",
    completedAt: null,
    sortOrder: 0,
    revision: 1,
    createdAt: stamp,
    updatedAt: stamp,
  };

  it("accepts a general task without a scheduled day", () => {
    expect(taskSchema.parse(task).scheduledDay).toBeNull();
  });

  it("rejects the legacy day-bound task shape and unknown statuses", () => {
    expect(() =>
      taskSchema.parse({ ...task, day: "2026-10-13", completed: false }),
    ).toThrow();
    expect(() => taskSchema.parse({ ...task, status: "backlog" })).toThrow();
  });

  it("requires plan fields from the closed palette and status set", () => {
    const plan = {
      id: "f67fcad6-2827-4668-829f-1950f441d054",
      title: "Creator Showcase",
      description: "",
      status: "active",
      startDate: null,
      targetDate: "2026-10-16",
      color: "clay",
      archived: false,
      revision: 2,
      createdAt: stamp,
      updatedAt: stamp,
    };
    expect(planSchema.parse(plan).targetDate).toBe("2026-10-16");
    expect(() => planSchema.parse({ ...plan, color: "#ff0000" })).toThrow();
    expect(() => planSchema.parse({ ...plan, status: "at_risk" })).toThrow();
  });

  it("types event plan membership and plan changes", () => {
    expect(() =>
      eventSchema.parse({
        id: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
        title: "Walkthrough",
        notes: "",
        startAtUtc: stamp,
        timeZone: "America/New_York",
        durationMinutes: 60,
        reminderMinutesBefore: null,
        reminderStatus: "none",
        revision: 1,
        createdAt: stamp,
        updatedAt: stamp,
      }),
    ).toThrow();
    expect(
      linkChangeSchema.parse({
        action: "set",
        id: "f67fcad6-2827-4668-829f-1950f441d054",
      }).action,
    ).toBe("set");
    expect(() => linkChangeSchema.parse({ action: "set" })).toThrow();
  });
});

describe("planner response boundary", () => {
  it("rejects fields outside the approved operation schema", () => {
    expect(() =>
      plannerResponseSchema.parse({
        kind: "proposal",
        proposalId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
        summary: "Move gym",
        expiresAt: "2026-08-12T20:00:00.000Z",
        references: [],
        operations: [
          {
            type: "delete_event",
            eventId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
            expectedRevision: 1,
            sql: "DROP TABLE schedule_events",
          },
        ],
      }),
    ).toThrow();
  });

  it("accepts a clarification without mutations", () => {
    expect(
      plannerResponseSchema.parse({
        kind: "clarification",
        question: "Which Gym event should I move?",
      }).kind,
    ).toBe("clarification");
  });

  it("accepts only typed reminder changes within seven days", () => {
    const base = {
      kind: "proposal",
      proposalId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
      summary: "Remind before gym",
      expiresAt: "2030-05-10T20:00:00.000Z",
      references: [],
    } as const;
    expect(
      plannerResponseSchema.parse({
        ...base,
        operations: [
          {
            type: "update_event",
            eventId: "f67fcad6-2827-4668-829f-1950f441d054",
            expectedRevision: 1,
            title: null,
            notes: null,
            durationMinutes: null,
            reminderChange: { action: "set", minutesBefore: 15 },
          },
        ],
      }).kind,
    ).toBe("proposal");
    expect(() =>
      plannerResponseSchema.parse({
        ...base,
        operations: [
          {
            type: "update_event",
            eventId: "f67fcad6-2827-4668-829f-1950f441d054",
            expectedRevision: 1,
            title: null,
            notes: null,
            durationMinutes: null,
            reminderChange: { action: "set", minutesBefore: 10_081 },
          },
        ],
      }),
    ).toThrow();
  });

  it("accepts planning operations with typed references and day changes", () => {
    const parsed = plannerResponseSchema.parse({
      kind: "proposal",
      proposalId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
      summary: "Start the bake sale",
      expiresAt: "2026-09-15T20:00:00.000Z",
      references: [
        {
          id: "f67fcad6-2827-4668-829f-1950f441d054",
          kind: "task",
          title: "Book venue",
        },
      ],
      operations: [
        {
          type: "create_plan",
          title: "Bake sale",
          description: "",
          status: "planning",
          startDate: null,
          targetDate: "2026-10-03",
        },
        {
          type: "create_task",
          title: "Buy flour",
          description: "",
          plan: { newTitle: "Bake sale" },
          milestone: null,
          scheduledDay: null,
          dueDate: "2026-10-01",
          status: "todo",
          priority: "normal",
        },
        {
          type: "schedule_task",
          taskId: "f67fcad6-2827-4668-829f-1950f441d054",
          expectedRevision: 2,
          scheduledDay: { action: "set", day: "2026-09-16" },
          dueDate: { action: "unchanged" },
        },
      ],
    });
    expect(parsed.kind).toBe("proposal");
  });

  it("rejects references and day changes outside the contract", () => {
    const proposal = (operation: Record<string, unknown>) => ({
      kind: "proposal",
      proposalId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
      summary: "Change it",
      expiresAt: "2026-09-15T20:00:00.000Z",
      references: [],
      operations: [operation],
    });
    for (const operation of [
      {
        type: "set_task_plan",
        taskId: "f67fcad6-2827-4668-829f-1950f441d054",
        expectedRevision: 1,
        plan: { id: "not-a-uuid" },
        milestone: null,
      },
      {
        type: "set_task_plan",
        taskId: "f67fcad6-2827-4668-829f-1950f441d054",
        expectedRevision: 1,
        plan: { id: "f67fcad6-2827-4668-829f-1950f441d054", newTitle: "Both" },
        milestone: null,
      },
      {
        type: "schedule_task",
        taskId: "f67fcad6-2827-4668-829f-1950f441d054",
        expectedRevision: 1,
        scheduledDay: { action: "set" },
        dueDate: { action: "unchanged" },
      },
      {
        type: "delete_plan",
        planId: "f67fcad6-2827-4668-829f-1950f441d054",
        expectedRevision: 1,
      },
    ]) {
      expect(() => plannerResponseSchema.parse(proposal(operation))).toThrow();
    }
  });
});

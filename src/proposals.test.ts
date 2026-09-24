import { describe, expect, it } from "vitest";
import type { Proposal } from "./api";
import {
  acceptedOperations,
  describeProposal,
  proposalEnablesReminder,
  withEdits,
} from "./proposals";

const venueId = "f67fcad6-2827-4668-829f-1950f441d054";
const charityId = "30bb9c6a-4020-45a6-806b-5eb71c7ae76f";
const callId = "5d5a3e1c-2d67-4a7e-9d3c-6c9a1f0d8e21";

describe("proposal previews", () => {
  it("names existing records by title and new plans as new", () => {
    const proposal: Pick<Proposal, "operations" | "references"> = {
      references: [
        { id: venueId, kind: "task", title: "Book venue" },
        { id: charityId, kind: "plan", title: "Streamer Charity Week" },
        { id: callId, kind: "event", title: "Sponsor call" },
      ],
      operations: [
        {
          id: "op-0",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "update_task",
            taskId: venueId,
            expectedRevision: 1,
            title: null,
            description: null,
            status: "done",
            priority: null,
            estimate: { action: "unchanged" },
          },
        },
        {
          id: "op-1",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "create_task",
            title: "Buy flour",
            description: "",
            plan: { newTitle: "Bake sale" },
            milestone: null,
            scheduledDay: null,
            dueDate: "2026-10-01",
            status: "todo",
            priority: "high",
            workstream: null,
            fromInbox: null,
          },
        },
        {
          id: "op-2",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "set_event_plan",
            eventId: callId,
            expectedRevision: 3,
            plan: { id: charityId },
          },
        },
        {
          id: "op-3",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "reschedule_event",
            eventId: callId,
            expectedRevision: 3,
            title: null,
            notes: "",
            startAtUtc: "2026-09-18T19:00:00.000Z",
            timeZone: "America/New_York",
            durationMinutes: null,
            reminderChange: { action: "set", minutesBefore: 15 },
          },
        },
        {
          id: "op-4",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "schedule_task",
            taskId: venueId,
            expectedRevision: 1,
            scheduledDay: { action: "clear" },
            dueDate: { action: "set", day: "2026-10-06" },
            plannedWeek: { action: "unchanged" },
          },
        },
      ],
    };
    const previews = describeProposal(proposal, "America/New_York");
    expect(previews.map((preview) => preview.title)).toEqual([
      "Mark “Book venue” done",
      "Add task “Buy flour”",
      "Add “Sponsor call” to “Streamer Charity Week”",
      "Move “Sponsor call”",
      "Reschedule “Book venue”",
    ]);
    expect(previews[1].details).toEqual([
      "In “Bake sale” (new)",
      "Due 01 Oct",
      "High priority",
    ]);
    expect(previews[3].details).toEqual([
      "To Fri 18 Sep, 15:00",
      "Clear the notes",
      "Reminder 15 minutes before",
    ]);
    expect(previews[4].details).toEqual(["No scheduled day", "Due 06 Oct"]);
    expect(previews.map((preview) => preview.tone)).toEqual([
      "change",
      "create",
      "move",
      "move",
      "move",
    ]);
    expect(proposalEnablesReminder(proposal)).toBe(true);
  });

  it("rejecting a new plan also rejects what was going to live in it", () => {
    const proposal: Pick<Proposal, "operations" | "references"> = {
      references: [],
      operations: [
        {
          id: "op-0",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "create_plan",
            title: "Bake sale",
            description: "",
            status: "planning",
            startDate: null,
            targetDate: null,
            fromInbox: null,
          },
        },
        {
          id: "op-1",
          dependsOn: ["op-0"],
          reason: null,
          suggested: false,
          change: {
            type: "create_task",
            title: "Buy flour",
            description: "",
            plan: { newTitle: "Bake sale" },
            milestone: null,
            scheduledDay: null,
            dueDate: "2026-10-01",
            status: "todo",
            priority: "normal",
            workstream: null,
            fromInbox: null,
          },
        },
        {
          id: "op-2",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "create_event",
            title: "Sponsor call",
            notes: "",
            startAtUtc: "2026-09-18T19:00:00.000Z",
            timeZone: "America/New_York",
            durationMinutes: 30,
            reminderMinutesBefore: null,
            plan: null,
            fromInbox: null,
          },
        },
      ],
    };
    const previews = describeProposal(proposal, "America/New_York");

    expect(acceptedOperations(previews, new Set())).toEqual([
      "op-0",
      "op-1",
      "op-2",
    ]);
    // The task can't be applied without the plan it was going to join, so it goes too.
    expect(acceptedOperations(previews, new Set(["op-0"]))).toEqual(["op-2"]);
    // Rejecting only the task leaves the plan, which stands on its own.
    expect(acceptedOperations(previews, new Set(["op-1"]))).toEqual([
      "op-0",
      "op-2",
    ]);
    expect(acceptedOperations(previews, new Set(["op-0", "op-2"]))).toEqual([]);
  });

  it("describes workstreams and work that comes from the inbox", () => {
    const streamId = "7c3e9a1b-2f4d-4e6a-8b0c-9d1e2f3a4b5c";
    const itemId = "0b8f2a4c-6d1e-4f3a-9b7c-5e2d1a0f9c8b";
    const previews = describeProposal(
      {
        references: [
          { id: charityId, kind: "plan", title: "Streamer Charity Week" },
          { id: venueId, kind: "task", title: "Book venue" },
          { id: streamId, kind: "workstream", title: "Logistics" },
          { id: itemId, kind: "inbox_item", title: "Print posters" },
        ],
        operations: [
          {
            id: "op-0",
            dependsOn: [],
            reason: null,
            suggested: false,
            change: {
              type: "create_workstream",
              plan: { id: charityId },
              name: "Promotion",
            },
          },
          {
            id: "op-1",
            dependsOn: [],
            reason: null,
            suggested: false,
            change: {
              type: "set_task_workstream",
              taskId: venueId,
              expectedRevision: 2,
              workstream: { id: streamId },
            },
          },
          {
            id: "op-2",
            dependsOn: ["op-0"],
            reason: null,
            suggested: false,
            change: {
              type: "create_task",
              title: "Print posters",
              description: "",
              plan: { id: charityId },
              milestone: null,
              scheduledDay: null,
              dueDate: null,
              status: "todo",
              priority: "normal",
              workstream: { newTitle: "Promotion" },
              fromInbox: { itemId, expectedRevision: 1 },
            },
          },
        ],
      },
      "America/New_York",
    );
    expect(previews.map((preview) => preview.title)).toEqual([
      "Add workstream “Promotion”",
      "Put “Book venue” in “Logistics”",
      "Add task “Print posters”",
    ]);
    expect(previews[0].details).toEqual(["In “Streamer Charity Week”"]);
    expect(previews[2].details).toEqual([
      "In “Streamer Charity Week”",
      "Workstream “Promotion” (new)",
      "From your inbox",
    ]);
  });

  it("applies edits and carries a renamed new plan to what goes in it", () => {
    const proposal: Pick<Proposal, "operations"> = {
      operations: [
        {
          id: "op-0",
          dependsOn: [],
          reason: null,
          suggested: false,
          change: {
            type: "create_plan",
            title: "Bake sale",
            description: "",
            status: "planning",
            startDate: null,
            targetDate: null,
            fromInbox: null,
          },
        },
        {
          id: "op-1",
          dependsOn: ["op-0"],
          reason: null,
          suggested: false,
          change: {
            type: "create_task",
            title: "Buy flour",
            description: "",
            plan: { newTitle: "bake  SALE" },
            milestone: null,
            scheduledDay: null,
            dueDate: "2026-10-01",
            status: "todo",
            priority: "normal",
            workstream: null,
            fromInbox: null,
          },
        },
      ],
    };
    expect(withEdits(proposal, new Map())).toBe(proposal);
    const renamed = withEdits(
      proposal,
      new Map([
        [
          "op-0",
          {
            type: "create_plan" as const,
            title: "School fair",
            description: "",
            status: "planning" as const,
            startDate: null,
            targetDate: null,
            fromInbox: null,
          },
        ],
      ]),
    );
    const task = renamed.operations[1].change;
    expect(task.type === "create_task" && task.plan).toEqual({
      newTitle: "School fair",
    });
    // The original stays as the planner proposed it.
    const original = proposal.operations[1].change;
    expect(original.type === "create_task" && original.plan).toEqual({
      newTitle: "bake  SALE",
    });
  });
});

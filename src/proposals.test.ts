import { describe, expect, it } from "vitest";
import type { Proposal } from "./api";
import { describeProposal, proposalEnablesReminder } from "./proposals";

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
          type: "update_task",
          taskId: venueId,
          expectedRevision: 1,
          title: null,
          description: null,
          status: "done",
          priority: null,
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
          priority: "high",
        },
        {
          type: "set_event_plan",
          eventId: callId,
          expectedRevision: 3,
          plan: { id: charityId },
        },
        {
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
        {
          type: "schedule_task",
          taskId: venueId,
          expectedRevision: 1,
          scheduledDay: { action: "clear" },
          dueDate: { action: "set", day: "2026-10-06" },
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
});

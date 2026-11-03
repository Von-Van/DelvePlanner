import { describe, expect, it } from "vitest";
import {
  AttentionInput,
  maxAttentionItems,
  worthAttention,
} from "./AssistantView";
import { HeadsUp } from "./headsUpApi";
import { sectionShortcutKey, sectionShortcuts } from "./shortcuts";

function headsUp(id: string, title: string): HeadsUp {
  return {
    id,
    kind: "deadline",
    title,
    evidence: "No time is planned for it yet.",
    target: null,
    actionLabel: "Plan it",
    actionRequest: `${title}. Plan time for it.`,
    score: 90,
    urgent: true,
    status: "active",
    surfacedAt: "2026-11-05T14:00:00.000Z",
    expiresAt: "2026-11-07T05:00:00.000Z",
    snoozedUntil: null,
  };
}

const quiet: AttentionInput = {
  headsUps: [],
  brokenCalendars: [],
  overCapacityBy: 0,
  unfinished: 0,
  dueOpen: 0,
  releasable: 0,
  planningPrompt: null,
};

describe("worthAttention", () => {
  it("gives nothing on a quiet day instead of filling a quota", () => {
    expect(worthAttention(quiet)).toEqual([]);
  });

  it("explains every item with the facts behind it", () => {
    const items = worthAttention({ ...quiet, unfinished: 2, dueOpen: 1 });
    expect(items.map((item) => item.title)).toEqual([
      "2 unfinished tasks from earlier days",
      "1 task due today",
    ]);
    for (const item of items) expect(item.evidence).not.toBe("");
  });

  it("shows at most three, most pressing first", () => {
    const items = worthAttention({
      headsUps: [],
      brokenCalendars: ["Work"],
      overCapacityBy: 90,
      unfinished: 3,
      dueOpen: 2,
      releasable: 1,
      planningPrompt: "week",
    });
    expect(items).toHaveLength(maxAttentionItems);
    expect(items.map((item) => item.id)).toEqual([
      "calendars",
      "capacity",
      "unfinished",
    ]);
    expect(items[0].title).toBe("“Work” can't refresh");
    expect(items[1].evidence).toContain("1 h 30 min");
  });

  it("puts the assistant's heads-ups after a broken calendar and keeps the cap", () => {
    const items = worthAttention({
      ...quiet,
      brokenCalendars: ["Work"],
      unfinished: 1,
      headsUps: [
        headsUp("a", "“Venue deposit” is due tomorrow"),
        headsUp("b", "Friday is overbooked by 2 h"),
      ],
    });
    expect(items.map((item) => item.id)).toEqual([
      "calendars",
      "heads-up-a",
      "heads-up-b",
    ]);
    expect(items[1]).toMatchObject({
      headsUpId: "a",
      headsUpKind: "deadline",
      action: { label: "Plan it", target: "assistant" },
    });
  });

  it("sends planning sessions to their own flow", () => {
    expect(
      worthAttention({ ...quiet, planningPrompt: "day" })[0].action.target,
    ).toBe("plan-today");
    expect(
      worthAttention({ ...quiet, planningPrompt: "week" })[0].action.target,
    ).toBe("plan-week");
  });
});

describe("section shortcuts", () => {
  it("opens the three main areas first and keeps the older places on 4 to 7", () => {
    expect(sectionShortcuts["1"]).toBe("assistant");
    expect(sectionShortcuts["2"]).toBe("schedule");
    expect(sectionShortcuts["3"]).toBe("projects");
    expect(sectionShortcutKey("people")).toBe("4");
    expect(sectionShortcutKey("inbox")).toBe("5");
    expect(sectionShortcutKey("calendars")).toBe("6");
    expect(sectionShortcutKey("knowledge")).toBe("7");
  });
});

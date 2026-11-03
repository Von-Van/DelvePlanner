import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AssistantFeedback, LessonCard } from "./AssistantFeedback";
import {
  Feedback,
  feedbackApi,
  feedbackOutcome,
  FeedbackReceipt,
  lessonProblem,
} from "./feedbackApi";
import { MemoryRecord, provenance } from "./memoryApi";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

function feedback(overrides: Partial<Feedback> = {}): Feedback {
  return {
    id: "f1",
    runId: "run-1",
    suggestionId: null,
    target: "change",
    about: "Review lease · Thu 4:30 pm",
    rating: "not_helpful",
    reason: "I don't do deep work after 4",
    scope: "pending",
    memoryId: null,
    createdAt: "2026-11-02T20:00:00.000Z",
    updatedAt: "2026-11-02T20:00:00.000Z",
    ...overrides,
  };
}

const receipt: FeedbackReceipt = {
  feedback: feedback(),
  lesson: { kind: "preference", content: "I don't do deep work after 4." },
};

describe("feedback on a reply", () => {
  it("asks whether a saved reply helped, and needs a saved run", () => {
    const html = renderToStaticMarkup(
      <AssistantFeedback runId="run-1" onMessage={() => {}} />,
    );
    expect(html).toContain("Was this helpful?");
    expect(html).toContain("Helpful</button>");
    expect(html).toContain("Not helpful…</button>");
    expect(
      renderToStaticMarkup(
        <AssistantFeedback runId={null} onMessage={() => {}} />,
      ),
    ).toBe("");
  });

  it("offers the lesson in the person's words, with both choices", () => {
    const html = renderToStaticMarkup(
      <LessonCard
        receipt={receipt}
        api={feedbackApi}
        onSettled={() => {}}
        onMessage={() => {}}
      />,
    );
    expect(html).toContain("LEARN FROM THIS?");
    expect(html).toContain("I don&#x27;t do deep work after 4.");
    expect(html).toContain("Review lease · Thu 4:30 pm");
    expect(html).toContain("Just this once</button>");
    expect(html).toContain("Remember for next time</button>");
    expect(html).toContain('<option value="preference" selected="">');
  });

  it("says what came of the feedback once the person has chosen", () => {
    expect(feedbackOutcome(feedback())).toBeNull();
    expect(feedbackOutcome(feedback({ scope: "this_time" }))).toMatch(
      /this conversation only.*Nothing was remembered/,
    );
    expect(
      feedbackOutcome(feedback({ scope: "lasting", memoryId: "m1" })),
    ).toMatch(/^Remembered\. .*Settings › Memory/);
    expect(
      feedbackOutcome(
        feedback({ scope: "lasting", memoryId: null, reason: null }),
      ),
    ).toMatch(/then deleted/);
    expect(
      feedbackOutcome(feedback({ rating: "helpful", scope: "this_time" })),
    ).toBe("You marked this reply as helpful.");
    expect(
      feedbackOutcome(
        feedback({ reason: null, scope: "this_time", about: null }),
      ),
    ).toBe("You marked this reply as not helpful.");
  });

  it("checks the lesson's words", () => {
    expect(lessonProblem("  ")).toMatch(/Write/);
    expect(lessonProblem("x".repeat(501))).toMatch(/under 500/);
    expect(lessonProblem("No deep work after 4 pm.")).toBeNull();
  });

  it("labels a remembered lesson as coming from the person's feedback", () => {
    const record: MemoryRecord = {
      id: "m1",
      kind: "preference",
      content: "No deep work after 4 pm.",
      source: "user_stated",
      sourceRef: "feedback f1 on assistant run run-1",
      planId: null,
      confirmation: "confirmed",
      createdAt: "2026-11-02T20:00:00.000Z",
      updatedAt: "2026-11-02T20:00:00.000Z",
      confirmedAt: "2026-11-02T20:00:00.000Z",
      expiresAt: null,
      supersededBy: null,
      supersededAt: null,
    };
    expect(provenance(record, new Map())).toMatch(
      /^Preference · From your feedback/,
    );
    expect(provenance({ ...record, sourceRef: null }, new Map())).toMatch(
      /^Preference · You said this/,
    );
  });
});

describe("feedback commands", () => {
  beforeEach(() => invoke.mockReset());

  it("send the run, the choice and the lesson", async () => {
    invoke.mockResolvedValueOnce(receipt);
    await feedbackApi.record({
      runId: "run-1",
      target: "change",
      about: "Review lease · Thu 4:30 pm",
      rating: "not_helpful",
      reason: "I don't do deep work after 4",
    });
    expect(invoke).toHaveBeenLastCalledWith("record_assistant_feedback", {
      input: {
        runId: "run-1",
        target: "change",
        about: "Review lease · Thu 4:30 pm",
        rating: "not_helpful",
        reason: "I don't do deep work after 4",
      },
    });
    const kept = {
      feedback: feedback({ scope: "lasting", memoryId: "m1" }),
      lesson: null,
    };
    invoke.mockResolvedValueOnce(kept);
    await expect(
      feedbackApi.learn("f1", "preference", "No deep work after 4 pm."),
    ).resolves.toEqual(kept);
    expect(invoke).toHaveBeenLastCalledWith("learn_from_feedback", {
      feedbackId: "f1",
      kind: "preference",
      content: "No deep work after 4 pm.",
    });
    invoke.mockResolvedValueOnce({
      feedback: feedback({ scope: "this_time" }),
      lesson: null,
    });
    await feedbackApi.keepOneOff("f2");
    expect(invoke).toHaveBeenLastCalledWith("keep_feedback_one_off", {
      feedbackId: "f2",
    });
    invoke.mockResolvedValueOnce({ not: "feedback" });
    await expect(feedbackApi.list("run-1")).rejects.toThrow(/shape/);
  });
});

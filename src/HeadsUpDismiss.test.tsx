import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FeedbackReceipt } from "./feedbackApi";
import { HeadsUpDismiss, headsUpLessonExplanation } from "./HeadsUpDismiss";
import { headsUpApi, headsUpKindLabels, headsUpKinds } from "./headsUpApi";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const quiet = {
  id: "h1",
  title: "“Garden” has gone quiet",
  kind: "quiet_project" as const,
};

const headsUp = {
  ...quiet,
  evidence: "Nothing has changed in 30 days; 2 tasks are open.",
  target: { kind: "plan" as const, id: "p1" },
  actionLabel: "Check in",
  actionRequest: "Garden has gone quiet.",
  score: 20,
  urgent: false,
  status: "dismissed" as const,
  surfacedAt: "2026-11-05T14:00:00.000Z",
  expiresAt: "2026-11-12T05:00:00.000Z",
  snoozedUntil: null,
};

const lesson: FeedbackReceipt = {
  feedback: {
    id: "f1",
    runId: null,
    suggestionId: "h1",
    target: "suggestion",
    about: quiet.title,
    rating: "not_helpful",
    reason: "because the garden is on hold until spring",
    scope: "pending",
    memoryId: null,
    createdAt: "2026-11-05T14:01:00.000Z",
    updatedAt: "2026-11-05T14:01:00.000Z",
  },
  lesson: {
    kind: "preference",
    content: "The garden is on hold until spring.",
  },
};

const page = (html: string) =>
  html
    .replace(/<[^>]+>/g, " ")
    .replace(/&rsquo;/g, "’")
    .replace(/&#x27;/g, "'")
    .replace(/\s+/g, " ");

describe("dismissing a heads-up", () => {
  it("asks why, offers to stop the kind, and can be cancelled", () => {
    const text = page(
      renderToStaticMarkup(
        <HeadsUpDismiss
          headsUp={quiet}
          onDone={() => {}}
          onCancel={() => {}}
          onMessage={() => {}}
        />,
      ),
    );
    expect(text).toContain("Why isn’t this useful? (optional)");
    expect(text).toContain("Stop showing quiet projects");
    expect(text).toContain("Settings › Heads-ups");
    expect(text).toContain("Cancel");
    expect(text).toContain("Dismiss");
  });

  it("offers the reason back as a lesson, honest about what it changes", () => {
    const text = page(
      renderToStaticMarkup(
        <HeadsUpDismiss
          headsUp={quiet}
          onDone={() => {}}
          onCancel={() => {}}
          onMessage={() => {}}
          initialLesson={lesson}
        />,
      ),
    );
    expect(text).toContain("LEARN FROM THIS?");
    expect(text).toContain(`about ${quiet.title}`);
    expect(text).toContain("Just this once");
    expect(text).toContain("Remember for next time");
    expect(text).toContain(headsUpLessonExplanation);
    expect(headsUpLessonExplanation).toMatch(/doesn't change which heads-ups/);
  });

  it("names every kind in plain words", () => {
    for (const kind of headsUpKinds)
      expect(headsUpKindLabels[kind]).toMatch(/^[a-z]/);
  });
});

describe("heads-up commands", () => {
  beforeEach(() => invoke.mockReset());

  it("send the dismissal and read back the kind stopped and the lesson", async () => {
    const receipt = {
      headsUp,
      mutedKind: "quiet_project",
      feedback: lesson,
    };
    invoke.mockResolvedValueOnce(receipt);
    await expect(
      headsUpApi.dismiss("h1", {
        stopKind: true,
        reason: "because the garden is on hold until spring",
      }),
    ).resolves.toEqual(receipt);
    expect(invoke).toHaveBeenLastCalledWith("dismiss_heads_up", {
      id: "h1",
      dismissal: {
        stopKind: true,
        reason: "because the garden is on hold until spring",
      },
    });

    invoke.mockResolvedValueOnce({ headsUp, mutedKind: null, feedback: null });
    await headsUpApi.dismiss("h1");
    expect(invoke).toHaveBeenLastCalledWith("dismiss_heads_up", {
      id: "h1",
      dismissal: null,
    });
  });

  it("reads and saves the stopped kinds with the other settings", async () => {
    const settings = {
      enabled: true,
      notify: false,
      mutedKinds: ["focus_window"],
    };
    invoke.mockResolvedValueOnce(settings);
    await expect(headsUpApi.settings()).resolves.toEqual(settings);
    invoke.mockResolvedValueOnce(settings);
    await headsUpApi.updateSettings({
      enabled: true,
      notify: false,
      mutedKinds: ["focus_window"],
    });
    expect(invoke).toHaveBeenLastCalledWith("update_heads_up_settings", {
      settings,
    });
  });
});

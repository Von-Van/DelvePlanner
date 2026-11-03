import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AgentRun } from "./api";
import { assistantBoard, doneLimit, needsYouCount } from "./assistantTasks";
import { TasksView } from "./TasksView";

const now = Date.parse("2026-11-03T18:00:00.000Z");

function run(key: string, overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    runKey: key,
    runId: `run-${key}`,
    storedRunId: `stored-${key}`,
    threadId: `thread-${key}`,
    architecture: "A1",
    goal: `Request ${key}`,
    startedAt: "2026-11-03T17:00:00.000Z",
    finishedAt: "2026-11-03T17:00:05.000Z",
    status: "answered",
    stopReason: null,
    answer: `Answer ${key}`,
    error: null,
    cancelled: false,
    model: null,
    usage: {
      modelCalls: 1,
      toolCalls: 1,
      toolFailures: 0,
      promptTokens: 10,
      completionTokens: 5,
      durationMs: 5000,
    },
    draftCheck: null,
    review: { state: "none" },
    proposal: null,
    memorySuggestions: [],
    trace: [],
    ...overrides,
  };
}

function drafted(
  key: string,
  review: AgentRun["review"] = { state: "pending" },
  expiresAt = "2026-11-03T19:00:00.000Z",
): AgentRun {
  return run(key, {
    status: "drafted",
    review,
    proposal: {
      kind: "proposal",
      proposalId: "30bb9c6a-4020-45a6-806b-5eb71c7ae76f",
      summary: "Book movers on Friday.",
      operations: [{}, {}] as never,
      references: [],
      expiresAt,
    },
  });
}

describe("assistantBoard", () => {
  it("puts changes to approve and open questions under Needs you", () => {
    const board = assistantBoard(
      [
        drafted("a"),
        run("b", { status: "needs_clarification", answer: "Which review?" }),
        run("c"),
      ],
      [],
      now,
    );
    expect(board.needsYou.map((card) => card.kind)).toEqual([
      "review",
      "question",
    ]);
    expect(board.done.map((card) => card.runKey)).toEqual(["c"]);
    expect(needsYouCount([drafted("a"), run("c")], now)).toBe(1);
  });

  it("treats a question answered by a follow-up as done", () => {
    const question = run("q", {
      status: "needs_clarification",
      threadId: "t",
    });
    const followUp = run("f", {
      threadId: "t",
      startedAt: "2026-11-03T17:05:00.000Z",
      finishedAt: "2026-11-03T17:05:05.000Z",
    });
    const board = assistantBoard([question, followUp], [], now);
    expect(board.needsYou).toEqual([]);
    expect(board.done.map((card) => card.runKey)).toEqual(["f", "q"]);
  });

  it("never asks for approval of expired, applied or turned-down changes", () => {
    const board = assistantBoard(
      [
        drafted("old", { state: "pending" }, "2026-11-03T17:30:00.000Z"),
        drafted("applied", { state: "applied", changes: 2 }),
        drafted("no", { state: "discarded" }),
      ],
      [],
      now,
    );
    expect(board.needsYou).toEqual([]);
    const labels = Object.fromEntries(
      board.done.map((card) => [
        card.runKey,
        card.kind === "done" ? card.label : "",
      ]),
    );
    expect(labels).toEqual({
      old: "Expired",
      applied: "Applied",
      no: "Not applied",
    });
  });

  it("offers another try only when asking again could help", () => {
    const board = assistantBoard(
      [
        run("safe", { status: "cannot_complete", answer: "Capacity unknown." }),
        run("stopped", { cancelled: true }),
        run("fine"),
      ],
      [],
      now,
    );
    const retry = Object.fromEntries(
      board.done.map((card) => [
        card.runKey,
        card.kind === "done" && card.retry,
      ]),
    );
    expect(retry).toEqual({ safe: true, stopped: true, fine: false });
  });

  it("shows a running request as working until its record arrives", () => {
    const live = [
      {
        runKey: "a",
        request: "Plan tomorrow",
        steps: 3,
        lastStep: "Read tomorrow's calendar",
        startedAt: "2026-11-03T17:59:00.000Z",
      },
    ];
    expect(assistantBoard([], live, now).working).toHaveLength(1);
    expect(assistantBoard([run("a")], live, now).working).toEqual([]);
  });

  it("keeps the Done column short", () => {
    const runs = Array.from({ length: doneLimit + 4 }, (_, index) =>
      run(`r${index}`),
    );
    expect(assistantBoard(runs, [], now).done).toHaveLength(doneLimit);
  });
});

describe("TasksView", () => {
  it("shows the three columns with the right actions", () => {
    const html = renderToStaticMarkup(
      <TasksView
        board={assistantBoard(
          [
            drafted("a"),
            run("b", { status: "needs_clarification", answer: "Which one?" }),
            run("c", { status: "cannot_complete", answer: "I stopped." }),
          ],
          [],
          now,
        )}
        canStart
        onStart={() => undefined}
        onReview={() => undefined}
        onAnswer={() => undefined}
        onRetry={() => undefined}
        onStop={() => undefined}
      />,
    );
    expect(html).toContain("Needs you");
    expect(html).toContain("Working");
    expect(html).toContain("2 changes ready");
    expect(html).toContain("Review changes");
    expect(html).toContain("Which one?");
    expect(html).toContain("Stopped safely");
    expect(html).toContain("Try again");
    expect(html).toContain("isn&#x27;t working on anything right now");
  });
});

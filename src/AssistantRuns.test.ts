import { describe, expect, it } from "vitest";
import { agentRunSchema, AgentRun } from "./api";
import {
  durationLabel,
  fallbackReason,
  runMeasures,
  runTone,
  shortDigest,
  withRun,
  withStep,
} from "./AssistantRuns";

/** A finished run as the app sends it, checked against the same schema the app uses. */
function run(overrides: Partial<AgentRun> = {}): AgentRun {
  return agentRunSchema.parse({
    runKey: "key-1",
    runId: "run-1",
    storedRunId: null,
    threadId: null,
    architecture: "A1",
    goal: "What's due this week?",
    startedAt: "2026-11-02T15:00:00.000Z",
    finishedAt: "2026-11-02T15:00:04.200Z",
    status: "answered",
    stopReason: null,
    answer: "Two tasks are due.",
    error: null,
    cancelled: false,
    model: {
      name: "qwen3:8b",
      digest: "0123456789abcdef0123",
      ollamaVersion: "0.32.0",
    },
    usage: {
      modelCalls: 2,
      toolCalls: 3,
      toolFailures: 1,
      promptTokens: 1200,
      completionTokens: 80,
      durationMs: 4200,
    },
    draftCheck: null,
    review: { state: "none" },
    proposal: null,
    memorySuggestions: [],
    trace: [
      {
        runId: "run-1",
        seq: 1,
        kind: "plan",
        at: "2026-11-02T15:00:00.100Z",
        summary: "run started",
        data: {},
      },
      {
        runId: "run-1",
        seq: 2,
        kind: "model_call",
        at: "2026-11-02T15:00:01.100Z",
        durationMs: null,
        summary: "model call 1",
        data: { model: "qwen3:8b" },
      },
    ],
    ...overrides,
  });
}

describe("agent runs", () => {
  it("parses the app's run record, including steps without a duration", () => {
    expect(run().trace).toHaveLength(2);
  });

  it("shows every measurable fact of a run", () => {
    const measures = Object.fromEntries(
      runMeasures(run()).map((measure) => [measure.label, measure.value]),
    );
    expect(measures).toEqual({
      Time: "4.2 s",
      "Model calls": "2",
      "Tool calls": "3 (1 failed)",
      Tokens: "1,200 in · 80 out",
      Model: "qwen3:8b · 0123456789ab",
    });
  });

  it("adds the safety check when the agent drafted changes", () => {
    const measures = runMeasures(
      run({
        status: "drafted",
        draftCheck: {
          drafted: 2,
          passed: 1,
          problems: ["stale revision"],
          memoryProblems: [],
        },
      }),
    );
    expect(measures.find((m) => m.label === "Safety check")?.value).toBe(
      "1 of 2 drafts passed",
    );
  });

  it("reads durations at a glance", () => {
    expect(durationLabel(820)).toBe("820 ms");
    expect(durationLabel(12_400)).toBe("12 s");
    expect(durationLabel(65_000)).toBe("1 min 5 s");
    expect(shortDigest("sha256:abcdef0123456789")).toBe("abcdef012345");
  });

  it("tones a run by how it ended", () => {
    expect(runTone(run())).toBe("good");
    expect(runTone(run({ status: "needs_clarification" }))).toBe("attention");
    expect(runTone(run({ status: "failed" }))).toBe("problem");
    expect(runTone(run({ status: "failed", cancelled: true }))).toBe("neutral");
  });

  it("hands only failed runs to the old planner", () => {
    expect(fallbackReason(run())).toBeNull();
    expect(
      fallbackReason(
        run({ status: "cannot_complete", stopReason: "ambiguous_target" }),
      ),
    ).toBeNull();
    expect(
      fallbackReason(
        run({ status: "cannot_complete", stopReason: "model_error" }),
      ),
    ).toBe("the model call failed");
    expect(
      fallbackReason(run({ status: "cannot_complete", stopReason: "error" })),
    ).toBe("the agent stopped with an error");
    expect(fallbackReason(run({ status: "needs_clarification" }))).toBeNull();
    expect(
      fallbackReason(run({ status: "failed", error: "model unreachable" })),
    ).toBe("model unreachable");
    expect(fallbackReason(run({ status: "failed", error: null }))).toBe(
      "the run failed",
    );
    expect(
      fallbackReason(run({ status: "failed", cancelled: true })),
    ).toBeNull();
  });

  it("keeps live steps in order and ignores repeats", () => {
    const [first, second] = run().trace;
    expect(withStep(withStep([second], first), second)).toEqual([
      first,
      second,
    ]);
  });

  it("puts the newest run first without duplicates", () => {
    const older = run({ runKey: "old" });
    const newer = run();
    expect(withRun([newer, older], newer).map((r) => r.runKey)).toEqual([
      "key-1",
      "old",
    ]);
  });
});

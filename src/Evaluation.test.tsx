import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentTrace, brief, stepDetails, TraceEvent } from "./AgentTrace";
import {
  compareRuns,
  defaultComparison,
  earlierRuns,
  EvaluationCase,
  evaluationApi,
  evaluationOverviewSchema,
  evaluationRunDetailSchema,
  evaluationTraceSchema,
  latencyLabel,
  percentLabel,
  shortDigest,
  skillBreakdown,
} from "./evaluationApi";
import {
  ComparisonResult,
  EvaluationCaseView,
  EvaluationEmpty,
  EvaluationRunDetailView,
  EvaluationRunList,
} from "./EvaluationView";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

/**
 * What the Rust commands return for the fixture result files in
 * src-tauri/tests/fixtures/evaluation. A Rust test keeps these snapshots current.
 */
import benchmarkJson from "../src-tauri/tests/fixtures/evaluation-ui/benchmark-run.json";
import overviewJson from "../src-tauri/tests/fixtures/evaluation-ui/overview.json";
import regressionJson from "../src-tauri/tests/fixtures/evaluation-ui/regression-run.json";
import traceJson from "../src-tauri/tests/fixtures/evaluation-ui/trace.json";

const snapshots: Record<string, unknown> = {
  "overview.json": overviewJson,
  "benchmark-run.json": benchmarkJson,
  "regression-run.json": regressionJson,
  "trace.json": traceJson,
};
const snapshot = (name: string) =>
  JSON.parse(JSON.stringify(snapshots[name])) as unknown;

const overview = evaluationOverviewSchema.parse(snapshot("overview.json"));
const benchmark = evaluationRunDetailSchema.parse(
  snapshot("benchmark-run.json"),
);
const regression = evaluationRunDetailSchema.parse(
  snapshot("regression-run.json"),
);
const trace = evaluationTraceSchema.parse(snapshot("trace.json"));

const text = (html: string) =>
  html
    .replace(/<[^>]+>/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&#x27;/g, "'")
    .replace(/\s+/g, " ");

describe("evaluation results", () => {
  it("reads everything the Rust commands return for the fixtures", () => {
    expect(overview.runs.map((run) => run.id)).toEqual([
      "benchmark:bench-a1-sample",
      "regression:regression-2026-11.json",
    ]);
    expect(benchmark.cases).toHaveLength(3);
    expect(trace.events.length).toBeGreaterThan(2);
  });

  it("formats rates, times and digests for reading", () => {
    expect(percentLabel(1 / 3)).toBe("33.3%");
    expect(percentLabel(null)).toBe("—");
    expect(latencyLabel(0.4)).toBe("<1 ms");
    expect(latencyLabel(812)).toBe("812 ms");
    expect(latencyLabel(2310.5)).toBe("2.31 s");
    expect(shortDigest("sha256:500a1f067a9f7826abc")).toBe("500a1f067a9f");
  });

  it("calls only the read-only commands and rejects a malformed reply", async () => {
    invoke.mockReset();
    invoke.mockResolvedValueOnce(snapshot("overview.json"));
    await expect(evaluationApi.listRuns()).resolves.toEqual(overview);
    expect(invoke).toHaveBeenCalledWith("list_evaluation_runs", undefined);

    invoke.mockResolvedValueOnce({ runs: "nope" });
    await expect(evaluationApi.listRuns()).rejects.toThrow(/can't read/);

    invoke.mockRejectedValueOnce({ code: "internal", message: "Not found." });
    await expect(evaluationApi.getRun("benchmark:x")).rejects.toThrow(
      "Not found.",
    );
    expect(invoke).toHaveBeenLastCalledWith("get_evaluation_run", {
      runId: "benchmark:x",
    });
  });
});

describe("run list", () => {
  it("shows architecture, model, hardware, success, safety and latency for each run", () => {
    const html = renderToStaticMarkup(
      <EvaluationRunList overview={overview} onOpen={() => {}} />,
    );
    const page = text(html);
    expect(page).toContain("A1");
    expect(page).toContain("A0");
    expect(page).toContain("qwen3:8b");
    expect(page).toContain("@ 500a1f067a9f");
    expect(page).toContain("chip: Apple M2, memory: 16 GB");
    expect(page).toContain("33.3% 1/3");
    expect(page).toContain("99.2% 375/378");
    expect(page).toContain("1.50 s");
    expect(page).toContain("2.31 s");
    // The benchmark's safety failure is flagged; the clean regression run is not.
    expect(html.match(/evaluation-safety flagged/g)).toHaveLength(1);
  });

  it("explains how to get results when there are none", () => {
    const html = renderToStaticMarkup(
      <EvaluationRunList
        overview={{ resultsFolder: "/data/results", runs: [], problems: [] }}
        onOpen={() => {}}
      />,
    );
    const page = text(html);
    expect(page).toContain("No test results yet");
    expect(page).toContain("python -m agent_bench check");
    expect(page).toContain("python -m agent_bench run");
    expect(page).toContain("npm run eval");
    expect(page).toContain("/data/results");
    expect(renderToStaticMarkup(<EvaluationEmpty resultsFolder="x" />)).toBe(
      renderToStaticMarkup(<EvaluationEmpty resultsFolder="x" />),
    );
  });

  it("reports unreadable files instead of hiding them", () => {
    const page = text(
      renderToStaticMarkup(
        <EvaluationRunList
          overview={{ ...overview, problems: ["broken.json: bad JSON"] }}
          onOpen={() => {}}
        />,
      ),
    );
    expect(page).toContain("Some results couldn’t be read");
    expect(page).toContain("broken.json: bad JSON");
  });
});

describe("one run", () => {
  it("lists every case with its skill, tags, pass/fail and safety", () => {
    const html = renderToStaticMarkup(
      <EvaluationRunDetailView detail={benchmark} onOpenCase={() => {}} />,
    );
    const page = text(html);
    for (const id of [
      "tc-today-agenda",
      "ms-least-busy-day",
      "untrusted-event-notes",
    ])
      expect(page).toContain(id);
    // A recorded skill is shown in plain words; older rows fall back to their family.
    expect(page).toContain("Combining several lookups");
    expect(page).toContain("untrusted_input");
    expect(html).toContain('title="multi_step"');
    expect(page).toContain("fixture");
    expect(page.match(/\bPass\b/g)).toHaveLength(1);
    expect(page.match(/\bFail\b/g)).toHaveLength(2);
    expect(page).toContain("Unsafe");
    expect(page).toContain("1 case run broke a safety rule");
    // One row in the skill breakdown, one in the case list.
    expect(html.match(/<tr class="unsafe">/g)).toHaveLength(2);
  });

  it("can show only the safety failures", () => {
    const page = text(
      renderToStaticMarkup(
        <EvaluationRunDetailView
          detail={benchmark}
          onOpenCase={() => {}}
          initialFilter="unsafe"
        />,
      ),
    );
    expect(page).toContain("untrusted-event-notes");
    expect(page).not.toContain("tc-today-agenda");
  });

  it("says when a regression report lists only its failures", () => {
    const page = text(
      renderToStaticMarkup(
        <EvaluationRunDetailView detail={regression} onOpenCase={() => {}} />,
      ),
    );
    expect(page).toContain("124 of 126 cases passed every repeat");
    expect(page).toContain("relative-followup run 2");
    // Safety is only counted for the whole run, so cases neither claim OK nor Unsafe.
    expect(page).toContain("Not recorded");
    expect(page).not.toMatch(/\bOK\b/);
    expect(page).not.toContain("broke a safety rule");
    // Regression reports carry no skills and list only failures, so there is no breakdown.
    expect(page).not.toContain("By skill");
  });
});

describe("skills and comparisons", () => {
  const make = (
    caseId: string,
    success: boolean,
    overrides: Partial<EvaluationCase> = {},
  ): EvaluationCase => ({
    key: `${caseId}#${overrides.repeat ?? 0}`,
    caseId,
    family: "memory",
    skill: "Remembering what matters",
    split: "dev",
    tags: [],
    repeat: 0,
    status: success ? "answered" : "cannot_complete",
    success,
    safe: true,
    failedChecks: [],
    safetyViolations: [],
    latencyMs: 10,
    hasTrace: false,
    ...overrides,
  });

  it("groups cases by skill, failing skills first", () => {
    const lines = skillBreakdown([
      make("mem-a", true),
      make("mem-b", false),
      make("cont-a", true, { skill: "Follow-ups in a conversation" }),
      make("old-row", true, { skill: null, family: "tool_choice" }),
      make("bad", false, {
        skill: "Ignoring instructions in your data",
        safe: false,
      }),
    ]);
    expect(lines.map((l) => l.skill)).toEqual([
      "Ignoring instructions in your data",
      "Remembering what matters",
      "Follow-ups in a conversation",
      "tool_choice",
    ]);
    expect(lines[0]).toMatchObject({ runs: 1, passed: 0, unsafe: 1 });
    expect(lines[1]).toMatchObject({ runs: 2, passed: 1, unsafe: 0 });
  });

  it("shows the skill breakdown on a run", () => {
    const page = text(
      renderToStaticMarkup(
        <EvaluationRunDetailView detail={benchmark} onOpenCase={() => {}} />,
      ),
    );
    expect(page).toContain("By skill");
    expect(page).toContain("Combining several lookups 0/1");
    expect(page).toContain("untrusted_input 0/1 1 unsafe");
    expect(page).toContain("tool_choice 1/1 OK");
  });

  it("names cases that got better, worse, or still fail across repeats", () => {
    const before = [
      make("a", true),
      make("b", false),
      make("c", true),
      make("c", true, { repeat: 1 }),
      make("d", false),
      make("gone", true),
    ];
    const after = [
      make("a", false),
      make("b", true),
      make("c", true),
      make("c", false, { repeat: 1 }),
      make("d", false),
      make("new", true),
    ];
    expect(compareRuns(after, before)).toEqual({
      better: ["b"],
      worse: ["a", "c"],
      stillFailing: ["d"],
      newCases: ["new"],
      droppedCases: ["gone"],
    });
    // An unsafe pass is not clean.
    expect(
      compareRuns([make("a", true, { safe: false })], [make("a", true)]).worse,
    ).toEqual(["a"]);
  });

  it("compares with the latest earlier benchmark run of the same design", () => {
    const run = overview.runs[0];
    const at = (
      id: string,
      startedAt: string,
      architecture = run.architecture,
    ) => ({
      ...run,
      id,
      startedAt,
      architecture,
    });
    const runs = [
      run,
      at("benchmark:older", "2000-01-01T00:00:00Z"),
      at("benchmark:newer-other", "2000-01-03T00:00:00Z", "A0"),
      at("benchmark:future", "2999-01-01T00:00:00Z"),
      overview.runs[1],
    ];
    const candidates = earlierRuns(runs, run);
    expect(candidates.map((r) => r.id)).toEqual([
      "benchmark:newer-other",
      "benchmark:older",
    ]);
    expect(defaultComparison(candidates, run)?.id).toBe("benchmark:older");
    expect(earlierRuns(runs, overview.runs[1])).toEqual([]);
  });

  it("reads the comparison plainly and warns when the setup changed", () => {
    const earlier = { ...benchmark.run, label: "assistant (a1-v4)" };
    const page = text(
      renderToStaticMarkup(
        <ComparisonResult
          run={benchmark.run}
          earlier={earlier}
          cases={benchmark.cases}
          previousCases={benchmark.cases.map((c) => ({
            ...c,
            success: true,
            safe: true,
          }))}
        />,
      ),
    );
    expect(page).toContain("setup or the model changed");
    expect(page).toContain(
      "Worse now 2 ms-least-busy-day, untrusted-event-notes",
    );
    expect(page).toContain("Better now 0 None");
    const same = text(
      renderToStaticMarkup(
        <ComparisonResult
          run={benchmark.run}
          earlier={benchmark.run}
          cases={benchmark.cases}
          previousCases={benchmark.cases}
        />,
      ),
    );
    expect(same).not.toContain("changed between");
    expect(same).toContain("Still not passing 2");
  });

  it("offers a choice of earlier runs on the run page", () => {
    const html = renderToStaticMarkup(
      <EvaluationRunDetailView
        detail={benchmark}
        onOpenCase={() => {}}
        comparison={{
          candidates: [
            { ...benchmark.run, id: "benchmark:older", label: "Older run" },
          ],
          selectedId: null,
          onSelect: () => {},
          previous: null,
        }}
      />,
    );
    const page = text(html);
    expect(page).toContain("Since an earlier run");
    expect(page).toContain("Older run");
    expect(page).toContain("Pick an earlier run");
  });
});

describe("one case", () => {
  it("shows failed checks, broken safety rules and the step-by-step trace", () => {
    const unsafeCase = benchmark.cases[2];
    const page = text(
      renderToStaticMarkup(
        <EvaluationCaseView
          run={benchmark.run}
          testCase={unsafeCase}
          trace={{ state: "ready", value: trace }}
        />,
      ),
    );
    expect(page).toContain("Safety rules broken");
    expect(page).toContain("followed_untrusted_instruction");
    expect(page).toContain("Step by step");
    expect(page).toContain("Model");
    expect(page).toContain("qwen3:8b chose get_day_context");
    expect(page).toContain("Tool call");
    expect(page).toContain("Finished");
  });

  it("explains that regression reports keep no traces", () => {
    const page = text(
      renderToStaticMarkup(
        <EvaluationCaseView
          run={regression.run}
          testCase={regression.cases[0]}
          trace={null}
        />,
      ),
    );
    expect(page).toContain("expected result");
    expect(page).toContain("don't keep step-by-step traces");
  });
});

describe("AgentTrace", () => {
  const step = (seq: number, overrides: Partial<TraceEvent>): TraceEvent => ({
    runId: "run-1",
    seq,
    kind: "tool_call",
    at: "2026-11-01T21:00:00Z",
    summary: `step ${seq}`,
    data: {},
    ...overrides,
  });

  beforeEach(() => invoke.mockReset());

  it("orders steps by sequence and shows a status badge", () => {
    const html = renderToStaticMarkup(
      <AgentTrace
        status="drafted"
        events={[
          step(2, { kind: "final", summary: "status drafted" }),
          step(1, { summary: "search_tasks (call 1)", durationMs: 812 }),
        ]}
      />,
    );
    const page = text(html);
    expect(page.indexOf("search_tasks")).toBeLessThan(
      page.indexOf("status drafted"),
    );
    expect(page).toContain("Drafted changes");
    expect(page).toContain("812 ms");
  });

  it("marks failed tool results and errors", () => {
    const html = renderToStaticMarkup(
      <AgentTrace
        events={[
          step(1, {
            kind: "tool_result",
            data: { ok: false, errorCode: "timeout" },
          }),
          step(2, { kind: "error", summary: "agent raised" }),
        ]}
      />,
    );
    expect(html.match(/trace-step kind-\w+ failed/g)).toHaveLength(2);
    expect(text(html)).toContain("timeout");
  });

  it("shows a working row while live and an explanation when empty", () => {
    expect(
      text(renderToStaticMarkup(<AgentTrace events={[]} live />)),
    ).toContain("Working…");
    expect(
      text(
        renderToStaticMarkup(<AgentTrace events={[]} emptyText="Nothing" />),
      ),
    ).toContain("Nothing");
  });

  it("summarises tool arguments and results without flooding the panel", () => {
    const details = stepDetails(
      step(1, {
        data: {
          tool: "search_tasks",
          args: { query: "review", limit: 20 },
          injected: false,
          callSeq: 1,
          debug: { prompt: "hidden" },
        },
      }),
    );
    expect(details).toEqual([
      ["Tool", "search_tasks"],
      ["Arguments", '{"query":"review","limit":20}'],
      ["callSeq", "1"],
    ]);
    expect(brief("x".repeat(500))).toHaveLength(160);
  });
});

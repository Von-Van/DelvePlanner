import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentTrace, brief, stepDetails, TraceEvent } from "./AgentTrace";
import {
  evaluationApi,
  evaluationOverviewSchema,
  evaluationRunDetailSchema,
  evaluationTraceSchema,
  latencyLabel,
  percentLabel,
  shortDigest,
} from "./evaluationApi";
import {
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
  it("lists every case with family, tags, pass/fail and safety", () => {
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
    expect(page).toContain("multi_step");
    expect(page).toContain("untrusted_input");
    expect(page).toContain("fixture");
    expect(page.match(/\bPass\b/g)).toHaveLength(1);
    expect(page.match(/\bFail\b/g)).toHaveLength(2);
    expect(page).toContain("Unsafe");
    expect(page).toContain("1 case run broke a safety rule");
    expect(html.match(/<tr class="unsafe">/g)).toHaveLength(1);
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
    expect(page).not.toContain("broke a safety rule");
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

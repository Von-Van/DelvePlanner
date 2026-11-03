/**
 * Read-only access to benchmark and regression results for the Model & Evaluation panel. Every
 * call reads files or saved runs; nothing here starts a benchmark or changes planner data.
 */
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { TraceEvent, traceEventSchema } from "./AgentTrace";

export const runSources = ["benchmark", "regression", "recorded"] as const;
export type RunSource = (typeof runSources)[number];

const countSchema = z.number().int().nonnegative();
const msSchema = z.number().nonnegative().nullable();

export const evaluationRunSchema = z.object({
  id: z.string().min(1),
  source: z.enum(runSources),
  label: z.string(),
  architecture: z.string().nullable(),
  modelName: z.string().nullable(),
  modelDigest: z.string().nullable(),
  startedAt: z.string().nullable(),
  hardware: z.string().nullable(),
  cases: countSchema,
  repeats: countSchema,
  attempts: countSchema,
  passedAttempts: countSchema,
  successRate: z.number().min(0).max(1).nullable(),
  safetyFailures: countSchema,
  latencyP50Ms: msSchema,
  latencyP95Ms: msSchema,
});
export type EvaluationRun = z.infer<typeof evaluationRunSchema>;

export const evaluationOverviewSchema = z.object({
  resultsFolder: z.string(),
  runs: z.array(evaluationRunSchema),
  problems: z.array(z.string()),
});
export type EvaluationOverview = z.infer<typeof evaluationOverviewSchema>;

export const evaluationCaseSchema = z.object({
  key: z.string().min(1),
  caseId: z.string(),
  family: z.string().nullable(),
  /** What the case checks, in plain words. Older runs and regression reports leave it out. */
  skill: z.string().nullable(),
  split: z.string().nullable(),
  tags: z.array(z.string()),
  repeat: countSchema,
  status: z.string().nullable(),
  success: z.boolean(),
  /** Null when the source records safety only for the run as a whole. */
  safe: z.boolean().nullable(),
  failedChecks: z.array(z.object({ name: z.string(), detail: z.string() })),
  safetyViolations: z.array(z.object({ rule: z.string(), detail: z.string() })),
  latencyMs: msSchema,
  hasTrace: z.boolean(),
});
export type EvaluationCase = z.infer<typeof evaluationCaseSchema>;

export const evaluationRunDetailSchema = z.object({
  run: evaluationRunSchema,
  cases: z.array(evaluationCaseSchema),
  note: z.string().nullable(),
});
export type EvaluationRunDetail = z.infer<typeof evaluationRunDetailSchema>;

export const evaluationTraceSchema = z.object({
  events: z.array(traceEventSchema),
  status: z.string().nullable(),
});
export type EvaluationTrace = { events: TraceEvent[]; status: string | null };

const commandErrorSchema = z.object({ message: z.string() });

async function read<T>(
  schema: z.ZodType<T>,
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  let raw: unknown;
  try {
    raw = await invoke(command, args);
  } catch (cause) {
    const parsed = commandErrorSchema.safeParse(cause);
    throw new Error(parsed.success ? parsed.data.message : String(cause));
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    throw new Error(
      "The test results came back in a shape this view can't read.",
    );
  }
  return parsed.data;
}

export const evaluationApi = {
  listRuns: () => read(evaluationOverviewSchema, "list_evaluation_runs"),
  getRun: (runId: string) =>
    read(evaluationRunDetailSchema, "get_evaluation_run", { runId }),
  getTrace: (runId: string, caseKey: string) =>
    read(evaluationTraceSchema, "get_evaluation_trace", { runId, caseKey }),
};
export type EvaluationApi = typeof evaluationApi;

// ---------------------------------------------------------------------------------------------
// Formatting shared by the panel and its tests.

export const sourceLabels: Record<RunSource, string> = {
  benchmark: "Benchmark",
  regression: "Regression set",
  recorded: "Saved in app",
};

export function percentLabel(rate: number | null) {
  return rate === null ? "—" : `${Math.round(rate * 1000) / 10}%`;
}

export function latencyLabel(ms: number | null) {
  if (ms === null) return "—";
  if (ms < 1) return "<1 ms";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

/** The first 12 characters of a model digest, enough to tell two builds apart. */
export function shortDigest(digest: string | null) {
  if (!digest) return null;
  return digest.replace(/^sha256:/, "").slice(0, 12);
}

export function dateLabel(iso: string | null) {
  if (!iso) return "Unknown date";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

// ---------------------------------------------------------------------------------------------
// Skills and comparisons: how a run reads to someone asking "is the assistant getting better?"

/** The case's plain-language skill, or its family for runs saved before skills were recorded. */
export function skillOf(testCase: EvaluationCase) {
  return testCase.skill ?? testCase.family;
}

export type SkillLine = {
  skill: string;
  runs: number;
  passed: number;
  unsafe: number;
};

/** One line per skill: skills with a failure first, then the rest, each group alphabetical. */
export function skillBreakdown(cases: EvaluationCase[]): SkillLine[] {
  const lines = new Map<string, SkillLine>();
  for (const testCase of cases) {
    const skill = skillOf(testCase) ?? "Other";
    const line = lines.get(skill) ?? { skill, runs: 0, passed: 0, unsafe: 0 };
    line.runs += 1;
    if (testCase.success) line.passed += 1;
    if (testCase.safe === false) line.unsafe += 1;
    lines.set(skill, line);
  }
  const clean = (line: SkillLine) =>
    line.passed === line.runs && line.unsafe === 0;
  return [...lines.values()].sort(
    (a, b) =>
      Number(clean(a)) - Number(clean(b)) || a.skill.localeCompare(b.skill),
  );
}

export type RunComparison = {
  better: string[];
  worse: string[];
  stillFailing: string[];
  newCases: string[];
  droppedCases: string[];
};

/**
 * Case by case, what changed between an earlier run and this one. A case is clean when every one
 * of its runs passed safely; "better" became clean, "worse" stopped being clean. Cases only one run
 * has are listed, not judged.
 */
export function compareRuns(
  current: EvaluationCase[],
  previous: EvaluationCase[],
): RunComparison {
  const cleanByCase = (cases: EvaluationCase[]) => {
    const out = new Map<string, boolean>();
    for (const c of cases) {
      const ok = c.success && c.safe !== false;
      out.set(c.caseId, (out.get(c.caseId) ?? true) && ok);
    }
    return out;
  };
  const now = cleanByCase(current);
  const then = cleanByCase(previous);
  const sorted = (ids: Iterable<string>) => [...ids].sort();
  const shared = sorted([...now.keys()].filter((id) => then.has(id)));
  return {
    better: shared.filter((id) => now.get(id) && !then.get(id)),
    worse: shared.filter((id) => !now.get(id) && then.get(id)),
    stillFailing: shared.filter((id) => !now.get(id) && !then.get(id)),
    newCases: sorted([...now.keys()].filter((id) => !then.has(id))),
    droppedCases: sorted([...then.keys()].filter((id) => !now.has(id))),
  };
}

/** Earlier benchmark runs this one can be compared with, newest first. */
export function earlierRuns(runs: EvaluationRun[], run: EvaluationRun) {
  if (run.source !== "benchmark" || !run.startedAt) return [];
  const started = run.startedAt;
  return runs
    .filter(
      (other) =>
        other.id !== run.id &&
        other.source === "benchmark" &&
        other.startedAt !== null &&
        other.startedAt < started,
    )
    .sort((a, b) => (b.startedAt ?? "").localeCompare(a.startedAt ?? ""));
}

/** The run to compare with unless the person picks another: the latest earlier run of the same design. */
export function defaultComparison(
  candidates: EvaluationRun[],
  run: EvaluationRun,
) {
  return (
    candidates.find((other) => other.architecture === run.architecture) ?? null
  );
}

/** Whether two runs differ in something that changes how their scores compare. */
export function settingsDiffer(a: EvaluationRun, b: EvaluationRun) {
  return (
    a.label !== b.label ||
    a.modelName !== b.modelName ||
    a.modelDigest !== b.modelDigest
  );
}

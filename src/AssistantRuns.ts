import type { AgentRun, AgentRunStatus } from "./api";
import type { TraceEvent } from "./AgentTrace";

/** How each way a run can end reads on the Assistant. */
export const runStatusLabels: Record<AgentRunStatus, string> = {
  answered: "Answered",
  drafted: "Changes proposed",
  needs_clarification: "Needs more detail",
  cannot_complete: "Couldn't complete",
  failed: "Didn't finish",
};

/** The tone a status is shown in: good, needs the user, or went wrong. */
export function runTone(run: Pick<AgentRun, "status" | "cancelled">) {
  if (run.cancelled) return "neutral";
  switch (run.status) {
    case "answered":
    case "drafted":
      return "good";
    case "needs_clarification":
      return "attention";
    case "cannot_complete":
    case "failed":
      return "problem";
  }
}

/** "820 ms", "4.2 s", "1 min 5 s". */
export function durationLabel(ms: number) {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const seconds = ms / 1000;
  if (seconds < 60)
    return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds - minutes * 60);
  return rest === 0 ? `${minutes} min` : `${minutes} min ${rest} s`;
}

/** "1,234" */
export function countLabel(value: number) {
  return value.toLocaleString("en-US");
}

/** A model digest short enough to read, as Ollama shows it. */
export function shortDigest(digest: string) {
  return digest.replace(/^sha256:/, "").slice(0, 12);
}

export type RunMeasure = {
  label: string;
  value: string;
  /** What the number means, for a tooltip or screen reader. */
  hint: string;
};

/**
 * The measurable facts of one run, in the order the Assistant shows them. These are the same
 * numbers the benchmark and the Evaluation view score, so a live answer can be judged the same way.
 */
export function runMeasures(run: AgentRun): RunMeasure[] {
  const { usage } = run;
  const measures: RunMeasure[] = [
    {
      label: "Time",
      value: durationLabel(usage.durationMs),
      hint: "From sending the request to the final answer.",
    },
    {
      label: "Model calls",
      value: countLabel(usage.modelCalls),
      hint: "How many times the agent asked the local model for its next step.",
    },
    {
      label: "Tool calls",
      value:
        usage.toolFailures > 0
          ? `${countLabel(usage.toolCalls)} (${countLabel(usage.toolFailures)} failed)`
          : countLabel(usage.toolCalls),
      hint: "Read-only lookups into your planner. Failed ones returned an error the agent had to handle.",
    },
    {
      label: "Tokens",
      value: `${countLabel(usage.promptTokens)} in · ${countLabel(usage.completionTokens)} out`,
      hint: "Text the model read and wrote, counted by the local runtime.",
    },
  ];
  if (run.draftCheck)
    measures.push({
      label: "Safety check",
      value: `${run.draftCheck.passed} of ${run.draftCheck.drafted} changes passed`,
      hint: "Each proposed change must name a record and version the agent actually looked up.",
    });
  measures.push({
    label: "Model",
    value: run.model
      ? `${run.model.name} · ${shortDigest(run.model.digest)}`
      : "Unknown",
    hint: "The exact local model and version that produced this run.",
  });
  return measures;
}

/** Adds a step that arrived live, once, keeping the steps in order. */
export function withStep(steps: TraceEvent[], step: TraceEvent) {
  if (steps.some((existing) => existing.seq === step.seq)) return steps;
  return [...steps, step].sort((a, b) => a.seq - b.seq);
}

/** Puts a finished run first, replacing an older copy of the same run. */
export function withRun(runs: AgentRun[], run: AgentRun) {
  return [run, ...runs.filter((existing) => existing.runKey !== run.runKey)];
}

/** A key for a new run, known before it starts so its live steps can be matched to it. */
export function newRunKey() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `run-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/** What one recent run's row says: the time, the cost, and what was asked. */
export function runSummaryLine(run: AgentRun) {
  return `${durationLabel(run.usage.durationMs)} · ${run.usage.modelCalls} model · ${run.usage.toolCalls} tool calls`;
}

/**
 * Why a finished run should hand its request to the old planner, or null when it shouldn't.
 * Only a failed run falls back: one the user stopped, or one that answered, drafted, asked a
 * question or honestly couldn't complete, stands as the agent's result.
 */
export function fallbackReason(run: AgentRun): string | null {
  if (run.status !== "failed" || run.cancelled) return null;
  return run.error?.trim() || "the run failed";
}

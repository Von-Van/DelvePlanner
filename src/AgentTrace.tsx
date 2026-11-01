/**
 * Step-by-step view of one agent run: model calls, tool calls with their arguments and results,
 * drafts, and the final status. Purely presentational and read-only, so the Evaluation panel can
 * show saved benchmark traces and the Assistant can show a live run with the same component.
 */
import { useEffect, useRef } from "react";
import { z } from "zod";
import "./AgentTrace.css";

export const traceKinds = [
  "model_call",
  "tool_call",
  "tool_result",
  "plan",
  "verify",
  "repair",
  "draft",
  "final",
  "error",
] as const;
export type TraceKind = (typeof traceKinds)[number];

/** One trace step in the shared shape every component emits (plan contract 5). */
export const traceEventSchema = z.object({
  runId: z.string(),
  seq: z.number().int().nonnegative(),
  kind: z.enum(traceKinds),
  at: z.string(),
  durationMs: z.number().nonnegative().nullish(),
  summary: z.string(),
  data: z.record(z.string(), z.unknown()).default({}),
});
export type TraceEvent = z.infer<typeof traceEventSchema>;

export const traceKindLabels: Record<TraceKind, string> = {
  model_call: "Model",
  tool_call: "Tool call",
  tool_result: "Tool result",
  plan: "Plan",
  verify: "Check",
  repair: "Repair",
  draft: "Draft",
  final: "Finished",
  error: "Error",
};

const statusLabels: Record<string, string> = {
  answered: "Answered",
  drafted: "Drafted changes",
  needs_clarification: "Asked a question",
  cannot_complete: "Could not complete",
  error: "Crashed",
  failed: "Failed",
  cancelled: "Cancelled",
  running: "Running",
};

export function statusLabel(status: string) {
  return statusLabels[status] ?? status.replace(/_/g, " ");
}

function durationLabel(ms: number | null | undefined) {
  if (ms === null || ms === undefined) return null;
  if (ms < 1) return "<1 ms";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

/** A short, single-line rendering of any JSON value, so details never flood the panel. */
export function brief(value: unknown, limit = 160): string {
  let text: string;
  if (typeof value === "string") text = value;
  else {
    try {
      text = JSON.stringify(value) ?? String(value);
    } catch {
      text = String(value);
    }
  }
  return text.length > limit ? `${text.slice(0, limit - 1)}…` : text;
}

/**
 * The details worth reading for a step, as label/value pairs. Known fields come first with plain
 * names; any other field follows so nothing recorded is hidden.
 */
export function stepDetails(event: TraceEvent): [string, string][] {
  const data = event.data;
  const known: [string, string][] = [
    ["tool", "Tool"],
    ["model", "Model"],
    ["args", "Arguments"],
    ["arguments", "Arguments"],
    ["ok", "Succeeded"],
    ["errorCode", "Error code"],
    ["error", "Error"],
    ["records", "Records returned"],
    ["result", "Result"],
    ["op", "Change"],
    ["target", "Target"],
    ["status", "Status"],
    ["inputTokens", "Input tokens"],
    ["outputTokens", "Output tokens"],
    ["injected", "Injected fault"],
  ];
  const seen = new Set<string>();
  const rows: [string, string][] = [];
  for (const [key, label] of known) {
    if (!(key in data) || data[key] === null || data[key] === undefined)
      continue;
    seen.add(key);
    if (key === "injected" && data[key] === false) continue;
    rows.push([label, brief(data[key])]);
  }
  for (const [key, value] of Object.entries(data)) {
    if (seen.has(key) || value === null || value === undefined) continue;
    if (key === "debug") continue;
    rows.push([key, brief(value)]);
  }
  return rows;
}

function isFailure(event: TraceEvent) {
  return (
    event.kind === "error" ||
    (event.kind === "tool_result" && event.data.ok === false)
  );
}

export function AgentTrace({
  events,
  status,
  live = false,
  emptyText = "No steps were recorded for this run.",
  compact = false,
}: {
  events: TraceEvent[];
  /** The run's final status, shown as a badge above the steps. */
  status?: string | null;
  /** True while a run is in progress: shows a working row and keeps the newest step in view. */
  live?: boolean;
  emptyText?: string;
  /** Tighter rows for a narrow side panel. */
  compact?: boolean;
}) {
  const ordered = [...events].sort((left, right) => left.seq - right.seq);
  const endRef = useRef<HTMLLIElement>(null);

  useEffect(() => {
    if (live) endRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [live, events.length]);

  return (
    <div className={`agent-trace${compact ? " compact" : ""}`}>
      {status && (
        <p className={`trace-status status-${status}`}>
          <span className="trace-status-dot" aria-hidden="true" />
          {statusLabel(status)}
        </p>
      )}
      {ordered.length === 0 && !live ? (
        <p className="trace-empty">{emptyText}</p>
      ) : (
        <ol className="trace-steps" aria-label="Run steps">
          {ordered.map((event) => {
            const details = stepDetails(event);
            const duration = durationLabel(event.durationMs);
            return (
              <li
                key={`${event.runId}-${event.seq}`}
                className={`trace-step kind-${event.kind}${isFailure(event) ? " failed" : ""}`}
              >
                <span className="trace-seq" aria-hidden="true">
                  {event.seq}
                </span>
                <div className="trace-body">
                  <div className="trace-line">
                    <span className="trace-kind">
                      {traceKindLabels[event.kind]}
                    </span>
                    <span className="trace-summary">{event.summary}</span>
                    {duration && (
                      <span className="trace-duration">{duration}</span>
                    )}
                  </div>
                  {details.length > 0 && (
                    <details className="trace-details">
                      <summary>Details</summary>
                      <dl>
                        {details.map(([label, value], index) => (
                          <div key={`${label}-${index}`}>
                            <dt>{label}</dt>
                            <dd>{value}</dd>
                          </div>
                        ))}
                      </dl>
                    </details>
                  )}
                </div>
              </li>
            );
          })}
          {live && (
            <li ref={endRef} className="trace-step working" aria-live="polite">
              <span className="trace-seq" aria-hidden="true">
                …
              </span>
              <div className="trace-body">
                <span className="trace-summary">Working…</span>
              </div>
            </li>
          )}
        </ol>
      )}
    </div>
  );
}

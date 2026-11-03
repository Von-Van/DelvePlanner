/**
 * Model & Evaluation: the measured side of the assistant, inside the app. Lists benchmark and
 * regression runs, opens a run to show every case, and opens a case to show its step-by-step trace.
 * Read-only: it never starts a benchmark and never touches planner data.
 */
import { useCallback, useEffect, useState } from "react";
import { AgentTrace } from "./AgentTrace";
import {
  compareRuns,
  dateLabel,
  defaultComparison,
  earlierRuns,
  evaluationApi,
  EvaluationApi,
  EvaluationCase,
  EvaluationOverview,
  EvaluationRun,
  EvaluationRunDetail,
  EvaluationTrace,
  latencyLabel,
  percentLabel,
  settingsDiffer,
  shortDigest,
  skillBreakdown,
  skillOf,
  sourceLabels,
} from "./evaluationApi";
import { Glyph, LoadingLine } from "./Geometry";
import { useHeadingFocus } from "./useHeadingFocus";
import "./Evaluation.css";

type Loadable<T> =
  | { state: "loading" }
  | { state: "ready"; value: T }
  | { state: "failed"; message: string };

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

export function EvaluationView({
  headingRef,
  focusToken,
  api = evaluationApi,
}: {
  headingRef: React.RefObject<HTMLHeadingElement>;
  focusToken: number;
  /** Replaced in tests; the app always uses the real read-only commands. */
  api?: EvaluationApi;
}) {
  const [overview, setOverview] = useState<Loadable<EvaluationOverview>>({
    state: "loading",
  });
  const [runId, setRunId] = useState<string | null>(null);
  const [detail, setDetail] = useState<Loadable<EvaluationRunDetail> | null>(
    null,
  );
  const [caseKey, setCaseKey] = useState<string | null>(null);
  const [trace, setTrace] = useState<Loadable<EvaluationTrace> | null>(null);
  // undefined: compare with the default earlier run; null: compare with nothing.
  const [compareChoice, setCompareChoice] = useState<string | null | undefined>(
    undefined,
  );
  const [previous, setPrevious] =
    useState<Loadable<EvaluationRunDetail> | null>(null);

  useHeadingFocus(headingRef, focusToken);

  const refresh = useCallback(async () => {
    setOverview({ state: "loading" });
    try {
      setOverview({ state: "ready", value: await api.listRuns() });
    } catch (cause) {
      setOverview({ state: "failed", message: messageOf(cause) });
    }
  }, [api]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (runId === null) {
      setDetail(null);
      return;
    }
    let current = true;
    setDetail({ state: "loading" });
    api.getRun(runId).then(
      (value) => current && setDetail({ state: "ready", value }),
      (cause) =>
        current && setDetail({ state: "failed", message: messageOf(cause) }),
    );
    return () => {
      current = false;
    };
  }, [api, runId]);

  useEffect(() => {
    if (runId === null || caseKey === null) {
      setTrace(null);
      return;
    }
    let current = true;
    setTrace({ state: "loading" });
    api.getTrace(runId, caseKey).then(
      (value) => current && setTrace({ state: "ready", value }),
      (cause) =>
        current && setTrace({ state: "failed", message: messageOf(cause) }),
    );
    return () => {
      current = false;
    };
  }, [api, runId, caseKey]);

  const openRun = (id: string | null) => {
    setCaseKey(null);
    setCompareChoice(undefined);
    setRunId(id);
  };

  const run = detail?.state === "ready" ? detail.value : null;
  const candidates =
    run && overview.state === "ready"
      ? earlierRuns(overview.value.runs, run.run)
      : [];
  const compareId =
    compareChoice === undefined
      ? run
        ? (defaultComparison(candidates, run.run)?.id ?? null)
        : null
      : compareChoice;

  useEffect(() => {
    if (compareId === null) {
      setPrevious(null);
      return;
    }
    let current = true;
    setPrevious({ state: "loading" });
    api.getRun(compareId).then(
      (value) => current && setPrevious({ state: "ready", value }),
      (cause) =>
        current && setPrevious({ state: "failed", message: messageOf(cause) }),
    );
    return () => {
      current = false;
    };
  }, [api, compareId]);
  const selectedCase =
    run && caseKey ? run.cases.find((c) => c.key === caseKey) : undefined;

  return (
    <div className="evaluation-page">
      <header className="topbar">
        <div className="date-heading">
          <p>MEASURED ON THIS DEVICE</p>
          <h1 ref={headingRef} tabIndex={-1}>
            Model &amp; Evaluation
          </h1>
        </div>
        <button
          className="secondary-button"
          onClick={() => {
            openRun(null);
            void refresh();
          }}
        >
          <Glyph>↻</Glyph> Reload results
        </button>
      </header>
      <p className="page-intro">
        How the assistant measures up. Each run below is a set of test requests
        the assistant answered on a fixed, made-up planner, scored
        automatically. Safety failures are counted on their own and never
        averaged into the success rate. This view only reads saved results; it
        never runs tests or touches your plans.
      </p>

      <nav className="evaluation-trail" aria-label="Where you are">
        <button
          className="text-button"
          disabled={runId === null}
          onClick={() => openRun(null)}
        >
          All runs
        </button>
        {runId !== null && (
          <>
            <span aria-hidden="true">›</span>
            <button
              className="text-button"
              disabled={caseKey === null}
              onClick={() => setCaseKey(null)}
            >
              {run?.run.label ?? "Run"}
            </button>
          </>
        )}
        {caseKey !== null && (
          <>
            <span aria-hidden="true">›</span>
            <span className="evaluation-trail-current">
              {selectedCase?.caseId ?? caseKey}
            </span>
          </>
        )}
      </nav>

      {runId === null ? (
        overview.state === "loading" ? (
          <LoadingLine label="Reading results" />
        ) : overview.state === "failed" ? (
          <p className="evaluation-error" role="alert">
            {overview.message}
          </p>
        ) : (
          <EvaluationRunList overview={overview.value} onOpen={openRun} />
        )
      ) : detail === null || detail.state === "loading" ? (
        <LoadingLine label="Opening run" />
      ) : detail.state === "failed" ? (
        <p className="evaluation-error" role="alert">
          {detail.message}
        </p>
      ) : caseKey === null || !selectedCase ? (
        <EvaluationRunDetailView
          detail={detail.value}
          onOpenCase={setCaseKey}
          comparison={{
            candidates,
            selectedId: compareId,
            onSelect: setCompareChoice,
            previous,
          }}
        />
      ) : (
        <EvaluationCaseView
          run={detail.value.run}
          testCase={selectedCase}
          trace={trace}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Runs

export function EvaluationEmpty({ resultsFolder }: { resultsFolder: string }) {
  return (
    <section className="evaluation-empty">
      <h2>No test results yet</h2>
      <p>
        Results appear here once a benchmark or regression run has been saved to
        this device. Running one needs the source code, a terminal, and the
        local model; it uses made-up planner data, never yours.
      </p>
      <ol>
        <li>
          <strong>Assistant check.</strong> From the repository, with the local
          model running, run <code>python -m agent_bench check</code>. It tests
          the assistant the way the app runs it and saves the results straight
          into this device&rsquo;s results folder.
        </li>
        <li>
          <strong>Assistant benchmark.</strong> Run{" "}
          <code>python -m agent_bench run --out &lt;folder&gt;/my-run</code>, or
          copy a run folder from <code>agent_bench/results/</code>.
        </li>
        <li>
          <strong>Regression set.</strong> Run <code>npm run eval</code>, then
          copy <code>eval/results/latest.json</code>.
        </li>
        <li>
          Put the run folder or file in this device&rsquo;s results folder, then
          choose <em>Reload results</em>:
          <code className="evaluation-path">{resultsFolder}</code>
        </li>
      </ol>
    </section>
  );
}

export function EvaluationRunList({
  overview,
  onOpen,
}: {
  overview: EvaluationOverview;
  onOpen: (runId: string) => void;
}) {
  return (
    <>
      {overview.problems.length > 0 && (
        <section className="evaluation-problems" role="status">
          <strong>Some results couldn&rsquo;t be read</strong>
          <ul>
            {overview.problems.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        </section>
      )}
      {overview.runs.length === 0 ? (
        <EvaluationEmpty resultsFolder={overview.resultsFolder} />
      ) : (
        <ul className="evaluation-runs" aria-label="Test runs">
          {overview.runs.map((run) => (
            <li key={run.id}>
              <button className="evaluation-run" onClick={() => onOpen(run.id)}>
                <RunHeading run={run} />
                <RunMetrics run={run} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function RunHeading({ run }: { run: EvaluationRun }) {
  const digest = shortDigest(run.modelDigest);
  return (
    <div className="evaluation-run-heading">
      <span className="evaluation-arch" title="Assistant design">
        {run.architecture ?? "—"}
      </span>
      <div>
        <strong>{run.label}</strong>
        <p>
          {sourceLabels[run.source]} · {dateLabel(run.startedAt)}
        </p>
        <p className="evaluation-model">
          {run.modelName ?? "No model recorded"}
          {digest && <span title={run.modelDigest ?? ""}> @ {digest}</span>}
          {run.hardware && <> · {run.hardware}</>}
        </p>
      </div>
    </div>
  );
}

function RunMetrics({ run }: { run: EvaluationRun }) {
  return (
    <dl className="evaluation-metrics">
      <div>
        <dt>Success</dt>
        <dd>
          {percentLabel(run.successRate)}
          <small>
            {run.passedAttempts}/{run.attempts}
          </small>
        </dd>
      </div>
      <div
        className={`evaluation-safety${run.safetyFailures > 0 ? " flagged" : ""}`}
      >
        <dt>Safety failures</dt>
        <dd>{run.safetyFailures}</dd>
      </div>
      <div>
        <dt>Latency p50</dt>
        <dd>{latencyLabel(run.latencyP50Ms)}</dd>
      </div>
      <div>
        <dt>Latency p95</dt>
        <dd>{latencyLabel(run.latencyP95Ms)}</dd>
      </div>
      <div>
        <dt>Cases</dt>
        <dd>
          {run.cases}
          {run.repeats > 1 && <small>×{run.repeats}</small>}
        </dd>
      </div>
    </dl>
  );
}

// ---------------------------------------------------------------------------------------------
// One run

type CaseFilter = "all" | "failed" | "unsafe";

export type ComparisonState = {
  /** Earlier benchmark runs, newest first. */
  candidates: EvaluationRun[];
  selectedId: string | null;
  onSelect: (runId: string | null) => void;
  previous: Loadable<EvaluationRunDetail> | null;
};

export function EvaluationRunDetailView({
  detail,
  onOpenCase,
  initialFilter = "all",
  comparison,
}: {
  detail: EvaluationRunDetail;
  onOpenCase: (caseKey: string) => void;
  initialFilter?: CaseFilter;
  comparison?: ComparisonState;
}) {
  const [filter, setFilter] = useState<CaseFilter>(initialFilter);
  const { run, cases } = detail;
  const shown = cases.filter((c) =>
    filter === "failed"
      ? !c.success
      : filter === "unsafe"
        ? c.safe === false
        : true,
  );
  const showRepeat = run.repeats > 1;

  return (
    <section className="evaluation-detail" aria-label={run.label}>
      <div className="evaluation-run static">
        <RunHeading run={run} />
        <RunMetrics run={run} />
      </div>
      {run.safetyFailures > 0 && (
        <p className="evaluation-safety-banner" role="alert">
          {run.safetyFailures === 1
            ? "1 case run broke a safety rule."
            : `${run.safetyFailures} case runs broke a safety rule.`}{" "}
          A safety failure blocks shipping no matter how good the success rate
          is.
        </p>
      )}
      {detail.note && <p className="evaluation-note">{detail.note}</p>}

      {run.source === "benchmark" && <SkillBreakdown cases={cases} />}
      {comparison && comparison.candidates.length > 0 && (
        <RunComparisonView run={run} cases={cases} comparison={comparison} />
      )}

      <h3 className="evaluation-section-heading">Every case</h3>
      <div className="evaluation-filter" role="group" aria-label="Show">
        {(
          [
            ["all", `All (${cases.length})`],
            ["failed", `Failed (${cases.filter((c) => !c.success).length})`],
            [
              "unsafe",
              `Safety (${cases.filter((c) => c.safe === false).length})`,
            ],
          ] as [CaseFilter, string][]
        ).map(([value, label]) => (
          <button
            key={value}
            className={`evaluation-segment${filter === value ? " active" : ""}`}
            aria-pressed={filter === value}
            onClick={() => setFilter(value)}
          >
            {label}
          </button>
        ))}
      </div>

      {shown.length === 0 ? (
        <p className="evaluation-note">No cases match.</p>
      ) : (
        <table className="evaluation-cases">
          <thead>
            <tr>
              <th scope="col">Case</th>
              <th scope="col">Skill</th>
              <th scope="col">Result</th>
              <th scope="col">Safety</th>
              <th scope="col">Time</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((c) => (
              <CaseRow
                key={c.key}
                testCase={c}
                showRepeat={showRepeat}
                onOpen={() => onOpenCase(c.key)}
              />
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

function SkillBreakdown({ cases }: { cases: EvaluationCase[] }) {
  const lines = skillBreakdown(cases);
  if (lines.length === 0) return null;
  return (
    <section className="evaluation-skills" aria-label="By skill">
      <h3 className="evaluation-section-heading">By skill</h3>
      <table className="evaluation-cases">
        <thead>
          <tr>
            <th scope="col">Skill</th>
            <th scope="col">Passed</th>
            <th scope="col">Safety</th>
          </tr>
        </thead>
        <tbody>
          {lines.map((line) => (
            <tr
              key={line.skill}
              className={line.unsafe > 0 ? "unsafe" : undefined}
            >
              <th scope="row">{line.skill}</th>
              <td>
                <span
                  className={`evaluation-result ${line.passed === line.runs ? "pass" : "fail"}`}
                >
                  {line.passed}/{line.runs}
                </span>
              </td>
              <td>
                {line.unsafe > 0 ? (
                  <span className="evaluation-result unsafe">
                    {line.unsafe} unsafe
                  </span>
                ) : (
                  <span className="evaluation-muted">OK</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function CaseList({ label, ids }: { label: string; ids: string[] }) {
  return (
    <div className="evaluation-change">
      <dt>
        {label} <small>{ids.length}</small>
      </dt>
      <dd>{ids.length === 0 ? "None" : ids.join(", ")}</dd>
    </div>
  );
}

function RunComparisonView({
  run,
  cases,
  comparison,
}: {
  run: EvaluationRun;
  cases: EvaluationCase[];
  comparison: ComparisonState;
}) {
  const { candidates, selectedId, onSelect, previous } = comparison;
  const earlier = candidates.find((other) => other.id === selectedId) ?? null;
  return (
    <section className="evaluation-compare" aria-label="Since an earlier run">
      <div className="evaluation-compare-heading">
        <h3 className="evaluation-section-heading">Since an earlier run</h3>
        <label>
          Compare with{" "}
          <select
            value={selectedId ?? ""}
            onChange={(event) => onSelect(event.target.value || null)}
          >
            <option value="">Nothing</option>
            {candidates.map((other) => (
              <option key={other.id} value={other.id}>
                {other.label} · {dateLabel(other.startedAt)}
              </option>
            ))}
          </select>
        </label>
      </div>
      {selectedId === null || previous === null ? (
        <p className="evaluation-note">
          Pick an earlier run to see which cases got better or worse.
        </p>
      ) : previous.state === "loading" ? (
        <LoadingLine label="Opening the earlier run" />
      ) : previous.state === "failed" ? (
        <p className="evaluation-error" role="alert">
          {previous.message}
        </p>
      ) : (
        <ComparisonResult
          run={run}
          earlier={earlier ?? previous.value.run}
          cases={cases}
          previousCases={previous.value.cases}
        />
      )}
    </section>
  );
}

export function ComparisonResult({
  run,
  earlier,
  cases,
  previousCases,
}: {
  run: EvaluationRun;
  earlier: EvaluationRun;
  cases: EvaluationCase[];
  previousCases: EvaluationCase[];
}) {
  const result = compareRuns(cases, previousCases);
  return (
    <>
      {settingsDiffer(run, earlier) && (
        <p className="evaluation-note">
          The assistant&rsquo;s setup or the model changed between these runs (
          {earlier.label}
          {earlier.modelName ? `, ${earlier.modelName}` : ""} then; {run.label}
          {run.modelName ? `, ${run.modelName}` : ""} now). Read the difference
          with that in mind.
        </p>
      )}
      <dl className="evaluation-changes">
        <CaseList label="Better now" ids={result.better} />
        <CaseList label="Worse now" ids={result.worse} />
        <CaseList label="Still not passing" ids={result.stillFailing} />
        {result.newCases.length > 0 && (
          <CaseList label="New this time" ids={result.newCases} />
        )}
        {result.droppedCases.length > 0 && (
          <CaseList label="No longer tested" ids={result.droppedCases} />
        )}
      </dl>
    </>
  );
}

function CaseRow({
  testCase,
  showRepeat,
  onOpen,
}: {
  testCase: EvaluationCase;
  showRepeat: boolean;
  onOpen: () => void;
}) {
  return (
    <tr className={testCase.safe === false ? "unsafe" : undefined}>
      <th scope="row">
        <button className="text-button evaluation-case-link" onClick={onOpen}>
          {testCase.caseId}
          {showRepeat && <small> run {testCase.repeat}</small>}
        </button>
        {testCase.tags.length > 0 && (
          <span className="evaluation-tags">
            {testCase.tags.map((tag) => (
              <span key={tag} className="evaluation-tag">
                {tag}
              </span>
            ))}
          </span>
        )}
      </th>
      <td title={testCase.family ?? undefined}>{skillOf(testCase) ?? "—"}</td>
      <td>
        <span
          className={`evaluation-result ${testCase.success ? "pass" : "fail"}`}
        >
          {testCase.success ? "Pass" : "Fail"}
        </span>
      </td>
      <td>
        {testCase.safe === false ? (
          <span className="evaluation-result unsafe">Unsafe</span>
        ) : testCase.safe === null ? (
          <span
            className="evaluation-muted"
            title="This report counts safety failures for the whole run, not per case."
          >
            Not recorded
          </span>
        ) : (
          <span className="evaluation-muted">OK</span>
        )}
      </td>
      <td>{latencyLabel(testCase.latencyMs)}</td>
    </tr>
  );
}

// ---------------------------------------------------------------------------------------------
// One case

export function EvaluationCaseView({
  run,
  testCase,
  trace,
}: {
  run: EvaluationRun;
  testCase: EvaluationCase;
  trace: Loadable<EvaluationTrace> | null;
}) {
  return (
    <section className="evaluation-case" aria-label={testCase.caseId}>
      <header className="evaluation-case-header">
        <h2>{testCase.caseId}</h2>
        <p>
          {[
            testCase.skill,
            testCase.family,
            testCase.split,
            run.repeats > 1 ? `run ${testCase.repeat}` : null,
            latencyLabel(testCase.latencyMs),
          ]
            .filter(Boolean)
            .join(" · ")}
        </p>
        <p className="evaluation-verdicts">
          <span
            className={`evaluation-result ${testCase.success ? "pass" : "fail"}`}
          >
            {testCase.success ? "Pass" : "Fail"}
          </span>
          {testCase.safe === false && (
            <span className="evaluation-result unsafe">Unsafe</span>
          )}
          {testCase.tags.map((tag) => (
            <span key={tag} className="evaluation-tag">
              {tag}
            </span>
          ))}
        </p>
      </header>

      {testCase.safetyViolations.length > 0 && (
        <div className="evaluation-safety-banner" role="alert">
          <strong>Safety rules broken</strong>
          <ul>
            {testCase.safetyViolations.map((v, index) => (
              <li key={`${v.rule}-${index}`}>
                <code>{v.rule}</code> {v.detail}
              </li>
            ))}
          </ul>
        </div>
      )}

      {testCase.failedChecks.length > 0 && (
        <div className="evaluation-checks">
          <strong>Checks that failed</strong>
          <ul>
            {testCase.failedChecks.map((check, index) => (
              <li key={`${check.name}-${index}`}>
                <code>{check.name}</code> {check.detail}
              </li>
            ))}
          </ul>
        </div>
      )}

      <h3>Step by step</h3>
      {!testCase.hasTrace ? (
        <p className="evaluation-note">
          {run.source === "regression"
            ? "Regression reports don't keep step-by-step traces."
            : "No trace was saved for this case."}
        </p>
      ) : trace === null || trace.state === "loading" ? (
        <LoadingLine label="Loading steps" />
      ) : trace.state === "failed" ? (
        <p className="evaluation-error" role="alert">
          {trace.message}
        </p>
      ) : (
        <AgentTrace
          events={trace.value.events}
          status={trace.value.status ?? testCase.status}
        />
      )}
    </section>
  );
}

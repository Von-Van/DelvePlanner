import { FormEvent, useCallback, useEffect, useMemo, useState } from "react";
import { AgentTrace, TraceEvent } from "./AgentTrace";
import {
  AgentRun,
  AssistantStatus,
  api,
  DelvePlannerError,
  Proposal,
} from "./api";
import {
  fallbackReason,
  newRunKey,
  runMeasures,
  runStatusLabels,
  runSummaryLine,
  runTone,
  withRun,
  withStep,
} from "./AssistantRuns";
import { localTimeZone } from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import {
  acceptedOperations,
  describeProposal,
  OperationTone,
} from "./proposals";
import {
  commandAriaShortcut,
  commandShortcutText,
  submitOnCommandEnter,
} from "./shortcuts";

/**
 * The new assistant: the local agent reads the planner through read-only tools, answers or
 * proposes changes, and every run shows how it got there (each step) and what it cost (time,
 * model and tool calls, tokens, the exact model). Nothing changes until the user applies it.
 */
export function AssistantAgentPanel({
  onChanged,
  onMessage,
  onFallback,
}: {
  /** Called after applied changes, so the rest of the app reloads. */
  onChanged: () => void;
  onMessage: (message: string) => void;
  /**
   * Hands the request to the old planner when the agent can't run it: the agent isn't in this
   * build, or the run failed (no agent process, model unreachable, a crash).
   */
  onFallback?: (request: string, reason: string) => void;
}) {
  const [status, setStatus] = useState<AssistantStatus | null>(null);
  const [goal, setGoal] = useState("");
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [live, setLive] = useState<{ key: string; steps: TraceEvent[] } | null>(
    null,
  );
  const [cancelling, setCancelling] = useState(false);

  useEffect(() => {
    api
      .assistantStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
    api
      .listAssistantRuns()
      .then((list) => {
        setRuns(list);
        setSelectedKey(list[0]?.runKey ?? null);
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    const unlisten = api.onAssistantStep(({ runKey, event }) =>
      setLive((current) =>
        current && current.key === runKey
          ? { ...current, steps: withStep(current.steps, event) }
          : current,
      ),
    );
    return () => {
      unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, []);

  const running = live !== null;
  const selected = runs.find((run) => run.runKey === selectedKey) ?? null;

  async function submit(event: FormEvent) {
    event.preventDefault();
    const request = goal.trim();
    if (!request || running) return;
    if (status && !status.available) {
      if (onFallback) {
        setGoal("");
        onFallback(request, "it isn't included in this build");
      }
      return;
    }
    const key = newRunKey();
    setLive({ key, steps: [] });
    setSelectedKey(key);
    try {
      const run = await api.runAssistant(request, localTimeZone, key);
      setRuns((current) => withRun(current, run));
      setGoal("");
      // The failed run stays in the list with its steps; the planner answers the request.
      const reason = fallbackReason(run);
      if (reason && onFallback) onFallback(request, reason);
    } catch (cause) {
      setSelectedKey(runs[0]?.runKey ?? null);
      if (onFallback) onFallback(request, messageOf(cause));
      else onMessage(messageOf(cause));
    } finally {
      setLive(null);
      setCancelling(false);
    }
  }

  async function stop() {
    setCancelling(true);
    try {
      await api.cancelAssistantRun();
    } catch (cause) {
      onMessage(messageOf(cause));
    }
  }

  const refreshRuns = useCallback(async () => {
    try {
      setRuns(await api.listAssistantRuns());
    } catch (cause) {
      onMessage(messageOf(cause));
    }
  }, [onMessage]);

  const available = status?.available ?? false;

  return (
    <section className="planner-card agent-panel" aria-label="Local agent">
      <div className="planner-heading">
        <div className="planner-orb" aria-hidden="true">
          <i />
        </div>
        <div>
          <p>LOCAL AGENT · {status?.architecture ?? "A1"}</p>
          <h2>Ask, and see how it works it out.</h2>
        </div>
      </div>
      <div className={`model-state ${available ? "ready" : "not-ready"}`}>
        <span />
        <div>
          <strong>
            {available
              ? "Agent ready"
              : onFallback
                ? "Agent not available · the planner will answer"
                : "Agent not available in this build"}
          </strong>
          <p>
            {status?.detail ??
              "Checking whether the agent is included in this build…"}
          </p>
        </div>
      </div>

      <form onSubmit={submit} className="command-form">
        <textarea
          maxLength={2000}
          value={goal}
          onChange={(event) => setGoal(event.target.value)}
          onKeyDown={submitOnCommandEnter}
          aria-keyshortcuts={commandAriaShortcut("Enter")}
          aria-label="Ask the agent"
          placeholder="“What's overdue in the launch project?” or “Move the budget draft to Thursday.”"
          disabled={running || (!available && !onFallback)}
        />
        <div>
          <span className="assistant-chips">
            <span className="assistant-chip read">Reads your planner</span>
            <span className="assistant-chip draft">
              Drafts only · you approve every change
            </span>
          </span>
          {running ? (
            <button
              type="button"
              className="secondary-button small"
              onClick={stop}
              disabled={cancelling}
            >
              {cancelling ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <button
              className="primary-button small"
              disabled={!goal.trim() || (!available && !onFallback)}
              title={`Ask (${commandShortcutText("↩")})`}
            >
              <Mark size={6} filled />
              Ask
            </button>
          )}
        </div>
      </form>

      <div aria-live="polite">
        {live && (
          <div className="agent-run">
            {goal.trim() && <p className="chat-request">{goal.trim()}</p>}
            <div className="chat-reply">
              <span className="chat-avatar" aria-hidden="true">
                D
              </span>
              <div className="chat-reply-body">
                <p className="proposal-kicker">WORKING · STEP BY STEP</p>
                <AgentTrace events={live.steps} live compact />
              </div>
            </div>
          </div>
        )}
        {!live && selected && (
          <RunView
            key={selected.runKey}
            run={selected}
            onChanged={async () => {
              await refreshRuns();
              onChanged();
            }}
            onMessage={onMessage}
            onRefresh={refreshRuns}
          />
        )}
      </div>

      {runs.length > 0 && (
        <RecentRuns
          runs={runs}
          selectedKey={live ? null : selectedKey}
          onSelect={setSelectedKey}
        />
      )}

      <p className="planner-footnote">
        <span aria-hidden="true">◌</span> Runs on this computer only. Every run
        is measured the same way the test suite measures the agent.
      </p>
    </section>
  );
}

function RunView({
  run,
  onChanged,
  onMessage,
  onRefresh,
}: {
  run: AgentRun;
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
  onRefresh: () => Promise<void>;
}) {
  const [showSteps, setShowSteps] = useState(false);
  const measures = useMemo(() => runMeasures(run), [run]);
  return (
    <div className="agent-run">
      <p className="chat-request">{run.goal}</p>
      <div className="chat-reply">
        <span className="chat-avatar" aria-hidden="true">
          D
        </span>
        <div className="chat-reply-body">
          <div className="agent-run-head">
            <span className={`agent-status ${runTone(run)}`}>
              {run.cancelled ? "Stopped" : runStatusLabels[run.status]}
            </span>
          </div>

          {run.answer && run.status !== "drafted" && (
            <p className="agent-answer">{run.answer}</p>
          )}
          {run.status === "needs_clarification" && (
            <p className="agent-note">
              Add the missing detail to your request and ask again.
            </p>
          )}
          {run.error && <p className="agent-note problem">{run.error}</p>}

          {run.draftCheck && run.draftCheck.problems.length > 0 && (
            <div className="agent-note problem">
              The agent proposed changes the app couldn't verify, so none are
              offered:
              <ul>
                {run.draftCheck.problems.map((problem) => (
                  <li key={problem}>{problem}</li>
                ))}
              </ul>
            </div>
          )}

          {run.proposal && (
            <ChangeReview
              proposal={run.proposal}
              onChanged={onChanged}
              onMessage={onMessage}
              onRefresh={onRefresh}
            />
          )}
          {run.review.state === "applied" && (
            <p className="agent-note good">
              Applied {run.review.changes} change
              {run.review.changes === 1 ? "" : "s"}.
            </p>
          )}
          {run.review.state === "discarded" && (
            <p className="agent-note">Proposed changes discarded.</p>
          )}
          {run.review.state === "expired" && (
            <p className="agent-note">
              These proposed changes expired. Ask again for fresh ones.
            </p>
          )}
          {run.review.state === "failed" && (
            <p className="agent-note problem">{run.review.message}</p>
          )}

          <dl className="agent-measures" aria-label="Run measurements">
            {measures.map((measure) => (
              <div key={measure.label} title={measure.hint}>
                <dt>{measure.label}</dt>
                <dd>{measure.value}</dd>
              </div>
            ))}
          </dl>

          <button
            className="text-button"
            onClick={() => setShowSteps((open) => !open)}
            aria-expanded={showSteps}
          >
            {showSteps ? "Hide" : "Show"} the {run.trace.length} steps{" "}
            <Glyph>{showSteps ? "↑" : "↓"}</Glyph>
          </button>
          {showSteps && (
            <AgentTrace
              events={run.trace}
              status={run.cancelled ? "cancelled" : run.status}
              compact
              emptyText="This run stopped before its first step."
            />
          )}
        </div>
      </div>
    </div>
  );
}

function ChangeReview({
  proposal,
  onChanged,
  onMessage,
  onRefresh,
}: {
  proposal: Proposal;
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
  onRefresh: () => Promise<void>;
}) {
  const previews = useMemo(() => describeProposal(proposal), [proposal]);
  const [rejected, setRejected] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const accepted = acceptedOperations(previews, rejected);
  const kept = new Set(accepted);

  function toggle(id: string) {
    setRejected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function apply() {
    setBusy(true);
    try {
      await api.applyAssistantChanges(proposal.proposalId, accepted);
      await onChanged();
    } catch (cause) {
      onMessage(messageOf(cause));
      await onRefresh();
    } finally {
      setBusy(false);
    }
  }

  async function discard() {
    setBusy(true);
    try {
      await api.discardAssistantChanges(proposal.proposalId);
    } catch (cause) {
      onMessage(messageOf(cause));
    } finally {
      await onRefresh();
      setBusy(false);
    }
  }

  const rejectedCount = previews.length - accepted.length;
  return (
    <section className="change-review" aria-label="Changes to review">
      <div className="change-review-head">
        <h3>
          {previews.length} change{previews.length === 1 ? "" : "s"} to review
        </h3>
        <span>Nothing is saved until you apply.</span>
      </div>
      <p className="change-review-summary">{proposal.summary}</p>
      <ul>
        {previews.map((preview) => {
          const isKept = kept.has(preview.id);
          return (
            <li key={preview.id} className={isKept ? "kept" : "rejected"}>
              <i
                className={`operation-mark ${preview.tone}`}
                aria-hidden="true"
              />
              <div className="change-text">
                <span className="change-op">{toneLabels[preview.tone]}</span>
                <strong>{preview.title}</strong>
                {preview.details.length > 0 && (
                  <small>{preview.details.join(" · ")}</small>
                )}
                {preview.reason && (
                  <small className="change-reason">{preview.reason}</small>
                )}
              </div>
              <div
                className="change-choice"
                role="group"
                aria-label={`Accept or reject: ${preview.title}`}
              >
                <button
                  type="button"
                  className="change-accept"
                  aria-pressed={isKept}
                  onClick={() => isKept || toggle(preview.id)}
                  disabled={busy}
                >
                  {isKept ? "Accepted" : "Accept"}
                </button>
                <button
                  type="button"
                  className="change-reject"
                  aria-pressed={!isKept}
                  onClick={() => isKept && toggle(preview.id)}
                  disabled={busy}
                >
                  {isKept ? "Reject" : "Rejected"}
                </button>
              </div>
            </li>
          );
        })}
      </ul>
      <div className="change-review-foot">
        <span>
          {accepted.length} accepted · {rejectedCount} rejected
        </span>
        <button className="secondary-button" onClick={discard} disabled={busy}>
          Discard all
        </button>
        <button
          className="primary-button small"
          onClick={apply}
          disabled={busy || accepted.length === 0}
        >
          {busy && <Spinner size={7} />}
          Apply {accepted.length} change{accepted.length === 1 ? "" : "s"}
        </button>
      </div>
    </section>
  );
}

const toneLabels: Record<OperationTone, string> = {
  create: "Add",
  change: "Change",
  move: "Move",
  delete: "Remove",
};

function RecentRuns({
  runs,
  selectedKey,
  onSelect,
}: {
  runs: AgentRun[];
  selectedKey: string | null;
  onSelect: (key: string) => void;
}) {
  return (
    <section className="agent-recent" aria-label="Recent runs">
      <p className="side-kicker">RECENT RUNS · THIS SESSION</p>
      <ul>
        {runs.map((run) => (
          <li key={run.runKey}>
            <button
              className={run.runKey === selectedKey ? "selected" : ""}
              onClick={() => onSelect(run.runKey)}
              aria-pressed={run.runKey === selectedKey}
            >
              <span className={`agent-status-dot ${runTone(run)}`} />
              <span className="agent-recent-goal">{run.goal}</span>
              <small>
                {run.cancelled ? "Stopped" : runStatusLabels[run.status]} ·{" "}
                {runSummaryLine(run)}
              </small>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function messageOf(cause: unknown) {
  if (cause instanceof DelvePlannerError) return cause.message;
  return cause instanceof Error ? cause.message : String(cause);
}

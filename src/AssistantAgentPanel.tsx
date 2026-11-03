import {
  FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { AgentTrace, TraceEvent } from "./AgentTrace";
import {
  AgentRun,
  AssistantStatus,
  api,
  Conversation,
  ConversationMessage,
  DelvePlannerError,
  Proposal,
} from "./api";
import {
  AssistantConversations,
  ConversationHistory,
  earlierMessages,
} from "./AssistantConversations";
import {
  fallbackReason,
  newRunKey,
  runMeasures,
  runStatusLabels,
  runTone,
  withRun,
  withStep,
} from "./AssistantRuns";
import { AssistantFeedback, FeedbackAbout } from "./AssistantFeedback";
import { localTimeZone } from "./date";
import { MemorySuggestions } from "./MemorySuggestions";
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
  ask,
  onAskTaken,
  open,
  onOpenTaken,
  onStarted,
  onEnded,
  onRunsChange,
  onChanged,
  onMessage,
  onFallback,
}: {
  /**
   * A request handed over from elsewhere (a heads-up's "Plan it"). Each new `key` runs it once,
   * as a new conversation, so it starts from current planner data and nothing earlier.
   */
  ask?: { request: string; key: number } | null;
  /** Called once a handed-over request has started, so it isn't run again on the next visit. */
  onAskTaken?: () => void;
  /**
   * A run to show, from the Tasks board or the approval card. Each new `key` opens it once, with
   * its conversation, so its question can be answered or its changes reviewed.
   */
  open?: { runKey: string; key: number } | null;
  /** Called once a requested run is showing, so it isn't opened again on the next visit. */
  onOpenTaken?: () => void;
  /** Called as a request starts, so the rest of the app can show it as working. */
  onStarted?: (runKey: string, request: string) => void;
  /** Called when a request is over, however it ended, even if it never produced a run. */
  onEnded?: (runKey: string) => void;
  /** Called with this session's runs whenever they change, so other screens stay current. */
  onRunsChange?: (runs: AgentRun[]) => void;
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
  // The open conversation: follow-ups join it, so the agent sees what came before.
  const [threadId, setThreadIdState] = useState<string | null>(null);
  // The same, readable after an await: a conversation that finishes loading after the person
  // has moved on must not replace what is on screen.
  const openThread = useRef<string | null>(null);
  const setThreadId = (id: string | null) => {
    openThread.current = id;
    setThreadIdState(id);
  };
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [history, setHistory] = useState<ConversationMessage[]>([]);

  // Runs are reported only once they've loaded, so an empty first render doesn't erase them.
  const runsLoaded = useRef(false);
  useEffect(() => {
    if (runsLoaded.current) onRunsChange?.(runs);
  }, [runs]);

  useEffect(() => {
    api
      .assistantStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
    api
      .listAssistantRuns()
      .then((list) => {
        runsLoaded.current = true;
        setRuns(list);
        setSelectedKey(list[0]?.runKey ?? null);
      })
      .catch(() => undefined);
    api
      .listConversations()
      .then(setConversations)
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
  const selected =
    runs.find(
      (run) =>
        run.runKey === selectedKey &&
        (run.threadId === null || run.threadId === threadId),
    ) ?? null;
  const earlier = earlierMessages(history, selected?.storedRunId ?? null);
  // Runs of the open conversation whose changes still wait for a decision, by stored run id,
  // so a proposal stays reachable after a follow-up that proposed nothing.
  const awaitingReview = new Map(
    runs
      .filter(
        (run) =>
          run.threadId === threadId &&
          run.storedRunId !== null &&
          run.review.state === "pending" &&
          run.runKey !== selected?.runKey,
      )
      .map((run) => [run.storedRunId as string, run.runKey]),
  );

  async function submit(event: FormEvent) {
    event.preventDefault();
    await runRequest(goal.trim(), threadId, true);
  }

  // A handed-over request runs once per key, in a new conversation.
  const handledAsk = useRef<number | null>(null);
  useEffect(() => {
    if (!ask || handledAsk.current === ask.key || running) return;
    handledAsk.current = ask.key;
    onAskTaken?.();
    setThreadId(null);
    setHistory([]);
    setSelectedKey(null);
    void runRequest(ask.request, null, false);
    // runRequest reads the latest state when it is called; only a new key should start a run.
  }, [ask?.key, running]);

  // A requested run opens once per key, once this session's runs have loaded. One that finished
  // elsewhere after these runs loaded (a task started on the board) is fetched once.
  const handledOpen = useRef<number | null>(null);
  const fetchedForOpen = useRef<number | null>(null);
  useEffect(() => {
    if (!open || handledOpen.current === open.key || running) return;
    const run = runs.find((item) => item.runKey === open.runKey);
    if (!run) {
      if (runsLoaded.current && fetchedForOpen.current !== open.key) {
        fetchedForOpen.current = open.key;
        void refreshRuns();
      }
      return;
    }
    handledOpen.current = open.key;
    onOpenTaken?.();
    setThreadId(run.threadId);
    setHistory([]);
    setSelectedKey(run.runKey);
    if (run.threadId) void loadConversation(run.threadId);
    // loadConversation reads the latest state; only a new key or newly loaded runs matter.
  }, [open?.key, runs, running]);

  /** Runs one request; `typed` says it came from the box, which is emptied once it is sent. */
  async function runRequest(
    request: string,
    threadId: string | null,
    typed: boolean,
  ) {
    if (!request || running) return;
    if (status && !status.available) {
      if (onFallback) {
        if (typed) setGoal("");
        onFallback(request, "it isn't included in this build");
      }
      return;
    }
    const key = newRunKey();
    setLive({ key, steps: [] });
    setSelectedKey(key);
    onStarted?.(key, request);
    try {
      const run = await api.runAssistant(request, localTimeZone, key, threadId);
      setRuns((current) => withRun(current, run));
      if (typed) setGoal("");
      if (run.threadId) {
        setThreadId(run.threadId);
        await loadConversation(run.threadId);
      }
      // The failed run stays in the list with its steps; the planner answers the request.
      const reason = fallbackReason(run);
      if (reason && onFallback) onFallback(request, reason);
    } catch (cause) {
      setSelectedKey(runs[0]?.runKey ?? null);
      // The request may have been saved before the run failed; show where it went.
      if (threadId) void loadConversation(threadId);
      else
        void api
          .listConversations()
          .then(setConversations)
          .catch(() => undefined);
      if (onFallback) onFallback(request, messageOf(cause));
      else onMessage(messageOf(cause));
    } finally {
      setLive(null);
      setCancelling(false);
      onEnded?.(key);
    }
  }

  async function loadConversation(id: string) {
    try {
      const [detail, list] = await Promise.all([
        api.getConversation(id),
        api.listConversations(),
      ]);
      setConversations(list);
      if (openThread.current === id) setHistory(detail.messages);
    } catch (cause) {
      onMessage(messageOf(cause));
    }
  }

  async function openConversation(id: string) {
    if (running || id === threadId) return;
    setThreadId(id);
    setHistory([]);
    // The newest run of this conversation from this session, if any, shows in full.
    setSelectedKey(runs.find((run) => run.threadId === id)?.runKey ?? null);
    await loadConversation(id);
  }

  function newConversation() {
    if (running) return;
    setThreadId(null);
    setHistory([]);
    setSelectedKey(null);
  }

  async function renameConversation(id: string, title: string) {
    try {
      const renamed = await api.renameConversation(id, title);
      setConversations((current) =>
        current.map((item) => (item.id === id ? renamed : item)),
      );
    } catch (cause) {
      onMessage(messageOf(cause));
    }
  }

  async function deleteConversation(conversation: Conversation) {
    if (
      !window.confirm(
        `Delete “${conversation.title}”? Its messages and the runs it started are removed from this computer.`,
      )
    )
      return;
    try {
      await api.deleteConversation(conversation.id);
      setConversations((current) =>
        current.filter((item) => item.id !== conversation.id),
      );
      setRuns((current) =>
        current.filter((run) => run.threadId !== conversation.id),
      );
      if (conversation.id === threadId) newConversation();
    } catch (cause) {
      onMessage(messageOf(cause));
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

      <ConversationHistory
        messages={earlier}
        awaitingReview={awaitingReview}
        onReview={setSelectedKey}
      />

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

      <AssistantConversations
        conversations={conversations}
        openId={threadId}
        disabled={running}
        onOpen={openConversation}
        onNew={newConversation}
        onRename={renameConversation}
        onDelete={deleteConversation}
      />

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
  const [aboutChange, setAboutChange] = useState<FeedbackAbout | null>(null);
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
              onSayWhy={
                run.storedRunId
                  ? (about) =>
                      setAboutChange((current) => ({
                        about,
                        key: (current?.key ?? 0) + 1,
                      }))
                  : undefined
              }
            />
          )}
          <MemorySuggestions
            runKey={run.runKey}
            suggestions={run.memorySuggestions}
            problems={run.draftCheck?.memoryProblems ?? []}
            onMessage={onMessage}
            onRefresh={onRefresh}
          />
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
          {!run.cancelled && run.status !== "failed" && (
            <AssistantFeedback
              runId={run.storedRunId}
              aboutChange={aboutChange}
              onMessage={onMessage}
            />
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
  onSayWhy,
}: {
  proposal: Proposal;
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
  onRefresh: () => Promise<void>;
  /** Opens feedback about one turned-down change, so the assistant can learn why. */
  onSayWhy?: (about: string) => void;
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
                {!isKept && onSayWhy && (
                  <button
                    type="button"
                    className="text-button change-say-why"
                    onClick={() =>
                      onSayWhy(
                        [preview.title, ...preview.details.slice(0, 2)].join(
                          " · ",
                        ),
                      )
                    }
                    disabled={busy}
                  >
                    Say why…
                  </button>
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

function messageOf(cause: unknown) {
  if (cause instanceof DelvePlannerError) return cause.message;
  return cause instanceof Error ? cause.message : String(cause);
}

import { FormEvent, useEffect, useState } from "react";
import { Spinner } from "./Geometry";
import {
  Feedback,
  feedbackApi,
  FeedbackApi,
  feedbackOutcome,
  FeedbackReceipt,
  lessonKinds,
  lessonProblem,
  MAX_FEEDBACK_ABOUT_CHARS,
  MAX_FEEDBACK_REASON_CHARS,
} from "./feedbackApi";
import { kindLabels, MemoryKind } from "./memoryApi";

/** A change the person wants to say something about, from the change review. */
export type FeedbackAbout = { about: string; key: number };

/**
 * "Was this helpful?" under one assistant reply (milestone M6). A "not helpful" can say why; the
 * reason is offered back as a lesson in the person's own words, which they can keep for next time
 * or leave with this conversation. Nothing is learned until they press Remember.
 */
export function AssistantFeedback({
  runId,
  aboutChange,
  onMessage,
  api = feedbackApi,
}: {
  /** The run's id in the run history; feedback needs a saved run. */
  runId: string | null;
  /** Set when the person pressed "Say why" on a turned-down change. */
  aboutChange?: FeedbackAbout | null;
  onMessage: (message: string) => void;
  api?: Pick<FeedbackApi, "record" | "list" | "learn" | "keepOneOff">;
}) {
  const [receipts, setReceipts] = useState<FeedbackReceipt[]>([]);
  const [form, setForm] = useState<{ about: string | null } | null>(null);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!runId) return;
    let live = true;
    api
      .list(runId)
      .then((list) => live && setReceipts(list))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [runId, api]);

  useEffect(() => {
    if (aboutChange) {
      setForm({ about: aboutChange.about.slice(0, MAX_FEEDBACK_ABOUT_CHARS) });
      setReason("");
    }
  }, [aboutChange]);

  if (!runId) return null;

  function replace(next: FeedbackReceipt) {
    setReceipts((current) => {
      const found = current.some((r) => r.feedback.id === next.feedback.id);
      return found
        ? current.map((r) => (r.feedback.id === next.feedback.id ? next : r))
        : [...current, next];
    });
  }

  async function send(rating: Feedback["rating"], about: string | null) {
    if (!runId) return;
    setBusy(true);
    try {
      const receipt = await api.record({
        runId,
        target: about ? "change" : "answer",
        about,
        rating,
        reason: rating === "not_helpful" ? reason.trim() || null : null,
      });
      replace(receipt);
      setForm(null);
      setReason("");
    } catch (cause) {
      onMessage(messageOf(cause));
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (form) void send("not_helpful", form.about);
  }

  // One rating per reply; turned-down changes can still say why from the change review.
  const rated = receipts.length > 0;
  return (
    <div className="assistant-feedback">
      {receipts.map((receipt) =>
        receipt.feedback.scope === "pending" && receipt.lesson ? (
          <LessonCard
            key={receipt.feedback.id}
            receipt={receipt}
            api={api}
            onSettled={replace}
            onMessage={onMessage}
          />
        ) : (
          <p
            key={receipt.feedback.id}
            className={`agent-note ${receipt.feedback.scope === "lasting" ? "good" : ""}`}
          >
            {feedbackOutcome(receipt.feedback)}
          </p>
        ),
      )}

      {form ? (
        <form className="feedback-form" onSubmit={submit}>
          <label>
            <span>
              {form.about
                ? `What was wrong with “${form.about}”?`
                : "What wasn't helpful?"}
            </span>
            <textarea
              value={reason}
              maxLength={MAX_FEEDBACK_REASON_CHARS}
              onChange={(event) => setReason(event.target.value)}
              placeholder="For example: I don't do deep work after 4 pm."
              rows={2}
              autoFocus
            />
          </label>
          <div className="proposal-actions">
            <button
              type="button"
              className="secondary-button"
              onClick={() => setForm(null)}
              disabled={busy}
            >
              Cancel
            </button>
            <button className="primary-button small" disabled={busy}>
              {busy && <Spinner size={7} />}
              Send feedback
            </button>
          </div>
        </form>
      ) : (
        !rated && (
          <div
            className="feedback-bar"
            role="group"
            aria-label="Was this reply helpful?"
          >
            <span>Was this helpful?</span>
            <button
              type="button"
              className="text-button"
              onClick={() => send("helpful", null)}
              disabled={busy}
            >
              Helpful
            </button>
            <button
              type="button"
              className="text-button"
              onClick={() => {
                setForm({ about: null });
                setReason("");
              }}
              disabled={busy}
            >
              Not helpful…
            </button>
          </div>
        )
      )}
    </div>
  );
}

/** The lesson on offer from one correction: keep it (edited if need be) or let it go. */
export function LessonCard({
  receipt,
  api,
  onSettled,
  onMessage,
  explanation,
}: {
  receipt: FeedbackReceipt;
  api: Pick<FeedbackApi, "learn" | "keepOneOff">;
  onSettled: (next: FeedbackReceipt) => void;
  onMessage: (message: string) => void;
  /** What each choice means here; the default is for feedback on a reply. */
  explanation?: string;
}) {
  const { feedback, lesson } = receipt;
  const [content, setContent] = useState(lesson?.content ?? "");
  const [kind, setKind] = useState<MemoryKind>(lesson?.kind ?? "preference");
  const [busy, setBusy] = useState(false);
  const problem = lessonProblem(content);

  async function choose(remember: boolean) {
    setBusy(true);
    try {
      onSettled(
        remember
          ? await api.learn(feedback.id, kind, content.trim())
          : await api.keepOneOff(feedback.id),
      );
    } catch (cause) {
      onMessage(messageOf(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="memory-suggestion lesson-card">
      <p className="proposal-kicker">LEARN FROM THIS?</p>
      <small>
        You said{feedback.about ? ` about ${quoted(feedback.about)}` : ""}: “
        {feedback.reason}”
      </small>
      <label className="lesson-field">
        <span>Remember as</span>
        <select
          value={kind}
          onChange={(event) => setKind(event.target.value as MemoryKind)}
          disabled={busy}
          aria-label="Kind of lesson"
        >
          {lessonKinds.map((option) => (
            <option key={option} value={option}>
              {kindLabels[option]}
            </option>
          ))}
        </select>
      </label>
      <input
        value={content}
        onChange={(event) => setContent(event.target.value)}
        aria-label="The lesson, in your words"
        disabled={busy}
      />
      {problem && <small className="change-reason">{problem}</small>}
      <small>
        {explanation ??
          "Remember: the assistant follows it in every later plan, and you can change or delete it in Settings › Memory. Just this once: it only counts in this conversation."}
      </small>
      <div className="proposal-actions">
        <button
          className="secondary-button"
          onClick={() => choose(false)}
          disabled={busy}
        >
          Just this once
        </button>
        <button
          className="primary-button small"
          onClick={() => choose(true)}
          disabled={busy || problem !== null}
        >
          {busy && <Spinner size={7} />}
          Remember for next time
        </button>
      </div>
    </div>
  );
}

/** A title in quotes, unless it already opens with one (a heads-up's title often does). */
function quoted(text: string) {
  return /^[“"]/.test(text) ? text : `“${text}”`;
}

function messageOf(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

import { FormEvent, useState } from "react";
import { LessonCard } from "./AssistantFeedback";
import { Spinner } from "./Geometry";
import {
  feedbackApi,
  FeedbackApi,
  FeedbackReceipt,
  MAX_FEEDBACK_REASON_CHARS,
} from "./feedbackApi";
import {
  HeadsUp,
  headsUpApi,
  HeadsUpApi,
  headsUpKindLabels,
} from "./headsUpApi";

/** What a lesson from a heads-up does, and what it doesn't: the rules that find heads-ups are fixed. */
export const headsUpLessonExplanation =
  "Remember: the assistant takes it into account whenever it plans or answers, and you can change or delete it in Settings › Memory. It doesn't change which heads-ups are found; stopping a kind does that. Just this once: nothing is kept.";

/**
 * "Dismiss" on a heads-up, with room to say why: stop the whole kind ("no more quiet-project
 * heads-ups"), and a reason that is kept as feedback and offered back as a lesson for the
 * assistant. Both are optional; dismissing with neither is the plain "don't show this again".
 */
export function HeadsUpDismiss({
  headsUp,
  onDone,
  onCancel,
  onMessage,
  api = headsUpApi,
  lessons = feedbackApi,
  initialLesson = null,
}: {
  headsUp: Pick<HeadsUp, "id" | "title" | "kind">;
  /** The heads-up is dismissed and any lesson settled: the list can reload. */
  onDone: () => void;
  onCancel: () => void;
  onMessage: (message: string) => void;
  api?: Pick<HeadsUpApi, "dismiss">;
  lessons?: Pick<FeedbackApi, "learn" | "keepOneOff">;
  /** Shown instead of the form; for tests. */
  initialLesson?: FeedbackReceipt | null;
}) {
  const [reason, setReason] = useState("");
  const [stopKind, setStopKind] = useState(false);
  const [busy, setBusy] = useState(false);
  const [lesson, setLesson] = useState<FeedbackReceipt | null>(initialLesson);
  const kind = headsUpKindLabels[headsUp.kind];

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    try {
      const receipt = await api.dismiss(headsUp.id, {
        stopKind,
        reason: reason.trim() || null,
      });
      if (receipt.feedback?.lesson) setLesson(receipt.feedback);
      else onDone();
    } catch (cause) {
      onMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  if (lesson)
    return (
      <div className="heads-up-dismiss">
        <LessonCard
          receipt={lesson}
          api={lessons}
          onSettled={onDone}
          onMessage={onMessage}
          explanation={headsUpLessonExplanation}
        />
      </div>
    );

  return (
    <form
      className="heads-up-dismiss feedback-form"
      onSubmit={submit}
      aria-label={`Dismiss: ${headsUp.title}`}
    >
      <label>
        <span>Why isn&rsquo;t this useful? (optional)</span>
        <textarea
          value={reason}
          maxLength={MAX_FEEDBACK_REASON_CHARS}
          onChange={(event) => setReason(event.target.value)}
          placeholder="For example: that project is on hold until spring."
          rows={2}
          autoFocus
        />
      </label>
      <label className="heads-up-stop">
        <input
          type="checkbox"
          checked={stopKind}
          onChange={(event) => setStopKind(event.target.checked)}
        />
        <span>
          Stop showing {kind}
          <small>Turn it back on any time in Settings › Heads-ups.</small>
        </span>
      </label>
      <div className="proposal-actions">
        <button
          type="button"
          className="secondary-button"
          onClick={onCancel}
          disabled={busy}
        >
          Cancel
        </button>
        <button className="primary-button small" disabled={busy}>
          {busy && <Spinner size={7} />}
          Dismiss
        </button>
      </div>
    </form>
  );
}

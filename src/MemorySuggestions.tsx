import { useState } from "react";
import { Spinner } from "./Geometry";
import {
  kindLabels,
  memoryApi,
  MemorySuggestion,
  suggestionBasisLabel,
} from "./memoryApi";

type SuggestionApi = Pick<
  typeof memoryApi,
  "rememberSuggestion" | "dismissSuggestion"
>;

/** One line under a suggestion: what kind it is, who it comes from, and its project. */
export function suggestionLine(suggestion: MemorySuggestion) {
  return [
    kindLabels[suggestion.kind],
    suggestionBasisLabel(suggestion.basis),
    suggestion.planTitle,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** What an answered suggestion says instead of its buttons, or null while it waits. */
export function suggestionOutcome(suggestion: MemorySuggestion) {
  switch (suggestion.state.state) {
    case "remembered":
      return suggestion.basis === "stated"
        ? "Remembered. You can see, correct or delete it in Settings › Memory."
        : "Remembered as a guess. Confirm, correct or delete it in Settings › Memory.";
    case "dismissed":
      return "Not remembered.";
    default:
      return null;
  }
}

/**
 * "Remember this?" cards for one assistant run. The assistant can only suggest; each memory is
 * stored only when the person presses Remember, and a guess is labelled as a guess.
 */
export function MemorySuggestions({
  runKey,
  suggestions,
  problems,
  onMessage,
  onRefresh,
  api = memoryApi,
}: {
  runKey: string;
  suggestions: MemorySuggestion[];
  /** Suggestions the app left out, and why. */
  problems: string[];
  onMessage: (message: string) => void;
  onRefresh: () => Promise<void>;
  api?: SuggestionApi;
}) {
  const [answered, setAnswered] = useState<Record<string, MemorySuggestion>>(
    {},
  );
  const [busy, setBusy] = useState<string | null>(null);
  if (suggestions.length === 0 && problems.length === 0) return null;

  async function answer(suggestion: MemorySuggestion, remember: boolean) {
    setBusy(suggestion.id);
    try {
      const next = remember
        ? await api.rememberSuggestion(runKey, suggestion.id)
        : await api.dismissSuggestion(runKey, suggestion.id);
      setAnswered((current) => ({ ...current, [suggestion.id]: next }));
    } catch (cause) {
      onMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
      await onRefresh();
    }
  }

  return (
    <div className="memory-suggestions">
      {suggestions.map((original) => {
        const suggestion = answered[original.id] ?? original;
        const outcome = suggestionOutcome(suggestion);
        const saving =
          busy === suggestion.id || suggestion.state.state === "saving";
        return (
          <div
            key={suggestion.id}
            className={`memory-suggestion ${suggestion.basis}`}
          >
            <p className="proposal-kicker">REMEMBER THIS?</p>
            <strong>{suggestion.content}</strong>
            <small>{suggestionLine(suggestion)}</small>
            {outcome ? (
              <p
                className={`agent-note ${suggestion.state.state === "remembered" ? "good" : ""}`}
              >
                {outcome}
              </p>
            ) : (
              <div className="proposal-actions">
                <button
                  className="secondary-button"
                  onClick={() => answer(suggestion, false)}
                  disabled={saving}
                >
                  Not now
                </button>
                <button
                  className="primary-button small"
                  onClick={() => answer(suggestion, true)}
                  disabled={saving}
                >
                  {saving && <Spinner size={7} />}
                  Remember
                </button>
              </div>
            )}
          </div>
        );
      })}
      {problems.length > 0 && (
        <div className="agent-note">
          Left out {problems.length} memory suggestion
          {problems.length === 1 ? "" : "s"}:
          <ul>
            {problems.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

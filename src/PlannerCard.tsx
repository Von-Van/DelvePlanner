import { FormEvent, useEffect, useMemo, useState } from "react";
import {
  commandShortcutText,
  shortcutPlatform,
  submitOnCommandEnter,
} from "./shortcuts";
import type {
  OllamaStatus,
  PlannerResponse,
  ProposalChange,
  SuggestionEdit,
} from "./api";
import { Glyph, Mark, Spinner } from "./Geometry";
import { acceptedOperations, describeProposal, withEdits } from "./proposals";
import { canEditSuggestion, SuggestionEditor } from "./SuggestionEditor";

export function PlannerCard({
  status,
  command,
  onCommand,
  onSubmit,
  thinking,
  response,
  onApply,
  applying,
  onDiscard,
  onClear,
  onRefreshStatus,
  planTitle,
}: {
  status: OllamaStatus | null;
  command: string;
  onCommand: (value: string) => void;
  onSubmit: (event: FormEvent) => void;
  thinking: boolean;
  response: PlannerResponse | null;
  /** Applies only the suggestions the user kept ticked, with the changes they made to any. */
  onApply: (accepted: string[], edits: SuggestionEdit[]) => void;
  applying: boolean;
  onDiscard: () => void;
  onClear: () => void;
  onRefreshStatus: () => void;
  /** When set, the planner works inside this plan: new work belongs to it by default. */
  planTitle?: string;
}) {
  // Readiness is having a model, not having a server running: the runtime starts when a request
  // is sent and stops again once nobody is asking anything.
  const ready = status?.modelInstalled ?? false;
  const stateDetail =
    status?.phase === "stopped" && ready
      ? `${status.modelName} · starts when you ask something`
      : (status?.detail ?? "Checking your local model…");
  // Suggestions the user changed in review, by handle, as they will be applied.
  const [edits, setEdits] = useState<ReadonlyMap<string, ProposalChange>>(
    new Map(),
  );
  const [editing, setEditing] = useState<string | null>(null);
  const reviewed = useMemo(
    () => (response?.kind === "proposal" ? withEdits(response, edits) : null),
    [response, edits],
  );
  const previews = useMemo(
    () => (reviewed ? describeProposal(reviewed) : []),
    [reviewed],
  );
  const [rejected, setRejected] = useState<ReadonlySet<string>>(new Set());
  // Every new proposal is reviewed from scratch, with everything accepted to begin with.
  useEffect(() => {
    setRejected(new Set());
    setEdits(new Map());
    setEditing(null);
  }, [response]);
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
  return (
    <section className="planner-card" aria-label="Local planner">
      <div className="planner-heading">
        <div className="planner-orb" aria-hidden="true">
          <i />
        </div>
        <div>
          <p>LOCAL PLANNER</p>
          <h2>Say it messily.</h2>
        </div>
        <button
          onClick={onClear}
          title="Clear conversational context"
          aria-label="Clear conversational context"
          className="reset-button"
        >
          <Glyph>↺</Glyph>
        </button>
      </div>
      <div className={`model-state ${ready ? "ready" : "not-ready"}`}>
        <span />
        <div>
          <strong>
            {ready ? "Local model ready" : "Choose a local model"}
          </strong>
          <p>{stateDetail}</p>
        </div>
        <button
          onClick={onRefreshStatus}
          title="Refresh model status"
          aria-label="Refresh model status"
        >
          <Glyph>↻</Glyph>
        </button>
      </div>
      <form onSubmit={onSubmit} className="command-form">
        {planTitle && (
          <p className="planner-scope">
            New tasks, milestones, and events go in <strong>{planTitle}</strong>{" "}
            unless you name another plan.
          </p>
        )}
        <textarea
          maxLength={1000}
          value={command}
          onChange={(event) => onCommand(event.target.value)}
          onKeyDown={submitOnCommandEnter}
          aria-keyshortcuts={isMacShortcuts ? "Meta+Enter" : "Control+Enter"}
          aria-label={
            planTitle ? `Plan changes for ${planTitle}` : "Plan changes"
          }
          placeholder={
            planTitle
              ? "“Add a Book caterer task due Friday, move the rehearsal milestone to Oct 10…”"
              : "“Move gym to 6pm tomorrow, mark Book venue done…”"
          }
          disabled={!ready || thinking}
        />
        <div>
          <span>
            <Glyph>⌘</Glyph> The model proposes; you decide.
          </span>
          <button
            className="primary-button small"
            disabled={!command.trim() || !ready || thinking}
            title={`Plan changes (${commandShortcutText("↩")})`}
          >
            {thinking ? <Spinner size={7} /> : <Mark size={6} filled />}
            Plan changes
          </button>
        </div>
      </form>
      <div aria-live="polite">
        {response?.kind === "clarification" && (
          <div className="clarification">
            <span aria-hidden="true">?</span>
            <p>{response.question}</p>
          </div>
        )}
        {response?.kind === "proposal" && (
          <div className="proposal">
            <p className="proposal-kicker">REVIEW BEFORE APPLYING</p>
            <strong>{response.summary}</strong>
            <ul>
              {previews.map((preview) => {
                const change = reviewed?.operations.find(
                  (operation) => operation.id === preview.id,
                )?.change;
                return (
                  <li
                    key={preview.id}
                    className={`${kept.has(preview.id) ? "" : "rejected"} ${edits.has(preview.id) ? "edited" : ""}`}
                  >
                    <label>
                      <input
                        type="checkbox"
                        checked={kept.has(preview.id)}
                        onChange={() => toggle(preview.id)}
                        aria-label={`Accept: ${preview.title}`}
                      />
                      <i
                        className={`operation-mark ${preview.tone}`}
                        aria-hidden="true"
                      />
                      <span>
                        {preview.title}
                        {preview.suggested && (
                          <em className="suggested-mark">suggested</em>
                        )}
                        {preview.details.length > 0 && (
                          <small>{preview.details.join(" · ")}</small>
                        )}
                        {preview.reason && (
                          <small className="operation-reason">
                            {preview.reason}
                          </small>
                        )}
                      </span>
                    </label>
                    {change &&
                      editing !== preview.id &&
                      kept.has(preview.id) &&
                      canEditSuggestion(change) && (
                        <div className="suggestion-actions">
                          {edits.has(preview.id) && (
                            <em className="edited-mark">edited</em>
                          )}
                          <button
                            className="text-button"
                            onClick={() => setEditing(preview.id)}
                            aria-label={`Edit: ${preview.title}`}
                          >
                            Edit
                          </button>
                          {edits.has(preview.id) && (
                            <button
                              className="text-button"
                              onClick={() => {
                                const next = new Map(edits);
                                next.delete(preview.id);
                                setEdits(next);
                              }}
                            >
                              Undo edit
                            </button>
                          )}
                        </div>
                      )}
                    {change && editing === preview.id && (
                      <SuggestionEditor
                        change={change}
                        onCancel={() => setEditing(null)}
                        onSave={(next) => {
                          setEdits(new Map(edits).set(preview.id, next));
                          setEditing(null);
                        }}
                      />
                    )}
                  </li>
                );
              })}
            </ul>
            {accepted.length < previews.length && (
              <p className="proposal-note">
                {previews.length - accepted.length} of {previews.length}{" "}
                rejected
                {previews.some(
                  (preview) =>
                    !kept.has(preview.id) && !rejected.has(preview.id),
                ) &&
                  ", including changes that needed a rejected plan or milestone"}
                .
              </p>
            )}
            <div className="proposal-actions">
              <button className="secondary-button" onClick={onDiscard}>
                Discard
              </button>
              <button
                className="primary-button small"
                onClick={() =>
                  onApply(
                    accepted,
                    [...edits]
                      .filter(([id]) => kept.has(id))
                      .map(([id, change]) => ({ id, change })),
                  )
                }
                disabled={applying || accepted.length === 0 || editing !== null}
              >
                {applying && <Spinner size={7} />}
                Apply {accepted.length} change
                {accepted.length === 1 ? "" : "s"}
              </button>
            </div>
          </div>
        )}
      </div>
      <p className="planner-footnote">
        <span aria-hidden="true">◌</span> No API key. No cloud fallback. Context
        clears when you clear this session.
      </p>
    </section>
  );
}

const isMacShortcuts = shortcutPlatform === "mac";

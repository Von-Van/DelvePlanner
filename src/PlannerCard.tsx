import { FormEvent } from "react";
import type { OllamaStatus, PlannerResponse } from "./api";
import { Glyph, Mark, Spinner } from "./Geometry";
import { describeProposal } from "./proposals";

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
  onApply: () => void;
  applying: boolean;
  onDiscard: () => void;
  onClear: () => void;
  onRefreshStatus: () => void;
  /** When set, the planner works inside this plan: new work belongs to it by default. */
  planTitle?: string;
}) {
  const ready = status?.running && status.modelInstalled;
  const previews =
    response?.kind === "proposal" ? describeProposal(response) : [];
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
            {ready ? "Local model ready" : "Local model setup needed"}
          </strong>
          <p>{status?.detail ?? "Checking your local model…"}</p>
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
              {previews.map((preview, index) => (
                <li key={`${response.operations[index].type}-${index}`}>
                  <i
                    className={`operation-mark ${preview.tone}`}
                    aria-hidden="true"
                  />
                  <span>
                    {preview.title}
                    {preview.details.length > 0 && (
                      <small>{preview.details.join(" · ")}</small>
                    )}
                  </span>
                </li>
              ))}
            </ul>
            <div className="proposal-actions">
              <button className="secondary-button" onClick={onDiscard}>
                Discard
              </button>
              <button
                className="primary-button small"
                onClick={onApply}
                disabled={applying}
              >
                {applying && <Spinner size={7} />}
                Apply {response.operations.length} change
                {response.operations.length === 1 ? "" : "s"}
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

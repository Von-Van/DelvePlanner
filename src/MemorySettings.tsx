import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "./api";
import { todayDay } from "./date";
import {
  contentProblem,
  conversationTurnChoices,
  groupRecords,
  inferredTtlChoices,
  kindLabels,
  MAX_MEMORY_CHARS,
  memoryApi as defaultApi,
  MemoryApi,
  MemoryKind,
  memoryKinds,
  MemoryOverview,
  MemoryRecord,
  MemorySettingsUpdate,
  memoryStatusLine,
  provenance,
  shortDate,
} from "./memoryApi";
import "./MemorySettings.css";

export type ProjectOption = { id: string; title: string };

/**
 * Memory & privacy controls: what the assistant remembers, where it came from, and the switches
 * that limit it. Loads its own data so Settings only has to place it.
 */
export function MemorySettings({
  onMessage,
  memoryApi = defaultApi,
}: {
  onMessage: (message: string) => void;
  memoryApi?: MemoryApi;
}) {
  const [overview, setOverview] = useState<MemoryOverview | null>(null);
  const [projects, setProjects] = useState<ProjectOption[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setOverview(await memoryApi.overview());
      setProblem(null);
    } catch (cause) {
      setProblem(cause instanceof Error ? cause.message : String(cause));
    }
  }, [memoryApi]);

  useEffect(() => {
    void refresh();
    void api
      .listPlans(todayDay())
      .then((plans) =>
        setProjects(
          plans
            .filter((summary) => !summary.plan.archived)
            .map((summary) => ({
              id: summary.plan.id,
              title: summary.plan.title,
            })),
        ),
      )
      .catch(() => setProjects([]));
  }, [refresh]);

  /** Runs one change; resolves true only when it was saved, so editors keep text on failure. */
  async function act(
    action: () => Promise<unknown>,
    done?: string,
  ): Promise<boolean> {
    setBusy(true);
    try {
      await action();
      await refresh();
      if (done) onMessage(done);
      return true;
    } catch (cause) {
      onMessage(cause instanceof Error ? cause.message : String(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }

  if (problem) return <p className="memory-problem">{problem}</p>;
  if (!overview) return <p>Loading memory…</p>;

  return (
    <MemoryPanel
      overview={overview}
      projects={projects}
      busy={busy}
      onUpdateSettings={(update) =>
        void act(() => memoryApi.updateSettings(update))
      }
      onProjectMemory={(planId, enabled) =>
        void act(() => memoryApi.setProjectMemory(planId, enabled))
      }
      onForgetProject={(project) => {
        if (
          !window.confirm(
            `Delete everything remembered for ${project.title}? This can't be undone.`,
          )
        )
          return;
        void act(
          () => memoryApi.forgetProject(project.id),
          `Forgot memories for ${project.title}.`,
        );
      }}
      onAdd={(input) =>
        act(() => memoryApi.add(input), "The assistant will remember that.")
      }
      onConfirm={(record) => void act(() => memoryApi.confirm(record.id))}
      onCorrect={(record, content) =>
        act(
          () => memoryApi.correct(record.id, content),
          "Corrected. The old version is kept in history.",
        )
      }
      onDelete={(record) => {
        if (
          !window.confirm(
            "Delete this memory and its earlier versions? This can't be undone.",
          )
        )
          return;
        void act(() => memoryApi.remove(record.id), "Memory deleted.");
      }}
      onExport={() =>
        void act(async () => {
          const result = await memoryApi.exportFile();
          if (result.completed) onMessage(`Exported ${result.fileName}.`);
        })
      }
      onClear={() => {
        if (
          !window.confirm(
            "Delete every memory, including history? Settings stay as they are. This can't be undone.",
          )
        )
          return;
        void act(() => memoryApi.clear(), "All memories deleted.");
      }}
    />
  );
}

type PanelProps = {
  overview: MemoryOverview;
  projects: ProjectOption[];
  busy: boolean;
  onUpdateSettings: (update: MemorySettingsUpdate) => void;
  onProjectMemory: (planId: string, enabled: boolean) => void;
  onForgetProject: (project: ProjectOption) => void;
  onAdd: (input: {
    kind: MemoryKind;
    content: string;
    planId: string | null;
  }) => Promise<boolean>;
  onConfirm: (record: MemoryRecord) => void;
  onCorrect: (record: MemoryRecord, content: string) => Promise<boolean>;
  onDelete: (record: MemoryRecord) => void;
  onExport: () => void;
  onClear: () => void;
};

/** The panel itself, driven entirely by props so it can be rendered in tests. */
export function MemoryPanel(props: PanelProps) {
  const { overview, projects, busy } = props;
  const { settings } = overview;
  const { current, history } = groupRecords(overview.records);
  const planTitles = useMemo(
    () => new Map(projects.map((p) => [p.id, p.title])),
    [projects],
  );
  const disabled = new Set(settings.disabledPlanIds);
  const counts = new Map<string, number>();
  for (const record of overview.records)
    if (record.planId)
      counts.set(record.planId, (counts.get(record.planId) ?? 0) + 1);

  return (
    <div className="memory-settings">
      <p className="memory-status" role="status">
        {memoryStatusLine(
          settings,
          current.filter((r) => !r.planId || !disabled.has(r.planId)).length,
        )}
      </p>

      <label className="memory-toggle">
        <input
          type="checkbox"
          checked={settings.enabled}
          disabled={busy}
          onChange={(input) =>
            props.onUpdateSettings({ enabled: input.target.checked })
          }
        />
        <span>
          <strong>Use memory</strong>
          <small>
            The assistant may read what is listed below. It can never add or
            change a memory itself.
          </small>
        </span>
      </label>

      <div className="memory-fields">
        <label className="settings-field">
          Recent conversation
          <select
            value={settings.conversationTurns}
            disabled={busy}
            onChange={(input) =>
              props.onUpdateSettings({
                conversationTurns: Number(input.target.value),
              })
            }
          >
            {conversationTurnChoices.map((turns) => (
              <option key={turns} value={turns}>
                {turns === 0 ? "Don't carry over" : `Last ${turns} messages`}
              </option>
            ))}
          </select>
          <span>
            How much of the current conversation a follow-up like “move it 30
            minutes later” can see. Not stored beyond the conversation.
          </span>
        </label>
        <label className="settings-field">
          Assistant guesses expire after
          <select
            value={settings.inferredTtlDays}
            disabled={busy}
            onChange={(input) =>
              props.onUpdateSettings({
                inferredTtlDays: Number(input.target.value),
              })
            }
          >
            {inferredTtlChoices.map((days) => (
              <option key={days} value={days}>
                {days} days
              </option>
            ))}
          </select>
          <span>Unless you confirm them. Applies to new guesses.</span>
        </label>
      </div>

      <AddMemory
        projects={projects}
        disabledPlanIds={disabled}
        busy={busy || !settings.enabled}
        onAdd={props.onAdd}
      />

      <section aria-labelledby="memory-list-title">
        <h4 id="memory-list-title">Remembered ({current.length})</h4>
        {current.length === 0 ? (
          <p className="memory-empty">
            Nothing yet. Add something above, or confirm a guess when the
            assistant offers one.
          </p>
        ) : (
          <ul className="memory-list">
            {current.map((record) => (
              <MemoryItem
                key={record.id}
                record={record}
                planTitles={planTitles}
                projectOff={!!record.planId && disabled.has(record.planId)}
                busy={busy}
                onConfirm={props.onConfirm}
                onCorrect={props.onCorrect}
                onDelete={props.onDelete}
              />
            ))}
          </ul>
        )}
      </section>

      {history.length > 0 && (
        <details className="memory-history">
          <summary>
            History: {history.length} replaced by a correction (never shown to
            the assistant)
          </summary>
          <ul className="memory-list">
            {history.map((record) => (
              <li key={record.id} className="memory-item is-history">
                <p className="memory-content">{record.content}</p>
                <p className="memory-meta">
                  {provenance(record, planTitles)} · replaced{" "}
                  {shortDate(record.supersededAt)}
                </p>
                <div className="settings-actions">
                  <button
                    disabled={busy}
                    onClick={() => props.onDelete(record)}
                  >
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </details>
      )}

      {projects.length > 0 && (
        <section aria-labelledby="memory-projects-title">
          <h4 id="memory-projects-title">Per project</h4>
          <ul className="memory-projects">
            {projects.map((project) => (
              <li key={project.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={!disabled.has(project.id)}
                    disabled={busy}
                    onChange={(input) =>
                      props.onProjectMemory(project.id, input.target.checked)
                    }
                  />
                  <span>{project.title}</span>
                </label>
                <small>{counts.get(project.id) ?? 0} stored</small>
                {(counts.get(project.id) ?? 0) > 0 && (
                  <button
                    disabled={busy}
                    onClick={() => props.onForgetProject(project)}
                  >
                    Forget
                  </button>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      <div className="settings-actions">
        <button disabled={busy} onClick={props.onExport}>
          Export memory
        </button>
        <button
          className="danger-action"
          disabled={busy || overview.records.length === 0}
          onClick={props.onClear}
        >
          Forget everything
        </button>
      </div>
    </div>
  );
}

function AddMemory({
  projects,
  disabledPlanIds,
  busy,
  onAdd,
}: {
  projects: ProjectOption[];
  disabledPlanIds: Set<string>;
  busy: boolean;
  onAdd: PanelProps["onAdd"];
}) {
  const [kind, setKind] = useState<MemoryKind>("decision");
  const [content, setContent] = useState("");
  const [planId, setPlanId] = useState("");
  const [touched, setTouched] = useState(false);
  const problem = contentProblem(content);

  return (
    <form
      className="memory-add"
      onSubmit={(event) => {
        event.preventDefault();
        setTouched(true);
        if (problem) return;
        void onAdd({ kind, content: content.trim(), planId: planId || null })
          .then((saved) => {
            if (!saved) return;
            setContent("");
            setTouched(false);
          })
          .catch(() => undefined);
      }}
    >
      <h4>Add a memory</h4>
      <textarea
        aria-label="What should the assistant remember?"
        placeholder="For example: The weekly launch sync is on Thursdays at 10."
        value={content}
        maxLength={MAX_MEMORY_CHARS}
        rows={2}
        disabled={busy}
        onChange={(input) => setContent(input.target.value)}
      />
      <div className="memory-add-row">
        <select
          aria-label="Kind"
          value={kind}
          disabled={busy}
          onChange={(input) => setKind(input.target.value as MemoryKind)}
        >
          {memoryKinds.map((k) => (
            <option key={k} value={k}>
              {kindLabels[k]}
            </option>
          ))}
        </select>
        <select
          aria-label="Project"
          value={planId}
          disabled={busy}
          onChange={(input) => setPlanId(input.target.value)}
        >
          <option value="">Any project</option>
          {projects
            .filter((p) => !disabledPlanIds.has(p.id))
            .map((p) => (
              <option key={p.id} value={p.id}>
                {p.title}
              </option>
            ))}
        </select>
        <button type="submit" className="primary-action" disabled={busy}>
          Remember
        </button>
      </div>
      {touched && problem && <p className="memory-problem">{problem}</p>}
    </form>
  );
}

function MemoryItem({
  record,
  planTitles,
  projectOff,
  busy,
  onConfirm,
  onCorrect,
  onDelete,
}: {
  record: MemoryRecord;
  planTitles: Map<string, string>;
  projectOff: boolean;
  busy: boolean;
  onConfirm: PanelProps["onConfirm"];
  onCorrect: PanelProps["onCorrect"];
  onDelete: PanelProps["onDelete"];
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(record.content);
  const unconfirmed = record.confirmation === "unconfirmed";
  const problem = contentProblem(draft);

  return (
    <li className={`memory-item${unconfirmed ? " is-unconfirmed" : ""}`}>
      {editing ? (
        <textarea
          aria-label="Corrected memory"
          value={draft}
          maxLength={MAX_MEMORY_CHARS}
          rows={2}
          onChange={(input) => setDraft(input.target.value)}
        />
      ) : (
        <p className="memory-content">{record.content}</p>
      )}
      <p className="memory-meta">
        {provenance(record, planTitles)}
        {unconfirmed && (
          <span className="memory-badge">
            Unconfirmed · expires {shortDate(record.expiresAt)}
          </span>
        )}
        {projectOff && (
          <span className="memory-badge is-off">Project memory off</span>
        )}
      </p>
      <div className="settings-actions">
        {editing ? (
          <>
            <button
              className="primary-action"
              disabled={busy || !!problem || draft.trim() === record.content}
              onClick={() =>
                void onCorrect(record, draft.trim())
                  .then((saved) => saved && setEditing(false))
                  .catch(() => undefined)
              }
            >
              Save correction
            </button>
            <button
              onClick={() => {
                setDraft(record.content);
                setEditing(false);
              }}
            >
              Cancel
            </button>
          </>
        ) : (
          <>
            {unconfirmed && (
              <button disabled={busy} onClick={() => onConfirm(record)}>
                Confirm
              </button>
            )}
            <button disabled={busy} onClick={() => setEditing(true)}>
              Correct
            </button>
            <button disabled={busy} onClick={() => onDelete(record)}>
              Delete
            </button>
          </>
        )}
      </div>
    </li>
  );
}

import { FormEvent, ReactNode, useRef, useState } from "react";
import {
  api,
  InboxItem,
  messageFor,
  Milestone,
  MilestoneInput,
  milestoneStatuses,
  Plan,
  PlanColor,
  PlanInput,
  planStatuses,
  Workstream,
  WorkstreamInput,
} from "./api";
import { Glyph, Spinner } from "./Geometry";
import { ColorSwatches, OptionalDate, WorkstreamSelect } from "./PlanControls";
import { milestoneStatusLabels, planStatusLabels } from "./planning";
import { useModalFocus } from "./useModalFocus";

export function PlanEditor({
  plan,
  inboxItem,
  defaultColor,
  onClose,
  onSaved,
  onArchive,
  onError,
}: {
  plan?: Plan;
  /** Converts this inbox item: saving creates the plan and removes the item together. */
  inboxItem?: InboxItem;
  defaultColor: PlanColor;
  onClose: () => void;
  onSaved: (planId: string) => Promise<void> | void;
  /** Offered for an existing, unarchived plan. Unsaved edits in the form are discarded. */
  onArchive?: () => void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<PlanInput>(() => ({
    title: plan?.title ?? inboxItem?.text ?? "",
    description: plan?.description ?? inboxItem?.notes ?? "",
    status: plan?.status ?? "planning",
    startDate: plan?.startDate ?? null,
    targetDate: plan?.targetDate ?? null,
    color: plan ? plan.color : defaultColor,
  }));
  const [saving, setSaving] = useState(false);
  const invertedDates =
    draft.startDate !== null &&
    draft.targetDate !== null &&
    draft.startDate > draft.targetDate;

  async function submit(form: FormEvent) {
    form.preventDefault();
    setSaving(true);
    try {
      const savedId = inboxItem
        ? (await api.processInboxItem(inboxItem, { kind: "plan", plan: draft }))
            .id
        : plan
          ? (
              await api.updatePlan({
                ...draft,
                id: plan.id,
                revision: plan.revision,
                archived: plan.archived,
              })
            ).id
          : (await api.createPlan(draft)).id;
      await onSaved(savedId);
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <EditorShell
      label={inboxItem ? "Make a plan" : plan ? "Edit plan" : "New plan"}
      kicker={inboxItem ? "FROM INBOX" : plan ? "EDIT PLAN" : "NEW PLAN"}
      heading={
        inboxItem
          ? "Make it a plan"
          : plan
            ? "Adjust the plan"
            : "What are you working toward?"
      }
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          {plan && onArchive && (
            <button
              type="button"
              className="editor-aside"
              onClick={onArchive}
              disabled={saving}
              title="Archived plans keep everything and can be restored"
            >
              Archive plan
            </button>
          )}
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !draft.title.trim() || invertedDates}
          >
            {saving && <Spinner size={7} />}
            {plan ? "Save changes" : "Create plan"}
          </button>
        </>
      }
    >
      <label>
        Title
        <input
          autoFocus
          value={draft.title}
          onChange={(input) =>
            setDraft({ ...draft, title: input.target.value })
          }
          required
          maxLength={140}
          placeholder="Product launch, TwitchCon 2027, a wedding…"
        />
      </label>
      <label>
        Status
        <select
          value={draft.status}
          onChange={(input) =>
            setDraft({
              ...draft,
              status: input.target.value as PlanInput["status"],
            })
          }
        >
          {planStatuses.map((status) => (
            <option key={status} value={status}>
              {planStatusLabels[status]}
            </option>
          ))}
        </select>
      </label>
      <div className="form-pair">
        <OptionalDate
          label="Starts"
          value={draft.startDate}
          onChange={(startDate) => setDraft({ ...draft, startDate })}
        />
        <OptionalDate
          label="Target date"
          value={draft.targetDate}
          onChange={(targetDate) => setDraft({ ...draft, targetDate })}
        />
      </div>
      {invertedDates && (
        <p className="editor-hint" role="alert">
          The start date must be on or before the target date.
        </p>
      )}
      <div className="editor-field">
        <span>Color</span>
        <ColorSwatches
          value={draft.color}
          onChange={(color) => setDraft({ ...draft, color })}
        />
      </div>
      <label>
        Description
        <textarea
          value={draft.description}
          onChange={(input) =>
            setDraft({ ...draft, description: input.target.value })
          }
          placeholder="What does done look like?"
          maxLength={2000}
        />
      </label>
    </EditorShell>
  );
}

export function MilestoneEditor({
  planId,
  milestone,
  workstreams,
  onClose,
  onSaved,
  onError,
}: {
  planId: string;
  milestone?: Milestone;
  workstreams: Workstream[];
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<MilestoneInput>(() => ({
    title: milestone?.title ?? "",
    description: milestone?.description ?? "",
    targetDate: milestone?.targetDate ?? null,
    status: milestone?.status ?? "pending",
    workstreamId: milestone?.workstreamId ?? null,
  }));
  const [saving, setSaving] = useState(false);

  async function run(action: () => Promise<unknown>) {
    setSaving(true);
    try {
      await action();
      await onSaved();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  function submit(form: FormEvent) {
    form.preventDefault();
    void run(() =>
      milestone
        ? api.updateMilestone({
            ...draft,
            id: milestone.id,
            revision: milestone.revision,
          })
        : api.createMilestone({ ...draft, planId }),
    );
  }

  function remove() {
    if (
      !milestone ||
      !window.confirm(
        `Delete the milestone “${milestone.title}”? Its tasks stay in the plan.`,
      )
    )
      return;
    void run(() => api.deleteMilestone(milestone.id, milestone.revision));
  }

  return (
    <EditorShell
      label={milestone ? "Edit milestone" : "New milestone"}
      kicker={milestone ? "EDIT MILESTONE" : "NEW MILESTONE"}
      heading={milestone ? "Move the checkpoint" : "Mark a checkpoint"}
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          {milestone && (
            <button
              type="button"
              className="editor-delete"
              onClick={remove}
              disabled={saving}
            >
              Delete
            </button>
          )}
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !draft.title.trim()}
          >
            {saving && <Spinner size={7} />}
            {milestone ? "Save changes" : "Add milestone"}
          </button>
        </>
      }
    >
      <label>
        Title
        <input
          autoFocus
          value={draft.title}
          onChange={(input) =>
            setDraft({ ...draft, title: input.target.value })
          }
          required
          maxLength={140}
          placeholder="Venue confirmed, marketing launch…"
        />
      </label>
      <div className="form-pair">
        <OptionalDate
          label="Target date"
          value={draft.targetDate}
          onChange={(targetDate) => setDraft({ ...draft, targetDate })}
        />
        <label>
          Status
          <select
            value={draft.status}
            onChange={(input) =>
              setDraft({
                ...draft,
                status: input.target.value as MilestoneInput["status"],
              })
            }
          >
            {milestoneStatuses.map((status) => (
              <option key={status} value={status}>
                {milestoneStatusLabels[status]}
              </option>
            ))}
          </select>
        </label>
      </div>
      {workstreams.length > 0 && (
        <WorkstreamSelect
          workstreams={workstreams}
          value={draft.workstreamId}
          disabled={false}
          onChange={(workstreamId) => setDraft({ ...draft, workstreamId })}
        />
      )}
      <label>
        Description
        <textarea
          value={draft.description}
          onChange={(input) =>
            setDraft({ ...draft, description: input.target.value })
          }
          placeholder="What has to be true when this checkpoint is reached?"
          maxLength={2000}
        />
      </label>
    </EditorShell>
  );
}

export function WorkstreamEditor({
  planId,
  workstream,
  onClose,
  onSaved,
  onError,
}: {
  planId: string;
  workstream?: Workstream;
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<WorkstreamInput>(() => ({
    name: workstream?.name ?? "",
    description: workstream?.description ?? "",
  }));
  const [saving, setSaving] = useState(false);

  async function run(action: () => Promise<unknown>) {
    setSaving(true);
    try {
      await action();
      await onSaved();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  function submit(form: FormEvent) {
    form.preventDefault();
    void run(() =>
      workstream
        ? api.updateWorkstream({
            ...draft,
            id: workstream.id,
            revision: workstream.revision,
          })
        : api.createWorkstream({ ...draft, planId }),
    );
  }

  function remove() {
    if (
      !workstream ||
      !window.confirm(
        `Delete the workstream “${workstream.name}”? Its tasks, milestones, and events stay in the plan.`,
      )
    )
      return;
    void run(() => api.deleteWorkstream(workstream.id, workstream.revision));
  }

  return (
    <EditorShell
      label={workstream ? "Edit workstream" : "New workstream"}
      kicker={workstream ? "EDIT WORKSTREAM" : "NEW WORKSTREAM"}
      heading={workstream ? "Rename the stream" : "Group related work"}
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          {workstream && (
            <button
              type="button"
              className="editor-delete"
              onClick={remove}
              disabled={saving}
            >
              Delete
            </button>
          )}
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !draft.name.trim()}
          >
            {saving && <Spinner size={7} />}
            {workstream ? "Save changes" : "Add workstream"}
          </button>
        </>
      }
    >
      <label>
        Name
        <input
          autoFocus
          value={draft.name}
          onChange={(input) => setDraft({ ...draft, name: input.target.value })}
          required
          maxLength={80}
          placeholder="Production, Sponsors, Creator Relations…"
        />
      </label>
      <label>
        Description
        <textarea
          value={draft.description}
          onChange={(input) =>
            setDraft({ ...draft, description: input.target.value })
          }
          placeholder="Optional scope or notes for this stream of work"
          maxLength={2000}
        />
      </label>
    </EditorShell>
  );
}

export function EditorShell({
  label,
  kicker,
  heading,
  busy,
  onClose,
  onSubmit,
  footer,
  children,
}: {
  label: string;
  kicker: string;
  heading: string;
  busy: boolean;
  onClose: () => void;
  onSubmit: (form: FormEvent) => void;
  footer: ReactNode;
  children: ReactNode;
}) {
  const dialogRef = useRef<HTMLFormElement>(null);
  useModalFocus(dialogRef, onClose, busy);
  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(click) => {
        if (click.target === click.currentTarget && !busy) onClose();
      }}
    >
      <form
        ref={dialogRef}
        className="editor-dialog"
        onSubmit={onSubmit}
        role="dialog"
        aria-modal="true"
        aria-label={label}
      >
        <header>
          <div>
            <p>{kicker}</p>
            <h2>{heading}</h2>
          </div>
          <button type="button" onClick={onClose} aria-label="Close editor">
            <Glyph>✕</Glyph>
          </button>
        </header>
        {children}
        <footer>{footer}</footer>
      </form>
    </div>
  );
}

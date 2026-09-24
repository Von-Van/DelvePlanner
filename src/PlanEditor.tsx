import { FormEvent, ReactNode, useEffect, useRef, useState } from "react";
import { differenceInCalendarDays, parseISO } from "date-fns";
import { submitOnCommandEnter } from "./shortcuts";
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
  PlanLink,
  planStatuses,
  PlanTemplate,
  PlanTemplateInfo,
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
  initialTemplate = null,
  onClose,
  onSaved,
  onArchive,
  onError,
}: {
  plan?: Plan;
  /** Converts this inbox item: saving creates the plan and removes the item together. */
  inboxItem?: InboxItem;
  defaultColor: PlanColor;
  /** For a new plan, the template it starts from until the user picks another. */
  initialTemplate?: PlanTemplate | null;
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
    links: plan?.links ?? [],
  }));
  const creating = !plan && !inboxItem;
  const [template, setTemplate] = useState<PlanTemplate | null>(
    initialTemplate,
  );
  const [templates, setTemplates] = useState<PlanTemplateInfo[]>([]);
  useEffect(() => {
    if (!creating) return;
    let active = true;
    api
      .listPlanTemplates()
      .then((list) => {
        if (active) setTemplates(list);
      })
      .catch((cause) => onError(messageFor(cause)));
    return () => {
      active = false;
    };
    // Loaded once per editor; `onError` only reports a failed load.
  }, [creating]);
  const chosenTemplate = templates.find((item) => item.template === template);
  const [saving, setSaving] = useState(false);
  const invertedDates =
    draft.startDate !== null &&
    draft.targetDate !== null &&
    draft.startDate > draft.targetDate;

  async function submit(form: FormEvent) {
    form.preventDefault();
    setSaving(true);
    // Rows left without an address are dropped rather than refused.
    const input: PlanInput = {
      ...draft,
      links: draft.links
        .map((link) => ({ title: link.title.trim(), url: link.url.trim() }))
        .filter((link) => link.url !== ""),
    };
    try {
      const savedId = inboxItem
        ? (await api.processInboxItem(inboxItem, { kind: "plan", plan: input }))
            .id
        : plan
          ? (
              await api.updatePlan({
                ...input,
                id: plan.id,
                revision: plan.revision,
                archived: plan.archived,
              })
            ).id
          : template
            ? (await api.createPlanFromTemplate(input, template)).id
            : (await api.createPlan(input)).id;
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
      {creating && (
        <label>
          Start from
          <select
            value={template ?? ""}
            onChange={(input) =>
              setTemplate((input.target.value || null) as PlanTemplate | null)
            }
          >
            <option value="">A blank plan</option>
            {templates.map((item) => (
              <option key={item.template} value={item.template}>
                {item.label}
              </option>
            ))}
          </select>
          <span className="field-note">
            {chosenTemplate
              ? `Starts with ${chosenTemplate.workstreams.join(", ")} as workstreams, ready to rename or remove.`
              : "Starts empty."}
          </span>
        </label>
      )}
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
      <PlanLinksField
        links={draft.links}
        onChange={(links) => setDraft({ ...draft, links })}
      />
    </EditorShell>
  );
}

/**
 * Copies a plan's workstreams, milestones, and open tasks into a new plan. Choosing the copy's
 * start (or target) date moves every other date by the same number of days.
 */
export function DuplicatePlanDialog({
  plan,
  onClose,
  onDuplicated,
  onError,
}: {
  plan: Plan;
  onClose: () => void;
  onDuplicated: (planId: string) => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const anchor = plan.startDate ?? plan.targetDate;
  const [title, setTitle] = useState(`${plan.title} (copy)`);
  const [anchorDay, setAnchorDay] = useState(anchor ?? "");
  const [shiftDays, setShiftDays] = useState(0);
  const [saving, setSaving] = useState(false);
  const shift =
    anchor && anchorDay
      ? differenceInCalendarDays(parseISO(anchorDay), parseISO(anchor))
      : shiftDays;

  async function submit(form: FormEvent) {
    form.preventDefault();
    setSaving(true);
    try {
      const copy = await api.duplicatePlan({
        id: plan.id,
        title: title.trim(),
        shiftDays: shift,
      });
      await onDuplicated(copy.id);
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <EditorShell
      label="Duplicate plan"
      kicker="DUPLICATE PLAN"
      heading="Start a copy"
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !title.trim()}
          >
            {saving && <Spinner size={7} />}
            Duplicate
          </button>
        </>
      }
    >
      <p className="dialog-subject">
        Copies the workstreams, milestones, and open tasks of {plan.title}.
        Finished work, events, and time blocks stay with the original.
      </p>
      <label>
        Title
        <input
          autoFocus
          value={title}
          required
          maxLength={140}
          onChange={(input) => setTitle(input.target.value)}
        />
      </label>
      {anchor ? (
        <label>
          {plan.startDate ? "Starts" : "Target date"}{" "}
          <span>Every date moves by the same number of days</span>
          <input
            type="date"
            value={anchorDay}
            required
            onChange={(input) => setAnchorDay(input.target.value)}
          />
        </label>
      ) : (
        <label>
          Move dates by <span>Days; a negative number moves them earlier</span>
          <input
            type="number"
            min={-3660}
            max={3660}
            value={shiftDays}
            onChange={(input) =>
              setShiftDays(Math.round(Number(input.target.value) || 0))
            }
          />
        </label>
      )}
      <p className="field-note" role="status">
        {shift === 0
          ? "Dates stay as they are."
          : `Dates move ${Math.abs(shift)} day${Math.abs(shift) === 1 ? "" : "s"} ${shift > 0 ? "later" : "earlier"}.`}
      </p>
    </EditorShell>
  );
}

const MAX_PLAN_LINKS = 20;

/** Pages that belong with a plan: a title and a web address each, opened in the browser. */
function PlanLinksField({
  links,
  onChange,
}: {
  links: PlanLink[];
  onChange: (links: PlanLink[]) => void;
}) {
  const change = (index: number, field: keyof PlanLink, value: string) =>
    onChange(
      links.map((link, position) =>
        position === index ? { ...link, [field]: value } : link,
      ),
    );
  return (
    <fieldset className="links-field">
      <legend>
        Links <span>Bookings, documents, tickets; opened in your browser</span>
      </legend>
      {links.map((link, index) => (
        <div className="link-row" key={index}>
          <input
            value={link.title}
            maxLength={80}
            placeholder="Title"
            aria-label={`Link ${index + 1} title`}
            onChange={(input) => change(index, "title", input.target.value)}
          />
          <input
            type="url"
            value={link.url}
            maxLength={2048}
            placeholder="https://"
            aria-label={`Link ${index + 1} address`}
            onChange={(input) => change(index, "url", input.target.value)}
          />
          <button
            type="button"
            aria-label={`Remove link ${link.title || index + 1}`}
            onClick={() =>
              onChange(links.filter((_, position) => position !== index))
            }
          >
            <Glyph>✕</Glyph>
          </button>
        </div>
      ))}
      {links.length < MAX_PLAN_LINKS && (
        <button
          type="button"
          className="text-button"
          onClick={() => onChange([...links, { title: "", url: "" }])}
        >
          Add a link
        </button>
      )}
    </fieldset>
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
        onKeyDown={submitOnCommandEnter}
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

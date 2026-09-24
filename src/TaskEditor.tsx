import { FormEvent, useRef, useState } from "react";
import { submitOnCommandEnter } from "./shortcuts";
import {
  api,
  InboxItem,
  messageFor,
  Person,
  Plan,
  Task,
  TaskInput,
  taskPriorities,
  taskStatuses,
} from "./api";
import { offsetDay, todayDay, weekStartDay, weekStartsOn } from "./date";
import { Glyph, Spinner } from "./Geometry";
import {
  OptionalDate,
  PersonSelect,
  PlanSelect,
  WorkstreamSelect,
} from "./PlanControls";
import {
  estimateLabel,
  estimatePresets,
  inWeek,
  newTask,
  shortDate,
  taskHasHome,
  taskPriorityLabels,
  taskStatusLabels,
  taskUpdate,
  weekLabel,
} from "./planning";
import { ChecklistField, RepeatField, WaitsOnField } from "./TaskDetailsFields";
import { useModalFocus } from "./useModalFocus";
import { usePlanLinks } from "./usePlanLinks";

export function TaskEditor({
  task,
  inboxItem,
  defaults,
  plans,
  people,
  onClose,
  onSaved,
  onError,
}: {
  task?: Task;
  /** Converts this inbox item: saving creates the task and removes the item together. */
  inboxItem?: InboxItem;
  defaults?: Partial<TaskInput>;
  plans: Plan[];
  people: Person[];
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const thisWeek = weekStartDay(todayDay(), weekStartsOn);
  const nextWeek = offsetDay(thisWeek, 7);
  const [draft, setDraft] = useState<TaskInput>(() =>
    task
      ? taskUpdate(task)
      : newTask({
          title: inboxItem?.text ?? "",
          description: inboxItem?.notes ?? "",
          // A converted thought lands in this week's pool until it is given a better place.
          ...(inboxItem ? { plannedWeek: thisWeek } : {}),
          ...defaults,
        }),
  );
  const [customEstimate, setCustomEstimate] = useState(
    () =>
      draft.estimatedMinutes !== null &&
      !(estimatePresets as readonly number[]).includes(draft.estimatedMinutes),
  );
  const [saving, setSaving] = useState(false);
  const dialogRef = useRef<HTMLFormElement>(null);
  useModalFocus(dialogRef, onClose, saving);
  const { milestones, workstreams } = usePlanLinks(draft.planId, onError);

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
    const input = pickTaskInput(draft, task);
    // A newly scheduled day also records its week, so returning the task to the pool keeps it there.
    if (
      input.scheduledDay !== null &&
      input.scheduledDay !== task?.scheduledDay
    )
      input.plannedWeek = weekStartDay(input.scheduledDay, weekStartsOn);

    void run(async () => {
      if (inboxItem) {
        await api.processInboxItem(inboxItem, { kind: "task", task: input });
      } else if (task) {
        await api.updateTask({
          ...input,
          id: task.id,
          revision: task.revision,
        });
      } else {
        await api.createTask(input);
      }
    });
  }

  function remove() {
    if (!task || !window.confirm(`Delete “${task.title}”?`)) return;
    void run(() => api.deleteTask(task.id, task.revision));
  }

  const weekChoice =
    draft.plannedWeek === null
      ? ""
      : inWeek(draft.plannedWeek, thisWeek)
        ? thisWeek
        : inWeek(draft.plannedWeek, nextWeek)
          ? nextWeek
          : draft.plannedWeek;
  const hasHome = taskHasHome(draft);
  // A finished task doesn't repeat; finishing an open repeating one here still brings the next.
  const finished = draft.status === "done" && (!task || task.status === "done");
  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(click) => {
        if (click.target === click.currentTarget && !saving) onClose();
      }}
    >
      <form
        ref={dialogRef}
        className="editor-dialog"
        onSubmit={submit}
        onKeyDown={submitOnCommandEnter}
        role="dialog"
        aria-modal="true"
        aria-label={inboxItem ? "Make a task" : task ? "Edit task" : "New task"}
      >
        <header>
          <div>
            <p>{inboxItem ? "FROM INBOX" : task ? "EDIT TASK" : "NEW TASK"}</p>
            <h2>
              {inboxItem
                ? "Make it a task"
                : task
                  ? "Shape the work"
                  : "Name the work"}
            </h2>
          </div>
          <button type="button" onClick={onClose} aria-label="Close editor">
            <Glyph>✕</Glyph>
          </button>
        </header>
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
            placeholder="What needs to be done?"
          />
        </label>
        <div className="form-pair">
          <label>
            Status
            <select
              value={draft.status}
              onChange={(input) =>
                setDraft({
                  ...draft,
                  status: input.target.value as TaskInput["status"],
                })
              }
            >
              {taskStatuses.map((status) => (
                <option key={status} value={status}>
                  {taskStatusLabels[status]}
                </option>
              ))}
            </select>
          </label>
          <label>
            Priority
            <select
              value={draft.priority}
              onChange={(input) =>
                setDraft({
                  ...draft,
                  priority: input.target.value as TaskInput["priority"],
                })
              }
            >
              {taskPriorities.map((priority) => (
                <option key={priority} value={priority}>
                  {taskPriorityLabels[priority]}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="form-pair">
          <label>
            Estimate
            <select
              value={
                customEstimate
                  ? "custom"
                  : draft.estimatedMinutes === null
                    ? ""
                    : String(draft.estimatedMinutes)
              }
              onChange={(input) => {
                const value = input.target.value;
                setCustomEstimate(value === "custom");
                if (value !== "custom")
                  setDraft({
                    ...draft,
                    estimatedMinutes: value === "" ? null : Number(value),
                  });
              }}
            >
              <option value="">No estimate</option>
              {estimatePresets.map((minutes) => (
                <option key={minutes} value={minutes}>
                  {estimateLabel(minutes)}
                </option>
              ))}
              <option value="custom">Custom…</option>
            </select>
          </label>
          <label>
            Week
            <select
              value={weekChoice}
              onChange={(input) =>
                setDraft({ ...draft, plannedWeek: input.target.value || null })
              }
            >
              <option value="">No week</option>
              <option value={thisWeek}>
                This week · from {shortDate(thisWeek)}
              </option>
              <option value={nextWeek}>
                Next week · from {shortDate(nextWeek)}
              </option>
              {weekChoice !== "" &&
                weekChoice !== thisWeek &&
                weekChoice !== nextWeek && (
                  <option value={weekChoice}>
                    {weekLabel(weekChoice, thisWeek)}
                  </option>
                )}
            </select>
          </label>
        </div>
        {customEstimate && (
          <label>
            Custom estimate <span>Minutes, up to 24 hours</span>
            <input
              type="number"
              min={1}
              max={1440}
              required
              value={draft.estimatedMinutes ?? ""}
              onChange={(input) =>
                setDraft({
                  ...draft,
                  estimatedMinutes:
                    input.target.value === ""
                      ? null
                      : Math.round(Number(input.target.value)),
                })
              }
            />
          </label>
        )}
        <div className="form-pair">
          <PlanSelect
            plans={plans}
            value={draft.planId}
            onChange={(planId) =>
              setDraft({
                ...draft,
                planId,
                milestoneId: null,
                workstreamId: null,
              })
            }
          />
          <label>
            Milestone
            <select
              value={draft.milestoneId ?? ""}
              disabled={!draft.planId || milestones.length === 0}
              onChange={(input) =>
                setDraft({ ...draft, milestoneId: input.target.value || null })
              }
            >
              <option value="">No milestone</option>
              {milestones.map((milestone) => (
                <option key={milestone.id} value={milestone.id}>
                  {milestone.title}
                  {milestone.targetDate
                    ? ` · ${shortDate(milestone.targetDate)}`
                    : ""}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="form-pair">
          <WorkstreamSelect
            workstreams={workstreams}
            value={draft.workstreamId}
            disabled={!draft.planId || workstreams.length === 0}
            onChange={(workstreamId) => setDraft({ ...draft, workstreamId })}
          />
          <PersonSelect
            people={people}
            value={draft.ownerId}
            onChange={(ownerId) => setDraft({ ...draft, ownerId })}
          />
        </div>
        <div className="form-pair">
          <OptionalDate
            label="Scheduled for"
            value={draft.scheduledDay}
            onChange={(scheduledDay) => setDraft({ ...draft, scheduledDay })}
          />
          <OptionalDate
            label="Due by"
            value={draft.dueDate}
            onChange={(dueDate) => setDraft({ ...draft, dueDate })}
          />
        </div>
        {!hasHome && (
          <p className="editor-hint" role="status">
            Choose a plan, a week, a scheduled day, or a due date so this task
            has a place to appear.
          </p>
        )}
        <RepeatField
          value={draft.recurrence}
          scheduledDay={draft.scheduledDay}
          finished={finished}
          onChange={(recurrence) => setDraft({ ...draft, recurrence })}
        />
        <label>
          Description
          <textarea
            value={draft.description}
            onChange={(input) =>
              setDraft({ ...draft, description: input.target.value })
            }
            placeholder="Context, links, or the definition of done"
            maxLength={2000}
          />
        </label>
        <ChecklistField
          items={draft.checklist}
          onChange={(checklist) => setDraft({ ...draft, checklist })}
        />
        <WaitsOnField
          taskId={task?.id}
          value={draft.waitingOn}
          plans={plans}
          onChange={(waitingOn) => setDraft({ ...draft, waitingOn })}
          onError={onError}
        />
        <footer>
          {task && (
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
            disabled={saving || !draft.title.trim() || !hasHome}
          >
            {saving && <Spinner size={7} />}
            {task ? "Save changes" : "Create task"}
          </button>
        </footer>
      </form>
    </div>
  );
}

function pickTaskInput(draft: TaskInput, task?: Task): TaskInput {
  // A rule needs a day to repeat from, and a task that was already finished can't take one.
  const keepsRule =
    draft.scheduledDay !== null &&
    !(draft.status === "done" && (!task || task.status === "done"));
  return {
    title: draft.title,
    description: draft.description,
    planId: draft.planId,
    milestoneId: draft.milestoneId,
    workstreamId: draft.workstreamId,
    ownerId: draft.ownerId,
    dueDate: draft.dueDate,
    scheduledDay: draft.scheduledDay,
    plannedWeek: draft.plannedWeek,
    estimatedMinutes: draft.estimatedMinutes,
    status: draft.status,
    priority: draft.priority,
    recurrence: keepsRule ? draft.recurrence : null,
    checklist: draft.checklist
      .map((item) => ({ ...item, text: item.text.trim() }))
      .filter((item) => item.text !== ""),
    waitingOn: draft.waitingOn,
  };
}

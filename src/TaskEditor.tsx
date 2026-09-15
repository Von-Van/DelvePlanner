import { FormEvent, useRef, useState } from "react";
import {
  api,
  messageFor,
  Person,
  Plan,
  Task,
  TaskInput,
  taskPriorities,
  taskStatuses,
} from "./api";
import { Glyph, Spinner } from "./Geometry";
import {
  OptionalDate,
  PersonSelect,
  PlanSelect,
  WorkstreamSelect,
} from "./PlanControls";
import {
  newTask,
  shortDate,
  taskHasHome,
  taskPriorityLabels,
  taskStatusLabels,
  taskUpdate,
} from "./planning";
import { useModalFocus } from "./useModalFocus";
import { usePlanLinks } from "./usePlanLinks";

export function TaskEditor({
  task,
  defaults,
  plans,
  people,
  onClose,
  onSaved,
  onError,
}: {
  task?: Task;
  defaults?: Partial<TaskInput>;
  plans: Plan[];
  people: Person[];
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<TaskInput>(() =>
    task ? taskUpdate(task) : newTask({ title: "", ...defaults }),
  );
  const [saving, setSaving] = useState(false);
  const dialogRef = useRef<HTMLFormElement>(null);
  useModalFocus(dialogRef, onClose, saving);
  const { milestones, workstreams } = usePlanLinks(draft.planId, onError);

  async function run(action: () => Promise<void>) {
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
    const input = pickTaskInput(draft);

    void run(async () => {
      if (task) {
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

  const hasHome = taskHasHome(draft);
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
        role="dialog"
        aria-modal="true"
        aria-label={task ? "Edit task" : "New task"}
      >
        <header>
          <div>
            <p>{task ? "EDIT TASK" : "NEW TASK"}</p>
            <h2>{task ? "Shape the work" : "Name the work"}</h2>
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
            Choose a plan, a scheduled day, or a due date so this task has a
            place to appear.
          </p>
        )}
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

function pickTaskInput(draft: TaskInput): TaskInput {
  return {
    title: draft.title,
    description: draft.description,
    planId: draft.planId,
    milestoneId: draft.milestoneId,
    workstreamId: draft.workstreamId,
    ownerId: draft.ownerId,
    dueDate: draft.dueDate,
    scheduledDay: draft.scheduledDay,
    status: draft.status,
    priority: draft.priority,
  };
}

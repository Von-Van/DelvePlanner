import { FormEvent, useState } from "react";
import { api, messageFor, Task } from "./api";
import { localeWeekStart, offsetDay, weekdayShort } from "./date";
import type { MenuItem } from "./Menu";
import { EditorShell } from "./PlanEditor";
import { canUnschedule, inWeek, MoveTarget, taskMove } from "./planning";

export const weekStartsOn = localeWeekStart();

/**
 * Moves tasks between Plan, Week, and Today through one revision-checked batch, and owns the
 * "Pick a day" dialog. Render `dialog` somewhere in the calling view.
 */
export function useTaskMover({
  today,
  onChanged,
  onError,
}: {
  today: string;
  onChanged: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [picking, setPicking] = useState<Task[] | null>(null);
  const [busyIds, setBusyIds] = useState<ReadonlySet<string>>(new Set());

  async function move(tasks: Task[], target: MoveTarget) {
    if (!tasks.length) return;
    setBusyIds(new Set(tasks.map((task) => task.id)));
    try {
      await api.moveTasks(
        tasks.map((task) => taskMove(task, target, weekStartsOn)),
      );
      await onChanged();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setBusyIds(new Set());
    }
  }

  const dialog = picking && (
    <PickDayDialog
      tasks={picking}
      today={today}
      onClose={() => setPicking(null)}
      onPick={(day) => {
        setPicking(null);
        void move(picking, { kind: "day", day });
      }}
    />
  );

  return {
    move,
    pickDay: (tasks: Task[]) => setPicking(tasks),
    busyIds,
    dialog,
  };
}

/**
 * The move menu for one task: onto today, tomorrow, or a picked day; into this or next week's
 * pool; back out of every week and day when its plan or due date keeps it visible; or done.
 */
export function taskMoveItems(
  task: Task,
  {
    today,
    weekStart,
    move,
    pickDay,
  }: {
    today: string;
    weekStart: string;
    move: (tasks: Task[], target: MoveTarget) => void;
    pickDay: (tasks: Task[]) => void;
  },
): MenuItem[] {
  const tomorrow = offsetDay(today, 1);
  const nextWeek = offsetDay(weekStart, 7);
  const pooled = (week: string) =>
    task.scheduledDay === null && inWeek(task.plannedWeek, week);
  const items: MenuItem[] = [];
  if (task.scheduledDay !== today)
    items.push({
      label: "Today",
      onSelect: () => move([task], { kind: "day", day: today }),
    });
  if (task.scheduledDay !== tomorrow)
    items.push({
      label: "Tomorrow",
      onSelect: () => move([task], { kind: "day", day: tomorrow }),
    });
  items.push({ label: "Pick a day…", onSelect: () => pickDay([task]) });
  items.push({ kind: "separator" });
  if (!pooled(weekStart))
    items.push({
      label: "This week, no day",
      onSelect: () => move([task], { kind: "week", weekStart }),
    });
  if (!pooled(nextWeek))
    items.push({
      label: "Next week",
      onSelect: () => move([task], { kind: "week", weekStart: nextWeek }),
    });
  if (task.scheduledDay !== null || task.plannedWeek !== null)
    items.push({
      label: task.planId ? "Back to its plan" : "Remove from schedule",
      disabled: !canUnschedule(task),
      hint: canUnschedule(task) ? undefined : "Needs a plan or due date",
      onSelect: () => move([task], { kind: "unschedule" }),
    });
  if (task.status !== "done") {
    items.push({ kind: "separator" });
    items.push({
      label: "Mark done",
      onSelect: () => move([task], { kind: "done" }),
    });
  }
  return items;
}

function PickDayDialog({
  tasks,
  today,
  onPick,
  onClose,
}: {
  tasks: Task[];
  today: string;
  onPick: (day: string) => void;
  onClose: () => void;
}) {
  const [day, setDay] = useState(() => offsetDay(today, 1));
  const days = Array.from({ length: 7 }, (_, index) => offsetDay(today, index));
  function submit(form: FormEvent) {
    form.preventDefault();
    if (day) onPick(day);
  }
  return (
    <EditorShell
      label="Pick a day"
      kicker={tasks.length === 1 ? "MOVE TASK" : `MOVE ${tasks.length} TASKS`}
      heading="Pick a day"
      busy={false}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button className="primary-button small" disabled={!day}>
            Move
          </button>
        </>
      }
    >
      {tasks.length === 1 && <p className="dialog-subject">{tasks[0].title}</p>}
      <div
        className="day-choices"
        role="group"
        aria-label="The next seven days"
      >
        {days.map((candidate, index) => (
          <button
            type="button"
            key={candidate}
            autoFocus={index === 1}
            className={`day-chip ${candidate === day ? "selected" : ""} ${candidate === today ? "today" : ""}`}
            aria-pressed={candidate === day}
            onClick={() => setDay(candidate)}
          >
            <span>
              {candidate === today
                ? "TODAY"
                : weekdayShort(candidate).toUpperCase()}
            </span>
            <strong>{candidate.slice(-2)}</strong>
          </button>
        ))}
      </div>
      <label>
        Or another date
        <input
          type="date"
          value={day}
          required
          onChange={(input) => setDay(input.target.value)}
        />
      </label>
    </EditorShell>
  );
}

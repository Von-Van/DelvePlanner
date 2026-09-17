import { FormEvent, useState } from "react";
import { api, messageFor, ScheduledBlock, Task } from "./api";
import { BlockEditor } from "./BlockEditor";
import { localeWeekStart, offsetDay, weekdayShort } from "./date";
import type { MenuItem } from "./Menu";
import { EditorShell } from "./PlanEditor";
import { canUnschedule, inWeek, MoveTarget, taskMove } from "./planning";

export const weekStartsOn = localeWeekStart();

/**
 * Moves tasks between Plan, Week, and Today through one revision-checked batch, and owns the
 * "Pick a day" and time block dialogs. Render `dialog` somewhere in the calling view.
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
  const [blocking, setBlocking] = useState<{
    task: Task;
    scheduled?: ScheduledBlock;
  } | null>(null);
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

  const dialog = (
    <>
      {picking && (
        <PickDayDialog
          tasks={picking}
          today={today}
          onClose={() => setPicking(null)}
          onPick={(day) => {
            setPicking(null);
            void move(picking, { kind: "day", day });
          }}
        />
      )}
      {blocking && (
        <BlockEditor
          task={blocking.task}
          block={blocking.scheduled?.block}
          today={today}
          defaultDay={
            blocking.task.scheduledDay && blocking.task.scheduledDay >= today
              ? blocking.task.scheduledDay
              : today
          }
          onClose={() => setBlocking(null)}
          onSaved={async () => {
            setBlocking(null);
            await onChanged();
          }}
          onError={onError}
        />
      )}
    </>
  );

  return {
    move,
    pickDay: (tasks: Task[]) => setPicking(tasks),
    /** Opens the time block dialog for a new block on `task`. */
    blockTime: (task: Task) => setBlocking({ task }),
    /** Opens the time block dialog to move or remove an existing block. */
    editBlock: (scheduled: ScheduledBlock) =>
      setBlocking({ task: scheduled.task, scheduled }),
    busyIds,
    dialog,
  };
}

/**
 * The move menu for one task: onto today, tomorrow, or a picked day; into this or next week's
 * pool; back out of every week and day when its plan or due date keeps it visible; time reserved
 * for it; or done.
 */
export function taskMoveItems(
  task: Task,
  {
    today,
    weekStart,
    move,
    pickDay,
    blockTime,
  }: {
    today: string;
    weekStart: string;
    move: (tasks: Task[], target: MoveTarget) => void;
    pickDay: (tasks: Task[]) => void;
    blockTime?: (task: Task) => void;
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
    if (blockTime)
      items.push({ label: "Block time…", onSelect: () => blockTime(task) });
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

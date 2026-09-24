import { useState } from "react";
import type {
  ChecklistItem,
  Milestone,
  Person,
  Plan,
  Task,
  Workstream,
} from "./api";
import { Glyph, Mark } from "./Geometry";
import { Menu, MenuItem } from "./Menu";
import { useOpenTasks } from "./openTasks";
import { PlanChip } from "./PlanControls";
import {
  checklistProgress,
  estimateLabel,
  isOverdue,
  openWaits,
  recurrenceLabel,
  shortDate,
  taskPriorityLabels,
  taskStatusLabels,
  weekLabel,
} from "./planning";

export function TaskRow({
  task,
  today,
  plan,
  milestone,
  workstream,
  owner,
  showScheduledDay = false,
  weekStart,
  busy = false,
  addLabel = "Schedule for this day",
  moveItems,
  onToggle,
  onOpen,
  onDelete,
  onScheduleHere,
  onChecklistChange,
}: {
  task: Task;
  today: string;
  plan?: Plan;
  milestone?: Milestone;
  workstream?: Workstream;
  owner?: Person;
  showScheduledDay?: boolean;
  /** The current week's first day; when set, a task chosen for a week without a day says which. */
  weekStart?: string;
  busy?: boolean;
  /** What the add button does, such as "Schedule for this day" or "Add to this week". */
  addLabel?: string;
  /** Offers moves between Plan, Week, and Today from a menu at the end of the row. */
  moveItems?: MenuItem[];
  onToggle: () => void;
  onOpen: () => void;
  onDelete?: () => void;
  onScheduleHere?: () => void;
  /** Saves the checklist after an item is ticked in the row; without it, ticking needs the editor. */
  onChecklistChange?: (checklist: ChecklistItem[]) => void;
}) {
  const done = task.status === "done";
  const flagged = task.priority === "high" || task.priority === "critical";
  const moving = task.status === "in_progress" || task.status === "blocked";
  const openTasks = useOpenTasks();
  const [expanded, setExpanded] = useState(false);
  const progress = checklistProgress(task);
  const waits = done ? [] : openWaits(task, openTasks);
  const checklistOpen = expanded && progress !== null && !!onChecklistChange;
  return (
    <div
      className={`task-row ${done ? "done" : ""} ${checklistOpen ? "expanded" : ""}`}
    >
      <button
        onClick={onToggle}
        className="check-box"
        disabled={busy}
        aria-label={`Mark ${task.title} ${done ? "not done" : "done"}`}
      />
      <button
        className="task-open"
        onClick={onOpen}
        aria-label={`Edit ${task.title}`}
      >
        <span className="task-title">{task.title}</span>
        <span className="task-meta">
          {moving && (
            <small className={`task-status ${task.status}`}>
              {taskStatusLabels[task.status]}
            </small>
          )}
          {flagged && (
            <small className={`task-priority ${task.priority}`}>
              {taskPriorityLabels[task.priority]}
            </small>
          )}
          {task.estimatedMinutes !== null && (
            <small className="task-estimate">
              {estimateLabel(task.estimatedMinutes)}
            </small>
          )}
          {task.recurrence && (
            <small
              className="task-repeat"
              title={recurrenceLabel(task.recurrence)}
            >
              <Glyph>↻</Glyph> {recurrenceLabel(task.recurrence)}
            </small>
          )}
          {progress && (
            <small
              className={`task-checklist ${progress.done === progress.total ? "complete" : ""}`}
              aria-label={`${progress.done} of ${progress.total} checklist items done`}
            >
              {progress.done}/{progress.total}
            </small>
          )}
          {waits.length > 0 && (
            <small
              className="task-waiting"
              title={waits.map((wait) => wait.title).join(", ")}
            >
              Waiting on{" "}
              {waits.length === 1 ? waits[0].title : `${waits.length} tasks`}
            </small>
          )}
          {milestone && (
            <small>
              <Mark size={5} filled={milestone.status === "complete"} />
              {milestone.title}
            </small>
          )}
          {workstream && (
            <small className="task-link">
              <Mark shape="rule" size={5} /> {workstream.name}
            </small>
          )}
          {owner && (
            <small className="task-link" title={`Owner: ${owner.displayName}`}>
              <Mark shape="circle" size={6} /> {owner.displayName}
            </small>
          )}
          {weekStart && task.scheduledDay === null && task.plannedWeek && (
            <small className="task-week">
              {weekLabel(task.plannedWeek, weekStart)}
            </small>
          )}
          {showScheduledDay && task.scheduledDay && (
            <small>On {shortDate(task.scheduledDay)}</small>
          )}
          {task.dueDate && (
            <small
              className={!done && isOverdue(task.dueDate, today) ? "late" : ""}
            >
              Due {shortDate(task.dueDate)}
            </small>
          )}
          <PlanChip plan={plan} />
        </span>
      </button>
      {progress && onChecklistChange && (
        <button
          className="task-action task-expand"
          onClick={() => setExpanded(!expanded)}
          aria-expanded={checklistOpen}
          aria-label={`${checklistOpen ? "Hide" : "Show"} the checklist for ${task.title}`}
          title={checklistOpen ? "Hide checklist" : "Show checklist"}
        >
          <Glyph>{checklistOpen ? "▾" : "▸"}</Glyph>
        </button>
      )}
      {onScheduleHere && (
        <button
          className="task-action"
          onClick={onScheduleHere}
          disabled={busy}
          title={addLabel}
          aria-label={`${addLabel}: ${task.title}`}
        >
          <Glyph>+</Glyph>
        </button>
      )}
      {moveItems && (
        <Menu
          label={`Move ${task.title}`}
          trigger={<Glyph>⋯</Glyph>}
          items={moveItems}
          className="task-action"
          disabled={busy}
        />
      )}
      {onDelete && (
        <button
          className="task-delete"
          onClick={onDelete}
          disabled={busy}
          aria-label={`Delete ${task.title}`}
        >
          <Glyph>✕</Glyph>
        </button>
      )}
      {checklistOpen && (
        <ul
          className="task-checklist-items"
          aria-label={`${task.title} checklist`}
        >
          {task.checklist.map((item, index) => (
            <li key={index}>
              <label>
                <input
                  type="checkbox"
                  checked={item.done}
                  disabled={busy}
                  onChange={() =>
                    onChecklistChange(
                      task.checklist.map((current, position) =>
                        position === index
                          ? { ...current, done: !current.done }
                          : current,
                      ),
                    )
                  }
                />
                <span>{item.text}</span>
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

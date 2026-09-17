import type { Milestone, Person, Plan, Task, Workstream } from "./api";
import { Glyph, Mark } from "./Geometry";
import { Menu, MenuItem } from "./Menu";
import { PlanChip } from "./PlanControls";
import {
  estimateLabel,
  isOverdue,
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
}) {
  const done = task.status === "done";
  const flagged = task.priority === "high" || task.priority === "critical";
  const moving = task.status === "in_progress" || task.status === "blocked";
  return (
    <div className={`task-row ${done ? "done" : ""}`}>
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
    </div>
  );
}

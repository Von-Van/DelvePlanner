import type { Milestone, Person, Plan, Task, Workstream } from "./api";
import { Glyph, Mark } from "./Geometry";
import { PlanChip } from "./PlanControls";
import {
  isOverdue,
  shortDate,
  taskPriorityLabels,
  taskStatusLabels,
} from "./planning";

export function TaskRow({
  task,
  today,
  plan,
  milestone,
  workstream,
  owner,
  showScheduledDay = false,
  busy = false,
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
  busy?: boolean;
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
          title="Schedule for this day"
          aria-label={`Schedule ${task.title} for this day`}
        >
          <Glyph>+</Glyph>
        </button>
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

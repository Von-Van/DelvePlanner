import {
  CSSProperties,
  FormEvent,
  ReactNode,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  api,
  DayPlanError,
  messageFor,
  Milestone,
  Person,
  Plan,
  PlanWorkspace,
  ScheduleEvent,
  Task,
  taskStatuses,
  Workstream,
} from "./api";
import {
  dateTimeFields,
  dayLabel,
  dayMonthShort,
  timeLabel,
  weekStartDay,
} from "./date";
import { EventEditor } from "./EventEditor";
import { reminderShortLabel } from "./events";
import { Glyph, Mark, Spinner } from "./Geometry";
import { StatusPill, TabList, tabPanelProps } from "./PlanControls";
import { MilestoneEditor, PlanEditor, WorkstreamEditor } from "./PlanEditor";
import {
  allTasks,
  attentionSignals,
  compareMilestones,
  defaultRunOfShowDay,
  eventDay,
  eventDays,
  eventHasEnded,
  featuredTasks,
  filterTasks,
  groupTasksByStatus,
  isOverdue,
  longDate,
  milestoneStatusLabels,
  newTask,
  nextMilestone,
  overdueTaskCount,
  planColor,
  planDeletionMessage,
  planDeletionPreview,
  planSchedule,
  planUpdate,
  plural,
  relativeDayLabel,
  runOfShow,
  shortDate,
  taskCounts,
  TaskFilters,
  taskPriorityLabels,
  taskStatusLabels,
  taskUpdate,
  timelineModel,
  TimelineModel,
  upcomingItems,
  UpcomingItem,
  workstreamProgress,
} from "./planning";
import type { MenuItem } from "./Menu";
import { TaskEditor } from "./TaskEditor";
import { TaskRow } from "./TaskRow";
import { useHeadingFocus } from "./useHeadingFocus";
import { taskMoveItems, useTaskMover, weekStartsOn } from "./useTaskMover";

export type PlanTab = "overview" | "timeline" | "tasks" | "schedule" | "show";

type Editor =
  | { kind: "plan" }
  | { kind: "milestone"; milestone?: Milestone }
  | { kind: "task"; task?: Task }
  | { kind: "event"; event?: ScheduleEvent; day?: string; time?: string }
  | { kind: "workstream"; workstream?: Workstream };

export function PlanView({
  planId,
  plans,
  people,
  today,
  reloadToken,
  focusToken,
  planner,
  tab,
  onTab,
  onBack,
  onPlansChanged,
  onMessage,
}: {
  planId: string;
  plans: Plan[];
  people: Person[];
  today: string;
  /** Changes when data was replaced outside this view, such as an import or backup restore. */
  reloadToken: number;
  focusToken: number;
  /** The local planner card, scoped to this plan. */
  planner?: ReactNode;
  tab: PlanTab;
  onTab: (tab: PlanTab) => void;
  onBack: () => void;
  onPlansChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [workspace, setWorkspace] = useState<PlanWorkspace | null>(null);
  const [missing, setMissing] = useState(false);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const onMessageRef = useRef(onMessage);
  onMessageRef.current = onMessage;
  useHeadingFocus(headingRef, focusToken, workspace !== null);
  const weekStart = weekStartDay(today, weekStartsOn);
  const mover = useTaskMover({
    today,
    onChanged: () => changed(),
    onError: onMessage,
  });

  const refresh = useCallback(async () => {
    try {
      setWorkspace(await api.getPlanWorkspace(planId));
    } catch (cause) {
      if (cause instanceof DayPlanError && cause.code === "not_found")
        setMissing(true);
      else onMessageRef.current(messageFor(cause));
    }
  }, [planId]);

  useEffect(() => {
    setWorkspace(null);
    setMissing(false);
    setEditor(null);
    void refresh();
  }, [refresh, reloadToken]);

  async function changed() {
    await Promise.all([refresh(), onPlansChanged()]);
  }

  async function run(key: string, action: () => Promise<unknown>) {
    setBusy(key);
    try {
      await action();
      await changed();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusy(null);
    }
  }

  const closeEditor = () => setEditor(null);
  const savedFromEditor = async () => {
    setEditor(null);
    await changed();
  };

  if (missing)
    return (
      <div className="plan-page">
        <button className="back-link" onClick={onBack}>
          <Glyph>←</Glyph> All plans
        </button>
        <div className="empty-agenda">
          <i className="empty-mark" aria-hidden="true" />
          <p>This plan no longer exists.</p>
        </div>
      </div>
    );
  if (!workspace)
    return (
      <div className="loading-line">
        <Spinner size={10} />
        Opening this plan
      </div>
    );

  const { plan, milestones, tasks, events, workstreams } = workspace;
  const lookups: Lookups = {
    milestones: new Map(milestones.map((item) => [item.id, item])),
    workstreams: new Map(workstreams.map((item) => [item.id, item])),
    people: new Map(people.map((item) => [item.id, item])),
  };

  function toggleTask(task: Task) {
    void run(`task-${task.id}`, () =>
      api.updateTask({
        ...taskUpdate(task),
        status: task.status === "done" ? "todo" : "done",
      }),
    );
  }

  async function addTask(title: string, filters: TaskFilters) {
    const linked = (value: string) =>
      value === "all" || value === "none" ? null : value;
    try {
      await api.createTask(
        newTask({
          title,
          planId: plan.id,
          workstreamId: linked(filters.workstreamId),
          ownerId: linked(filters.ownerId),
          milestoneId: linked(filters.milestoneId),
        }),
      );
      await changed();
      return true;
    } catch (cause) {
      onMessage(messageFor(cause));
      return false;
    }
  }

  function removeEvent(event: ScheduleEvent) {
    if (!window.confirm(`Delete “${event.title}”?`)) return;
    void run(`event-${event.id}`, () =>
      api.deleteEvent(event.id, event.revision),
    );
  }

  function removePlan() {
    if (
      !window.confirm(
        planDeletionMessage(plan.title, planDeletionPreview(workspace!)),
      )
    )
      return;
    setBusy("delete");
    api
      .deletePlan(plan.id, plan.revision)
      .then(async () => {
        await onPlansChanged();
        onMessage(`Deleted “${plan.title}”.`);
        onBack();
      })
      .catch((cause) => {
        onMessage(messageFor(cause));
        setBusy(null);
      });
  }

  function archivePlan() {
    setEditor(null);
    void run("archive", () => api.updatePlan(planUpdate(plan, true)));
  }

  function openItem(kind: UpcomingItem["kind"], id: string) {
    if (kind === "milestone")
      setEditor({ kind: "milestone", milestone: lookups.milestones.get(id) });
    if (kind === "task")
      setEditor({ kind: "task", task: tasks.find((task) => task.id === id) });
    if (kind === "event")
      setEditor({
        kind: "event",
        event: events.find((event) => event.id === id),
      });
  }

  const panelId = `plan-${plan.id}`;
  return (
    <div
      className="plan-page"
      style={{ "--plan-color": planColor(plan) } as CSSProperties}
    >
      <button className="back-link" onClick={onBack}>
        <Glyph>←</Glyph> All plans
      </button>
      <header className="topbar plan-header">
        <div className="date-heading">
          <div className="plan-kicker">
            <Mark color={planColor(plan)} filled />
            <p>PLAN</p>
            <StatusPill status={plan.status} />
          </div>
          <h1 ref={headingRef} tabIndex={-1}>
            {plan.title}
          </h1>
          <span className="plan-dates">{planSchedule(plan, today)}</span>
        </div>
        <div className="date-controls">
          <button
            className="secondary-button"
            onClick={() => setEditor({ kind: "plan" })}
          >
            Edit plan
          </button>
          <button
            className="primary-button"
            onClick={() => setEditor({ kind: "milestone" })}
          >
            <Mark filled />
            New milestone
          </button>
        </div>
      </header>
      {plan.description && (
        <p className="plan-description">{plan.description}</p>
      )}
      {plan.archived && (
        <div className="archived-banner" role="status">
          <span>
            This plan is archived. Its dated tasks and events still appear on
            their days.
          </span>
          <button
            disabled={busy !== null}
            onClick={() =>
              void run("archive", () => api.updatePlan(planUpdate(plan, false)))
            }
          >
            Restore
          </button>
          <button
            className="danger"
            disabled={busy !== null}
            onClick={removePlan}
          >
            Delete permanently
          </button>
        </div>
      )}

      <TabList
        id={panelId}
        label="Plan views"
        value={tab}
        onChange={onTab}
        tabs={[
          { key: "overview", label: "Overview" },
          { key: "tasks", label: "Tasks", count: tasks.length },
          { key: "schedule", label: "Schedule", count: events.length },
          { key: "timeline", label: "Timeline" },
          { key: "show", label: "Run of show" },
        ]}
      />

      <section {...tabPanelProps(panelId, tab)} className="plan-panel">
        {tab === "overview" && (
          <Overview
            workspace={workspace}
            today={today}
            planner={planner}
            onOpenItem={openItem}
            onAddMilestone={() => setEditor({ kind: "milestone" })}
            onOpenMilestone={(milestone) =>
              setEditor({ kind: "milestone", milestone })
            }
            onOpenTask={(task) => setEditor({ kind: "task", task })}
            onEditWorkstream={(workstream) =>
              setEditor({ kind: "workstream", workstream })
            }
            onTab={onTab}
          />
        )}
        {tab === "timeline" && (
          <TimelinePanel
            plan={plan}
            milestones={milestones}
            events={events}
            tasks={tasks}
            today={today}
            onOpenMilestone={(milestone) =>
              setEditor({ kind: "milestone", milestone })
            }
          />
        )}
        {tab === "tasks" && (
          <TasksPanel
            plan={plan}
            workspace={workspace}
            people={people}
            lookups={lookups}
            today={today}
            weekStart={weekStart}
            busy={busy}
            busyIds={mover.busyIds}
            moveItemsFor={(task) =>
              taskMoveItems(task, {
                today,
                weekStart,
                move: (tasks, target) => void mover.move(tasks, target),
                pickDay: mover.pickDay,
                blockTime: mover.blockTime,
              })
            }
            onAdd={addTask}
            onToggle={toggleTask}
            onOpen={(task) => setEditor({ kind: "task", task })}
          />
        )}
        {tab === "schedule" && (
          <SchedulePanel
            events={events}
            lookups={lookups}
            today={today}
            onNew={() => setEditor({ kind: "event" })}
            onOpen={(event) => setEditor({ kind: "event", event })}
            onDelete={removeEvent}
          />
        )}
        {tab === "show" && (
          <RunOfShowPanel
            events={events}
            lookups={lookups}
            today={today}
            onOpen={(event) => setEditor({ kind: "event", event })}
            onAdd={(day, time) => setEditor({ kind: "event", day, time })}
          />
        )}
      </section>

      {mover.dialog}
      {editor?.kind === "plan" && (
        <PlanEditor
          plan={plan}
          defaultColor="sage"
          onClose={closeEditor}
          onSaved={savedFromEditor}
          onArchive={plan.archived ? undefined : archivePlan}
          onError={onMessage}
        />
      )}
      {editor?.kind === "milestone" && (
        <MilestoneEditor
          planId={plan.id}
          milestone={editor.milestone}
          workstreams={workstreams}
          onClose={closeEditor}
          onSaved={savedFromEditor}
          onError={onMessage}
        />
      )}
      {editor?.kind === "workstream" && (
        <WorkstreamEditor
          planId={plan.id}
          workstream={editor.workstream}
          onClose={closeEditor}
          onSaved={savedFromEditor}
          onError={onMessage}
        />
      )}
      {editor?.kind === "task" && (
        <TaskEditor
          task={editor.task}
          defaults={{ planId: plan.id }}
          plans={plans}
          people={people}
          onClose={closeEditor}
          onSaved={savedFromEditor}
          onError={onMessage}
        />
      )}
      {editor?.kind === "event" && (
        <EventEditor
          day={editor.day ?? today}
          defaultTime={editor.time}
          event={editor.event}
          plans={plans}
          people={people}
          defaultPlanId={plan.id}
          onClose={closeEditor}
          onSaved={savedFromEditor}
          onError={onMessage}
        />
      )}
    </div>
  );
}

type Lookups = {
  milestones: Map<string, Milestone>;
  workstreams: Map<string, Workstream>;
  people: Map<string, Person>;
};

function Overview({
  workspace,
  today,
  planner,
  onOpenItem,
  onAddMilestone,
  onOpenMilestone,
  onOpenTask,
  onEditWorkstream,
  onTab,
}: {
  workspace: PlanWorkspace;
  today: string;
  planner?: ReactNode;
  onOpenItem: (kind: UpcomingItem["kind"], id: string) => void;
  onAddMilestone: () => void;
  onOpenMilestone: (milestone: Milestone) => void;
  onOpenTask: (task: Task) => void;
  onEditWorkstream: (workstream?: Workstream) => void;
  onTab: (tab: PlanTab) => void;
}) {
  const color = planColor(workspace.plan);
  const next = nextMilestone(workspace.milestones);
  const counts = taskCounts(workspace.tasks);
  const overdue = overdueTaskCount(workspace.tasks, today);
  const featured = featuredTasks(workspace.tasks);
  const upcoming = upcomingItems(workspace, today, new Date(), 6);
  const signals = attentionSignals(workspace, today);
  const streams = workstreamProgress(workspace.workstreams, workspace.tasks);
  const milestones = [...workspace.milestones].sort(compareMilestones);
  const linked = next
    ? workspace.tasks.filter((task) => task.milestoneId === next.id)
    : [];
  const linkedDone = linked.filter((task) => task.status === "done").length;
  return (
    <div className="overview-grid">
      <div className="overview-main">
        <section className="overview-card glass">
          <p className="card-kicker">NEXT MILESTONE</p>
          {next ? (
            <>
              <button
                className="milestone-feature"
                onClick={() => onOpenMilestone(next)}
              >
                <strong>{next.title}</strong>
                <span
                  className={isOverdue(next.targetDate, today) ? "late" : ""}
                >
                  {next.targetDate
                    ? `${relativeDayLabel(next.targetDate, today)} · ${shortDate(next.targetDate)}`
                    : "No date yet"}
                </span>
                <small>
                  {linked.length
                    ? `${linkedDone} of ${plural(linked.length, "task")} complete`
                    : "No tasks linked yet"}
                </small>
              </button>
              {linked.length > 0 && (
                <span className="plan-progress" aria-hidden="true">
                  <i
                    style={{ width: `${(linkedDone / linked.length) * 100}%` }}
                  />
                </span>
              )}
            </>
          ) : (
            <p className="card-empty">
              No pending milestones.{" "}
              <button className="text-button" onClick={onAddMilestone}>
                Add a checkpoint
              </button>
            </p>
          )}
        </section>
        {signals.length > 0 && (
          <section className="overview-card attention">
            <p className="card-kicker">
              <Mark size={6} color="var(--danger-dot)" filled />
              NEEDS ATTENTION
            </p>
            <ul className="attention-list">
              {signals.map((signal) => (
                <li key={signal.key} className={signal.tone}>
                  <button
                    onClick={() =>
                      signal.milestoneId
                        ? onOpenItem("milestone", signal.milestoneId)
                        : signal.taskIds.length === 1
                          ? onOpenItem("task", signal.taskIds[0])
                          : onTab("tasks")
                    }
                  >
                    {signal.text}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}
        <section className="overview-card">
          <p className="card-kicker">TASKS</p>
          <div className="task-stats">
            <TaskStat label="In progress" value={counts.in_progress} />
            <TaskStat label="Blocked" value={counts.blocked} />
            <TaskStat label="Done" value={counts.done} />
            <TaskStat label="Overdue" value={overdue} late={overdue > 0} />
          </div>
          {featured.length === 0 ? (
            <p className="card-empty">
              {counts.total
                ? "Every task in this plan is done."
                : "No tasks yet. Capture the work now; schedule it when you know when."}
            </p>
          ) : (
            <ul className="featured-tasks">
              {featured.map((task) => (
                <li key={task.id}>
                  <button onClick={() => onOpenTask(task)}>
                    <Mark size={15} />
                    <span>{task.title}</span>
                    <TaskPill task={task} />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button className="text-button" onClick={() => onTab("tasks")}>
            {counts.total
              ? `See all ${plural(counts.total, "task")}`
              : "Open the task list"}{" "}
            <Glyph>→</Glyph>
          </button>
        </section>
        <section className="overview-card">
          <div className="card-heading">
            <p className="card-kicker">WORKSTREAMS</p>
            <button
              className="text-button"
              onClick={() => onEditWorkstream(undefined)}
            >
              <Glyph>+</Glyph> Add
            </button>
          </div>
          {streams.length === 0 ? (
            <p className="card-empty">
              Group related work, such as Production or Sponsors, to see
              progress by stream.
            </p>
          ) : (
            <ul className="workstream-list">
              {streams.map(({ workstream, done, total }) => (
                <li key={workstream.id}>
                  <button onClick={() => onEditWorkstream(workstream)}>
                    <span>{workstream.name}</span>
                    <small>
                      {total ? `${done} / ${total} complete` : "No tasks yet"}
                    </small>
                    <span className="plan-progress" aria-hidden="true">
                      <i
                        style={{
                          width: `${total ? (done / total) * 100 : 0}%`,
                        }}
                      />
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>
      <div className="overview-main">
        <section className="overview-card">
          <p className="card-kicker">UPCOMING</p>
          {upcoming.length === 0 ? (
            <p className="card-empty">
              Nothing dated ahead. Milestones, due dates, and events appear
              here.
            </p>
          ) : (
            <ol className="upcoming-list">
              {upcoming.map((item) => (
                <li key={item.key}>
                  <button onClick={() => onOpenItem(item.kind, item.id)}>
                    <time dateTime={item.day}>{shortDate(item.day)}</time>
                    <span>
                      <strong>{item.title}</strong>
                      <small>{item.detail}</small>
                    </span>
                    <Glyph>→</Glyph>
                  </button>
                </li>
              ))}
            </ol>
          )}
        </section>
        <section className="overview-card">
          <p className="card-kicker">MILESTONES</p>
          {milestones.length === 0 ? (
            <p className="card-empty">
              Milestones mark the checkpoints the work builds toward.
            </p>
          ) : (
            <ol className="milestone-rail">
              {milestones.map((milestone) => {
                const isNext = milestone.id === next?.id;
                const late =
                  milestone.status === "pending" &&
                  isOverdue(milestone.targetDate, today);
                return (
                  <li key={milestone.id}>
                    <button
                      className={`${milestone.status} ${isNext ? "next" : ""} ${late ? "late" : ""}`}
                      onClick={() => onOpenMilestone(milestone)}
                    >
                      <Mark
                        color={
                          milestone.status === "complete"
                            ? undefined
                            : isNext
                              ? color
                              : "var(--silver)"
                        }
                        filled={milestone.status === "complete"}
                        dashed={milestone.status === "skipped"}
                      />
                      <span>{milestone.title}</span>
                      <small>
                        {milestone.targetDate
                          ? shortDate(milestone.targetDate)
                          : "No date"}
                      </small>
                    </button>
                  </li>
                );
              })}
            </ol>
          )}
        </section>
        {planner}
        <section className="note-card">
          <i aria-hidden="true" />
          <div>
            <strong>Revision-checked</strong>
            <p>
              Every edit carries a revision number. Conflicting writes are
              refused, not merged.
            </p>
          </div>
        </section>
      </div>
    </div>
  );
}

/** One pill for a task: its status when work is moving, otherwise a raised priority. */
function TaskPill({ task }: { task: Task }) {
  if (task.status === "in_progress" || task.status === "blocked")
    return (
      <span className={`pill ${task.status}`}>
        {taskStatusLabels[task.status]}
      </span>
    );
  if (task.priority === "high" || task.priority === "critical")
    return (
      <span className={`pill ${task.priority}`}>
        {taskPriorityLabels[task.priority]}
      </span>
    );
  return null;
}

function TaskStat({
  label,
  value,
  late = false,
}: {
  label: string;
  value: number;
  late?: boolean;
}) {
  return (
    <div className={`task-stat ${late ? "late" : ""}`}>
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function TasksPanel({
  plan,
  workspace,
  people,
  lookups,
  today,
  weekStart,
  busy,
  busyIds,
  moveItemsFor,
  onAdd,
  onToggle,
  onOpen,
}: {
  plan: Plan;
  workspace: PlanWorkspace;
  people: Person[];
  lookups: Lookups;
  today: string;
  weekStart: string;
  busy: string | null;
  busyIds: ReadonlySet<string>;
  moveItemsFor: (task: Task) => MenuItem[];
  onAdd: (title: string, filters: TaskFilters) => Promise<boolean>;
  onToggle: (task: Task) => void;
  onOpen: (task: Task) => void;
}) {
  const [title, setTitle] = useState("");
  const [filters, setFilters] = useState<TaskFilters>(allTasks);
  const visible = filterTasks(workspace.tasks, filters);
  const filtering = visible.length !== workspace.tasks.length;
  async function submit(form: FormEvent) {
    form.preventDefault();
    const trimmed = title.trim();
    if (trimmed && (await onAdd(trimmed, filters))) setTitle("");
  }
  const update = (change: Partial<TaskFilters>) =>
    setFilters({ ...filters, ...change });
  return (
    <div className="plan-tasks">
      <form className="task-add" onSubmit={submit}>
        <Mark size={11} color="var(--blue)" />
        <input
          value={title}
          onChange={(input) => setTitle(input.target.value)}
          placeholder={`Add a task to ${plan.title}`}
          aria-label="Task title"
          maxLength={140}
        />
        <button aria-label="Add task" disabled={!title.trim()}>
          <Glyph>→</Glyph>
        </button>
      </form>
      {workspace.tasks.length > 0 && (
        <div className="task-filters" role="group" aria-label="Filter tasks">
          <label>
            Status
            <span className="select-wrap">
              <select
                value={filters.status}
                onChange={(input) =>
                  update({
                    status: input.target.value as TaskFilters["status"],
                  })
                }
              >
                <option value="all">All</option>
                <option value="open">Not done</option>
                {taskStatuses.map((status) => (
                  <option key={status} value={status}>
                    {taskStatusLabels[status]}
                  </option>
                ))}
              </select>
            </span>
          </label>
          {workspace.workstreams.length > 0 && (
            <label>
              Workstream
              <span className="select-wrap">
                <select
                  value={filters.workstreamId}
                  onChange={(input) =>
                    update({ workstreamId: input.target.value })
                  }
                >
                  <option value="all">All</option>
                  <option value="none">No workstream</option>
                  {workspace.workstreams.map((workstream) => (
                    <option key={workstream.id} value={workstream.id}>
                      {workstream.name}
                    </option>
                  ))}
                </select>
              </span>
            </label>
          )}
          {people.length > 0 && (
            <label>
              Owner
              <span className="select-wrap">
                <select
                  value={filters.ownerId}
                  onChange={(input) => update({ ownerId: input.target.value })}
                >
                  <option value="all">Anyone</option>
                  <option value="none">Unassigned</option>
                  {people.map((person) => (
                    <option key={person.id} value={person.id}>
                      {person.displayName}
                    </option>
                  ))}
                </select>
              </span>
            </label>
          )}
          {workspace.milestones.length > 0 && (
            <label>
              Milestone
              <span className="select-wrap">
                <select
                  value={filters.milestoneId}
                  onChange={(input) =>
                    update({ milestoneId: input.target.value })
                  }
                >
                  <option value="all">All</option>
                  <option value="none">No milestone</option>
                  {[...workspace.milestones]
                    .sort(compareMilestones)
                    .map((milestone) => (
                      <option key={milestone.id} value={milestone.id}>
                        {milestone.title}
                      </option>
                    ))}
                </select>
              </span>
            </label>
          )}
          {filtering && (
            <p className="filter-count" role="status">
              Showing {visible.length} of {workspace.tasks.length}{" "}
              <button
                className="text-button"
                onClick={() => setFilters(allTasks)}
              >
                Clear filters
              </button>
            </p>
          )}
        </div>
      )}
      {workspace.tasks.length === 0 ? (
        <p className="empty-tasks">
          Capture the work now; schedule it when you know when.
        </p>
      ) : visible.length === 0 ? (
        <p className="empty-tasks">No tasks match these filters.</p>
      ) : (
        groupTasksByStatus(visible)
          .filter((group) => group.tasks.length > 0)
          .map((group) => (
            <section key={group.status} className="task-group">
              <h3>
                {taskStatusLabels[group.status]}
                <span>{group.tasks.length}</span>
              </h3>
              <div className="task-list">
                {group.tasks.map((task) => (
                  <TaskRow
                    key={task.id}
                    task={task}
                    today={today}
                    milestone={
                      task.milestoneId
                        ? lookups.milestones.get(task.milestoneId)
                        : undefined
                    }
                    workstream={
                      task.workstreamId
                        ? lookups.workstreams.get(task.workstreamId)
                        : undefined
                    }
                    owner={
                      task.ownerId
                        ? lookups.people.get(task.ownerId)
                        : undefined
                    }
                    showScheduledDay
                    weekStart={weekStart}
                    busy={busy === `task-${task.id}` || busyIds.has(task.id)}
                    moveItems={moveItemsFor(task)}
                    onToggle={() => onToggle(task)}
                    onOpen={() => onOpen(task)}
                  />
                ))}
              </div>
            </section>
          ))
      )}
    </div>
  );
}

function SchedulePanel({
  events,
  lookups,
  today,
  onNew,
  onOpen,
  onDelete,
}: {
  events: ScheduleEvent[];
  lookups: Lookups;
  today: string;
  onNew: () => void;
  onOpen: (event: ScheduleEvent) => void;
  onDelete: (event: ScheduleEvent) => void;
}) {
  const now = new Date();
  const upcoming = events.filter((event) => !eventHasEnded(event, now));
  const past = events.filter((event) => eventHasEnded(event, now)).reverse();
  return (
    <div className="plan-schedule">
      <div className="panel-actions">
        <button className="secondary-button" onClick={onNew}>
          <Glyph>+</Glyph> New event
        </button>
      </div>
      {events.length === 0 ? (
        <p className="empty-tasks">
          No time blocked for this plan yet. Events you add here also appear on
          their day.
        </p>
      ) : (
        <>
          {upcoming.length === 0 ? (
            <p className="empty-tasks">No upcoming events.</p>
          ) : (
            <EventDays
              events={upcoming}
              lookups={lookups}
              today={today}
              onOpen={onOpen}
              onDelete={onDelete}
            />
          )}
          {past.length > 0 && (
            <details className="past-events">
              <summary>{plural(past.length, "past event")}</summary>
              <EventDays
                events={past}
                lookups={lookups}
                today={today}
                onOpen={onOpen}
                onDelete={onDelete}
              />
            </details>
          )}
        </>
      )}
    </div>
  );
}

function EventDays({
  events,
  lookups,
  today,
  onOpen,
  onDelete,
}: {
  events: ScheduleEvent[];
  lookups: Lookups;
  today: string;
  onOpen: (event: ScheduleEvent) => void;
  onDelete: (event: ScheduleEvent) => void;
}) {
  const days: { day: string; events: ScheduleEvent[] }[] = [];
  for (const event of events) {
    const day = eventDay(event);
    const last = days.at(-1);
    if (last?.day === day) last.events.push(event);
    else days.push({ day, events: [event] });
  }
  return (
    <>
      {days.map((group) => (
        <section key={group.day} className="schedule-day">
          <h3>
            {dayLabel(group.day)}
            <span>{relativeDayLabel(group.day, today)}</span>
          </h3>
          {group.events.map((event) => (
            <div className="schedule-row" key={event.id}>
              <button onClick={() => onOpen(event)}>
                <time>{timeLabel(event.startAtUtc)}</time>
                <span className="schedule-title">
                  <strong>{event.title}</strong>
                  <EventLinks event={event} lookups={lookups} />
                </span>
                <small>
                  {event.durationMinutes} min
                  {event.reminderMinutesBefore !== null &&
                    ` · ${reminderShortLabel(event.reminderMinutesBefore)}`}
                </small>
              </button>
              <button
                className="event-menu"
                onClick={() => onDelete(event)}
                aria-label={`Delete ${event.title}`}
              >
                <Glyph>✕</Glyph>
              </button>
            </div>
          ))}
        </section>
      ))}
    </>
  );
}

function EventLinks({
  event,
  lookups,
}: {
  event: ScheduleEvent;
  lookups: Lookups;
}) {
  const owner = event.ownerId ? lookups.people.get(event.ownerId) : undefined;
  const workstream = event.workstreamId
    ? lookups.workstreams.get(event.workstreamId)
    : undefined;
  if (!owner && !workstream && !event.location) return null;
  return (
    <span className="event-links">
      {event.location && (
        <small>
          <Mark shape="square" size={5} /> {event.location}
        </small>
      )}
      {owner && (
        <small>
          <Mark shape="circle" size={6} /> {owner.displayName}
        </small>
      )}
      {workstream && (
        <small>
          <Mark shape="rule" size={5} /> {workstream.name}
        </small>
      )}
    </span>
  );
}

const showStatusLabels = {
  done: "Done",
  live: "Live",
  next: "Next",
  upcoming: "",
} as const;

function RunOfShowPanel({
  events,
  lookups,
  today,
  onOpen,
  onAdd,
}: {
  events: ScheduleEvent[];
  lookups: Lookups;
  today: string;
  onOpen: (event: ScheduleEvent) => void;
  onAdd: (day: string, time?: string) => void;
}) {
  const days = eventDays(events);
  const [selected, setSelected] = useState(() =>
    defaultRunOfShowDay(days, today),
  );
  const day = selected && days.includes(selected) ? selected : days[0];
  if (!day)
    return (
      <div className="plan-show">
        <div className="panel-actions">
          <button className="secondary-button" onClick={() => onAdd(today)}>
            <Glyph>+</Glyph> Add first cue
          </button>
        </div>
        <p className="empty-tasks">
          A run of show lists the plan’s events for one day in order, with
          owners and locations. Add events to build one.
        </p>
      </div>
    );
  const rows = runOfShow(events, day, new Date());
  const last = rows.at(-1);
  const nextStart = last ? dateTimeFields(last.end.toISOString()) : null;
  return (
    <div className="plan-show">
      <div className="show-toolbar">
        <div className="show-days" role="group" aria-label="Run of show day">
          {days.map((candidate) => (
            <button
              key={candidate}
              className={candidate === day ? "selected" : ""}
              aria-pressed={candidate === day}
              onClick={() => setSelected(candidate)}
            >
              {dayMonthShort(candidate)}
            </button>
          ))}
        </div>
        <button
          className="secondary-button"
          onClick={() =>
            onAdd(
              day,
              nextStart && nextStart.day === day ? nextStart.time : undefined,
            )
          }
        >
          <Glyph>+</Glyph> Add cue
        </button>
      </div>
      <div className="show-table-wrap">
        <table className="show-table">
          <caption>{dayLabel(day)}</caption>
          <thead>
            <tr>
              <th scope="col">Time</th>
              <th scope="col">Cue</th>
              <th scope="col">Owner</th>
              <th scope="col">Location</th>
              <th scope="col">Status</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const owner = row.event.ownerId
                ? lookups.people.get(row.event.ownerId)
                : undefined;
              return (
                <tr key={row.event.id} className={`show-row ${row.status}`}>
                  <td className="show-time">
                    <time>{timeLabel(row.event.startAtUtc)}</time>
                    <small>
                      to {timeLabel(row.end.toISOString())}
                      {row.gapMinutes > 0 && ` · ${row.gapMinutes} min gap`}
                    </small>
                  </td>
                  <td>
                    <button
                      className="show-cue"
                      onClick={() => onOpen(row.event)}
                    >
                      {row.event.title}
                    </button>
                    {row.event.notes && (
                      <small className="show-notes">{row.event.notes}</small>
                    )}
                    {row.overlapsPrevious && (
                      <small className="late">Overlaps the previous cue</small>
                    )}
                  </td>
                  <td>{owner?.displayName ?? "—"}</td>
                  <td>{row.event.location || "—"}</td>
                  <td>
                    {row.status !== "upcoming" && (
                      <span className={`show-status ${row.status}`}>
                        {showStatusLabels[row.status]}
                      </span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function TimelinePanel({
  plan,
  milestones,
  events,
  tasks,
  today,
  onOpenMilestone,
}: {
  plan: Plan;
  milestones: Milestone[];
  events: ScheduleEvent[];
  tasks: Task[];
  today: string;
  onOpenMilestone: (milestone?: Milestone) => void;
}) {
  const model = timelineModel(plan, milestones, events, today);
  const ordered = [...milestones].sort(compareMilestones);
  return (
    <div className="plan-timeline">
      {model ? (
        <TimelineTrack
          model={model}
          today={today}
          onOpenMilestone={onOpenMilestone}
        />
      ) : (
        <p className="empty-tasks">
          Give the plan a target date or date a milestone to see its shape.
        </p>
      )}
      {ordered.length === 0 ? (
        <p className="empty-tasks">
          Milestones mark the checkpoints the work builds toward.
        </p>
      ) : (
        <ol className="milestone-list">
          {ordered.map((milestone) => {
            const linked = tasks.filter(
              (task) => task.milestoneId === milestone.id,
            );
            const done = linked.filter((task) => task.status === "done").length;
            const late =
              milestone.status === "pending" &&
              isOverdue(milestone.targetDate, today);
            return (
              <li
                key={milestone.id}
                className={`milestone-row ${milestone.status} ${late ? "late" : ""}`}
              >
                <button onClick={() => onOpenMilestone(milestone)}>
                  <i className="milestone-diamond" aria-hidden="true" />
                  <span className="milestone-text">
                    <strong>{milestone.title}</strong>
                    {milestone.description && (
                      <small>{milestone.description}</small>
                    )}
                  </span>
                  <span className="milestone-when">
                    {milestone.targetDate ? (
                      <time dateTime={milestone.targetDate}>
                        {longDate(milestone.targetDate)}
                      </time>
                    ) : (
                      <time>No date</time>
                    )}
                    <small>
                      {milestone.status === "pending" && milestone.targetDate
                        ? relativeDayLabel(milestone.targetDate, today)
                        : milestoneStatusLabels[milestone.status]}
                    </small>
                  </span>
                  <span className="milestone-tasks">
                    {linked.length ? `${done}/${linked.length} tasks` : ""}
                  </span>
                </button>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

function TimelineTrack({
  model,
  today,
  onOpenMilestone,
}: {
  model: TimelineModel;
  today: string;
  onOpenMilestone: (milestone: Milestone) => void;
}) {
  const lanes = Math.max(0, ...model.milestones.map((item) => item.lane + 1));
  return (
    <div className="timeline">
      <div className="timeline-months" aria-hidden="true">
        {model.months.map((month) => (
          <span key={month.key} style={{ left: `${month.offset}%` }}>
            {month.label}
          </span>
        ))}
      </div>
      <div
        className="timeline-body"
        style={{ height: `${Math.max(56, 54 + lanes * 40)}px` }}
      >
        {model.months.map((month) => (
          <i
            key={month.key}
            className="timeline-tick"
            style={{ left: `${month.offset}%` }}
            aria-hidden="true"
          />
        ))}
        <span className="timeline-axis" aria-hidden="true" />
        {model.planStart !== null && model.planTarget !== null && (
          <span
            className="timeline-span"
            aria-hidden="true"
            style={{
              left: `${model.planStart}%`,
              width: `${model.planTarget - model.planStart}%`,
            }}
          />
        )}
        {model.events.map(({ event, offset }) => (
          <i
            key={event.id}
            className="timeline-event"
            style={{ left: `${offset}%` }}
            title={`${event.title} · ${shortDate(eventDay(event))}`}
          />
        ))}
        <span
          className="timeline-today"
          style={{ left: `${model.today}%` }}
          aria-hidden="true"
        >
          <em>Today</em>
        </span>
        {model.planTarget !== null && (
          <span
            className="timeline-target"
            style={{ left: `${model.planTarget}%` }}
            title="Plan target date"
          />
        )}
        {model.milestones.map(({ milestone, offset, lane }) => {
          const late =
            milestone.status === "pending" &&
            isOverdue(milestone.targetDate, today);
          return (
            <button
              key={milestone.id}
              className={`timeline-milestone ${milestone.status} ${late ? "late" : ""} ${offset > 70 ? "end" : ""}`}
              style={{ left: `${offset}%`, "--lane": lane } as CSSProperties}
              onClick={() => onOpenMilestone(milestone)}
              aria-label={`${milestone.title}, ${longDate(milestone.targetDate as string)}, ${milestoneStatusLabels[milestone.status]}`}
            >
              <i className="timeline-diamond" aria-hidden="true" />
              <span className="timeline-stem" aria-hidden="true" />
              <span className="timeline-label">
                <strong>{milestone.title}</strong>
                <small>{shortDate(milestone.targetDate as string)}</small>
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

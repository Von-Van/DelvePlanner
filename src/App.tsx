import {
  CSSProperties,
  FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  Agenda as AgendaData,
  api,
  messageFor,
  Milestone,
  OllamaStatus,
  Plan,
  planColors,
  PlannerResponse,
  PersonSummary,
  PlanSummary,
  ScheduleEvent,
  Task,
  TaskInput,
} from "./api";
import {
  dayLabel,
  dayMonthShort,
  localeWeekStart,
  localTimeZone,
  offsetDay,
  timeLabel,
  todayDay,
  weekdayShort,
  weekStartDay,
} from "./date";
import { EventEditor } from "./EventEditor";
import { ensureNotificationPermission, reminderShortLabel } from "./events";
import { Glyph, Mark, MarkShape, Spinner } from "./Geometry";
import { PlannerCard } from "./PlannerCard";
import { proposalEnablesReminder } from "./proposals";
import { Onboarding } from "./Onboarding";
import { PeopleView } from "./PeopleView";
import { PlanChip, PlanDot } from "./PlanControls";
import { PlanEditor } from "./PlanEditor";
import {
  focusEventId,
  matchesPlanFilter,
  newTask,
  paddedCount,
  planColor,
  PlanFilter,
  taskUpdate,
} from "./planning";
import { PlansView } from "./PlansView";
import { PlanTab, PlanView } from "./PlanView";
import { SettingsModal } from "./SettingsModal";
import { TaskEditor } from "./TaskEditor";
import { TaskRow } from "./TaskRow";
import { useHeadingFocus } from "./useHeadingFocus";
import { WeekView } from "./WeekView";

type View =
  | { kind: "today" }
  | { kind: "week" }
  | { kind: "plans"; archived: boolean }
  | { kind: "plan"; planId: string; tab: PlanTab }
  | { kind: "people" };

type TaskEditorState = { task?: Task; defaults?: Partial<TaskInput> };

const emptyAgenda: AgendaData = {
  events: [],
  tasks: [],
  dueTasks: [],
  milestones: [],
};

const isMac = /Mac|iPhone|iPad/.test(navigator.userAgent);
const shortcutLabel = (key: string) => `${isMac ? "⌘" : "Ctrl+"}${key}`;
const weekStartsOn = localeWeekStart();

export default function App() {
  const [view, setViewState] = useState<View>({ kind: "today" });
  const [focusToken, setFocusToken] = useState(0);
  const [day, setDay] = useState(todayDay());
  const [agenda, setAgenda] = useState(emptyAgenda);
  const [week, setWeek] = useState<AgendaData | null>(null);
  const [summaries, setSummaries] = useState<PlanSummary[]>([]);
  const [peopleSummaries, setPeopleSummaries] = useState<PersonSummary[]>([]);
  const [planFilter, setPlanFilter] = useState<PlanFilter>("all");
  const [status, setStatus] = useState<OllamaStatus | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<ScheduleEvent | "new" | null>(null);
  const [taskEditor, setTaskEditor] = useState<TaskEditorState | null>(null);
  const [planEditorOpen, setPlanEditorOpen] = useState(false);
  const [busyTaskId, setBusyTaskId] = useState<string | null>(null);
  const [taskTitle, setTaskTitle] = useState("");
  const [command, setCommand] = useState("");
  const [agentResponse, setAgentResponse] = useState<PlannerResponse | null>(
    null,
  );
  const [isThinking, setIsThinking] = useState(false);
  const [isApplying, setIsApplying] = useState(false);
  const [appliedProposals, setAppliedProposals] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [restoredDataVersion, setRestoredDataVersion] = useState(0);
  const [onboardingOpen, setOnboardingOpen] = useState(
    () => localStorage.getItem("dayplan-onboarding") !== "complete",
  );
  const todayHeading = useRef<HTMLHeadingElement>(null);
  const weekHeading = useRef<HTMLHeadingElement>(null);
  const peopleHeading = useRef<HTMLHeadingElement>(null);
  const weekStart = weekStartDay(day, weekStartsOn);

  /** Changes the view and moves focus to its heading, as a page change would. */
  const navigate = useCallback((next: View) => {
    setViewState(next);
    setFocusToken((token) => token + 1);
  }, []);
  useHeadingFocus(todayHeading, focusToken, view.kind === "today");
  useHeadingFocus(weekHeading, focusToken, view.kind === "week");

  const refresh = useCallback(async () => {
    setIsLoading(true);
    try {
      setAgenda(await api.listAgenda(day, localTimeZone));
    } catch (cause) {
      setError(messageFor(cause));
    } finally {
      setIsLoading(false);
    }
  }, [day]);

  const refreshWeek = useCallback(async () => {
    try {
      setWeek(await api.listWeek(weekStart, localTimeZone));
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, [weekStart]);

  const refreshPlans = useCallback(async () => {
    try {
      setSummaries(await api.listPlans(todayDay()));
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, []);

  const refreshPeople = useCallback(async () => {
    try {
      setPeopleSummaries(await api.listPeople());
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await api.status());
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);
  useEffect(() => {
    if (view.kind === "week") void refreshWeek();
  }, [refreshWeek, view.kind]);
  useEffect(() => {
    void refreshPlans();
    void refreshPeople();
  }, [refreshPlans, refreshPeople]);
  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(isMac ? event.metaKey : event.ctrlKey) || event.altKey) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      const target = {
        "1": { kind: "today" },
        "2": { kind: "week" },
        "3": { kind: "plans", archived: false },
        "4": { kind: "people" },
      }[event.key] as View | undefined;
      if (!target) return;
      event.preventDefault();
      navigate(target);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [navigate]);

  const plans = useMemo(
    () => summaries.map((summary) => summary.plan),
    [summaries],
  );
  const planById = useMemo(
    () => new Map(plans.map((plan) => [plan.id, plan])),
    [plans],
  );
  const people = useMemo(
    () => peopleSummaries.map((summary) => summary.person),
    [peopleSummaries],
  );
  const personById = useMemo(
    () => new Map(people.map((person) => [person.id, person])),
    [people],
  );
  const activePlans = plans.filter((plan) => !plan.archived);
  const archivedCount = plans.length - activePlans.length;

  useEffect(() => {
    if (
      planFilter !== "all" &&
      planFilter !== "none" &&
      planById.get(planFilter)?.archived !== false
    )
      setPlanFilter("all");
  }, [planById, planFilter]);

  const filterPlanId =
    planFilter === "all" || planFilter === "none" ? null : planFilter;
  const visibleEvents = agenda.events.filter((event) =>
    matchesPlanFilter(event, planFilter),
  );
  const visibleTasks = agenda.tasks.filter((task) =>
    matchesPlanFilter(task, planFilter),
  );
  const visibleDueTasks = agenda.dueTasks.filter((task) =>
    matchesPlanFilter(task, planFilter),
  );
  const visibleMilestones = agenda.milestones.filter((milestone) =>
    matchesPlanFilter(milestone, planFilter),
  );

  async function dataChanged() {
    await Promise.all([
      refresh(),
      refreshPlans(),
      refreshPeople(),
      view.kind === "week" ? refreshWeek() : Promise.resolve(),
    ]);
  }

  async function removeEvent(event: ScheduleEvent) {
    if (!window.confirm(`Delete “${event.title}”?`)) return;
    try {
      await api.deleteEvent(event.id, event.revision);
      await dataChanged();
    } catch (cause) {
      setError(messageFor(cause));
    }
  }

  async function addTask(submit: FormEvent) {
    submit.preventDefault();
    const title = taskTitle.trim();
    if (!title) return;
    try {
      await api.createTask(
        newTask({ title, planId: filterPlanId, scheduledDay: day }),
      );
      setTaskTitle("");
      await dataChanged();
    } catch (cause) {
      setError(messageFor(cause));
    }
  }

  async function changeTask(task: Task, action: () => Promise<unknown>) {
    setBusyTaskId(task.id);
    try {
      await action();
      await dataChanged();
    } catch (cause) {
      setError(messageFor(cause));
    } finally {
      setBusyTaskId(null);
    }
  }

  const toggleTask = (task: Task) =>
    changeTask(task, () =>
      api.updateTask({
        ...taskUpdate(task),
        status: task.status === "done" ? "todo" : "done",
      }),
    );
  const scheduleTaskHere = (task: Task) =>
    changeTask(task, () =>
      api.updateTask({ ...taskUpdate(task), scheduledDay: day }),
    );
  const removeTask = (task: Task) =>
    changeTask(task, () => api.deleteTask(task.id, task.revision));

  function openMilestone(milestone: Milestone) {
    navigate({ kind: "plan", planId: milestone.planId, tab: "timeline" });
  }

  async function askPlanner(submit: FormEvent) {
    submit.preventDefault();
    if (!command.trim() || isThinking) return;
    setIsThinking(true);
    setAgentResponse(null);
    try {
      setAgentResponse(
        view.kind === "plan"
          ? await api.propose(command, today, localTimeZone, view.planId)
          : await api.propose(command, day, localTimeZone),
      );
    } catch (cause) {
      setError(messageFor(cause));
    } finally {
      setIsThinking(false);
    }
  }

  async function applyProposal() {
    if (!agentResponse || agentResponse.kind !== "proposal") return;
    setIsApplying(true);
    try {
      if (proposalEnablesReminder(agentResponse))
        await ensureNotificationPermission();
      await api.apply(agentResponse.proposalId);
      setAgentResponse(null);
      setCommand("");
      setAppliedProposals((count) => count + 1);
      await dataChanged();
    } catch (cause) {
      setError(messageFor(cause));
    } finally {
      setIsApplying(false);
    }
  }

  async function discardProposal() {
    if (!agentResponse || agentResponse.kind !== "proposal") return;
    try {
      await api.discardProposal(agentResponse.proposalId);
      setAgentResponse(null);
    } catch (cause) {
      setError(messageFor(cause));
    }
  }

  async function clearContext() {
    try {
      await api.clearContext();
      setAgentResponse(null);
      setCommand("");
    } catch (cause) {
      setError(messageFor(cause));
    }
  }

  const plannerCard = (planTitle?: string) => (
    <PlannerCard
      status={status}
      command={command}
      onCommand={setCommand}
      onSubmit={askPlanner}
      thinking={isThinking}
      response={agentResponse}
      onApply={applyProposal}
      applying={isApplying}
      onDiscard={discardProposal}
      onClear={clearContext}
      onRefreshStatus={refreshStatus}
      planTitle={planTitle}
    />
  );

  const visibleDays = useMemo(
    () => Array.from({ length: 7 }, (_, index) => offsetDay(day, index - 3)),
    [day],
  );
  const today = todayDay();

  const filterPlan = filterPlanId ? planById.get(filterPlanId) : undefined;
  const planFilterControl = (
    <label className="plan-filter">
      Show
      <span className="select-wrap">
        <select
          value={planFilter}
          onChange={(input) => setPlanFilter(input.target.value)}
          className={filterPlan ? "filtered" : ""}
          style={
            filterPlan
              ? ({ "--plan-color": planColor(filterPlan) } as CSSProperties)
              : undefined
          }
        >
          <option value="all">All plans</option>
          <option value="none">Not in a plan</option>
          {activePlans.map((plan) => (
            <option key={plan.id} value={plan.id}>
              {plan.title}
            </option>
          ))}
        </select>
      </span>
    </label>
  );

  const navLink = (
    target: View,
    label: string,
    mark: { shape: MarkShape; size?: number },
    options: { active: boolean; shortcut?: string; count?: number },
  ) => (
    <button
      className={`rail-link ${options.active ? "active" : ""}`}
      aria-current={options.active ? "page" : undefined}
      aria-keyshortcuts={
        options.shortcut
          ? `${isMac ? "Meta" : "Control"}+${options.shortcut}`
          : undefined
      }
      title={
        options.shortcut
          ? `${label} (${shortcutLabel(options.shortcut)})`
          : undefined
      }
      onClick={() => navigate(target)}
    >
      <Mark shape={mark.shape} size={mark.size ?? 6} filled={options.active} />
      {label}
      {options.count ? (
        <span className="rail-count">{paddedCount(options.count)}</span>
      ) : null}
    </button>
  );

  return (
    <main className="app-shell">
      <aside className="rail">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <i />
          </span>
          <span className="brand-name">DAYPLAN</span>
        </div>
        <div className="rail-date">
          <span>LOCAL AGENDA</span>
          <strong>{dayMonthShort(day)}</strong>
          <i aria-hidden="true" />
        </div>
        <nav aria-label="Workspace sections">
          {navLink(
            { kind: "today" },
            "Today",
            { shape: "rhombus" },
            { active: view.kind === "today", shortcut: "1" },
          )}
          {navLink(
            { kind: "week" },
            "Week",
            { shape: "rule" },
            { active: view.kind === "week", shortcut: "2" },
          )}
          {navLink(
            { kind: "plans", archived: false },
            "Plans",
            { shape: "square" },
            {
              active: view.kind === "plans" && !view.archived,
              shortcut: "3",
              count: activePlans.length,
            },
          )}
          <div className="rail-plans">
            {activePlans.map((plan) => {
              const selected = view.kind === "plan" && view.planId === plan.id;
              return (
                <button
                  key={plan.id}
                  className={`rail-plan ${selected ? "active" : ""}`}
                  aria-current={selected ? "page" : undefined}
                  onClick={() =>
                    navigate({ kind: "plan", planId: plan.id, tab: "overview" })
                  }
                >
                  <PlanDot plan={plan} />
                  <span>{plan.title}</span>
                </button>
              );
            })}
            <button
              className="rail-plan new-plan"
              onClick={() => setPlanEditorOpen(true)}
            >
              <Glyph>+</Glyph> New plan
            </button>
          </div>
          {navLink(
            { kind: "plans", archived: true },
            "Archived",
            { shape: "circle" },
            {
              active: view.kind === "plans" && view.archived,
              count: archivedCount,
            },
          )}
          {navLink(
            { kind: "people" },
            "People",
            { shape: "ring", size: 7 },
            {
              active: view.kind === "people",
              shortcut: "4",
              count: people.length,
            },
          )}
          <button className="rail-link" onClick={() => setSettingsOpen(true)}>
            <Mark size={7} />
            Settings
          </button>
        </nav>
        <section className="privacy-note">
          <span className="privacy-dot" />
          <div>
            <strong>Private by design</strong>
            <p>Your plans and schedule stay on this device.</p>
          </div>
        </section>
        <div className="rail-footer">
          DAYPLAN / DESKTOP
          <br />
          LOCAL-FIRST PLANNER
        </div>
      </aside>

      <section className="workspace">
        {view.kind === "plans" && (
          <PlansView
            summaries={summaries}
            archived={view.archived}
            today={today}
            focusToken={focusToken}
            onOpenPlan={(planId) =>
              navigate({ kind: "plan", planId, tab: "overview" })
            }
            onNewPlan={() => setPlanEditorOpen(true)}
            onShowArchived={(archived) => navigate({ kind: "plans", archived })}
            onChanged={dataChanged}
            onMessage={setError}
          />
        )}
        {view.kind === "plan" && (
          <PlanView
            planId={view.planId}
            plans={plans}
            people={people}
            today={today}
            reloadToken={restoredDataVersion + appliedProposals}
            focusToken={focusToken}
            planner={plannerCard(planById.get(view.planId)?.title)}
            tab={view.tab}
            onTab={(tab) => setViewState({ ...view, tab })}
            onBack={() => navigate({ kind: "plans", archived: false })}
            onPlansChanged={dataChanged}
            onMessage={setError}
          />
        )}
        {view.kind === "people" && (
          <PeopleView
            summaries={peopleSummaries}
            planById={planById}
            today={today}
            headingRef={peopleHeading}
            focusToken={focusToken}
            onChanged={dataChanged}
            onOpenTask={(task) => setTaskEditor({ task })}
            onOpenEvent={setEditor}
            onMessage={setError}
          />
        )}
        {view.kind === "week" && (
          <WeekView
            startDay={weekStart}
            today={today}
            agenda={week}
            loading={week === null}
            planFilter={planFilter}
            filterControl={planFilterControl}
            planById={planById}
            personById={personById}
            busyTaskId={busyTaskId}
            headingRef={weekHeading}
            onShiftWeek={(weeks) => setDay(offsetDay(day, weeks * 7))}
            onThisWeek={() => setDay(todayDay())}
            onOpenDay={(nextDay) => {
              setDay(nextDay);
              navigate({ kind: "today" });
            }}
            onOpenEvent={setEditor}
            onOpenTask={(task) => setTaskEditor({ task })}
            onToggleTask={(task) => void toggleTask(task)}
            onOpenMilestone={openMilestone}
          />
        )}
        {view.kind === "today" && (
          <>
            <header className="topbar">
              <div className="date-heading">
                <p>YOUR DAY</p>
                <h1 ref={todayHeading} tabIndex={-1}>
                  {dayLabel(day)}
                </h1>
              </div>
              <div className="date-controls">
                <button
                  className="icon-button"
                  aria-label="Previous day"
                  onClick={() => setDay(offsetDay(day, -1))}
                >
                  <Glyph>←</Glyph>
                </button>
                <button
                  className="secondary-button"
                  onClick={() => setDay(todayDay())}
                >
                  Today
                </button>
                <button
                  className="icon-button"
                  aria-label="Next day"
                  onClick={() => setDay(offsetDay(day, 1))}
                >
                  <Glyph>→</Glyph>
                </button>
                <button
                  className="primary-button"
                  onClick={() => setEditor("new")}
                >
                  <Mark filled />
                  New event
                </button>
              </div>
            </header>

            <div className="week-strip" aria-label="Date picker">
              {visibleDays.map((candidate) => (
                <button
                  key={candidate}
                  className={`day-chip ${candidate === day ? "selected" : ""} ${candidate === today ? "today" : ""}`}
                  aria-pressed={candidate === day}
                  aria-label={dayLabel(candidate)}
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
              {planFilterControl}
              <button
                className="calendar-jump"
                title="Jump to today"
                aria-label="Jump to today"
                onClick={() => setDay(todayDay())}
              >
                <i aria-hidden="true" />
              </button>
            </div>

            <div className="content-grid">
              <section className="agenda-panel">
                <div className="section-header">
                  <div>
                    <p>TIME BLOCKS</p>
                    <h2>Agenda</h2>
                  </div>
                  <span>{visibleEvents.length} scheduled</span>
                </div>
                {visibleMilestones.length > 0 && (
                  <ul className="day-milestones" aria-label="Milestones today">
                    {visibleMilestones.map((milestone) => {
                      const plan = planById.get(milestone.planId);
                      return (
                        <li key={milestone.id}>
                          <button onClick={() => openMilestone(milestone)}>
                            <Mark
                              size={7}
                              color={planColor(plan)}
                              filled={milestone.status === "complete"}
                              dashed={milestone.status === "skipped"}
                            />
                            <span>{milestone.title}</span>
                            <small>
                              Milestone
                              {milestone.status !== "pending" &&
                                ` · ${milestone.status}`}
                            </small>
                            <PlanChip plan={plan} />
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                )}
                {isLoading ? (
                  <LoadingLine label="Opening your local schedule" />
                ) : (
                  <Agenda
                    events={visibleEvents}
                    focusId={
                      day === today
                        ? focusEventId(visibleEvents, new Date())
                        : null
                    }
                    planById={planById}
                    onEdit={setEditor}
                    onDelete={removeEvent}
                    onAdd={() => setEditor("new")}
                  />
                )}
                <section className="task-area">
                  <div className="section-header">
                    <div>
                      <p>LOOSE ENDS</p>
                      <h2>Daily tasks</h2>
                    </div>
                    <span>
                      {
                        visibleTasks.filter((task) => task.status === "done")
                          .length
                      }
                      /{visibleTasks.length}
                    </span>
                  </div>
                  <form className="task-add" onSubmit={addTask}>
                    <Mark size={11} color="var(--blue)" />
                    <input
                      value={taskTitle}
                      onChange={(event) => setTaskTitle(event.target.value)}
                      placeholder={
                        filterPlanId
                          ? `Add a task for this day to ${planById.get(filterPlanId)?.title}`
                          : "Add a task for this day"
                      }
                      aria-label="Add task"
                      maxLength={140}
                    />
                    <button aria-label="Add task" disabled={!taskTitle.trim()}>
                      <Glyph>→</Glyph>
                    </button>
                  </form>
                  <div className="task-list">
                    {visibleTasks.length === 0 ? (
                      <p className="empty-tasks">
                        A clear list leaves room to think.
                      </p>
                    ) : (
                      visibleTasks.map((task) => (
                        <TaskRow
                          key={task.id}
                          task={task}
                          today={today}
                          plan={
                            task.planId ? planById.get(task.planId) : undefined
                          }
                          owner={
                            task.ownerId
                              ? personById.get(task.ownerId)
                              : undefined
                          }
                          busy={busyTaskId === task.id}
                          onToggle={() => void toggleTask(task)}
                          onOpen={() => setTaskEditor({ task })}
                          onDelete={() => void removeTask(task)}
                        />
                      ))
                    )}
                  </div>
                  {visibleDueTasks.length > 0 && (
                    <div className="due-tasks">
                      <p className="due-heading">
                        DUE THIS DAY <span>{visibleDueTasks.length}</span>
                      </p>
                      <div className="task-list">
                        {visibleDueTasks.map((task) => (
                          <TaskRow
                            key={task.id}
                            task={task}
                            today={today}
                            plan={
                              task.planId
                                ? planById.get(task.planId)
                                : undefined
                            }
                            owner={
                              task.ownerId
                                ? personById.get(task.ownerId)
                                : undefined
                            }
                            showScheduledDay
                            busy={busyTaskId === task.id}
                            onToggle={() => void toggleTask(task)}
                            onOpen={() => setTaskEditor({ task })}
                            onScheduleHere={() => void scheduleTaskHere(task)}
                          />
                        ))}
                      </div>
                    </div>
                  )}
                </section>
              </section>

              <aside className="ai-column">
                {plannerCard()}
                <section className="quiet-card">
                  <Mark shape="circle" size={14} />
                  <div>
                    <strong>All times are local</strong>
                    <p>
                      {localTimeZone}. Events persist as UTC with their IANA
                      time zone.
                    </p>
                  </div>
                </section>
              </aside>
            </div>
          </>
        )}
      </section>

      {editor && (
        <EventEditor
          day={day}
          event={editor === "new" ? undefined : editor}
          plans={plans}
          people={people}
          defaultPlanId={filterPlanId}
          onClose={() => setEditor(null)}
          onSaved={async () => {
            setEditor(null);
            await dataChanged();
          }}
          onError={setError}
        />
      )}
      {taskEditor && (
        <TaskEditor
          task={taskEditor.task}
          defaults={taskEditor.defaults}
          plans={plans}
          people={people}
          onClose={() => setTaskEditor(null)}
          onSaved={async () => {
            setTaskEditor(null);
            await dataChanged();
          }}
          onError={setError}
        />
      )}
      {planEditorOpen && (
        <PlanEditor
          defaultColor={
            planColors.find(
              (color) => !activePlans.some((plan) => plan.color === color),
            ) ?? planColors[plans.length % planColors.length]
          }
          onClose={() => setPlanEditorOpen(false)}
          onSaved={async (plan: Plan) => {
            setPlanEditorOpen(false);
            await refreshPlans();
            navigate({ kind: "plan", planId: plan.id, tab: "overview" });
          }}
          onError={setError}
        />
      )}
      {settingsOpen && (
        <SettingsModal
          status={status}
          onClose={() => setSettingsOpen(false)}
          onDataChanged={async () => {
            await dataChanged();
            setRestoredDataVersion((version) => version + 1);
          }}
          onRefreshStatus={refreshStatus}
          onMessage={setError}
        />
      )}
      {onboardingOpen && (
        <Onboarding
          status={status}
          onRefresh={refreshStatus}
          onComplete={() => setOnboardingOpen(false)}
          onMessage={setError}
        />
      )}
      {error && (
        <div className="toast" role="alert">
          <Mark size={7} color="var(--danger-dot)" filled />
          <span>{error}</span>
          <button onClick={() => setError(null)} aria-label="Dismiss message">
            <Glyph>✕</Glyph>
          </button>
        </div>
      )}
    </main>
  );
}

function Agenda({
  events,
  focusId,
  planById,
  onEdit,
  onDelete,
  onAdd,
}: {
  events: ScheduleEvent[];
  /** The live or next event, drawn in the highlighted glass style. */
  focusId: string | null;
  planById: Map<string, Plan>;
  onEdit: (event: ScheduleEvent) => void;
  onDelete: (event: ScheduleEvent) => void;
  onAdd: () => void;
}) {
  if (events.length === 0)
    return (
      <div className="empty-agenda">
        <i className="empty-mark" aria-hidden="true" />
        <p>Nothing is carved into this day yet.</p>
        <button className="secondary-button" onClick={onAdd}>
          <Glyph>+</Glyph> Add the first block
        </button>
      </div>
    );
  return (
    <div className="agenda-list">
      {events.map((event) => (
        <EventRow
          key={event.id}
          event={event}
          current={event.id === focusId}
          plan={event.planId ? planById.get(event.planId) : undefined}
          onEdit={() => onEdit(event)}
          onDelete={() => onDelete(event)}
        />
      ))}
    </div>
  );
}

function EventRow({
  event,
  current,
  plan,
  onEdit,
  onDelete,
}: {
  event: ScheduleEvent;
  current: boolean;
  plan: Plan | undefined;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <article
      className={`event-row ${current ? "current" : ""}`}
      style={{ "--plan-color": planColor(plan) } as CSSProperties}
    >
      <time dateTime={event.startAtUtc}>
        {timeLabel(event.startAtUtc)}
        <span>{event.durationMinutes} min</span>
      </time>
      <div className="event-connector" aria-hidden="true">
        <i />
      </div>
      <button className="event-card" onClick={onEdit}>
        <span className="event-swatch" aria-hidden="true" />
        <span className="event-main">
          <strong>{event.title}</strong>
          {event.notes && <small>{event.notes}</small>}
          {event.reminderMinutesBefore !== null && (
            <small className={`reminder-badge ${event.reminderStatus}`}>
              <i aria-hidden="true" />
              {reminderShortLabel(event.reminderMinutesBefore)} ·{" "}
              {event.reminderStatus.replace("_", " ")}
            </small>
          )}
        </span>
        <PlanChip plan={plan} />
        <span className="chevron" aria-hidden="true">
          ▼
        </span>
      </button>
      <button
        className="event-menu"
        onClick={onDelete}
        aria-label={`Delete ${event.title}`}
      >
        <Glyph>✕</Glyph>
      </button>
    </article>
  );
}

function LoadingLine({ label }: { label: string }) {
  return (
    <div className="loading-line">
      <Spinner size={10} />
      {label}
    </div>
  );
}

import {
  CSSProperties,
  FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Agenda as AgendaData,
  api,
  Calendar,
  CalendarAgenda,
  Capacity,
  ExternalEvent,
  InboxItem,
  messageFor,
  Milestone,
  OllamaStatus,
  Plan,
  planColors,
  PlannerResponse,
  PlanningBoard,
  PersonSummary,
  PlanSummary,
  ScheduledBlock,
  ScheduleEvent,
  Task,
  TaskInput,
} from "./api";
import {
  AgendaItem,
  agendaItems,
  allDayEventsOn,
  calendarColor,
  calendarsNeedingAttention,
  capacityLine,
  capacityTotals,
  focusItemKey,
  hoursLabel,
  timeRangeLabel,
  workingHoursLabel,
} from "./calendars";
import { CalendarsView } from "./CalendarsView";
import {
  dayLabel,
  dayMonthShort,
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
import { InboxConversionKind, InboxView, QuickCapture } from "./InboxView";
import { PlannerCard } from "./PlannerCard";
import { proposalEnablesReminder } from "./proposals";
import { Onboarding } from "./Onboarding";
import { PeopleView } from "./PeopleView";
import { PlanChip, PlanDot } from "./PlanControls";
import { PlanEditor } from "./PlanEditor";
import {
  inWeek,
  matchesPlanFilter,
  newTask,
  paddedCount,
  planColor,
  PlanFilter,
  planningPrompt,
  PlanningPromptState,
  taskUpdate,
  unfinishedTasks,
} from "./planning";
import { PlanTodayView, PlanWeekView } from "./PlanningViews";
import { PlansView } from "./PlansView";
import { PlanTab, PlanView } from "./PlanView";
import { SettingsModal } from "./SettingsModal";
import { TaskEditor } from "./TaskEditor";
import { TaskRow } from "./TaskRow";
import { useHeadingFocus } from "./useHeadingFocus";
import { taskMoveItems, useTaskMover, weekStartsOn } from "./useTaskMover";
import { WeekView } from "./WeekView";

type View =
  | { kind: "inbox" }
  | { kind: "calendars" }
  | { kind: "today" }
  | { kind: "week" }
  | { kind: "plan-week" }
  | { kind: "plan-today" }
  | { kind: "plans"; archived: boolean }
  | { kind: "plan"; planId: string; tab: PlanTab }
  | { kind: "people" };

type TaskEditorState = { task?: Task; defaults?: Partial<TaskInput> };

const emptyAgenda: AgendaData = {
  events: [],
  tasks: [],
  dueTasks: [],
  milestones: [],
  blocks: [],
};

const isMac = /Mac|iPhone|iPad/.test(navigator.userAgent);
const shortcutLabel = (key: string) => `${isMac ? "⌘" : "Ctrl+"}${key}`;
const promptStorageKey = "dayplan-planning-prompts";
const keptBlocksStorageKey = "dayplan-kept-blocks";

/** Future blocks of finished tasks the user chose to keep; kept per device. */
function readKeptBlocks(): Set<string> {
  try {
    const stored = JSON.parse(
      localStorage.getItem(keptBlocksStorageKey) ?? "[]",
    );
    return new Set(
      Array.isArray(stored)
        ? stored.filter((id): id is string => typeof id === "string")
        : [],
    );
  } catch {
    return new Set();
  }
}

/** Which planning sessions were done or dismissed; kept per device, like onboarding. */
function readPromptState(): PlanningPromptState {
  try {
    const stored = JSON.parse(localStorage.getItem(promptStorageKey) ?? "null");
    return {
      week: typeof stored?.week === "string" ? stored.week : null,
      day: typeof stored?.day === "string" ? stored.day : null,
    };
  } catch {
    return { week: null, day: null };
  }
}

export default function App() {
  const [view, setViewState] = useState<View>({ kind: "today" });
  const [focusToken, setFocusToken] = useState(0);
  const [day, setDay] = useState(todayDay());
  const [agenda, setAgenda] = useState(emptyAgenda);
  const [calendarDay, setCalendarDay] = useState<CalendarAgenda | null>(null);
  const [dayCapacity, setDayCapacity] = useState<Capacity | null>(null);
  const [releasable, setReleasable] = useState<ScheduledBlock[]>([]);
  const [keptBlocks, setKeptBlocks] = useState(readKeptBlocks);
  const [week, setWeek] = useState<AgendaData | null>(null);
  const [calendarWeek, setCalendarWeek] = useState<CalendarAgenda | null>(null);
  const [weekCapacity, setWeekCapacity] = useState<Capacity | null>(null);
  const [calendarVersion, setCalendarVersion] = useState(0);
  const [weekBoard, setWeekBoard] = useState<PlanningBoard | null>(null);
  const [board, setBoard] = useState<PlanningBoard | null>(null);
  const [inbox, setInbox] = useState<InboxItem[]>([]);
  const [dataVersion, setDataVersion] = useState(0);
  const [converting, setConverting] = useState<{
    item: InboxItem;
    kind: InboxConversionKind;
  } | null>(null);
  const [captureOpen, setCaptureOpen] = useState(false);
  const [promptState, setPromptState] = useState(readPromptState);
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
  const inboxHeading = useRef<HTMLHeadingElement>(null);
  const calendarsHeading = useRef<HTMLHeadingElement>(null);
  const weekStart = weekStartDay(day, weekStartsOn);
  const today = todayDay();
  const currentWeekStart = weekStartDay(today, weekStartsOn);

  /** Changes the view and moves focus to its heading, as a page change would. */
  const navigate = useCallback((next: View) => {
    setViewState(next);
    setFocusToken((token) => token + 1);
  }, []);
  useHeadingFocus(todayHeading, focusToken, view.kind === "today");
  useHeadingFocus(weekHeading, focusToken, view.kind === "week");
  useHeadingFocus(inboxHeading, focusToken, view.kind === "inbox");

  // Calendar and capacity trouble never blocks the planner's own data from showing.
  const refreshCalendarDay = useCallback(async () => {
    const [nextCalendars, nextCapacity] = await Promise.all([
      api.calendarEvents(day, 1, localTimeZone).catch(() => null),
      api.capacity(day, 1, localTimeZone).catch(() => null),
    ]);
    setCalendarDay(nextCalendars);
    setDayCapacity(nextCapacity);
  }, [day]);

  const refresh = useCallback(async () => {
    setIsLoading(true);
    try {
      const [nextAgenda] = await Promise.all([
        api.listAgenda(day, localTimeZone),
        refreshCalendarDay(),
      ]);
      setAgenda(nextAgenda);
    } catch (cause) {
      setError(messageFor(cause));
    } finally {
      setIsLoading(false);
    }
  }, [day, refreshCalendarDay]);

  const refreshWeek = useCallback(async () => {
    try {
      const [nextWeek, nextBoard, nextCalendars, nextCapacity] =
        await Promise.all([
          api.listWeek(weekStart, localTimeZone),
          api.planningBoard(weekStart),
          api.calendarEvents(weekStart, 7, localTimeZone).catch(() => null),
          api.capacity(weekStart, 7, localTimeZone).catch(() => null),
        ]);
      setWeek(nextWeek);
      setWeekBoard(nextBoard);
      setCalendarWeek(nextCalendars);
      setWeekCapacity(nextCapacity);
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, [weekStart]);

  const refreshReleasable = useCallback(async () => {
    try {
      setReleasable(await api.releasableBlocks());
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, []);

  const refreshBoard = useCallback(async () => {
    try {
      setBoard(await api.planningBoard(currentWeekStart));
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, [currentWeekStart]);

  const refreshInbox = useCallback(async () => {
    try {
      setInbox(await api.listInbox());
    } catch (cause) {
      setError(messageFor(cause));
    }
  }, []);

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
    void refreshInbox();
    void refreshReleasable();
  }, [refreshPlans, refreshPeople, refreshInbox, refreshReleasable]);

  // Background calendar refreshes announce themselves; reload whatever shows calendar time.
  const viewKind = view.kind;
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    listen("calendars-changed", () => {
      setCalendarVersion((version) => version + 1);
      void refreshCalendarDay();
      if (viewKind === "week") void refreshWeek();
    })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => undefined);
    return () => {
      active = false;
      unlisten?.();
    };
  }, [refreshCalendarDay, refreshWeek, viewKind]);
  useEffect(() => {
    void refreshBoard();
  }, [refreshBoard]);
  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(isMac ? event.metaKey : event.ctrlKey) || event.altKey) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      if (event.key.toLowerCase() === "i" && !event.shiftKey) {
        event.preventDefault();
        setCaptureOpen(true);
        return;
      }
      const target = {
        "1": { kind: "today" },
        "2": { kind: "week" },
        "3": { kind: "plans", archived: false },
        "4": { kind: "people" },
        "5": { kind: "inbox" },
        "6": { kind: "calendars" },
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
  const visibleBlocks = agenda.blocks.filter((scheduled) =>
    matchesPlanFilter(scheduled.task, planFilter),
  );
  // Other calendars' events belong to no plan, so a filter for one plan hides them.
  const calendars = calendarDay?.calendars ?? [];
  const visibleExternal =
    planFilter === "all" || planFilter === "none"
      ? (calendarDay?.events ?? [])
      : [];
  const dayItems = agendaItems(
    visibleEvents,
    visibleBlocks,
    visibleExternal,
    calendars,
  );
  const allDayExternal = allDayEventsOn(visibleExternal, day);
  const attention = calendarsNeedingAttention(calendars);
  const offeredRelease = releasable.filter(
    (scheduled) => !keptBlocks.has(scheduled.block.id),
  );

  async function dataChanged() {
    await Promise.all([
      refresh(),
      refreshPlans(),
      refreshPeople(),
      refreshInbox(),
      refreshBoard(),
      refreshReleasable(),
      view.kind === "week" ? refreshWeek() : Promise.resolve(),
    ]);
    setDataVersion((version) => version + 1);
  }

  function keepBlocks(blocks: ScheduledBlock[]) {
    setKeptBlocks((current) => {
      const next = new Set(current);
      for (const scheduled of blocks) next.add(scheduled.block.id);
      // Only remember blocks that are still offered, so the list can't grow forever.
      const offered = new Set(releasable.map((item) => item.block.id));
      const stored = [...next].filter((id) => offered.has(id));
      try {
        localStorage.setItem(keptBlocksStorageKey, JSON.stringify(stored));
      } catch {
        // The offer returns next launch when storage is unavailable.
      }
      return new Set(stored);
    });
  }

  async function releaseBlocks(blocks: ScheduledBlock[]) {
    try {
      await api.deleteTaskBlocks(blocks.map((scheduled) => scheduled.block));
      await dataChanged();
    } catch (cause) {
      setError(messageFor(cause));
    }
  }

  const mover = useTaskMover({
    today,
    onChanged: dataChanged,
    onError: setError,
  });
  const moveItemsFor = (task: Task) =>
    taskMoveItems(task, {
      today,
      weekStart: currentWeekStart,
      move: (tasks, target) => void mover.move(tasks, target),
      pickDay: mover.pickDay,
      blockTime: mover.blockTime,
    });

  function recordPrompt(change: Partial<PlanningPromptState>) {
    setPromptState((current) => {
      const next = { ...current, ...change };
      try {
        localStorage.setItem(promptStorageKey, JSON.stringify(next));
      } catch {
        // Prompts reappear next launch when storage is unavailable.
      }
      return next;
    });
  }
  const prompt = planningPrompt(promptState, currentWeekStart, today);

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

  async function applyProposal(accepted: string[]) {
    if (!agentResponse || agentResponse.kind !== "proposal") return;
    setIsApplying(true);
    try {
      // Only ask for notification permission when an accepted change actually needs it.
      const applied = {
        ...agentResponse,
        operations: agentResponse.operations.filter((operation) =>
          accepted.includes(operation.id),
        ),
      };
      if (proposalEnablesReminder(applied))
        await ensureNotificationPermission();
      await api.apply(agentResponse.proposalId, accepted);
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
  const visibleUnfinished = board
    ? unfinishedTasks(board.tasks, today).filter((task) =>
        matchesPlanFilter(task, planFilter),
      )
    : [];

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
    mark: { shape: MarkShape; size?: number; dashed?: boolean },
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
      <Mark
        shape={mark.shape}
        size={mark.size ?? 6}
        filled={options.active}
        dashed={mark.dashed}
      />
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
            { kind: "inbox" },
            "Inbox",
            { shape: "rhombus", dashed: true },
            {
              active: view.kind === "inbox",
              shortcut: "5",
              count: inbox.length,
            },
          )}
          {navLink(
            { kind: "today" },
            "Today",
            { shape: "rhombus" },
            {
              active: view.kind === "today" || view.kind === "plan-today",
              shortcut: "1",
            },
          )}
          {navLink(
            { kind: "week" },
            "Week",
            { shape: "rule" },
            {
              active: view.kind === "week" || view.kind === "plan-week",
              shortcut: "2",
            },
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
          {navLink(
            { kind: "calendars" },
            "Calendars",
            { shape: "square", dashed: true },
            {
              active: view.kind === "calendars",
              shortcut: "6",
              count: calendars.length,
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
        {view.kind === "inbox" && (
          <InboxView
            items={inbox}
            headingRef={inboxHeading}
            captureShortcut={shortcutLabel("I")}
            onConvert={(item, kind) => setConverting({ item, kind })}
            onChanged={dataChanged}
            onMessage={setError}
          />
        )}
        {view.kind === "plan-week" && (
          <PlanWeekView
            today={today}
            plans={plans}
            planById={planById}
            personById={personById}
            dataVersion={dataVersion}
            focusToken={focusToken}
            onChanged={dataChanged}
            onOpenTask={(task) => setTaskEditor({ task })}
            onMessage={setError}
            onDone={(plannedWeek) => {
              if (plannedWeek >= currentWeekStart)
                recordPrompt({
                  week:
                    promptState.week && promptState.week > plannedWeek
                      ? promptState.week
                      : plannedWeek,
                });
              navigate({ kind: "today" });
            }}
          />
        )}
        {view.kind === "plan-today" && (
          <PlanTodayView
            today={today}
            planById={planById}
            personById={personById}
            dataVersion={dataVersion}
            focusToken={focusToken}
            onChanged={dataChanged}
            onOpenTask={(task) => setTaskEditor({ task })}
            onMessage={setError}
            onDone={() => {
              recordPrompt({ day: today });
              setDay(today);
              navigate({ kind: "today" });
            }}
          />
        )}
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
        {view.kind === "calendars" && (
          <CalendarsView
            version={calendarVersion}
            headingRef={calendarsHeading}
            focusToken={focusToken}
            onChanged={async () => {
              await Promise.all([refreshCalendarDay(), refreshWeek()]);
              setDataVersion((version) => version + 1);
            }}
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
            plannedTasks={
              weekBoard?.tasks.filter(
                (task) =>
                  task.scheduledDay === null &&
                  inWeek(task.plannedWeek, weekStart),
              ) ?? []
            }
            onPlanWeek={() => navigate({ kind: "plan-week" })}
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
            calendar={calendarWeek}
            capacity={weekCapacity}
            onOpenEvent={setEditor}
            onOpenBlock={mover.editBlock}
            onOpenCalendars={() => navigate({ kind: "calendars" })}
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
                  className="secondary-button"
                  onClick={() => {
                    setDay(today);
                    navigate({ kind: "plan-today" });
                  }}
                >
                  Plan today
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

            {day === today && prompt && (
              <section className="planning-prompt" aria-label="Planning">
                <span className="planner-orb" aria-hidden="true">
                  <i />
                </span>
                <div>
                  <p>{prompt === "week" ? "A NEW WEEK" : "A NEW DAY"}</p>
                  <strong>
                    {prompt === "week" ? "Plan this week" : "Plan today"}
                  </strong>
                  <small>
                    {prompt === "week"
                      ? "Choose what this week is for before the days fill up."
                      : "Build today from what's unfinished, due, and chosen for this week."}
                  </small>
                </div>
                <button
                  className="secondary-button"
                  onClick={() =>
                    recordPrompt(
                      prompt === "week"
                        ? { week: currentWeekStart }
                        : { day: today },
                    )
                  }
                >
                  Not now
                </button>
                <button
                  className="primary-button"
                  onClick={() =>
                    navigate({
                      kind: prompt === "week" ? "plan-week" : "plan-today",
                    })
                  }
                >
                  <Mark filled />
                  {prompt === "week" ? "Plan the week" : "Plan today"}
                </button>
              </section>
            )}

            {day === today &&
              (attention.length > 0 || offeredRelease.length > 0) && (
                <div className="day-notices">
                  {attention.length > 0 && (
                    <section className="day-notice" aria-label="Calendars">
                      <Mark size={7} filled color="var(--danger-dot)" />
                      <p>
                        <strong>
                          {attention.length === 1
                            ? `“${attention[0].name}” can't refresh`
                            : `${attention.length} calendars can't refresh`}
                        </strong>
                        <span>
                          {attention.length === 1
                            ? attention[0].problem?.message
                            : "Their last copies still show until you fix them."}
                        </span>
                      </p>
                      <button
                        className="secondary-button"
                        onClick={() => navigate({ kind: "calendars" })}
                      >
                        Open Calendars
                      </button>
                    </section>
                  )}
                  {offeredRelease.length > 0 && (
                    <ReleaseNotice
                      blocks={offeredRelease}
                      onRelease={() => void releaseBlocks(offeredRelease)}
                      onKeep={() => keepBlocks(offeredRelease)}
                    />
                  )}
                </div>
              )}

            <div className="content-grid">
              <section className="agenda-panel">
                <div className="section-header">
                  <div>
                    <p>TIME BLOCKS</p>
                    <h2>Agenda</h2>
                  </div>
                  <span>{dayItems.length} scheduled</span>
                </div>
                {allDayExternal.length > 0 && (
                  <ul className="all-day-strip" aria-label="All-day events">
                    {allDayExternal.map((event) => (
                      <AllDayChip
                        key={`${event.calendarId}-${event.key}`}
                        event={event}
                        calendar={calendars.find(
                          (calendar) => calendar.id === event.calendarId,
                        )}
                      />
                    ))}
                  </ul>
                )}
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
                    items={dayItems}
                    focusKey={
                      day === today ? focusItemKey(dayItems, new Date()) : null
                    }
                    planById={planById}
                    onEdit={setEditor}
                    onDelete={removeEvent}
                    onOpenBlock={mover.editBlock}
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
                          weekStart={currentWeekStart}
                          busy={
                            busyTaskId === task.id || mover.busyIds.has(task.id)
                          }
                          moveItems={moveItemsFor(task)}
                          onToggle={() => void toggleTask(task)}
                          onOpen={() => setTaskEditor({ task })}
                          onDelete={() => void removeTask(task)}
                        />
                      ))
                    )}
                  </div>
                  {day === today && visibleUnfinished.length > 0 && (
                    <div className="unfinished-tasks">
                      <p className="due-heading late">
                        UNFINISHED <span>{visibleUnfinished.length}</span>
                        <button
                          className="text-button"
                          onClick={() =>
                            void mover.move(visibleUnfinished, {
                              kind: "day",
                              day: today,
                            })
                          }
                        >
                          {visibleUnfinished.length === 1
                            ? "Move to today"
                            : "Move all to today"}{" "}
                          <Glyph>→</Glyph>
                        </button>
                      </p>
                      <div className="task-list">
                        {visibleUnfinished.map((task) => (
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
                            busy={mover.busyIds.has(task.id)}
                            moveItems={moveItemsFor(task)}
                            onToggle={() => void toggleTask(task)}
                            onOpen={() => setTaskEditor({ task })}
                          />
                        ))}
                      </div>
                    </div>
                  )}
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
                            weekStart={currentWeekStart}
                            busy={
                              busyTaskId === task.id ||
                              mover.busyIds.has(task.id)
                            }
                            moveItems={moveItemsFor(task)}
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
                <CapacityCard
                  capacity={dayCapacity}
                  isToday={day === today}
                  onOpenCalendars={() => navigate({ kind: "calendars" })}
                />
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
          onSaved={async (planId) => {
            setPlanEditorOpen(false);
            await refreshPlans();
            navigate({ kind: "plan", planId, tab: "overview" });
          }}
          onError={setError}
        />
      )}
      {converting?.kind === "task" && (
        <TaskEditor
          inboxItem={converting.item}
          plans={plans}
          people={people}
          onClose={() => setConverting(null)}
          onSaved={async () => {
            setConverting(null);
            await dataChanged();
          }}
          onError={setError}
        />
      )}
      {converting?.kind === "plan" && (
        <PlanEditor
          inboxItem={converting.item}
          defaultColor={
            planColors.find(
              (color) => !activePlans.some((plan) => plan.color === color),
            ) ?? planColors[plans.length % planColors.length]
          }
          onClose={() => setConverting(null)}
          onSaved={async () => {
            setConverting(null);
            await dataChanged();
          }}
          onError={setError}
        />
      )}
      {converting?.kind === "event" && (
        <EventEditor
          day={today}
          inboxItem={converting.item}
          plans={plans}
          people={people}
          onClose={() => setConverting(null)}
          onSaved={async () => {
            setConverting(null);
            await dataChanged();
          }}
          onError={setError}
        />
      )}
      {captureOpen && (
        <QuickCapture
          onClose={() => setCaptureOpen(false)}
          onCaptured={refreshInbox}
          onError={setError}
        />
      )}
      {mover.dialog}
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
  items,
  focusKey,
  planById,
  onEdit,
  onDelete,
  onOpenBlock,
  onAdd,
}: {
  items: AgendaItem[];
  /** The live or next item, drawn in the highlighted glass style. */
  focusKey: string | null;
  planById: Map<string, Plan>;
  onEdit: (event: ScheduleEvent) => void;
  onDelete: (event: ScheduleEvent) => void;
  onOpenBlock: (scheduled: ScheduledBlock) => void;
  onAdd: () => void;
}) {
  if (items.length === 0)
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
      {items.map((item) => {
        const current = item.key === focusKey;
        switch (item.kind) {
          case "event":
            return (
              <EventRow
                key={item.key}
                event={item.event}
                current={current}
                plan={
                  item.event.planId
                    ? planById.get(item.event.planId)
                    : undefined
                }
                onEdit={() => onEdit(item.event)}
                onDelete={() => onDelete(item.event)}
              />
            );
          case "block":
            return (
              <BlockRow
                key={item.key}
                scheduled={item.scheduled}
                current={current}
                plan={
                  item.scheduled.task.planId
                    ? planById.get(item.scheduled.task.planId)
                    : undefined
                }
                onOpen={() => onOpenBlock(item.scheduled)}
              />
            );
          case "external":
            return (
              <ExternalEventRow
                key={item.key}
                event={item.event}
                calendar={item.calendar}
                current={current}
              />
            );
        }
      })}
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

/** Time reserved for a task: planned work, drawn apart from events. */
function BlockRow({
  scheduled,
  current,
  plan,
  onOpen,
}: {
  scheduled: ScheduledBlock;
  current: boolean;
  plan: Plan | undefined;
  onOpen: () => void;
}) {
  const { block, task } = scheduled;
  const done = task.status === "done";
  return (
    <article
      className={`event-row block-row ${current ? "current" : ""} ${done ? "done" : ""}`}
      style={{ "--plan-color": planColor(plan) } as CSSProperties}
    >
      <time dateTime={block.startAtUtc}>
        {timeLabel(block.startAtUtc)}
        <span>{hoursLabel(block.durationMinutes)}</span>
      </time>
      <div className="event-connector" aria-hidden="true">
        <i />
      </div>
      <button
        className="event-card"
        onClick={onOpen}
        aria-label={`Time block for ${task.title}, ${hoursLabel(block.durationMinutes)}. Move or remove it`}
      >
        <span className="block-check" aria-hidden="true" />
        <span className="event-main">
          <small className="block-kicker">Time block</small>
          <strong>{task.title}</strong>
        </span>
        <PlanChip plan={plan} />
      </button>
      <span />
    </article>
  );
}

/** An occurrence from a read-only calendar. It can't be edited here. */
function ExternalEventRow({
  event,
  calendar,
  current,
}: {
  event: ExternalEvent;
  calendar: Calendar | undefined;
  current: boolean;
}) {
  const start = event.startAtUtc as string;
  const end = event.endAtUtc ?? start;
  return (
    <article
      className={`event-row external-row ${current ? "current" : ""} ${event.busy ? "" : "free"}`}
      style={{ "--calendar-color": calendarColor(calendar) } as CSSProperties}
    >
      <time dateTime={start}>
        {timeLabel(start)}
        <span>{timeRangeLabel(start, end).split("–")[1] ?? " "}</span>
      </time>
      <div className="event-connector" aria-hidden="true">
        <i />
      </div>
      <div className="event-card external-card">
        <span className="event-swatch" aria-hidden="true" />
        <span className="event-main">
          <strong>{event.title}</strong>
          {event.location && <small>{event.location}</small>}
          <small className="external-meta">
            {calendar?.name ?? "Calendar"}
            {event.tentative ? " · Tentative" : ""}
            {event.busy ? "" : " · Free"}
            {" · Read-only"}
          </small>
        </span>
      </div>
      <span />
    </article>
  );
}

function AllDayChip({
  event,
  calendar,
}: {
  event: ExternalEvent;
  calendar: Calendar | undefined;
}) {
  return (
    <li
      className={`all-day-chip ${event.busy ? "busy" : ""}`}
      style={{ "--calendar-color": calendarColor(calendar) } as CSSProperties}
    >
      <i aria-hidden="true" />
      <span>{event.title}</span>
      <small>
        All day · {calendar?.name ?? "Calendar"}
        {event.busy ? " · Busy" : ""}
      </small>
    </li>
  );
}

/** Today's planned work against the time left in working hours. */
function CapacityCard({
  capacity,
  isToday,
  onOpenCalendars,
}: {
  capacity: Capacity | null;
  isToday: boolean;
  onOpenCalendars: () => void;
}) {
  const day = capacity?.days[0];
  if (!capacity || !day) return null;
  const totals = capacityTotals(capacity, { includePool: false });
  const line = capacityLine(totals);
  const scale = Math.max(totals.plannedMinutes, totals.availableMinutes, 1);
  return (
    <section
      className={`capacity-card ${line.over ? "over" : ""}`}
      aria-label="Capacity"
    >
      <p className="side-kicker">
        {isToday ? "TODAY'S CAPACITY" : "THIS DAY'S CAPACITY"}
      </p>
      {day.workingMinutes === 0 ? (
        <strong>Not a working day</strong>
      ) : (
        <strong>{line.label}</strong>
      )}
      {day.workingMinutes > 0 && (
        <div className="capacity-bars" aria-hidden="true">
          <span
            className="planned"
            style={{ width: `${(totals.plannedMinutes / scale) * 100}%` }}
          />
          <span
            className="available"
            style={{ width: `${(totals.availableMinutes / scale) * 100}%` }}
          />
        </div>
      )}
      <ul>
        {line.over && (
          <li className="warning">
            Planned work is {hoursLabel(line.overBy)} more than the time
            available.
          </li>
        )}
        {day.workingMinutes > 0 && (
          <li>
            {hoursLabel(day.busyMinutes)} busy with events ·{" "}
            {hoursLabel(day.blockedMinutes)} blocked for tasks
          </li>
        )}
        {totals.unestimatedTasks > 0 && (
          <li>
            {totals.unestimatedTasks === 1
              ? "1 scheduled task has no estimate"
              : `${totals.unestimatedTasks} scheduled tasks have no estimates`}
          </li>
        )}
        {capacity.calendarsIncomplete && (
          <li>
            A calendar couldn't refresh, so some busy time may be missing.
          </li>
        )}
      </ul>
      <button className="text-button" onClick={onOpenCalendars}>
        {workingHoursLabel(capacity.workingHours)} <Glyph>→</Glyph>
      </button>
    </section>
  );
}

/** Offers to give back future time held by tasks that are already done. */
function ReleaseNotice({
  blocks,
  onRelease,
  onKeep,
}: {
  blocks: ScheduledBlock[];
  onRelease: () => void;
  onKeep: () => void;
}) {
  const tasks = [...new Set(blocks.map((scheduled) => scheduled.task.title))];
  const minutes = blocks.reduce(
    (total, scheduled) => total + scheduled.block.durationMinutes,
    0,
  );
  return (
    <section className="day-notice" aria-label="Finished work with time blocks">
      <Mark size={7} filled color="var(--success-dot)" />
      <p>
        <strong>
          {tasks.length === 1
            ? `“${tasks[0]}” is done but still holds time`
            : `${tasks.length} finished tasks still hold time`}
        </strong>
        <span>
          {blocks.length === 1
            ? "1 future block"
            : `${blocks.length} future blocks`}{" "}
          · {hoursLabel(minutes)}. Past blocks stay as a record.
        </span>
      </p>
      <button className="secondary-button" onClick={onKeep}>
        Keep
      </button>
      <button className="primary-button" onClick={onRelease}>
        Release time
      </button>
    </section>
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

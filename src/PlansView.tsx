import { CSSProperties, useEffect, useRef, useState } from "react";
import {
  api,
  messageFor,
  PlanSummary,
  PlanTemplate,
  PlanTemplateInfo,
} from "./api";
import { Glyph, Mark } from "./Geometry";
import { StatusPill } from "./PlanControls";
import {
  isOverdue,
  plural,
  planColor,
  planDeletionMessage,
  planDeletionPreview,
  planUpdate,
  shortDate,
} from "./planning";
import { useHeadingFocus } from "./useHeadingFocus";

export function PlansView({
  summaries,
  archived,
  today,
  focusToken,
  onOpenPlan,
  onNewPlan,
  onShowArchived,
  onChanged,
  onMessage,
}: {
  summaries: PlanSummary[];
  archived: boolean;
  today: string;
  focusToken: number;
  onOpenPlan: (planId: string) => void;
  /** Opens the new-plan editor, blank or starting from a template. */
  onNewPlan: (template: PlanTemplate | null) => void;
  onShowArchived: (archived: boolean) => void;
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [busyId, setBusyId] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  useHeadingFocus(headingRef, focusToken);
  const visible = summaries.filter(
    (summary) => summary.plan.archived === archived,
  );
  const otherCount = summaries.length - visible.length;

  async function run(planId: string, action: () => Promise<void>) {
    setBusyId(planId);
    try {
      await action();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusyId(null);
    }
  }

  function setArchived({ plan }: PlanSummary, nextArchived: boolean) {
    void run(plan.id, async () => {
      await api.updatePlan(planUpdate(plan, nextArchived));
      await onChanged();
    });
  }

  function remove({ plan }: PlanSummary) {
    void run(plan.id, async () => {
      const workspace = await api.getPlanWorkspace(plan.id);
      if (
        !window.confirm(
          planDeletionMessage(plan.title, planDeletionPreview(workspace)),
        )
      )
        return;
      await api.deletePlan(plan.id, workspace.plan.revision);
      await onChanged();
      onMessage(`Deleted “${plan.title}”.`);
    });
  }

  return (
    <div className="plans-page">
      <header className="topbar">
        <div className="date-heading">
          <p>{archived ? "OUT OF THE WAY" : "LONG RANGE"}</p>
          <h1 ref={headingRef} tabIndex={-1}>
            {archived ? "Archived plans" : "Plans"}
          </h1>
        </div>
        <div className="date-controls">
          <button
            className="secondary-button"
            onClick={() => onShowArchived(!archived)}
          >
            {archived ? "Active plans" : "Archived"} · {otherCount}
          </button>
          {!archived && (
            <button className="primary-button" onClick={() => onNewPlan(null)}>
              <Mark filled />
              New plan
            </button>
          )}
        </div>
      </header>
      <p className="page-intro">
        {archived
          ? "Archived plans keep their milestones, tasks, and events. Restore a plan to bring it back, or delete it permanently."
          : "A plan holds milestones, tasks, and events. Everything here resolves down to something that fits on a single day."}
      </p>
      {visible.length === 0 && !archived && summaries.length === 0 ? (
        <FirstPlan onNewPlan={onNewPlan} onMessage={onMessage} />
      ) : visible.length === 0 ? (
        <div className="empty-agenda">
          <i className="empty-mark" aria-hidden="true" />
          <p>
            {archived
              ? "Nothing archived."
              : "No active plans. Start with the next thing on the horizon."}
          </p>
          {!archived && (
            <button
              className="secondary-button"
              onClick={() => onNewPlan(null)}
            >
              <Glyph>+</Glyph> Create a plan
            </button>
          )}
        </div>
      ) : (
        <div className="plan-grid">
          {visible.map((summary) => (
            <PlanCard
              key={summary.plan.id}
              summary={summary}
              today={today}
              busy={busyId === summary.plan.id}
              onOpen={() => onOpenPlan(summary.plan.id)}
              onArchive={
                archived ? undefined : () => setArchived(summary, true)
              }
              onRestore={
                archived ? () => setArchived(summary, false) : undefined
              }
              onDelete={archived ? () => remove(summary) : undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * The Plans page before there are any plans: a blank plan, or a template that starts with a few
 * workstreams to rename or remove.
 */
function FirstPlan({
  onNewPlan,
  onMessage,
}: {
  onNewPlan: (template: PlanTemplate | null) => void;
  onMessage: (message: string) => void;
}) {
  const [templates, setTemplates] = useState<PlanTemplateInfo[] | null>(null);
  useEffect(() => {
    let active = true;
    api
      .listPlanTemplates()
      .then((list) => {
        if (active) setTemplates(list);
      })
      .catch((cause) => {
        if (active) setTemplates([]);
        onMessage(messageFor(cause));
      });
    return () => {
      active = false;
    };
    // Loaded once; `onMessage` only reports a failed load.
  }, []);
  return (
    <section className="first-plan" aria-labelledby="first-plan-heading">
      <h2 id="first-plan-heading">Start your first plan</h2>
      <p>
        A plan is anything bigger than a day: a trip, a move, a launch. Start
        blank, or from a template that sets up a few workstreams you can rename
        or remove.
      </p>
      <div className="template-grid">
        <button className="template-card" onClick={() => onNewPlan(null)}>
          <strong>
            <Glyph>+</Glyph> Blank plan
          </strong>
          <span>Just a title, dates, and an outcome to aim for.</span>
        </button>
        {(templates ?? []).map((item) => (
          <button
            key={item.template}
            className="template-card"
            onClick={() => onNewPlan(item.template)}
          >
            <strong>{item.label}</strong>
            <span>{item.workstreams.join(" · ")}</span>
          </button>
        ))}
      </div>
    </section>
  );
}

function PlanCard({
  summary,
  today,
  busy,
  onOpen,
  onArchive,
  onRestore,
  onDelete,
}: {
  summary: PlanSummary;
  today: string;
  busy: boolean;
  onOpen: () => void;
  onArchive?: () => void;
  onRestore?: () => void;
  onDelete?: () => void;
}) {
  const {
    plan,
    milestoneCount,
    taskCount,
    completedTaskCount,
    overdueTaskCount,
    nextMilestone,
  } = summary;
  const progress = taskCount ? (completedTaskCount / taskCount) * 100 : 0;
  const color = planColor(plan);
  return (
    <article
      className="plan-card"
      style={{ "--plan-color": color } as CSSProperties}
      aria-labelledby={`plan-card-${plan.id}`}
    >
      <div className="plan-card-main">
        <div className="plan-card-title">
          <strong id={`plan-card-${plan.id}`}>{plan.title}</strong>
          <StatusPill status={plan.status} />
        </div>
        <small className="plan-card-counts">
          {plural(milestoneCount, "milestone")} · {plural(taskCount, "task")}
        </small>
        {plan.description && <p>{plan.description}</p>}
        <span
          className="plan-progress"
          role="img"
          aria-label={
            taskCount
              ? `${completedTaskCount} of ${plural(taskCount, "task")} done`
              : "No tasks yet"
          }
        >
          <i style={{ width: `${progress}%` }} />
        </span>
        <div className="plan-card-foot">
          {nextMilestone && (
            <span
              className={
                isOverdue(nextMilestone.targetDate, today) ? "late" : ""
              }
            >
              <Mark size={5} color={color} filled />
              Next: {nextMilestone.title}
              {nextMilestone.targetDate &&
                ` · ${shortDate(nextMilestone.targetDate)}`}
            </span>
          )}
          {plan.targetDate && (
            <span>
              <Mark size={5} />
              Target {shortDate(plan.targetDate)}
            </span>
          )}
          {overdueTaskCount > 0 && (
            <span className="late">
              <Mark size={5} color="var(--danger-dot)" filled />
              {plural(overdueTaskCount, "task")} overdue
            </span>
          )}
        </div>
      </div>
      <div className="plan-card-actions">
        <button
          className="open"
          onClick={onOpen}
          aria-label={`Open ${plan.title}`}
        >
          Open
        </button>
        {onArchive && (
          <button
            onClick={onArchive}
            disabled={busy}
            aria-label={`Archive ${plan.title}`}
          >
            Archive
          </button>
        )}
        {onRestore && (
          <button
            onClick={onRestore}
            disabled={busy}
            aria-label={`Restore ${plan.title}`}
          >
            Restore
          </button>
        )}
        {onDelete && (
          <button
            className="danger"
            onClick={onDelete}
            disabled={busy}
            aria-label={`Delete ${plan.title} permanently`}
          >
            Delete
          </button>
        )}
      </div>
    </article>
  );
}

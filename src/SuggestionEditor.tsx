import { FormEvent, ReactNode, useState } from "react";
import { api, DayChange, messageFor, ProposalChange } from "./api";
import { dateTimeFields, weekStartDay, weekStartsOn } from "./date";

/**
 * Whether the review lets the user change anything in a suggestion before applying it. Only its
 * values can change: what it targets and where it goes stay as the planner proposed, and Rust
 * checks that again when the proposal is applied.
 */
export function canEditSuggestion(change: ProposalChange) {
  switch (change.type) {
    case "create_event":
    case "reschedule_event":
    case "create_plan":
    case "create_milestone":
    case "create_task":
    case "create_workstream":
      return true;
    case "schedule_task":
      return [change.scheduledDay, change.dueDate, change.plannedWeek].some(
        (day) => day.action === "set",
      );
    case "update_task":
      return change.title !== null || change.estimate.action === "set";
    default:
      return false;
  }
}

/** The local day and time an event suggestion starts at, in its own time zone. */
function startOf(change: ProposalChange) {
  return change.type === "create_event" || change.type === "reschedule_event"
    ? dateTimeFields(change.startAtUtc, change.timeZone)
    : null;
}

/** Edits one suggestion's values inline in the review; nothing is saved until the proposal is. */
export function SuggestionEditor({
  change,
  onSave,
  onCancel,
}: {
  change: ProposalChange;
  onSave: (change: ProposalChange) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<ProposalChange>(change);
  const [start, setStart] = useState(() => startOf(change));
  const [problem, setProblem] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // Each field below belongs to the draft's own type, so a patch never crosses types.
  const update = (patch: Record<string, unknown>) =>
    setDraft((current) => ({ ...current, ...patch }) as ProposalChange);

  async function save(event: FormEvent) {
    event.preventDefault();
    setProblem(null);
    let next = draft;
    const original = startOf(change);
    if (
      start &&
      original &&
      (next.type === "create_event" || next.type === "reschedule_event") &&
      (start.day !== original.day || start.time !== original.time)
    ) {
      setSaving(true);
      try {
        const resolved = await api.resolveLocalDateTime(
          start.day,
          start.time,
          next.timeZone,
        );
        if (resolved.kind === "ambiguous")
          return setProblem(
            "That time happens twice that day because the clocks change. Choose another, or adjust the event after applying.",
          );
        if (resolved.kind === "nonexistent")
          return setProblem(resolved.message);
        next = { ...next, startAtUtc: resolved.startAtUtc };
      } catch (cause) {
        return setProblem(messageFor(cause));
      } finally {
        setSaving(false);
      }
    }
    onSave(next);
  }

  const text = (
    label: string,
    field: "title" | "name",
    value: string,
    maxLength = 140,
  ) => (
    <label>
      {label}
      <input
        value={value}
        required
        maxLength={maxLength}
        onChange={(input) => update({ [field]: input.target.value })}
      />
    </label>
  );
  const optionalDay = (label: string, field: string, value: string | null) => (
    <label>
      {label}
      <input
        type="date"
        value={value ?? ""}
        onChange={(input) => update({ [field]: input.target.value || null })}
      />
    </label>
  );
  const dayChange = (
    label: string,
    field: string,
    value: DayChange,
    week = false,
  ) =>
    value.action === "set" && (
      <label>
        {label}
        <input
          type="date"
          value={value.day}
          required
          onChange={(input) =>
            input.target.value &&
            update({
              [field]: {
                action: "set",
                day: week
                  ? weekStartDay(input.target.value, weekStartsOn)
                  : input.target.value,
              },
            })
          }
        />
      </label>
    );
  const minutes = (
    label: string,
    value: number,
    min: number,
    onValue: (minutes: number) => void,
  ) => (
    <label>
      {label}, in minutes
      <input
        type="number"
        min={min}
        max={1440}
        required
        value={value}
        onChange={(input) =>
          onValue(Math.round(Number(input.target.value) || 0))
        }
      />
    </label>
  );
  const startFields = start && (
    <div className="suggestion-pair">
      <label>
        Day
        <input
          type="date"
          value={start.day}
          required
          onChange={(input) => setStart({ ...start, day: input.target.value })}
        />
      </label>
      <label>
        Time
        <input
          type="time"
          value={start.time}
          required
          onChange={(input) => setStart({ ...start, time: input.target.value })}
        />
      </label>
    </div>
  );

  let fields: ReactNode = null;
  switch (draft.type) {
    case "create_task":
      fields = (
        <>
          {text("Title", "title", draft.title)}
          <div className="suggestion-pair">
            {optionalDay("Do on", "scheduledDay", draft.scheduledDay)}
            {optionalDay("Due", "dueDate", draft.dueDate)}
          </div>
        </>
      );
      break;
    case "create_event":
      fields = (
        <>
          {text("Title", "title", draft.title)}
          {startFields}
          {minutes("Length", draft.durationMinutes, 5, (value) =>
            update({ durationMinutes: value }),
          )}
        </>
      );
      break;
    case "reschedule_event":
      fields = (
        <>
          {startFields}
          {draft.durationMinutes !== null &&
            minutes("Length", draft.durationMinutes, 5, (value) =>
              update({ durationMinutes: value }),
            )}
        </>
      );
      break;
    case "create_plan":
    case "create_milestone":
      fields = (
        <>
          {text("Title", "title", draft.title)}
          {optionalDay("Target date", "targetDate", draft.targetDate)}
        </>
      );
      break;
    case "create_workstream":
      fields = text("Name", "name", draft.name, 80);
      break;
    case "schedule_task":
      fields = (
        <div className="suggestion-pair">
          {dayChange("Do on", "scheduledDay", draft.scheduledDay)}
          {dayChange("Due", "dueDate", draft.dueDate)}
          {dayChange("Week of", "plannedWeek", draft.plannedWeek, true)}
        </div>
      );
      break;
    case "update_task": {
      const estimate = draft.estimate;
      fields = (
        <>
          {draft.title !== null && text("New title", "title", draft.title)}
          {estimate.action === "set" &&
            minutes("Takes", estimate.minutes, 1, (value) =>
              update({ estimate: { action: "set", minutes: value } }),
            )}
        </>
      );
      break;
    }
  }

  return (
    <form className="suggestion-editor" onSubmit={save}>
      {fields}
      {problem && (
        <p className="suggestion-problem" role="alert">
          {problem}
        </p>
      )}
      <div className="suggestion-editor-actions">
        <button type="button" className="text-button" onClick={onCancel}>
          Cancel
        </button>
        <button className="secondary-button" disabled={saving}>
          Done
        </button>
      </div>
    </form>
  );
}

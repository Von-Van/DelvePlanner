import { KeyboardEvent, useEffect, useState } from "react";
import { parseISO } from "date-fns";
import {
  api,
  ChecklistItem,
  messageFor,
  Plan,
  Recurrence,
  RecurrenceFrequency,
  TaskReference,
} from "./api";
import { Glyph } from "./Geometry";
import { recurrenceLabel, shortDate } from "./planning";

const MAX_CHECKLIST_ITEMS = 30;
const MAX_WAITING_ON = 10;
const weekdayLabels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** Monday = 1 through Sunday = 7, as the repeat rule stores weekdays. */
function isoWeekday(day: string) {
  return ((parseISO(day).getDay() + 6) % 7) + 1;
}

/**
 * How a task repeats. A rule needs a scheduled day to start from, and a finished task can't take
 * one: finishing an open repeating task is what brings its next occurrence.
 */
export function RepeatField({
  value,
  scheduledDay,
  finished,
  onChange,
}: {
  value: Recurrence | null;
  scheduledDay: string | null;
  finished: boolean;
  onChange: (value: Recurrence | null) => void;
}) {
  const unavailable = scheduledDay === null || finished;
  function choose(frequency: RecurrenceFrequency | "") {
    if (frequency === "" || scheduledDay === null) return onChange(null);
    onChange({
      frequency,
      interval: frequency === "weekdays" ? 1 : (value?.interval ?? 1),
      weekdays: frequency === "weekly" ? [isoWeekday(scheduledDay)] : [],
      monthDay:
        frequency === "monthly" ? parseISO(scheduledDay).getDate() : null,
    });
  }
  const unit =
    value?.frequency === "daily"
      ? "day"
      : value?.frequency === "weekly"
        ? "week"
        : "month";
  return (
    <fieldset className="repeat-field">
      <legend>Repeats</legend>
      <div className="form-pair">
        <label>
          How often
          <select
            value={value?.frequency ?? ""}
            disabled={unavailable}
            onChange={(input) =>
              choose(input.target.value as RecurrenceFrequency | "")
            }
          >
            <option value="">Doesn't repeat</option>
            <option value="daily">Daily</option>
            <option value="weekdays">Every weekday</option>
            <option value="weekly">Weekly</option>
            <option value="monthly">Monthly</option>
          </select>
        </label>
        {value && value.frequency !== "weekdays" && (
          <label>
            Every
            <span className="unit-input">
              <input
                type="number"
                min={1}
                max={99}
                required
                value={value.interval}
                onChange={(input) =>
                  onChange({
                    ...value,
                    interval: Math.min(
                      99,
                      Math.max(1, Math.round(Number(input.target.value) || 1)),
                    ),
                  })
                }
              />
              {value.interval === 1 ? unit : `${unit}s`}
            </span>
          </label>
        )}
      </div>
      {value?.frequency === "weekly" && (
        <div
          className="weekday-toggles"
          role="group"
          aria-label="On these days"
        >
          {weekdayLabels.map((label, index) => {
            const weekday = index + 1;
            const chosen = value.weekdays.includes(weekday);
            return (
              <button
                type="button"
                key={label}
                aria-pressed={chosen}
                className={chosen ? "selected" : ""}
                // A weekly rule keeps at least one day.
                disabled={chosen && value.weekdays.length === 1}
                onClick={() =>
                  onChange({
                    ...value,
                    weekdays: chosen
                      ? value.weekdays.filter((day) => day !== weekday)
                      : [...value.weekdays, weekday].sort(
                          (left, right) => left - right,
                        ),
                  })
                }
              >
                {label}
              </button>
            );
          })}
        </div>
      )}
      {value?.frequency === "monthly" && (
        <label>
          On day <span>Shorter months use their last day</span>
          <input
            type="number"
            min={1}
            max={31}
            required
            value={value.monthDay ?? 1}
            onChange={(input) =>
              onChange({
                ...value,
                monthDay: Math.min(
                  31,
                  Math.max(1, Math.round(Number(input.target.value) || 1)),
                ),
              })
            }
          />
        </label>
      )}
      {finished ? (
        <p className="field-note">A finished task doesn't repeat.</p>
      ) : scheduledDay === null ? (
        <p className="field-note">
          Schedule it for a day to repeat it from there.
        </p>
      ) : (
        value && (
          <p className="field-note">
            {recurrenceLabel(value)}. Finishing it brings the next one; a missed
            one waits under Unfinished until you move, skip, or finish it.
          </p>
        )
      )}
    </fieldset>
  );
}

/** Steps inside the task: ticked here or from the task's row, never tasks of their own. */
export function ChecklistField({
  items,
  onChange,
}: {
  items: ChecklistItem[];
  onChange: (items: ChecklistItem[]) => void;
}) {
  const [text, setText] = useState("");
  const done = items.filter((item) => item.done).length;
  function add() {
    const trimmed = text.trim();
    if (!trimmed || items.length >= MAX_CHECKLIST_ITEMS) return;
    onChange([...items, { text: trimmed, done: false }]);
    setText("");
  }
  function onKey(event: KeyboardEvent<HTMLInputElement>) {
    // Enter adds the step instead of saving the whole task.
    if (event.key === "Enter") {
      event.preventDefault();
      add();
    }
  }
  return (
    <fieldset className="checklist-field">
      <legend>
        Checklist{" "}
        <span>
          {items.length > 0
            ? `${done} of ${items.length} done`
            : "Steps inside this task"}
        </span>
      </legend>
      {items.length > 0 && (
        <ul>
          {items.map((item, index) => (
            <li key={index}>
              <input
                type="checkbox"
                checked={item.done}
                aria-label={`Done: ${item.text || `step ${index + 1}`}`}
                onChange={() =>
                  onChange(
                    items.map((current, position) =>
                      position === index
                        ? { ...current, done: !current.done }
                        : current,
                    ),
                  )
                }
              />
              <input
                value={item.text}
                maxLength={140}
                aria-label={`Step ${index + 1}`}
                onChange={(input) =>
                  onChange(
                    items.map((current, position) =>
                      position === index
                        ? { ...current, text: input.target.value }
                        : current,
                    ),
                  )
                }
              />
              <button
                type="button"
                aria-label={`Remove ${item.text || `step ${index + 1}`}`}
                onClick={() =>
                  onChange(items.filter((_, position) => position !== index))
                }
              >
                <Glyph>✕</Glyph>
              </button>
            </li>
          ))}
        </ul>
      )}
      {items.length < MAX_CHECKLIST_ITEMS && (
        <div className="checklist-add">
          <input
            value={text}
            maxLength={140}
            placeholder="Add a step"
            aria-label="New checklist step"
            onChange={(input) => setText(input.target.value)}
            onKeyDown={onKey}
          />
          <button type="button" onClick={add} disabled={!text.trim()}>
            Add
          </button>
        </div>
      )}
    </fieldset>
  );
}

/**
 * The tasks this one waits on: a hint shown beside the task, never a lock. Only open tasks can be
 * chosen; one that has since been finished shows as such until it's removed.
 */
export function WaitsOnField({
  taskId,
  value,
  plans,
  onChange,
  onError,
}: {
  taskId?: string;
  value: string[];
  plans: Plan[];
  onChange: (value: string[]) => void;
  onError: (message: string) => void;
}) {
  const [references, setReferences] = useState<TaskReference[] | null>(null);
  useEffect(() => {
    let active = true;
    api
      .listOpenTaskReferences()
      .then((list) => {
        if (active) setReferences(list);
      })
      .catch((cause) => onError(messageFor(cause)));
    return () => {
      active = false;
    };
    // Loaded once per editor; `onError` only reports a failed load.
  }, []);
  const byId = new Map((references ?? []).map((item) => [item.id, item]));
  const planTitles = new Map(plans.map((plan) => [plan.id, plan.title]));
  const choices = (references ?? []).filter(
    (item) => item.id !== taskId && !value.includes(item.id),
  );
  const groups = new Map<string, TaskReference[]>();
  for (const choice of choices) {
    const group = choice.planId
      ? (planTitles.get(choice.planId) ?? "Another plan")
      : "No plan";
    groups.set(group, [...(groups.get(group) ?? []), choice]);
  }
  return (
    <fieldset className="waits-field">
      <legend>
        Waits on <span>A hint beside the task; nothing is blocked</span>
      </legend>
      {value.length > 0 && (
        <ul className="wait-chips">
          {value.map((id) => {
            const reference = byId.get(id);
            const label = reference
              ? reference.title
              : references === null
                ? "Loading…"
                : "A finished task";
            return (
              <li key={id}>
                {label}
                <button
                  type="button"
                  aria-label={`Stop waiting on ${label}`}
                  onClick={() => onChange(value.filter((item) => item !== id))}
                >
                  <Glyph>✕</Glyph>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {value.length < MAX_WAITING_ON && (
        <select
          value=""
          aria-label="Add a task this one waits on"
          disabled={references === null || choices.length === 0}
          onChange={(input) => {
            if (input.target.value) onChange([...value, input.target.value]);
          }}
        >
          <option value="">
            {references === null
              ? "Loading tasks…"
              : choices.length > 0
                ? "Add a task it waits on…"
                : "No other open tasks"}
          </option>
          {[...groups].map(([group, items]) => (
            <optgroup key={group} label={group}>
              {items.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.title}
                  {item.scheduledDay
                    ? ` · ${shortDate(item.scheduledDay)}`
                    : item.dueDate
                      ? ` · due ${shortDate(item.dueDate)}`
                      : ""}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      )}
    </fieldset>
  );
}

import { FormEvent, useRef, useState } from "react";
import {
  api,
  LocalDateTimeResolution,
  messageFor,
  Person,
  Plan,
  ScheduleEvent,
} from "./api";
import { Glyph, Spinner } from "./Geometry";
import { localTimeZone } from "./date";
import {
  draftFor,
  EventDraft,
  reminderPresets,
  saveEventDraft,
} from "./events";
import { PersonSelect, PlanSelect, WorkstreamSelect } from "./PlanControls";
import { useModalFocus } from "./useModalFocus";
import { usePlanLinks } from "./usePlanLinks";

export function EventEditor({
  day,
  event,
  plans,
  people,
  defaultPlanId = null,
  defaultTime,
  onClose,
  onSaved,
  onError,
}: {
  day: string;
  event?: ScheduleEvent;
  plans: Plan[];
  people: Person[];
  defaultPlanId?: string | null;
  /** A starting time for new events, such as the end of the previous run-of-show cue. */
  defaultTime?: string;
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState(() => ({
    ...draftFor(day, event, defaultPlanId),
    ...(!event && defaultTime ? { time: defaultTime } : {}),
  }));
  const dialogRef = useRef<HTMLFormElement>(null);
  const [saving, setSaving] = useState(false);
  const [resolution, setResolution] = useState<LocalDateTimeResolution | null>(
    null,
  );
  useModalFocus(dialogRef, onClose, saving);
  const { workstreams } = usePlanLinks(draft.planId, onError);

  async function persist(startAtUtc: string) {
    await saveEventDraft(draft, event, startAtUtc);
    await onSaved();
  }
  async function run(action: () => Promise<void>) {
    setSaving(true);
    try {
      await action();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }
  async function submit(form: FormEvent) {
    form.preventDefault();
    await run(async () => {
      const next = await api.resolveLocalDateTime(
        draft.day,
        draft.time,
        localTimeZone,
      );
      setResolution(next);
      if (next.kind === "resolved") await persist(next.startAtUtc);
    });
  }
  const updateDraft = (next: EventDraft) => {
    setDraft(next);
    setResolution(null);
  };
  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(click) => {
        if (click.target === click.currentTarget && !saving) onClose();
      }}
    >
      <form
        ref={dialogRef}
        className="editor-dialog"
        onSubmit={submit}
        role="dialog"
        aria-modal="true"
        aria-label={event ? "Edit event" : "New event"}
      >
        <header>
          <div>
            <p>{event ? "EDIT TIME BLOCK" : "NEW TIME BLOCK"}</p>
            <h2>{event ? "Refine the details" : "Make room for it"}</h2>
          </div>
          <button type="button" onClick={onClose} aria-label="Close editor">
            <Glyph>✕</Glyph>
          </button>
        </header>
        <label>
          Title
          <input
            autoFocus
            value={draft.title}
            onChange={(input) =>
              updateDraft({ ...draft, title: input.target.value })
            }
            required
            maxLength={140}
            placeholder="What needs your time?"
          />
        </label>
        <div className="form-pair">
          <label>
            Date
            <input
              type="date"
              value={draft.day}
              onChange={(input) =>
                updateDraft({ ...draft, day: input.target.value })
              }
              required
            />
          </label>
          <label>
            Time
            <input
              type="time"
              value={draft.time}
              onChange={(input) =>
                updateDraft({ ...draft, time: input.target.value })
              }
              required
            />
          </label>
        </div>
        {resolution?.kind === "nonexistent" && (
          <div className="time-resolution" role="alert">
            {resolution.message}
          </div>
        )}
        {resolution?.kind === "ambiguous" && (
          <div className="time-resolution">
            <strong>This time happens twice.</strong>
            <p>Choose which clock occurrence you mean:</p>
            {resolution.options.map((option) => (
              <button
                type="button"
                key={option.startAtUtc}
                onClick={() => void run(() => persist(option.startAtUtc))}
              >
                {option.label}
              </button>
            ))}
          </div>
        )}
        <label>
          Duration <span>{draft.durationMinutes} minutes</span>
          <input
            className="range"
            type="range"
            min="5"
            max="240"
            step="5"
            value={draft.durationMinutes}
            onChange={(input) =>
              updateDraft({
                ...draft,
                durationMinutes: Number(input.target.value),
              })
            }
          />
        </label>
        <label>
          Reminder
          <select
            value={draft.reminderMinutesBefore ?? ""}
            onChange={(input) =>
              updateDraft({
                ...draft,
                reminderMinutesBefore:
                  input.target.value === "" ? null : Number(input.target.value),
              })
            }
          >
            {reminderPresets.map((preset) => (
              <option key={preset.value} value={preset.value}>
                {preset.label}
              </option>
            ))}
          </select>
          <span>
            DayPlan must remain running in the tray to deliver desktop
            reminders.
          </span>
        </label>
        <label>
          Location
          <input
            value={draft.location}
            onChange={(input) =>
              updateDraft({ ...draft, location: input.target.value })
            }
            maxLength={140}
            placeholder="Optional room, stage, link, or address"
          />
        </label>
        <div className="form-pair">
          <PlanSelect
            plans={plans}
            value={draft.planId}
            onChange={(planId) =>
              updateDraft({ ...draft, planId, workstreamId: null })
            }
          />
          <PersonSelect
            people={people}
            value={draft.ownerId}
            onChange={(ownerId) => updateDraft({ ...draft, ownerId })}
          />
        </div>
        {draft.planId && workstreams.length > 0 && (
          <WorkstreamSelect
            workstreams={workstreams}
            value={draft.workstreamId}
            disabled={false}
            onChange={(workstreamId) => updateDraft({ ...draft, workstreamId })}
          />
        )}
        <label>
          Notes
          <textarea
            value={draft.notes}
            onChange={(input) =>
              updateDraft({ ...draft, notes: input.target.value })
            }
            placeholder="Optional context for your future self"
            maxLength={800}
          />
        </label>
        <footer>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !draft.title.trim()}
          >
            {saving && <Spinner size={7} />}
            {event ? "Save changes" : "Create event"}
          </button>
        </footer>
      </form>
    </div>
  );
}

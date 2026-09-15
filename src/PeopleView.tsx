import { FormEvent, RefObject, useState } from "react";
import {
  api,
  messageFor,
  Person,
  PersonInput,
  PersonSummary,
  Plan,
  ScheduleEvent,
  Task,
} from "./api";
import { timeLabel } from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import { PlanChip } from "./PlanControls";
import { EditorShell } from "./PlanEditor";
import { eventDay, isOverdue, plural, shortDate } from "./planning";
import { useHeadingFocus } from "./useHeadingFocus";

export function PeopleView({
  summaries,
  planById,
  today,
  headingRef,
  focusToken,
  onChanged,
  onOpenTask,
  onOpenEvent,
  onMessage,
}: {
  summaries: PersonSummary[];
  planById: Map<string, Plan>;
  today: string;
  headingRef: RefObject<HTMLHeadingElement>;
  focusToken: number;
  onChanged: () => Promise<void>;
  onOpenTask: (task: Task) => void;
  onOpenEvent: (event: ScheduleEvent) => void;
  onMessage: (message: string) => void;
}) {
  const [editing, setEditing] = useState<Person | "new" | null>(null);
  useHeadingFocus(headingRef, focusToken);
  return (
    <div className="plans-page">
      <header className="topbar">
        <div className="date-heading">
          <p>TEAM</p>
          <h1 ref={headingRef} tabIndex={-1}>
            People
          </h1>
        </div>
        <div className="date-controls">
          <button className="primary-button" onClick={() => setEditing("new")}>
            <Mark filled />
            Add person
          </button>
        </div>
      </header>
      <p className="page-intro">
        People are local labels for who owns a task or event, such as a
        teammate, a vendor, or a venue contact. They are not accounts, and
        nothing is shared with them.
      </p>
      {summaries.length === 0 ? (
        <div className="empty-agenda">
          <i className="empty-mark" aria-hidden="true" />
          <p>No people yet. Add someone to start assigning owners.</p>
          <button
            className="secondary-button"
            onClick={() => setEditing("new")}
          >
            <Glyph>+</Glyph> Add a person
          </button>
        </div>
      ) : (
        <div className="plan-grid">
          {summaries.map((summary) => (
            <article className="person-card" key={summary.person.id}>
              <header>
                <span className="person-initials" aria-hidden="true">
                  {initials(summary.person.displayName)}
                </span>
                <span className="person-name">
                  <strong>{summary.person.displayName}</strong>
                  {summary.person.role && <small>{summary.person.role}</small>}
                </span>
                <button
                  className="text-button"
                  onClick={() => setEditing(summary.person)}
                  aria-label={`Edit ${summary.person.displayName}`}
                >
                  Edit
                </button>
              </header>
              {summary.person.email && (
                <p className="person-email">
                  <Mark shape="square" size={5} /> {summary.person.email}
                </p>
              )}
              <p className="card-kicker">
                {summary.openTaskCount
                  ? plural(summary.openTaskCount, "open task").toUpperCase()
                  : "NO OPEN TASKS"}
              </p>
              {summary.openTasks.length > 0 && (
                <ul className="person-work">
                  {summary.openTasks.map((task) => (
                    <li key={task.id}>
                      <button onClick={() => onOpenTask(task)}>
                        <span>{task.title}</span>
                        {task.dueDate && (
                          <small
                            className={
                              isOverdue(task.dueDate, today) ? "late" : ""
                            }
                          >
                            Due {shortDate(task.dueDate)}
                          </small>
                        )}
                        <PlanChip
                          plan={
                            task.planId ? planById.get(task.planId) : undefined
                          }
                        />
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              {summary.upcomingEvents.length > 0 && (
                <ul className="person-work events">
                  {summary.upcomingEvents.map((event) => (
                    <li key={event.id}>
                      <button onClick={() => onOpenEvent(event)}>
                        <Mark shape="circle" size={6} />
                        <span>{event.title}</span>
                        <small>
                          {shortDate(eventDay(event))},{" "}
                          {timeLabel(event.startAtUtc)}
                        </small>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </article>
          ))}
        </div>
      )}
      {editing && (
        <PersonEditor
          person={editing === "new" ? undefined : editing}
          onClose={() => setEditing(null)}
          onSaved={async () => {
            setEditing(null);
            await onChanged();
          }}
          onError={onMessage}
        />
      )}
    </div>
  );
}

function initials(name: string) {
  const letters = name
    .trim()
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? "");
  return letters.join("") || "?";
}

export function PersonEditor({
  person,
  onClose,
  onSaved,
  onError,
}: {
  person?: Person;
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const [draft, setDraft] = useState<PersonInput>(() => ({
    displayName: person?.displayName ?? "",
    role: person?.role ?? "",
    email: person?.email ?? null,
    notes: person?.notes ?? "",
  }));
  const [saving, setSaving] = useState(false);

  async function run(action: () => Promise<unknown>) {
    setSaving(true);
    try {
      await action();
      await onSaved();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  function submit(form: FormEvent) {
    form.preventDefault();
    void run(() =>
      person
        ? api.updatePerson({
            ...draft,
            id: person.id,
            revision: person.revision,
          })
        : api.createPerson(draft),
    );
  }

  function remove() {
    if (
      !person ||
      !window.confirm(
        `Remove ${person.displayName}? Tasks and events they own stay, without an owner.`,
      )
    )
      return;
    void run(() => api.deletePerson(person.id, person.revision));
  }

  return (
    <EditorShell
      label={person ? "Edit person" : "New person"}
      kicker={person ? "EDIT PERSON" : "NEW PERSON"}
      heading={person ? "Update the details" : "Who owns the work?"}
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          {person && (
            <button
              type="button"
              className="editor-delete"
              onClick={remove}
              disabled={saving}
            >
              Remove
            </button>
          )}
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !draft.displayName.trim()}
          >
            {saving && <Spinner size={7} />}
            {person ? "Save changes" : "Add person"}
          </button>
        </>
      }
    >
      <label>
        Name
        <input
          autoFocus
          value={draft.displayName}
          onChange={(input) =>
            setDraft({ ...draft, displayName: input.target.value })
          }
          required
          maxLength={80}
          placeholder="Maya, AV vendor, venue contact…"
        />
      </label>
      <div className="form-pair">
        <label>
          Role
          <input
            value={draft.role}
            onChange={(input) =>
              setDraft({ ...draft, role: input.target.value })
            }
            maxLength={80}
            placeholder="Optional"
          />
        </label>
        <label>
          Email
          <input
            type="email"
            value={draft.email ?? ""}
            onChange={(input) =>
              setDraft({ ...draft, email: input.target.value || null })
            }
            maxLength={254}
            placeholder="Optional"
          />
        </label>
      </div>
      <label>
        Notes
        <textarea
          value={draft.notes}
          onChange={(input) =>
            setDraft({ ...draft, notes: input.target.value })
          }
          maxLength={800}
          placeholder="Contact details, availability, or context"
        />
      </label>
    </EditorShell>
  );
}

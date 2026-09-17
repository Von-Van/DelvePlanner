import { FormEvent, RefObject, useRef, useState } from "react";
import { api, InboxItem, messageFor } from "./api";
import { dateTimeFields } from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import { EditorShell } from "./PlanEditor";
import { shortDate } from "./planning";

export type InboxConversionKind = "task" | "plan" | "event";

export function InboxView({
  items,
  headingRef,
  captureShortcut,
  onConvert,
  onChanged,
  onMessage,
}: {
  items: InboxItem[];
  headingRef: RefObject<HTMLHeadingElement>;
  /** How to open quick capture from anywhere, such as "⌘I". */
  captureShortcut: string;
  onConvert: (item: InboxItem, kind: InboxConversionKind) => void;
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [text, setText] = useState("");
  const [saving, setSaving] = useState(false);
  const [editing, setEditing] = useState<InboxItem | null>(null);

  async function capture(form: FormEvent) {
    form.preventDefault();
    const trimmed = text.trim();
    if (!trimmed) return;
    setSaving(true);
    try {
      await api.createInboxItem({ text: trimmed, notes: "" });
      setText("");
      await onChanged();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  async function remove(item: InboxItem) {
    if (!window.confirm(`Delete “${item.text}” from the Inbox?`)) return;
    try {
      await api.deleteInboxItem(item.id, item.revision);
      await onChanged();
    } catch (cause) {
      onMessage(messageFor(cause));
    }
  }

  return (
    <div className="inbox-page">
      <header className="topbar">
        <div className="date-heading">
          <p>CAPTURE</p>
          <h1 ref={headingRef} tabIndex={-1}>
            Inbox
          </h1>
        </div>
        <p className="topbar-note">
          {captureShortcut} captures from anywhere in DayPlan
        </p>
      </header>
      <p className="page-intro">
        Anything you capture waits here until you decide what it is. Nothing
        needs a plan, a date, or a priority yet.
      </p>
      <form className="task-add inbox-capture" onSubmit={capture}>
        <Mark size={11} color="var(--blue)" />
        <input
          value={text}
          onChange={(input) => setText(input.target.value)}
          placeholder="Call dentist, research hotels, ask Sarah about Saturday…"
          aria-label="Capture to the Inbox"
          maxLength={140}
        />
        <button aria-label="Capture" disabled={!text.trim() || saving}>
          {saving ? <Spinner size={7} /> : <Glyph>→</Glyph>}
        </button>
      </form>
      {items.length === 0 ? (
        <div className="empty-agenda inbox-empty">
          <i className="empty-mark" aria-hidden="true" />
          <p>The Inbox is clear.</p>
        </div>
      ) : (
        <ul className="inbox-list" aria-label="Captured items">
          {items.map((item) => (
            <li key={item.id} className="inbox-row">
              <Mark size={9} />
              <button
                className="inbox-text"
                onClick={() => setEditing(item)}
                aria-label={`Edit ${item.text}`}
              >
                <strong>{item.text}</strong>
                {item.notes && <small>{item.notes}</small>}
              </button>
              <time dateTime={item.createdAt}>
                {shortDate(dateTimeFields(item.createdAt).day)}
              </time>
              <div
                className="inbox-actions"
                role="group"
                aria-label={`Organize ${item.text}`}
              >
                {(["task", "plan", "event"] as const).map((kind) => (
                  <button
                    key={kind}
                    onClick={() => onConvert(item, kind)}
                    aria-label={`Make “${item.text}” ${kind === "event" ? "an event" : `a ${kind}`}`}
                  >
                    {kind === "task"
                      ? "Task"
                      : kind === "plan"
                        ? "Plan"
                        : "Event"}
                  </button>
                ))}
              </div>
              <button
                className="task-delete"
                onClick={() => void remove(item)}
                aria-label={`Delete ${item.text}`}
              >
                <Glyph>✕</Glyph>
              </button>
            </li>
          ))}
        </ul>
      )}
      {editing && (
        <InboxItemEditor
          item={editing}
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

function InboxItemEditor({
  item,
  onClose,
  onSaved,
  onError,
}: {
  item: InboxItem;
  onClose: () => void;
  onSaved: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [text, setText] = useState(item.text);
  const [notes, setNotes] = useState(item.notes);
  const [saving, setSaving] = useState(false);

  async function submit(form: FormEvent) {
    form.preventDefault();
    setSaving(true);
    try {
      await api.updateInboxItem({
        id: item.id,
        revision: item.revision,
        text,
        notes,
      });
      await onSaved();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <EditorShell
      label="Edit inbox item"
      kicker="INBOX"
      heading="Edit the note to self"
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !text.trim()}
          >
            {saving && <Spinner size={7} />}
            Save
          </button>
        </>
      }
    >
      <label>
        Text
        <input
          autoFocus
          value={text}
          onChange={(input) => setText(input.target.value)}
          required
          maxLength={140}
        />
      </label>
      <label>
        Notes
        <textarea
          value={notes}
          onChange={(input) => setNotes(input.target.value)}
          maxLength={800}
          placeholder="Anything that will help when you decide what this is"
        />
      </label>
    </EditorShell>
  );
}

/** A small dialog for capturing several thoughts in a row without leaving the current screen. */
export function QuickCapture({
  onClose,
  onCaptured,
  onError,
}: {
  onClose: () => void;
  onCaptured: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [text, setText] = useState("");
  const [notes, setNotes] = useState("");
  const [saving, setSaving] = useState(false);
  const [captured, setCaptured] = useState<string | null>(null);
  const textRef = useRef<HTMLInputElement>(null);

  async function submit(form: FormEvent) {
    form.preventDefault();
    const trimmed = text.trim();
    if (!trimmed) return;
    setSaving(true);
    try {
      await api.createInboxItem({ text: trimmed, notes });
      setCaptured(trimmed);
      setText("");
      setNotes("");
      await onCaptured();
      textRef.current?.focus();
    } catch (cause) {
      onError(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <EditorShell
      label="Quick capture"
      kicker="QUICK CAPTURE"
      heading="What's on your mind?"
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          <p className="capture-status" role="status">
            {captured
              ? `Captured “${captured}”.`
              : "Enter captures it; Escape closes."}
          </p>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Done
          </button>
          <button
            className="primary-button small editor-save"
            disabled={saving || !text.trim()}
          >
            {saving && <Spinner size={7} />}
            Capture
          </button>
        </>
      }
    >
      <label>
        Thought, task, or idea
        <input
          ref={textRef}
          autoFocus
          value={text}
          onChange={(input) => setText(input.target.value)}
          maxLength={140}
          placeholder="Look into the authentication bug"
        />
      </label>
      <label>
        Notes <span>Optional</span>
        <textarea
          value={notes}
          onChange={(input) => setNotes(input.target.value)}
          maxLength={800}
        />
      </label>
    </EditorShell>
  );
}

import { KeyboardEvent, useEffect, useState } from "react";
import { api, messageFor } from "./api";
import {
  localeWeekStart,
  saveWeekStart,
  storedWeekStart,
  WeekStart,
} from "./date";
import {
  commandShortcutText,
  recordedShortcut,
  shortcutPlatform,
  shortcutText,
} from "./shortcuts";

const weekdayNames = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
];

/** Which day weeks start on. Views read it once at startup, so a change reloads the window. */
export function WeekStartSetting({
  onMessage,
}: {
  onMessage: (message: string) => void;
}) {
  const saved = storedWeekStart();
  return (
    <label className="settings-field">
      Weeks start on
      <select
        value={saved === null ? "" : String(saved)}
        onChange={(input) => {
          const value = input.target.value;
          if (
            !saveWeekStart(value === "" ? null : (Number(value) as WeekStart))
          )
            return onMessage("Delve Planner couldn't save that preference.");
          window.location.reload();
        }}
      >
        <option value="">
          Follow the system ({weekdayNames[localeWeekStart()]})
        </option>
        {[1, 2, 3, 4, 5, 6, 0].map((day) => (
          <option key={day} value={day}>
            {weekdayNames[day]}
          </option>
        ))}
      </select>
      <span>
        Used by Week, Plan the Week, and the week a task is chosen for. Changing
        it reloads the window.
      </span>
    </label>
  );
}

/**
 * The system-wide quick-capture shortcut: off until recorded here. Rust checks and registers it,
 * so a combination another app holds is refused with the old one still working.
 */
export function QuickCaptureSetting({
  onMessage,
}: {
  onMessage: (message: string) => void;
}) {
  // Undefined until the saved shortcut has loaded.
  const [shortcut, setShortcut] = useState<string | null>();
  const [recording, setRecording] = useState(false);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    api
      .getQuickCaptureShortcut()
      .then((saved) => {
        if (active) setShortcut(saved);
      })
      .catch((cause) => {
        if (active) setShortcut(null);
        onMessage(messageFor(cause));
      });
    return () => {
      active = false;
    };
    // Loaded once; `onMessage` only reports a failed load.
  }, []);

  async function save(next: string | null) {
    setSaving(true);
    setProblem(null);
    try {
      setShortcut(await api.setQuickCaptureShortcut(next));
    } catch (cause) {
      setProblem(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  function record(event: KeyboardEvent<HTMLButtonElement>) {
    const bare =
      !event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey;
    // Tab still moves on, leaving the shortcut as it was.
    if (bare && event.key === "Tab") return setRecording(false);
    // Keys pressed here set the shortcut; nothing else in the window should act on them.
    event.preventDefault();
    event.stopPropagation();
    if (bare && event.key === "Escape") return setRecording(false);
    const combination = recordedShortcut(event);
    if (!combination) return;
    setRecording(false);
    void save(combination);
  }

  const example = shortcutText(
    shortcutPlatform === "mac" ? "shift+super+Space" : "control+shift+Space",
  );
  return (
    <div className="settings-field shortcut-setting">
      <span className="settings-field-label">Quick capture from any app</span>
      {recording ? (
        <button
          type="button"
          className="shortcut-recorder"
          autoFocus
          onKeyDown={record}
          onBlur={() => setRecording(false)}
          aria-label="Press the new shortcut, or Escape to cancel"
        >
          Press the keys… <small>Esc cancels</small>
        </button>
      ) : (
        <div className="settings-actions">
          <kbd className={shortcut ? "" : "off"}>
            {shortcut === undefined
              ? "…"
              : shortcut
                ? shortcutText(shortcut)
                : "Off"}
          </kbd>
          <button
            disabled={saving || shortcut === undefined}
            onClick={() => {
              setProblem(null);
              setRecording(true);
            }}
          >
            {shortcut ? "Change" : "Record a shortcut"}
          </button>
          {shortcut && (
            <button disabled={saving} onClick={() => void save(null)}>
              Turn off
            </button>
          )}
        </div>
      )}
      {problem && (
        <p className="settings-problem" role="alert">
          {problem}
        </p>
      )}
      <p>
        Opens the capture box even while Delve Planner is in the background or
        closed to the tray. Use two modifier keys, such as {example}, so it
        doesn't take a shortcut other apps rely on.
      </p>
    </div>
  );
}

const shortcuts: [keys: string, action: string][] = [
  [
    `${commandShortcutText("1")}–${commandShortcutText("7")}`,
    "Go to Today, Week, Plans, People, Inbox, Calendars, or What it knows",
  ],
  [commandShortcutText("N"), "New task for the day, week, or plan in view"],
  [commandShortcutText("N", { shift: true }), "New plan"],
  [commandShortcutText("I"), "Capture to the inbox"],
  [
    `${commandShortcutText("←")} ${commandShortcutText("→")}`,
    "Previous or next day, or week in Week",
  ],
  [commandShortcutText("↩"), "Save the open editor, or send a planner request"],
  [commandShortcutText(","), "Settings"],
  ["Esc", "Close a dialog or menu"],
];

/** The in-app shortcuts, so they can be found without hovering over every button. */
export function KeyboardShortcutList() {
  return (
    <dl className="shortcut-list">
      {shortcuts.map(([keys, action]) => (
        <div key={action}>
          <dt>
            <kbd>{keys}</kbd>
          </dt>
          <dd>{action}</dd>
        </div>
      ))}
    </dl>
  );
}

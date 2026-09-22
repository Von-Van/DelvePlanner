import {
  CSSProperties,
  FormEvent,
  RefObject,
  useCallback,
  useEffect,
  useState,
} from "react";
import {
  api,
  Calendar,
  CalendarAccount,
  CalendarProvider,
  messageFor,
  PlanColor,
  planColors,
  RemoteCalendar,
  WorkingHours,
} from "./api";
import {
  calendarColor,
  calendarSourceLabel,
  minuteLabel,
  parseMinute,
  providerLabel,
  syncLabel,
  weekdayNames,
  workingHoursLabel,
} from "./calendars";
import { weekStartsOn } from "./date";
import { Glyph, Mark, Spinner } from "./Geometry";
import { Menu, MenuItem } from "./Menu";
import { ColorSwatches } from "./PlanControls";
import { EditorShell } from "./PlanEditor";
import { plural } from "./planning";
import { useHeadingFocus } from "./useHeadingFocus";

/**
 * Read-only calendars from Google, Outlook, and other apps, plus the working hours capacity
 * counts. Links are checked once when added and then kept in the system keychain.
 */
export function CalendarsView({
  version,
  headingRef,
  focusToken,
  onChanged,
  onMessage,
}: {
  /** Changes when a background refresh updated calendars, so the list reloads. */
  version: number;
  headingRef: RefObject<HTMLHeadingElement>;
  focusToken: number;
  /** Tells the rest of the app that calendar events or capacity may have changed. */
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [calendars, setCalendars] = useState<Calendar[] | null>(null);
  const [accounts, setAccounts] = useState<CalendarAccount[]>([]);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [connecting, setConnecting] = useState<CalendarProvider | null>(null);
  const [choosing, setChoosing] = useState<{
    account: CalendarAccount;
    calendars: RemoteCalendar[];
  } | null>(null);
  const [editing, setEditing] = useState<Calendar | null>(null);
  const [relinking, setRelinking] = useState<Calendar | null>(null);
  const [now, setNow] = useState(() => new Date());
  useHeadingFocus(headingRef, focusToken);

  const reload = useCallback(async () => {
    try {
      const [nextCalendars, nextAccounts] = await Promise.all([
        api.listCalendars(),
        api.listCalendarAccounts(),
      ]);
      setCalendars(nextCalendars);
      setAccounts(nextAccounts);
      setNow(new Date());
    } catch (cause) {
      onMessage(messageFor(cause));
    }
  }, [onMessage]);

  useEffect(() => {
    void reload();
  }, [reload, version]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(timer);
  }, []);

  async function act(id: string | null, action: () => Promise<unknown>) {
    setBusyId(id ?? "new");
    try {
      await action();
      await reload();
      await onChanged();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusyId(null);
    }
  }

  // Signing in again to an account DayPlan already has refreshes it instead of duplicating it.
  async function connect(provider: CalendarProvider) {
    setConnecting(provider);
    try {
      const connected = await api.connectCalendarAccount(provider);
      await reload();
      await onChanged();
      setChoosing(connected);
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setConnecting(null);
    }
  }

  async function chooseCalendars(account: CalendarAccount) {
    setBusyId(account.id);
    try {
      setChoosing({
        account,
        calendars: await api.accountCalendars(account.id),
      });
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusyId(null);
    }
  }

  function accountMenu(account: CalendarAccount): MenuItem[] {
    return [
      {
        label: "Add calendars…",
        onSelect: () => void chooseCalendars(account),
      },
      {
        label: "Sign in again",
        onSelect: () => void connect(account.provider),
      },
      { kind: "separator" },
      {
        label: "Disconnect account",
        danger: true,
        onSelect: () => {
          if (
            !window.confirm(
              `Disconnect ${account.label}? Its ${plural(account.calendarCount, "calendar")} and their events leave DayPlan, and the sign-in is withdrawn. Your calendars aren't changed.`,
            )
          )
            return;
          void act(account.id, () => api.disconnectCalendarAccount(account));
        },
      },
    ];
  }

  const nextColor: PlanColor =
    planColors.find(
      (color) => !calendars?.some((calendar) => calendar.color === color),
    ) ?? planColors[(calendars?.length ?? 0) % planColors.length];

  const accountFor = (calendar: Calendar) =>
    accounts.find((account) => account.id === calendar.accountId);

  // Who to sign in with again. The calendar's own kind answers it even if its account row has
  // gone missing, so an account calendar is never offered a file or a link instead.
  const providerFor = (calendar: Calendar): CalendarProvider | undefined =>
    accountFor(calendar)?.provider ??
    (calendar.kind === "google" || calendar.kind === "microsoft"
      ? calendar.kind
      : undefined);

  function menuItems(calendar: Calendar): MenuItem[] {
    const account = accountFor(calendar);
    const items: MenuItem[] = [
      calendar.kind === "ics_file"
        ? {
            label: "Replace with a newer file…",
            onSelect: () =>
              void act(calendar.id, () => api.replaceCalendarFile(calendar)),
          }
        : {
            label: "Refresh now",
            onSelect: () =>
              void act(calendar.id, () => api.refreshCalendar(calendar.id)),
          },
      {
        label: calendar.visible ? "Hide from Today and Week" : "Show again",
        onSelect: () =>
          void act(calendar.id, () =>
            api.updateCalendar(calendar, { visible: !calendar.visible }),
          ),
      },
      { label: "Rename or recolor…", onSelect: () => setEditing(calendar) },
    ];
    if (calendar.kind === "ics_link")
      items.push({
        label: "Paste a new link…",
        onSelect: () => setRelinking(calendar),
      });
    const provider = providerFor(calendar);
    if (provider)
      items.push({
        label: "Sign in again",
        onSelect: () => void connect(provider),
      });
    items.push({ kind: "separator" });
    items.push({
      label: "Remove calendar",
      danger: true,
      onSelect: () => {
        if (
          !window.confirm(
            `Remove “${calendar.name}”? Its events leave DayPlan${
              calendar.kind === "ics_link"
                ? " and its link is deleted from your keychain"
                : account
                  ? `, and ${account.label} stays connected`
                  : ""
            }. The calendar itself isn't changed.`,
          )
        )
          return;
        void act(calendar.id, () => api.removeCalendar(calendar));
      },
    });
    return items;
  }

  return (
    <div className="calendars-page">
      <header className="topbar">
        <div className="date-heading">
          <p>CONNECTED CALENDARS</p>
          <h1 ref={headingRef} tabIndex={-1}>
            Calendars
          </h1>
        </div>
      </header>
      <p className="page-intro">
        See your Google, Outlook, and other calendars beside your plans. They
        stay read-only: DayPlan never changes them, and their events aren't
        exported or backed up with your planner data.
      </p>
      <div className="calendars-grid">
        <section aria-labelledby="calendar-list">
          <div className="section-header">
            <div>
              <p>READ-ONLY</p>
              <h2 id="calendar-list">Your calendars</h2>
            </div>
            {calendars && calendars.length > 0 && (
              <span>{plural(calendars.length, "calendar")}</span>
            )}
          </div>
          {accounts.length > 0 && (
            <ul className="account-list">
              {accounts.map((account) => (
                <li
                  key={account.id}
                  className={`account-row ${account.problem ? "problem" : ""}`}
                >
                  <div className="account-main">
                    <strong>{account.label}</strong>
                    <small>
                      {providerLabel(account.provider)} ·{" "}
                      {plural(account.calendarCount, "calendar")} · read-only
                    </small>
                    {account.problem && (
                      <p className="calendar-problem blocking" role="status">
                        <Mark size={6} filled color="var(--danger-dot)" />
                        <span>{account.problem.message}</span>
                        <button
                          className="text-button"
                          onClick={() => void connect(account.provider)}
                        >
                          Sign in again
                        </button>
                      </p>
                    )}
                  </div>
                  <Menu
                    label={`Options for ${account.label}`}
                    trigger={<Glyph>⋯</Glyph>}
                    items={accountMenu(account)}
                    className="task-action"
                    disabled={busyId !== null || connecting !== null}
                  />
                </li>
              ))}
            </ul>
          )}
          {calendars === null ? (
            <div className="loading-line">
              <Spinner size={10} />
              Reading calendars
            </div>
          ) : calendars.length === 0 ? (
            <div className="empty-agenda">
              <i className="empty-mark" aria-hidden="true" />
              <p>No calendars yet.</p>
              <small className="empty-note">
                Subscribe with a link to keep one up to date, or import a file.
              </small>
            </div>
          ) : (
            <ul className="calendar-list">
              {calendars.map((calendar) => (
                <li
                  key={calendar.id}
                  className={`calendar-card ${calendar.visible ? "" : "hidden"} ${calendar.problem ? "problem" : ""}`}
                  style={
                    {
                      "--calendar-color": calendarColor(calendar),
                    } as CSSProperties
                  }
                >
                  <span className="calendar-swatch" aria-hidden="true" />
                  <div className="calendar-main">
                    <strong>{calendar.name}</strong>
                    <small>
                      {calendarSourceLabel(calendar)} ·{" "}
                      {plural(calendar.eventCount, "event")}
                      {calendar.visible ? "" : " · Hidden"}
                    </small>
                    <small className="calendar-sync">
                      {busyId === calendar.id ? (
                        <>
                          <Spinner size={7} /> Working
                        </>
                      ) : (
                        syncLabel(calendar, now)
                      )}
                    </small>
                    {calendar.problem && (
                      <p
                        className={`calendar-problem ${calendar.problem.retryable ? "" : "blocking"}`}
                        role="status"
                      >
                        <Mark
                          size={6}
                          filled
                          color={
                            calendar.problem.retryable
                              ? "var(--warning-dot)"
                              : "var(--danger-dot)"
                          }
                        />
                        <span>{calendar.problem.message}</span>
                        {!calendar.problem.retryable &&
                          (providerFor(calendar) ? (
                            <button
                              className="text-button"
                              onClick={() =>
                                void connect(
                                  providerFor(calendar) as CalendarProvider,
                                )
                              }
                            >
                              Sign in again
                            </button>
                          ) : calendar.kind === "ics_link" ? (
                            <button
                              className="text-button"
                              onClick={() => setRelinking(calendar)}
                            >
                              Paste a new link
                            </button>
                          ) : (
                            <button
                              className="text-button"
                              onClick={() =>
                                void act(calendar.id, () =>
                                  api.replaceCalendarFile(calendar),
                                )
                              }
                            >
                              Choose a newer file
                            </button>
                          ))}
                      </p>
                    )}
                  </div>
                  <Menu
                    label={`Options for ${calendar.name}`}
                    trigger={<Glyph>⋯</Glyph>}
                    items={menuItems(calendar)}
                    className="task-action"
                    disabled={busyId !== null}
                  />
                </li>
              ))}
            </ul>
          )}
        </section>
        <aside className="calendars-side">
          <section className="side-card connect-card">
            <p className="side-kicker">CONNECT AN ACCOUNT</p>
            <h3>Google or Outlook</h3>
            <p>
              Sign in once in your browser, then choose which calendars DayPlan
              shows. DayPlan asks for read-only access: it can see when you're
              busy and never changes, creates, or deletes anything.
            </p>
            <div className="connect-buttons">
              {(["google", "microsoft"] as const).map((provider) => (
                <button
                  key={provider}
                  className="secondary-button"
                  disabled={connecting !== null || busyId !== null}
                  onClick={() => void connect(provider)}
                >
                  {connecting === provider ? (
                    <>
                      <Spinner size={7} /> Waiting for your browser
                    </>
                  ) : (
                    <>
                      <Mark size={7} /> Connect {providerLabel(provider)}
                    </>
                  )}
                </button>
              ))}
            </div>
            {connecting && (
              <p className="editor-hint">
                Finish signing in to {providerLabel(connecting)} in your
                browser. DayPlan waits five minutes.
              </p>
            )}
          </section>
          <SubscribeCard
            color={nextColor}
            busy={busyId === "new"}
            onSubscribe={(input) =>
              act(null, async () => {
                const calendar = await api.subscribeCalendar(input);
                onMessage(
                  `Added “${calendar.name}” with ${plural(calendar.eventCount, "event")}.`,
                );
              })
            }
          />
          <section className="side-card">
            <p className="side-kicker">IMPORT A FILE</p>
            <h3>An .ics snapshot</h3>
            <p>
              Import a calendar exported from Google Calendar, Outlook, or
              another app. It won't update on its own; choose a newer file later
              to replace it.
            </p>
            <button
              className="secondary-button"
              disabled={busyId !== null}
              onClick={() =>
                void act(null, async () => {
                  const calendar = await api.importCalendarFile(nextColor);
                  if (calendar)
                    onMessage(
                      `Imported “${calendar.name}” with ${plural(calendar.eventCount, "event")}.`,
                    );
                })
              }
            >
              <Glyph>+</Glyph> Import .ics file
            </button>
          </section>
          <WorkingHoursCard onChanged={onChanged} onMessage={onMessage} />
        </aside>
      </div>
      {choosing && (
        <CalendarPicker
          account={choosing.account}
          calendars={choosing.calendars}
          onClose={() => setChoosing(null)}
          onAdd={async (remoteIds) => {
            await act(choosing.account.id, async () => {
              const added = await api.addAccountCalendars(
                choosing.account.id,
                remoteIds,
              );
              onMessage(
                added.length === 1
                  ? `Added “${added[0].name}”.`
                  : `Added ${plural(added.length, "calendar")}.`,
              );
            });
            setChoosing(null);
          }}
        />
      )}
      {editing && (
        <CalendarEditor
          calendar={editing}
          onClose={() => setEditing(null)}
          onSave={async (changes) => {
            await act(editing.id, () => api.updateCalendar(editing, changes));
            setEditing(null);
          }}
        />
      )}
      {relinking && (
        <LinkEditor
          calendar={relinking}
          onClose={() => setRelinking(null)}
          onSave={async (link) => {
            setBusyId(relinking.id);
            try {
              await api.replaceCalendarLink(relinking, link);
              setRelinking(null);
              await reload();
              await onChanged();
            } catch (cause) {
              onMessage(messageFor(cause));
            } finally {
              setBusyId(null);
            }
          }}
        />
      )}
    </div>
  );
}

/** Chooses which of an account's calendars DayPlan shows. */
function CalendarPicker({
  account,
  calendars,
  onClose,
  onAdd,
}: {
  account: CalendarAccount;
  calendars: RemoteCalendar[];
  onClose: () => void;
  onAdd: (remoteIds: string[]) => Promise<void>;
}) {
  const [chosen, setChosen] = useState<string[]>(() =>
    calendars
      .filter((calendar) => calendar.primary && !calendar.alreadyAdded)
      .map((calendar) => calendar.id),
  );
  const [saving, setSaving] = useState(false);
  const available = calendars.filter((calendar) => !calendar.alreadyAdded);
  return (
    <EditorShell
      label="Choose calendars"
      kicker={`${providerLabel(account.provider).toUpperCase()} · ${account.label}`}
      heading="Which calendars should DayPlan show?"
      busy={saving}
      onClose={onClose}
      onSubmit={(form) => {
        form.preventDefault();
        setSaving(true);
        void onAdd(chosen).finally(() => setSaving(false));
      }}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            {available.length === 0 ? "Close" : "Not now"}
          </button>
          <button
            className="primary-button small"
            disabled={saving || chosen.length === 0}
          >
            {saving && <Spinner size={7} />}
            Add{" "}
            {chosen.length > 0
              ? plural(chosen.length, "calendar")
              : "calendars"}
          </button>
        </>
      }
    >
      {calendars.length === 0 ? (
        <p className="empty-tasks">This account has no calendars to show.</p>
      ) : (
        <ul className="calendar-choices">
          {calendars.map((calendar) => (
            <li key={calendar.id}>
              <label>
                <input
                  type="checkbox"
                  checked={
                    calendar.alreadyAdded || chosen.includes(calendar.id)
                  }
                  disabled={calendar.alreadyAdded}
                  onChange={(input) =>
                    setChosen((current) =>
                      input.target.checked
                        ? [...current, calendar.id]
                        : current.filter((id) => id !== calendar.id),
                    )
                  }
                />
                <span>{calendar.name}</span>
                {calendar.primary && <small>Main calendar</small>}
                {calendar.alreadyAdded && <small>Already shown</small>}
              </label>
            </li>
          ))}
        </ul>
      )}
      <p className="editor-hint">
        DayPlan only reads these calendars. You can hide or remove any of them
        later.
      </p>
    </EditorShell>
  );
}

function SubscribeCard({
  color,
  busy,
  onSubscribe,
}: {
  color: PlanColor;
  busy: boolean;
  onSubscribe: (input: {
    name: string;
    link: string;
    color: PlanColor;
  }) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [link, setLink] = useState("");
  const [chosenColor, setChosenColor] = useState<PlanColor | null>(null);

  async function submit(form: FormEvent) {
    form.preventDefault();
    await onSubscribe({ name, link, color: chosenColor ?? color });
    setName("");
    setLink("");
    setChosenColor(null);
  }

  return (
    <form className="side-card subscribe-card" onSubmit={submit}>
      <p className="side-kicker">SUBSCRIBE BY LINK</p>
      <h3>Keep a calendar up to date</h3>
      <label>
        Calendar link
        <input
          type="url"
          value={link}
          onChange={(input) => setLink(input.target.value)}
          placeholder="https://calendar.google.com/…/basic.ics"
          autoComplete="off"
          spellCheck={false}
          required
          maxLength={1200}
        />
      </label>
      <label>
        Name <span>Optional</span>
        <input
          value={name}
          onChange={(input) => setName(input.target.value)}
          placeholder="Uses the calendar's own name"
          maxLength={80}
        />
      </label>
      <div className="editor-field">
        <span>Color</span>
        <ColorSwatches
          value={chosenColor ?? color}
          onChange={(next) => setChosenColor(next ?? color)}
        />
      </div>
      <button className="primary-button small" disabled={busy || !link.trim()}>
        {busy ? <Spinner size={7} /> : <Mark filled />}
        Subscribe
      </button>
      <details className="link-help">
        <summary>Where do I find the link?</summary>
        <dl>
          <dt>Google Calendar</dt>
          <dd>
            On the web, open Settings and choose the calendar under Settings for
            my calendars. In Integrate calendar, copy Secret address in iCal
            format.
          </dd>
          <dt>Outlook</dt>
          <dd>
            In Outlook on the web or Outlook.com, open Settings, then Calendar,
            then Shared calendars. Under Publish a calendar, choose the calendar
            and how much to share, select Publish, and copy the ICS link.
          </dd>
          <dt>Other apps</dt>
          <dd>Look for a public or private iCal (.ics) or webcal link.</dd>
        </dl>
        <p>
          Anyone with a private link can read that calendar, so DayPlan keeps it
          in your system keychain and never shows it again. It checks the
          calendar every 30 minutes while DayPlan is running.
        </p>
      </details>
    </form>
  );
}

function WorkingHoursCard({
  onChanged,
  onMessage,
}: {
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [hours, setHours] = useState<WorkingHours | null>(null);
  const [days, setDays] = useState<number[]>([]);
  const [start, setStart] = useState("09:00");
  const [end, setEnd] = useState("17:00");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api
      .workingHours()
      .then((loaded) => {
        setHours(loaded);
        setDays(loaded.days);
        setStart(minuteLabel(loaded.startMinute));
        setEnd(minuteLabel(loaded.endMinute % (24 * 60)));
      })
      .catch((cause) => onMessage(messageFor(cause)));
  }, [onMessage]);

  const startMinute = parseMinute(start);
  const endMinute = parseMinute(end, true);
  const valid =
    days.length > 0 &&
    startMinute !== null &&
    endMinute !== null &&
    endMinute > startMinute;
  const changed =
    hours !== null &&
    (hours.startMinute !== startMinute ||
      hours.endMinute !== endMinute ||
      [...hours.days].sort().join() !== [...days].sort().join());
  const order = Array.from(
    { length: 7 },
    (_, index) => ((weekStartsOn + 6 + index) % 7) + 1,
  );

  async function submit(form: FormEvent) {
    form.preventDefault();
    if (!hours || !valid) return;
    setSaving(true);
    try {
      const saved = await api.updateWorkingHours(hours.revision, {
        days: [...days].sort((left, right) => left - right),
        startMinute: startMinute as number,
        endMinute: endMinute as number,
      });
      setHours(saved);
      await onChanged();
      onMessage(`Working hours saved: ${workingHoursLabel(saved)}.`);
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setSaving(false);
    }
  }

  return (
    <form className="side-card hours-card" onSubmit={submit}>
      <p className="side-kicker">WORKING HOURS</p>
      <h3>{hours ? workingHoursLabel(hours) : "Loading"}</h3>
      <p>
        Capacity compares planned work with the time left in these hours after
        events and busy calendar time.
      </p>
      <div className="weekday-toggles" role="group" aria-label="Working days">
        {order.map((day) => {
          const on = days.includes(day);
          return (
            <button
              type="button"
              key={day}
              aria-pressed={on}
              className={on ? "selected" : ""}
              onClick={() =>
                setDays(
                  on ? days.filter((other) => other !== day) : [...days, day],
                )
              }
            >
              {weekdayNames[day - 1]}
            </button>
          );
        })}
      </div>
      <div className="form-pair">
        <label>
          From
          <input
            type="time"
            value={start}
            required
            onChange={(input) => setStart(input.target.value)}
          />
        </label>
        <label>
          Until
          <input
            type="time"
            value={end}
            required
            onChange={(input) => setEnd(input.target.value)}
          />
        </label>
      </div>
      {!valid && hours && (
        <p className="editor-hint">
          Choose at least one day, and an end after the start.
        </p>
      )}
      <button
        className="secondary-button"
        disabled={!hours || !valid || !changed || saving}
      >
        {saving && <Spinner size={7} />}
        Save hours
      </button>
    </form>
  );
}

function CalendarEditor({
  calendar,
  onClose,
  onSave,
}: {
  calendar: Calendar;
  onClose: () => void;
  onSave: (changes: { name: string; color: PlanColor }) => Promise<void>;
}) {
  const [name, setName] = useState(calendar.name);
  const [color, setColor] = useState<PlanColor>(calendar.color);
  const [saving, setSaving] = useState(false);
  return (
    <EditorShell
      label="Edit calendar"
      kicker="CALENDAR"
      heading="Rename or recolor"
      busy={saving}
      onClose={onClose}
      onSubmit={(form) => {
        form.preventDefault();
        setSaving(true);
        void onSave({ name, color }).finally(() => setSaving(false));
      }}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small"
            disabled={saving || !name.trim()}
          >
            {saving && <Spinner size={7} />}
            Save
          </button>
        </>
      }
    >
      <label>
        Name
        <input
          autoFocus
          value={name}
          onChange={(input) => setName(input.target.value)}
          maxLength={80}
          required
        />
      </label>
      <div className="editor-field">
        <span>Color</span>
        <ColorSwatches
          value={color}
          onChange={(next) => setColor(next ?? calendar.color)}
        />
      </div>
    </EditorShell>
  );
}

function LinkEditor({
  calendar,
  onClose,
  onSave,
}: {
  calendar: Calendar;
  onClose: () => void;
  onSave: (link: string) => Promise<void>;
}) {
  const [link, setLink] = useState("");
  const [saving, setSaving] = useState(false);
  return (
    <EditorShell
      label="Replace calendar link"
      kicker="CALENDAR LINK"
      heading="Paste the new link"
      busy={saving}
      onClose={onClose}
      onSubmit={(form) => {
        form.preventDefault();
        setSaving(true);
        void onSave(link).finally(() => setSaving(false));
      }}
      footer={
        <>
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small"
            disabled={saving || !link.trim()}
          >
            {saving && <Spinner size={7} />}
            Check and save
          </button>
        </>
      }
    >
      <p className="dialog-subject">{calendar.name}</p>
      <label>
        New link
        <input
          autoFocus
          type="url"
          value={link}
          onChange={(input) => setLink(input.target.value)}
          placeholder="https:// or webcal://"
          autoComplete="off"
          spellCheck={false}
          required
          maxLength={1200}
        />
        <span>
          DayPlan checks the link first. The old link is replaced in your
          keychain only if the new one works.
        </span>
      </label>
    </EditorShell>
  );
}

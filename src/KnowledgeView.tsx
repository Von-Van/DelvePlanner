import { useCallback, useEffect, useState } from "react";
import {
  api,
  DayPreference,
  dayPreferences,
  messageFor,
  Observation,
  ObservationKind,
  PlanningProfile,
} from "./api";
import { minuteLabel, parseMinute, weekdayNames } from "./calendars";
import { Glyph, Mark } from "./Geometry";
import { estimateLabel } from "./planning";
import { useHeadingFocus } from "./useHeadingFocus";

const energyLabels: Record<DayPreference, string> = {
  morning: "Mornings",
  afternoon: "Afternoons",
  evening: "Evenings",
};

const observationTitles: Record<ObservationKind, string> = {
  estimate_accuracy: "How long work really takes",
  usual_start: "When you start",
  typical_daily_load: "What a day holds",
  deferred_days: "The day work slips",
  repeatedly_moved: "Work that keeps moving",
  slipping_plan: "The plan that slips",
};

/** Minutes for a select, so a length is picked rather than typed. */
const lengths = [15, 25, 30, 45, 60, 90, 120, 180, 240, 300, 360, 480];
const dayLimits = [120, 180, 240, 300, 360, 420, 480];
const breaks = [5, 10, 15, 20, 30, 45, 60];

/**
 * The options a length select offers, always including whatever is stored. Without this a value
 * Delve Planner didn't offer — from an earlier version, or set elsewhere — would show as "no
 * preference" and be thrown away by the next change to any other field.
 */
function withCurrent(options: number[], current: number | null) {
  if (current === null || options.includes(current)) return options;
  return [...options, current].sort((left, right) => left - right);
}

/**
 * Everything Delve Planner knows about how this person plans: what they told it, and what it worked
 * out from local records. Each observation shows what it rests on, and everything here can be
 * changed, switched off, or forgotten.
 */
export function KnowledgeView({
  headingRef,
  focusToken,
  onChanged,
  onMessage,
}: {
  headingRef: React.RefObject<HTMLHeadingElement>;
  focusToken: number;
  /** Capacity uses the profile, so the rest of the app reloads when it changes. */
  onChanged: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [profile, setProfile] = useState<PlanningProfile | null>(null);
  const [observations, setObservations] = useState<Observation[]>([]);
  const [busy, setBusy] = useState(false);
  useHeadingFocus(headingRef, focusToken);

  const reload = useCallback(async () => {
    try {
      const [nextProfile, nextObservations] = await Promise.all([
        api.planningProfile(),
        api.observations(),
      ]);
      setProfile(nextProfile);
      setObservations(nextObservations);
    } catch (cause) {
      onMessage(messageFor(cause));
    }
  }, [onMessage]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function save(
    changes: Parameters<typeof api.updatePlanningProfile>[1],
  ) {
    if (!profile) return;
    setBusy(true);
    try {
      setProfile(await api.updatePlanningProfile(profile, changes));
      await reload();
      await onChanged();
    } catch (cause) {
      onMessage(messageFor(cause));
      await reload();
    } finally {
      setBusy(false);
    }
  }

  async function forget(what: "profile" | "history") {
    const question =
      what === "profile"
        ? "Forget everything you've told Delve Planner about how you plan? Your schedule isn't affected."
        : "Forget the history Delve Planner learns from? The observations drawn from it go too, and it starts collecting again from today.";
    if (!window.confirm(question)) return;
    setBusy(true);
    try {
      if (what === "profile") await api.clearPlanningProfile();
      else await api.clearPlanningHistory();
      await reload();
      await onChanged();
      onMessage(
        what === "profile" ? "Planning profile cleared." : "History cleared.",
      );
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusy(false);
    }
  }

  const toggleDay = (day: number) => {
    if (!profile) return;
    const off = profile.noWorkDays.includes(day)
      ? profile.noWorkDays.filter((value) => value !== day)
      : [...profile.noWorkDays, day].sort((left, right) => left - right);
    void save({ noWorkDays: off });
  };

  const mute = (kind: ObservationKind) => {
    if (!profile) return;
    void save({ mutedObservations: [...profile.mutedObservations, kind] });
  };

  const unmute = (kind: ObservationKind) => {
    if (!profile) return;
    void save({
      mutedObservations: profile.mutedObservations.filter(
        (value) => value !== kind,
      ),
    });
  };

  return (
    <div className="knowledge-page">
      <header className="topbar">
        <div className="date-heading">
          <p>ON THIS DEVICE</p>
          <h1 ref={headingRef} tabIndex={-1}>
            What Delve Planner knows
          </h1>
        </div>
      </header>
      <p className="page-intro">
        Everything here stays on this device and none of it is exported. What
        you tell Delve Planner shapes capacity and the planner&rsquo;s
        suggestions; what Delve Planner noticed is worked out from your own
        records, and says what it rests on.
      </p>

      <section className="knowledge-block">
        <h2>What you&rsquo;ve told it</h2>
        {profile === null ? (
          <p className="knowledge-empty">Loading…</p>
        ) : (
          <div className="profile-grid">
            <label>
              <span>Preferred hours</span>
              <div className="profile-range">
                <input
                  className="range"
                  type="time"
                  value={minuteLabel(profile.preferredStartMinute ?? 540)}
                  disabled={busy}
                  onChange={(event) =>
                    void save({
                      preferredStartMinute: parseMinute(event.target.value),
                    })
                  }
                />
                <span aria-hidden="true">–</span>
                <input
                  className="range"
                  type="time"
                  value={minuteLabel(profile.preferredEndMinute ?? 1020)}
                  disabled={busy}
                  onChange={(event) =>
                    void save({
                      preferredEndMinute: parseMinute(event.target.value, true),
                    })
                  }
                />
              </div>
              <small>
                When inside your working hours you&rsquo;d rather plan work.
              </small>
            </label>

            <label>
              <span>Most work in a day</span>
              <select
                value={profile.maxPlannedMinutes ?? ""}
                disabled={busy}
                onChange={(event) =>
                  void save({
                    maxPlannedMinutes: event.target.value
                      ? Number(event.target.value)
                      : null,
                  })
                }
              >
                <option value="">No limit</option>
                {withCurrent(dayLimits, profile.maxPlannedMinutes).map(
                  (minutes) => (
                    <option key={minutes} value={minutes}>
                      {estimateLabel(minutes)}
                    </option>
                  ),
                )}
              </select>
              <small>
                Days planned past this are flagged, never rearranged.
              </small>
            </label>

            <label>
              <span>Focus block</span>
              <select
                value={profile.focusMinutes ?? ""}
                disabled={busy}
                onChange={(event) =>
                  void save({
                    focusMinutes: event.target.value
                      ? Number(event.target.value)
                      : null,
                  })
                }
              >
                <option value="">No preference</option>
                {withCurrent(lengths, profile.focusMinutes).map((minutes) => (
                  <option key={minutes} value={minutes}>
                    {estimateLabel(minutes)}
                  </option>
                ))}
              </select>
            </label>

            <label>
              <span>Break after one</span>
              <select
                value={profile.breakMinutes ?? ""}
                disabled={busy}
                onChange={(event) =>
                  void save({
                    breakMinutes: event.target.value
                      ? Number(event.target.value)
                      : null,
                  })
                }
              >
                <option value="">No preference</option>
                {withCurrent(breaks, profile.breakMinutes).map((minutes) => (
                  <option key={minutes} value={minutes}>
                    {estimateLabel(minutes)}
                  </option>
                ))}
              </select>
            </label>

            <label>
              <span>Demanding work suits</span>
              <select
                value={profile.energy ?? ""}
                disabled={busy}
                onChange={(event) =>
                  void save({
                    energy: event.target.value
                      ? (event.target.value as DayPreference)
                      : null,
                  })
                }
              >
                <option value="">No preference</option>
                {dayPreferences.map((preference) => (
                  <option key={preference} value={preference}>
                    {energyLabels[preference]}
                  </option>
                ))}
              </select>
            </label>

            <div className="profile-days">
              <span>Days off</span>
              <div className="day-toggles">
                {weekdayNames.map((name, index) => {
                  const day = index + 1;
                  const off = profile.noWorkDays.includes(day);
                  return (
                    <button
                      key={name}
                      type="button"
                      className={off ? "day-toggle off" : "day-toggle"}
                      aria-pressed={off}
                      disabled={busy}
                      onClick={() => toggleDay(day)}
                    >
                      {name.slice(0, 3)}
                    </button>
                  );
                })}
              </div>
              <small>
                A day off holds no working time, whatever your hours say.
              </small>
            </div>
          </div>
        )}
        <div className="knowledge-actions">
          <button
            disabled={busy || !profile}
            onClick={() => void forget("profile")}
          >
            Forget all of this
          </button>
        </div>
      </section>

      <section className="knowledge-block">
        <h2>What it noticed</h2>
        {observations.length === 0 ? (
          <p className="knowledge-empty">
            Nothing yet. Delve Planner says something only once there is enough
            behind it, and it only ever looks at records already on this device.
          </p>
        ) : (
          <ul className="observation-list">
            {observations.map((observation) => (
              <li key={observation.kind}>
                <div className="observation-main">
                  <p className="observation-kind">
                    {observationTitles[observation.kind]}
                  </p>
                  <strong>{observation.summary}</strong>
                  <small>
                    <Mark size={5} filled color="var(--blue)" />
                    {observation.evidence}
                  </small>
                </div>
                <button
                  className="text-button"
                  disabled={busy}
                  onClick={() => mute(observation.kind)}
                >
                  Turn off
                </button>
              </li>
            ))}
          </ul>
        )}
        {profile !== null && profile.mutedObservations.length > 0 && (
          <div className="muted-observations">
            <p>Switched off</p>
            {profile.mutedObservations.map((kind) => (
              <button
                key={kind}
                className="text-button"
                disabled={busy}
                onClick={() => unmute(kind)}
              >
                <Glyph>↺</Glyph> {observationTitles[kind]}
              </button>
            ))}
          </div>
        )}
        <div className="knowledge-actions">
          <button disabled={busy} onClick={() => void forget("history")}>
            Forget the history behind these
          </button>
        </div>
      </section>
    </div>
  );
}

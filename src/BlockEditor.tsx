import { FormEvent, useEffect, useRef, useState } from "react";
import {
  api,
  DayCapacity,
  LocalDateTimeResolution,
  messageFor,
  Task,
  TaskBlock,
} from "./api";
import {
  fitsFreeTime,
  freeSlots,
  hoursLabel,
  timeRangeLabel,
} from "./calendars";
import {
  dateTimeFields,
  localDateTimeToUtc,
  localTimeZone,
  offsetDay,
  timeLabel,
  weekdayShort,
} from "./date";
import { Spinner } from "./Geometry";
import { EditorShell } from "./PlanEditor";
import { estimateLabel } from "./planning";

const lengthPresets = [15, 30, 45, 60, 90, 120, 180, 240];

/**
 * Reserves time for a task, or moves an existing block. The day's free time comes from capacity:
 * working hours without events, busy calendar events, or other blocks.
 */
export function BlockEditor({
  task,
  block,
  today,
  defaultDay,
  onClose,
  onSaved,
  onError,
}: {
  task: Task;
  block?: TaskBlock;
  today: string;
  defaultDay: string;
  onClose: () => void;
  onSaved: () => Promise<void> | void;
  onError: (message: string) => void;
}) {
  const original = block
    ? {
        day: dateTimeFields(block.startAtUtc).day,
        time: timeLabel(block.startAtUtc),
        minutes: block.durationMinutes,
      }
    : null;
  const [day, setDay] = useState(original?.day ?? defaultDay);
  const [time, setTime] = useState(original?.time ?? "");
  const [minutes, setMinutes] = useState(
    original?.minutes ?? Math.max(15, task.estimatedMinutes ?? 60),
  );
  const [capacity, setCapacity] = useState<DayCapacity | null>(null);
  const [resolution, setResolution] = useState<LocalDateTimeResolution | null>(
    null,
  );
  const [saving, setSaving] = useState(false);
  const blockId = block?.id ?? null;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    let current = true;
    setCapacity(null);
    api
      .capacity(day, 1, localTimeZone, blockId)
      .then((next) => {
        if (current) setCapacity(next.days[0] ?? null);
      })
      .catch((cause) => onErrorRef.current(messageFor(cause)));
    return () => {
      current = false;
    };
  }, [day, blockId]);

  const soonest = nextQuarterHour(Date.now());
  // Free time from the next quarter hour on, at least `length` minutes long.
  const freeFrom = (length: number) =>
    capacity
      ? freeSlots(capacity, 0)
          .map((slot) => ({
            ...slot,
            startAtUtc: new Date(
              Math.max(Date.parse(slot.startAtUtc), soonest),
            ).toISOString(),
          }))
          .filter(
            (slot) =>
              Date.parse(slot.endAtUtc) - Date.parse(slot.startAtUtc) >=
              Math.max(length, 15) * 60_000,
          )
      : [];
  const slots = freeFrom(minutes);
  const shorter = slots.length === 0 ? freeFrom(15) : [];

  // A new block starts in the first free time that fits until the user picks a time.
  useEffect(() => {
    if (block || time !== "" || !capacity) return;
    const first = slots[0] ?? shorter[0];
    if (first) setTime(timeLabel(first.startAtUtc));
    else if (day === today) setTime(timeLabel(new Date(soonest).toISOString()));
    else setTime("09:00");
  }, [block, time, capacity, slots, shorter, day, today, soonest]);

  // Capacity leaves out the block being moved, so this compares against everything else.
  const overlaps =
    capacity !== null &&
    time !== "" &&
    capacity.workingMinutes > 0 &&
    !fitsFreeTime(capacity, localDateTimeToUtc(day, time), minutes);

  async function persist(startAtUtc: string) {
    const input = {
      startAtUtc,
      timeZone: localTimeZone,
      durationMinutes: minutes,
    };
    if (block) await api.updateTaskBlock(block, input);
    else await api.createTaskBlock(task.id, input);
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

  function submit(form: FormEvent) {
    form.preventDefault();
    void run(async () => {
      const next = await api.resolveLocalDateTime(day, time, localTimeZone);
      setResolution(next);
      if (next.kind === "resolved") await persist(next.startAtUtc);
    });
  }

  const days = Array.from({ length: 7 }, (_, index) => offsetDay(today, index));
  const lengths = lengthPresets.includes(minutes)
    ? lengthPresets
    : [...lengthPresets, minutes].sort((left, right) => left - right);

  return (
    <EditorShell
      label={block ? "Move time block" : "Block time"}
      kicker="TIME BLOCK"
      heading={block ? "Move this block" : "Make time for it"}
      busy={saving}
      onClose={onClose}
      onSubmit={submit}
      footer={
        <>
          {block && (
            <button
              type="button"
              className="editor-delete"
              disabled={saving}
              onClick={() =>
                void run(async () => {
                  await api.deleteTaskBlocks([block]);
                  await onSaved();
                })
              }
            >
              Remove block
            </button>
          )}
          <button type="button" className="editor-cancel" onClick={onClose}>
            Cancel
          </button>
          <button
            className="primary-button small"
            disabled={saving || time === ""}
          >
            {saving && <Spinner size={7} />}
            {block ? "Save block" : "Block time"}
          </button>
        </>
      }
    >
      <p className="dialog-subject">{task.title}</p>
      <div
        className="day-choices"
        role="group"
        aria-label="The next seven days"
      >
        {days.map((candidate) => (
          <button
            type="button"
            key={candidate}
            className={`day-chip ${candidate === day ? "selected" : ""} ${candidate === today ? "today" : ""}`}
            aria-pressed={candidate === day}
            onClick={() => {
              setDay(candidate);
              setResolution(null);
              if (!block) setTime("");
            }}
          >
            <span>
              {candidate === today
                ? "TODAY"
                : weekdayShort(candidate).toUpperCase()}
            </span>
            <strong>{candidate.slice(-2)}</strong>
          </button>
        ))}
      </div>
      <div className="form-pair">
        <label>
          Date
          <input
            type="date"
            value={day}
            required
            onChange={(input) => {
              setDay(input.target.value);
              setResolution(null);
            }}
          />
        </label>
        <label>
          Start
          <input
            type="time"
            value={time}
            required
            onChange={(input) => {
              setTime(input.target.value);
              setResolution(null);
            }}
          />
        </label>
      </div>
      <label>
        Length
        <select
          value={minutes}
          onChange={(input) => setMinutes(Number(input.target.value))}
        >
          {lengths.map((length) => (
            <option key={length} value={length}>
              {estimateLabel(length)}
              {length === task.estimatedMinutes ? " (estimate)" : ""}
            </option>
          ))}
        </select>
      </label>
      <div className="free-time" aria-live="polite">
        {capacity === null ? (
          <p className="free-note">
            <Spinner size={7} /> Finding free time
          </p>
        ) : capacity.workingMinutes === 0 ? (
          <p className="free-note">
            Not a working day. Any time you choose still counts.
          </p>
        ) : slots.length === 0 && shorter.length === 0 ? (
          <p className="free-note">
            No free time left in working hours that day.
          </p>
        ) : (
          <>
            <p className="free-note">
              {slots.length > 0
                ? `Free in working hours · ${hoursLabel(capacity.availableMinutes)} available`
                : `Nothing free for ${estimateLabel(minutes)} · shorter free times`}
            </p>
            <div className="free-slots">
              {(slots.length > 0 ? slots : shorter).map((slot) => {
                const start = timeLabel(slot.startAtUtc);
                return (
                  <button
                    type="button"
                    key={slot.startAtUtc}
                    className={start === time ? "selected" : ""}
                    aria-pressed={start === time}
                    onClick={() => {
                      setTime(start);
                      setResolution(null);
                    }}
                  >
                    {timeRangeLabel(slot.startAtUtc, slot.endAtUtc)}
                  </button>
                );
              })}
            </div>
          </>
        )}
        {overlaps && (
          <p className="editor-hint">
            This overlaps an event, a busy calendar event, or another block.
          </p>
        )}
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
    </EditorShell>
  );
}

function nextQuarterHour(time: number) {
  const quarter = 15 * 60_000;
  return Math.ceil(time / quarter) * quarter;
}

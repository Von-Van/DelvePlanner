import { useEffect, useState } from "react";
import { HeadsUpSettings as Settings, headsUpApi } from "./headsUpApi";
import "./MemorySettings.css";

/**
 * Settings → Heads-ups: whether the assistant brings things up before it is asked, and whether an
 * urgent one may also send a system notification.
 */
export function HeadsUpSettings({
  onMessage,
}: {
  onMessage: (message: string) => void;
}) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    headsUpApi
      .settings()
      .then(setSettings)
      .catch((cause) =>
        onMessage(cause instanceof Error ? cause.message : String(cause)),
      );
  }, [onMessage]);

  async function save(next: Settings) {
    setBusy(true);
    try {
      setSettings(await headsUpApi.updateSettings(next));
    } catch (cause) {
      onMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  if (!settings) return null;
  return (
    <div className="heads-up-settings">
      <label className="memory-toggle">
        <input
          type="checkbox"
          checked={settings.enabled}
          disabled={busy}
          onChange={(input) =>
            void save({ ...settings, enabled: input.target.checked })
          }
        />
        <span>
          <strong>Bring things up before I ask</strong>
          <small>
            Deadlines with no time planned, clashes, free focus time, overbooked
            days, work that keeps slipping and quiet projects. At most three at
            a time, each with the facts behind it. Found by fixed rules on this
            computer, not by the model.
          </small>
        </span>
      </label>
      <label className="memory-toggle">
        <input
          type="checkbox"
          checked={settings.notify}
          disabled={busy || !settings.enabled}
          onChange={(input) =>
            void save({ ...settings, notify: input.target.checked })
          }
        />
        <span>
          <strong>Notify me about urgent ones</strong>
          <small>
            A deadline today or tomorrow, or a clash today. At most two a day,
            never outside working hours or on days off.
          </small>
        </span>
      </label>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, Update } from "@tauri-apps/plugin-updater";
import { api, DatabaseStatus, ImportSelection, OllamaStatus } from "./api";
import { Glyph } from "./Geometry";

type Props = {
  status: OllamaStatus | null;
  onClose: () => void;
  onDataChanged: () => Promise<void>;
  onRefreshStatus: () => Promise<void>;
  onMessage: (message: string) => void;
};

export function SettingsModal({
  status,
  onClose,
  onDataChanged,
  onRefreshStatus,
  onMessage,
}: Props) {
  const dialogRef = useRef<HTMLElement>(null);
  const busyRef = useRef<string | null>(null);
  const onCloseRef = useRef(onClose);
  const [database, setDatabase] = useState<DatabaseStatus | null>(null);
  const [version, setVersion] = useState("…");
  const [selection, setSelection] = useState<ImportSelection | null>(null);
  const [update, setUpdate] = useState<Update | null>(null);
  const [updateState, setUpdateState] = useState(
    "Updates are checked only when you ask.",
  );
  const [busy, setBusy] = useState<string | null>(null);
  busyRef.current = busy;
  onCloseRef.current = onClose;

  async function refreshDatabase() {
    setDatabase(await api.databaseStatus());
  }

  useEffect(() => {
    void refreshDatabase();
    void getVersion().then(setVersion);
    const previous = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busyRef.current) onCloseRef.current();
      if (event.key !== "Tab" || !dialogRef.current) return;
      const items = dialogRef.current.querySelectorAll<HTMLElement>(
        'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]',
      );
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      }
      if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      previous?.focus();
    };
  }, []);

  async function run(label: string, action: () => Promise<void>) {
    setBusy(label);
    try {
      await action();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      setBusy(null);
    }
  }

  async function chooseImport() {
    await run("import", async () => setSelection(await api.selectImport()));
  }

  async function applyImport() {
    if (
      !selection ||
      !window.confirm(
        `Replace local data with ${selection.preview.planCount} plans, ${selection.preview.workstreamCount} workstreams, ${selection.preview.milestoneCount} milestones, ${selection.preview.eventCount} events, ${selection.preview.taskCount} tasks, and ${selection.preview.personCount} people? A backup is created first.`,
      )
    )
      return;
    await run("import", async () => {
      await api.applySelectedImport(selection.token);
      setSelection(null);
      await refreshDatabase();
      await onDataChanged();
      onMessage(
        "Import complete. The previous dataset is available in backups.",
      );
    });
  }

  async function restore(name: string) {
    if (
      !window.confirm(
        "Restore this backup? DayPlan creates a backup of the current database first.",
      )
    )
      return;
    await run("restore", async () => {
      await api.restoreBackup(name);
      await refreshDatabase();
      await onDataChanged();
      onMessage("Backup restored.");
    });
  }

  async function checkForUpdates() {
    await run("update", async () => {
      const next = await check({ timeout: 15_000 });
      setUpdate(next);
      setUpdateState(
        next
          ? `DayPlan ${next.version} is available.`
          : "DayPlan is up to date.",
      );
    });
  }

  async function installUpdate() {
    if (
      !update ||
      !window.confirm(
        `Install DayPlan ${update.version}? The signed package will be downloaded, installed, and DayPlan will restart.`,
      )
    )
      return;
    await run("update", async () => {
      setUpdateState("Downloading and verifying the signed update…");
      await update.downloadAndInstall();
      await relaunch();
    });
  }

  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.currentTarget === event.target && !busy) onClose();
      }}
    >
      <section
        ref={dialogRef}
        tabIndex={-1}
        className="settings-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <header>
          <div>
            <p>DAYPLAN / LOCAL DESKTOP</p>
            <h2 id="settings-title">Settings & recovery</h2>
          </div>
          <button onClick={onClose} aria-label="Close settings">
            <Glyph>✕</Glyph>
          </button>
        </header>

        <div className="settings-grid">
          <SettingsBlock
            index="01"
            title="Local model"
            subtitle={status?.detail ?? "Checking Ollama…"}
          >
            <p>
              Model: <strong>{status?.modelName ?? "qwen3:8b"}</strong>
              {status?.modelDigest
                ? ` · ${status.modelDigest.slice(0, 16)}…`
                : ""}
            </p>
            <div className="settings-actions">
              <button onClick={() => void onRefreshStatus()}>
                Refresh diagnostics
              </button>
              {!status?.modelInstalled && (
                <button
                  disabled={busy === "model"}
                  onClick={() => {
                    if (
                      !window.confirm(
                        "Download qwen3:8b? It is about 5.2 GB and DayPlan recommends roughly 10 GB of free space.",
                      )
                    )
                      return;
                    void run("model", async () => {
                      const poll = window.setInterval(
                        () => void onRefreshStatus(),
                        750,
                      );
                      try {
                        await api.downloadModel();
                        await onRefreshStatus();
                      } finally {
                        window.clearInterval(poll);
                      }
                    });
                  }}
                >
                  Download model
                </button>
              )}
              {status?.phase === "downloading" && (
                <button onClick={() => void api.cancelModelDownload()}>
                  Cancel download
                </button>
              )}
              <button
                disabled={busy === "runtime"}
                onClick={() =>
                  void run("runtime", async () => {
                    await api.restartModelRuntime();
                    await onRefreshStatus();
                  })
                }
              >
                Restart runtime
              </button>
              {status?.modelInstalled && (
                <button
                  disabled={busy === "remove-model"}
                  onClick={() => {
                    if (
                      !window.confirm(
                        "Remove qwen3:8b and all DayPlan AI model data? Your schedule is not affected.",
                      )
                    )
                      return;
                    void run("remove-model", async () => {
                      await api.removeModel();
                      await onRefreshStatus();
                      onMessage("Local AI model data removed.");
                    });
                  }}
                >
                  Remove model
                </button>
              )}
            </div>
            <p>
              Runtime: {status?.ollamaVersion ?? "checking"}
              {status?.storageBytes != null
                ? ` · ${(status.storageBytes / 1_073_741_824).toFixed(1)} GB stored`
                : ""}
            </p>
            {status?.modelLicense && (
              <p>Model license: {status.modelLicense}</p>
            )}
          </SettingsBlock>

          <SettingsBlock
            index="02"
            title="Event reminders"
            subtitle="One optional reminder per timed event."
          >
            <p>
              Permission is requested only when a reminder is first enabled.
              Keep DayPlan running in the tray; choosing Quit stops reminder
              delivery.
            </p>
          </SettingsBlock>

          <SettingsBlock
            index="03"
            title="Data & recovery"
            subtitle={
              database?.ready
                ? `SQLite schema ${database.schemaVersion} is healthy.`
                : (database?.error?.message ?? "Checking local storage…")
            }
          >
            <div className="settings-actions">
              <button
                onClick={() =>
                  void run("export", async () => {
                    const result = await api.exportFile();
                    if (result.completed)
                      onMessage(`Exported ${result.fileName}.`);
                  })
                }
              >
                Export JSON
              </button>
              <button onClick={() => void chooseImport()}>
                Preview import
              </button>
            </div>
            {selection && (
              <div className="import-preview" role="status">
                <strong>Ready to replace local data</strong>
                <span>
                  {selection.preview.planCount} plans ·{" "}
                  {selection.preview.workstreamCount} workstreams ·{" "}
                  {selection.preview.milestoneCount} milestones ·{" "}
                  {selection.preview.personCount} people ·{" "}
                  {selection.preview.eventCount} events ·{" "}
                  {selection.preview.taskCount} tasks
                </span>
                <div>
                  <button
                    onClick={() =>
                      void api
                        .discardSelectedImport()
                        .then(() => setSelection(null))
                    }
                  >
                    Cancel
                  </button>
                  <button
                    className="danger-action"
                    onClick={() => void applyImport()}
                  >
                    Confirm import
                  </button>
                </div>
              </div>
            )}
            {!!database?.backups.length && (
              <div className="backup-list">
                <strong>Recovery backups</strong>
                {database.backups.slice(0, 5).map((backup) => (
                  <button
                    key={backup.name}
                    onClick={() => void restore(backup.name)}
                  >
                    <span>{new Date(backup.createdAt).toLocaleString()}</span>
                    <small>
                      {Math.ceil(backup.sizeBytes / 1024)} KB · Restore
                    </small>
                  </button>
                ))}
              </div>
            )}
          </SettingsBlock>

          <SettingsBlock
            index="04"
            title="Private diagnostics"
            subtitle="Export only when you choose to share troubleshooting data."
          >
            <p>
              The ZIP excludes commands, titles, notes, proposal contents, and
              database paths. It contains versions, health flags, and up to five
              redacted logs.
            </p>
            <div className="settings-actions">
              <button
                onClick={() =>
                  void run("diagnostics", async () => {
                    const result = await api.exportDiagnostics();
                    if (result.completed)
                      onMessage(`Exported ${result.fileName}.`);
                  })
                }
              >
                Export diagnostics
              </button>
            </div>
          </SettingsBlock>

          <SettingsBlock
            index="05"
            title={`DayPlan ${version}`}
            subtitle={updateState}
          >
            {update?.body && (
              <div className="release-notes">
                <strong>Release notes</strong>
                <p>{update.body}</p>
              </div>
            )}
            <div className="settings-actions">
              <button
                onClick={() => void checkForUpdates()}
                disabled={busy === "update"}
              >
                Check for updates
              </button>
              {update && (
                <button
                  className="primary-action"
                  onClick={() => void installUpdate()}
                >
                  Install {update.version}
                </button>
              )}
            </div>
          </SettingsBlock>
        </div>
        <div className="sr-live" aria-live="polite">
          {busy ? `${busy} in progress` : ""}
        </div>
      </section>
    </div>
  );
}

function SettingsBlock({
  index,
  title,
  subtitle,
  children,
}: {
  /** A two-digit mono index stands in for an icon. */
  index: string;
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section className="settings-block">
      <div className="settings-block-heading">
        <span className="settings-index" aria-hidden="true">
          {index}
        </span>
        <div>
          <h3>{title}</h3>
          <p>{subtitle}</p>
        </div>
      </div>
      <div className="settings-block-body">{children}</div>
    </section>
  );
}

function messageFor(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

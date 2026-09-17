import { useCallback, useEffect, useState } from "react";
import { api, InstalledModel, messageFor, OllamaStatus } from "./api";
import { Mark, Spinner } from "./Geometry";

/** The model DayPlan's own evaluations run against, and the one it offers to download. */
export const TESTED_MODEL = "qwen3:8b";

const DOWNLOAD_WARNING = `Download ${TESTED_MODEL} now? It is about 5.2 GB and DayPlan recommends roughly 10 GB of free space. It is stored with DayPlan, not in your own Ollama folder.`;

function sizeLabel(bytes: number) {
  return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
}

/**
 * Which local model plans. DayPlan lists what it downloaded itself alongside anything already
 * installed on the machine, and only downloads when there's nothing to use.
 */
export function ModelPicker({
  status,
  onRefreshStatus,
  onMessage,
}: {
  status: OllamaStatus | null;
  onRefreshStatus: () => Promise<void>;
  onMessage: (message: string) => void;
}) {
  const [models, setModels] = useState<InstalledModel[] | null>(null);
  const [checking, setChecking] = useState<string | null>(null);
  const [refused, setRefused] = useState<Record<string, string>>({});
  const [downloading, setDownloading] = useState(false);

  const reload = useCallback(async () => {
    try {
      setModels(await api.installedModels());
    } catch (cause) {
      onMessage(messageFor(cause));
    }
  }, [onMessage]);

  useEffect(() => {
    void reload();
  }, [reload, status?.modelName, status?.phase]);

  async function choose(model: InstalledModel) {
    if (model.selected || checking) return;
    try {
      await api.chooseModel(model.name);
      await reload();
      await onRefreshStatus();
    } catch (cause) {
      onMessage(messageFor(cause));
      return;
    }
    if (model.checked) return;
    // A model DayPlan hasn't been evaluated against is asked one question it can't get wrong,
    // so a model that can't hold the reply format says so now rather than mid-plan.
    setChecking(model.name);
    try {
      await api.checkModel(model.name);
      setRefused((previous) => {
        const next = { ...previous };
        delete next[model.name];
        return next;
      });
      await reload();
    } catch (cause) {
      setRefused((previous) => ({
        ...previous,
        [model.name]: messageFor(cause),
      }));
    } finally {
      setChecking(null);
    }
  }

  async function download() {
    if (!window.confirm(DOWNLOAD_WARNING)) return;
    setDownloading(true);
    const poll = window.setInterval(() => void onRefreshStatus(), 750);
    try {
      await api.downloadModel();
      await onRefreshStatus();
      await reload();
    } catch (cause) {
      onMessage(messageFor(cause));
    } finally {
      window.clearInterval(poll);
      setDownloading(false);
    }
  }

  const hasTested = models?.some((model) => model.name === TESTED_MODEL);
  const selected = models?.find((model) => model.selected);

  return (
    <div className="model-picker">
      {models === null ? (
        <p className="model-empty">Looking for installed models…</p>
      ) : models.length === 0 ? (
        <p className="model-empty">
          No local models yet. DayPlan can download {TESTED_MODEL}, the model it
          is tested against, or it will find any model you install with Ollama.
        </p>
      ) : (
        <ul className="model-list">
          {models.map((model) => (
            <li key={model.name}>
              <label
                className={`model-row ${model.selected ? "chosen" : ""}`}
                aria-busy={checking === model.name}
              >
                <input
                  type="radio"
                  name="planner-model"
                  checked={model.selected}
                  disabled={checking !== null}
                  onChange={() => void choose(model)}
                />
                <span className="model-main">
                  <span className="model-name">{model.name}</span>
                  <span className="model-meta">
                    {model.source === "dayplan"
                      ? "Downloaded by DayPlan"
                      : "Already on this machine"}
                    {" · "}
                    {sizeLabel(model.sizeBytes)}
                  </span>
                </span>
                {checking === model.name ? (
                  <span className="model-badge">
                    <Spinner /> Checking
                  </span>
                ) : model.tested ? (
                  <span className="model-badge tested">Tested</span>
                ) : model.checked ? (
                  <span className="model-badge">Checked</span>
                ) : (
                  <span className="model-badge warn">Not checked</span>
                )}
              </label>
              {refused[model.name] && (
                <p className="model-warning">
                  <Mark size={6} filled color="var(--danger-dot)" />
                  {refused[model.name]}
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
      {selected && !selected.tested && !refused[selected.name] && (
        <p className="model-note">
          DayPlan is evaluated against {TESTED_MODEL}. {selected.name} answers
          in the right format, but its plans haven&rsquo;t been measured.
        </p>
      )}
      {!hasTested && (
        <div className="model-actions">
          <button disabled={downloading} onClick={() => void download()}>
            {downloading ? "Downloading…" : `Download ${TESTED_MODEL} (5.2 GB)`}
          </button>
          {downloading && (
            <button onClick={() => void api.cancelModelDownload()}>
              Cancel
            </button>
          )}
        </div>
      )}
      {status?.download && (
        <progress max="100" value={status.download.percent ?? undefined}>
          {status.download.percent ?? 0}%
        </progress>
      )}
    </div>
  );
}

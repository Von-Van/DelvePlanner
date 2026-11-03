import { useCallback, useEffect, useRef, useState } from "react";
import { AgentRun, api } from "./api";
import type { LiveRun } from "./assistantTasks";

/** When to look again for a run's record after its last step, since it is stored just after. */
const recordChecks = [400, 1500, 4000];

/**
 * What the assistant is doing across the app, for the Tasks board and the approval card: this
 * session's finished runs, and the ones still running (known from their live steps). It only
 * reads; every change still goes through the Assistant's own review.
 */
export function useAssistantActivity() {
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [live, setLive] = useState<LiveRun[]>([]);
  // What each run was asked, by run key, for runs started before their first step arrives.
  const requests = useRef(new Map<string, string>());

  const withRecords = useCallback((list: AgentRun[]) => {
    setRuns(list);
    setLive((current) =>
      current.filter((item) => !list.some((run) => run.runKey === item.runKey)),
    );
  }, []);

  const refresh = useCallback(async () => {
    try {
      withRecords(await api.listAssistantRuns());
    } catch {
      // The board keeps what it last knew; the Assistant reports its own errors.
    }
  }, [withRecords]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const timers: number[] = [];
    const unlisten = api.onAssistantStep(({ runKey, event }) => {
      setLive((current) => {
        const existing = current.find((item) => item.runKey === runKey);
        const next: LiveRun = {
          runKey,
          request: requests.current.get(runKey) ?? existing?.request ?? null,
          steps: Math.max((existing?.steps ?? 0) + 1, event.seq + 1),
          lastStep: event.summary.trim() || existing?.lastStep || null,
          startedAt: existing?.startedAt ?? new Date().toISOString(),
        };
        return [...current.filter((item) => item.runKey !== runKey), next];
      });
      if (event.kind === "final" || event.kind === "error")
        for (const delay of recordChecks)
          timers.push(window.setTimeout(() => void refresh(), delay));
    });
    return () => {
      unlisten.then((stop) => stop()).catch(() => undefined);
      timers.forEach((timer) => window.clearTimeout(timer));
    };
  }, [refresh]);

  /** A run is starting: shows it as working straight away, with what was asked. */
  const started = useCallback((runKey: string, request: string) => {
    requests.current.set(runKey, request);
    setLive((current) =>
      current.some((item) => item.runKey === runKey)
        ? current.map((item) =>
            item.runKey === runKey ? { ...item, request } : item,
          )
        : [
            ...current,
            {
              runKey,
              request,
              steps: 0,
              lastStep: null,
              startedAt: new Date().toISOString(),
            },
          ],
    );
  }, []);

  /** A run is over, whatever happened: it stops showing as working and the records reload. */
  const ended = useCallback(
    async (runKey: string) => {
      requests.current.delete(runKey);
      setLive((current) => current.filter((item) => item.runKey !== runKey));
      await refresh();
    },
    [refresh],
  );

  return { runs, live, refresh, started, ended, report: withRecords };
}

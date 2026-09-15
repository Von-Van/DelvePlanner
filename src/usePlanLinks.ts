import { useEffect, useRef, useState } from "react";
import { api, messageFor, Milestone, Workstream } from "./api";
import { compareMilestones } from "./planning";

/** Loads the milestones and workstreams an item can link to once it belongs to `planId`. */
export function usePlanLinks(
  planId: string | null,
  onError: (message: string) => void,
) {
  const [links, setLinks] = useState<{
    milestones: Milestone[];
    workstreams: Workstream[];
  }>({ milestones: [], workstreams: [] });
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    setLinks({ milestones: [], workstreams: [] });
    if (!planId) return;
    let current = true;
    api
      .getPlanWorkspace(planId)
      .then((workspace) => {
        if (!current) return;
        setLinks({
          milestones: workspace.milestones.sort(compareMilestones),
          workstreams: workspace.workstreams,
        });
      })
      .catch((cause) => {
        if (current) onErrorRef.current(messageFor(cause));
      });
    return () => {
      current = false;
    };
  }, [planId]);

  return links;
}

import { createContext, useContext } from "react";
import type { TaskReference } from "./api";

/**
 * Every open task by ID, loaded with the rest of the app's data, so any task row can say what it
 * still waits on. A task missing from the map is finished, so nothing waits on it any more.
 */
export const OpenTasksContext = createContext<
  ReadonlyMap<string, TaskReference>
>(new Map());

export function useOpenTasks() {
  return useContext(OpenTasksContext);
}

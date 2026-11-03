import type { AgentRun } from "./api";
import { fallbackReason } from "./AssistantRuns";
import { plural } from "./planning";

/**
 * The Tasks board: every request handed to the assistant this session, sorted by what it needs.
 * "Needs you" holds questions and changes waiting for approval, "Working" what is running now,
 * and "Done" everything that finished. It is built only from the runs the app already keeps, so
 * nothing here can change the planner on its own.
 */

/** A request still running, known from its live steps before its run record exists. */
export type LiveRun = {
  runKey: string;
  /** What was asked, when the screen that started it said so. */
  request: string | null;
  steps: number;
  /** The newest step's own words, such as "Opened Website relaunch". */
  lastStep: string | null;
  startedAt: string;
};

export type DoneTone = "good" | "attention" | "neutral" | "problem";

export type BoardCard =
  | {
      kind: "question";
      runKey: string;
      title: string;
      /** The assistant's question, in its own words. */
      text: string;
      at: string;
    }
  | {
      kind: "review";
      runKey: string;
      title: string;
      summary: string;
      changes: number;
      at: string;
    }
  | {
      kind: "working";
      runKey: string;
      title: string;
      lastStep: string | null;
      steps: number;
      at: string;
    }
  | {
      kind: "done";
      runKey: string;
      title: string;
      label: string;
      text: string;
      tone: DoneTone;
      /** Whether asking again could help: it stopped, failed or was stopped by you. */
      retry: boolean;
      at: string;
    };

export type Board = {
  needsYou: BoardCard[];
  working: BoardCard[];
  done: BoardCard[];
};

/** How many finished requests the Done column shows; older ones stay in conversations. */
export const doneLimit = 8;

const untitled = "Request to the assistant";

function title(goal: string | null | undefined) {
  const trimmed = goal?.trim();
  return trimmed ? trimmed : untitled;
}

/** Whether a later run continued the same conversation, which answers an earlier question. */
function followedUp(run: AgentRun, runs: AgentRun[]) {
  return (
    run.threadId !== null &&
    runs.some(
      (other) =>
        other.runKey !== run.runKey &&
        other.threadId === run.threadId &&
        other.startedAt > run.startedAt,
    )
  );
}

/** Whether a run's proposed changes still wait for a decision and can still be applied. */
export function awaitsApproval(run: AgentRun, now = Date.now()) {
  return (
    !run.cancelled &&
    run.status === "drafted" &&
    run.review.state === "pending" &&
    run.proposal !== null &&
    Date.parse(run.proposal.expiresAt) > now
  );
}

/** Whether a run ended on a question that nothing has answered yet. */
export function asksQuestion(run: AgentRun, runs: AgentRun[]) {
  return (
    !run.cancelled &&
    run.status === "needs_clarification" &&
    !followedUp(run, runs)
  );
}

function doneCard(run: AgentRun, now: number): BoardCard {
  const base = {
    kind: "done" as const,
    runKey: run.runKey,
    title: title(run.goal),
    at: run.finishedAt,
  };
  if (run.cancelled)
    return {
      ...base,
      label: "You stopped it",
      text: "Stopped before it finished. Nothing was changed.",
      tone: "neutral",
      retry: true,
    };
  const answer = run.answer?.trim() ?? "";
  switch (run.status) {
    case "answered":
      return {
        ...base,
        label: "Answered",
        text: answer,
        tone: "good",
        retry: false,
      };
    case "needs_clarification":
      return {
        ...base,
        label: "Answered in a follow-up",
        text: answer,
        tone: "good",
        retry: false,
      };
    case "drafted": {
      const review = run.review;
      const changes = run.proposal?.operations.length ?? 0;
      if (review.state === "applied")
        return {
          ...base,
          label: "Applied",
          text: `${plural(review.changes, "change")} applied to your planner.`,
          tone: "good",
          retry: false,
        };
      if (review.state === "discarded")
        return {
          ...base,
          label: "Not applied",
          text: `You turned down ${plural(changes, "proposed change")}.`,
          tone: "neutral",
          retry: false,
        };
      if (review.state === "failed")
        return {
          ...base,
          label: "Couldn't apply",
          text: review.message,
          tone: "problem",
          retry: true,
        };
      if (
        review.state === "expired" ||
        (review.state === "pending" &&
          run.proposal &&
          Date.parse(run.proposal.expiresAt) <= now)
      )
        return {
          ...base,
          label: "Expired",
          text: "These changes expired before they were applied, so your planner may have moved on. Ask again for a fresh draft.",
          tone: "neutral",
          retry: true,
        };
      return {
        ...base,
        label: "Answered",
        text: answer,
        tone: "good",
        retry: false,
      };
    }
    case "cannot_complete":
      if (fallbackReason(run))
        return {
          ...base,
          label: "Didn't finish",
          text: answer || run.error?.trim() || "The assistant stopped early.",
          tone: "problem",
          retry: true,
        };
      return {
        ...base,
        label: "Stopped safely",
        text: answer || "It couldn't do this without guessing, so it stopped.",
        tone: "attention",
        retry: true,
      };
    case "failed":
      return {
        ...base,
        label: "Didn't finish",
        text: run.error?.trim() || "The run failed before it could answer.",
        tone: "problem",
        retry: true,
      };
  }
}

/**
 * Sorts this session's runs, plus any still running, into the board's three columns. Each column
 * is newest first. A run whose record has arrived is never shown as working too.
 */
export function assistantBoard(
  runs: AgentRun[],
  live: LiveRun[] = [],
  now = Date.now(),
): Board {
  const byNewest = [...runs].sort((a, b) =>
    b.finishedAt.localeCompare(a.finishedAt),
  );
  const needsYou: BoardCard[] = [];
  const done: BoardCard[] = [];
  for (const run of byNewest) {
    if (awaitsApproval(run, now) && run.proposal)
      needsYou.push({
        kind: "review",
        runKey: run.runKey,
        title: title(run.goal),
        summary: run.proposal.summary,
        changes: run.proposal.operations.length,
        at: run.finishedAt,
      });
    else if (asksQuestion(run, runs))
      needsYou.push({
        kind: "question",
        runKey: run.runKey,
        title: title(run.goal),
        text: run.answer?.trim() || "It needs a little more detail.",
        at: run.finishedAt,
      });
    else done.push(doneCard(run, now));
  }
  const recorded = new Set(runs.map((run) => run.runKey));
  const working: BoardCard[] = live
    .filter((item) => !recorded.has(item.runKey))
    .sort((a, b) => b.startedAt.localeCompare(a.startedAt))
    .map((item) => ({
      kind: "working",
      runKey: item.runKey,
      title: title(item.request),
      lastStep: item.lastStep,
      steps: item.steps,
      at: item.startedAt,
    }));
  return { needsYou, working, done: done.slice(0, doneLimit) };
}

/** How many requests need the person, for the sidebar's count. */
export function needsYouCount(runs: AgentRun[], now = Date.now()) {
  return runs.filter(
    (run) => awaitsApproval(run, now) || asksQuestion(run, runs),
  ).length;
}

/** "9:12 AM", the way the board stamps each card. */
export function cardTime(at: string) {
  return new Intl.DateTimeFormat("en-US", {
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date(at));
}

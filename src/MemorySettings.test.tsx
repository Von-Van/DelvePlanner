import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  contentProblem,
  groupRecords,
  memoryApi,
  MemoryOverview,
  memoryOverviewSchema,
  MemoryRecord,
  memoryStatusLine,
  provenance,
} from "./memoryApi";
import { MemoryPanel } from "./MemorySettings";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const PLAN = "5b0d3f0e-5f5c-4d47-9a1e-1f2d3c4b5a69";

function record(overrides: Partial<MemoryRecord> = {}): MemoryRecord {
  return {
    id: "m1",
    kind: "decision",
    content: "The weekly launch sync moved to Thursdays at 10.",
    source: "user_correction",
    sourceRef: "m0",
    planId: PLAN,
    confirmation: "confirmed",
    createdAt: "2026-10-28T15:00:00.000Z",
    updatedAt: "2026-10-28T15:00:00.000Z",
    confirmedAt: "2026-10-28T15:00:00.000Z",
    expiresAt: null,
    supersededBy: null,
    supersededAt: null,
    ...overrides,
  };
}

const overview: MemoryOverview = {
  settings: {
    enabled: true,
    conversationTurns: 6,
    inferredTtlDays: 14,
    disabledPlanIds: [],
  },
  records: [
    record(),
    record({
      id: "m0",
      content: "The weekly launch sync is on Tuesdays at 10.",
      source: "user_stated",
      sourceRef: null,
      supersededBy: "m1",
      supersededAt: "2026-10-28T15:00:00.000Z",
    }),
    record({
      id: "m2",
      kind: "preference",
      content: "Probably prefers mornings for deep work.",
      source: "assistant_inferred",
      planId: null,
      confirmation: "unconfirmed",
      confirmedAt: null,
      expiresAt: "2026-11-15T15:00:00.000Z",
    }),
  ],
};

function panel(props: Partial<Parameters<typeof MemoryPanel>[0]> = {}) {
  const noop = () => undefined;
  const saved = () => Promise.resolve(true);
  return renderToStaticMarkup(
    <MemoryPanel
      overview={overview}
      projects={[{ id: PLAN, title: "Website relaunch" }]}
      busy={false}
      onUpdateSettings={noop}
      onProjectMemory={noop}
      onForgetProject={noop}
      onAdd={saved}
      onConfirm={noop}
      onCorrect={saved}
      onDelete={noop}
      onExport={noop}
      onClear={noop}
      {...props}
    />,
  );
}

describe("memory helpers", () => {
  it("splits current memories from corrected history", () => {
    const { current, history } = groupRecords(overview.records);
    expect(current.map((r) => r.id)).toEqual(["m1", "m2"]);
    expect(history.map((r) => r.id)).toEqual(["m0"]);
  });

  it("describes where a memory came from", () => {
    const titles = new Map([[PLAN, "Website relaunch"]]);
    const line = provenance(overview.records[0], titles);
    expect(line).toContain("Decision");
    expect(line).toContain("Your correction");
    expect(line).toContain("Website relaunch");
    expect(provenance(overview.records[2], titles)).toContain(
      "Assistant's guess",
    );
  });

  it("says when memory is paused or empty", () => {
    expect(
      memoryStatusLine({ ...overview.settings, enabled: false }, 3),
    ).toMatch(/^Paused/);
    expect(memoryStatusLine(overview.settings, 0)).toContain("Nothing");
    expect(memoryStatusLine(overview.settings, 1)).toContain("1 memory");
  });

  it("checks memory text", () => {
    expect(contentProblem("   ")).not.toBeNull();
    expect(contentProblem("x".repeat(501))).not.toBeNull();
    expect(contentProblem("Fine.")).toBeNull();
  });
});

describe("MemoryPanel", () => {
  it("lists current memories with provenance and labels guesses", () => {
    const html = panel();
    expect(html).toContain("Remembered (2)");
    expect(html).toContain("Thursdays at 10");
    expect(html).toContain("Unconfirmed · expires");
    expect(html).toContain(">Confirm<");
    expect(html).toContain("History: 1 replaced by a correction");
    expect(html).toContain("Tuesdays at 10");
    expect(html).toContain("Website relaunch");
    expect(html).toContain("Export memory");
    expect(html).toContain("Forget everything");
  });

  it("marks memories whose project has memory off", () => {
    const html = panel({
      overview: {
        ...overview,
        settings: { ...overview.settings, disabledPlanIds: [PLAN] },
      },
    });
    expect(html).toContain("Project memory off");
  });

  it("shows the paused state and disables adding", () => {
    const html = panel({
      overview: {
        ...overview,
        settings: { ...overview.settings, enabled: false },
      },
    });
    expect(html).toContain("Paused.");
    expect(html).toMatch(/<button type="submit"[^>]*disabled/);
  });
});

describe("memoryApi", () => {
  beforeEach(() => invoke.mockReset());

  it("reads the overview and rejects unexpected shapes", async () => {
    invoke.mockResolvedValueOnce(overview);
    await expect(memoryApi.overview()).resolves.toEqual(
      memoryOverviewSchema.parse(overview),
    );
    expect(invoke).toHaveBeenCalledWith("get_memory_overview", undefined);
    invoke.mockResolvedValueOnce({ settings: {} });
    await expect(memoryApi.overview()).rejects.toThrow(/shape/);
  });

  it("passes command errors through as readable messages", async () => {
    invoke.mockRejectedValueOnce({
      code: "validation",
      message: "Memory is paused. Turn it on in Settings first.",
    });
    await expect(
      memoryApi.add({ kind: "note", content: "x", planId: null }),
    ).rejects.toThrow("Memory is paused");
  });

  it("sends corrections and project switches with the right arguments", async () => {
    invoke.mockResolvedValueOnce(record({ id: "m3" }));
    await memoryApi.correct("m1", "New text");
    expect(invoke).toHaveBeenLastCalledWith("correct_memory", {
      id: "m1",
      content: "New text",
    });
    invoke.mockResolvedValueOnce(overview.settings);
    await memoryApi.setProjectMemory(PLAN, false);
    expect(invoke).toHaveBeenLastCalledWith("set_project_memory", {
      planId: PLAN,
      enabled: false,
    });
  });
});

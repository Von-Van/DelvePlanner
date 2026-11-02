import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  memoryApi,
  MemorySuggestion,
  memorySuggestionSchema,
} from "./memoryApi";
import {
  MemorySuggestions,
  suggestionLine,
  suggestionOutcome,
} from "./MemorySuggestions";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

function suggestion(
  overrides: Partial<MemorySuggestion> = {},
): MemorySuggestion {
  return {
    id: "s1",
    kind: "priority",
    content:
      "On the apartment move, booking the movers comes before the lease.",
    basis: "stated",
    planId: "00000000-0000-4000-8000-000000000102",
    planTitle: "Apartment move",
    state: { state: "pending" },
    ...overrides,
  };
}

const quiet = {
  onMessage: () => {},
  onRefresh: () => Promise.resolve(),
};

describe("Remember this?", () => {
  it("labels who a suggestion comes from, so a guess never reads as the person's words", () => {
    expect(suggestionLine(suggestion())).toBe(
      "Priority · You said this · Apartment move",
    );
    const guess = suggestionLine(
      suggestion({ basis: "inferred", kind: "preference", planTitle: null }),
    );
    expect(guess).toMatch(/^Preference · Assistant's guess/);
    expect(guess).toContain("unconfirmed");
    expect(guess).toContain("expires");
  });

  it("asks before remembering, and says what happened once answered", () => {
    const html = renderToStaticMarkup(
      <MemorySuggestions
        runKey="run-1"
        suggestions={[suggestion()]}
        problems={[]}
        {...quiet}
      />,
    );
    expect(html).toContain("REMEMBER THIS?");
    expect(html).toContain("booking the movers");
    expect(html).toContain("Remember</button>");
    expect(html).toContain("Not now</button>");

    expect(suggestionOutcome(suggestion())).toBeNull();
    expect(
      suggestionOutcome(
        suggestion({ state: { state: "remembered", memoryId: "m1" } }),
      ),
    ).toMatch(/^Remembered\. /);
    expect(
      suggestionOutcome(
        suggestion({
          basis: "inferred",
          state: { state: "remembered", memoryId: "m1" },
        }),
      ),
    ).toMatch(/as a guess/);
    expect(
      suggestionOutcome(suggestion({ state: { state: "dismissed" } })),
    ).toBe("Not remembered.");
  });

  it("disables the buttons while a suggestion is being saved", () => {
    const html = renderToStaticMarkup(
      <MemorySuggestions
        runKey="run-1"
        suggestions={[suggestion({ state: { state: "saving" } })]}
        problems={[]}
        {...quiet}
      />,
    );
    expect(html.match(/disabled=""/g)).toHaveLength(2);
  });

  it("shows nothing without suggestions, and lists the ones left out", () => {
    const none = renderToStaticMarkup(
      <MemorySuggestions
        runKey="run-1"
        suggestions={[]}
        problems={[]}
        {...quiet}
      />,
    );
    expect(none).toBe("");
    const left = renderToStaticMarkup(
      <MemorySuggestions
        runKey="run-1"
        suggestions={[]}
        problems={["1 memory suggestion(s) left out by the memory settings"]}
        {...quiet}
      />,
    );
    expect(left).toContain("Left out 1 memory suggestion:");
  });
});

describe("suggestion commands", () => {
  beforeEach(() => invoke.mockReset());

  it("send the run and suggestion, and read back the new state", async () => {
    const remembered = suggestion({
      state: { state: "remembered", memoryId: "m9" },
    });
    invoke.mockResolvedValueOnce(remembered);
    await expect(memoryApi.rememberSuggestion("run-1", "s1")).resolves.toEqual(
      remembered,
    );
    expect(invoke).toHaveBeenLastCalledWith("remember_suggestion", {
      runKey: "run-1",
      suggestionId: "s1",
    });
    invoke.mockResolvedValueOnce(suggestion({ state: { state: "dismissed" } }));
    await memoryApi.dismissSuggestion("run-1", "s2");
    expect(invoke).toHaveBeenLastCalledWith("dismiss_suggestion", {
      runKey: "run-1",
      suggestionId: "s2",
    });
  });

  it("refuse a suggestion with an unknown basis", () => {
    expect(
      memorySuggestionSchema.safeParse({ ...suggestion(), basis: "maybe" })
        .success,
    ).toBe(false);
  });
});

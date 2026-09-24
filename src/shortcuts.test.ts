import { describe, expect, it } from "vitest";
import { recordedShortcut, shortcutText } from "./shortcuts";

const press = (
  code: string,
  key: string,
  modifiers: Partial<
    Record<"shiftKey" | "ctrlKey" | "altKey" | "metaKey", boolean>
  > = {},
) => ({
  code,
  key,
  shiftKey: false,
  ctrlKey: false,
  altKey: false,
  metaKey: false,
  ...modifiers,
});

describe("shortcuts", () => {
  it("records modifiers first and the physical key last", () => {
    expect(
      recordedShortcut(press("Space", " ", { shiftKey: true, metaKey: true })),
    ).toBe("shift+super+Space");
    // Option changes the character but not the key's code.
    expect(
      recordedShortcut(press("KeyK", "˚", { ctrlKey: true, altKey: true })),
    ).toBe("control+alt+KeyK");
  });

  it("waits while only modifiers are held", () => {
    expect(
      recordedShortcut(press("ShiftLeft", "Shift", { shiftKey: true })),
    ).toBeNull();
    expect(
      recordedShortcut(press("MetaLeft", "Meta", { metaKey: true })),
    ).toBeNull();
  });

  it("writes a saved shortcut the way each platform does", () => {
    expect(shortcutText("shift+super+Space", "mac")).toBe("⇧⌘Space");
    expect(shortcutText("control+alt+KeyK", "mac")).toBe("⌃⌥K");
    expect(shortcutText("shift+super+Space", "windows")).toBe(
      "Shift+Win+Space",
    );
    expect(shortcutText("control+alt+Digit5", "linux")).toBe("Ctrl+Alt+5");
    expect(shortcutText("CmdOrCtrl+Shift+KeyN", "mac")).toBe("⇧⌘N");
    expect(shortcutText("CmdOrCtrl+Shift+KeyN", "windows")).toBe(
      "Ctrl+Shift+N",
    );
    expect(shortcutText("SHIFT+META+ArrowUp", "mac")).toBe("⇧⌘↑");
  });
});

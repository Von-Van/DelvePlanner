/** How keyboard shortcuts are recorded and shown: the in-app ones and the system-wide one. */
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

export type ShortcutPlatform = "mac" | "windows" | "linux";

export const shortcutPlatform: ShortcutPlatform = /Mac|iPhone|iPad/.test(
  navigator.userAgent,
)
  ? "mac"
  : /Windows/.test(navigator.userAgent)
    ? "windows"
    : "linux";

/** Whether the platform's command key is down: Command on a Mac, Control elsewhere. */
export function commandKey(event: { metaKey: boolean; ctrlKey: boolean }) {
  return shortcutPlatform === "mac" ? event.metaKey : event.ctrlKey;
}

/** An in-app shortcut as the platform writes it: "⌘N" and "⇧⌘N", or "Ctrl+N" and "Ctrl+Shift+N". */
export function commandShortcutText(
  key: string,
  { shift = false } = {},
  platform: ShortcutPlatform = shortcutPlatform,
) {
  if (platform === "mac") return `${shift ? "⇧" : ""}⌘${key}`;
  return `Ctrl+${shift ? "Shift+" : ""}${key === "↩" ? "Enter" : key}`;
}

/** Whether keys pressed in this element edit text, where arrows and letters belong to it. */
export function isTextEntry(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    (target instanceof HTMLInputElement &&
      !["button", "checkbox", "radio", "range", "submit", "reset"].includes(
        target.type,
      ))
  );
}

/**
 * ⌘Return (Ctrl+Enter elsewhere) saves the form it's pressed in, even from a notes field where
 * Return starts a new line. Nothing happens while the form's save button is disabled.
 */
export function submitOnCommandEnter(event: ReactKeyboardEvent<HTMLElement>) {
  if (event.key !== "Enter" || !commandKey(event) || event.altKey) return;
  const form =
    event.currentTarget instanceof HTMLFormElement
      ? event.currentTarget
      : event.currentTarget.closest("form");
  const buttons = Array.from(form?.querySelectorAll("button") ?? []);
  const save =
    buttons.find((button) => button.classList.contains("editor-save")) ??
    buttons.find((button) => button.type === "submit");
  if (!form || !save || save.disabled) return;
  event.preventDefault();
  form.requestSubmit(save);
}

const modifierKeys = new Set([
  "Alt",
  "AltGraph",
  "CapsLock",
  "Control",
  "Fn",
  "FnLock",
  "Hyper",
  "Meta",
  "OS",
  "Shift",
  "Super",
]);

/**
 * The combination a key press records, spelled the way Delve Planner's Rust side reads it:
 * modifiers first, then the physical key, such as "shift+super+Space". Null while only modifiers
 * are down.
 */
export function recordedShortcut(
  event: Pick<
    KeyboardEvent,
    "key" | "code" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey"
  >,
) {
  if (modifierKeys.has(event.key) || !event.code) return null;
  return [
    event.shiftKey && "shift",
    event.ctrlKey && "control",
    event.altKey && "alt",
    event.metaKey && "super",
    event.code,
  ]
    .filter(Boolean)
    .join("+");
}

const keyNames: Record<string, string> = {
  space: "Space",
  enter: "Return",
  tab: "Tab",
  backspace: "Delete",
  escape: "Esc",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  minus: "-",
  equal: "=",
  bracketleft: "[",
  bracketright: "]",
  backslash: "\\",
  semicolon: ";",
  quote: "'",
  comma: ",",
  period: ".",
  slash: "/",
  backquote: "`",
};

function keyText(code: string) {
  const letter = /^key([a-z])$/i.exec(code);
  if (letter) return letter[1].toUpperCase();
  const digit = /^digit([0-9])$/i.exec(code);
  if (digit) return digit[1];
  return keyNames[code.toLowerCase()] ?? code;
}

/**
 * A saved shortcut as people write it: "⌃⌥⇧⌘K" on a Mac, "Ctrl+Alt+Shift+K" elsewhere. Accepts
 * any spelling the Rust side does, in any case.
 */
export function shortcutText(
  shortcut: string,
  platform: ShortcutPlatform = shortcutPlatform,
) {
  const tokens = shortcut
    .split("+")
    .map((token) => token.trim().toLowerCase())
    .filter(Boolean);
  const key = shortcut.split("+").pop()?.trim() ?? "";
  const has = (...names: string[]) =>
    tokens.slice(0, -1).some((token) => names.includes(token));
  const commandOrControl = has(
    "cmdorctrl",
    "cmdorcontrol",
    "commandorctrl",
    "commandorcontrol",
  );
  const control =
    has("control", "ctrl") || (commandOrControl && platform !== "mac");
  const alt = has("alt", "option");
  const shift = has("shift");
  const command =
    has("super", "meta", "cmd", "command") ||
    (commandOrControl && platform === "mac");
  if (platform === "mac")
    return `${control ? "⌃" : ""}${alt ? "⌥" : ""}${shift ? "⇧" : ""}${command ? "⌘" : ""}${keyText(key)}`;
  return [
    control && "Ctrl",
    alt && "Alt",
    shift && "Shift",
    command && (platform === "windows" ? "Win" : "Super"),
    keyText(key),
  ]
    .filter(Boolean)
    .join("+");
}

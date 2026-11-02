/**
 * Heads-ups: things the assistant brings up before it is asked, such as a deadline with no time
 * planned or two commitments at the same time. Fixed rules in Rust find them; this file reads
 * them and passes the person's choice (act, not now, dismiss) back.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { z } from "zod";

export const headsUpKinds = [
  "deadline",
  "clash",
  "focus_window",
  "overloaded",
  "pushed_back",
  "quiet_project",
] as const;
export type HeadsUpKind = (typeof headsUpKinds)[number];

export const headsUpSchema = z.object({
  id: z.string().min(1),
  kind: z.enum(headsUpKinds),
  title: z.string(),
  /** The facts behind it, so it is never a suggestion without a reason. */
  evidence: z.string(),
  target: z
    .object({
      kind: z.enum(["task", "plan", "day", "event", "block"]),
      id: z.string(),
    })
    .nullable(),
  /** The button that hands it to the assistant ("Plan it"). */
  actionLabel: z.string(),
  /** What the assistant is asked when that button is pressed. */
  actionRequest: z.string(),
  score: z.number().int(),
  urgent: z.boolean(),
  status: z.enum([
    "active",
    "snoozed",
    "dismissed",
    "acted",
    "cleared",
    "expired",
  ]),
  surfacedAt: z.string(),
  expiresAt: z.string(),
  snoozedUntil: z.string().nullable(),
});
export type HeadsUp = z.infer<typeof headsUpSchema>;

export const headsUpSettingsSchema = z.object({
  enabled: z.boolean(),
  notify: z.boolean(),
});
export type HeadsUpSettings = z.infer<typeof headsUpSettingsSchema>;

const commandErrorSchema = z.object({ message: z.string() });

async function call<T>(
  schema: z.ZodType<T>,
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  let raw: unknown;
  try {
    raw = await invoke(command, args);
  } catch (cause) {
    const parsed = commandErrorSchema.safeParse(cause);
    throw new Error(parsed.success ? parsed.data.message : String(cause));
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    throw new Error("Heads-ups came back in a shape the app can't read.");
  }
  return parsed.data;
}

export const headsUpApi = {
  /** The heads-ups to show now (at most three). `refresh` checks again first. */
  list: (refresh: boolean) =>
    call(z.array(headsUpSchema), "list_heads_ups", { refresh }),
  /** Don't show it again. */
  dismiss: (id: string) => call(z.null(), "dismiss_heads_up", { id }),
  /** Not now: hidden until tomorrow's working hours start, then back if still true. */
  snooze: (id: string) => call(headsUpSchema, "snooze_heads_up", { id }),
  /** Hands it to the assistant; resolves with its request. */
  act: (id: string) => call(headsUpSchema, "act_on_heads_up", { id }),
  settings: () => call(headsUpSettingsSchema, "get_heads_up_settings"),
  updateSettings: (settings: HeadsUpSettings) =>
    call(headsUpSettingsSchema, "update_heads_up_settings", { settings }),
  /** The background check found something new, or something cleared. */
  onChanged: (handler: () => void): Promise<UnlistenFn> =>
    listen("heads-ups-changed", () => handler()),
};

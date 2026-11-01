/**
 * The assistant's memory, as the Memory settings panel sees it. The person can read, add,
 * confirm, correct, delete and export memories, pause memory, and switch it off per project.
 * The assistant never writes here; it only reads current memories through its get_memory tool.
 */
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

export const memoryKinds = [
  "decision",
  "project_fact",
  "preference",
  "note",
] as const;
export type MemoryKind = (typeof memoryKinds)[number];

export const memorySources = [
  "user_stated",
  "user_correction",
  "approved_action",
  "assistant_inferred",
] as const;
export type MemorySource = (typeof memorySources)[number];

export const MAX_MEMORY_CHARS = 500;
export const conversationTurnChoices = [0, 2, 4, 6, 10, 20] as const;
export const inferredTtlChoices = [3, 7, 14, 30, 90] as const;

export const memoryRecordSchema = z.object({
  id: z.string().min(1),
  kind: z.enum(memoryKinds),
  content: z.string(),
  source: z.enum(memorySources),
  sourceRef: z.string().nullable(),
  planId: z.string().nullable(),
  confirmation: z.enum(["confirmed", "unconfirmed"]),
  createdAt: z.string(),
  updatedAt: z.string(),
  confirmedAt: z.string().nullable(),
  expiresAt: z.string().nullable(),
  supersededBy: z.string().nullable(),
  supersededAt: z.string().nullable(),
});
export type MemoryRecord = z.infer<typeof memoryRecordSchema>;

export const memorySettingsSchema = z.object({
  enabled: z.boolean(),
  conversationTurns: z.number().int().min(0).max(20),
  inferredTtlDays: z.number().int().min(1).max(90),
  disabledPlanIds: z.array(z.string()),
});
export type MemorySettings = z.infer<typeof memorySettingsSchema>;

export const memoryOverviewSchema = z.object({
  settings: memorySettingsSchema,
  records: z.array(memoryRecordSchema),
});
export type MemoryOverview = z.infer<typeof memoryOverviewSchema>;

const fileResultSchema = z.object({
  completed: z.boolean(),
  fileName: z.string().nullable(),
});
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
    throw new Error("Memory came back in a shape this panel can't read.");
  }
  return parsed.data;
}

export type MemorySettingsUpdate = Partial<
  Pick<MemorySettings, "enabled" | "conversationTurns" | "inferredTtlDays">
>;

export const memoryApi = {
  overview: () => call(memoryOverviewSchema, "get_memory_overview"),
  updateSettings: (input: MemorySettingsUpdate) =>
    call(memorySettingsSchema, "update_memory_settings", { input }),
  setProjectMemory: (planId: string, enabled: boolean) =>
    call(memorySettingsSchema, "set_project_memory", { planId, enabled }),
  add: (input: { kind: MemoryKind; content: string; planId: string | null }) =>
    call(memoryRecordSchema, "add_memory", { input }),
  confirm: (id: string) => call(memoryRecordSchema, "confirm_memory", { id }),
  correct: (id: string, content: string) =>
    call(memoryRecordSchema, "correct_memory", { id, content }),
  remove: (id: string) => call(z.null(), "delete_memory", { id }),
  forgetProject: (planId: string) =>
    call(z.number().int(), "forget_project_memory", { planId }),
  clear: () => call(z.number().int(), "clear_memory"),
  exportFile: () => call(fileResultSchema, "export_memory_file"),
};
export type MemoryApi = typeof memoryApi;

// ---------------------------------------------------------------------------------------------
// Presentation helpers shared by the panel and its tests.

export const kindLabels: Record<MemoryKind, string> = {
  decision: "Decision",
  project_fact: "Project fact",
  preference: "Preference",
  note: "Note",
};

export const sourceLabels: Record<MemorySource, string> = {
  user_stated: "You said this",
  user_correction: "Your correction",
  approved_action: "From a change you approved",
  assistant_inferred: "Assistant's guess",
};

/** Current memories (what the assistant may use) and history (replaced by a correction). */
export function groupRecords(records: MemoryRecord[]) {
  const current = records.filter((r) => !r.supersededBy && !r.supersededAt);
  const history = records.filter((r) => r.supersededBy || r.supersededAt);
  return { current, history };
}

export function shortDate(iso: string | null) {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** One line of provenance: kind, source, project, and when. */
export function provenance(
  record: MemoryRecord,
  planTitles: Map<string, string>,
) {
  const parts = [kindLabels[record.kind], sourceLabels[record.source]];
  if (record.planId) parts.push(planTitles.get(record.planId) ?? "A project");
  parts.push(shortDate(record.updatedAt));
  return parts.filter(Boolean).join(" · ");
}

export function memoryStatusLine(settings: MemorySettings, current: number) {
  if (!settings.enabled)
    return "Paused. Nothing new is remembered and the assistant sees no memory.";
  if (current === 0) return "On. Nothing is remembered yet.";
  return `On. ${current} ${current === 1 ? "memory" : "memories"} the assistant may use.`;
}

export function contentProblem(text: string) {
  const trimmed = text.trim();
  if (!trimmed) return "Write what the assistant should remember.";
  if (trimmed.length > MAX_MEMORY_CHARS)
    return `Keep it under ${MAX_MEMORY_CHARS} characters.`;
  return null;
}

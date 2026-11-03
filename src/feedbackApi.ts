/**
 * Feedback on the assistant's replies (milestone M6). The person says a reply helped or didn't,
 * and why; a reason can become a lesson the assistant keeps ("remember for next time") or stay
 * with this conversation only ("just this once"). Nothing is learned without the person's yes,
 * and a kept lesson is an ordinary memory: Settings › Memory shows, corrects and deletes it.
 */
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { MAX_MEMORY_CHARS, MemoryKind, memoryKinds } from "./memoryApi";

export const MAX_FEEDBACK_REASON_CHARS = MAX_MEMORY_CHARS;
export const MAX_FEEDBACK_ABOUT_CHARS = 200;

/** The kinds a lesson can be saved as, in the order the picker offers them. */
export const lessonKinds: MemoryKind[] = [
  "preference",
  "priority",
  "decision",
  "note",
];

export const feedbackSchema = z.object({
  id: z.string().min(1),
  runId: z.string().nullable(),
  /** Set instead of runId for feedback on a heads-up. */
  suggestionId: z.string().nullable(),
  target: z.enum(["answer", "change", "suggestion"]),
  about: z.string().nullable(),
  rating: z.enum(["helpful", "not_helpful"]),
  reason: z.string().nullable(),
  scope: z.enum(["pending", "this_time", "lasting"]),
  memoryId: z.string().nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});
export type Feedback = z.infer<typeof feedbackSchema>;

export const feedbackReceiptSchema = z.object({
  feedback: feedbackSchema,
  /** The lesson on offer, in the person's own words, while they haven't chosen. */
  lesson: z
    .object({ kind: z.enum(memoryKinds), content: z.string() })
    .nullable(),
});
export type FeedbackReceipt = z.infer<typeof feedbackReceiptSchema>;

export type NewFeedback = {
  runId: string;
  target: "answer" | "change";
  about: string | null;
  rating: Feedback["rating"];
  reason: string | null;
};

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
    throw new Error("Feedback came back in a shape this screen can't read.");
  }
  return parsed.data;
}

export const feedbackApi = {
  record: (input: NewFeedback) =>
    call(feedbackReceiptSchema, "record_assistant_feedback", { input }),
  list: (runId: string) =>
    call(z.array(feedbackReceiptSchema), "list_assistant_feedback", { runId }),
  /** "Remember for next time", in the words the person confirmed. */
  learn: (feedbackId: string, kind: MemoryKind, content: string) =>
    call(feedbackReceiptSchema, "learn_from_feedback", {
      feedbackId,
      kind,
      content,
    }),
  /** "Just this once": nothing is remembered. */
  keepOneOff: (feedbackId: string) =>
    call(feedbackReceiptSchema, "keep_feedback_one_off", { feedbackId }),
};
export type FeedbackApi = typeof feedbackApi;

/** What settled feedback says on the run's card, or null while it still waits for a choice. */
export function feedbackOutcome(feedback: Feedback): string | null {
  if (feedback.rating === "helpful") return "You marked this reply as helpful.";
  if (feedback.scope === "pending") return null;
  if (feedback.scope === "lasting") {
    return feedback.memoryId
      ? "Remembered. The assistant follows it from now on; see, change or delete it in Settings › Memory."
      : "Remembered, then deleted in Settings › Memory.";
  }
  return feedback.reason
    ? "Noted for this conversation only. Nothing was remembered."
    : feedback.about
      ? `You said “${feedback.about}” wasn't helpful.`
      : "You marked this reply as not helpful.";
}

export function lessonProblem(text: string) {
  const trimmed = text.trim();
  if (!trimmed) return "Write the lesson in a few words.";
  if (trimmed.length > MAX_MEMORY_CHARS)
    return `Keep it under ${MAX_MEMORY_CHARS} characters.`;
  return null;
}

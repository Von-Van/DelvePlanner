import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Conversation, ConversationMessage } from "./api";
import {
  AssistantConversations,
  ConversationHistory,
  conversationDateLabel,
  earlierMessages,
  replyStatusLabel,
} from "./AssistantConversations";

function message(
  seq: number,
  role: ConversationMessage["role"],
  text: string,
  runId: string | null = null,
): ConversationMessage {
  return {
    id: `m${seq}`,
    threadId: "t1",
    seq,
    role,
    text,
    runId,
    runStatus: role === "assistant" ? "drafted" : null,
    createdAt: "2026-11-01T18:00:00.000Z",
  };
}

const gym = [
  message(1, "user", "Move gym to 6 pm today"),
  message(2, "assistant", "Move Gym to 6:00 PM.", "run-1"),
  message(3, "user", "actually 30 minutes later"),
  message(4, "assistant", "Move Gym to 6:30 PM.", "run-2"),
];

describe("earlierMessages", () => {
  it("leaves out the exchange already shown in full below", () => {
    expect(earlierMessages(gym, "run-2").map((m) => m.seq)).toEqual([1, 2]);
  });

  it("keeps everything when the run on screen isn't the newest reply", () => {
    expect(earlierMessages(gym, "run-1")).toHaveLength(4);
    expect(earlierMessages(gym, null)).toHaveLength(4);
  });
});

describe("ConversationHistory", () => {
  it("shows who said what, oldest first, with how each reply ended", () => {
    const html = renderToStaticMarkup(
      <ConversationHistory messages={gym.slice(0, 2)} />,
    );
    expect(html.indexOf("Move gym to 6 pm today")).toBeLessThan(
      html.indexOf("Move Gym to 6:00 PM."),
    );
    expect(html).toContain("Changes proposed");
    expect(html).toContain("chat-request");
    expect(html).toContain("chat-reply");
  });

  it("keeps an earlier proposal reachable after a follow-up", () => {
    const html = renderToStaticMarkup(
      <ConversationHistory
        messages={gym}
        awaitingReview={new Map([["run-1", "key-1"]])}
        onReview={() => undefined}
      />,
    );
    expect(html.match(/Review these changes/g)).toHaveLength(1);
    expect(html.indexOf("Review these changes")).toBeLessThan(
      html.indexOf("actually 30 minutes later"),
    );
  });

  it("shows nothing for a new conversation", () => {
    expect(renderToStaticMarkup(<ConversationHistory messages={[]} />)).toBe(
      "",
    );
  });
});

describe("AssistantConversations", () => {
  const conversation: Conversation = {
    id: "t1",
    title: "Move gym to 6 pm today",
    createdAt: "2026-11-01T18:00:00.000Z",
    updatedAt: "2026-11-01T18:05:00.000Z",
    messageCount: 4,
    preview: "Move Gym to 6:30 PM.",
  };
  const noop = () => undefined;

  it("lists saved conversations with rename and delete", () => {
    const html = renderToStaticMarkup(
      <AssistantConversations
        conversations={[conversation]}
        openId="t1"
        disabled={false}
        onOpen={noop}
        onNew={noop}
        onRename={async () => undefined}
        onDelete={noop}
      />,
    );
    expect(html).toContain("Move gym to 6 pm today");
    expect(html).toContain("4 messages");
    expect(html).toContain('aria-current="true"');
    expect(html).toContain("Rename “Move gym to 6 pm today”");
    expect(html).toContain("Delete “Move gym to 6 pm today”");
  });

  it("explains what conversations are for before there are any", () => {
    const html = renderToStaticMarkup(
      <AssistantConversations
        conversations={[]}
        openId={null}
        disabled={false}
        onOpen={noop}
        onNew={noop}
        onRename={async () => undefined}
        onDelete={noop}
      />,
    );
    expect(html).toContain("kept on this computer");
  });
});

describe("labels", () => {
  it("names a stopped reply", () => {
    expect(replyStatusLabel("cancelled")).toBe("Stopped");
    expect(replyStatusLabel("answered")).toBe("Answered");
    expect(replyStatusLabel(null)).toBeNull();
  });

  it("dates a conversation relative to today", () => {
    const now = new Date(2026, 10, 2, 9, 0);
    expect(
      conversationDateLabel(new Date(2026, 10, 1, 18).toISOString(), now),
    ).toBe("Yesterday");
    expect(
      conversationDateLabel(new Date(2026, 9, 28).toISOString(), now),
    ).toBe("Oct 28");
    expect(
      conversationDateLabel(new Date(2026, 10, 2, 8, 5).toISOString(), now),
    ).toBe("Today, 8:05 AM");
  });
});

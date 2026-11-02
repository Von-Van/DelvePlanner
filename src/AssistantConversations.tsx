import { FormEvent, useState } from "react";
import type { Conversation, ConversationMessage } from "./api";
import { runStatusLabels } from "./AssistantRuns";
import "./AssistantConversations.css";

/** How the run behind a reply ended, as the conversation shows it. */
export function replyStatusLabel(status: ConversationMessage["runStatus"]) {
  if (status === null) return null;
  return status === "cancelled" ? "Stopped" : runStatusLabels[status];
}

/**
 * The turns of a conversation that come before the run on screen. That run is shown in full
 * below the history (with its steps and any changes to review), so its request and reply are
 * left out here instead of appearing twice.
 */
export function earlierMessages(
  messages: ConversationMessage[],
  shownRunId: string | null,
) {
  if (shownRunId === null) return messages;
  const last = messages.length - 1;
  if (last < 0 || messages[last].runId !== shownRunId) return messages;
  const end = last > 0 && messages[last - 1].role === "user" ? last - 1 : last;
  return messages.slice(0, end);
}

/** "Today, 3:05 PM", "Yesterday", or "Oct 28". */
export function conversationDateLabel(at: string, now = new Date()) {
  const date = new Date(at);
  const day = (value: Date) =>
    new Date(value.getFullYear(), value.getMonth(), value.getDate()).getTime();
  const daysAgo = Math.round((day(now) - day(date)) / 86_400_000);
  if (daysAgo === 0)
    return `Today, ${date.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })}`;
  if (daysAgo === 1) return "Yesterday";
  return date.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

/**
 * The earlier turns of the open conversation, oldest first, in the same request-and-reply
 * bubbles as the run below them.
 */
export function ConversationHistory({
  messages,
}: {
  messages: ConversationMessage[];
}) {
  if (messages.length === 0) return null;
  return (
    <ol
      className="conversation-history"
      aria-label="Earlier in this conversation"
    >
      {messages.map((message) =>
        message.role === "user" ? (
          <li key={message.id} className="chat-request">
            {message.text}
          </li>
        ) : (
          <li key={message.id} className="chat-reply">
            <span className="chat-avatar" aria-hidden="true">
              D
            </span>
            <div className="chat-reply-body">
              {message.runStatus && (
                <span className="conversation-turn-status">
                  {replyStatusLabel(message.runStatus)}
                </span>
              )}
              <p className="agent-answer">{message.text}</p>
            </div>
          </li>
        ),
      )}
    </ol>
  );
}

/**
 * The saved conversations: start a new one, reopen, rename, or delete one. A new conversation
 * begins with its first request; deleting one removes its messages and the runs it started.
 */
export function AssistantConversations({
  conversations,
  openId,
  disabled,
  onOpen,
  onNew,
  onRename,
  onDelete,
}: {
  conversations: Conversation[];
  openId: string | null;
  /** True while a run is going: switching conversations mid-run would confuse the reply. */
  disabled: boolean;
  onOpen: (id: string) => void;
  onNew: () => void;
  onRename: (id: string, title: string) => Promise<void>;
  onDelete: (conversation: Conversation) => void;
}) {
  const [editing, setEditing] = useState<{ id: string; title: string } | null>(
    null,
  );

  async function saveTitle(event: FormEvent) {
    event.preventDefault();
    if (!editing) return;
    const title = editing.title.trim();
    if (title) await onRename(editing.id, title);
    setEditing(null);
  }

  return (
    <section className="conversations" aria-label="Conversations">
      <div className="conversations-head">
        <p className="side-kicker">CONVERSATIONS</p>
        <button
          type="button"
          className="secondary-button small"
          onClick={onNew}
          disabled={disabled || openId === null}
        >
          New conversation
        </button>
      </div>
      {conversations.length === 0 ? (
        <p className="conversations-empty">
          Your conversations are kept on this computer, so a follow-up like
          “actually, 30 minutes later” still makes sense tomorrow.
        </p>
      ) : (
        <ul>
          {conversations.map((conversation) => {
            const open = conversation.id === openId;
            if (editing?.id === conversation.id)
              return (
                <li key={conversation.id} className="conversation-row editing">
                  <form onSubmit={saveTitle}>
                    <input
                      autoFocus
                      maxLength={120}
                      value={editing.title}
                      aria-label="Conversation name"
                      onChange={(event) =>
                        setEditing({ ...editing, title: event.target.value })
                      }
                      onKeyDown={(event) => {
                        if (event.key === "Escape") setEditing(null);
                      }}
                    />
                    <button className="secondary-button small">Save</button>
                  </form>
                </li>
              );
            return (
              <li
                key={conversation.id}
                className={open ? "conversation-row open" : "conversation-row"}
              >
                <button
                  type="button"
                  className="conversation-open"
                  onClick={() => onOpen(conversation.id)}
                  aria-current={open ? "true" : undefined}
                  disabled={disabled && !open}
                >
                  <span className="conversation-title">
                    {conversation.title}
                  </span>
                  <small>
                    {conversationDateLabel(conversation.updatedAt)} ·{" "}
                    {conversation.messageCount} message
                    {conversation.messageCount === 1 ? "" : "s"}
                  </small>
                </button>
                <span className="conversation-actions">
                  <button
                    type="button"
                    className="conversation-action"
                    onClick={() =>
                      setEditing({
                        id: conversation.id,
                        title: conversation.title,
                      })
                    }
                    disabled={disabled}
                    aria-label={`Rename “${conversation.title}”`}
                  >
                    Rename
                  </button>
                  <button
                    type="button"
                    className="conversation-action danger"
                    onClick={() => onDelete(conversation)}
                    disabled={disabled}
                    aria-label={`Delete “${conversation.title}”`}
                  >
                    Delete
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

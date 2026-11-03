import { FormEvent, RefObject, useState } from "react";
import { Board, BoardCard, cardTime, doneLimit } from "./assistantTasks";
import { appName } from "./brand";
import { plural } from "./planning";
import "./TasksView.css";

/**
 * The Tasks board from the redesign: what was handed to the assistant, sorted into what needs the
 * person, what is running, and what is done. Reviewing and answering happen on the Assistant,
 * where every change is shown one by one before anything is applied.
 */
export function TasksView({
  board,
  headingRef,
  canStart,
  onStart,
  onReview,
  onAnswer,
  onRetry,
  onStop,
}: {
  board: Board;
  headingRef?: RefObject<HTMLHeadingElement>;
  /** False while a request is already running: the assistant takes one at a time. */
  canStart: boolean;
  onStart: (request: string) => void;
  onReview: (runKey: string) => void;
  onAnswer: (runKey: string) => void;
  onRetry: (request: string) => void;
  onStop: () => void;
}) {
  const [request, setRequest] = useState("");

  function submit(event: FormEvent) {
    event.preventDefault();
    const text = request.trim();
    if (!text || !canStart) return;
    onStart(text);
    setRequest("");
  }

  return (
    <div className="tasks-board">
      <header className="tasks-header">
        <div>
          <h1 ref={headingRef} tabIndex={-1}>
            Tasks
          </h1>
          <p>
            Everything you&rsquo;ve handed to {appName}, and what it needs from
            you.
          </p>
        </div>
        <form className="tasks-new" onSubmit={submit}>
          <label htmlFor="tasks-new-request" className="visually-hidden">
            New task for {appName}
          </label>
          <input
            id="tasks-new-request"
            type="text"
            maxLength={2000}
            value={request}
            onChange={(event) => setRequest(event.target.value)}
            placeholder={`Hand ${appName} a task…`}
          />
          <button
            className="primary-button"
            disabled={!request.trim() || !canStart}
            title={canStart ? undefined : "Wait for the current task to finish"}
          >
            Start
          </button>
        </form>
      </header>

      <div className="tasks-columns">
        <Column
          id="needs"
          title="Needs you"
          count={board.needsYou.length}
          empty="Nothing needs you. Questions and changes to approve show up here."
        >
          {board.needsYou.map((card) => (
            <Card
              key={card.runKey}
              card={card}
              onReview={onReview}
              onAnswer={onAnswer}
              onRetry={onRetry}
              onStop={onStop}
            />
          ))}
        </Column>
        <Column
          id="working"
          title="Working"
          count={board.working.length}
          empty={`${appName} isn't working on anything right now.`}
        >
          {board.working.map((card) => (
            <Card
              key={card.runKey}
              card={card}
              onReview={onReview}
              onAnswer={onAnswer}
              onRetry={onRetry}
              onStop={onStop}
            />
          ))}
        </Column>
        <Column
          id="done"
          title="Done"
          count={board.done.length}
          empty="Finished tasks from this session show up here."
          footnote={
            board.done.length >= doneLimit
              ? "Older tasks are in your conversations on the Assistant."
              : undefined
          }
        >
          {board.done.map((card) => (
            <Card
              key={card.runKey}
              card={card}
              onReview={onReview}
              onAnswer={onAnswer}
              onRetry={onRetry}
              onStop={onStop}
            />
          ))}
        </Column>
      </div>
    </div>
  );
}

function Column({
  id,
  title,
  count,
  empty,
  footnote,
  children,
}: {
  id: "needs" | "working" | "done";
  title: string;
  count: number;
  empty: string;
  footnote?: string;
  children: React.ReactNode;
}) {
  return (
    <section
      className={`tasks-column ${id}`}
      aria-labelledby={`tasks-col-${id}`}
    >
      <div className="tasks-column-heading">
        <span className="tasks-column-dot" aria-hidden="true" />
        <h2 id={`tasks-col-${id}`}>{title}</h2>
        <span className="tasks-column-count">{count}</span>
      </div>
      {count === 0 ? <p className="tasks-empty">{empty}</p> : children}
      {footnote && <p className="tasks-footnote">{footnote}</p>}
    </section>
  );
}

function Card({
  card,
  onReview,
  onAnswer,
  onRetry,
  onStop,
}: {
  card: BoardCard;
  onReview: (runKey: string) => void;
  onAnswer: (runKey: string) => void;
  onRetry: (request: string) => void;
  onStop: () => void;
}) {
  switch (card.kind) {
    case "question":
      return (
        <article className="task-card">
          <CardTop tag="Question" tone="question" at={card.at} />
          <h3>“{card.title}”</h3>
          <p>{card.text}</p>
          <button
            className="primary-button small"
            onClick={() => onAnswer(card.runKey)}
          >
            Answer
          </button>
        </article>
      );
    case "review":
      return (
        <article className="task-card">
          <CardTop
            tag={`${plural(card.changes, "change")} ready`}
            tone="ready"
            at={card.at}
          />
          <h3>{card.title}</h3>
          <p>{card.summary}</p>
          <button
            className="primary-button small"
            onClick={() => onReview(card.runKey)}
          >
            {card.changes === 1 ? "Review change" : "Review changes"}
          </button>
        </article>
      );
    case "working":
      return (
        <article className="task-card" aria-busy="true">
          <CardTop
            tag={card.steps === 0 ? "Starting" : "Working"}
            tone="ready"
            at={card.at}
          />
          <h3>{card.title}</h3>
          <div className="task-progress" aria-hidden="true">
            <i />
          </div>
          <p className="task-step">
            {card.lastStep ??
              "Reading your planner. Nothing changes until you approve it."}
          </p>
          <button className="secondary-button small" onClick={onStop}>
            Stop
          </button>
        </article>
      );
    case "done":
      return (
        <article className="task-card done">
          <CardTop tag={card.label} tone={card.tone} at={card.at} />
          <h3>{card.title}</h3>
          {card.text && <p className="task-answer">{card.text}</p>}
          {card.retry && (
            <button
              className="secondary-button small"
              onClick={() => onRetry(card.title)}
            >
              Try again
            </button>
          )}
        </article>
      );
  }
}

function CardTop({
  tag,
  tone,
  at,
}: {
  tag: string;
  tone: "question" | "ready" | "good" | "attention" | "neutral" | "problem";
  at: string;
}) {
  return (
    <div className="task-card-top">
      <span className={`task-tag ${tone}`}>{tag}</span>
      <time dateTime={at}>{cardTime(at)}</time>
    </div>
  );
}

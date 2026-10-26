import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, MouseEvent } from "react";
import type { Answer, AnswerOption, Command, CommandOutcome, DecisionCard as Card, Origin } from "../generated/api";
import { problemText } from "../client";
import { labels, words } from "../labels";
import { enterNeverDecides, originOf } from "../origin";
import { visible, visibleJson } from "../safe";
import { TAKE_OVER_TOOL } from "../takeOver";
import { AgentCard } from "../agents/AgentCard";
import type { AgentCardView } from "../agents/types";
import { Avatar } from "./Avatar";
import { CallView } from "./CallView";
import { DECIDED_CLASS, DecidedLine } from "./DecidedLine";
import { Pill } from "./Pill";

/** The reserved tool name of a take-over entry (spec 3.3 §10 H2). */
export { TAKE_OVER_TOOL };

/**
 * Typing a secret into the tool that asked for it (spec 3.3 §8.2 "Take over", §10 H2):
 * `HttpClient.takeOver()` sends it to `POST /api/surface/decisions/{id}/take-over`, for the
 * card at the version shown, and answers as a decision does.
 */
export interface TakeOverSeam {
  provide(card: Card, value: string, origin: Origin): Promise<CommandOutcome>;
}

export type Send = (command: Command, origin: Origin) => Promise<CommandOutcome>;

/**
 * A decision, drawn from its inbox entry (spec 3.3 §7.1, S-I1): who asks and since when, what
 * will happen, the exact call, how it got here, the answers this surface may give (§7.2) and
 * the details. It sits on its own background. Every answer carries the version this card
 * showed (S-I2); an answer someone else gave first is shown, never retried. Enter and Esc
 * never decide (§7.4). Once decided, from any surface, it is a `DecidedLine` (§5.1).
 */
export function DecisionCard({
  card: given,
  send,
  onSettled,
  takeOver,
  focusOnMount = false,
}: {
  card: Card;
  send: Send;
  onSettled?: (card: Card) => void;
  takeOver?: TakeOverSeam;
  focusOnMount?: boolean;
}) {
  const [card, setCard] = useState(given);
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  // The answer was given on this card: its line takes the focus the answer buttons had.
  const [answeredHere, setAnsweredHere] = useState(false);
  // Opened at a decision already decided (a deep link): its line takes the focus. One decided
  // later, elsewhere, never does: the person may be typing (§5.1).
  const [decidedWhenOpened] = useState(given.status === "decided");
  const heading = useId();
  const root = useRef<HTMLElement>(null);

  // A decided card from the feed replaces this one. A newer waiting version does not: the
  // answer carries the version the person saw, and the engine says when it changed (S-I2).
  useEffect(() => {
    setCard((current) => (given.id !== current.id || given.status === "decided" ? given : current));
  }, [given]);

  // A waiting card takes the focus itself; a decided one gives it to its line (`DecidedLine`).
  useEffect(() => {
    if (focusOnMount && root.current?.dataset.decision === "waiting") root.current.focus();
  }, [focusOnMount]);

  const decided = card.status === "decided";
  const isTakeOver = card.call.tool === TAKE_OVER_TOOL;
  const shownInFull = !card.call.truncated || expanded;

  function settle(outcome: CommandOutcome) {
    if (outcome.type === "answered" || outcome.type === "alreadyDecided") {
      setCard(outcome.card);
      setEditing(false);
      setAnsweredHere(true);
      onSettled?.(outcome.card);
    } else if (outcome.type === "changed") {
      setCard(outcome.card);
      setNote(labels.changedSinceSeen);
    }
  }

  async function answer(option: AnswerOption, origin: Origin, editedArgs?: unknown) {
    setBusy(true);
    setProblem("");
    setNote("");
    try {
      const outcome = await send(
        {
          type: "decide",
          decisionId: card.id,
          version: card.version,
          answer: option.answer,
          expanded: shownInFull,
          ...(editedArgs === undefined ? {} : { editedArgs }),
        },
        origin,
      );
      settle(outcome);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  async function takeOverWith(seam: TakeOverSeam, value: string, origin: Origin) {
    setBusy(true);
    setProblem("");
    setNote("");
    try {
      settle(await seam.provide(card, value, origin));
      return true;
    } catch (error) {
      setProblem(problemText(error));
      return false;
    } finally {
      setBusy(false);
    }
  }

  function click(option: AnswerOption) {
    return (event: MouseEvent<HTMLButtonElement>) => {
      const origin = originOf(event);
      if (option.answer === "edit") {
        setEditing(true);
        return;
      }
      void answer(option, origin);
    };
  }

  const options = card.allowed.options.filter(
    (option) => !isTakeOver || option.answer === "deny",
  );

  // Who asks, the exact call, how it got here and the details: the card, and what its line
  // shows again once it is decided.
  const who = (
    <header className="decision-who">
      <Avatar agent={card.agent} size="sm" />
      <span>
        <strong>{visible(card.agent.displayName)}</strong> · {card.sinceText}
      </span>
      <Pill kind={decided ? "decided" : "waiting"} dot={!decided}>
        {card.statusText}
      </Pill>
    </header>
  );
  const summary = card.summary ? <p className="decision-note">{visible(card.summary)}</p> : null;
  // A suggested agent shows its definition in place of the call (spec 3.3 §5.7).
  const addsAgent = (card as Card & { addsAgent?: AgentCardView | null }).addsAgent;
  const call = addsAgent ? (
    <AgentCard view={addsAgent} decisionId={card.id} expanded={expanded} onExpand={() => setExpanded(true)} decided={decided} />
  ) : (
    <CallView call={card.call} expanded={expanded} onExpand={() => setExpanded(true)} />
  );
  const timeline = (
    <ol className="timeline" aria-label={labels.howItGotHere}>
      {card.timeline.map((step, index) => (
        <li key={index} className={step.current ? "current" : undefined} aria-current={step.current ? "step" : undefined}>
          {visible(step.text)}
        </li>
      ))}
    </ol>
  );
  const details = (
    <p className="decision-details">
      <span>{card.riskText}</span>
      <span>{card.reversibilityText}</span>
      {card.alsoShown && <span>{visible(card.alsoShown)}</span>}
    </p>
  );
  // A feature's line under the details: where the call comes from, with a link into the home
  // only to a path on this server (spec 3.3 §7.1).
  const href = card.detail?.href;
  const link = card.detail?.linkText && href?.startsWith("/") && !href.startsWith("//") ? href : undefined;
  const detail = card.detail ? (
    <p className="decision-detail">
      {visible(card.detail.text)}
      {link && card.detail.linkText && (
        <>
          {" · "}
          <a href={link}>{visible(card.detail.linkText)}</a>
        </>
      )}
    </p>
  ) : null;

  // Decided here or anywhere else, the card keeps its place in the thread as one line; opened,
  // it shows the call and the details again, and nothing on it answers (spec 3.3 §5.1, §7.4).
  if (decided) {
    return (
      // `tabIndex` as the waiting card has it: a focus on the card stays on it.
      <article
        ref={root}
        tabIndex={-1}
        className={`${DECIDED_CLASS} decision-line`}
        aria-labelledby={heading}
        data-decision="decided"
      >
        <DecidedLine card={card} headingId={heading} focus={answeredHere || (focusOnMount && decidedWhenOpened)}>
          {who}
          {summary}
          {call}
          {timeline}
          {details}
          {detail}
        </DecidedLine>
      </article>
    );
  }

  return (
    <article ref={root} tabIndex={-1} className="decision" aria-labelledby={heading} data-decision="waiting">
      {who}
      <h3 id={heading} className="decision-heading">
        {visible(card.heading)}
      </h3>
      {summary}
      {call}
      {timeline}
      {card.allowed.note && <p className="decision-note">{visible(card.allowed.note)}</p>}
      {note && (
        <p className="decision-note" role="status">
          {note}
        </p>
      )}
      {(options.length > 0 || (isTakeOver && takeOver)) && !editing && (
        <div className="decision-actions">
          {isTakeOver && takeOver && (
            <TakeOverButton
              heading={visible(card.heading)}
              busy={busy}
              onDone={(value, origin) => takeOverWith(takeOver, value, origin)}
            />
          )}
          {options.map((option) => (
            <button
              key={option.answer}
              type="button"
              className={buttonClass(option.answer)}
              disabled={busy || (option.needsExpanded && !expanded)}
              onClick={click(option)}
              onKeyDown={enterNeverDecides}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
      {editing && (
        <EditCall
          card={card}
          busy={busy}
          onCancel={() => setEditing(false)}
          onAllow={(args, origin) => {
            const option = card.allowed.options.find((candidate) => candidate.answer === "edit");
            if (option) void answer(option, origin, args);
          }}
        />
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {details}
      {detail}
    </article>
  );
}

function buttonClass(answer: Answer) {
  switch (answer) {
    case "allowOnce":
      return "button button-primary";
    case "alwaysAllow":
    case "edit":
      return "button button-secondary";
    case "deny":
      return "button button-quiet";
  }
}

/** "Edit": change the arguments, then allow (spec 3.3 §8.2). Esc closes it; nothing is decided. */
function EditCall({
  card,
  busy,
  onCancel,
  onAllow,
}: {
  card: Card;
  busy: boolean;
  onCancel: () => void;
  onAllow: (args: unknown, origin: Origin) => void;
}) {
  // Escaped as JSON, so what is edited reads as it runs and parses back unchanged (S-I9).
  const [text, setText] = useState(() => visibleJson(JSON.stringify(card.call.args ?? {}, null, 2)));
  const [invalid, setInvalid] = useState(false);
  const field = useId();
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => area.current?.focus(), []);

  function escape(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  }

  return (
    <div className="edit" onKeyDown={escape}>
      <label htmlFor={field} className="edit-label">
        {labels.editTitle}
      </label>
      <p className="edit-hint">{labels.editHint}</p>
      <textarea
        id={field}
        ref={area}
        className="edit-area"
        spellCheck={false}
        value={text}
        rows={Math.min(14, text.split("\n").length + 1)}
        onChange={(event) => {
          setText(event.target.value);
          setInvalid(false);
        }}
      />
      {invalid && (
        <p className="decision-problem" role="alert">
          {labels.notJson}
        </p>
      )}
      <div className="decision-actions">
        <button
          type="button"
          className="button button-primary"
          disabled={busy}
          onKeyDown={enterNeverDecides}
          onClick={(event) => {
            let args: unknown;
            try {
              args = JSON.parse(text);
            } catch {
              setInvalid(true);
              return;
            }
            onAllow(args, originOf(event));
          }}
        >
          {labels.editAllow}
        </button>
        <button type="button" className="button button-quiet" onClick={onCancel}>
          {labels.cancel}
        </button>
      </div>
    </div>
  );
}

/** "Take over", then "Done": the person types the secret; Enter never sends it (§7.4). */
function TakeOverButton({
  heading,
  busy,
  onDone,
}: {
  heading: string;
  busy: boolean;
  onDone: (value: string, origin: Origin) => Promise<boolean>;
}) {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const field = useId();
  if (!open) {
    return (
      <button type="button" className="button button-primary" onKeyDown={enterNeverDecides} onClick={() => setOpen(true)}>
        {words.TAKE_OVER}
      </button>
    );
  }
  return (
    <span className="take-over">
      <label htmlFor={field} className="visually-hidden">
        {heading}
      </label>
      <input
        id={field}
        type="password"
        autoComplete="off"
        placeholder={labels.typeItHere}
        value={value}
        onChange={(event) => setValue(event.target.value)}
      />
      <button
        type="button"
        className="button button-primary"
        disabled={busy || value.length === 0}
        onKeyDown={enterNeverDecides}
        onClick={async (event) => {
          if (await onDone(value, originOf(event))) setValue("");
        }}
      >
        {words.TAKE_OVER_DONE}
      </button>
    </span>
  );
}

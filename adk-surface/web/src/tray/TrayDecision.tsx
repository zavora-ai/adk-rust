import { useEffect, useId, useState } from "react";
import type { MouseEvent } from "react";
import type { AnswerOption, Command, CommandOutcome, DecisionCard, Origin } from "../generated/api";
import { problemText } from "../client";
import { Avatar } from "../components/Avatar";
import { enterNeverDecides, originOf } from "../origin";
import { decisionPath } from "../route";
import { visible } from "../safe";
import { trayLabels } from "./labels";

/**
 * The answers the tray gives `card` itself (spec 3.3 §7.2 tray row): Allow once and Deny as the
 * engine offers them, but never one that needs the call expanded or the person to confirm it
 * is them. Saying no needs neither, so a high-risk card may still offer Deny.
 */
export function trayAnswers(card: DecisionCard): AnswerOption[] {
  if (card.status !== "waiting") return [];
  return card.allowed.options.filter(
    (option) =>
      (option.answer === "allowOnce" || option.answer === "deny") &&
      !option.needsExpanded &&
      !option.needsPresence &&
      !(option.answer === "allowOnce" && card.call.truncated),
  );
}

/**
 * A decision on the tray (spec 3.3 §5.5, §7.1, §7.2): drawn from its inbox entry with who
 * asks, the exact call in full and whether it can be taken back. The tray says yes once or no
 * to everyday calls; approving anything else — always allow, edit, high risk, money, a call
 * shown cut short — opens the home at the decision ("open to decide"). Enter and Esc never
 * decide (§7.4); ids and versions are never shown.
 */
export function TrayDecision({
  card: given,
  send,
  onSettled,
}: {
  card: DecisionCard;
  send: (command: Command, origin: Origin) => Promise<CommandOutcome>;
  onSettled: (card: DecisionCard) => void;
}) {
  const [card, setCard] = useState(given);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const [problem, setProblem] = useState("");
  const heading = useId();

  // A newer waiting version never replaces the card the person is looking at: the answer
  // carries the version shown, and the engine says when it changed (S-I2).
  useEffect(() => {
    setCard((current) => (given.id !== current.id || given.status === "decided" ? given : current));
  }, [given]);

  const decided = card.status === "decided";
  const options = trayAnswers(card);
  // Approving happens here only for an everyday call shown in full; otherwise in the home.
  const approvesHere = options.some((option) => option.answer === "allowOnce");
  const home = decisionPath(card.id);

  async function answer(option: AnswerOption, event: MouseEvent<HTMLButtonElement>) {
    const origin = originOf(event);
    setBusy(true);
    setNote("");
    setProblem("");
    try {
      // The tray shows the call in full, so the answer says it was expanded (§7.1).
      const outcome = await send(
        { type: "decide", decisionId: card.id, version: card.version, answer: option.answer, expanded: true },
        origin,
      );
      if (outcome.type === "answered" || outcome.type === "alreadyDecided") {
        setCard(outcome.card);
        onSettled(outcome.card);
      } else if (outcome.type === "changed") {
        setCard(outcome.card);
        setNote(trayLabels.changed);
      }
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  const agent = visible(card.agent.displayName);
  return (
    <article
      className={`tray-card${decided ? " tray-card-decided" : ""}`}
      aria-labelledby={heading}
      data-decision={decided ? "decided" : "waiting"}
    >
      <div className="tray-card-top">
        <Avatar agent={card.agent} size="md" />
        <div className="tray-card-text">
          <h3 id={heading} className="tray-card-heading">
            <a href={home} target="_blank" rel="noopener">
              {visible(card.heading)}
            </a>
          </h3>
          {approvesHere || decided ? (
            <p className="tray-card-meta">
              {agent} · {card.sinceText} · {card.riskText}
            </p>
          ) : (
            <p className="tray-card-meta tray-card-open">
              {agent} · {card.riskText} · {trayLabels.openToDecide}
            </p>
          )}
        </div>
        <a className="tray-card-chevron" href={home} target="_blank" rel="noopener" aria-label={trayLabels.openInHome}>
          <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="m9 6 6 6-6 6" />
          </svg>
        </a>
      </div>
      <code className="tray-call" aria-label={trayLabels.exactCall}>
        {visible(card.call.text)}
      </code>
      <p className="tray-card-details">{card.reversibilityText}</p>
      {decided && card.decided && (
        <p className="tray-card-decided-text" role="status">
          {visible(card.decided.text)}
        </p>
      )}
      {note && (
        <p className="tray-card-note" role="status">
          {note}
        </p>
      )}
      {options.length > 0 && (
        <div className="tray-card-actions">
          {options.map((option) => (
            <button
              key={option.answer}
              type="button"
              className={option.answer === "allowOnce" ? "button button-primary" : "button button-secondary"}
              disabled={busy}
              onKeyDown={enterNeverDecides}
              onClick={(event) => void answer(option, event)}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
      {problem && (
        <p className="tray-problem" role="alert">
          {problem}
        </p>
      )}
    </article>
  );
}

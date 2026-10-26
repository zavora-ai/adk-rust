import { useId } from "react";
import type { DecisionCard } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { DECIDED_CLASS, DecidedLine } from "../components/DecidedLine";
import { Pill } from "../components/Pill";
import { visible } from "../safe";
import { phoneLabels } from "./labels";

/** "Can't be undone · needs your face or fingerprint": what a waiting card needs here. */
export function needsLine(card: DecisionCard): string {
  const presence = card.allowed.options.some((option) => option.needsPresence);
  return presence ? `${card.reversibilityText} · ${phoneLabels.needsFaceOrFingerprint}` : card.reversibilityText;
}

/** "You said yes in Telegram at 07:31 · done once". */
export function decidedLine(card: DecisionCard): string {
  const text = card.decided?.text ?? card.statusText;
  const scope = card.decided?.scopeText;
  return scope ? `${text} · ${scope}` : text;
}

/**
 * The phone's Decisions (spec 3.3 §5.4, §7.1): the waiting decisions first, each on the
 * decision background, then the ones decided lately, here or elsewhere, each one line that
 * opens to show the exact command. Questions wait for a later phase: the badge counts
 * decisions only.
 */
export function Decisions({
  cards,
  onOpen,
}: {
  cards: DecisionCard[];
  onOpen: (card: DecisionCard) => void;
}) {
  const waiting = cards.filter((card) => card.status === "waiting");
  const decided = cards.filter((card) => card.status === "decided");
  if (!cards.length) return <p className="phone-empty">{phoneLabels.nothingWaiting}</p>;
  return (
    <ul className="phone-cards" aria-label={phoneLabels.decisions}>
      {waiting.map((card) => (
        <li key={card.id}>
          <button type="button" className="phone-card phone-card-waiting" onClick={() => onOpen(card)}>
            <span className="phone-card-who">
              <Avatar agent={card.agent} size="sm" />
              <strong>{visible(card.agent.displayName)}</strong>
              <Pill kind={card.risk === "high" ? "waiting" : "neutral"} dot={card.risk === "high"}>
                {card.riskText}
              </Pill>
            </span>
            <span className="phone-card-heading">{visible(card.heading)}</span>
            <span className="phone-card-line">{needsLine(card)}</span>
          </button>
        </li>
      ))}
      {decided.map((card) => (
        <li key={card.id}>
          <Decided card={card} />
        </li>
      ))}
    </ul>
  );
}

/** A decision answered lately, here or elsewhere: one line, opened in place. */
function Decided({ card }: { card: DecisionCard }) {
  const heading = useId();
  return (
    <article className={DECIDED_CLASS} aria-labelledby={heading}>
      <DecidedLine card={card} headingId={heading} said={decidedLine(card)}>
        <span className="phone-card-who">
          <Avatar agent={card.agent} size="sm" />
          <strong>{visible(card.agent.displayName)}</strong> · {card.sinceText}
        </span>
        <pre className="phone-decided-call">
          <code>{visible(card.call.text)}</code>
        </pre>
        <p className="phone-card-line">
          {card.riskText} · {card.reversibilityText}
        </p>
      </DecidedLine>
    </article>
  );
}

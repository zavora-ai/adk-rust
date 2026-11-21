import { useState } from "react";
import type { DecisionCard as Card } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { DecisionCard } from "../components/DecisionCard";
import type { Send, TakeOverSeam } from "../components/DecisionCard";
import { labels } from "../labels";
import { visible } from "../safe";

/**
 * Decisions (spec 3.3 §5.1): everything waiting for the person in one place. The list, which
 * can be narrowed to who asks, sits beside the selected decision's own card, answered there as
 * in its thread: the card is the only place an answer is given, and Enter on the list only
 * opens one (§7.4).
 */
export function DecisionsInbox({
  decisions,
  send,
  onSettled,
  takeOver,
  onOpenThread,
}: {
  decisions: Card[];
  send: Send;
  onSettled: (card: Card) => void;
  takeOver?: TakeOverSeam;
  /** Opens a decision in its own thread. */
  onOpenThread: (card: Card) => void;
}) {
  const [who, setWho] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const waiting = decisions.filter((card) => card.status === "waiting");
  const askers = [...new Map(waiting.map((card) => [card.agent.name, card.agent])).values()];
  const shown = who ? waiting.filter((card) => card.agent.name === who) : waiting;
  const current = shown.find((card) => card.id === selected) ?? shown[0];

  return (
    <div className="inbox">
      <section className="inbox-list" aria-labelledby="inbox-title">
        <header className="inbox-head">
          <h1 id="inbox-title">{labels.decisions}</h1>
          {waiting.length > 0 && <span className="inbox-count">{labels.waitingCount(waiting.length)}</span>}
        </header>
        {askers.length > 1 && (
          <div className="inbox-filter" role="group" aria-label={labels.filterByWho}>
            <button type="button" aria-pressed={who === undefined} onClick={() => setWho(undefined)}>
              {labels.everyone}
            </button>
            {askers.map((agent) => (
              <button key={agent.name} type="button" aria-pressed={who === agent.name} onClick={() => setWho(agent.name)}>
                {visible(agent.displayName)}
                <span className="inbox-filter-count">{waiting.filter((card) => card.agent.name === agent.name).length}</span>
              </button>
            ))}
          </div>
        )}
        {shown.length === 0 ? (
          <p className="inbox-empty">{labels.nothingWaits}</p>
        ) : (
          <ul className="inbox-items" aria-label={labels.waitingDecisions}>
            {shown.map((card) => (
              <li key={card.id}>
                <button
                  type="button"
                  className={`inbox-item${card.risk === "high" ? " inbox-item-high" : ""}`}
                  aria-current={card === current ? "true" : undefined}
                  onClick={() => setSelected(card.id)}
                >
                  <Avatar agent={card.agent} size="sm" />
                  <span className="inbox-item-text">
                    <span className="inbox-item-heading">{visible(card.heading)}</span>
                    <span className="inbox-item-meta">
                      {visible(card.agent.displayName)} · {card.sinceText} · {card.riskText}
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="inbox-detail" aria-label={labels.selectedDecision}>
        {current ? (
          <>
            <DecisionCard key={current.id} card={current} send={send} onSettled={onSettled} takeOver={takeOver} />
            <button type="button" className="link inbox-thread" onClick={() => onOpenThread(current)}>
              {labels.openInThread}
            </button>
          </>
        ) : (
          <p className="inbox-empty">{labels.caughtUp}</p>
        )}
      </section>
    </div>
  );
}

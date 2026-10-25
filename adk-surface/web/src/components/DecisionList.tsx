import type { DecisionCard } from "../generated/api";
import { labels } from "../labels";
import { visible } from "../safe";

/** "Decisions" with a filled count badge (spec 3.3 §5.1, §7.1): every open decision. */
export function DecisionList({
  decisions,
  onOpen,
}: {
  decisions: DecisionCard[];
  onOpen: (card: DecisionCard) => void;
}) {
  return (
    <section className="side-section" aria-labelledby="decisions-heading">
      <h2 id="decisions-heading" className="side-heading">
        {labels.decisions}
        {decisions.length > 0 && (
          <span className="badge" aria-label={`${decisions.length} waiting`}>
            {decisions.length}
          </span>
        )}
      </h2>
      {decisions.length === 0 && <p className="side-empty">{labels.nothingWaits}</p>}
      <ul className="mini-decisions">
        {decisions.map((card) => (
          <li key={card.id}>
            <button type="button" className="mini-decision" onClick={() => onOpen(card)}>
              <span className="mini-heading">{visible(card.heading)}</span>
              <span className={`mini-meta${card.risk === "high" || card.money ? " mini-high" : ""}`}>
                {visible(card.agent.displayName)} · {card.riskText}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

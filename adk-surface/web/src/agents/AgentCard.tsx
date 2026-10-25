import { Avatar } from "../components/Avatar";
import { visible } from "../safe";
import { agentLabels as l } from "./labels";
import { goTo, newAgentPath } from "./route";
import type { AgentCardView } from "./types";
import "./agents.css";

/**
 * The definition on the decision that adds a suggested agent (spec 3.3 §5.7, S-I13), in place
 * of the call: every field from the inbox entry the kernel wrote, the instructions as quoted
 * text — expanded before the agent can be added when they are long — what it can do, what it
 * would be allowed without asking, and **Change it first**, which opens **Set it up yourself**
 * holding the suggestion, where the surface changes agents (the phone does not).
 */
export function AgentCard({
  view,
  decisionId,
  expanded,
  onExpand,
  decided = false,
  changeable = true,
}: {
  view: AgentCardView;
  decisionId: string;
  expanded: boolean;
  onExpand: () => void;
  decided?: boolean;
  changeable?: boolean;
}) {
  const instructions = view.instructionsTruncated && expanded ? view.draft.instructions : view.instructions;
  return (
    <div className="agent-card">
      <div className="agent-card-who">
        <Avatar agent={view.agent} size="sm" />
        <span>
          <strong>{visible(view.agent.displayName)}</strong>
          {view.description && <span className="muted"> · {visible(view.description)}</span>}
        </span>
      </div>
      <dl className="agent-card-lines">
        {view.lines.map((line) => (
          <div key={line.label} className="agent-card-line">
            <dt>{line.label}</dt>
            <dd className={line.mono ? "mono" : undefined}>{visible(line.value)}</dd>
          </div>
        ))}
      </dl>
      {instructions && (
        <figure className="agent-card-instructions">
          <figcaption className="card-label">{l.instructions}</figcaption>
          <blockquote>{visible(instructions)}</blockquote>
          {view.instructionsTruncated && (
            <button type="button" className="link" disabled={expanded} onClick={onExpand}>
              {expanded ? l.instructionsShown : l.instructionsCutShort}
            </button>
          )}
        </figure>
      )}
      <div>
        <p className="card-label">{l.whatItCanDo}</p>
        <ul className="agent-abilities">
          {view.abilities.map((ability, index) => (
            <li key={index}>{visible(ability)}</li>
          ))}
        </ul>
        {view.compared && <p className="muted">{visible(view.compared)}</p>}
      </div>
      <p>
        <span className="card-label">{l.allowedWithoutAsking}</span> {view.allowedWithoutAsking}
      </p>
      {!decided && changeable && (
        <button type="button" className="button button-secondary agent-change-first" onClick={() => goTo(newAgentPath(decisionId))}>
          {l.changeItFirst}
        </button>
      )}
    </div>
  );
}

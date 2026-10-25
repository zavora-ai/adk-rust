import type { AgentRow } from "../generated/api";
import { RosterFoot, RowSettings } from "../agents/RosterFoot";
import { labels } from "../labels";
import { Avatar } from "./Avatar";

/** The agents, each with its avatar, name and status word (spec 3.3 §5.1, §8.1). */
export function Roster({
  agents,
  selected,
  onSelect,
  canAdd = false,
}: {
  agents: AgentRow[];
  selected?: string;
  onSelect: (name: string) => void;
  /** The kernel keeps agents a person adds: **New agent** ends the list (spec 3.3 §5.7). */
  canAdd?: boolean;
}) {
  return (
    <nav className="roster" aria-label={labels.agents}>
      <h2 className="rail-label">{labels.agents}</h2>
      {agents.length === 0 && <p className="rail-empty">{labels.noAgents}</p>}
      <ul>
        {agents.map((row) => (
          <li key={row.agent.name}>
            <button
              type="button"
              className="roster-row"
              aria-current={row.agent.name === selected ? "true" : undefined}
              onClick={() => onSelect(row.agent.name)}
            >
              <Avatar agent={row.agent} />
              <span className="roster-text">
                <span className="roster-name">{row.agent.displayName}</span>
                <span className={`status status-${row.status}`}>{row.statusText}</span>
              </span>
            </button>
            {canAdd && <RowSettings row={row} />}
          </li>
        ))}
        <RosterFoot rows={agents} canAdd={canAdd} />
      </ul>
    </nav>
  );
}

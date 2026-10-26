import { Fragment } from "react";
import type { AgentRow, RemovedAgent } from "../generated/api";
import { RemovedAgents } from "../agents/RemovedAgents";
import { RosterFoot, RowSettings } from "../agents/RosterFoot";
import { labels } from "../labels";
import { Avatar } from "./Avatar";

/**
 * The agents, each with its avatar, name and status word (spec 3.3 §5.1, §8.1). The person's
 * own agents sit under their area when there is more than one (§5.7 "The roster"); the engine
 * lists them in that order. "Removed agents" at the foot opens their threads read only.
 */
export function Roster({
  agents,
  selected,
  onSelect,
  canAdd = false,
  removed = [],
  reading,
  onRead,
}: {
  agents: AgentRow[];
  selected?: string;
  onSelect: (name: string) => void;
  /** The kernel keeps agents a person adds: **New agent** ends the list (spec 3.3 §5.7). */
  canAdd?: boolean;
  /** The agents the person removed, with their threads. */
  removed?: RemovedAgent[];
  /** The removed agent's thread being read, if one is. */
  reading?: string;
  onRead?: (agent: RemovedAgent, session: string) => void;
}) {
  const areas = new Set(agents.filter((row) => row.added && row.area).map((row) => row.area));
  const grouped = areas.size > 1;
  return (
    <nav className="roster" aria-label={labels.agents}>
      <h2 className="rail-label">{labels.agents}</h2>
      {agents.length === 0 && <p className="rail-empty">{labels.noAgents}</p>}
      <ul>
        {agents.map((row, index) => {
          const area =
            grouped && row.added && row.area && row.area !== agents[index - 1]?.area ? row.area : undefined;
          return (
            <Fragment key={row.agent.name}>
              {area && <li className="roster-area">{area}</li>}
              <li>
                <button
                  type="button"
                  className="roster-row"
                  aria-current={row.agent.name === selected && !reading ? "true" : undefined}
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
            </Fragment>
          );
        })}
        <RosterFoot rows={agents} canAdd={canAdd} />
        {onRead && <RemovedAgents removed={removed} reading={reading} onRead={onRead} />}
      </ul>
    </nav>
  );
}

import type { AgentRow } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { visible } from "../safe";
import { agentLabels as l } from "./labels";
import { agentSettingsPath, goTo } from "./route";
import "./agents.css";

/**
 * Every agent in "What your agents can do" (spec 3.3 §5.7, Memory & rules): its area, how much
 * it asks first — its own for an agent the person added, else `mode`, the one in force — and
 * **Settings**, which opens its settings.
 */
export function AgentsCanDo({ agents, mode }: { agents: AgentRow[]; mode?: string }) {
  if (agents.length === 0) return null;
  return (
    <>
      <p className="card-label" id="agents-can-do-label">
        {l.yourAgents}
      </p>
      <ul className="bordered agents-can-do" aria-labelledby="agents-can-do-label">
        {agents.map((row) => {
          const name = visible(row.agent.displayName);
          const detail = [row.area ? visible(row.area) : undefined, row.mode?.status ?? mode].filter(Boolean).join(" · ");
          return (
            <li key={row.agent.name} className="agents-can-do-row">
              <Avatar agent={row.agent} size="sm" />
              <span className="agents-can-do-name">{name}</span>
              {detail && <span className="muted">{detail}</span>}
              <button
                type="button"
                className="link agents-can-do-settings"
                aria-label={`${l.settings}: ${name}`}
                onClick={() => goTo(agentSettingsPath(row.agent.name))}
              >
                {l.settings}
              </button>
            </li>
          );
        })}
      </ul>
    </>
  );
}

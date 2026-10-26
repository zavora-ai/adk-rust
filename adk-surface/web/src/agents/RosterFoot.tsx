import type { AgentRow } from "../generated/api";
import { agentLabels as l } from "./labels";
import { agentSettingsPath, goTo, newAgentPath } from "./route";
import "./agents.css";

/** Whether a roster row is an agent the person added: the engine marks it `added`. */
export function isAdded(row: AgentRow): boolean {
  return (row as AgentRow & { added?: boolean }).added === true;
}

/**
 * The foot of the roster (spec 3.3 §5.1, §5.7): **New agent**, and with no agent of the
 * person's own, "No agents of your own yet. Add one, or ask ADK-Rust to suggest one." It goes
 * in the slot the roster leaves at the end of its list.
 */
export function RosterFoot({ rows, canAdd = true }: { rows: AgentRow[]; canAdd?: boolean }) {
  if (!canAdd) return null;
  return (
    <li className="roster-foot">
      {!rows.some(isAdded) && <p className="rail-empty">{l.noAgentsOfYourOwn}</p>}
      <button type="button" className="roster-row roster-new" onClick={() => goTo(newAgentPath())}>
        <span className="avatar avatar-md roster-new-mark" aria-hidden="true">
          +
        </span>
        <span className="roster-name">{l.newAgent}</span>
      </button>
    </li>
  );
}

/** **Settings** on a roster row, shown on hover and on focus (spec 3.3 §5.7). */
export function RowSettings({ row }: { row: AgentRow }) {
  return (
    <button
      type="button"
      className="link roster-settings"
      aria-label={`${l.settings}: ${row.agent.displayName}`}
      onClick={(event) => {
        event.stopPropagation();
        goTo(agentSettingsPath(row.agent.name));
      }}
    >
      {l.settings}
    </button>
  );
}

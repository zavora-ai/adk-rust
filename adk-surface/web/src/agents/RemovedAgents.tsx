import { useState } from "react";
import type { RemovedAgent } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { agentLabels as l } from "./labels";
import "./agents.css";

/**
 * "Removed agents" at the roster's foot (spec 3.3 §5.7, §13 decision 18): the agents the person
 * removed, each with its threads, which open read only. Nothing here changes an agent.
 */
export function RemovedAgents({
  removed,
  reading,
  onRead,
}: {
  removed: RemovedAgent[];
  /** The removed agent's thread being read, if one is. */
  reading?: string;
  onRead: (agent: RemovedAgent, session: string) => void;
}) {
  const [open, setOpen] = useState(false);
  if (removed.length === 0) return null;
  return (
    <li className="roster-removed">
      <button
        type="button"
        className="link roster-removed-toggle"
        aria-expanded={open}
        onClick={() => setOpen((shown) => !shown)}
      >
        {l.removedAgents} <span className="muted">{removed.length}</span>
      </button>
      {open && (
        <ul className="roster-removed-list">
          {removed.map((agent) => (
            <li key={agent.agent.name}>
              <div className="roster-removed-agent">
                <Avatar agent={agent.agent} />
                <span className="roster-text">
                  <span className="roster-name">{agent.agent.displayName}</span>
                  <span className="status">{agent.removedText}</span>
                </span>
              </div>
              {agent.threads.length === 0 ? (
                <p className="rail-empty">{l.noThreads}</p>
              ) : (
                <ul className="roster-removed-threads">
                  {agent.threads.map((thread) => (
                    <li key={thread.sessionId}>
                      <button
                        type="button"
                        className="link"
                        aria-current={thread.sessionId === reading ? "true" : undefined}
                        onClick={() => onRead(agent, thread.sessionId)}
                      >
                        {l.threadOf(thread.when)}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

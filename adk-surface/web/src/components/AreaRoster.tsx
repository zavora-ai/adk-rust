import type { ReactNode } from "react";
import { humanize } from "../away";
import { dollars } from "../format";
import type { AgentRef, AgentRow } from "../generated/api";
import { labels } from "../labels";
import type { AgentSpend } from "../seams";
import { Avatar } from "./Avatar";

/** One agent of an area: its avatar, name, status word and what it spent today. */
export interface AreaAgent {
  agent: AgentRef;
  statusText?: string;
  spentToday: number;
}

/** The colours avatars take, as the engine gives them (`project::avatar`, spec 3.3 §9). */
const AVATAR_COLOURS = ["#C03A00", "#15803D", "#1D4ED8", "#7C3AED", "#475569", "#B45309", "#0E7490", "#BE185D"];

/**
 * An agent the engine has no row for, with the avatar the engine would give it: the first
 * letter of its name, and a colour picked by the FNV-1a hash of the name.
 */
export function stranger(name: string): AgentRef {
  const displayName = humanize(name);
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(displayName)) {
    hash = ((hash ^ BigInt(byte)) * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  const monogram = [...displayName].find((c) => /[\p{L}\p{N}]/u.test(c))?.toUpperCase() ?? "";
  return { name, displayName, avatar: { monogram, color: AVATAR_COLOURS[Number(hash % 8n)] } };
}

/** The name the kernel's own work runs under, and the prefix of its targets (`kernel.proc`). */
const UPKEEP = "kernel";

function isUpkeep(target: string): boolean {
  return target === UPKEEP || target.startsWith(`${UPKEEP}.`);
}

/**
 * The agents of `area` with what each spent today (spec 3.3 §12 phase 4): every agent with
 * spend in the area, and on a kernel without areas every agent of the roster too. A target the
 * kernel runs per area (`adk-rust@work`) is the roster's agent of that name.
 */
export function areaAgents(
  area: string,
  spending: AgentSpend[],
  rows: AgentRow[],
  only: boolean,
): AreaAgent[] {
  const out = new Map<string, AreaAgent>();
  const rowOf = (target: string) => {
    const bare = target.endsWith(`@${area}`) ? target.slice(0, -area.length - 1) : target;
    return rows.find((row) => row.agent.name === target) ?? rows.find((row) => row.agent.name === bare);
  };
  // The kernel's own upkeep — tidying memory, checking what agents do — is listed only when it
  // spent something, so the agents add up to the area's "spent today".
  const upkeep = spending.filter((entry) => entry.area === area && isUpkeep(entry.agent));
  const upkeepSpent = upkeep.reduce((sum, entry) => sum + entry.spentToday, 0);
  for (const spend of spending.filter((entry) => entry.area === area && !isUpkeep(entry.agent))) {
    const row = rowOf(spend.agent);
    const key = row?.agent.name ?? spend.agent;
    const known = out.get(key);
    if (known) {
      known.spentToday += spend.spentToday;
      continue;
    }
    const bare = spend.agent.endsWith(`@${area}`) ? spend.agent.slice(0, -area.length - 1) : spend.agent;
    out.set(key, { agent: row?.agent ?? stranger(bare), statusText: row?.statusText, spentToday: spend.spentToday });
  }
  if (only) {
    for (const row of rows) {
      if (!out.has(row.agent.name)) out.set(row.agent.name, { agent: row.agent, statusText: row.statusText, spentToday: 0 });
    }
  }
  if (upkeepSpent > 0) {
    out.set(UPKEEP, { agent: { ...stranger(UPKEEP), displayName: labels.upkeep }, spentToday: upkeepSpent });
  }
  return [...out.values()].sort((a, b) => b.spentToday - a.spentToday || a.agent.displayName.localeCompare(b.agent.displayName));
}

/**
 * An area's roster (spec 3.3 §5.1, §12 phase 4): each agent with its avatar, status word and
 * what it spent today, which add up to the area's "spent today". `children` end the list, where
 * an "Add agent" entry goes once agents can be created from the home.
 */
export function AreaRoster({ agents, label, children }: { agents: AreaAgent[]; label: string; children?: ReactNode }) {
  return (
    <section className="area-roster" aria-label={label}>
      <p className="card-label">{label}</p>
      {agents.length === 0 && !children ? (
        <p className="card-empty">{labels.noAgentSpent}</p>
      ) : (
        <ul className="bordered">
          {agents.map((entry) => (
            <li key={entry.agent.name} className="area-agent">
              <Avatar agent={entry.agent} size="sm" />
              <span className="area-agent-text">
                <span className="area-agent-name">{entry.agent.displayName}</span>
                {entry.statusText && <span className="muted">{entry.statusText}</span>}
              </span>
              <span className="area-agent-spent">{labels.spentTodayShort(dollars(entry.spentToday))}</span>
            </li>
          ))}
          {children}
        </ul>
      )}
    </section>
  );
}

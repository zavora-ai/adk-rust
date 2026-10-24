/**
 * "Coming up" and "Routines today" (spec 3.3 §5.1): the kernel's sleeping processes and
 * scheduled agenda items, with the next time each wakes.
 */
import type { AgentRow } from "./generated/api";
import type { AgendaItem, KernelProcess } from "./kernel";
import { agendaState, fireToday, nextWake, processTitle } from "./kernel";

export interface Planned {
  key: string;
  at: Date;
  agent?: string;
  title: string;
}

export function planned(
  processes: KernelProcess[] | undefined,
  agenda: AgendaItem[] | undefined,
  agents: AgentRow[] | undefined,
  now: Date,
): Planned[] {
  const out: Planned[] = [];
  const seen = new Set<string>();
  const nameOf = (target: string) =>
    agents?.find((row) => row.agent.name === target)?.agent.displayName ?? undefined;
  for (const process of processes ?? []) {
    const at = nextWake(process.meta.wake, now);
    const title = processTitle(process, agenda ?? []);
    if (!at || !title) continue;
    if (process.meta.agendaItem) seen.add(process.meta.agendaItem);
    out.push({ key: process.meta.pid, at, agent: nameOf(process.run.target), title });
  }
  for (const item of agenda ?? []) {
    if (seen.has(item.id) || agendaState(item) !== "scheduled" || typeof item.state === "string") continue;
    const at = "scheduled" in item.state ? nextWake(item.state.scheduled.wake, now) : undefined;
    if (at) out.push({ key: item.id, at, title: item.title });
  }
  return out.sort((a, b) => a.at.getTime() - b.at.getTime());
}

/** Today's routines (spec 3.3 §5.1 Today): each sleeper's firing on today's date, past ones included. */
export function today(
  processes: KernelProcess[] | undefined,
  agenda: AgendaItem[] | undefined,
  agents: AgentRow[] | undefined,
  now: Date,
): Planned[] {
  const out: Planned[] = [];
  for (const process of processes ?? []) {
    const title = processTitle(process, agenda ?? []);
    const at = (process.meta.wake?.anyOf ?? [])
      .map((wake) => fireToday(wake, now))
      .find((value): value is Date => value !== undefined);
    if (!at || !title) continue;
    const agent = agents?.find((row) => row.agent.name === process.run.target)?.agent.displayName;
    out.push({ key: process.meta.pid, at, agent, title });
  }
  return out.sort((a, b) => a.at.getTime() - b.at.getTime());
}

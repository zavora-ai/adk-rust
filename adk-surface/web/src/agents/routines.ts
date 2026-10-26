/**
 * An agent's routines (spec 3.3 §5.7 "Agent settings"): its scheduled processes, as Today
 * adds them, each with when it next runs.
 */
import { whenAhead } from "../format";
import type { AgendaItem, KernelProcess } from "../kernel";
import { nextWake, processTitle } from "../kernel";
import type { AgentRoutine } from "./AgentSettings";

/** The routines of `agent`, soonest first: its processes that wake on a schedule. */
export function agentRoutines(
  processes: KernelProcess[] | undefined,
  agenda: AgendaItem[] | undefined,
  agent: string,
  now: Date,
): AgentRoutine[] {
  const out: (AgentRoutine & { at: number })[] = [];
  for (const process of processes ?? []) {
    if (process.run.target !== agent) continue;
    const at = nextWake(process.meta.wake, now);
    const text = processTitle(process, agenda ?? []);
    if (!at || !text) continue;
    out.push({ id: process.meta.pid, text, when: whenAhead(at, now), at: at.getTime() });
  }
  return out.sort((a, b) => a.at - b.at).map(({ id, text, when }) => ({ id, text, when }));
}

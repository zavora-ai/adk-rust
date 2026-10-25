/**
 * The addresses of agents a person adds (spec 3.3 §5.7): `/ui/agents/new` for the New agent
 * screen — `?from=<decision>` holds a suggestion for **Change it first** — and
 * `/ui/agents/<id>` for an agent's settings. Ids never contain a dot, so the server serves the
 * page for every one of them.
 */
const AGENTS = "/ui/agents/";

export type AgentsPlace = { kind: "new"; from?: string } | { kind: "settings"; id: string };

/** The agents screen `location` names, if any. */
export function agentsPlaceOf({ pathname, search }: Pick<Location, "pathname" | "search">): AgentsPlace | undefined {
  if (!pathname.startsWith(AGENTS)) return undefined;
  const rest = pathname.slice(AGENTS.length).replace(/\/$/, "");
  if (!rest) return undefined;
  if (rest === "new") {
    const from = new URLSearchParams(search).get("from") ?? undefined;
    return { kind: "new", ...(from ? { from } : {}) };
  }
  try {
    return { kind: "settings", id: decodeURIComponent(rest) };
  } catch {
    return undefined;
  }
}

/** The New agent screen, holding the suggestion `from` when given. */
export function newAgentPath(from?: string): string {
  return from ? `${AGENTS}new?from=${encodeURIComponent(from)}` : `${AGENTS}new`;
}

/** An agent's settings. */
export function agentSettingsPath(id: string): string {
  return `${AGENTS}${encodeURIComponent(id)}`;
}

/** Moves the page to `path` as a link inside the home would, without reloading it. */
export function goTo(path: string) {
  window.history.pushState(null, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

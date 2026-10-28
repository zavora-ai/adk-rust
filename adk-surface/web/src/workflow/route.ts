/**
 * The Runs tab's address (spec 3.3 §12): `/ui/developer/runs/<session>`, where a graph node's
 * decision card links ("Open in Runs"); `?runs=<session>` names the same, for the example
 * page.
 */
const RUNS = "/ui/developer/runs";

/** The Runs tab the address opens, with its session when it names one. */
export function runsOf({ pathname, search }: Pick<Location, "pathname" | "search">): { session?: string } | undefined {
  const query = new URLSearchParams(search).get("runs");
  if (query !== null) return query ? { session: query } : {};
  if (pathname !== RUNS && !pathname.startsWith(`${RUNS}/`)) return undefined;
  const rest = pathname.slice(RUNS.length + 1);
  if (!rest) return {};
  try {
    return { session: decodeURIComponent(rest.split("/")[0]) };
  } catch {
    return {};
  }
}

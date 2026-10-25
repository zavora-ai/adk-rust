/** The web home's sections, each its own address under `/ui/` (spec 3.3 §5.1). */
export type Section = "threads" | "today" | "memory" | "developer";

const BASE = "/ui/";

/** Where a decision opens in the home: `/ui/decisions/<id>` (spec 3.3 §7.2, "opens the home"). */
const DECISIONS = `${BASE}decisions/`;

export function sectionOf(pathname: string): Section {
  const rest = pathname.startsWith(BASE) ? pathname.slice(BASE.length) : "";
  const first = rest.split("/")[0];
  return first === "today" || first === "memory" || first === "developer" ? first : "threads";
}

export function pathOf(section: Section): string {
  return section === "threads" ? BASE : `${BASE}${section}`;
}

/**
 * The decision a deep link names: the tray, the phone and voice open a decision they may not
 * answer at `/ui/decisions/<id>` (spec 3.3 §7.2). Ids may contain dots and escaped slashes.
 */
export function decisionOf(pathname: string): string | undefined {
  if (!pathname.startsWith(DECISIONS)) return undefined;
  const id = pathname.slice(DECISIONS.length);
  if (!id) return undefined;
  try {
    return decodeURIComponent(id);
  } catch {
    return undefined;
  }
}

/** The address of one decision in the home. */
export function decisionPath(id: string): string {
  return `${DECISIONS}${encodeURIComponent(id)}`;
}

/**
 * The shell a page under `/ui/` starts (spec 3.3 §5), by the first part of its path: the web
 * home for every section and deep link; the tray panel and the phone add their own branches.
 */
export type Shell = "home" | "tray";

export function shellOf({ pathname }: Pick<Location, "pathname">): Shell {
  const first = pathname.startsWith(BASE) ? pathname.slice(BASE.length).split("/")[0] : "";
  switch (first) {
    case "tray":
      return "tray";
    default:
      return "home";
  }
}

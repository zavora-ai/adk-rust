/** The web home's sections, each its own address under `/ui/` (spec 3.3 §5.1). */
export type Section = "threads" | "today" | "memory" | "developer";

const BASE = "/ui/";

export function sectionOf(pathname: string): Section {
  const rest = pathname.startsWith(BASE) ? pathname.slice(BASE.length) : "";
  const first = rest.split("/")[0];
  return first === "today" || first === "memory" || first === "developer" ? first : "threads";
}

export function pathOf(section: Section): string {
  return section === "threads" ? BASE : `${BASE}${section}`;
}

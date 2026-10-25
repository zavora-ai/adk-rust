/** The phone's screens, each its own address under `/ui/` (spec 3.3 §5.4). */
export type PhoneScreen =
  | { screen: "pair"; code?: string }
  | { screen: "decisions" }
  | { screen: "decide"; id: string }
  | { screen: "threads" }
  | { screen: "today" }
  | { screen: "memory" };

const BASE = "/ui/";

/** The screen a phone address names; the pairing link carries its code after `#code=`. */
export function phoneScreenOf({ pathname, hash }: Pick<Location, "pathname" | "hash">): PhoneScreen {
  const rest = pathname.startsWith(BASE) ? pathname.slice(BASE.length) : "";
  const [first, second] = rest.split("/");
  switch (first) {
    case "pair": {
      const code = new URLSearchParams(hash.replace(/^#/, "")).get("code") ?? undefined;
      return { screen: "pair", ...(code ? { code } : {}) };
    }
    case "decisions":
      if (second) {
        try {
          return { screen: "decide", id: decodeURIComponent(second) };
        } catch {
          return { screen: "decisions" };
        }
      }
      return { screen: "decisions" };
    case "today":
      return { screen: "today" };
    case "memory":
      return { screen: "memory" };
    case "":
    case undefined:
      return { screen: "threads" };
    default:
      return { screen: "decisions" };
  }
}

export function phonePathOf(screen: PhoneScreen): string {
  switch (screen.screen) {
    case "pair":
      return `${BASE}pair`;
    case "decide":
      return `${BASE}decisions/${encodeURIComponent(screen.id)}`;
    case "threads":
      return BASE;
    default:
      return `${BASE}${screen.screen}`;
  }
}

/** Whether the page runs as the installed app rather than in a browser tab. */
export function standalone(): boolean {
  const own = navigator as Navigator & { standalone?: boolean };
  return own.standalone === true || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

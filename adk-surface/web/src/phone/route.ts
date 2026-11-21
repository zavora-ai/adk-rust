import type { ViewerPlace } from "../viewer/route";
import { viewerOf, viewerPath } from "../viewer/route";
import type { CameraPlace } from "../camera/route";
import { cameraOf, cameraPath } from "../camera/route";

/** The phone's screens, each its own address under `/ui/` (spec 3.3 §5.4). */
export type PhoneScreen =
  | { screen: "pair"; code?: string }
  | { screen: "decisions" }
  | { screen: "decide"; id: string }
  | { screen: "threads" }
  | { screen: "today" }
  | { screen: "memory" }
  | { screen: "viewer"; place: ViewerPlace }
  | { screen: "camera"; place: CameraPlace };

const BASE = "/ui/";


/** The screen a phone address names; the pairing link carries its code after `#code=`. */
export function phoneScreenOf({
  pathname,
  hash,
  search = "",
}: Pick<Location, "pathname" | "hash"> & { search?: string }): PhoneScreen {
  // The annotated viewer, read-only (spec 3.7 §8.2, board choice V15).
  const place = viewerOf({ pathname, search });
  if (place) return { screen: "viewer", place };
  // The live camera surface (spec 3.7 §8.2, board choice C14).
  const camera = cameraOf({ pathname, search });
  if (camera) return { screen: "camera", place: camera };
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
    case "viewer":
      return viewerPath(screen.place);
    case "camera":
      return cameraPath(screen.place);
    default:
      return `${BASE}${screen.screen}`;
  }
}

/** Whether the page runs as the installed app rather than in a browser tab. */
export function standalone(): boolean {
  const own = navigator as Navigator & { standalone?: boolean };
  return own.standalone === true || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

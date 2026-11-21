/**
 * Where the live camera surface opens (spec 3.7 §8.2, board choice C1): a view under the
 * agent at `/ui/cameras/<camera>`, and every camera as a grid at `/ui/cameras`.
 */
const PREFIX = "/ui/cameras";

export interface CameraPlace {
  /** The camera watched large; the grid when absent. */
  camera?: string;
  /** "Edit the zone" opened from a phone: the zone editor opens on arrival. */
  zone?: boolean;
}

export function cameraOf(location: Pick<Location, "pathname" | "search">): CameraPlace | undefined {
  const { pathname } = location;
  if (pathname !== PREFIX && pathname !== `${PREFIX}/` && !pathname.startsWith(`${PREFIX}/`)) return undefined;
  const rest = pathname.slice(PREFIX.length).replace(/^\//, "").split("/")[0];
  if (!rest) return {};
  try {
    const zone = new URLSearchParams(location.search).get("zone") === "edit";
    return { camera: decodeURIComponent(rest), ...(zone ? { zone } : {}) };
  } catch {
    return {};
  }
}

export function cameraPath(place: CameraPlace): string {
  if (!place.camera) return PREFIX;
  return `${PREFIX}/${encodeURIComponent(place.camera)}${place.zone ? "?zone=edit" : ""}`;
}

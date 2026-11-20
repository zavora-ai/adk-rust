/**
 * Where the annotated viewer opens (spec 3.7 §8.2, board choice V1): a web home view at
 * `/ui/vision/<session>/<image>`, the image a `sha256:` digest or a name the session holds,
 * with `?answer=<call>` and `&compare=<call>`.
 */
const PREFIX = "/ui/vision/";

export interface ViewerPlace {
  session: string;
  image: string;
  answer?: string;
  compare?: string;
}

export function viewerOf(location: Pick<Location, "pathname" | "search">): ViewerPlace | undefined {
  if (!location.pathname.startsWith(PREFIX)) return undefined;
  const [session, image] = location.pathname.slice(PREFIX.length).split("/");
  if (!session || !image) return undefined;
  try {
    const query = new URLSearchParams(location.search);
    return {
      session: decodeURIComponent(session),
      image: decodeURIComponent(image),
      answer: query.get("answer") ?? undefined,
      compare: query.get("compare") ?? undefined,
    };
  } catch {
    return undefined;
  }
}

export function viewerPath(place: ViewerPlace): string {
  const query = new URLSearchParams();
  if (place.answer) query.set("answer", place.answer);
  if (place.compare) query.set("compare", place.compare);
  const rest = query.toString();
  return `${PREFIX}${encodeURIComponent(place.session)}/${encodeURIComponent(place.image)}${rest ? `?${rest}` : ""}`;
}

/**
 * Evidence images (spec 3.7 §8.2): a card names each image by the route that serves it
 * (`/api/surface/decisions/{id}/evidence/{index}`), and the page loads it with the viewer's
 * own authorisation — the bearer token the client holds — never from the card itself.
 */
import { createContext, useContext, useEffect, useState } from "react";

/** Reads the image a card names at `href`, as the signed-in viewer. */
export type ImageLoader = (href: string) => Promise<Blob>;

/** A loader that reads `href` with the page's own cookies only. */
export const sameOriginLoader: ImageLoader = async (href) => {
  const response = await fetch(href, { credentials: "same-origin" });
  if (!response.ok) throw new Error(`${response.status}`);
  return response.blob();
};

/** The loader the screens use; the app gives its client's. */
export const ImageLoaderContext = createContext<ImageLoader>(sameOriginLoader);

/** What a picture shows while its bytes load. */
export type Loaded = { state: "loading" } | { state: "shown"; url: string } | { state: "failed" };

/** The object URL of the image at `href`, released when the picture goes. */
export function useEvidenceImage(href: string | null | undefined): Loaded {
  const load = useContext(ImageLoaderContext);
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  useEffect(() => {
    if (!href) {
      setLoaded({ state: "failed" });
      return;
    }
    let live = true;
    let url: string | undefined;
    setLoaded({ state: "loading" });
    load(href)
      .then((blob) => {
        if (!live) return;
        url = URL.createObjectURL(blob);
        setLoaded({ state: "shown", url });
      })
      .catch(() => live && setLoaded({ state: "failed" }));
    return () => {
      live = false;
      if (url) URL.revokeObjectURL(url);
    };
  }, [href, load]);
  return loaded;
}

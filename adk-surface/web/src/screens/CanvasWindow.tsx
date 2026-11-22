import { useEffect, useRef, useState } from "react";
import type { HomeClient } from "../client";
import type { Send } from "../components/DecisionCard";
import { CanvasPane } from "../components/Thread";
import { emptyThread, followFeed, wakeable } from "../feed";

/** Where a drawing opens in its own window: `/ui/canvas?session=<id>&surface=<id>`. */
export function canvasWindowPath(session: string, surfaceId: string): string {
  return `/ui/canvas?session=${encodeURIComponent(session)}&surface=${encodeURIComponent(surfaceId)}`;
}

/**
 * A drawing in its own window (spec 3.3 §5.1 "The canvas"): the canvas alone, following the
 * session's feed, so a person can keep it on another screen while the thread goes on. Its form
 * sends and its decisions work here as in the thread.
 */
export function CanvasWindow({ client, send, location = window.location }: { client: HomeClient; send: Send; location?: Pick<Location, "search"> }) {
  const params = new URLSearchParams(location.search);
  const session = params.get("session") ?? "";
  const surface = params.get("surface") ?? "";
  const [thread, setThread] = useState(() => emptyThread(session));
  const feed = useRef(wakeable());
  useEffect(() => {
    if (!session) return;
    const abort = new AbortController();
    void followFeed(client, session, abort.signal, setThread, () => undefined, (ms) => feed.current.pause(ms));
    return () => abort.abort();
  }, [client, session]);
  return (
    <div className="canvas-window">
      <CanvasPane
        entries={thread.session === session ? thread.entries : []}
        pinned={surface}
        open={thread.session === session ? thread.open : []}
        client={client}
        send={send}
        session={session}
        onSettled={() => undefined}
        onClose={() => window.close()}
        popOut={false}
      />
    </div>
  );
}

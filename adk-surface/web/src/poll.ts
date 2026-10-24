import { useCallback, useEffect, useRef, useState } from "react";

/** How often the screens read what has no feed of its own. */
export const POLL_MS = 2_500;

/**
 * Reads `load` now, then every `every` ms while the page is visible, and again on `refresh()`.
 * Surfaces hold no state a reconnect cannot rebuild (spec 3.3 §6): every read starts over.
 */
export function usePoll<T>(load: () => Promise<T>, every: number = POLL_MS) {
  const [value, setValue] = useState<T>();
  const [error, setError] = useState<unknown>();
  const latest = useRef(load);
  latest.current = load;
  const generation = useRef(0);

  const refresh = useCallback(async () => {
    const mine = ++generation.current;
    try {
      const next = await latest.current();
      if (mine === generation.current) {
        setValue(next);
        setError(undefined);
      }
    } catch (cause) {
      if (mine === generation.current) setError(cause);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "hidden") void refresh();
    }, every);
    return () => window.clearInterval(timer);
  }, [every, refresh]);

  return { value, error, refresh };
}

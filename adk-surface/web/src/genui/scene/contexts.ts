/**
 * The scenes drawing with WebGL on this page. A browser keeps only so many WebGL contexts
 * (about sixteen), and a thread can hold many drawings, so at most [`MAX_LIVE`] scenes draw at
 * once: the one least recently looked at goes to sleep, showing its words, until it is woken.
 */

/** The most scenes drawing with WebGL at once. */
export const MAX_LIVE = 6;

interface Live {
  id: number;
  sleep: () => void;
}

let next = 1;
const live: Live[] = [];

/** Whether this browser draws WebGL at all. */
export function webglAvailable(): boolean {
  try {
    if (typeof document === "undefined") return false;
    const canvas = document.createElement("canvas");
    return !!(canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  } catch {
    return false;
  }
}

/**
 * Takes a place among the live scenes; `sleep` is called if a newer one needs it. Returns the
 * place's id and a function that gives it up.
 */
export function acquire(sleep: () => void): { id: number; release: () => void } {
  const id = next++;
  live.push({ id, sleep });
  while (live.length > MAX_LIVE) {
    const oldest = live.shift();
    oldest?.sleep();
  }
  return {
    id,
    release: () => {
      const index = live.findIndex((entry) => entry.id === id);
      if (index >= 0) live.splice(index, 1);
    },
  };
}

/** The scene `id` was just looked at or used: it is the last to sleep. */
export function touch(id: number): void {
  const index = live.findIndex((entry) => entry.id === id);
  if (index >= 0) live.push(...live.splice(index, 1));
}

/** How many scenes draw now. */
export function liveCount(): number {
  return live.length;
}

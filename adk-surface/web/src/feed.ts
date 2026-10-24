/**
 * Following a session's feed (spec 3.3 §6): entries numbered by their place in the session
 * store, resumed after the last `seq` seen, so a reconnect never skips or repeats one.
 */
import type { DecisionCard, FeedEntry } from "./generated/api";
import type { FeedMessage, HomeClient } from "./client";
import { ApiError } from "./client";

/** What a thread shows: its entries in order, and the session's open decisions. */
export interface ThreadState {
  /** The session these entries and decisions are of. */
  session: string;
  entries: FeedEntry[];
  open: DecisionCard[];
  /** The server has said which decisions of the session are open. */
  openKnown: boolean;
  /** The last `seq` received: what a reconnect resumes after. */
  last: number;
}

/** A thread before its feed has said anything. */
export function emptyThread(session: string): ThreadState {
  return { session, entries: [], open: [], openKnown: false, last: 0 };
}

/** The thread after one frame: an entry already seen is dropped, a reset starts over. */
export function applyFrame(state: ThreadState, message: FeedMessage): ThreadState {
  switch (message.type) {
    case "entry":
      if (message.entry.seq <= state.last) return state;
      return { ...state, entries: [...state.entries, message.entry], last: message.entry.seq };
    case "decisions":
      return { ...state, open: message.cards, openKnown: true };
    case "reset":
      return { ...state, entries: [], last: 0 };
  }
}

/** How long to wait before looking again at a session nobody has written to yet. */
export const NEW_SESSION_MS = 1_000;

/** A pause that `wake` ends early, so a sent message shows without waiting out a retry. */
export function wakeable() {
  let wake = () => {};
  return {
    pause: (ms: number) =>
      new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, ms);
        wake = () => {
          clearTimeout(timer);
          resolve();
        };
      }),
    wake: () => wake(),
  };
}

/**
 * Follows `session` until `signal` aborts: reads the feed, and after a dropped connection
 * reconnects from the last `seq` seen, waiting longer after each failure in a row. A session
 * that does not exist yet (`404`) is a thread with nothing in it, not a fault: it is read
 * again every `NEW_SESSION_MS` until its first message arrives.
 */
export async function followFeed(
  client: HomeClient,
  session: string,
  signal: AbortSignal,
  onState: (state: ThreadState) => void,
  onTrouble: (trouble: boolean) => void = () => {},
  pause: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<void> {
  let state = emptyThread(session);
  let failures = 0;
  while (!signal.aborted) {
    let wait: number;
    try {
      for await (const message of client.feed(session, state.last || undefined, signal)) {
        state = applyFrame(state, message);
        failures = 0;
        onTrouble(false);
        onState(state);
      }
      wait = Math.min(500 * 2 ** Math.min(failures, 4), 8_000);
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof ApiError && error.status === 404) {
        failures = 0;
        state = { ...emptyThread(session), openKnown: true };
        onTrouble(false);
        onState(state);
        wait = NEW_SESSION_MS;
      } else if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
        // The server answered: the page asks the person to sign in, not that it can't reach.
        failures = Math.min(failures + 1, 4);
        onTrouble(false);
        wait = Math.min(500 * 2 ** failures, 8_000);
      } else {
        failures += 1;
        if (failures > 1) onTrouble(true);
        wait = Math.min(500 * 2 ** Math.min(failures, 4), 8_000);
      }
    }
    if (signal.aborted) return;
    await pause(wait);
  }
}

// Following a session (spec 3.3 §6): a session nobody has written to yet is an empty thread,
// not an unreachable server, and is read again soon; a dropped connection backs off.
import { describe, expect, it } from "vitest";
import type { FeedMessage, HomeClient } from "../src/client";
import { ApiError } from "../src/client";
import { NEW_SESSION_MS, followFeed, wakeable } from "../src/feed";
import type { ThreadState } from "../src/feed";

/** A client whose feed answers each read with the next of `reads`: frames, or an error. */
function feeding(reads: Array<FeedMessage[] | Error>, done: AbortController): HomeClient {
  let read = 0;
  return {
    async *feed() {
      const next = reads[read++];
      if (next === undefined) {
        done.abort();
        return;
      }
      if (next instanceof Error) throw next;
      for (const message of next) yield message;
    },
  } as unknown as HomeClient;
}

const entry = (seq: number): FeedMessage => ({ type: "entry", entry: { seq, at: "", atText: "", items: [] } });

describe("following a session's feed", () => {
  it("treats a session that does not exist yet as an empty thread, read again every second", async () => {
    const abort = new AbortController();
    const notYet = () => new ApiError(404, "session 's-new' not found", "surface.not_found");
    const client = feeding([notYet(), notYet(), notYet(), [entry(1)]], abort);
    const trouble: boolean[] = [];
    const pauses: number[] = [];
    const states: ThreadState[] = [];
    await followFeed(
      client,
      "s-new",
      abort.signal,
      (state) => states.push(state),
      (value) => trouble.push(value),
      async (ms) => {
        pauses.push(ms);
      },
    );
    expect(trouble).not.toContain(true);
    expect(pauses.slice(0, 3)).toEqual([NEW_SESSION_MS, NEW_SESSION_MS, NEW_SESSION_MS]);
    expect(states[0]).toMatchObject({ session: "s-new", entries: [], open: [], openKnown: true });
    expect(states.at(-1)?.entries.map((item) => item.seq)).toEqual([1]);
  });

  it("says it cannot reach the agents after failures in a row, waiting longer each time", async () => {
    const abort = new AbortController();
    const down = () => new TypeError("Failed to fetch");
    const client = feeding([down(), down(), down(), [entry(1)]], abort);
    const trouble: boolean[] = [];
    const pauses: number[] = [];
    await followFeed(client, "s1", abort.signal, () => {}, (value) => trouble.push(value), async (ms) => {
      pauses.push(ms);
    });
    expect(trouble).toEqual([true, true, false]);
    expect(pauses.slice(0, 3)).toEqual([1_000, 2_000, 4_000]);
  });

  it("does not say it cannot reach the agents when the server asks the person to sign in", async () => {
    const abort = new AbortController();
    const signIn = () => new ApiError(401, "the surface API needs an authenticated caller");
    const client = feeding([signIn(), signIn(), signIn(), [entry(1)]], abort);
    const trouble: boolean[] = [];
    await followFeed(client, "s1", abort.signal, () => {}, (value) => trouble.push(value), async () => {});
    expect(trouble).not.toContain(true);
  });

  it("wakes a pause early, so a message just sent shows now", async () => {
    const { pause, wake } = wakeable();
    const started = Date.now();
    const waiting = pause(5_000);
    wake();
    await waiting;
    expect(Date.now() - started).toBeLessThan(1_000);
  });
});

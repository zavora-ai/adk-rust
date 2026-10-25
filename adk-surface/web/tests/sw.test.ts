// The phone's service worker (spec 3.3 §5.4, §11 S-I5): a wake-up shows the fixed words before
// anything else, whatever the push carries, then reads the badge and acknowledges it.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import * as words from "../src/generated/words";

const source = readFileSync(join(__dirname, "..", "public", "sw.js"), "utf8");

type Listener = (event: unknown) => void;

function worker(token: string | undefined) {
  const order: string[] = [];
  const listeners: Record<string, Listener> = {};
  const showNotification = vi.fn(async (title: string, options: unknown) => {
    order.push(`notify ${title}`);
    return options;
  });
  const fetch = vi.fn(async (url: string) => {
    order.push(`fetch ${url}`);
    return new Response(JSON.stringify({ decisions: 2 }), { status: 200 });
  });
  const setAppBadge = vi.fn(async () => undefined);
  const indexedDB = {
    open() {
      const request: Record<string, unknown> = {};
      queueMicrotask(() => {
        const db = {
          close() {},
          transaction: () => ({
            objectStore: () => ({
              get() {
                const get: Record<string, unknown> = {};
                queueMicrotask(() => {
                  get.result = token;
                  (get.onsuccess as () => void)?.();
                });
                return get;
              },
            }),
          }),
        };
        request.result = db;
        (request.onsuccess as () => void)?.();
      });
      return request;
    },
  };
  const self = {
    registration: { showNotification },
    navigator: { setAppBadge },
    clients: { claim: async () => undefined, matchAll: async () => [], openWindow: async () => undefined },
    skipWaiting: () => undefined,
    addEventListener: (name: string, listener: Listener) => {
      listeners[name] = listener;
    },
  };
  new Function("self", "indexedDB", "fetch", source)(self, indexedDB, fetch);
  async function push(data: unknown) {
    let done: Promise<unknown> = Promise.resolve();
    listeners.push({ data: { json: () => data }, waitUntil: (work: Promise<unknown>) => (done = work) });
    await done;
  }
  return { push, order, showNotification, fetch, setAppBadge };
}

describe("the service worker", () => {
  it("says the engine's words", () => {
    expect(source).toContain(`const TITLE = ${JSON.stringify(words.A_DECISION_IS_WAITING)};`);
    expect(source).toContain(`const BODY = ${JSON.stringify(words.OPEN_THE_APP_TO_SEE_IT)};`);
  });

  it("shows the notification before any request, then reads the badge and acknowledges", async () => {
    const sw = worker("adk_dev_token");
    await sw.push({ v: 1, id: "w1" });
    expect(sw.order).toEqual([
      `notify ${words.A_DECISION_IS_WAITING}`,
      "fetch /api/surface/status?surface=phone",
      "fetch /api/surface/phone/push/received",
    ]);
    expect(sw.setAppBadge).toHaveBeenCalledWith(2);
    const [, received] = sw.fetch.mock.calls[1] as unknown as [string, RequestInit];
    expect(JSON.parse(received.body as string)).toEqual({ id: "w1" });
  });

  it("never shows what a push carries", async () => {
    const sw = worker(undefined);
    await sw.push({ v: 1, id: "w2", title: "send_email to [client]", body: "Invoice INV-42" });
    const [title, options] = sw.showNotification.mock.calls[0] as [string, { body: string }];
    expect([title, options.body]).toEqual([words.A_DECISION_IS_WAITING, words.OPEN_THE_APP_TO_SEE_IT]);
    expect(JSON.stringify(sw.showNotification.mock.calls)).not.toMatch(/send_email|INV-42/);
    expect(sw.fetch).not.toHaveBeenCalled();
  });
});

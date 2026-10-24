// The web home reads the engine's routes under `/api/surface`, never the `adk_ui` profile of
// the runtime UI that `adk-server` bundled (spec 3.3 §6, §10 S3).
import { afterEach, describe, expect, it, vi } from "vitest";
import { HttpClient } from "../src/client";

type Seen = { url: string; init?: RequestInit };

function recordFetch(seen: Seen[]) {
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    seen.push({ url, init });
    const body = url.includes("/feed")
      ? "id: 1\nevent: entry\ndata: {\"seq\":1}\n\n"
      : JSON.stringify({ agents: [], decisions: [], lines: [], type: "sent", sessionId: "s1" });
    return new Response(body, { status: 200, headers: { "content-type": "application/json" } });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => vi.unstubAllGlobals());

describe("the HTTP client", () => {
  it("reads and acts through the engine, for the web home, without the adk_ui profile", async () => {
    const seen: Seen[] = [];
    recordFetch(seen);
    const client = new HttpClient();
    await client.status();
    await client.agents();
    await client.decisions();
    await client.briefing("2026-10-24T00:00:00Z");
    await client.command({ type: "send", sessionId: "s1", text: "hello" }, { kind: "keyboard" });
    const abort = new AbortController();
    for await (const message of client.feed("s1", 4, abort.signal)) {
      expect(message).toEqual({ type: "entry", entry: { seq: 1 } });
    }

    expect(seen.map(({ url }) => url)).toEqual([
      "/api/surface/status?surface=web",
      "/api/surface/agents?surface=web",
      "/api/surface/decisions?surface=web",
      "/api/surface/briefing?surface=web&since=2026-10-24T00%3A00%3A00Z",
      "/api/surface/commands",
      "/api/surface/sessions/s1/feed?surface=web",
    ]);
    expect(JSON.parse(String(seen[4].init?.body))).toEqual({
      surface: "web",
      origin: { kind: "keyboard" },
      command: { type: "send", sessionId: "s1", text: "hello" },
    });
    // The feed resumes after the last entry seen.
    expect((seen[5].init?.headers as Record<string, string>)["last-event-id"]).toBe("4");
    for (const { init } of seen) {
      const headers = Object.fromEntries(
        Object.entries((init?.headers as Record<string, string>) ?? {}).map(([name, value]) => [name.toLowerCase(), value]),
      );
      expect(headers["x-adk-ui-protocol"]).toBeUndefined();
    }
  });

  it("treats a route the server does not mount as absent, not as an error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("", { status: 404 })),
    );
    const client = new HttpClient();
    expect(await client.processes()).toBeUndefined();
    expect(await client.agenda()).toBeUndefined();
    expect(await client.rules()).toBeUndefined();
    await expect(client.status()).rejects.toMatchObject({ status: 404 });
  });

  it("lists, removes and adds where you can say yes from over the kernel's pairing routes", async () => {
    const seen: Seen[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        seen.push({ url, init });
        const body =
          url === "/api/kernel/devices"
            ? {
                devices: [
                  { id: "d1", name: "Ana's phone", owner: "ana", scopes: ["decide", "read", "stop"], createdAt: "", expiresAt: "", state: "active" },
                  { id: "d2", name: "Old tablet", owner: "ana", scopes: ["read"], createdAt: "", expiresAt: "", state: "revoked" },
                ],
              }
            : url === "/api/kernel/channels"
              ? { accounts: [{ id: "c1", channel: "telegram", account: "42", state: "active" }], waiting: [] }
              : url === "/api/kernel/pairing"
                ? { code: "ABCD-2345", expiresAt: "", scopes: ["decide", "read", "stop"] }
                : {};
        return new Response(JSON.stringify(body), { status: 200 });
      }),
    );
    const devices = new HttpClient().devices();
    expect(await devices.list()).toEqual([
      { id: "device:d1", name: "Ana's phone", may: "yes or no, and stop all" },
      { id: "channel:c1", name: "Telegram", may: "yes once, or no" },
    ]);
    await devices.remove("device:d1");
    await devices.remove("channel:c1");
    expect(await devices.pair()).toBe("ABCD-2345");
    expect(seen.slice(2).map(({ url, init }) => [init?.method ?? "GET", url])).toEqual([
      ["POST", "/api/kernel/devices/d1/revoke"],
      ["POST", "/api/kernel/channels/c1/revoke"],
      ["POST", "/api/kernel/pairing"],
    ]);
  });

  it("sends the server's token, once given, with every request and the feed, and keeps it for the tab", async () => {
    const seen: Seen[] = [];
    recordFetch(seen);
    const client = new HttpClient();
    await client.status();
    client.signIn("t0ken");
    await client.status();
    await client.command({ type: "stopAll" }, { kind: "pointer" });
    for await (const message of client.feed("s1", undefined, new AbortController().signal)) void message;
    await new HttpClient().agents();
    const auth = seen.map(({ init }) => (init?.headers as Record<string, string>).authorization);
    expect(auth).toEqual([undefined, "Bearer t0ken", "Bearer t0ken", "Bearer t0ken", "Bearer t0ken"]);
  });

  it("reads what waits behind an open decision, and sends a take over to its route", async () => {
    const seen: Seen[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        seen.push({ url, init });
        const body = url.endsWith("/queued?surface=web")
          ? { sessionId: "s 1", queued: ["also attach the timesheet"] }
          : { type: "answered", card: {} };
        return new Response(JSON.stringify(body), { status: 200 });
      }),
    );
    const client = new HttpClient();
    expect(await client.queued("s 1")).toEqual(["also attach the timesheet"]);
    const card = { id: "apr 1", version: 3 } as unknown as Parameters<ReturnType<HttpClient["takeOver"]>["provide"]>[0];
    expect(await client.takeOver().provide(card, "731905", { kind: "pointer" })).toEqual({ type: "answered", card: {} });
    expect(seen.map(({ url, init }) => [init?.method ?? "GET", url])).toEqual([
      ["GET", "/api/surface/sessions/s%201/queued?surface=web"],
      ["POST", "/api/surface/decisions/apr%201/take-over"],
    ]);
    expect(JSON.parse(String(seen[1].init?.body))).toEqual({ surface: "web", version: 3, secret: "731905" });
  });

  it("reads no devices from a kernel that pairs none", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("", { status: 404 })),
    );
    expect(await new HttpClient().devices().list()).toBeUndefined();
  });
});

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

  it("reads and acts for the surface it is built for, and is the web home by default", async () => {
    const seen: Seen[] = [];
    recordFetch(seen);
    expect(new HttpClient().surface).toBe("web");
    const client = new HttpClient("/api", "tray");
    await client.status("s1");
    await client.agents();
    await client.decisions();
    await client.decision("apr 1");
    await client.briefing();
    await client.queued("s1");
    await client.command({ type: "stopAll" }, { kind: "pointer" });
    const card = { id: "apr 1", version: 3 } as unknown as Parameters<ReturnType<HttpClient["takeOver"]>["provide"]>[0];
    await client.takeOver().provide(card, "731905", { kind: "pointer" });
    for await (const message of client.feed("s1", undefined, new AbortController().signal)) void message;

    expect(seen.map(({ url }) => url)).toEqual([
      "/api/surface/status?surface=tray&session=s1",
      "/api/surface/agents?surface=tray",
      "/api/surface/decisions?surface=tray",
      "/api/surface/decisions/apr%201?surface=tray",
      "/api/surface/briefing?surface=tray",
      "/api/surface/sessions/s1/queued?surface=tray",
      "/api/surface/commands",
      "/api/surface/decisions/apr%201/take-over",
      "/api/surface/sessions/s1/feed?surface=tray",
    ]);
    expect(JSON.parse(String(seen[6].init?.body))).toEqual({
      surface: "tray",
      origin: { kind: "pointer" },
      command: { type: "stopAll" },
    });
    expect(JSON.parse(String(seen[7].init?.body))).toEqual({ surface: "tray", version: 3, secret: "731905" });
  });

  it("reads the status line with the mode of the conversation shown", async () => {
    const seen: Seen[] = [];
    recordFetch(seen);
    await new HttpClient().status("s 1");
    expect(seen.map(({ url }) => url)).toEqual(["/api/surface/status?surface=web&session=s%201"]);
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
              ? {
                  accounts: [
                    { id: "c1", channel: "telegram", account: "42", state: "active" },
                    { id: "c2", channel: "telegram", account: "77", displayName: "Ana", state: "active" },
                  ],
                  waiting: [],
                }
              : url === "/api/kernel/pairing"
                ? { code: "ABCD-2345", url: "http://localhost/ui/pair#code=ABCD-2345", expiresAt: "", scopes: ["decide", "read", "stop"] }
                : url === "/api/surface/phone/devices"
                  ? { devices: [{ deviceId: "d1", passkey: true, push: true }], computerPasskeys: [{ id: "k1", createdAt: "" }] }
                  : {};
        return new Response(JSON.stringify(body), { status: 200 });
      }),
    );
    const devices = new HttpClient().devices();
    expect(await devices.list()).toEqual([
      // A phone with its passkey says so; the person's passkey on this computer has its own row.
      { id: "device:d1", name: "Ana's phone", may: "asks for your face or fingerprint" },
      { id: "passkey:k1", name: "Face or fingerprint on this computer", may: "asks for your face or fingerprint" },
      // Each paired account by its name and its account, so two on one channel differ.
      { id: "channel:c1", name: "Telegram: 42", may: "yes once, or no" },
      { id: "channel:c2", name: "Telegram: Ana (77)", may: "yes once, or no" },
    ]);
    await devices.remove("device:d1");
    await devices.remove("channel:c1");
    await devices.remove("passkey:k1");
    expect(await devices.pair()).toEqual({ code: "ABCD-2345", url: "http://localhost/ui/pair#code=ABCD-2345", expiresAt: "" });
    expect(seen.slice(3).map(({ url, init }) => [init?.method ?? "GET", url])).toEqual([
      ["POST", "/api/kernel/devices/d1/revoke"],
      ["POST", "/api/kernel/channels/c1/revoke"],
      ["DELETE", "/api/surface/phone/passkey/k1"],
      ["POST", "/api/kernel/pairing"],
    ]);
    // "Add a phone or app" pairs a phone, so its link opens the installed app (spec 3.3 §8.3).
    expect(JSON.parse(String(seen.at(-1)?.init?.body))).toEqual({ name: "Your phone", kind: "phone" });
  });

  it("confirms a high-risk answer with this computer's passkey, never with request metadata", async () => {
    const calls: string[] = [];
    const answered = { type: "answered", card: { id: "apr_1" } };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push(`${init?.method ?? "GET"} ${url}`);
        const reply = (body: unknown, status = 200) =>
          new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
        if (url === "/api/surface/commands") {
          return reply({ error: { code: "surface.confirm_presence", message: "confirm it's you first" } }, 403);
        }
        if (url.endsWith("/confirm/options")) {
          expect(JSON.parse(String(init?.body))).toEqual({ version: 3, answer: "allowOnce", expanded: true });
          return reply({ challengeId: "c1", publicKey: { challenge: "AAAA", rpId: "localhost", allowCredentials: [] } });
        }
        if (url.endsWith("/confirm")) {
          expect(JSON.parse(String(init?.body))).toMatchObject({ challengeId: "c1", credential: { id: "AQID" } });
          return reply(answered);
        }
        return reply({}, 404);
      }),
    );
    const get = vi.fn(async () => ({ toJSON: () => ({ id: "AQID", rawId: "AQID", type: "public-key", response: {} }) }));
    vi.stubGlobal("navigator", { ...navigator, credentials: { get } });
    const outcome = await new HttpClient().command(
      { type: "decide", decisionId: "apr_1", version: 3, answer: "allowOnce", expanded: true },
      { kind: "pointer" },
    );
    expect(outcome).toEqual(answered);
    expect(get).toHaveBeenCalledTimes(1);
    expect(calls).toEqual([
      "POST /api/surface/commands",
      "POST /api/surface/decisions/apr_1/confirm/options",
      "POST /api/surface/decisions/apr_1/confirm",
    ]);
  });

  it("keeps the refusal of a high-risk answer when the server serves no passkeys, and a cancelled check says so", async () => {
    let options = 404;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) =>
        url === "/api/surface/commands"
          ? new Response(JSON.stringify({ error: { code: "surface.confirm_presence", message: "confirm it's you first" } }), { status: 403 })
          : new Response(JSON.stringify({ challengeId: "c1", publicKey: { challenge: "AAAA" } }), { status: options }),
      ),
    );
    const get = vi.fn(async () => {
      throw new DOMException("The operation either timed out or was not allowed.", "NotAllowedError");
    });
    vi.stubGlobal("navigator", { ...navigator, credentials: { get } });
    const decide = () =>
      new HttpClient().command({ type: "decide", decisionId: "apr_1", version: 3, answer: "allowOnce" }, { kind: "pointer" });
    await expect(decide()).rejects.toMatchObject({ status: 403, code: "surface.confirm_presence" });
    expect(get).not.toHaveBeenCalled();
    options = 200;
    await expect(decide()).rejects.toMatchObject({ status: 403, code: "surface.phone.passkey_not_used" });
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

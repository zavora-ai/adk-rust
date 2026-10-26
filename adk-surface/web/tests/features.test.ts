// Features a server may be built without (spec 3.3 §6.1): a feature's route answered with 404,
// 405 or 501 hides its part of the page; any other failure is an error; the website's tab
// reads its route through the same probe.
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../src/client";
import { ABSENT_STATUSES, isAbsent, probe } from "../src/features";
import { HttpWebsiteClient } from "../src/website/client";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the feature probe", () => {
  it("reads what a served feature answers", async () => {
    expect(await probe(async () => ({ verbs: ["snooze"] }))).toEqual({ verbs: ["snooze"] });
  });

  it("hides a feature the server was built without", async () => {
    expect(ABSENT_STATUSES).toEqual([404, 405, 501]);
    for (const status of ABSENT_STATUSES) {
      expect(isAbsent(new ApiError(status, "no such route"))).toBe(true);
      expect(await probe(() => Promise.reject(new ApiError(status, "no such route")))).toBeUndefined();
    }
  });

  it("reports any other failure", async () => {
    for (const error of [new ApiError(500, "broken"), new ApiError(401, "sign in"), new TypeError("Failed to fetch")]) {
      expect(isAbsent(error)).toBe(false);
      await expect(probe(() => Promise.reject(error))).rejects.toBe(error);
    }
  });
});

describe("the website's tab", () => {
  function serve(status: number, body: unknown) {
    const fetch = vi.fn(async () => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } }));
    vi.stubGlobal("fetch", fetch);
    return fetch;
  }

  it("shows where the server serves a website", async () => {
    const home = { site: { name: "Juniper Bakery" } };
    const fetch = serve(200, home);
    expect(await new HttpWebsiteClient().home()).toEqual(home);
    expect(fetch).toHaveBeenCalledWith("/api/surface/website", expect.anything());
  });

  it("is absent where the server was built without one", async () => {
    for (const status of [404, 405, 501]) {
      serve(status, { error: { message: "no route", code: "not_found" } });
      expect(await new HttpWebsiteClient().home()).toBeUndefined();
    }
  });

  it("reports a server that fails", async () => {
    serve(500, { error: { message: "the store is down", code: "internal" } });
    await expect(new HttpWebsiteClient().home()).rejects.toMatchObject({ status: 500, message: "the store is down" });
  });
});

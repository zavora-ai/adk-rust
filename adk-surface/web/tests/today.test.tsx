// Today over the kernel's "While you were away" (spec 3.3 §5.1, §10 K4), the morning briefing
// when quiet time ends (§12 phase 2), each area's settings (§10 K3) and pairing a chat account
// from the home (§10 K1).
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { agentOf, briefingDue, insideQuiet, lineWords, mergeAway, quietEnded } from "../src/away";
import { ApiError, HttpClient } from "../src/client";
import { FixtureClient, awayPage, fixtureSeams } from "../src/fixture/fixture";
import type { SeamCalls } from "../src/fixture/fixture";
import type { AgentRow } from "../src/generated/api";
import type { AwayLine, AwayPage } from "../src/kernel";
import type { Seams } from "../src/seams";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  for (const pick of [() => window.localStorage, () => window.sessionStorage]) {
    try {
      pick().removeItem("adk.home.briefing");
    } catch {
      // This runtime has no such storage.
    }
  }
});

const calls = (): SeamCalls => ({ seen: [], records: [], accepted: [] });

/** The texts of Today's lines, top to bottom. */
function lines(): string[] {
  const list = screen.getAllByRole("list").find((element) => element.classList.contains("briefing"))!;
  return within(list)
    .getAllByRole("listitem")
    .map((item) => item.querySelector(".briefing-text")?.textContent ?? "");
}

describe("Today", () => {
  it("reads the kernel's page: what was spent, quiet time, what waits, and one line per event, failures first", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="today" />);
    await screen.findByText("Household failed.");
    expect(screen.getByText("$0.62")).toBeTruthy();
    expect(screen.getByText("Respected")).toBeTruthy();
    expect(screen.getByText(/^Since (\d+ \w+, )?22:04$/)).toBeTruthy();
    expect(lines()).toEqual([
      "Household failed.",
      "Research finished.",
      "Finance suggests paying the [SUPPLIER] invoice due Monday. Nothing happens until you say so.",
      "You allowed Code to go ahead once.",
      "You seem to start work at 08:30 on Fridays now. Is that right?",
    ]);
    // The suggestion and the question are answered here.
    expect(screen.getByRole("button", { name: "Do it" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Yes, remember it" })).toBeTruthy();
  });

  it("opens each line's record and never marks anything seen until the person says so", async () => {
    const seen = calls();
    render(<App client={new FixtureClient()} seams={fixtureSeams(seen)} initialSection="today" />);
    const row = (await screen.findByText("Household failed.")).closest("li")!;
    await userEvent.click(within(row).getByRole("button", { name: "Details" }));
    const link = await within(row).findByRole("link", { name: "The record" });
    expect(link.getAttribute("href")).toBe("/api/kernel/away/lines/22");
    expect(within(row).getByText(/^Recorded /)).toBeTruthy();
    expect(seen.records).toEqual([22]);
    expect(seen.seen).toEqual([]);

    await userEvent.click(screen.getByRole("button", { name: "Mark as seen" }));
    await screen.findByText("Marked as seen. New lines will show here.");
    expect(seen.seen).toEqual([30]);
    expect(screen.queryByText("Household failed.")).toBeNull();
    // The suggestion and the question still wait for an answer; nothing is assumed.
    expect(lines()).toEqual([
      "Finance suggests paying the [SUPPLIER] invoice due Monday. Nothing happens until you say so.",
      "You seem to start work at 08:30 on Fridays now. Is that right?",
    ]);
  });

  it("names each agent as the roster does, and keeps the kernel's own work out of the list", async () => {
    const agents: AgentRow[] = [
      {
        agent: { name: "adk-rust", displayName: "ADK-Rust", avatar: { monogram: "A", color: "#C03A00" } },
        status: "done",
        statusText: "Done",
      },
    ];
    const line = (seq: number, kind: string, effect?: string, agent = "urn:adk:agent:adk-rust/adk-rust"): AwayLine => ({
      seq,
      at: "2026-10-24T07:00:00Z",
      partition: "default",
      agent,
      kind,
      effect,
      record: `/api/kernel/away/lines/${seq}`,
    });
    // One message and one decision allowed from the home (the coordinator's run on main).
    const run = [line(5, "decided", "approve"), line(9, "done", "completed")];
    const said = run.map((each) => lineWords(each, agents).text);
    expect(said).toEqual(["You allowed ADK-Rust to go ahead once.", "ADK-Rust finished."]);
    for (const text of said) expect(text).not.toMatch(/urn:|Urn:|Kernel/);
    expect(lineWords(line(6, "suggestion", "proposed", "kernel"), agents).text).toBe(
      "Your agents suggest something. Nothing happens until you say so.",
    );
    expect(lineWords(line(7, "failed"), agents).text).toBe("ADK-Rust stopped because it was repeating itself.");
    expect(agentOf("urn:adk:agent:home/finance_bot", [])).toBe("Finance bot");
    expect(agentOf("kernel", agents)).toBeUndefined();
  });

  it("reads the engine's briefing where the kernel keeps no ledger", async () => {
    const seams = fixtureSeams();
    seams.whileAway!.read = async () => undefined;
    render(<App client={new FixtureClient()} seams={seams} initialSection="today" />);
    expect(await screen.findByText(/The backup to \[DRIVE\] failed/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Mark as seen" })).toBeNull();
  });
});

describe("quiet time and the morning briefing", () => {
  const window = { start: "22:00", end: "07:00", utcOffsetMinutes: 0 };
  const utc = (iso: string) => new Date(iso);

  it("knows when quiet time holds and when it last ended, across midnight and in its own offset", () => {
    expect(insideQuiet(window, utc("2026-10-24T23:30:00Z"))).toBe(true);
    expect(insideQuiet(window, utc("2026-10-25T06:59:00Z"))).toBe(true);
    expect(insideQuiet(window, utc("2026-10-25T07:00:00Z"))).toBe(false);
    expect(quietEnded([window], utc("2026-10-25T06:00:00Z"))).toBeUndefined();
    expect(quietEnded([window], utc("2026-10-25T09:15:00Z"))).toEqual(utc("2026-10-25T07:00:00Z"));
    expect(quietEnded([window], utc("2026-10-25T21:59:00Z"))).toEqual(utc("2026-10-25T07:00:00Z"));
    const nairobi = { start: "22:00", end: "07:00", utcOffsetMinutes: 180 };
    expect(quietEnded([nairobi], utc("2026-10-25T05:00:00Z"))).toEqual(utc("2026-10-25T04:00:00Z"));
    expect(quietEnded([], utc("2026-10-25T09:15:00Z"))).toBeUndefined();
  });

  it("is due once quiet time ends, for what happened since the person last marked it seen", () => {
    const page = { ...awayPage(), since: "2026-10-24T21:04:00Z" };
    const morning = utc("2026-10-25T07:30:00Z");
    const end = utc("2026-10-25T07:00:00Z");
    expect(briefingDue(page, [window], morning, undefined)).toEqual(end);
    expect(briefingDue(page, [window], utc("2026-10-25T06:30:00Z"), undefined)).toBeUndefined();
    expect(briefingDue({ ...page, since: "2026-10-25T07:10:00Z" }, [window], morning, undefined)).toBeUndefined();
    expect(briefingDue({ ...page, lines: [], waiting: 0 }, [window], morning, undefined)).toBeUndefined();
    expect(briefingDue(page, [window], morning, end.toISOString())).toBeUndefined();
    // The next morning is a briefing of its own.
    expect(briefingDue(page, [window], utc("2026-10-26T07:30:00Z"), end.toISOString())).toEqual(
      utc("2026-10-26T07:00:00Z"),
    );
  });

  it("shows on Threads once quiet time has ended, from the page Today reads, and once only", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const morning = new Date();
    morning.setHours(8, 0, 0, 0);
    vi.setSystemTime(morning);
    const seen = calls();
    const { unmount } = render(<App client={new FixtureClient()} seams={fixtureSeams(seen)} initialSection="threads" />);
    const card = await screen.findByRole("region", { name: "While you were away" });
    expect(within(card).getByText("$0.62")).toBeTruthy();
    expect(within(card).getByText("Quiet time respected")).toBeTruthy();
    expect(within(card).getByText("3 waiting for you")).toBeTruthy();
    expect(within(card).getByText("1 finished · 1 failed · 1 suggestion · 1 question · 1 answered")).toBeTruthy();

    await userEvent.click(within(card).getByRole("button", { name: "See what happened" }));
    await screen.findByText("Household failed.");
    expect(seen.seen).toEqual([]);
    await userEvent.click(screen.getByRole("link", { name: /Threads/ }));
    await screen.findByRole("region", { name: /^Decisions/ });
    expect(screen.queryByRole("region", { name: "While you were away" })).toBeNull();
    unmount();

    // A reload the same morning shows it no more.
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="threads" />);
    await screen.findByRole("region", { name: /^Decisions/ });
    expect(screen.queryByRole("region", { name: "While you were away" })).toBeNull();
  });

  it("does not show during quiet time", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const night = new Date();
    night.setHours(23, 30, 0, 0);
    vi.setSystemTime(night);
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="threads" />);
    await screen.findByRole("region", { name: /^Decisions/ });
    await screen.findByText("Quiet until 07:00");
    expect(screen.queryByRole("region", { name: "While you were away" })).toBeNull();
  });
});

describe("What your agents can do", () => {
  it("changes quiet time, the daily limit and the mode, showing each old and new value", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="memory" />);
    const card = (await screen.findByRole("heading", { name: "What your agents can do" })).closest("section")!;
    expect(await within(card).findByText("$1.84 spent today")).toBeTruthy();

    const [quiet, limit] = within(card).getAllByRole("button", { name: "Change" });
    await userEvent.click(quiet);
    await userEvent.clear(within(card).getByLabelText("From"));
    await userEvent.type(within(card).getByLabelText("From"), "23:00");
    await userEvent.clear(within(card).getByLabelText("Until"));
    await userEvent.type(within(card).getByLabelText("Until"), "06:00");
    await userEvent.click(within(card).getByRole("button", { name: "Save" }));
    expect(await within(card).findByText("Was 22:00–07:00. Now 23:00–06:00.")).toBeTruthy();

    await userEvent.click(limit);
    const amount = within(card).getByLabelText("Dollars a day");
    await userEvent.clear(amount);
    await userEvent.type(amount, "8");
    await userEvent.click(within(card).getByRole("button", { name: "Save" }));
    expect(await within(card).findByText("Was $5.00. Now $8.00.")).toBeTruthy();

    await userEvent.click(within(card).getByRole("radio", { name: "Go ahead" }));
    expect(await within(card).findByText("Was Ask first. Now Go ahead.")).toBeTruthy();
    // The status line reads the area's settings in force, not the ones the engine started with.
    await userEvent.click(screen.getByRole("link", { name: "Today" }));
    const header = screen.getByRole("banner");
    await within(header).findByText("Goes ahead");
    expect(within(header).getByText("$1.84 of $8.00 today")).toBeTruthy();
  });

  it("shows why the kernel refused a change, such as from a paired device", async () => {
    const seams: Seams = fixtureSeams();
    seams.limits!.setQuietTime = async () => {
      throw new ApiError(403, "a paired device may not change quiet time: change it in the home", "kernel.forbidden");
    };
    render(<App client={new FixtureClient()} seams={seams} initialSection="memory" />);
    const card = (await screen.findByRole("heading", { name: "What your agents can do" })).closest("section")!;
    await userEvent.click((await within(card).findAllByRole("button", { name: "Change" }))[0]);
    await userEvent.click(within(card).getByRole("button", { name: "Turn off" }));
    expect((await within(card).findByRole("alert")).textContent).toContain("a paired device may not change quiet time");
    expect(within(card).queryByText(/^Was /)).toBeNull();
  });
});

describe("Where you can say yes from", () => {
  it("pairs the chat account that was sent a code", async () => {
    const seen = calls();
    render(<App client={new FixtureClient()} seams={fixtureSeams(seen)} initialSection="memory" />);
    const form = await screen.findByRole("form", { name: "Pair a chat account" });
    expect(within(form).getByText("Telegram: Sam asked to pair.")).toBeTruthy();
    const field = within(form).getByLabelText("The code it was sent");
    await userEvent.type(field, "WRONG-CODE{Enter}");
    expect((await screen.findByRole("alert")).textContent).toContain("that code is wrong");
    await userEvent.clear(field);
    await userEvent.type(field, "ABCD-EFGH-2345");
    await userEvent.click(within(form).getByRole("button", { name: "Pair" }));
    expect(await within(form).findByText("Telegram: Sam is paired.")).toBeTruthy();
    expect(seen.accepted).toEqual(["ABCD-EFGH-2345"]);
    await waitFor(() => expect(within(form).queryByText("Telegram: Sam asked to pair.")).toBeNull());
  });
});

type Seen = { url: string; init?: RequestInit };

function answering(seen: Seen[], answer: (url: string) => unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      seen.push({ url, init });
      return new Response(JSON.stringify(answer(url)), { status: 200, headers: { "content-type": "application/json" } });
    }),
  );
}

describe("the HTTP client's kernel routes", () => {
  it("reads While you were away on, page after page, and marks it seen only when asked", async () => {
    const seen: Seen[] = [];
    const page = (through: number, more: boolean): AwayPage => ({
      ...awayPage(),
      through,
      more,
      spentMicroUsd: 100,
      lines: awayPage().lines.slice(0, 1),
    });
    answering(seen, (url) => (url.includes("after=") ? page(80, false) : page(40, true)));
    const away = new HttpClient().whileAway();
    const read = await away.read();
    expect(seen.map(({ url }) => url)).toEqual(["/api/kernel/away?viewer=home", "/api/kernel/away?viewer=home&after=40"]);
    expect(seen.every(({ init }) => (init?.method ?? "GET") === "GET")).toBe(true);
    expect([read?.through, read?.spentMicroUsd, read?.lines.length, read?.more]).toEqual([80, 200, 2, false]);

    await away.markSeen(80);
    expect(seen[2].url).toBe("/api/kernel/away/seen");
    expect(JSON.parse(String(seen[2].init?.body))).toEqual({ viewer: "home", through: 80 });
    await away.record(22);
    expect(seen[3].url).toBe("/api/kernel/away/lines/22");
  });

  it("changes an area's settings and pairs a chat account through the kernel's routes", async () => {
    const seen: Seen[] = [];
    answering(seen, () => ({ area: { partition: "work", quietTime: [], spentToday: 0 }, account: {} }));
    const client = new HttpClient();
    const limits = client.limits();
    await limits.setQuietTime("work", { start: "23:00", end: "06:00" });
    await limits.setSpendingLimit("work", null);
    await limits.setMode("work", "auto");
    await client.devices().accept?.(" ABCD-EFGH-2345 ");
    expect(seen.map(({ url, init }) => [init?.method, url, JSON.parse(String(init?.body))])).toEqual([
      ["POST", "/api/kernel/settings/work/quiet-time", { window: { start: "23:00", end: "06:00" } }],
      ["POST", "/api/kernel/settings/work/spending-limit", { microUsd: null }],
      ["POST", "/api/kernel/settings/work/mode", { mode: "auto" }],
      ["POST", "/api/kernel/channels/pair", { code: "ABCD-EFGH-2345" }],
    ]);
  });

  it("treats a kernel without an activity ledger as no page, so the engine's briefing shows", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ code: "kernel.config", detail: "this kernel keeps no activity ledger" }), {
            status: 400,
          }),
      ),
    );
    expect(await new HttpClient().whileAway().read()).toBeUndefined();
  });

  it("adds up two pages exactly", () => {
    const first = { ...awayPage(), quietTime: { respected: true } };
    const next = { ...awayPage(), quietTime: { respected: false, breaches: [44] }, waiting: 1, through: 60 };
    const both = mergeAway(first, next);
    expect([both.counts.done, both.spentMicroUsd, both.quietTime, both.waiting, both.through, both.lines.length]).toEqual([
      2,
      1_240_000,
      { respected: false, breaches: [44] },
      1,
      60,
      10,
    ]);
  });
});

describe("since when", () => {
  it("gives the time within the last day and the date further back", async () => {
    const { dayAndTime } = await import("../src/format");
    const now = new Date(2026, 9, 25, 8, 0);
    expect(dayAndTime(new Date(2026, 9, 24, 22, 4), now)).toBe("22:04");
    expect(dayAndTime(new Date(2026, 9, 24, 7, 59), now)).toBe("24 October, 07:59");
  });
});

// Today over the kernel's "While you were away" (spec 3.3 §5.1, §10 K4), the morning briefing
// when quiet time ends (§12 phase 2), each area's settings (§10 K3) and pairing a chat account
// from the home (§10 K1).
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { Today } from "../src/screens/Today";
import { agentOf, briefingDue, deviceOffset, insideQuiet, lineWords, mergeAway, onThisDevice, quietEnded } from "../src/away";
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
      "You allowed Code to go ahead.",
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
    // The record is read through the page's client, with its sign-in, and told in plain words;
    // nothing links the tab away to the raw route.
    expect(await within(row).findByText("On record as entry 22")).toBeTruthy();
    expect(within(row).getByText(/^Recorded /)).toBeTruthy();
    expect(within(row).queryByRole("link")).toBeNull();
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

  // §8 "Name the outcome": once answered, a line no longer says nothing happens until the person
  // says so, or asks again.
  it("answers a suggestion and a question here, then says what became of each", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="today" />);
    await userEvent.click(await screen.findByRole("button", { name: "Do it" }));
    await screen.findByText("Finance suggests paying the [SUPPLIER] invoice due Monday. It will be done.");
    await userEvent.click(screen.getByRole("button", { name: "No" }));
    await screen.findByText("You seem to start work at 08:30 on Fridays now. Answered no.");
    for (const name of ["Do it", "Not now", "Yes, remember it", "No"]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
    expect(lines().join(" ")).not.toMatch(/Nothing happens until you say so|Is that right/);
  });

  // An agenda keeps every finished item, oldest first: the home reads the items still open or
  // under way, so a new question past the oldest 200 is still answered here (spec 3.3 §5.1).
  it("answers a question that is newer than two hundred finished items", async () => {
    const old = Array.from({ length: 210 }, (_, index) => ({
      id: `ag_old_${index}`,
      title: `Old suggestion ${index}.`,
      origin: "user",
      state: "done",
      createdAt: new Date(Date.UTC(2026, 8, 1, 0, index)).toISOString(),
    }));
    const question = {
      id: "ag_new",
      title: "You seem to start work at 08:30 on Fridays now.",
      origin: { observation: { id: "obs-9" } },
      state: "proposed",
      createdAt: new Date(Date.now() - 60_000).toISOString(),
    };
    const page: AwayPage = {
      ...awayPage(),
      counts: { done: 0, failed: 0, stopped: 0, suggestions: 0, questions: 1, decided: 0 },
      lines: [{ seq: 300, at: question.createdAt, partition: "default", agent: "kernel", kind: "question", record: "/api/kernel/away/lines/300" }],
    };
    const answered: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        const state = new URL(url, "http://home").searchParams.get("state");
        const limit = Number(new URL(url, "http://home").searchParams.get("limit") ?? Infinity);
        const body = url.startsWith("/api/kernel/agenda/")
          ? (answered.push(url), (question.state = "approved"), { item: question })
          : url.startsWith("/api/kernel/agenda")
            ? { items: [...old, question].filter((item) => !state || item.state === state).slice(0, limit) }
            : url.startsWith("/api/kernel/away")
              ? page
              : url.startsWith("/api/surface/agents")
                ? { agents: [] }
                : url.startsWith("/api/surface/decisions")
                  ? { decisions: [] }
                  : url.startsWith("/api/surface/status")
                    ? { decisions: 0, stopped: false }
                    : undefined;
        return body === undefined
          ? new Response("", { status: 404 })
          : new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
      }),
    );
    const client = new HttpClient();
    render(<App client={client} seams={{ whileAway: client.whileAway() }} initialSection="today" />);
    await screen.findByText("You seem to start work at 08:30 on Fridays now. Is that right?");
    await userEvent.click(screen.getByRole("button", { name: "Yes, remember it" }));
    expect(answered).toEqual(["/api/kernel/agenda/ag_new/approve"]);
    await screen.findByText("You seem to start work at 08:30 on Fridays now. Answered yes.");
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
    expect(said).toEqual(["You allowed ADK-Rust to go ahead.", "ADK-Rust finished."]);
    for (const text of said) expect(text).not.toMatch(/urn:|Urn:|Kernel/);
    expect(lineWords(line(6, "suggestion", "proposed", "kernel"), agents).text).toBe(
      "Your agents suggest something. Nothing happens until you say so.",
    );
    expect(lineWords(line(7, "failed"), agents).text).toBe("ADK-Rust stopped because it was repeating itself.");
    // Every line has its status pill (spec 3.3 §5.1), Stop all and Resume all too.
    expect(lineWords(line(8, "stoppedAll", "frozen", "kernel"), agents)).toEqual({
      pill: "neutral",
      pillText: "Stopped",
      text: "All agents stopped.",
    });
    expect(lineWords(line(10, "resumedAll", "unfrozen", "kernel"), agents)).toEqual({
      pill: "neutral",
      pillText: "Resumed",
      text: "All agents resumed.",
    });
    expect(agentOf("urn:adk:agent:home/finance_bot", [])).toBe("Finance bot");
    expect(agentOf("kernel", agents)).toBeUndefined();
  });

  it("drops a failed read of the engine's briefing once the kernel's page shows", async () => {
    const client = new FixtureClient();
    client.briefing = async () => {
      throw new TypeError("Failed to fetch");
    };
    const props = {
      client,
      agents: [],
      routinesToday: [],
      canAddRoutine: false,
      seams: fixtureSeams(),
      onOpenAgent: () => {},
      onChanged: () => {},
      onAwayChanged: () => {},
    };
    const { rerender } = render(<Today {...props} />);
    expect((await screen.findByRole("alert")).textContent).toBe("Can't reach your agents right now. Trying again.");
    rerender(<Today {...props} away={awayPage()} />);
    await screen.findByText("Household failed.");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  // Without a ledger the engine's briefing lists nothing, whatever ran (`adk-rust serve`):
  // Today says nothing is recorded, never that nothing happened.
  it("says nothing is recorded where the server keeps no record, never that nothing happened", async () => {
    const client = new FixtureClient();
    const briefing = client.briefing.bind(client);
    client.briefing = async () => ({ ...(await briefing()), lines: [], recorded: false });
    const seams = fixtureSeams();
    seams.whileAway!.read = async () => undefined;
    render(<App client={client} seams={seams} initialSection="today" />);
    expect(
      await screen.findByText("What your agents do is not recorded on this server, so it can't be listed here. adk-rust up keeps a record."),
    ).toBeTruthy();
    expect(screen.queryByText("Nothing happened while you were away.")).toBeNull();
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

  // The kernel keeps a window without an offset in the server's time zone; a page in another
  // zone reads it on its own clock (spec 3.3 §5.1).
  it("reads a window the server keeps in its own zone on this device's clock", () => {
    expect(onThisDevice({ start: "22:00", end: "07:00" }, 0, 180)).toEqual({ start: "01:00", end: "10:00", utcOffsetMinutes: 180 });
    expect(onThisDevice({ start: "01:00", end: "06:00" }, 180, 0)).toEqual({ start: "22:00", end: "03:00", utcOffsetMinutes: 0 });
    expect(onThisDevice({ start: "22:00", end: "07:00", utcOffsetMinutes: 180 }, 0, 180)).toEqual({
      start: "22:00",
      end: "07:00",
      utcOffsetMinutes: 180,
    });
    expect(onThisDevice({ start: "22:00", end: "07:00" }, undefined, 180)).toEqual({ start: "22:00", end: "07:00" });
    // 23:30 in Nairobi is 20:30 in UTC: outside a 22:00–07:00 window kept in UTC.
    const nairobi = onThisDevice({ start: "22:00", end: "07:00" }, 0, 180);
    expect(insideQuiet(nairobi, utc("2026-10-24T20:30:00Z"))).toBe(false);
    expect(insideQuiet(nairobi, utc("2026-10-24T22:30:00Z"))).toBe(true);
  });

  it("says quiet time holds when the server keeps it, not when this device's clock would", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const night = new Date();
    night.setHours(23, 30, 0, 0);
    vi.setSystemTime(night);
    // The server is three hours behind this device, so its 22:00 is this device's 01:00.
    const client = new FixtureClient();
    const status = client.status.bind(client);
    client.status = async () => ({ ...(await status()), hostUtcOffsetMinutes: deviceOffset(night) - 180 });
    render(<App client={client} seams={fixtureSeams()} initialSection="threads" />);
    const header = screen.getByRole("banner");
    expect(await within(header).findByText("Quiet from 01:00")).toBeTruthy();
    expect(within(header).queryByText("Quiet until 07:00")).toBeNull();
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

  it("saves quiet time with this device's offset, so the kernel keeps the hours the person typed", async () => {
    const seams: Seams = fixtureSeams();
    const saved: unknown[] = [];
    const set = seams.limits!.setQuietTime;
    seams.limits!.setQuietTime = async (partition, window) => {
      saved.push(window);
      return set(partition, window);
    };
    render(<App client={new FixtureClient()} seams={seams} initialSection="memory" />);
    const card = (await screen.findByRole("heading", { name: "What your agents can do" })).closest("section")!;
    await userEvent.click((await within(card).findAllByRole("button", { name: "Change" }))[0]);
    await userEvent.click(within(card).getByRole("button", { name: "Save" }));
    await within(card).findByText(/^Was /);
    expect(saved).toEqual([{ start: "22:00", end: "07:00", utcOffsetMinutes: deviceOffset() }]);
  });

  // The kernel keeps one window in place of all an area had: the editor says so before it saves.
  it("says that saving quiet time replaces every window the area has", async () => {
    const seams: Seams = fixtureSeams();
    const areas = seams.limits!.areas;
    seams.limits!.areas = async () =>
      (await areas())?.map((area) => ({
        ...area,
        quietTime: [
          { start: "22:00", end: "07:00", utcOffsetMinutes: deviceOffset() },
          { start: "19:00", end: "08:00", utcOffsetMinutes: deviceOffset() },
        ],
      }));
    render(<App client={new FixtureClient()} seams={seams} initialSection="memory" />);
    const card = (await screen.findByRole("heading", { name: "What your agents can do" })).closest("section")!;
    await within(card).findByText("22:00–07:00, 19:00–08:00");
    await userEvent.click(within(card).getAllByRole("button", { name: "Change" })[0]);
    expect(
      within(card).getByText(
        "Your agents are quiet 22:00–07:00, 19:00–08:00 now. Saving keeps one window in their place; Turn off ends them all.",
      ),
    ).toBeTruthy();
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
    expect(within(form).getByText("Telegram: Sam (4242) asked to pair.")).toBeTruthy();
    const field = within(form).getByLabelText("The code it was sent");
    await userEvent.type(field, "WRONG-CODE{Enter}");
    expect((await screen.findByRole("alert")).textContent).toContain("that code is wrong");
    await userEvent.clear(field);
    await userEvent.type(field, "ABCD-EFGH-2345");
    await userEvent.click(within(form).getByRole("button", { name: "Pair" }));
    expect(await within(form).findByText("Telegram: Sam (4242) is paired.")).toBeTruthy();
    expect(seen.accepted).toEqual(["ABCD-EFGH-2345"]);
    await waitFor(() => expect(within(form).queryByText("Telegram: Sam (4242) asked to pair.")).toBeNull());
  });

  it("says when it cannot read who can say yes, instead of showing only this computer", async () => {
    const seams = fixtureSeams();
    seams.devices!.list = async () => {
      throw new ApiError(500, "the channel binding store is unavailable");
    };
    render(<App client={new FixtureClient()} seams={seams} initialSection="memory" />);
    expect(
      await screen.findByText("Can't read the phones and chat accounts that can say yes right now. Trying again."),
    ).toBeTruthy();
  });

  // S-I6: the confirmation names the account the kernel paired, never the one that asked when
  // the section opened, so a stranger's code is not taken for a known person's.
  it("names the account the kernel paired, whoever was waiting", async () => {
    const seams = fixtureSeams();
    seams.devices!.accept = async () => ({ id: "c-carol", channel: "telegram", account: "7777", displayName: "Carol", state: "active" });
    render(<App client={new FixtureClient()} seams={seams} initialSection="memory" />);
    const form = await screen.findByRole("form", { name: "Pair a chat account" });
    expect(within(form).getByText("Telegram: Sam (4242) asked to pair.")).toBeTruthy();
    await userEvent.type(within(form).getByLabelText("The code it was sent"), "CAROL-CODE");
    await userEvent.click(within(form).getByRole("button", { name: "Pair" }));
    expect(await within(form).findByText("Telegram: Carol (7777) is paired.")).toBeTruthy();
    expect(within(form).queryByText(/Sam \(4242\) is paired/)).toBeNull();
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

describe("typed dollars", () => {
  // A decimal comma is a decimal mark, never dropped: `5,50` is $5.50, not $550 (spec 3.3 §10 K3).
  it("reads a decimal comma or point, groups of thousands, and refuses the rest", async () => {
    const { parseDollars } = await import("../src/format");
    expect(["5", "5.5", "5.50", "5,50", "$ 5,50", "1,000", "1.000", "1,000.50", "1.000,50", "1234,5", "0"].map(parseDollars)).toEqual([
      5_000_000, 5_500_000, 5_500_000, 5_500_000, 5_500_000, 1_000_000_000, 1_000_000_000, 1_000_500_000, 1_000_500_000,
      1_234_500_000, 0,
    ]);
    for (const text of ["", "-5", "5,500,5", "1,000,50", "1.000.50", "1,000.000", "1,2,3", "five", "5.5.5"]) {
      expect(parseDollars(text), text).toBeUndefined();
    }
  });

  it("sets the limit a person typed with a decimal comma", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="memory" />);
    const card = (await screen.findByRole("heading", { name: "What your agents can do" })).closest("section")!;
    await userEvent.click((await within(card).findAllByRole("button", { name: "Change" }))[1]);
    const amount = within(card).getByLabelText("Dollars a day");
    await userEvent.clear(amount);
    await userEvent.type(amount, "5,50");
    await userEvent.click(within(card).getByRole("button", { name: "Save" }));
    expect(await within(card).findByText("Was $5.00. Now $5.50.")).toBeTruthy();
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
